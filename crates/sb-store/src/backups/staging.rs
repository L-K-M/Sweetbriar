//! Quarantined restore staging (spec/03 "Restore" steps 1-3): verify
//! the selected generation again, copy it into an exclusive sibling
//! stage, then run the reviewed quarantine transaction there. Nothing
//! here swaps roots, writes a restore marker, quiesces the engine or
//! writes the live database. A failed attempt leaves unqualified
//! evidence behind and never returns a stage.
//!
//! Retry and stale-stage behavior: every attempt draws a fresh
//! incarnation and creates a new exclusive `<data root>.restoring-<inc>`
//! directory. A path that already exists is refused, never overwritten;
//! a stale or failed stage is never removed or reused by this API, and
//! stays as evidence for later recovery.

use std::cell::RefCell;
use std::ffi::OsString;
use std::io::{self, Read, Write};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use rusqlite::{Connection, OptionalExtension};
use sha2::{Digest, Sha256};

use super::*;

/// One bounded copy buffer: 64 KiB, as the publication protocol uses.
const CHUNK: usize = 64 * 1024;

/// The tables and columns the reviewed quarantine transaction reads or
/// writes, with the declared types its statements rely on. Checked on a
/// private connection before any untrusted SQL runs. Inherited programs
/// are not executed by the transaction itself: its authorizer denies
/// every action a trigger or view would take.
const REQUIRED_COLUMNS: &[(&str, &[(&str, &str)])] = &[
    ("store_meta", &[("key", "TEXT"), ("value", "TEXT")]),
    ("accounts", &[("account_id", "TEXT"), ("state", "TEXT")]),
    ("import_jobs", &[("job_id", "TEXT"), ("state", "TEXT")]),
    (
        "intents",
        &[
            ("intent_id", "TEXT"),
            ("state", "TEXT"),
            ("quarantine_reason", "TEXT"),
        ],
    ),
    (
        "send_attempts",
        &[("attempt_id", "TEXT"), ("state", "TEXT")],
    ),
    (
        "backup_generations",
        &[
            ("generation_id", "TEXT"),
            ("started_at", "INTEGER"),
            ("completed_at", "INTEGER"),
            ("state", "TEXT"),
            ("location", "TEXT"),
            ("manifest_sha256", "TEXT"),
            ("object_count", "INTEGER"),
            ("total_bytes", "INTEGER"),
        ],
    ),
    (
        "object_pins",
        &[
            ("object_sha256", "TEXT"),
            ("pin_kind", "TEXT"),
            ("pin_ref", "TEXT"),
            ("created_at", "INTEGER"),
        ],
    ),
    (
        "backup_ownership",
        &[("generation_id", "TEXT"), ("token", "TEXT")],
    ),
    ("backup_cleanup_pending", &[("generation_id", "TEXT")]),
    (
        "verifier_scratch",
        &[("id", "TEXT"), ("token", "TEXT"), ("incarnation", "TEXT")],
    ),
    (
        "verifier_objects",
        &[
            ("scratch_id", "TEXT"),
            ("sha256", "BLOB"),
            ("bytes", "BLOB"),
            ("referenced", "INTEGER"),
        ],
    ),
];

/// Metadata a caller declares about the selected generation. Every
/// field is revalidated against the manifest bytes read from the
/// verified source capability: a mismatch fails the stage before it
/// qualifies anything.
#[derive(Debug, Clone, Copy)]
pub struct RestoreMetadata {
    /// SHA-256 of the exact manifest bytes the selection verified.
    pub manifest_sha256: [u8; 32],
    /// Number of object records in the manifest.
    pub object_count: i64,
    /// Database bytes plus every object record's bytes.
    pub total_bytes: i64,
    /// When the selected generation reached `complete`; recorded on
    /// its quarantined row.
    pub completed_at: i64,
}

/// An opaque, qualified restore stage. Exposed only after the copied
/// bytes were reverified, the quarantine transaction committed, the
/// staging SQLite connection checkpointed and closed, and the required
/// file, directory and parent flushes succeeded.
#[derive(Debug)]
pub struct RestoreStage {
    path: PathBuf,
    incarnation: String,
    generation_id: String,
    manifest_sha256: String,
    source_location: PathBuf,
    preserved_root: PathBuf,
}

