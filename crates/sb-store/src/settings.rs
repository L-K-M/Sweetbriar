//! Settings rows (spec/02 "Commands, receipts and settings"): the SQL
//! helpers for the `settings` table. Validation against the registry is
//! the engine's business (the layer that owns the registry); these
//! helpers only read and write rows inside the caller's transaction.

use rusqlite::Transaction;
use rusqlite::params;
use sb_types::time::Timestamp;

use crate::StoreError;
use crate::schema::classify;

/// One stored setting (spec/02): its key, canonical JSON value and
/// revision.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSetting {
    key: String,
    value_json: String,
    revision: i64,
}

impl StoredSetting {
    fn new(key: String, value_json: String, revision: i64) -> Self {
        Self {
            key,
            value_json,
            revision,
        }
    }

    /// The setting's dotted key, e.g. `appearance.compose_cue`.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The stored value as canonical JSON (a string literal for the
    /// pilot's settings), exactly as `upsert` wrote it.
    pub fn value_json(&self) -> &str {
        &self.value_json
    }

    /// How many times the setting was written (1 on the first).
    pub fn revision(&self) -> i64 {
        self.revision
    }
}

/// Every settings row (spec/02), in key order.
pub fn read_all(tx: &Transaction) -> Result<Vec<StoredSetting>, StoreError> {
    let read_err = |e| classify(e, "read the settings");
    let mut stmt = tx
        .prepare("SELECT key, value_json, revision FROM settings ORDER BY key")
        .map_err(read_err)?;
    let rows = stmt
        .query_map([], |row| {
            Ok(StoredSetting::new(row.get(0)?, row.get(1)?, row.get(2)?))
        })
        .map_err(read_err)?
        .collect::<Result<_, _>>()
        .map_err(read_err)?;
    Ok(rows)
}

/// Write one setting (spec/02 rule 9): the first write inserts the row
/// at revision 1; every later one replaces `value_json` and bumps
/// `revision` and `updated_at`. Returns the row's new revision. The
/// caller's transaction makes the write atomic with whatever else it
/// commits — for `set_setting`, its receipt (spec/07).
pub fn upsert(
    tx: &Transaction,
    key: &str,
    value_json: &str,
    updated_at: Timestamp,
) -> Result<i64, StoreError> {
    tx.query_row(
        "INSERT INTO settings (key, value_json, revision, updated_at)
         VALUES (?1, ?2, 1, ?3)
         ON CONFLICT(key) DO UPDATE SET
           value_json = excluded.value_json,
           revision = settings.revision + 1,
           updated_at = excluded.updated_at
         RETURNING revision",
        params![key, value_json, updated_at.as_millis()],
        |row| row.get(0),
    )
    .map_err(|e| classify(e, "write a setting"))
}

#[cfg(test)]
mod tests;
