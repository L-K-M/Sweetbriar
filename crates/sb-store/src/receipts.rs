//! Idempotent command receipts (spec/02 `command_receipts`, spec/07
//! "Commands"): a command runs under an idempotency key inside one
//! write transaction; the effect and its receipt commit together, so
//! a retry that finds the receipt never re-runs the effect.

use rusqlite::Transaction;
use rusqlite::params;
use sb_types::time::Timestamp;

use crate::StoreError;
use crate::schema::classify;

/// A job command's receipt body (spec/07 "Engine command boundary"):
/// `{"job_id":"<id>"}` plus the command's optional `"state"` word. The
/// id is any nonempty stored string — escapes, control characters and
/// non-ASCII included — so it is encoded as JSON, never pasted into a
/// template. `serde_json`'s `Map` is sorted, so `job_id` precedes
/// `state` as the handwritten receipts always had it.
pub fn encode_job_receipt(job_id: &str, state: Option<&str>) -> String {
    let mut body = serde_json::Map::new();
    body.insert("job_id".to_owned(), job_id.into());
    if let Some(state) = state {
        body.insert("state".to_owned(), state.into());
    }
    serde_json::Value::Object(body).to_string()
}

/// The job id a stored receipt names (spec/07): a replayed command's
/// effects target this id, never a new caller argument. Malformed
/// stored JSON, or a `job_id` that is missing, empty or not a string,
/// is `Corrupt` — never an invented target.
pub fn receipt_job_id(receipt: &Receipt) -> Result<String, StoreError> {
    let parsed: serde_json::Value = serde_json::from_str(receipt.result_json())
        .map_err(|_| StoreError::corrupt("a stored command receipt is not JSON"))?;
    match parsed["job_id"].as_str() {
        Some(id) if !id.is_empty() => Ok(id.to_owned()),
        _ => Err(StoreError::corrupt("a stored command receipt names no job")),
    }
}

/// The recorded outcome of one command (spec/02): what ran, what it
/// answered, and when. A repeated idempotency key returns this stored
/// receipt without running anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Receipt {
    command: String,
    result_json: String,
    created_at: Timestamp,
}

impl Receipt {
    fn new(command: String, result_json: String, created_at: Timestamp) -> Self {
        Self {
            command,
            result_json,
            created_at,
        }
    }

    /// The command the receipt was recorded for.
    pub fn command(&self) -> &str {
        &self.command
    }

    /// The command's answer, as canonical JSON the caller wrote.
    pub fn result_json(&self) -> &str {
        &self.result_json
    }

    /// When the receipt was recorded (spec/02 rule 4).
    pub fn created_at(&self) -> Timestamp {
        self.created_at
    }
}

/// Run `command` once under `key`, inside the caller's write
/// transaction (spec/07): if `command_receipts` already holds `key`
/// for the same command, return the stored receipt without calling
/// `f`; for another command, the key was spent on something else:
/// `Conflict`. Otherwise run `f` — its `String` is the result JSON —
/// insert the receipt row, and return the receipt. Effect and receipt
/// commit together or not at all: they share `tx`.
pub fn run_once(
    tx: &Transaction,
    key: &str,
    command: &str,
    now: Timestamp,
    f: impl FnOnce(&Transaction) -> Result<String, StoreError>,
) -> Result<Receipt, StoreError> {
    match find(tx, key)? {
        Some(receipt) if receipt.command == command => Ok(receipt),
        Some(receipt) => Err(StoreError::conflict(format!(
            "idempotency key '{key}' was already used for command '{}'",
            receipt.command
        ))),
        None => {
            let result_json = f(tx)?;
            insert(tx, key, command, &result_json, now)?;
            Ok(Receipt::new(command.to_owned(), result_json, now))
        }
    }
}

/// The stored receipt for `key`, if any. The table is `STRICT` with
/// typed columns (spec/02), so a row that won't read back as one is a
/// SQLite failure, mapped by `classify`.
fn find(tx: &Transaction, key: &str) -> Result<Option<Receipt>, StoreError> {
    let read_err = |e| classify(e, "read a command receipt");
    let mut stmt = tx
        .prepare(
            "SELECT command, result_json, created_at FROM command_receipts
             WHERE idempotency_key = ?1",
        )
        .map_err(read_err)?;
    let mut rows = stmt.query(params![key]).map_err(read_err)?;
    let Some(row) = rows.next().map_err(read_err)? else {
        return Ok(None);
    };
    let command: String = row.get(0).map_err(read_err)?;
    let result_json: String = row.get(1).map_err(read_err)?;
    let created_at: i64 = row.get(2).map_err(read_err)?;
    Ok(Some(Receipt::new(
        command,
        result_json,
        Timestamp::from_millis(created_at),
    )))
}

/// Record the receipt row.
fn insert(
    tx: &Transaction,
    key: &str,
    command: &str,
    result_json: &str,
    now: Timestamp,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO command_receipts (idempotency_key, command, result_json, created_at)
         VALUES (?1, ?2, ?3, ?4)",
        params![key, command, result_json, now.as_millis()],
    )
    .map_err(|e| classify(e, "record a command receipt"))?;
    Ok(())
}

#[cfg(test)]
mod tests;
