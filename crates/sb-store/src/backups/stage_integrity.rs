//! Physical/FK observations over the same catalog-checked retained image.
//! This grants no byte-stability, quarantine, stage or publication authority.

use std::io::{self, SeekFrom};
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::{Connection, types::ValueRef};
use sb_types::fs::ReadFile;

use super::{
    image_inspect::{MAX_PROGRESS_CALLS, guarded},
    stage_canonical,
};
use crate::{Seams, StoreError};
use stage_canonical::inspection_error;

const PAGE_UNIT: u64 = 4096;
const READ_MULTIPLIER: u64 = 64;
const WORK_CALLS_PER_PAGE: u64 = 64;

struct Budgets {
    input: u64,
    work: u32,
}

impl Budgets {
    fn for_size(bytes: u64) -> Result<Self, StoreError> {
        let input = bytes
            .checked_mul(READ_MULTIPLIER)
            .and_then(|n| n.checked_add(2 * 1024 * 1024))
            .ok_or_else(|| StoreError::corrupt("restore image exceeds inspection input budget"))?;
        let work = bytes
            .div_ceil(PAGE_UNIT)
            .saturating_mul(WORK_CALLS_PER_PAGE)
            .saturating_add(u64::from(MAX_PROGRESS_CALLS))
            .min(u64::from(u32::MAX));
        Ok(Self {
            input,
            work: work as u32,
        })
    }
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) struct Inspection {
    pub schema_version: i64,
    pub file_bytes: u64,
}

pub(super) fn inspect(
    file: Box<dyn ReadFile>,
    seams: &Seams,
    cancel: &AtomicBool,
) -> Result<Option<Inspection>, StoreError> {
    inspect_with_budgets(file, seams, cancel, None)
}

fn inspect_with_budgets(
    mut file: Box<dyn ReadFile>,
    seams: &Seams,
    cancel: &AtomicBool,
    budgets: Option<Budgets>,
) -> Result<Option<Inspection>, StoreError> {
    if cancel.load(Ordering::Acquire) {
        return Ok(None);
    }
    // Actual capability length, never a page count from the image header.
    let measured = catch_unwind(AssertUnwindSafe(|| -> io::Result<u64> {
        let bytes = file.seek(SeekFrom::End(0))?;
        file.seek(SeekFrom::Start(0))?;
        Ok(bytes)
    }))
    .unwrap_or_else(|_| Err(io::Error::other("restore size callback panicked")));
    if cancel.load(Ordering::Acquire) {
        return Ok(None);
    }
    let file_bytes = measured
        .map_err(|error| StoreError::unavailable_io("measure restore image", error.kind()))?;
    let budgets = match budgets {
        Some(budgets) => budgets,
        None => Budgets::for_size(file_bytes)?,
    };
    let catalog =
        stage_canonical::inspect_then(file, seams, cancel, budgets.input, |connection| {
            guarded(connection, cancel, budgets.work, check)
        })?;
    Ok(catalog.map(|catalog| Inspection {
        schema_version: catalog.schema_version,
        file_bytes,
    }))
}

fn check(connection: &Connection) -> Result<(), StoreError> {
    let mut integrity = connection
        .prepare("PRAGMA main.integrity_check(8)")
        .map_err(|error| inspection_error(error, "prepare restore integrity check"))?;
    let mut rows = integrity
        .query([])
        .map_err(|error| inspection_error(error, "run restore integrity check"))?;
    let first = rows
        .next()
        .map_err(|error| inspection_error(error, "read restore integrity result"))?
        .ok_or_else(|| StoreError::corrupt("restore integrity check returned no result"))?;
    if first
        .get_ref(0)
        .map_err(|error| inspection_error(error, "read restore integrity value"))?
        != ValueRef::Text(b"ok")
    {
        return Err(StoreError::corrupt(
            "restore image failed physical integrity checking",
        ));
    }
    if rows
        .next()
        .map_err(|error| inspection_error(error, "finish restore integrity check"))?
        .is_some()
    {
        return Err(StoreError::corrupt(
            "restore integrity check returned additional findings",
        ));
    }
    drop(rows);
    drop(integrity);
    let mut foreign_keys = connection
        .prepare("PRAGMA main.foreign_key_check")
        .map_err(|error| inspection_error(error, "prepare restore foreign-key check"))?;
    let mut rows = foreign_keys
        .query([])
        .map_err(|error| inspection_error(error, "run restore foreign-key check"))?;
    if rows
        .next()
        .map_err(|error| inspection_error(error, "read restore foreign-key result"))?
        .is_some()
    {
        return Err(StoreError::corrupt(
            "restore image has foreign-key violations",
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
