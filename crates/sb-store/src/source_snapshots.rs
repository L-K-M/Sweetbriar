//! Private, durable snapshots of import sources (spec/03 "Import
//! source snapshots", spec/09 "Import"): before the importer counts or
//! reads a source, its bytes are copied into `recovery/` under a random
//! private name, hashed while streaming and flushed. Offsets,
//! checkpoints and reopen all work on that immutable copy. The original
//! is only ever read: later changes cannot change a capture, and
//! reopening never substitutes the live path's current contents.
//!
//! A copy counts as captured only once its file and `recovery/` have
//! been flushed, so a snapshot the caller may name survived power loss.
//! A failed or cancelled capture removes only the file it created;
//! crash leftovers stay as evidence for the engine's terminal-job
//! cleanup — nothing here guesses at garbage collection.

use std::io::{self, Read, Seek, SeekFrom, Write};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sb_types::fs::{FileKind, Fs, ReadFile, WriteFile};
use sb_types::random::Random;
use sha2::{Digest, Sha256};

use crate::StoreError;
use crate::objects::ObjectId;

/// Bytes streamed per copy or verify step, as in the object store.
const STREAM_CHUNK: usize = 64 * 1024;

/// A 128-bit token hits an occupied name once in 2^128 draws; only a
/// broken seam could exhaust this many tries.
const NAME_ATTEMPTS: usize = 8;

/// A token is 16 random bytes spelled as 32 lowercase hex digits. It
/// names the copy `recovery/import-source-<token>`; the path never
/// leaves this module.
const TOKEN_BYTES: usize = 16;
const TOKEN_LEN: usize = TOKEN_BYTES * 2;
const FILE_PREFIX: &str = "import-source-";

/// Captures and reopens private source copies over the `Fs` and
/// `Random` seams (spec/03). `new` does no I/O: `recovery/` is part of
/// the store layout and already exists.
pub(crate) struct ImportSourceSnapshots {
    fs: Arc<dyn Fs>,
    random: Arc<dyn Random>,
    recovery: PathBuf,
}

/// What `Store::capture_import_source` produced: the completed,
/// durable snapshot, or the cancellation the progress callback asked
/// for — which leaves no snapshot to reference.
#[derive(Debug)]
pub enum ImportSourceCapture {
    /// The copy is complete and flushed.
    Captured {
        /// The source's canonical path — what a job reports and
        /// records as the captured origin.
        source_path: PathBuf,
        /// The reference a checkpoint may store and later reopen.
        snapshot: ImportSourceSnapshot,
    },
    /// The progress callback broke before the copy finished.
    Cancelled,
}

/// A completed source copy's reference: the token naming its private
/// file plus the digest and length that verify it. The fields stay
/// private, so `open` is only ever pointed at a checked name: one a
/// capture made, or one [`ImportSourceSnapshot::from_metadata`]
/// validated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ImportSourceSnapshot {
    token: String,
    object: ObjectId,
}

impl ImportSourceSnapshot {
    /// Rebuild a reference from stored checkpoint metadata. The token
    /// is the private name's random part: exactly 32 lowercase hex
    /// digits. Anything else — a path, a separator, a wrong length —
    /// is refused, so a stored token can never name a file outside
    /// `recovery/`.
    pub fn from_metadata(
        token: &str,
        sha256: [u8; 32],
        byte_length: u64,
    ) -> Result<Self, StoreError> {
        if !is_token(token) {
            return Err(StoreError::InvalidInput {
                message: format!(
                    "an import source snapshot token is 32 lowercase hex digits, not {token:?}"
                ),
            });
        }
        Ok(Self {
            token: token.to_owned(),
            object: ObjectId::from_parts(sha256, byte_length),
        })
    }

    /// The random name of the private copy, 32 lowercase hex digits.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// The SHA-256 of the captured byte stream.
    pub fn sha256(&self) -> [u8; 32] {
        self.object.sha256()
    }

