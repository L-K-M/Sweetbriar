//! Qualified binding handoff (spec/03 "Restore", plan M1.8e3b b6b3b):
//! the only path from a prepared restore stage to a qualified one. It
//! accepts only a capability from this coordinator holding the running
//! operation lease, rechecks the source, live and stage-name bindings
//! and the live incarnation, re-authenticates the staged manifest and
//! objects (b6b3a), and runs the guarded quarantine with its
//! checkpoint, close and flushes (b6b2). After those flushes every
//! binding and the evidence are checked again, and only then is the
//! qualified stage built.
//!
//! The qualified stage certifies a point in time, not immutability, and
//! it is not swap authority: the swap (M1.8f) reauthenticates the image
//! bytes and every name again. A refused or cancelled qualification
//! removes nothing; the stage stays behind as unqualified evidence, and
//! the preserved root is never created here.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Weak};

use super::quarantine::{self, Quarantine};
use super::stage_bindings::StageBindings;
use super::stage_copy::{cancelled, io_error};
use super::stage_evidence::{self, Expected, StagedEvidence};
use super::stage_files::StageFiles;
use super::stage_quarantine::{self, ImagePhase};
use super::staging::{read_live_incarnation, to_hex};
use super::{Backups, PreparedRestoreStage, RestoreMetadata, SNAPSHOT_FILE};
use crate::{Seams, StoreError};

/// Progress points a test may observe to cancel or interfere.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum QualifyPhase {
    /// The coordinator, the lease, every binding and the live
    /// incarnation matched.
    BindingsChecked,
    /// The staged manifest and objects matched, before the quarantine.
    EvidenceChecked,
    /// One of the quarantined image's phases (b6b2).
    Image(ImagePhase),
    /// After the flushes: every binding, the image identities and the
    /// evidence matched again.
    Rechecked,
}

/// A restore stage whose quarantined image and bindings were verified
/// together at one point in time. Not `Clone`; it holds the backup
/// operation lease until dropped. Nothing publishes it, and it grants
/// no swap: the swap reauthenticates bytes and names again.
#[derive(Debug)]
pub struct QualifiedStage {
    /// The retained stage handles the swap reauthenticates through.
    pub(super) files: StageFiles,
    /// The retained source and live identity leases.
    pub(super) bindings: StageBindings,
    pub(super) manifest_bytes: u64,
    /// The post-quarantine image's SHA-256 and length.
    pub(super) image_sha256: [u8; 32],
    pub(super) image_bytes: u64,
    pub(super) schema_version: i64,
    path: PathBuf,
    incarnation: String,
    generation_id: String,
    manifest_sha256: String,
    source_location: PathBuf,
    preserved_root: PathBuf,
    pub(super) data_root: PathBuf,
    pub(super) live_incarnation: String,
    pub(super) metadata: RestoreMetadata,
    pub(super) coordinator: Arc<()>,
    /// The backup-operation lease, held until the swap or drop.
    pub(super) operation: Arc<()>,
}

impl QualifiedStage {
    /// The qualified staging root. Not published: only the swap may
    /// rename it, after reauthenticating it.
    pub fn path(&self) -> &Path {
        &self.path
    }
    /// The incarnation the restored store carries.
    pub fn incarnation(&self) -> &str {
        &self.incarnation
    }
    /// The restored-from generation.
    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }
    /// SHA-256 of the generation's exact manifest bytes.
    pub fn manifest_sha256(&self) -> &str {
        &self.manifest_sha256
    }
    /// The locator the generation was restored from.
    pub fn source_location(&self) -> &Path {
        &self.source_location
    }
    /// The future `<data root>.preserved-<live incarnation>` root.
    pub fn preserved_root(&self) -> &Path {
        &self.preserved_root
    }
    /// The quarantined image's SHA-256, lowercase hex.
    pub fn image_sha256(&self) -> String {
        to_hex(&self.image_sha256)
    }
    /// The quarantined image's length.
    pub fn image_bytes(&self) -> u64 {
        self.image_bytes
    }
    /// The live data root the stage replaces.
    pub(crate) fn data_root(&self) -> &Path {
        &self.data_root
    }
}

/// What a stage is bound to, borrowed from a prepared or a qualified
/// stage, so the swap reruns exactly the qualifier's checks.
pub(super) struct Bound<'a> {
    pub bindings: &'a StageBindings,
    pub files: &'a StageFiles,
    pub data_root: &'a Path,
    pub live_incarnation: &'a str,
    pub path: &'a Path,
    pub incarnation: &'a str,
    pub source_location: &'a Path,
}

