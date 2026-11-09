//! Read-only observation of the declared current catalog, not a physical
//! integrity, byte-stability, quarantine or stage-eligibility verdict.
//! The next integrity slice and final qualifier retain those obligations.

mod catalog;

use std::io::{self, Read, Seek, SeekFrom};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, AtomicU64, Ordering},
};

use rusqlite::limits::Limit;
use sb_types::fs::ReadFile;

use super::image_inspect::{MAX_PROGRESS_CALLS, MAX_SQLITE_VALUE, guarded};
use crate::{Seams, StoreError, schema};

const MAX_COLUMNS: i32 = 64;
const MAX_SCHEMA_INPUT: u64 = 2 * 1024 * 1024;

/// A non-renewable physical-input budget, installed before SQLite opens the
/// image. A later SELECT row limit cannot bound SQLite's eager schema load.
/// Count actual returned bytes so short reads and Interrupted retries do not
/// exhaust the budget spuriously. Seeking never renews it.
struct BudgetReader {
    file: Box<dyn ReadFile>,
    remaining: Arc<AtomicU64>,
    exhausted: Arc<AtomicBool>,
    fault: Arc<Mutex<Option<io::ErrorKind>>>,
}

impl Read for BudgetReader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        let remaining = self.remaining.load(Ordering::Relaxed);
        if remaining == 0 {
            self.exhausted.store(true, Ordering::Release);
            return Err(io::Error::other("catalog input budget exhausted"));
        }
        let count = bytes
            .len()
            .min(usize::try_from(remaining).unwrap_or(usize::MAX));
        let result = catch_unwind(AssertUnwindSafe(|| self.file.read(&mut bytes[..count])))
            .unwrap_or_else(|_| Err(io::Error::other("restore reader panicked")));
        if let Err(error) = &result
            && error.kind() != io::ErrorKind::Interrupted
        {
            self.record_fault(error.kind());
        }
        let read = result?;
        self.remaining.fetch_sub(read as u64, Ordering::Relaxed);
        Ok(read)
    }
}

impl Seek for BudgetReader {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        let result = catch_unwind(AssertUnwindSafe(|| self.file.seek(to)))
            .unwrap_or_else(|_| Err(io::Error::other("restore seek panicked")));
        if let Err(error) = &result {
            self.record_fault(error.kind());
        }
        result
    }
}

impl BudgetReader {
    fn record_fault(&self, kind: io::ErrorKind) {
        if let Ok(mut fault) = self.fault.lock() {
            fault.get_or_insert(kind);
        }
    }
}

fn read_fault(fault: &Mutex<Option<io::ErrorKind>>) -> Option<StoreError> {
    // SQLite integrity_check may report an xRead I/O failure as a finding
    // row. Preserve the first real callback cause independently of SQL.
    fault
        .lock()
        .map(|fault| *fault)
        .unwrap_or(Some(io::ErrorKind::Other))
        .map(|kind| StoreError::unavailable_io("cannot read retained restore image", kind))
}

