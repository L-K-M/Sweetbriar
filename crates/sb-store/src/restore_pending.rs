//! The restore-pending marker and its startup recovery (spec/03
//! "Restore" step 4, plan M1.8f). Before the swap renames anything, a
//! marker `<data root>.restore-pending` names the staged root and the
//! preserved one; it is written atomically (a flushed temporary file
//! renamed into place, then the parent flushed), so startup never sees
//! a torn marker. While it exists, a writable open first resolves the
//! interrupted swap:
//!
//! | data root | staged root | action                                     |
//! |-----------|-------------|--------------------------------------------|
//! | missing   | present     | finish: rename the stage into place         |
//! | present   | present     | not started: keep the stage, drop the marker |
//! | present   | missing     | finished: drop the marker                   |
//! | missing   | missing     | refuse (`Corrupt`); never create a store     |
//!
//! A stage the swap never used stays behind as unqualified evidence and
//! is never reused (a new restore stages under a new incarnation), the
//! same rule staging follows for any stale stage. Nothing here deletes
//! a store: the old root is only ever renamed to its preserved name.
//! The read-only open refuses while a marker exists, since it may not
//! write.

use std::ffi::{OsStr, OsString};
use std::io::{self, Read, Write};
use std::path::{Path, PathBuf};

use sb_types::fs::{ConfinedDir, Fs};

use crate::{StoreError, StorePaths, Uuid};

/// The marker's first line: format and version.
const HEADER: &str = "sweetbriar-restore-pending 1";
/// Far above any valid marker (four short lines); bounds the read.
const MAX_MARKER_BYTES: u64 = 1024;
/// Where the marker is written before its atomic rename.
const TEMPORARY_SUFFIX: &str = ".tmp";

/// An interrupted swap's record: the restored store's incarnation and
/// generation, and the two sibling names the swap moves between.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct PendingRestore {
    /// The restored store's new incarnation.
    pub incarnation: String,
    /// The generation it was restored from.
    pub generation_id: String,
    /// `<root name>.restoring-<incarnation>`.
    pub staging: OsString,
    /// `<root name>.preserved-<old incarnation>`.
    pub preserved: OsString,
}

impl PendingRestore {
    /// The record for swapping `data_root` to the stage of
    /// `incarnation`, preserving the root of `old_incarnation`. Every
    /// value must be a canonical UUID; names are derived, never taken.
    pub(crate) fn new(
        data_root: &Path,
        incarnation: &str,
        generation_id: &str,
        old_incarnation: &str,
    ) -> Result<Self, StoreError> {
        for value in [incarnation, generation_id, old_incarnation] {
            canonical_uuid(value)?;
        }
        if incarnation == old_incarnation {
            return Err(StoreError::corrupt(
                "a restore swap reuses the live incarnation",
            ));
        }
        let root = root_name(data_root)?;
        Ok(Self {
            incarnation: incarnation.to_owned(),
            generation_id: generation_id.to_owned(),
            staging: sibling(root, ".restoring-", incarnation),
            preserved: sibling(root, ".preserved-", old_incarnation),
        })
    }

    fn encode(&self) -> Vec<u8> {
        let name = |name: &OsStr| {
            name.to_str()
                .expect("built from a UTF-8 root name")
                .to_owned()
        };
        format!(
            "{HEADER}\nincarnation {}\ngeneration {}\nstaging {}\npreserved {}\n",
            self.incarnation,
            self.generation_id,
            name(&self.staging),
            name(&self.preserved),
        )
        .into_bytes()
    }

    /// Parse a marker for `data_root`: exactly the encoded lines, with
    /// both names re-derived from the UUIDs and required to match.
    fn decode(data_root: &Path, bytes: &[u8]) -> Result<Self, StoreError> {
        let invalid = || StoreError::corrupt("the restore-pending marker is invalid");
        let text = std::str::from_utf8(bytes).map_err(|_| invalid())?;
        let lines: Vec<&str> = text
            .strip_suffix('\n')
            .ok_or_else(invalid)?
            .split('\n')
            .collect();
        let [header, incarnation, generation, staging, preserved] = lines[..] else {
            return Err(invalid());
        };
        if header != HEADER {
            return Err(invalid());
        }
        let field = |line: &str, key: &str| -> Result<String, StoreError> {
            let value = line
                .strip_prefix(key)
                .and_then(|rest| rest.strip_prefix(' '))
                .ok_or_else(invalid)?;
            Ok(value.to_owned())
        };
        let incarnation = field(incarnation, "incarnation")?;
        let generation_id = field(generation, "generation")?;
        let staging = field(staging, "staging")?;
        let preserved = field(preserved, "preserved")?;
        // A marker under a root no restore could have written for is
        // as foreign as an unparseable one.
        let root = root_name(data_root).map_err(|_| invalid())?;
        let old_incarnation = preserved
            .strip_prefix(&format!("{root}.preserved-"))
            .ok_or_else(invalid)?;
        let pending = Self::new(data_root, &incarnation, &generation_id, old_incarnation)
            .map_err(|_| invalid())?;
        if pending.staging != OsStr::new(&staging) {
            return Err(invalid());
        }
        Ok(pending)
    }
}