impl<'a> Bound<'a> {
    fn of_prepared(prepared: &'a PreparedRestoreStage, names: &'a Names) -> Self {
        Self {
            bindings: &prepared.bindings,
            files: &prepared.files,
            data_root: &prepared.data_root,
            live_incarnation: &prepared.live_incarnation,
            path: &names.path,
            incarnation: &names.incarnation,
            source_location: &names.source_location,
        }
    }

    pub(super) fn of_qualified(stage: &'a QualifiedStage) -> Self {
        Self {
            bindings: &stage.bindings,
            files: &stage.files,
            data_root: &stage.data_root,
            live_incarnation: &stage.live_incarnation,
            path: &stage.path,
            incarnation: &stage.incarnation,
            source_location: &stage.source_location,
        }
    }
}

/// The names a prepared stage carries, copied before its fields move.
struct Names {
    path: PathBuf,
    incarnation: String,
    generation_id: String,
    manifest_sha256: String,
    source_location: PathBuf,
    preserved_root: PathBuf,
}

fn active(cancel: &AtomicBool) -> Result<(), StoreError> {
    if cancel.load(Ordering::Acquire) {
        return Err(cancelled());
    }
    Ok(())
}

impl Backups {
    /// Qualify a prepared stage (see the module docs). Blocking file
    /// and SQLite I/O; never call it on the writer thread's behalf.
    pub(crate) fn qualify_restore(
        &self,
        prepared: PreparedRestoreStage,
        cancel: &Arc<AtomicBool>,
    ) -> Result<QualifiedStage, StoreError> {
        self.qualify_with(prepared, cancel, |_| {})
    }

    /// `qualify_restore` with progress points for tests.
    pub(super) fn qualify_with(
        &self,
        prepared: PreparedRestoreStage,
        cancel: &Arc<AtomicBool>,
        mut progress: impl FnMut(QualifyPhase),
    ) -> Result<QualifiedStage, StoreError> {
        active(cancel)?;
        // A capability from another coordinator or store never
        // qualifies, nor one whose lease is not the running operation.
        if !Arc::ptr_eq(&prepared.coordinator, &self.coordinator) {
            return Err(StoreError::conflict(
                "the restore stage belongs to another store",
            ));
        }
        self.holds_operation(&prepared.operation)?;
        let names = Names {
            path: prepared.path().to_path_buf(),
            incarnation: prepared.incarnation().to_owned(),
            generation_id: prepared.generation_id().to_owned(),
            manifest_sha256: prepared.manifest_sha256().to_owned(),
            source_location: prepared.source_location().to_path_buf(),
            preserved_root: prepared.preserved_root().to_path_buf(),
        };
        let source_location = names
            .source_location
            .to_str()
            .ok_or_else(|| StoreError::corrupt("the restore source location is not valid UTF-8"))?;
        if to_hex(&prepared.metadata.manifest_sha256) != names.manifest_sha256 {
            return Err(StoreError::corrupt(
                "the restore metadata names another manifest",
            ));
        }
        let object_count = u64::try_from(prepared.metadata.object_count)
            .map_err(|_| StoreError::corrupt("invalid restore object count"))?;
        let total_bytes = u64::try_from(prepared.metadata.total_bytes)
            .map_err(|_| StoreError::corrupt("invalid restore byte total"))?;

        self.check_bindings(&Bound::of_prepared(&prepared, &names))?;
        progress(QualifyPhase::BindingsChecked);
        active(cancel)?;

        let expected = Expected {
            manifest_sha256: prepared.metadata.manifest_sha256,
            manifest_bytes: prepared.manifest_bytes,
            generation_id: &names.generation_id,
            object_count,
            total_bytes,
        };
        let evidence = stage_evidence::reauthenticate(&prepared.files, &expected, cancel, |_| {})?;
        progress(QualifyPhase::EvidenceChecked);
        active(cancel)?;

        let input = Quarantine {
            new_incarnation: &names.incarnation,
            generation: &names.generation_id,
            source_location,
            current_root: &prepared.data_root,
            preserved_root: &names.preserved_root,
            manifest_sha256: &names.manifest_sha256,
            object_count: prepared.metadata.object_count,
            total_bytes: prepared.metadata.total_bytes,
            completed_at: prepared.metadata.completed_at,
            cancel,
            max_work_calls: quarantine::work_allowance(prepared.files.database_bytes),
        };
        let seams = Seams::new(
            Arc::clone(&self.fs),
            Arc::clone(&self.clock),
            Arc::clone(&self.random),
        );
        let image = stage_quarantine::run(
            &prepared.files,
            &seams,
            &input,
            &evidence.store_uuid,
            |phase| progress(QualifyPhase::Image(phase)),
        )?;
        active(cancel)?;

        // After the checkpoint, close and flushes: the image must be the
        // retained file, at the snapshot's schema version, and every
        // binding and the evidence must still hold. `run` builds its
        // image from these same handles and catalog today; the identity
        // and schema comparisons keep that a checked fact, not an
        // assumption, should it ever change.
        if image.root_identity != prepared.files.root_identity
            || image.database_identity != prepared.files.database_identity
        {
            return Err(StoreError::conflict(
                "the quarantined image is not the retained stage",
            ));
        }
        if image.schema_version != evidence.schema_version {
            return Err(StoreError::corrupt(
                "the quarantined image's schema differs from the snapshot's",
            ));
        }
        self.check_bindings(&Bound::of_prepared(&prepared, &names))?;
        self.recheck_evidence(&prepared.files, &expected, &evidence, cancel)?;
        progress(QualifyPhase::Rechecked);
        active(cancel)?;

        Ok(QualifiedStage {
            files: prepared.files,
            bindings: prepared.bindings,
            manifest_bytes: prepared.manifest_bytes,
            image_sha256: image.sha256,
            image_bytes: image.bytes,
            schema_version: image.schema_version,
            path: names.path,
            incarnation: names.incarnation,
            generation_id: names.generation_id,
            manifest_sha256: names.manifest_sha256,
            source_location: names.source_location,
            preserved_root: names.preserved_root,
            data_root: prepared.data_root,
            live_incarnation: prepared.live_incarnation,
            metadata: prepared.metadata,
            coordinator: prepared.coordinator,
            operation: prepared.operation,
        })
    }

