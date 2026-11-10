//! The content-change counter behind "a generation is taken every hour
//! while there were changes" (spec/03 "Backup generations", plan
//! M1.8h1).
//!
//! `store_meta.content_changes` gains one in every committed write
//! transaction that changed at least one row, except the store's own
//! bookkeeping (writes submitted with [`Changes::Bookkeeping`]): backup
//! generations, their pins and retention, GC's grace pins, verifier
//! scratch, an export's streamed chunks, publication reservations and
//! the clean-shutdown marker. Those record how the store looks after its
//! own data, not the data, so a backup, a GC pass or a restart never
//! makes the next generation due by itself.
//!
//! A generation records the counter its snapshot holds as
//! `store_meta.backed_up_changes`: a commit landing after the snapshot
//! keeps the store due. The store is due while the counter is ahead,
//! or while it has no complete generation at all.
//!
//! Counting by default is the safe direction: a write site wrongly
//! left counting costs an extra generation, while a site wrongly marked
//! bookkeeping would cost a recovery point.

use rusqlite::{Connection, OptionalExtension, Transaction};

use crate::StoreError;
use crate::schema::classify;

/// Every committed change bumps it (decimal digits).
pub(crate) const CONTENT_CHANGES: &str = "content_changes";
/// The highest counter a complete generation's snapshot held.
pub(crate) const BACKED_UP_CHANGES: &str = "backed_up_changes";
/// The schema version that seeds both keys (migration 25).
pub(crate) const SCHEMA_VERSION: u32 = 25;

/// Whether a write transaction counts toward the next recovery point.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Changes {
    /// The archive's data: a commit that changed a row bumps the
    /// counter.
    Count,
    /// The store's records about itself (see the module doc): never
    /// counted.
    Bookkeeping,
}

/// One counter's value. Missing or malformed is `Corrupt`: migration 25
/// seeds both, and nothing deletes them.
pub(crate) fn read(conn: &Connection, key: &str) -> Result<u64, StoreError> {
    let value = conn
        .query_row("SELECT value FROM store_meta WHERE key=?1", [key], |row| {
            Ok(row
                .get_ref(0)?
                .as_str()
                .ok()
                .and_then(|text| text.parse::<u64>().ok()))
        })
        .optional()
        .map_err(|e| classify(e, "read the content-change counters"))?
        .flatten();
    value
        .filter(|value| *value <= i64::MAX as u64)
        .ok_or_else(|| StoreError::corrupt(format!("store_meta.{key} is missing or malformed")))
}

fn write(tx: &Transaction, key: &str, value: u64) -> Result<(), StoreError> {
    let updated = tx
        .execute(
            "UPDATE store_meta SET value=?2 WHERE key=?1",
            (key, value.to_string()),
        )
        .map_err(|e| classify(e, "record the content-change counters"))?;
    if updated != 1 {
        return Err(StoreError::corrupt(format!("store_meta.{key} is missing")));
    }
    Ok(())
}

/// One more committed change, inside the transaction that made it.
pub(crate) fn bump(tx: &Transaction) -> Result<(), StoreError> {
    let next = read(tx, CONTENT_CHANGES)?
        .checked_add(1)
        .filter(|next| *next <= i64::MAX as u64)
        .ok_or_else(|| StoreError::corrupt("the content-change counter is exhausted"))?;
    write(tx, CONTENT_CHANGES, next)
}

/// A generation whose snapshot held `mark` completed: the store is
/// backed up through `mark`. Never moves backwards, so an older
/// generation completing late can't make the store due again.
pub(crate) fn record_backed_up(tx: &Transaction, mark: u64) -> Result<(), StoreError> {
    if mark > read(tx, BACKED_UP_CHANGES)? {
        write(tx, BACKED_UP_CHANGES, mark)?;
    }
    Ok(())
}

/// Whether the store owes a generation: a change since the newest
/// generation's snapshot, or no complete generation at all (the
/// counters aren't read then: the answer is due either way).
pub(crate) fn backup_due(conn: &Connection) -> Result<bool, StoreError> {
    let complete: bool = conn
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM backup_generations WHERE state = 'complete')",
            [],
            |row| row.get(0),
        )
        .map_err(|e| classify(e, "look for a complete backup generation"))?;
    if !complete {
        return Ok(true);
    }
    Ok(read(conn, CONTENT_CHANGES)? > read(conn, BACKED_UP_CHANGES)?)
}