impl RestoreStage {
    /// The exclusive staging root `<data root>.restoring-<incarnation>`.
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// The new incarnation the staged store will carry.
    pub fn incarnation(&self) -> &str {
        &self.incarnation
    }
    /// The restored-from generation's identifier.
    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }
    /// SHA-256 of the exact verified manifest bytes.
    pub fn manifest_sha256(&self) -> &str {
        &self.manifest_sha256
    }
    /// The validated locator the selection was reopened from.
    pub fn source_location(&self) -> &Path {
        &self.source_location
    }
    /// The future `<data root>.preserved-<live incarnation>` root, whose
    /// suffix comes from the live store, not the source database.
    pub fn preserved_root(&self) -> &Path {
        &self.preserved_root
    }
}

/// Progress points a test may observe to cancel mid-stage
/// deterministically. Production callers pass an empty closure.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum StagePhase {
    /// The supplied locator reopened the verified generation directory.
    Located,
    /// The selected manifest digest and metadata were revalidated.
    ManifestVerified,
    /// The database was copied and its hash checked.
    DatabaseCopied,
    /// One referenced object was copied and its hash checked.
    ObjectCopied,
    /// The manifest itself was copied into the stage.
    ManifestCopied,
    /// The quarantine transaction committed and the database closed.
    Quarantined,
}

fn io_error(context: &str, error: io::Error) -> StoreError {
    if error.kind() == io::ErrorKind::InvalidData {
        StoreError::corrupt(format!(
            "invalid restore staging input while {context}: {error}"
        ))
    } else {
        StoreError::unavailable_io(
            format!("restore staging I/O while {context}: {error}"),
            error.kind(),
        )
    }
}

fn cancelled() -> StoreError {
    StoreError::unavailable("restore staging cancelled")
}

fn interrupted() -> io::Error {
    io::Error::from(io::ErrorKind::Interrupted)
}

fn to_hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

fn valid_absolute(path: &Path) -> bool {
    path.is_absolute()
        && path
            .components()
            .all(|part| matches!(part, Component::RootDir | Component::Normal(_)))
}

/// A sibling name derived from `root`'s final component:
/// `<name><suffix><id>`. The parent is shared with `root`, so the new
/// entry never crosses a filesystem boundary.
fn sibling_name(root: &Path, suffix: &str, id: &str) -> OsString {
    let mut name = root
        .file_name()
        .expect("a data root always has a final name")
        .to_os_string();
    name.push(suffix);
    name.push(id);
    name
}

struct HashReader<R> {
    input: R,
    hash: Sha256,
}
impl<R: Read> Read for HashReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.input.read(buf)?;
        self.hash.update(&buf[..n]);
        Ok(n)
    }
}

/// Create `name` below `parent` when absent, open it, and flush the
/// parent so the new entry is durable. A pre-existing non-directory or
/// symlink cannot be opened as a directory and fails here.
fn ensure_child_dir(
    parent: &dyn sb_types::fs::ConfinedDir,
    name: &str,
) -> Result<Box<dyn sb_types::fs::ConfinedDir>, StoreError> {
    match parent.create_private_dir(Path::new(name)) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {}
        Err(error) => {
            return Err(io_error(
                &format!("creating the staging child {name}"),
                error,
            ));
        }
    }
    let dir = parent
        .open_dir(Path::new(name))
        .map_err(|error| io_error(&format!("opening the staging child {name}"), error))?;
    parent.sync().map_err(|error| {
        io_error(
            &format!("flushing the parent of staging child {name}"),
            error,
        )
    })?;
    Ok(dir)
}

