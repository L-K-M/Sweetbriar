//! Verified private restore copy (spec/03 "Restore" steps 1-3, b1):
//! revalidate the retained selection against the supplied locator and
//! copy the database, objects and manifest into an exclusive sibling
//! stage. b2 qualifies the staged database; nothing here runs source SQL
//! or touches the live store, and a failure leaves only unqualified
//! evidence behind. A stale or failed stage is never removed or reused.

use std::cell::Cell;
use std::ffi::OsString;
use std::io::{self, Read};
use std::path::{Component, Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use sha2::{Digest, Sha256};

use super::stage_copy::{cancelled, copy_retained, copy_verified, io_error};
use super::stage_files::StageFiles;
use super::*;

/// Metadata a caller declares about the selected generation. Every field
/// is revalidated against the manifest bytes read from the verified
/// source capability. `completed_at` is caller-asserted bookkeeping, not
/// a verified timestamp.
#[derive(Debug, Clone, Copy)]
pub struct RestoreMetadata {
    /// SHA-256 of the exact manifest bytes the selection verified.
    pub manifest_sha256: [u8; 32],
    /// Number of object records in the manifest.
    pub object_count: i64,
    /// Database bytes plus every object record's bytes.
    pub total_bytes: i64,
    /// Caller-asserted completion time, absent from the manifest.
    pub completed_at: i64,
}

/// An opaque, unqualified private stage copy. Returned only after the
/// copied bytes were reverified and the required flushes succeeded. It
/// does NOT certify eligibility for a swap: b2 must inspect and quarantine
/// the staged database, recheck the binding fields, and only then expose
/// a qualified stage. The operation Arc lease is held until drop.
#[derive(Debug)]
pub struct PreparedRestoreStage {
    /// These capabilities are unqualified. A future qualifier must reverify
    /// bytes and compare their identities to the current names before writing.
    #[allow(dead_code)]
    pub(super) files: StageFiles,
    path: PathBuf,
    incarnation: String,
    generation_id: String,
    manifest_sha256: String,
    source_location: PathBuf,
    preserved_root: PathBuf,
    /// The live data root, the live incarnation the preserved root was
    /// derived from, the coordinator token, and the revalidated
    /// metadata. b2 must recheck every component before qualifying; a
    /// capability from another coordinator or store is refused.
    #[allow(dead_code)] // consumed by the b2 qualifier
    pub(crate) data_root: PathBuf,
    #[allow(dead_code)] // consumed by the b2 qualifier
    pub(crate) live_incarnation: String,
    #[allow(dead_code)] // consumed by the b2 qualifier
    pub(crate) metadata: RestoreMetadata,
    #[allow(dead_code)] // consumed by the b2 qualifier
    pub(crate) coordinator: Arc<()>,
    /// The backup-operation lease. Held until this capability (and the
    /// qualified stage b2 builds from it) is dropped.
    #[allow(dead_code)] // moved into the b2 qualified stage
    pub(crate) operation: Arc<()>,
}

impl PreparedRestoreStage {
    /// The exclusive staging root. Unqualified: the staged database has
    /// not been inspected or quarantined, so this path must not be
    /// published or swapped.
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
    /// The future `<data root>.preserved-<live incarnation>` root.
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
    /// The database was copied and its hash checked.
    DatabaseCopied,
    /// The object-copy pass finished against the selected manifest.
    ObjectsCopied,
    /// The manifest itself was copied into the stage.
    ManifestCopied,
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
/// `<name><suffix><id>`, sharing `root`'s parent.
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
    bytes: u64,
}
impl<R: Read> Read for HashReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.input.read(buf)?;
        self.bytes = self.bytes.checked_add(n as u64).ok_or_else(|| {
            io::Error::new(io::ErrorKind::InvalidData, "manifest length overflow")
        })?;
        self.hash.update(&buf[..n]);
        Ok(n)
    }
}

/// Parse the source manifest once, hashing the exact bytes and handing
/// each object record to `sink`; returns its metadata, digest, object
/// count, object-byte total and exact manifest byte length.
fn parse_manifest(
    source: &dyn sb_types::fs::ConfinedDir,
    cancel: &AtomicBool,
    mut sink: impl FnMut(manifest::Entry) -> Result<(), StoreError>,
) -> Result<(manifest::Manifest, [u8; 32], u64, u64, u64), StoreError> {
    let (file, _) = source
        .open_regular(Path::new("manifest.json"))
        .map_err(|error| io_error("reading the selected manifest", error))?;
    let mut input = HashReader {
        input: file,
        hash: Sha256::new(),
        bytes: 0,
    };
    let mut count = 0u64;
    let mut object_bytes = 0u64;
    let mut sink_error = None;
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
            sink(entry).map_err(|error| {
                sink_error = Some(error);
                io::Error::other("manifest sink failed")
            })
        },
    );
    let digest: [u8; 32] = input.hash.finalize().into();
    if let Some(error) = sink_error {
        return Err(error);
    }
    let manifest = parsed.map_err(|error| io_error("parsing the selected manifest", error))?;
    Ok((manifest, digest, count, object_bytes, input.bytes))
}