    /// The hash as 64 lowercase hex digits.
    pub fn sha256_hex(&self) -> String {
        self.object.sha256_hex()
    }

    /// How many bytes were captured.
    pub fn byte_length(&self) -> u64 {
        self.object.byte_length()
    }
}

/// What the copy loop produced: the finished stream's digest and
/// length, or the early stop `progress` asked for.
#[derive(Debug)]
enum Copied {
    Done(ObjectId),
    Cancelled,
}

impl ImportSourceSnapshots {
    /// The seams and the store's `recovery/` directory.
    pub(crate) fn new(fs: Arc<dyn Fs>, random: Arc<dyn Random>, recovery: PathBuf) -> Self {
        Self {
            fs,
            random,
            recovery,
        }
    }

    /// Copy `source` into `recovery/` under a fresh private name while
    /// hashing, then flush the file and the directory (spec/03). The
    /// source must be a regular file and is only read. The copy is
    /// bounded by the size observed when the capture began plus one
    /// detection byte: a source that grew or ended early fails with
    /// the consumed-byte offset. `progress` sees the copied total
    /// after each chunk — once at 0 before the first read — and
    /// breaking it cancels.
    ///
    /// On failure or cancellation this attempt's private file is
    /// removed on a best-effort basis, never hiding the real outcome.
    /// An occupied private name is never overwritten or removed; the
    /// source never is either.
    pub(crate) fn capture(
        &self,
        source: &Path,
        mut progress: impl FnMut(u64) -> ControlFlow<()>,
    ) -> Result<ImportSourceCapture, StoreError> {
        let canonical = self
            .fs
            .canonicalize(source)
            .map_err(|e| source_err("resolve", source, e))?;
        let Some(md) = self
            .fs
            .metadata(&canonical)
            .map_err(|e| source_err("stat", &canonical, e))?
        else {
            return Err(StoreError::not_found(format!(
                "import source {}",
                source.display()
            )));
        };
        if md.kind() != FileKind::File {
            return Err(StoreError::InvalidInput {
                message: format!(
                    "an import source must be a regular file: {}",
                    canonical.display()
                ),
            });
        }
        let mut input = retry_interrupted(|| self.fs.open(&canonical))
            .map_err(|e| source_err("read", &canonical, e))?;
        let (path, token, mut dest) = self.create_private()?;
        // From here this attempt's private file exists: every early
        // exit removes exactly it, best-effort.
        let outcome = copy_bounded(&mut input, &mut dest, md.len(), &mut progress, &canonical)
            .and_then(|copied| match copied {
                Copied::Done(object) => dest
                    .sync_all()
                    .and_then(|()| self.fs.sync_dir(&self.recovery))
                    .map_err(|e| {
                        StoreError::unavailable(format!("cannot flush the import source copy: {e}"))
                    })
                    .map(|()| Some(object)),
                Copied::Cancelled => Ok(None),
            });
        drop(dest);
        match outcome {
            Ok(Some(object)) => Ok(ImportSourceCapture::Captured {
                source_path: canonical,
                snapshot: ImportSourceSnapshot { token, object },
            }),
            // Cancelled or failed: this attempt's private file goes,
            // best-effort, never hiding the real outcome.
            Ok(None) => {
                let _ = self.fs.remove(&path);
                Ok(ImportSourceCapture::Cancelled)
            }
            Err(e) => {
                let _ = self.fs.remove(&path);
                Err(e)
            }
        }
    }

