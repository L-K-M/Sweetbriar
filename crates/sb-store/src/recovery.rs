//! The Recovery list (spec/07 `recovery(account_id?)`, spec/04
//! "Epoch change"): logical messages with no `present` occurrence,
//! each shown by its last occurrence's mailbox, state and time, newest
//! first, keyset paged. Nothing here deletes or writes; the rows stay
//! for history (spec/04).
//!
//! A message whose last occurrence is only `vanish_pending` is held by
//! the breaker, not confirmed gone, so it is not listed (spec/04
//! "The breaker").

use rusqlite::types::Value;
use rusqlite::{OptionalExtension, Transaction};

use crate::StoreError;
use crate::lists::{PageRead, bound};
use crate::schema::classify;

/// The largest page (spec/07: at most 1,000 rows).
pub const MAX_RECOVERY_PAGE_ROWS: usize = 1_000;

/// The occurrence states the Recovery list shows (spec/07).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryState {
    Vanished,
    EpochStale,
    Superseded,
}

impl RecoveryState {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "vanished" => Some(Self::Vanished),
            "epoch_stale" => Some(Self::EpochStale),
            "superseded" => Some(Self::Superseded),
            _ => None,
        }
    }
}

/// One Recovery row: the logical message and its last occurrence's
/// mailbox, state and time. Fields are public for the mappings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredRecovery {
    pub message_id: i64,
    pub account_id: String,
    pub mailbox_id: i64,
    pub mailbox_name: String,
    pub state: RecoveryState,
    /// The last occurrence's `last_seen_at`, epoch milliseconds.
    pub last_seen_at: i64,
    /// The last occurrence's id, the order's tie-break.
    pub occurrence_id: i64,
}

/// A row's position in the newest-first order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryKey {
    pub last_seen_at: i64,
    pub occurrence_id: i64,
}

/// Where a Recovery page starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryAnchor {
    Top,
    /// Rows strictly older than the key's row.
    After(RecoveryKey),
}

/// One page of the Recovery list, newest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryPage {
    pub rows: Vec<StoredRecovery>,
    /// Whether more rows follow the page's last one.
    pub has_more: bool,
}

/// The shared relation: for every message with no `present`
/// occurrence, its last occurrence, which must be one of the listed
/// states, so a message whose last occurrence is the breaker's
/// `vanish_pending` drops out. `cursor` adds the keyset, or the
/// anchor's own row for the cursor's membership check. The account
/// filter sits on the occurrence, whose account the schema's foreign
/// keys tie to the message's.
fn source(cursor: &str) -> String {
    format!(
        "FROM occurrences o
         JOIN logical_messages m ON m.message_id = o.message_id
         JOIN mailboxes mb ON mb.mailbox_id = o.mailbox_id
         WHERE o.state IN ('vanished','epoch_stale','superseded')
           AND o.occurrence_id = (
               SELECT o2.occurrence_id FROM occurrences o2
               WHERE o2.message_id = o.message_id
               ORDER BY o2.last_seen_at DESC, o2.occurrence_id DESC
               LIMIT 1
           )
           AND NOT EXISTS (
               SELECT 1 FROM occurrences p
               WHERE p.message_id = o.message_id AND p.state = 'present'
           )
           AND (:account IS NULL OR o.account_id = :account)
           {cursor}"
    )
}

