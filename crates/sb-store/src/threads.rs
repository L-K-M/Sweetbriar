//! Thread membership storage (spec/02 "Threads"): what a rebuild reads,
//! and the writes it publishes with. The grouping itself is the
//! engine's; these helpers run inside the caller's read or write
//! transaction.
//!
//! ```text
//!   imports ──add messages──► thread_inputs += 1
//!   rebuild: read sources ─► group (engine) ─► publish in chunks
//!            └──────────── threads_built := inputs it read ─┘
//! ```

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::StoreError;
use crate::list_rows;
use crate::schema::classify;

/// How often imports added logical messages (spec/02).
pub(crate) const INPUTS_KEY: &str = "thread_inputs";

/// How many of those the last complete rebuild read (spec/02).
pub(crate) const BUILT_KEY: &str = "threads_built";

/// Migration 4's starting watermarks: nothing built, and one rebuild
/// owed only when messages were imported before threads were kept.
pub(crate) const FIRST_BUILT: u64 = 0;
pub(crate) const OWED_AT_MIGRATION: u64 = 1;

/// What one logical message gives threading: header fields where
/// `message_headers` holds them, else the summary's (spec/02).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThreadSource {
    pub message_id: i64,
    pub account_id: String,
    pub header_id: Option<String>,
    pub references: Vec<String>,
    pub in_reply_to: Option<String>,
    pub subject: Option<String>,
    /// The representative's `sort_date`; `None` without one.
    pub representative_date: Option<i64>,
}

/// A thread as stored (spec/02 `d_threads`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredThread {
    pub thread_id: i64,
    pub account_scope: Option<String>,
    pub subject_norm: Option<String>,
    pub last_date: Option<i64>,
}

/// The two watermarks: a rebuild is due while `built < inputs`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadRevisions {
    pub inputs: u64,
    pub built: u64,
}

impl ThreadRevisions {
    pub fn stale(self) -> bool {
        self.built < self.inputs
    }
}

pub fn thread_revisions(tx: &Transaction) -> Result<ThreadRevisions, StoreError> {
    Ok(ThreadRevisions {
        inputs: counter(tx, INPUTS_KEY)?,
        built: counter(tx, BUILT_KEY)?,
    })
}

/// Up to `limit` messages after `after`, by message id.
pub fn thread_sources(
    tx: &Transaction,
    after: i64,
    limit: usize,
) -> Result<Vec<ThreadSource>, StoreError> {
    let mut statement = tx
        .prepare_cached(&format!(
            "SELECT lm.message_id, lm.account_id,
               COALESCE(h.header_message_id, s.header_message_id),
               COALESCE(h.references_json, s.references_json),
               COALESCE(h.in_reply_to, s.in_reply_to),
               COALESCE(h.subject, s.subject), r.sort_date
             FROM {} LEFT JOIN {} LEFT JOIN {}
             LEFT JOIN d_list_rows r ON r.message_id = lm.message_id AND r.representative = 1
             WHERE lm.message_id > ?1 ORDER BY lm.message_id LIMIT ?2",
            crate::message_source::MESSAGES,
            crate::message_source::HEADERS,
            crate::message_source::SUMMARY
        ))
        .map_err(sql_error)?;
    let rows = statement
        .query_map(params![after, limit_value(limit)], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<String>>(5)?,
                row.get::<_, Option<i64>>(6)?,
            ))
        })
        .map_err(sql_error)?;
    let mut sources = Vec::new();
    for row in rows {
        let (message_id, account_id, header_id, references, in_reply_to, subject, date) =
            row.map_err(sql_error)?;
        // Canonical arrays of strings; NULL is the empty list (spec/02).
        let references = match references {
            Some(json) => serde_json::from_str(&json).map_err(|e| {
                StoreError::corrupt(format!("message {message_id} has invalid references: {e}"))
            })?,
            None => Vec::new(),
        };
        sources.push(ThreadSource {
            message_id,
            account_id,
            header_id,
            references,
            in_reply_to,
            subject,
            representative_date: date,
        });
    }
    Ok(sources)
}

/// Up to `limit` stored threads after thread `after`, by id.
pub fn stored_threads(
    tx: &Transaction,
    after: i64,
    limit: usize,
) -> Result<Vec<StoredThread>, StoreError> {
    let mut statement = tx
        .prepare_cached(
            "SELECT thread_id, account_scope, subject_norm, last_date FROM d_threads
             WHERE thread_id > ?1 ORDER BY thread_id LIMIT ?2",
        )
        .map_err(sql_error)?;
    statement
        .query_map(params![after, limit_value(limit)], |row| {
            Ok(StoredThread {
                thread_id: row.get(0)?,
                account_scope: row.get(1)?,
                subject_norm: row.get(2)?,
                last_date: row.get(3)?,
            })
        })
        .map_err(sql_error)?
        .collect::<Result<_, _>>()
        .map_err(sql_error)
}