    /// Verify the whole private copy — kind, length, then the full
    /// hash over bounded reads — and return the same opened file
    /// rewound to the start (spec/03). Missing is `NotFound`; a wrong
    /// kind, length or hash is `Corrupt`, with the evidence left in
    /// `recovery/`. The original source path plays no part here.
    pub(crate) fn open(
        &self,
        snapshot: &ImportSourceSnapshot,
    ) -> Result<Box<dyn ReadFile>, StoreError> {
        let path = self.private_path(&snapshot.token);
        let step = format!("open the import source snapshot {}", snapshot.token);
        let unavailable = |e: io::Error| StoreError::unavailable(format!("cannot {step}: {e}"));
        let Some(md) = self.fs.metadata(&path).map_err(unavailable)? else {
            return Err(StoreError::not_found(format!(
                "import source snapshot {}",
                snapshot.token
            )));
        };
        if md.kind() != FileKind::File {
            return Err(StoreError::corrupt(format!(
                "import source snapshot {} is not a file",
                snapshot.token
            )));
        }
        if md.len() != snapshot.byte_length() {
            return Err(StoreError::corrupt(format!(
                "import source snapshot {} is {} bytes; {} were captured",
                snapshot.token,
                md.len(),
                snapshot.byte_length()
            )));
        }
        let mut file = retry_interrupted(|| self.fs.open(&path)).map_err(|e| match e.kind() {
            io::ErrorKind::NotFound => {
                StoreError::not_found(format!("import source snapshot {}", snapshot.token))
            }
            _ => unavailable(e),
        })?;
        verify(&mut file, snapshot)?;
        file.seek(SeekFrom::Start(0)).map_err(unavailable)?;
        Ok(file)
    }

    /// The copy's private path. Valid tokens are hex-only, so this can
    /// never name anything outside `recovery/`.
    fn private_path(&self, token: &str) -> PathBuf {
        self.recovery.join(format!("{FILE_PREFIX}{token}"))
    }

    /// A fresh `recovery/import-source-<token>` nobody holds, created
    /// new: an occupied name is never overwritten or removed — the
    /// next draw takes a different one.
    fn create_private(&self) -> Result<(PathBuf, String, Box<dyn WriteFile>), StoreError> {
        for _ in 0..NAME_ATTEMPTS {
            let mut bytes = [0u8; TOKEN_BYTES];
            self.random.fill(&mut bytes).map_err(|e| {
                StoreError::unavailable(format!("cannot name the import source copy: {e}"))
            })?;
            let token = hex(&bytes);
            let path = self.private_path(&token);
            match retry_interrupted(|| self.fs.create(&path)) {
                Ok(file) => return Ok((path, token, file)),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    // A failed create grants no ownership of this name.
                    return Err(StoreError::unavailable(format!(
                        "cannot create the import source copy: {e}"
                    )));
                }
            }
        }
        Err(StoreError::unavailable(
            "cannot create the import source copy: no free name",
        ))
    }
}

/// Map a failed seam call on a source: a vanished file is `NotFound`,
/// anything else `Unavailable`.
fn source_err(op: &str, source: &Path, e: io::Error) -> StoreError {
    match e.kind() {
        io::ErrorKind::NotFound => {
            StoreError::not_found(format!("import source {}", source.display()))
        }
        _ => StoreError::unavailable(format!(
            "cannot {op} the import source {}: {e}",
            source.display()
        )),
    }
}

