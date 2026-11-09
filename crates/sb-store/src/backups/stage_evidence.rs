//! Staged evidence re-authentication (spec/03 "Restore", plan M1.8e3b
//! b6b3a). Before the qualifier quarantines a prepared stage, the
//! manifest and every object the stage holds are read again through the
//! stage's own retained root and authenticated against what staging
//! verified: the exact manifest bytes (length bounded by a one-byte
//! witness, then the digest), its generation and database entry, each
//! object's length and digest, and the object count and byte total the
//! caller declared. Object reads go through retained child directories
//! derived from the manifest's hex digests; no other name is opened.
//!
//! This certifies a point in time only. It grants no stage eligibility,
//! quarantine or swap authority, and a failure removes nothing.

use std::io::{self, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use sb_types::fs::ConfinedDir;
use sha2::{Digest, Sha256};

use super::manifest;
use super::reconciliation;
use super::stage_copy::{cancelled, io_error};
use super::stage_files::StageFiles;
use super::staging::to_hex;
use crate::StoreError;

/// One bounded read buffer, as staging's copier uses.
const CHUNK: usize = 64 * 1024;

/// The staged manifest's name inside the stage root.
const MANIFEST: &str = "manifest.json";

/// What staging verified; the stage is re-read against it.
#[derive(Debug, Clone, Copy)]
pub(super) struct Expected<'a> {
    /// SHA-256 of the exact manifest bytes staging copied.
    pub manifest_sha256: [u8; 32],
    /// Their exact length.
    pub manifest_bytes: u64,
    /// The restored-from generation's canonical identifier.
    pub generation_id: &'a str,
    /// The object records the caller declared.
    pub object_count: u64,
    /// The database bytes plus every object record's bytes.
    pub total_bytes: u64,
}

/// The re-authenticated manifest facts the qualifier consumes.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct StagedEvidence {
    /// The snapshot's `store_uuid`, canonical: the staged database must
    /// carry the same one.
    pub store_uuid: String,
    /// The snapshot's schema version.
    pub schema_version: i64,
}

/// Progress points a test may observe to cancel deterministically.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum EvidencePhase {
    /// The manifest's bytes, generation and database entry matched.
    ManifestAuthenticated,
    /// Every object matched, and the count and byte total.
    ObjectsAuthenticated,
}

/// Re-authenticate the stage's manifest and objects against `expected`.
/// Two passes over the manifest: the first authenticates its bytes
/// before any object is opened; the second streams the same records to
/// the object checks and must hash to the same digest, so a manifest
/// changed between the passes is refused. Blocking file I/O.
pub(super) fn reauthenticate(
    files: &StageFiles,
    expected: &Expected<'_>,
    cancel: &AtomicBool,
    mut progress: impl FnMut(EvidencePhase),
) -> Result<StagedEvidence, StoreError> {
    active(cancel)?;
    let root = files.root.as_ref();
    let manifest = read_manifest(root, expected, cancel, |_| Ok(()))?;
    if reconciliation::uuid(manifest.generation_id) != expected.generation_id {
        return Err(StoreError::corrupt(
            "the staged manifest names a different generation",
        ));
    }
    if manifest.database.sha256 != files.database_sha256
        || manifest.database.bytes != files.database_bytes
    {
        return Err(StoreError::corrupt(
            "the staged manifest names a different database",
        ));
    }
    progress(EvidencePhase::ManifestAuthenticated);
    active(cancel)?;

    let objects = open_child(root, "objects")?;
    let sha_root = open_child(objects.as_ref(), "sha256")?;
    let mut buffer = vec![0u8; CHUNK];
    let mut count = 0u64;
    let mut object_bytes = 0u64;
    read_manifest(root, expected, cancel, |entry| {
        count = count
            .checked_add(1)
            .ok_or_else(|| StoreError::corrupt("staged object count overflow"))?;
        object_bytes = object_bytes
            .checked_add(entry.bytes)
            .ok_or_else(|| StoreError::corrupt("staged object bytes overflow"))?;
        verify_object(sha_root.as_ref(), &entry, &mut buffer, cancel)
    })?;
    let total_bytes = object_bytes
        .checked_add(manifest.database.bytes)
        .ok_or_else(|| StoreError::corrupt("staged manifest byte total overflow"))?;
    if count != expected.object_count || total_bytes != expected.total_bytes {
        return Err(StoreError::corrupt(
            "the staged objects differ from the restore metadata",
        ));
    }
    progress(EvidencePhase::ObjectsAuthenticated);
    active(cancel)?;

    Ok(StagedEvidence {
        store_uuid: reconciliation::uuid(manifest.store_uuid),
        schema_version: manifest.schema_version,
    })
}

fn active(cancel: &AtomicBool) -> Result<(), StoreError> {
    if cancel.load(Ordering::Acquire) {
        return Err(cancelled());
    }
    Ok(())
}