/// Stream `from` below `source` into a fresh file `to` below
/// `dest_parent`, checking cancellation per chunk and verifying the
/// exact bytes written against `expected_sha` and `expected_bytes`.
/// Memory stays bounded by [`CHUNK`]; a failure leaves unqualified
/// partial evidence behind.
fn copy_verified(
    source: &dyn sb_types::fs::ConfinedDir,
    from: &Path,
    dest_parent: &dyn sb_types::fs::ConfinedDir,
    to: &Path,
    expected_sha: [u8; 32],
    expected_bytes: u64,
    cancel: &AtomicBool,
) -> Result<(), StoreError> {
    let (mut input, _) = source
        .open_regular(from)
        .map_err(|error| io_error(&format!("reading {}", from.display()), error))?;
    let mut output = dest_parent
        .create(to)
        .map_err(|error| io_error(&format!("creating {}", to.display()), error))?;
    let mut hash = Sha256::new();
    let mut length = 0u64;
    let mut buffer = [0u8; CHUNK];
    loop {
        if cancel.load(Ordering::Acquire) {
            return Err(cancelled());
        }
        let n = input
            .read(&mut buffer)
            .map_err(|error| io_error(&format!("reading {}", from.display()), error))?;
        if n == 0 {
            break;
        }
        length = length
            .checked_add(n as u64)
            .ok_or_else(|| StoreError::corrupt("restore staging length overflow"))?;
        if length > expected_bytes {
            return Err(StoreError::corrupt(
                "restore source is longer than its manifest declares",
            ));
        }
        output
            .write_all(&buffer[..n])
            .map_err(|error| io_error(&format!("writing {}", to.display()), error))?;
        hash.update(&buffer[..n]);
    }
    if length != expected_bytes || hash.finalize().as_slice() != expected_sha {
        return Err(StoreError::corrupt(
            "restore source digest or length does not match the manifest",
        ));
    }
    output
        .sync_all()
        .map_err(|error| io_error(&format!("flushing {}", to.display()), error))?;
    Ok(())
}

/// Copy one manifest object through the confined capabilities, creating
/// its two-level hash directory and flushing the file and both
/// directories, as the publication protocol does.
fn copy_object(
    source: &dyn sb_types::fs::ConfinedDir,
    sha_root: &dyn sb_types::fs::ConfinedDir,
    entry: &manifest::Entry,
    cancel: &AtomicBool,
) -> Result<(), StoreError> {
    let hash = to_hex(&entry.sha256);
    let aa = ensure_child_dir(sha_root, &hash[..2])?;
    let bb = ensure_child_dir(aa.as_ref(), &hash[2..4])?;
    let relative = Path::new("objects/sha256")
        .join(&hash[..2])
        .join(&hash[2..4])
        .join(&hash);
    copy_verified(
        source,
        &relative,
        bb.as_ref(),
        Path::new(&hash),
        entry.sha256,
        entry.bytes,
        cancel,
    )?;
    bb.sync()
        .map_err(|error| io_error("flushing the staging object leaf", error))?;
    aa.sync()
        .map_err(|error| io_error("flushing the staging object directory", error))?;
    Ok(())
}

/// Run the reviewed quarantine transaction while observing the caller's
/// live cancellation flag: a short monitor mirrors the flag into the
/// shared cell the transaction's progress handler reads.
fn apply_quarantine_live(
    conn: &mut Connection,
    input: quarantine::Quarantine<'_>,
    cancel: &AtomicBool,
) -> Result<(), StoreError> {
    let shared = Arc::new(AtomicBool::new(cancel.load(Ordering::Acquire)));
    let done = Arc::new(AtomicBool::new(false));
    std::thread::scope(|scope| {
        let watch = Arc::clone(&shared);
        let stop = Arc::clone(&done);
        let monitor = std::thread::Builder::new()
            .name("restore-quarantine-cancel".into())
            .spawn_scoped(scope, move || {
                while !stop.load(Ordering::Acquire) {
                    if cancel.load(Ordering::Acquire) {
                        watch.store(true, Ordering::Release);
                        return;
                    }
                    std::thread::park_timeout(Duration::from_millis(2));
                }
            })
            .map_err(|error| {
                StoreError::unavailable_io(
                    "cannot start restore quarantine cancellation monitor",
                    error.kind(),
                )
            })?;
        let outcome = quarantine::apply(
            conn,
            &quarantine::Quarantine {
                cancel: &shared,
                ..input
            },
        );
        done.store(true, Ordering::Release);
        monitor.thread().unpark();
        outcome
    })
}