/// Read one page of the Recovery list, newest first, at most `limit`
/// rows. An unknown account is `NotFound`; a cursor's row that is no
/// longer in the list is [`PageRead::NotInWindow`].
pub fn recovery(
    tx: &Transaction,
    account: Option<&str>,
    anchor: RecoveryAnchor,
    limit: usize,
) -> Result<PageRead<RecoveryPage>, StoreError> {
    if !(1..=MAX_RECOVERY_PAGE_ROWS).contains(&limit) {
        return Err(StoreError::InvalidInput {
            message: format!("a recovery page holds 1 to {MAX_RECOVERY_PAGE_ROWS} rows"),
        });
    }
    if let Some(account) = account {
        let known: Option<i64> = tx
            .prepare_cached("SELECT 1 FROM accounts WHERE account_id = ?1")
            .and_then(|mut statement| statement.query_row([account], |row| row.get(0)))
            .optional()
            .map_err(sql_error)?;
        if known.is_none() {
            return Err(StoreError::not_found(format!("account {account}")));
        }
    }
    let cursor = match &anchor {
        RecoveryAnchor::Top => String::new(),
        RecoveryAnchor::After(key) => {
            let params = key_params(account, key);
            let found: Option<i64> = tx
                .prepare_cached(&format!(
                    "SELECT 1 {} LIMIT 1",
                    source(
                        "AND o.occurrence_id = :occurrence_id AND o.last_seen_at = :last_seen_at"
                    )
                ))
                .and_then(|mut statement| {
                    statement.query_row(bound(&params).as_slice(), |row| row.get(0))
                })
                .optional()
                .map_err(sql_error)?;
            if found.is_none() {
                return Ok(PageRead::NotInWindow);
            }
            "AND (o.last_seen_at < :last_seen_at
                 OR (o.last_seen_at = :last_seen_at AND o.occurrence_id < :occurrence_id))"
                .to_owned()
        }
    };

    let sql = format!(
        "SELECT m.message_id, m.account_id, o.mailbox_id, mb.remote_name, o.state,
                o.last_seen_at, o.occurrence_id
         {} ORDER BY o.last_seen_at DESC, o.occurrence_id DESC LIMIT :limit",
        source(&cursor)
    );
    let mut params: Vec<(&'static str, Value)> = vec![(
        ":account",
        account
            .map(|account| Value::Text(account.to_owned()))
            .unwrap_or(Value::Null),
    )];
    if let RecoveryAnchor::After(key) = &anchor {
        params.push((":last_seen_at", Value::Integer(key.last_seen_at)));
        params.push((":occurrence_id", Value::Integer(key.occurrence_id)));
    }
    params.push((
        ":limit",
        Value::Integer(i64::try_from(limit + 1).unwrap_or(i64::MAX)),
    ));
    let mut statement = tx.prepare(&sql).map_err(sql_error)?;
    let stored = statement
        .query_map(bound(&params).as_slice(), StoredRow::from_row)
        .map_err(sql_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_error)?;
    let mut rows = Vec::with_capacity(stored.len());
    for row in stored {
        rows.push(row.into_recovery()?);
    }
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    Ok(PageRead::Ready(RecoveryPage { rows, has_more }))
}

/// The anchor's own parameters, for its row's membership check.
fn key_params(account: Option<&str>, key: &RecoveryKey) -> Vec<(&'static str, Value)> {
    vec![
        (
            ":account",
            account
                .map(|account| Value::Text(account.to_owned()))
                .unwrap_or(Value::Null),
        ),
        (":occurrence_id", Value::Integer(key.occurrence_id)),
        (":last_seen_at", Value::Integer(key.last_seen_at)),
    ]
}

/// One raw stored row, before its state is parsed.
struct StoredRow {
    message_id: i64,
    account_id: String,
    mailbox_id: i64,
    mailbox_name: String,
    state: String,
    last_seen_at: i64,
    occurrence_id: i64,
}

impl StoredRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            message_id: row.get(0)?,
            account_id: row.get(1)?,
            mailbox_id: row.get(2)?,
            mailbox_name: row.get(3)?,
            state: row.get(4)?,
            last_seen_at: row.get(5)?,
            occurrence_id: row.get(6)?,
        })
    }

    /// The typed row; an unknown state is `Corrupt`, never defaulted.
    fn into_recovery(self) -> Result<StoredRecovery, StoreError> {
        let state = RecoveryState::parse(&self.state).ok_or_else(|| {
            StoreError::corrupt(format!(
                "a recovery occurrence's state {:?} is unknown",
                self.state
            ))
        })?;
        Ok(StoredRecovery {
            message_id: self.message_id,
            account_id: self.account_id,
            mailbox_id: self.mailbox_id,
            mailbox_name: self.mailbox_name,
            state,
            last_seen_at: self.last_seen_at,
            occurrence_id: self.occurrence_id,
        })
    }
}

fn sql_error(error: rusqlite::Error) -> StoreError {
    classify(error, "a recovery read")
}

#[cfg(test)]
mod tests;