/// Hashes and counts every byte the parser consumes.
struct Hashed<R> {
    input: R,
    hash: Sha256,
    bytes: u64,
}

impl<R: Read> Read for Hashed<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.input.read(buf)?;
        // `take` bounds the input at the expected length plus one, so
        // the count cannot overflow.
        self.bytes += n as u64;
        self.hash.update(&buf[..n]);
        Ok(n)
    }
}

/// Parse the staged manifest once, handing each object record to
/// `sink`. At most `manifest_bytes + 1` bytes are read: the witness byte
/// is how growth shows. The exact length and the digest are checked
/// after the whole bounded input was consumed, so a valid-looking
/// prefix or a changed body is refused.
fn read_manifest(
    root: &dyn ConfinedDir,
    expected: &Expected<'_>,
    cancel: &AtomicBool,
    mut sink: impl FnMut(manifest::Entry) -> Result<(), StoreError>,
) -> Result<manifest::Manifest, StoreError> {
    let (file, _) = root
        .open_regular(Path::new(MANIFEST))
        .map_err(|error| io_error("opening the staged manifest", error))?;
    let witness = expected
        .manifest_bytes
        .checked_add(1)
        .ok_or_else(|| StoreError::corrupt("staged manifest length overflow"))?;
    let mut input = Hashed {
        input: file.take(witness),
        hash: Sha256::new(),
        bytes: 0,
    };
    let mut sink_error = None;
    let parsed = manifest::parse(
        &mut input,
        || {
            if cancel.load(Ordering::Acquire) {
                Err(io::ErrorKind::Interrupted.into())
            } else {
                Ok(())
            }
        },
        |entry| {
            sink(entry).map_err(|error| {
                sink_error = Some(error);
                io::Error::other("staged manifest sink failed")
            })
        },
    );
    if let Some(error) = sink_error {
        return Err(error);
    }
    let manifest = match parsed {
        Ok(manifest) => manifest,
        Err(_) if cancel.load(Ordering::Acquire) => return Err(cancelled()),
        Err(error) => return Err(manifest_error(error)),
    };
    // The parser stops at the document's end; whatever the bounded input
    // still holds is growth and must be counted and hashed too.
    io::copy(&mut input, &mut io::sink())
        .map_err(|error| io_error("reading the staged manifest", error))?;
    if input.bytes != expected.manifest_bytes {
        return Err(StoreError::corrupt("the staged manifest length changed"));
    }
    let digest: [u8; 32] = input.hash.finalize().into();
    if digest != expected.manifest_sha256 {
        return Err(StoreError::corrupt("the staged manifest changed"));
    }
    Ok(manifest)
}

/// A manifest the parser refused is damaged evidence; a real I/O
/// failure keeps its native cause.
fn manifest_error(error: io::Error) -> StoreError {
    match error.kind() {
        io::ErrorKind::InvalidData | io::ErrorKind::UnexpectedEof => {
            StoreError::corrupt(format!("the staged manifest is invalid: {error}"))
        }
        _ => io_error("parsing the staged manifest", error),
    }
}

/// Open a direct child directory of the stage, never following links.
fn open_child(parent: &dyn ConfinedDir, name: &str) -> Result<Box<dyn ConfinedDir>, StoreError> {
    parent
        .open_dir(Path::new(name))
        .map_err(|error| io_error(&format!("opening the staged {name} directory"), error))
}

/// Stream one staged object from `objects/sha256/<aa>/<bb>/<hash>`
/// through confined child directories, bounded at its declared length
/// plus one witness byte, and check its length and digest.
fn verify_object(
    sha_root: &dyn ConfinedDir,
    entry: &manifest::Entry,
    buffer: &mut [u8],
    cancel: &AtomicBool,
) -> Result<(), StoreError> {
    active(cancel)?;
    let hash = to_hex(&entry.sha256);
    let aa = open_child(sha_root, &hash[..2])?;
    let bb = open_child(aa.as_ref(), &hash[2..4])?;
    let (mut file, _) = bb
        .open_regular(Path::new(&hash))
        .map_err(|error| io_error("opening a staged object", error))?;
    let limit = entry
        .bytes
        .checked_add(1)
        .ok_or_else(|| StoreError::corrupt("staged object length overflow"))?;
    let mut digest = Sha256::new();
    let mut total = 0u64;
    while total < limit {
        active(cancel)?;
        let want = usize::try_from((limit - total).min(buffer.len() as u64))
            .expect("bounded by the buffer length");
        let read = match file.read(&mut buffer[..want]) {
            Ok(0) => break,
            Ok(read) => read,
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(error) => return Err(io_error("reading a staged object", error)),
        };
        total += read as u64;
        digest.update(&buffer[..read]);
    }
    if total != entry.bytes {
        return Err(StoreError::corrupt("a staged object's length changed"));
    }
    let actual: [u8; 32] = digest.finalize().into();
    if actual != entry.sha256 {
        return Err(StoreError::corrupt("a staged object's bytes changed"));
    }
    Ok(())
}
