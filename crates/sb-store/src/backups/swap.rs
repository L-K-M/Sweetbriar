//! The restore swap's checks and renames (spec/03 "Restore" step 4,
//! plan M1.8f2). `ready_to_swap` runs while the live store is still
//! open: it accepts only this coordinator's qualified stage holding the
//! running operation lease, reruns the qualifier's binding checks,
//! re-hashes the image against the qualification's fingerprint,
//! re-authenticates the staged manifest and objects, and derives the
//! marker. `commit_swap` runs after the store closed with
//! its lock kept: the marker, the old root renamed to its preserved
//! name, the stage renamed into place (each rename followed by a parent
//! flush), and the marker removed. A crash anywhere is resolved by the
//! next open (`restore_pending::recover`); nothing is deleted.

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use sb_types::fs::Fs;

use super::Backups;
use super::qualify::{Bound, QualifiedStage};
use super::stage_copy::{cancelled, io_error};
use super::stage_evidence::{self, Expected};
use super::stage_quarantine::fingerprint;
use crate::restore_pending::{self, PendingRestore};
use crate::{StoreError, StorePaths};

fn active(cancel: &AtomicBool) -> Result<(), StoreError> {
    if cancel.load(Ordering::Acquire) {
        return Err(cancelled());
    }
    Ok(())
}

impl Backups {
    /// Every check the swap makes while the live store is still open.
    /// Answers the marker the swap writes. Reads only; any refusal leaves
    /// the store, the stage and the disk as they were.
    pub(crate) fn ready_to_swap(
        &self,
        stage: &QualifiedStage,
        cancel: &AtomicBool,
    ) -> Result<PendingRestore, StoreError> {
        active(cancel)?;
        if !Arc::ptr_eq(&stage.coordinator, &self.coordinator) {
            return Err(StoreError::conflict(
                "the restore stage belongs to another store",
            ));
        }
        self.holds_operation(&stage.operation)?;
        // The qualifier's own checks again: source and live bindings,
        // the stage's names and retained handles, the live incarnation.
        self.check_bindings(&Bound::of_qualified(stage))?;
        active(cancel)?;

        // The image is still the one qualification fingerprinted. The
        // name check above bound the retained handle to the stage's
        // name, so hashing through the handle hashes the named file.
        let image = fingerprint(&stage.files, stage.image_bytes, cancel)?;
        if image != stage.image_sha256 {
            return Err(StoreError::corrupt(
                "the qualified image changed before the swap",
            ));
        }
        // The staged manifest and objects the restored store will read,
        // as qualification left them.
        let metadata = &stage.metadata;
        let expected = Expected {
            manifest_sha256: metadata.manifest_sha256,
            manifest_bytes: stage.manifest_bytes,
            generation_id: stage.generation_id(),
            object_count: u64::try_from(metadata.object_count)
                .map_err(|_| StoreError::corrupt("invalid restore object count"))?,
            total_bytes: u64::try_from(metadata.total_bytes)
                .map_err(|_| StoreError::corrupt("invalid restore byte total"))?,
        };
        let evidence = stage_evidence::reauthenticate(&stage.files, &expected, cancel, |_| {})?;
        if evidence.schema_version != stage.schema_version {
            return Err(StoreError::corrupt(
                "the staged manifest's schema differs from the qualified image's",
            ));
        }
        if stage.schema_version > i64::from(crate::schema::LATEST_SCHEMA_VERSION) {
            return Err(StoreError::conflict(
                "the restored image is newer than this app",
            ));
        }

        // The marker's names are derived, never taken: they must be the
        // stage's own and the preserved root it was qualified for.
        let pending = PendingRestore::new(
            &stage.data_root,
            stage.incarnation(),
            stage.generation_id(),
            &stage.live_incarnation,
        )?;
        let parent = stage
            .data_root
            .parent()
            .ok_or_else(|| StoreError::corrupt("the data root has no parent"))?;
        if parent.join(&pending.staging) != stage.path()
            || parent.join(&pending.preserved) != stage.preserved_root()
        {
            return Err(StoreError::corrupt(
                "the qualified stage names other siblings than its marker",
            ));
        }
        // The old root is only ever renamed, never merged or replaced.
        // Only a missing name is free: a directory, a file, a link (even
        // a dangling one, so the name is not followed) or an entry that
        // cannot be inspected refuses while the store can be handed back.
        let parent_dir = self
            .fs
            .open_confined_dir(parent)
            .map_err(|error| io_error("opening the data root's parent", error))?;
        match parent_dir.open_dir(Path::new(&pending.preserved)) {
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            _ => {
                return Err(StoreError::conflict(
                    "the preserved root's name is already taken",
                ));
            }
        }
        active(cancel)?;
        Ok(pending)
    }
}

/// The swap itself, with the store closed and its lock still held:
/// the marker, then each rename followed by a flush of the parent so
/// the two renames persist in order, then the marker's removal. A
/// failure stops here; the next open resolves what is on disk.
pub(crate) fn commit_swap(
    fs: &dyn Fs,
    paths: &StorePaths,
    staging: &Path,
    preserved: &Path,
    pending: &PendingRestore,
) -> Result<(), StoreError> {
    let parent = paths
        .data_root()
        .parent()
        .ok_or_else(|| StoreError::corrupt("the data root has no parent"))?;
    let flush = |what: &str| {
        fs.sync_dir(parent)
            .map_err(|error| io_error(&format!("flushing the parent after {what}"), error))
    };
    restore_pending::write_marker(fs, paths, pending)?;
    fs.rename(paths.data_root(), preserved)
        .map_err(|error| io_error("preserving the old data root", error))?;
    flush("preserving the old data root")?;
    fs.rename(staging, paths.data_root())
        .map_err(|error| io_error("moving the restored root into place", error))?;
    flush("moving the restored root into place")?;
    restore_pending::remove_marker(fs, paths)
}

#[cfg(test)]
mod tests;