/// Stream `source` into `dest` while hashing, bounded to `expected`
/// bytes plus one detection byte: a source that keeps producing data
/// (a file being appended to without end) cannot run the copy — or
/// the hasher — unbounded. Reads that fail `Interrupted` retry; any
/// other read or write error stops the copy. `progress` sees the
/// copied total after each chunk, and once at 0 before the first
/// read; breaking it cancels, and no descriptor comes back.
fn copy_bounded(
    source: &mut dyn Read,
    dest: &mut dyn Write,
    expected: u64,
    progress: &mut dyn FnMut(u64) -> ControlFlow<()>,
    source_label: &Path,
) -> Result<Copied, StoreError> {
    if progress(0).is_break() {
        return Ok(Copied::Cancelled);
    }
    let bound = expected.saturating_add(1);
    let mut hasher = Sha256::new();
    let mut consumed = 0u64;
    let mut chunk = vec![0u8; STREAM_CHUNK];
    while consumed < bound {
        let want = (bound - consumed).min(STREAM_CHUNK as u64) as usize;
        let n = retry_interrupted(|| source.read(&mut chunk[..want])).map_err(|e| {
            StoreError::unavailable(format!(
                "cannot read the import source {} after {consumed} bytes: {e}",
                source_label.display()
            ))
        })?;
        if n == 0 {
            break;
        }
        dest.write_all(&chunk[..n]).map_err(|e| {
            StoreError::unavailable(format!(
                "cannot write the import source copy after {consumed} bytes: {e}"
            ))
        })?;
        hasher.update(&chunk[..n]);
        consumed += n as u64;
        if progress(consumed).is_break() {
            return Ok(Copied::Cancelled);
        }
    }
    if consumed > expected {
        return Err(StoreError::unavailable(format!(
            "the import source {} grew while being copied: it had {expected} bytes when the copy began but yielded the byte at offset {expected}",
            source_label.display()
        )));
    }
    if consumed < expected {
        return Err(StoreError::unavailable(format!(
            "the import source {} ended after {consumed} bytes; it had {expected} when the copy began",
            source_label.display()
        )));
    }
    Ok(Copied::Done(ObjectId::from_parts(
        hasher.finalize().into(),
        consumed,
    )))
}

/// Hash exactly `expected` bytes of `file`, then read a single
/// detection byte: a copy that grew between the length check and now
/// is caught without hashing it unboundedly. Every mismatch is
/// `Corrupt` and the file stays where it lies — it is evidence, not
/// garbage.
fn verify(file: &mut dyn Read, snapshot: &ImportSourceSnapshot) -> Result<(), StoreError> {
    let token = &snapshot.token;
    let expected = snapshot.byte_length();
    let mut hasher = Sha256::new();
    let mut consumed = 0u64;
    let mut chunk = vec![0u8; STREAM_CHUNK];
    while consumed < expected {
        let want = (expected - consumed).min(STREAM_CHUNK as u64) as usize;
        let n = retry_interrupted(|| file.read(&mut chunk[..want])).map_err(|e| {
            StoreError::unavailable(format!(
                "cannot read the import source snapshot {token}: {e}"
            ))
        })?;
        if n == 0 {
            return Err(StoreError::corrupt(format!(
                "import source snapshot {token} ended after {consumed} bytes; {expected} were captured"
            )));
        }
        hasher.update(&chunk[..n]);
        consumed += n as u64;
    }
    let mut detection = [0u8; 1];
    let extra = retry_interrupted(|| file.read(&mut detection)).map_err(|e| {
        StoreError::unavailable(format!(
            "cannot read the import source snapshot {token}: {e}"
        ))
    })?;
    if extra > 0 {
        return Err(StoreError::corrupt(format!(
            "import source snapshot {token} is longer than the {expected} bytes captured"
        )));
    }
    let digest: [u8; 32] = hasher.finalize().into();
    if digest != snapshot.sha256() {
        return Err(StoreError::corrupt(format!(
            "import source snapshot {token} failed its hash check"
        )));
    }
    Ok(())
}

/// Whether `token` is exactly 32 lowercase hex digits — the whole
/// grammar of a snapshot token, so no token can name another
/// directory.
fn is_token(token: &str) -> bool {
    token.len() == TOKEN_LEN
        && token
            .bytes()
            .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f'))
}

/// Retry a seam call that failed `Interrupted`, as spec/03 requires
/// for the source and snapshot streams.
fn retry_interrupted<T>(mut call: impl FnMut() -> io::Result<T>) -> io::Result<T> {
    loop {
        match call() {
            Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
            result => return result,
        }
    }
}

/// Bytes as lowercase hex, as the object file names spell them.
fn hex(bytes: &[u8]) -> String {
    const DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(DIGITS[(byte >> 4) as usize] as char);
        out.push(DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

#[cfg(test)]
mod tests;
