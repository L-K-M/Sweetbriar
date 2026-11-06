//! Durable restore swap and startup recovery (spec/03 "Restore" step
//! 4): the `.restore-pending` marker naming the staging and preserved
//! directories, both renames with their parent flushes, and the startup
//! handling of all three marker states. The caller guarantees every
//! connection to the live and staging databases closed before the
//! swap; the sibling `.lock` lease stays held across it, so a second
//! process cannot open either root meanwhile. Nothing is deleted: the
//! previous data root is renamed, and an interrupted swap leaves the
//! marker for the next open to finish or discard.

use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use serde::Deserialize;

use super::staging::sibling_name;
use super::*;

/// The marker suffix appended to the data-root path (spec/03).
const MARKER_SUFFIX: &str = ".restore-pending";
/// A marker never exceeds a small header plus two names and two UUIDs.
const MAX_MARKER_BYTES: u64 = 4096;

/// `<data root>.restore-pending`, a sibling of the root so it survives
/// either rename (spec/03).
fn marker_path(data_root: &Path) -> PathBuf {
    let mut marker = data_root.as_os_str().to_os_string();
    marker.push(MARKER_SUFFIX);
    PathBuf::from(marker)
}

/// The pending-swap record: the two sibling names the renames use plus
/// the observability ids a later restore report surfaces.
#[derive(Debug, Deserialize)]
struct RestoreMarker {
    staging: String,
    preserved: String,
    generation_id: String,
    incarnation: String,
}

fn marker_text(stage: &RestoreStage) -> Result<String, StoreError> {
    let staging = stage
        .path()
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| StoreError::corrupt("the restore stage path has no name"))?;
    let preserved = stage
        .preserved_root()
        .file_name()
        .and_then(|name| name.to_str())
        .ok_or_else(|| StoreError::corrupt("the preserved root path has no name"))?;
    serde_json::to_string(&serde_json::json!({
        "v": 1,
        "staging": staging,
        "preserved": preserved,
        "generation_id": stage.generation_id(),
        "incarnation": stage.incarnation(),
    }))
    .map_err(|error| StoreError::internal(format!("serialize restore marker: {error}")))
}

fn parse_marker(bytes: &[u8]) -> Result<RestoreMarker, StoreError> {
    #[derive(Deserialize)]
    struct Versioned {
        v: u32,
        #[serde(flatten)]
        marker: RestoreMarker,
    }
    let marker: Versioned = serde_json::from_slice(bytes)
        .map_err(|_| StoreError::corrupt("unreadable restore-pending marker"))?;
    if marker.v != 1 {
        return Err(StoreError::corrupt("unsupported restore-pending marker"));
    }
    for name in [&marker.marker.staging, &marker.marker.preserved] {
        if name.is_empty() || name.contains('/') || name.contains('\\') || name.contains('\0') {
            return Err(StoreError::corrupt(
                "restore-pending marker names an invalid path",
            ));
        }
    }
    Ok(marker.marker)
}

fn io_error(context: &str, error: io::Error) -> StoreError {
    if error.kind() == io::ErrorKind::InvalidData {
        StoreError::corrupt(format!(
            "invalid restore swap input while {context}: {error}"
        ))
    } else {
        StoreError::unavailable_io(
            format!("restore swap I/O while {context}: {error}"),
            error.kind(),
        )
    }
}

fn cancelled() -> StoreError {
    StoreError::unavailable("restore swap cancelled")
}

/// Whether `path` exists at all (the marker and the preserved target
/// refuse on any entry, whatever its kind).
fn exists(fs: &dyn Fs, path: &Path) -> Result<bool, StoreError> {
    Ok(fs
        .metadata(path)
        .map_err(|error| io_error(&format!("inspecting {}", path.display()), error))?
        .is_some())
}

/// Whether `path` exists as a directory: a staging name that resolves
/// to a file cannot finish a swap and counts as missing.
fn dir_exists(fs: &dyn Fs, path: &Path) -> Result<bool, StoreError> {
    Ok(fs
        .metadata(path)
        .map_err(|error| io_error(&format!("inspecting {}", path.display()), error))?
        .map(|meta| meta.kind() == sb_types::fs::FileKind::Directory)
        .unwrap_or(false))
}