/// Up to `limit` stored memberships after message `after`, as
/// (message, thread), by message id.
pub fn thread_memberships(
    tx: &Transaction,
    after: i64,
    limit: usize,
) -> Result<Vec<(i64, i64)>, StoreError> {
    let mut statement = tx
        .prepare_cached(
            "SELECT message_id, thread_id FROM d_thread_members
             WHERE message_id > ?1 ORDER BY message_id LIMIT ?2",
        )
        .map_err(sql_error)?;
    statement
        .query_map(params![after, limit_value(limit)], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .map_err(sql_error)?
        .collect::<Result<_, _>>()
        .map_err(sql_error)
}

/// What a rebuild stores for one thread (spec/02 `d_threads`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ThreadDescription<'a> {
    pub account_scope: Option<&'a str>,
    pub subject_norm: Option<&'a str>,
    pub last_date: Option<i64>,
}

/// A new, empty thread's id.
pub fn create_thread(
    tx: &Transaction,
    described: ThreadDescription<'_>,
) -> Result<i64, StoreError> {
    tx.prepare_cached(
        "INSERT INTO d_threads (account_scope, subject_norm, last_date) VALUES (?1, ?2, ?3)",
    )
    .and_then(|mut statement| {
        statement.execute(params![
            described.account_scope,
            described.subject_norm,
            described.last_date
        ])
    })
    .map_err(sql_error)?;
    Ok(tx.last_insert_rowid())
}

/// Store a thread's description; returns whether it changed.
pub fn describe_thread(
    tx: &Transaction,
    thread: i64,
    described: ThreadDescription<'_>,
) -> Result<bool, StoreError> {
    let changed = tx
        .prepare_cached(
            "UPDATE d_threads SET account_scope = ?2, subject_norm = ?3, last_date = ?4
             WHERE thread_id = ?1
               AND (account_scope, subject_norm, last_date) IS NOT (?2, ?3, ?4)",
        )
        .and_then(|mut statement| {
            statement.execute(params![
                thread,
                described.account_scope,
                described.subject_norm,
                described.last_date
            ])
        })
        .map_err(sql_error)?;
    Ok(changed > 0)
}

/// Put `message` in `thread`, leaving any other; returns whether its
/// membership changed.
pub fn assign_thread(tx: &Transaction, message: i64, thread: i64) -> Result<bool, StoreError> {
    // A move, else a first membership; an unchanged one is neither.
    let moved = tx
        .prepare_cached(
            "UPDATE d_thread_members SET thread_id = ?1
             WHERE message_id = ?2 AND thread_id IS NOT ?1",
        )
        .and_then(|mut statement| statement.execute(params![thread, message]))
        .map_err(sql_error)?;
    let added = tx
        .prepare_cached(
            "INSERT OR IGNORE INTO d_thread_members (thread_id, message_id) VALUES (?1, ?2)",
        )
        .and_then(|mut statement| statement.execute(params![thread, message]))
        .map_err(sql_error)?;
    Ok(moved + added > 0)
}

/// Delete a thread no group claims, with any members still in it;
/// returns whether a membership went with it.
pub fn remove_thread(tx: &Transaction, thread: i64) -> Result<bool, StoreError> {
    let members = tx
        .prepare_cached("DELETE FROM d_thread_members WHERE thread_id = ?1")
        .and_then(|mut statement| statement.execute([thread]))
        .map_err(sql_error)?;
    tx.prepare_cached("DELETE FROM d_threads WHERE thread_id = ?1")
        .and_then(|mut statement| statement.execute([thread]))
        .map_err(sql_error)?;
    Ok(members > 0)
}

/// Delete threads without members, as a crash between creating a
/// thread and filling it leaves them.
pub fn remove_empty_threads(tx: &Transaction) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM d_threads WHERE NOT EXISTS
           (SELECT 1 FROM d_thread_members m WHERE m.thread_id = d_threads.thread_id)",
        [],
    )
    .map_err(sql_error)?;
    Ok(())
}

/// Count a published batch of membership changes as a list change
/// (spec/02 `list_generation`).
pub fn note_membership_change(tx: &Transaction) -> Result<(), StoreError> {
    list_rows::advance_generation(tx).map(|_| ())
}

/// Record a complete rebuild of the inputs at `inputs`. A watermark
/// never moves back: a slower rebuild that read fewer inputs leaves it.
pub fn mark_threads_built(tx: &Transaction, inputs: u64) -> Result<(), StoreError> {
    if inputs > counter(tx, BUILT_KEY)? {
        set_counter(tx, BUILT_KEY, inputs)?;
    }
    Ok(())
}

/// Count an import transaction that added logical messages.
pub(crate) fn note_new_messages(tx: &Transaction) -> Result<(), StoreError> {
    let next = counter(tx, INPUTS_KEY)?
        .checked_add(1)
        .ok_or_else(|| StoreError::corrupt("the thread input count is exhausted"))?;
    set_counter(tx, INPUTS_KEY, next)
}

fn counter(conn: &Connection, key: &str) -> Result<u64, StoreError> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM store_meta WHERE key = ?1",
            [key],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_error)?;
    value
        .and_then(|value| value.parse().ok())
        .ok_or_else(|| StoreError::corrupt(format!("store_meta has no valid {key}")))
}

fn set_counter(tx: &Transaction, key: &str, value: u64) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE store_meta SET value = ?2 WHERE key = ?1",
        params![key, value.to_string()],
    )
    .map_err(sql_error)?;
    Ok(())
}

fn limit_value(limit: usize) -> i64 {
    i64::try_from(limit).unwrap_or(i64::MAX)
}

fn sql_error(error: rusqlite::Error) -> StoreError {
    classify(error, "thread storage")
}

#[cfg(test)]
mod tests;