impl Backups {
    /// Verify the selected external generation again and stage it as a
    /// quarantined sibling of this store's data root. Blocking file I/O
    /// and SQLite work; never call it on the writer thread's behalf
    /// without offloading. Cancellation, I/O, SQL or flush failures
    /// return `Err` and leave the live store and source unchanged.
    pub(crate) fn stage_restore(
        &self,
        selection: VerifiedExternalBackup,
        source_location: &Path,
        metadata: RestoreMetadata,
        cancel: &AtomicBool,
    ) -> Result<RestoreStage, StoreError> {
        let incarnation = Uuid::from_random_bytes(self.draw_id()?).to_string();
        self.stage_into(
            &selection,
            source_location,
            metadata,
            &incarnation,
            cancel,
            |_| {},
        )
    }

    /// Test-only variant exposing the deterministic progress points.
    #[cfg(test)]
    pub(crate) fn stage_restore_with(
        &self,
        selection: VerifiedExternalBackup,
        source_location: &Path,
        metadata: RestoreMetadata,
        cancel: &AtomicBool,
        progress: impl FnMut(StagePhase),
    ) -> Result<RestoreStage, StoreError> {
        let incarnation = Uuid::from_random_bytes(self.draw_id()?).to_string();
        self.stage_into(
            &selection,
            source_location,
            metadata,
            &incarnation,
            cancel,
            progress,
        )
    }

