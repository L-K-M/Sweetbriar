//! The configured backup destination (spec/02; spec/03; M1.8j1b).
//! Resolve it only after replay, and never create a missing chosen volume.

use std::io;
use std::path::{Path, PathBuf};

use rusqlite::OptionalExtension;

use super::{Access, Backups, FileKind, READER_PRAGMAS, StoreError, open_connection};

/// The registry key used by scheduled and requested generations.
pub const BACKUP_DESTINATION_SETTING: &str = "backups.destination";

/// A setting's syntax, independent of the volume's current availability.
/// Empty selects the default; otherwise an absolute path without NUL.
pub fn valid_backup_destination(value: &str) -> bool {
    value.is_empty() || (Path::new(value).is_absolute() && !value.as_bytes().contains(&0))
}

impl Backups {
    pub(super) fn configured_destination(&self) -> Result<PathBuf, StoreError> {
        let conn = open_connection(&self.state_db, Access::ReadOnly, READER_PRAGMAS)?;
        let raw: Option<String> = conn
            .query_row(
                "SELECT value_json FROM settings WHERE key = ?1",
                [BACKUP_DESTINATION_SETTING],
                |row| row.get(0),
            )
            .optional()
            .map_err(|error| crate::schema::classify(error, "read the backup destination"))?;
        let value = match raw {
            None => String::new(),
            Some(raw) => {
                let corrupt = || {
                    StoreError::corrupt(format!(
                        "setting {BACKUP_DESTINATION_SETTING:?} is not a canonical JSON string naming an absolute directory or the empty default"
                    ))
                };
                let value: String = serde_json::from_str(&raw).map_err(|_| corrupt())?;
                let canonical = serde_json::to_string(&value).map_err(|_| corrupt())?;
                if !valid_backup_destination(&value) || canonical != raw {
                    return Err(corrupt());
                }
                value
            }
        };
        if value.is_empty() {
            return self.ensure_destination(&self.backups_dir);
        }
        let root = Path::new(&value);
        let failed = |error: io::Error| {
            StoreError::unavailable_io(
                format!(
                    "cannot use the configured backup directory {}: {error}",
                    root.display()
                ),
                error.kind(),
            )
        };
        match self.fs.metadata(root).map_err(failed)? {
            Some(metadata) if metadata.kind() == FileKind::Directory => {}
            Some(_) => return Err(failed(io::ErrorKind::NotADirectory.into())),
            None => return Err(failed(io::ErrorKind::NotFound.into())),
        }
        // No ensure_destination here: it creates ancestors, which would
        // turn an absent mount into a directory on the wrong filesystem.
        self.fs.canonicalize(root).map_err(failed)
    }
}

#[cfg(test)]
mod tests;