impl Backups {
    /// Swap a qualified stage into this store's data-root place (spec/03
    /// step 4): write and flush the pending marker, rename the live root
    /// to `<root>.preserved-<inc>`, rename the staging root to the root,
    /// flush the parent and remove the marker. The caller must already
    /// have closed every connection to the live and staging databases;
    /// the sibling lease this call runs under stays held across the
    /// swap. Any failure leaves the marker for startup recovery.
    pub(crate) fn swap_restore_stage(
        &self,
        stage: RestoreStage,
        cancel: &AtomicBool,
    ) -> Result<(), StoreError> {
        if cancel.load(std::sync::atomic::Ordering::Acquire) {
            return Err(cancelled());
        }
        if !Arc::ptr_eq(&stage.coordinator, &self.coordinator) {
            return Err(StoreError::InvalidInput {
                message: "the restore stage belongs to a different coordinator".into(),
            });
        }
        let data_root = self
            .state_db
            .parent()
            .expect("the state database has a parent")
            .to_path_buf();
        let parent = data_root.parent().expect("a data root always has a parent");
        // The stage and the preserved target must be siblings of this
        // store's root and the stage name must carry its incarnation.
        let expected_stage = sibling_name(&data_root, ".restoring-", stage.incarnation());
        if stage.path().parent() != Some(parent)
            || stage.path().file_name() != Some(expected_stage.as_os_str())
        {
            return Err(StoreError::corrupt(
                "the restore stage does not name this data root's sibling",
            ));
        }
        let expected_preserved = sibling_name(&data_root, ".preserved-", &stage.live_incarnation);
        if stage.preserved_root().parent() != Some(parent)
            || stage.preserved_root().file_name() != Some(expected_preserved.as_os_str())
        {
            return Err(StoreError::corrupt(
                "the restore stage's preserved root is not this root's sibling",
            ));
        }
        let marker = marker_path(&data_root);
        if exists(self.fs.as_ref(), &marker)? {
            return Err(StoreError::conflict(
                "a restore swap is already pending at this data root",
            ));
        }
        if exists(self.fs.as_ref(), stage.preserved_root())? {
            return Err(StoreError::conflict(
                "the preserved restore root already exists",
            ));
        }
        swap(
            self.fs.as_ref(),
            &data_root,
            stage.path(),
            stage.preserved_root(),
            &marker_text(&stage)?,
            &marker,
            cancel,
        )
    }
}

/// The rename sequence, separated so startup recovery can finish the
/// second half: marker first, root aside, staging into place, flush,
/// marker gone.
fn swap(
    fs: &dyn Fs,
    data_root: &Path,
    staging: &Path,
    preserved: &Path,
    marker_text: &str,
    marker: &Path,
    cancel: &AtomicBool,
) -> Result<(), StoreError> {
    let parent = data_root.parent().expect("a data root always has a parent");
    if cancel.load(std::sync::atomic::Ordering::Acquire) {
        return Err(cancelled());
    }
    // The marker goes down completely — file bytes and directory entry —
    // before either rename, so a power cut can never lose it.
    let mut file = fs
        .create(marker)
        .map_err(|error| io_error(&format!("creating {}", marker.display()), error))?;
    file.write_all(marker_text.as_bytes())
        .and_then(|()| file.sync_all())
        .map_err(|error| io_error("writing the restore-pending marker", error))?;
    drop(file);
    fs.sync_dir(parent)
        .map_err(|error| io_error("flushing the restore-pending marker entry", error))?;

    if cancel.load(std::sync::atomic::Ordering::Acquire) {
        return Err(cancelled());
    }
    fs.rename(data_root, preserved).map_err(|error| {
        io_error(
            &format!(
                "renaming {} to {}",
                data_root.display(),
                preserved.display()
            ),
            error,
        )
    })?;
    // From here the data root is missing: a crash finishes the swap at
    // open rather than reviving the old root.
    fs.rename(staging, data_root).map_err(|error| {
        io_error(
            &format!("renaming {} to {}", staging.display(), data_root.display()),
            error,
        )
    })?;
    fs.sync_dir(parent)
        .map_err(|error| io_error("flushing the swapped root entries", error))?;
    fs.remove(marker)
        .map_err(|error| io_error("removing the restore-pending marker", error))?;
    fs.sync_dir(parent)
        .map_err(|error| io_error("flushing the removed restore marker", error))?;
    Ok(())
}