/// Create `name` below `parent` when absent, open it, and flush the
/// parent. A pre-existing non-directory or symlink fails here.
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

/// Copy one manifest object through the confined capabilities, creating
/// its two-level hash directory and flushing file and directories.
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

impl Backups {
    /// Verify the selected external generation again and copy it into an
    /// exclusive sibling of this store's data root. Blocking file I/O;
    /// never call it on the writer thread's behalf. Failures return
    /// `Err` and leave the live store and source unchanged. The returned
    /// capability is unqualified: b2 must quarantine it before a swap.
    pub(crate) fn stage_restore(
        &self,
        selection: VerifiedExternalBackup,
        source_location: &Path,
        metadata: RestoreMetadata,
        cancel: &AtomicBool,
    ) -> Result<PreparedRestoreStage, StoreError> {
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

    /// Stage under an explicit incarnation. The public entry point draws
    /// a fresh one; tests use this for deterministic path checks.
    pub(crate) fn stage_into(
        &self,
        selection: &VerifiedExternalBackup,
        source_location: &Path,
        metadata: RestoreMetadata,
        new_incarnation: &str,
        cancel: &AtomicBool,
        mut progress: impl FnMut(StagePhase),
    ) -> Result<PreparedRestoreStage, StoreError> {
        let operation = self.reserve_operation()?;
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
        let conn = open_connection(&self.state_db, Access::ReadOnly, READER_PRAGMAS)?;
        let current: String = conn
            .query_row(
                "SELECT value FROM store_meta WHERE key='incarnation'",
                [],
                |row| row.get(0),
            )
            .map_err(|error| classify(error, "read the live store incarnation"))?;
        let parsed: Uuid = current
            .parse()
            .map_err(|_| StoreError::corrupt("the live store incarnation is invalid"))?;
        if parsed.to_string() != current {
            return Err(StoreError::corrupt("the live store incarnation is invalid"));
        }
        conn.close()
            .map_err(|(_, error)| classify(error, "close the live store incarnation read"))?;
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
        let (manifest, digest, count, object_bytes, manifest_bytes) =
            parse_manifest(source.as_ref(), cancel, |_| Ok(()))?;
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
        // Step 2: the exclusive sibling stage root. Its path refuses
        // pre-existing data; nothing is ever removed on failure.
        let stage_parent_path = data_root.parent().expect("a data root has a parent");
        let stage_parent = self
            .fs
            .open_confined_dir(stage_parent_path)
            .map_err(|error| {
                io_error(
                    &format!("opening the staging parent {}", stage_parent_path.display()),
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

        let database = copy_retained(
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
        let copied = Cell::new(0u64);
        let (_, copied_digest, _, _, _) = parse_manifest(source.as_ref(), cancel, |entry| {
            copy_object(source.as_ref(), sha_root.as_ref(), &entry, cancel)?;
            copied.set(copied.get() + 1);
            Ok(())
        })?;
        if copied_digest != digest {
            return Err(StoreError::corrupt(
                "the selected manifest changed during object copying",
            ));
        }
        if copied.get() != count {
            return Err(StoreError::corrupt(
                "the staged object count differs from the manifest",
            ));
        }

        progress(StagePhase::ObjectsCopied);

        // Reuse the verified copier for the manifest too: its exact first-pass
        // length stops a growing source before it can fill unverified disk space.
        copy_verified(
            source.as_ref(),
            Path::new("manifest.json"),
            stage.as_ref(),
            Path::new("manifest.json"),
            digest,
            manifest_bytes,
            cancel,
        )?;
        sha_root
            .sync()
            .map_err(|error| io_error("flushing the staged object hash root", error))?;
        objects
            .sync()
            .map_err(|error| io_error("flushing the staged objects directory", error))?;
        stage
            .sync()
            .map_err(|error| io_error("flushing the staged generation", error))?;
        progress(StagePhase::ManifestCopied);
        if cancel.load(Ordering::Acquire) {
            return Err(cancelled());
        }

        let stage_path = data_root
            .parent()
            .expect("a data root has a parent")
            .join(&stage_name);
        Ok(PreparedRestoreStage {
            files: StageFiles::new(
                stage,
                database,
                manifest.database.sha256,
                manifest.database.bytes,
            )
            .map_err(|error| io_error("retaining the staging file identities", error))?,
            path: stage_path,
            incarnation: new_incarnation.to_owned(),
            generation_id: generation.to_owned(),
            manifest_sha256: to_hex(&digest),
            source_location: source_location.to_path_buf(),
            preserved_root,
            data_root,
            live_incarnation: current,
            metadata,
            coordinator: Arc::clone(&self.coordinator),
            operation,
        })
    }
}