/// How startup found a marker.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Recovered {
    /// No swap was pending.
    Nothing,
    /// The swap had not started; the stage is kept, the marker gone.
    NotStarted,
    /// The stage was renamed into place now.
    Completed,
    /// The swap had finished; only the marker remained.
    Finished,
}

/// `<data root>.restore-pending`.
pub(crate) fn marker_path(paths: &StorePaths) -> PathBuf {
    let mut name = paths.data_root().as_os_str().to_os_string();
    name.push(".restore-pending");
    PathBuf::from(name)
}

/// Whether a marker exists, for the read-only open, which refuses
/// rather than resolve it.
pub(crate) fn is_pending(fs: &dyn Fs, paths: &StorePaths) -> Result<bool, StoreError> {
    let marker = marker_path(paths);
    Ok(fs
        .metadata(&marker)
        .map_err(|error| io_error("checking for a pending restore", error))?
        .is_some())
}

/// Write the marker atomically and durably: a temporary sibling is
/// written and flushed, renamed over the marker name, and the parent
/// flushed. Only then may the swap rename anything.
pub(crate) fn write_marker(
    fs: &dyn Fs,
    paths: &StorePaths,
    pending: &PendingRestore,
) -> Result<(), StoreError> {
    let marker = marker_path(paths);
    let mut temporary = marker.as_os_str().to_os_string();
    temporary.push(TEMPORARY_SUFFIX);
    let temporary = PathBuf::from(temporary);
    // A temporary left by an earlier interrupted write never took
    // effect; it is ours by name and replaced.
    match fs.remove(&temporary) {
        Ok(()) => {}
        Err(error) if error.kind() == io::ErrorKind::NotFound => {}
        Err(error) => return Err(io_error("clearing a stale restore marker", error)),
    }
    let mut file = fs
        .create(&temporary)
        .map_err(|error| io_error("creating the restore marker", error))?;
    file.write_all(&pending.encode())
        .map_err(|error| io_error("writing the restore marker", error))?;
    file.sync_all()
        .map_err(|error| io_error("flushing the restore marker", error))?;
    drop(file);
    fs.rename(&temporary, &marker)
        .map_err(|error| io_error("placing the restore marker", error))?;
    sync_parent(fs, paths)
}

/// Remove the marker and flush its parent: the swap is resolved.
pub(crate) fn remove_marker(fs: &dyn Fs, paths: &StorePaths) -> Result<(), StoreError> {
    fs.remove(&marker_path(paths))
        .map_err(|error| io_error("removing the restore marker", error))?;
    sync_parent(fs, paths)
}

/// Resolve an interrupted swap before a writable open touches the data
/// root (see the module table). Runs under the store lease.
pub(crate) fn recover(fs: &dyn Fs, paths: &StorePaths) -> Result<Recovered, StoreError> {
    let Some(bytes) = read_marker(fs, &marker_path(paths))? else {
        return Ok(Recovered::Nothing);
    };
    let pending = PendingRestore::decode(paths.data_root(), &bytes)?;
    let parent_path = parent(paths)?;
    let parent = fs
        .open_confined_dir(parent_path)
        .map_err(|error| io_error("opening the data root's parent", error))?;
    let root = root_name(paths.data_root())?;
    let root_present = present(parent.as_ref(), OsStr::new(root))?;
    let staging_present = present(parent.as_ref(), &pending.staging)?;
    drop(parent);
    match (root_present, staging_present) {
        (false, true) => {
            // Only a stage that still holds its database replaces the
            // root: an emptied one would let the open create a store.
            require_database(fs, &parent_path.join(&pending.staging))?;
            fs.rename(&parent_path.join(&pending.staging), paths.data_root())
                .map_err(|error| io_error("finishing the restore swap", error))?;
            sync_parent(fs, paths)?;
            remove_marker(fs, paths)?;
            Ok(Recovered::Completed)
        }
        (true, true) => {
            remove_marker(fs, paths)?;
            Ok(Recovered::NotStarted)
        }
        (true, false) => {
            remove_marker(fs, paths)?;
            Ok(Recovered::Finished)
        }
        (false, false) => Err(StoreError::corrupt(
            "a restore swap was interrupted and neither the data root nor its staged copy remains",
        )),
    }
}