    /// Stage under an explicit incarnation. The public entry point draws
    /// a fresh one; tests use this to exercise a reused or colliding
    /// incarnation path deterministically.
    pub(crate) fn stage_into(
        &self,
        selection: &VerifiedExternalBackup,
        source_location: &Path,
        metadata: RestoreMetadata,
        new_incarnation: &str,
        cancel: &AtomicBool,
        mut progress: impl FnMut(StagePhase),
    ) -> Result<RestoreStage, StoreError> {
        let _operation = self.reserve_operation()?;
        if cancel.load(Ordering::Acquire) {
            return Err(cancelled());
        }
        if metadata.manifest_sha256 != selection.manifest_sha256
            || metadata.object_count < 0
            || metadata.total_bytes < 0
            || metadata.completed_at < 0
        {
            return Err(StoreError::corrupt(
                "restore metadata does not match the verified selection",
            ));
        }
        let incarnation: Uuid = new_incarnation
            .parse()
            .map_err(|_| StoreError::corrupt("invalid restore incarnation"))?;
        if incarnation.to_string() != new_incarnation {
            return Err(StoreError::corrupt("invalid restore incarnation"));
        }
        if !valid_absolute(source_location) {
            return Err(StoreError::InvalidInput {
                message: format!(
                    "the restore source location {} is not an absolute path",
                    source_location.display()
                ),
            });
        }
        let generation = selection.generation_id();
        if source_location.file_name().and_then(|name| name.to_str()) != Some(generation) {
            return Err(StoreError::corrupt(
                "the restore source location does not name the selected generation",
            ));
        }
        let location_text = source_location
            .to_str()
            .ok_or_else(|| StoreError::corrupt("the restore source location is not UTF-8"))?
            .to_owned();

        // The handoff retains identity but no path: the supplied locator
        // must reopen that same directory through a confined capability.
        // No pathname-based source read ever follows.
        let parent_path = source_location
            .parent()
            .ok_or_else(|| StoreError::corrupt("the restore source location has no parent"))?;
        let parent = self
            .fs
            .open_confined_dir(parent_path)
            .map_err(|error| io_error(&format!("opening {}", parent_path.display()), error))?;
        let source = parent
            .open_dir(Path::new(generation))
            .map_err(|error| io_error(&format!("reopening generation {generation}"), error))?;
        if source.identity().map_err(|error| {
            io_error(&format!("reading generation {generation} identity"), error)
        })? != selection.identity
        {
            return Err(StoreError::corrupt(
                "the restore source locator no longer names the verified generation",
            ));
        }
        progress(StagePhase::Located);

        // The future preserved-root suffix comes from the live store's
        // incarnation, never from the source database's copy.
        let data_root = self
            .state_db
            .parent()
            .expect("the state database has a parent")
            .to_path_buf();
        let current = self.live_incarnation()?;
        if current == new_incarnation {
            return Err(StoreError::conflict(
                "the restore incarnation reuses the live store incarnation",
            ));
        }
        let preserved_root = data_root
            .parent()
            .expect("a data root always has a parent")
            .join(sibling_name(&data_root, ".preserved-", &current));

        // Step 1: reverify the exact selected manifest bytes and the
        // caller's metadata against what the source serves now.
        let (manifest_file, _) = source
            .open_regular(Path::new("manifest.json"))
            .map_err(|error| io_error("reading the selected manifest", error))?;
        let mut input = HashReader {
            input: manifest_file,
            hash: Sha256::new(),
        };
        let mut count = 0u64;
        let mut object_bytes = 0u64;
        let parsed = manifest::parse(
            &mut input,
            || {
                if cancel.load(Ordering::Acquire) {
                    Err(interrupted())
                } else {
                    Ok(())
                }
            },
            |entry| {
                if cancel.load(Ordering::Acquire) {
                    return Err(interrupted());
                }
                count = count.checked_add(1).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "manifest object count overflow")
                })?;
                object_bytes = object_bytes.checked_add(entry.bytes).ok_or_else(|| {
                    io::Error::new(io::ErrorKind::InvalidData, "manifest object bytes overflow")
                })?;
                Ok(())
            },
        );
        let digest: [u8; 32] = input.hash.finalize().into();
        let manifest = parsed.map_err(|error| io_error("parsing the selected manifest", error))?;
        if reconciliation::uuid(manifest.generation_id) != generation {
            return Err(StoreError::corrupt(
                "the selected manifest names a different generation",
            ));
        }
        if digest != selection.manifest_sha256 {
            return Err(StoreError::corrupt(
                "the selected manifest changed before staging",
            ));
        }
        let total_bytes = object_bytes
            .checked_add(manifest.database.bytes)
            .ok_or_else(|| StoreError::corrupt("manifest byte total overflow"))?;
        if count != metadata.object_count as u64 || total_bytes != metadata.total_bytes as u64 {
            return Err(StoreError::corrupt(
                "restore metadata does not match the selected manifest",
            ));
        }
        progress(StagePhase::ManifestVerified);

        // Step 2: the exclusive sibling stage root. Its path refuses
        // pre-existing data; nothing is ever removed on failure.
        let stage_parent = self
            .fs
            .open_confined_dir(data_root.parent().expect("a data root has a parent"))
            .map_err(|error| {
                io_error(
                    &format!(
                        "opening the staging parent {}",
                        data_root.parent().unwrap().display()
                    ),
                    error,
                )
            })?;
        let stage_name = sibling_name(&data_root, ".restoring-", new_incarnation);
        match stage_parent.create_private_dir(Path::new(&stage_name)) {
            Ok(()) => {}
            Err(error) if error.kind() == io::ErrorKind::AlreadyExists => {
                return Err(StoreError::conflict(
                    "restore staging directory already exists",
                ));
            }
            Err(error) => {
                return Err(io_error(
                    &format!("creating the staging root {stage_name:?}"),
                    error,
                ));
            }
        }
        stage_parent
            .sync()
            .map_err(|error| io_error("flushing the staging root entry", error))?;
        let stage = stage_parent
            .open_dir(Path::new(&stage_name))
            .map_err(|error| io_error("opening the staging root", error))?;

        // A later swap makes this directory the data root, so the whole
        // layout must exist even though the generation carries only the
        // database, objects and manifest.
        let objects = ensure_child_dir(stage.as_ref(), "objects")?;
        ensure_child_dir(objects.as_ref(), "tmp")?;
        let sha_root = ensure_child_dir(objects.as_ref(), "sha256")?;
        ensure_child_dir(stage.as_ref(), "backups")?;
        ensure_child_dir(stage.as_ref(), "recovery")?;

        copy_verified(
            source.as_ref(),
            Path::new(SNAPSHOT_FILE),
            stage.as_ref(),
            Path::new(SNAPSHOT_FILE),
            manifest.database.sha256,
            manifest.database.bytes,
            cancel,
        )?;
        stage
            .sync()
            .map_err(|error| io_error("flushing the staged database", error))?;
        progress(StagePhase::DatabaseCopied);

        // Copy exactly the referenced objects in bounded memory: the
        // streaming parser hands each record to the sink as it arrives.
        let copied = RefCell::new(0u64);
        let failure = RefCell::new(None);
        let (manifest_file, _) = source
            .open_regular(Path::new("manifest.json"))
            .map_err(|error| io_error("rereading the selected manifest", error))?;
        let mut input = HashReader {
            input: manifest_file,
            hash: Sha256::new(),
        };
        let parsed = manifest::parse(
            &mut input,
            || {
                if cancel.load(Ordering::Acquire) {
                    Err(interrupted())
                } else {
                    Ok(())
                }
            },
            |entry| {
                if cancel.load(Ordering::Acquire) {
                    return Err(interrupted());
                }
                if let Err(error) = copy_object(source.as_ref(), sha_root.as_ref(), &entry, cancel)
                {
                    *failure.borrow_mut() = Some(error);
                    return Err(io::Error::other("object copy failed"));
                }
                *copied.borrow_mut() += 1;
                progress(StagePhase::ObjectCopied);
                Ok(())
            },
        );
        let copied_digest: [u8; 32] = input.hash.finalize().into();
        if let Some(error) = failure.into_inner() {
            return Err(error);
        }
        parsed.map_err(|error| io_error("reparsing the selected manifest", error))?;
        if copied_digest != digest {
            return Err(StoreError::corrupt(
                "the selected manifest changed during object copying",
            ));
        }
        if *copied.borrow() != count {
            return Err(StoreError::corrupt(
                "the staged object count differs from the manifest",
            ));
        }

        // The manifest itself is copied last, from a fresh read of the
        // source so the staged bytes are checked against the exact
        // selected digest once more.
        let (mut manifest_file, _) = source
            .open_regular(Path::new("manifest.json"))
            .map_err(|error| io_error("rereading the selected manifest for copying", error))?;
        let mut staged_manifest = stage
            .create(Path::new("manifest.json"))
            .map_err(|error| io_error("creating the staged manifest", error))?;
        let mut staged_hash = Sha256::new();
        let mut buffer = [0u8; CHUNK];
        loop {
            if cancel.load(Ordering::Acquire) {
                return Err(cancelled());
            }
            let n = manifest_file
                .read(&mut buffer)
                .map_err(|error| io_error("rereading the selected manifest for copying", error))?;
            if n == 0 {
                break;
            }
            staged_manifest
                .write_all(&buffer[..n])
                .map_err(|error| io_error("writing the staged manifest", error))?;
            staged_hash.update(&buffer[..n]);
        }
        if staged_hash.finalize().as_slice() != digest {
            return Err(StoreError::corrupt(
                "the selected manifest changed during staging",
            ));
        }
        staged_manifest
            .sync_all()
            .map_err(|error| io_error("flushing the staged manifest", error))?;
        sha_root
            .sync()
            .map_err(|error| io_error("flushing the staged object hash root", error))?;
        objects
            .sync()
            .map_err(|error| io_error("flushed the staged objects directory", error))?;
        stage
            .sync()
            .map_err(|error| io_error("flushing the staged generation", error))?;
        progress(StagePhase::ManifestCopied);

        let stage_path = data_root
            .parent()
            .expect("a data root has a parent")
            .join(&stage_name);

        // Inspect compatibility on a private connection first: the
        // quarantine transaction must never run against a schema it
        // did not expect, nor execute an inherited program.
        let staged_db = stage_path.join(SNAPSHOT_FILE);
        inspect_stage_schema(&staged_db)?;

        let mut conn = Connection::open(&staged_db)
            .map_err(|error| classify(error, "open restore staging"))?;
        conn.execute_batch("PRAGMA trusted_schema=OFF;")
            .map_err(|error| classify(error, "harden restore staging"))?;
        let manifest_sha256 = to_hex(&digest);
        let unused_cancel = Arc::new(AtomicBool::new(false));
        let applied = apply_quarantine_live(
            &mut conn,
            quarantine::Quarantine {
                new_incarnation,
                generation,
                source_location: &location_text,
                current_root: &data_root,
                preserved_root: &preserved_root,
                manifest_sha256: &manifest_sha256,
                object_count: metadata.object_count,
                total_bytes: metadata.total_bytes,
                completed_at: metadata.completed_at,
                cancel: &unused_cancel,
            },
            cancel,
        );
        applied?;
        let busy: i64 = conn
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .map_err(|error| classify(error, "checkpoint restore staging"))?;
        if busy != 0 {
            return Err(StoreError::unavailable(
                "restore staging checkpoint could not complete",
            ));
        }
        conn.close()
            .map_err(|(_, error)| classify(error, "close restore staging"))?;
        progress(StagePhase::Quarantined);

        // Qualify only after the checkpoint, close and every required
        // flush succeeded.
        stage
            .sync()
            .map_err(|error| io_error("flushing the qualified stage", error))?;
        stage_parent
            .sync()
            .map_err(|error| io_error("flushing the qualified stage parent", error))?;
        Ok(RestoreStage {
            path: stage_path,
            incarnation: new_incarnation.to_owned(),
            generation_id: generation.to_owned(),
            manifest_sha256,
            source_location: source_location.to_path_buf(),
            preserved_root,
        })
    }

    /// The live store's current incarnation, validated as a canonical
    /// UUID. The staged database's own value is irrelevant here.
    fn live_incarnation(&self) -> Result<String, StoreError> {
        let conn = open_connection(&self.state_db, Access::ReadOnly, READER_PRAGMAS)?;
        let incarnation: String = conn
            .query_row(
                "SELECT value FROM store_meta WHERE key='incarnation'",
                [],
                |row| row.get(0),
            )
            .map_err(|error| classify(error, "read the live store incarnation"))?;
        let parsed: Uuid = incarnation
            .parse()
            .map_err(|_| StoreError::corrupt("the live store incarnation is invalid"))?;
        if parsed.to_string() != incarnation {
            return Err(StoreError::corrupt("the live store incarnation is invalid"));
        }
        conn.close()
            .map_err(|(_, error)| classify(error, "close the live store incarnation read"))?;
        Ok(incarnation)
    }
}