/// Startup handling for a `.restore-pending` marker (spec/03): run
/// under the freshly taken lease, before the layout check could create
/// an empty root. Returns once the marker state is resolved; the store
/// open proceeds afterwards.
pub(crate) fn handle_restore_marker(fs: &dyn Fs, data_root: &Path) -> Result<(), StoreError> {
    let marker = marker_path(data_root);
    let parent = data_root.parent().expect("a data root always has a parent");
    let Some(_) = fs
        .metadata(&marker)
        .map_err(|error| io_error("inspecting the restore-pending marker", error))?
    else {
        return Ok(());
    };
    let (mut file, _) = fs
        .open_regular(&marker)
        .map_err(|error| io_error("reading the restore-pending marker", error))?;
    let mut bytes = Vec::new();
    file.by_ref()
        .take(MAX_MARKER_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| io_error("reading the restore-pending marker", error))?;
    if bytes.len() as u64 > MAX_MARKER_BYTES {
        return Err(StoreError::corrupt("oversized restore-pending marker"));
    }
    let marker_body = parse_marker(&bytes)?;
    // The marker's staging name must be the stage the recorded new
    // incarnation produced: anything else is a torn or foreign marker.
    let expected_staging = sibling_name(data_root, ".restoring-", &marker_body.incarnation);
    if std::ffi::OsStr::new(marker_body.staging.as_str()) != expected_staging.as_os_str() {
        return Err(StoreError::corrupt(
            "restore-pending marker names an inconsistent staging root",
        ));
    }
    // The recorded generation is carried for diagnostics; a marker
    // without one names no restorable image.
    if marker_body.generation_id.is_empty() {
        return Err(StoreError::corrupt(
            "restore-pending marker names no generation",
        ));
    }
    let staging = parent.join(&marker_body.staging);
    let root_exists = exists(fs, data_root)?;
    let staging_exists = dir_exists(fs, &staging)?;
    match (root_exists, staging_exists) {
        (true, true) => {
            // The first rename never happened: the live root is
            // untouched and the stage stays on disk as evidence.
            fs.remove(&marker)
                .map_err(|error| io_error("removing a not-started restore marker", error))?;
            fs.sync_dir(parent)
                .map_err(|error| io_error("flushing the removed restore marker", error))?;
        }
        (true, false) => {
            // The swap finished before the crash; only the marker's
            // removal was lost.
            fs.remove(&marker)
                .map_err(|error| io_error("removing a finished restore marker", error))?;
            fs.sync_dir(parent)
                .map_err(|error| io_error("flushing the removed restore marker", error))?;
        }
        (false, true) => {
            // The first rename committed but the second did not: the
            // preserved root holds the old store; finish the swap so
            // no empty root is ever created over the marker.
            fs.rename(&staging, data_root).map_err(|error| {
                io_error(
                    &format!("finishing the rename of {}", staging.display()),
                    error,
                )
            })?;
            fs.sync_dir(parent)
                .map_err(|error| io_error("flushing the finished restore swap", error))?;
            fs.remove(&marker)
                .map_err(|error| io_error("removing the finished restore marker", error))?;
            fs.sync_dir(parent)
                .map_err(|error| io_error("flushing the removed restore marker", error))?;
        }
        (false, false) => {
            return Err(StoreError::corrupt(
                "restore-pending marker names a missing staging and data root",
            ));
        }
    }
    Ok(())
}