    /// The running operation lease must be the one the stage holds.
    pub(super) fn holds_operation(&self, operation: &Arc<()>) -> Result<(), StoreError> {
        let running = self
            .active
            .lock()
            .map_err(|_| StoreError::conflict("backup operation state is unavailable"))?
            .as_ref()
            .and_then(Weak::upgrade);
        match running {
            Some(running) if Arc::ptr_eq(&running, operation) => Ok(()),
            _ => Err(StoreError::conflict(
                "the restore stage's operation is not the running one",
            )),
        }
    }

    /// The source and live bindings, the stage's own names, the retained
    /// handles' identities, and the live incarnation, read between two
    /// live rechecks as staging reads it.
    pub(super) fn check_bindings(&self, bound: &Bound<'_>) -> Result<(), StoreError> {
        let fs = self.fs.as_ref();
        bound
            .bindings
            .recheck(fs, bound.source_location, bound.data_root)?;
        let files = bound.files;
        let retained_root = files
            .root
            .identity()
            .map_err(|error| io_error("reading the retained stage identity", error))?;
        let retained_database = files
            .database
            .identity()
            .map_err(|error| io_error("reading the retained database identity", error))?;
        if retained_root != files.root_identity || retained_database != files.database_identity {
            return Err(StoreError::corrupt(
                "a retained stage handle changed identity",
            ));
        }
        // The stage's name must still open the retained directory and
        // database: a later swap renames by name.
        let (parent, name) = bound
            .path
            .parent()
            .zip(bound.path.file_name())
            .ok_or_else(|| StoreError::corrupt("the restore stage path has no parent"))?;
        let stage = fs
            .open_confined_dir(parent)
            .and_then(|dir| dir.open_dir(Path::new(name)))
            .map_err(|error| io_error("reopening the restore stage", error))?;
        let named_root = stage
            .identity()
            .map_err(|error| io_error("reading the restore stage identity", error))?;
        if named_root != files.root_identity {
            return Err(StoreError::conflict("the restore stage was replaced"));
        }
        let (_, _, named_database) = stage
            .open_regular_identified(Path::new(SNAPSHOT_FILE))
            .map_err(|error| io_error("reopening the staged database", error))?;
        if named_database != files.database_identity {
            return Err(StoreError::conflict("the staged database was replaced"));
        }

        let current = read_live_incarnation(&self.state_db)?;
        bound.bindings.live.recheck(fs, bound.data_root)?;
        if current != bound.live_incarnation {
            return Err(StoreError::conflict(
                "the live store changed incarnation since staging",
            ));
        }
        if current == bound.incarnation {
            return Err(StoreError::conflict(
                "the restore incarnation reuses the live store incarnation",
            ));
        }
        Ok(())
    }

    /// The staged manifest and objects once more, after the quarantine.
    fn recheck_evidence(
        &self,
        files: &StageFiles,
        expected: &Expected<'_>,
        before: &StagedEvidence,
        cancel: &AtomicBool,
    ) -> Result<(), StoreError> {
        let after = stage_evidence::reauthenticate(files, expected, cancel, |_| {})?;
        if &after != before {
            return Err(StoreError::corrupt(
                "the staged evidence changed during quarantine",
            ));
        }
        Ok(())
    }
}