/// Verify the staged database exposes exactly the tables and columns
/// the reviewed quarantine transaction expects, with no inherited
/// trigger or view substituting for one of them. Reads only; the
/// caller opens a separate fresh connection for the transaction itself,
/// whose authorizer denies every action an inherited program would take.
fn inspect_stage_schema(stage_path: &Path) -> Result<(), StoreError> {
    let conn = Connection::open(stage_path)
        .map_err(|error| classify(error, "open restore staging for schema inspection"))?;
    let result = (|| {
        conn.execute_batch("PRAGMA trusted_schema=OFF; PRAGMA query_only=ON;")
            .map_err(|error| classify(error, "configure restore staging inspection"))?;
        let version = crate::schema::read_user_version(&conn)?;
        if version <= 0 || version > i64::from(crate::schema::LATEST_SCHEMA_VERSION) {
            return Err(StoreError::corrupt(format!(
                "unsupported restore staging schema version {version}"
            )));
        }
        for (table, columns) in REQUIRED_COLUMNS {
            let kind: Option<String> = conn
                .query_row(
                    "SELECT type FROM sqlite_schema
                     WHERE name=?1 AND type IN ('table','view')",
                    [table],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|error| classify(error, "read restore staging schema"))?;
            if kind.as_deref() != Some("table") {
                return Err(StoreError::corrupt(format!(
                    "restore staging table {table} is missing or is not a table"
                )));
            }
            let mut statement = conn
                .prepare(&format!("PRAGMA table_info({table})"))
                .map_err(|error| classify(error, "inspect restore staging table"))?;
            let actual = statement
                .query_map([], |row| {
                    Ok((row.get::<_, String>(1)?, row.get::<_, String>(2)?))
                })
                .map_err(|error| classify(error, "inspect restore staging table"))?
                .collect::<Result<Vec<_>, _>>()
                .map_err(|error| classify(error, "inspect restore staging table"))?;
            for &(column, expected_type) in columns.iter() {
                let found = actual.iter().any(|(name, kind)| {
                    name.as_str() == column && kind.eq_ignore_ascii_case(expected_type)
                });
                if !found {
                    return Err(StoreError::corrupt(format!(
                        "restore staging table {table} lacks the expected {column} column"
                    )));
                }
            }
        }
        Ok(())
    })();
    conn.close()
        .map_err(|(_, error)| classify(error, "close restore staging inspection"))?;
    result
}