/// The stage's database must be a regular file before the stage may
/// become the data root.
fn require_database(fs: &dyn Fs, staging: &Path) -> Result<(), StoreError> {
    match fs.open_regular(&staging.join("state.sqlite")) {
        Ok(_) => Ok(()),
        Err(error)
            if matches!(
                error.kind(),
                io::ErrorKind::NotFound | io::ErrorKind::InvalidInput
            ) =>
        {
            Err(StoreError::corrupt(
                "an interrupted restore's staged copy has no database",
            ))
        }
        Err(error) => Err(io_error("checking the staged database", error)),
    }
}

/// The marker's bytes, bounded; `None` when there is no marker.
fn read_marker(fs: &dyn Fs, marker: &Path) -> Result<Option<Vec<u8>>, StoreError> {
    let (file, _) = match fs.open_regular(marker) {
        Ok(opened) => opened,
        Err(error) if error.kind() == io::ErrorKind::NotFound => return Ok(None),
        // A directory, fifo or other non-file under the marker's name.
        Err(error) if error.kind() == io::ErrorKind::InvalidInput => {
            return Err(StoreError::corrupt(
                "the restore-pending marker is not a file",
            ));
        }
        Err(error) => return Err(io_error("reading the restore marker", error)),
    };
    let mut bytes = Vec::new();
    file.take(MAX_MARKER_BYTES + 1)
        .read_to_end(&mut bytes)
        .map_err(|error| io_error("reading the restore marker", error))?;
    if bytes.len() as u64 > MAX_MARKER_BYTES {
        return Err(StoreError::corrupt("the restore-pending marker is invalid"));
    }
    Ok(Some(bytes))
}

/// Whether `name` is a directory below `parent`, never following a
/// link: a link or another kind of entry under a swap name is refused.
fn present(parent: &dyn ConfinedDir, name: &OsStr) -> Result<bool, StoreError> {
    match parent.open_dir(Path::new(name)) {
        Ok(_) => Ok(true),
        Err(error) if error.kind() == io::ErrorKind::NotFound => Ok(false),
        Err(error) => Err(io_error(
            &format!(
                "inspecting {} during restore recovery",
                name.to_string_lossy()
            ),
            error,
        )),
    }
}

fn sync_parent(fs: &dyn Fs, paths: &StorePaths) -> Result<(), StoreError> {
    fs.sync_dir(parent(paths)?)
        .map_err(|error| io_error("flushing the data root's parent", error))
}

fn parent(paths: &StorePaths) -> Result<&Path, StoreError> {
    paths
        .data_root()
        .parent()
        .ok_or_else(|| StoreError::corrupt("the data root has no parent"))
}

/// The data root's final name; restore requires UTF-8 without control
/// characters, so the marker's lines round-trip.
pub(super) fn root_name(data_root: &Path) -> Result<&str, StoreError> {
    data_root
        .file_name()
        .and_then(OsStr::to_str)
        .filter(|name| !name.chars().any(char::is_control))
        .ok_or_else(|| StoreError::InvalidInput {
            message: "a restore needs a data root named in UTF-8 without control characters"
                .to_owned(),
        })
}

fn sibling(root: &str, suffix: &str, id: &str) -> OsString {
    OsString::from(format!("{root}{suffix}{id}"))
}

fn canonical_uuid(value: &str) -> Result<(), StoreError> {
    let parsed: Uuid = value
        .parse()
        .map_err(|_| StoreError::corrupt("a restore identifier is not a UUID"))?;
    if parsed.to_string() != value {
        return Err(StoreError::corrupt(
            "a restore identifier is not a canonical UUID",
        ));
    }
    Ok(())
}

fn io_error(context: &str, error: io::Error) -> StoreError {
    StoreError::unavailable_io(
        format!("restore recovery I/O while {context}: {error}"),
        error.kind(),
    )
}

#[cfg(test)]
mod tests;
