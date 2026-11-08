//! Read-only observation of the declared current catalog, not a physical
//! integrity, byte-stability, quarantine or stage-eligibility verdict.
//! The next integrity slice and final qualifier retain those obligations.

mod catalog;

use std::io::{self, Read, Seek, SeekFrom};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

use rusqlite::limits::Limit;
use sb_types::fs::ReadFile;

use super::image_inspect::{MAX_PROGRESS_CALLS, MAX_SQLITE_VALUE, guarded, inspection_error};
use crate::{Seams, StoreError, schema};

const MAX_COLUMNS: i32 = 64;
const MAX_SCHEMA_INPUT: u64 = 2 * 1024 * 1024;

/// A non-renewable physical-input budget, installed before SQLite opens the
/// image. A later SELECT row limit cannot bound SQLite's eager schema load.
/// Count actual returned bytes so short reads and Interrupted retries do not
/// exhaust the budget spuriously. Seeking never renews it.
struct BudgetReader {
    file: Box<dyn ReadFile>,
    remaining: u64,
    exhausted: Arc<AtomicBool>,
}

impl Read for BudgetReader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        if self.remaining == 0 {
            self.exhausted.store(true, Ordering::Release);
            return Err(io::Error::other("catalog input budget exhausted"));
        }
        let count = bytes
            .len()
            .min(usize::try_from(self.remaining).unwrap_or(usize::MAX));
        let read = self.file.read(&mut bytes[..count])?;
        self.remaining -= read as u64;
        Ok(read)
    }
}

impl Seek for BudgetReader {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.file.seek(to)
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) struct Inspection {
    pub schema_version: i64,
}

/// Only the retained reader is used. Equality concerns the declared schema
/// at inspection time: the caller must reverify staged bytes and bindings
/// and run physical integrity checks before any qualification or write.
pub(super) fn inspect(
    file: Box<dyn ReadFile>,
    seams: &Seams,
    cancel: &AtomicBool,
) -> Result<Option<Inspection>, StoreError> {
    inspect_with_budget(file, seams, cancel, MAX_PROGRESS_CALLS)
}

fn inspect_with_budget(
    file: Box<dyn ReadFile>,
    seams: &Seams,
    cancel: &AtomicBool,
    work_budget: u32,
) -> Result<Option<Inspection>, StoreError> {
    if cancel.load(Ordering::Acquire) {
        return Ok(None);
    }
    let expected = catalog::reference(seams)?;
    let metadata_limit = expected.metadata_limit()?;
    if cancel.load(Ordering::Acquire) {
        return Ok(None);
    }
    let exhausted = Arc::new(AtomicBool::new(false));
    let file = Box::new(BudgetReader {
        file,
        remaining: MAX_SCHEMA_INPUT,
        exhausted: exhausted.clone(),
    });
    let connection = match crate::image_vfs::open(file) {
        Ok(connection) => connection,
        Err(_) if cancel.load(Ordering::Acquire) => return Ok(None),
        Err(_) if exhausted.load(Ordering::Acquire) => return Err(input_exhausted()),
        Err(error) => return Err(inspection_error(error, "open canonical catalog inspection")),
    };
    let result = guarded(&connection, cancel, work_budget, |connection| {
        for limit in [Limit::SQLITE_LIMIT_LENGTH, Limit::SQLITE_LIMIT_SQL_LENGTH] {
            connection
                .set_limit(limit, metadata_limit)
                .map_err(|error| inspection_error(error, "bound catalog metadata"))?;
        }
        connection
            .set_limit(Limit::SQLITE_LIMIT_COLUMN, MAX_COLUMNS)
            .map_err(|error| inspection_error(error, "bound catalog columns"))?;
        connection
            .set_limit(Limit::SQLITE_LIMIT_ATTACHED, 0)
            .map_err(|error| inspection_error(error, "confine catalog inspection"))?;
        connection
            .execute_batch(
                "PRAGMA trusted_schema=OFF; PRAGMA query_only=ON; PRAGMA temp_store=MEMORY;",
            )
            .map_err(|error| inspection_error(error, "harden catalog inspection"))?;
        let version: i64 = connection
            .query_row("PRAGMA user_version", [], |row| row.get(0))
            .map_err(|error| inspection_error(error, "read canonical catalog version"))?;
        if version != i64::from(schema::LATEST_SCHEMA_VERSION) {
            return Err(StoreError::corrupt(
                "catalog is not at the current schema version",
            ));
        }
        catalog::compare(connection, &expected)?;
        Ok(Inspection {
            schema_version: version,
        })
    });
    let closed = connection
        .close()
        .map_err(|(_, error)| inspection_error(error, "close catalog inspection"));
    if cancel.load(Ordering::Acquire) {
        return Ok(None);
    }
    if exhausted.load(Ordering::Acquire) {
        return Err(input_exhausted());
    }
    closed?;
    result
}

fn input_exhausted() -> StoreError {
    StoreError::corrupt("restore catalog input budget exceeded")
}

#[cfg(test)]
mod tests;