pub(super) fn inspection_error(error: rusqlite::Error, what: &str) -> StoreError {
    match error {
        // SQLite error messages can contain image-supplied schema names.
        rusqlite::Error::SqliteFailure(code, _) => {
            super::image_inspect::inspection_error(rusqlite::Error::SqliteFailure(code, None), what)
        }
        _ => StoreError::corrupt(format!("{what}: invalid SQLite result")),
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
    inspect_inner(
        file,
        seams,
        cancel,
        work_budget,
        None::<(
            u64,
            fn(&rusqlite::Connection) -> Result<Option<()>, StoreError>,
        )>,
    )
}

/// Continue only on the same catalog-checked connection. The caller supplies
/// the full inspection's independently bounded input allowance and installs
/// its own SQLite work/cancellation guard before running any further SQL.
pub(super) fn inspect_then(
    file: Box<dyn ReadFile>,
    seams: &Seams,
    cancel: &AtomicBool,
    input_budget: u64,
    after_catalog: impl FnOnce(&rusqlite::Connection) -> Result<Option<()>, StoreError>,
) -> Result<Option<Inspection>, StoreError> {
    inspect_inner(
        file,
        seams,
        cancel,
        MAX_PROGRESS_CALLS,
        Some((input_budget, after_catalog)),
    )
}

/// Observe the exact catalog on a fresh confined writer whose physical
/// input guard was installed before open. Only the later isolated quarantine
/// flow may lift query-only and the value limit; this grants no eligibility.
pub(super) fn inspect_writer(
    connection: &rusqlite::Connection,
    seams: &Seams,
    cancel: &AtomicBool,
) -> Result<Option<(Inspection, i32)>, StoreError> {
    if cancel.load(Ordering::Acquire) {
        return Ok(None);
    }
    let expected = catalog::reference(seams)?;
    let limit = expected.metadata_limit()?;
    guarded(connection, cancel, MAX_PROGRESS_CALLS, |connection| {
        check_catalog(connection, &expected, limit)
    })
}

fn check_catalog(
    connection: &rusqlite::Connection,
    expected: &catalog::Catalog,
    metadata_limit: i32,
) -> Result<(Inspection, i32), StoreError> {
    let mut value_limit = 0;
    for limit in [Limit::SQLITE_LIMIT_LENGTH, Limit::SQLITE_LIMIT_SQL_LENGTH] {
        let previous = connection
            .set_limit(limit, metadata_limit)
            .map_err(|error| inspection_error(error, "bound catalog metadata"))?;
        if matches!(limit, Limit::SQLITE_LIMIT_LENGTH) {
            value_limit = previous;
        }
    }
    connection
        .set_limit(Limit::SQLITE_LIMIT_COLUMN, MAX_COLUMNS)
        .map_err(|error| inspection_error(error, "bound catalog columns"))?;
    connection
        .set_limit(Limit::SQLITE_LIMIT_ATTACHED, 0)
        .map_err(|error| inspection_error(error, "confine catalog inspection"))?;
    connection
        .set_limit(
            Limit::SQLITE_LIMIT_EXPR_DEPTH,
            super::image_inspect::INSPECTION_EXPR_DEPTH,
        )
        .map_err(|error| inspection_error(error, "bound catalog depth"))?;
    connection
        .execute_batch("PRAGMA trusted_schema=OFF; PRAGMA query_only=ON; PRAGMA temp_store=MEMORY;")
        .map_err(|error| inspection_error(error, "harden catalog inspection"))?;
    let version: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .map_err(|error| inspection_error(error, "read canonical catalog version"))?;
    if version != i64::from(schema::LATEST_SCHEMA_VERSION) {
        return Err(StoreError::corrupt(
            "catalog is not at the current schema version",
        ));
    }
    catalog::compare(connection, expected)?;
    Ok((
        Inspection {
            schema_version: version,
        },
        value_limit,
    ))
}

fn inspect_inner<F>(
    file: Box<dyn ReadFile>,
    seams: &Seams,
    cancel: &AtomicBool,
    work_budget: u32,
    after_catalog: Option<(u64, F)>,
) -> Result<Option<Inspection>, StoreError>
where
    F: FnOnce(&rusqlite::Connection) -> Result<Option<()>, StoreError>,
{
    if cancel.load(Ordering::Acquire) {
        return Ok(None);
    }
    let expected = catalog::reference(seams)?;
    let metadata_limit = expected.metadata_limit()?;
    if cancel.load(Ordering::Acquire) {
        return Ok(None);
    }
    let exhausted = Arc::new(AtomicBool::new(false));
    // Only this thread updates the allowance between SQL phases. VFS reads
    // are serialized by image_vfs; seeking cannot renew either phase.
    let remaining = Arc::new(AtomicU64::new(MAX_SCHEMA_INPUT));
    let fault = Arc::new(Mutex::new(None));
    let file = Box::new(BudgetReader {
        file,
        remaining: remaining.clone(),
        exhausted: exhausted.clone(),
        fault: fault.clone(),
    });
    let connection = match crate::image_vfs::open(file) {
        Ok(connection) => connection,
        Err(_) if cancel.load(Ordering::Acquire) => return Ok(None),
        Err(error) => {
            return Err(read_fault(&fault).unwrap_or_else(|| {
                if exhausted.load(Ordering::Acquire) {
                    input_exhausted()
                } else {
                    inspection_error(error, "open canonical catalog inspection")
                }
            }));
        }
    };
    let mut integrity_phase = false;
    let result = (|| {
        let catalog = guarded(&connection, cancel, work_budget, |connection| {
            check_catalog(connection, &expected, metadata_limit)
        })?;
        let Some((catalog, value_limit)) = catalog else {
            return Ok(None);
        };
        if let Some((budget, check)) = after_catalog {
            if cancel.load(Ordering::Acquire) {
                return Ok(None);
            }
            connection
                .set_limit(Limit::SQLITE_LIMIT_LENGTH, value_limit)
                .map_err(|error| inspection_error(error, "restore full-image value limit"))?;
            integrity_phase = true;
            remaining.store(budget, Ordering::Relaxed);
            if check(&connection)?.is_none() {
                return Ok(None);
            }
        }
        Ok(Some(catalog))
    })();
    let closed = connection
        .close()
        .map_err(|(_, error)| inspection_error(error, "close catalog inspection"));
    if cancel.load(Ordering::Acquire) {
        return Ok(None);
    }
    if let Some(error) = read_fault(&fault) {
        return Err(error);
    }
    if exhausted.load(Ordering::Acquire) {
        return Err(if integrity_phase {
            StoreError::corrupt("restore integrity input budget exceeded")
        } else {
            input_exhausted()
        });
    }
    closed?;
    result
}

fn input_exhausted() -> StoreError {
    StoreError::corrupt("restore catalog input budget exceeded")
}

#[cfg(test)]
mod tests;
