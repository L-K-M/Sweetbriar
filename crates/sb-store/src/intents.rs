//! Parked and quarantined intents (spec/07 `intents(filter)`, spec/02
//! "Intents"): the health surface's intents as one keyset page, oldest
//! first, filtered by account and state. Read-only; nothing here writes.
//!
//! The row carries the intent's effect as stored: `set_flags`' flag
//! deltas, a move or copy's destination mailbox, `delete_to_trash`, or
//! a reserved operation's raw args. The target is the occurrence ids
//! `target_json` holds, not a resolved message.

use rusqlite::types::Value;
use rusqlite::{OptionalExtension, Transaction};
use serde_json::Value as Json;

use crate::StoreError;
use crate::lists::{PageRead, bound};
use crate::schema::classify;

/// The largest page (spec/07: at most 1,000 rows).
pub const MAX_INTENT_PAGE_ROWS: usize = 1_000;

/// The states the health surface lists (spec/02 "Intents").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentState {
    Parked,
    Quarantined,
}

impl IntentState {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Parked => "parked",
            Self::Quarantined => "quarantined",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        match text {
            "parked" => Some(Self::Parked),
            "quarantined" => Some(Self::Quarantined),
            _ => None,
        }
    }
}

/// What the intent does (spec/02 `intents.op`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentKind {
    SetFlags,
    Move,
    Copy,
    DeleteToTrash,
    /// Reserved, not produced in the pilot (spec/02).
    ExpungeScoped,
    /// Reserved, not produced in the pilot (spec/02).
    Append,
    /// Reserved, not produced in the pilot; sends use send attempts.
    Submit,
}

impl IntentKind {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "set_flags" => Some(Self::SetFlags),
            "move" => Some(Self::Move),
            "copy" => Some(Self::Copy),
            "delete_to_trash" => Some(Self::DeleteToTrash),
            "expunge_scoped" => Some(Self::ExpungeScoped),
            "append" => Some(Self::Append),
            "submit" => Some(Self::Submit),
            _ => None,
        }
    }
}

/// Why a quarantined intent waits for the user (spec/02).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuarantineReason {
    EpochChange,
    Restore,
    AmbiguousMatch,
    ConfigChange,
    Breaker,
}

impl QuarantineReason {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "epoch_change" => Some(Self::EpochChange),
            "restore" => Some(Self::Restore),
            "ambiguous_match" => Some(Self::AmbiguousMatch),
            "config_change" => Some(Self::ConfigChange),
            "breaker" => Some(Self::Breaker),
            _ => None,
        }
    }
}

/// The intent's effect as stored: flag deltas, a destination mailbox,
/// a trash move, or a reserved operation's raw args.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntentEffect {
    SetFlags {
        add: Vec<String>,
        remove: Vec<String>,
    },
    ToMailbox {
        mailbox_id: i64,
    },
    DeleteToTrash,
    /// A reserved operation, not produced in the pilot (spec/02); its
    /// args stay inspectable rather than guessed at.
    Reserved {
        args_json: String,
    },
}

/// One parked or quarantined intent as the health surface shows it.
/// Fields are public for the engine's and service's mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredIntent {
    pub intent_id: String,
    pub account_id: String,
    pub kind: IntentKind,
    pub state: IntentState,
    /// The target occurrence ids `target_json` holds.
    pub target_occurrence_ids: Vec<i64>,
    /// Set on a quarantined intent.
    pub quarantine_reason: Option<QuarantineReason>,
    /// A parked intent's terminal error, when one was recorded.
    pub last_error: Option<String>,
    pub effect: IntentEffect,
    pub created_at: i64,
}

/// Which parked or quarantined intents a page reads (spec/07
/// `intents(filter)`); `None` leaves a dimension unfiltered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IntentFilter {
    pub account_id: Option<String>,
    pub state: Option<IntentState>,
}

/// A row's position in the oldest-first order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentKey {
    pub created_at: i64,
    pub intent_id: String,
}

/// Where an intent page starts.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntentAnchor {
    Top,
    /// Rows after the key's row in the oldest-first order.
    After(IntentKey),
}

/// One page of parked or quarantined intents, oldest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentPage {
    pub rows: Vec<StoredIntent>,
    /// Whether more rows follow the page's last one.
    pub has_more: bool,
}

/// Read one page of the health surface's intents, oldest first, at most
/// `limit` rows. A cursor's row that is no longer in the filtered set is
/// [`PageRead::NotInWindow`]; a malformed stored payload is `Corrupt`.
pub fn intents(
    tx: &Transaction,
    filter: &IntentFilter,
    anchor: IntentAnchor,
    limit: usize,
) -> Result<PageRead<IntentPage>, StoreError> {
    if !(1..=MAX_INTENT_PAGE_ROWS).contains(&limit) {
        return Err(StoreError::InvalidInput {
            message: format!("an intent page holds 1 to {MAX_INTENT_PAGE_ROWS} rows"),
        });
    }
    if let IntentAnchor::After(key) = &anchor
        && !contains(tx, filter, key)?
    {
        return Ok(PageRead::NotInWindow);
    }
    let mut sql = String::from(
        "SELECT intent_id, account_id, op, state, target_json, quarantine_reason,
                last_error, args_json, created_at
         FROM intents
         WHERE state IN ('parked','quarantined')
           AND (:account IS NULL OR account_id = :account)
           AND (:state IS NULL OR state = :state)",
    );
    if matches!(anchor, IntentAnchor::After(_)) {
        sql.push_str(
            " AND (created_at > :cursor_created
                   OR (created_at = :cursor_created AND intent_id > :cursor_id))",
        );
    }
    sql.push_str(" ORDER BY created_at, intent_id LIMIT :limit");

    let state = filter.state.map(IntentState::as_str);
    let mut params: Vec<(&'static str, Value)> = vec![
        (
            ":account",
            filter
                .account_id
                .clone()
                .map(Value::Text)
                .unwrap_or(Value::Null),
        ),
        (
            ":state",
            state
                .map(|state| Value::Text(state.to_owned()))
                .unwrap_or(Value::Null),
        ),
        (
            ":limit",
            Value::Integer(i64::try_from(limit + 1).unwrap_or(i64::MAX)),
        ),
    ];
    if let IntentAnchor::After(key) = &anchor {
        params.push((":cursor_created", Value::Integer(key.created_at)));
        params.push((":cursor_id", Value::Text(key.intent_id.clone())));
    }
    let mut statement = tx.prepare(&sql).map_err(sql_error)?;
    let stored = statement
        .query_map(bound(&params).as_slice(), StoredRow::from_row)
        .map_err(sql_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_error)?;
    let mut rows = Vec::with_capacity(stored.len());
    for row in stored {
        rows.push(row.into_intent()?);
    }
    let has_more = rows.len() > limit;
    rows.truncate(limit);
    Ok(PageRead::Ready(IntentPage { rows, has_more }))
}

/// Whether `key`'s row is in the filtered set, with the same position.
fn contains(tx: &Transaction, filter: &IntentFilter, key: &IntentKey) -> Result<bool, StoreError> {
    let state = filter.state.map(IntentState::as_str);
    let params = vec![
        (
            ":account",
            filter
                .account_id
                .clone()
                .map(Value::Text)
                .unwrap_or(Value::Null),
        ),
        (
            ":state",
            state
                .map(|state| Value::Text(state.to_owned()))
                .unwrap_or(Value::Null),
        ),
        (":created_at", Value::Integer(key.created_at)),
        (":intent_id", Value::Text(key.intent_id.clone())),
    ];
    tx.prepare_cached(
        "SELECT 1 FROM intents
         WHERE intent_id = :intent_id AND created_at = :created_at
           AND state IN ('parked','quarantined')
           AND (:account IS NULL OR account_id = :account)
           AND (:state IS NULL OR state = :state)",
    )
    .and_then(|mut statement| {
        statement.query_row(bound(&params).as_slice(), |row| row.get::<_, i64>(0))
    })
    .optional()
    .map(|found| found.is_some())
    .map_err(sql_error)
}

/// One raw stored row, before its JSON payloads are parsed.
struct StoredRow {
    intent_id: String,
    account_id: String,
    op: String,
    state: String,
    target_json: String,
    quarantine_reason: Option<String>,
    last_error: Option<String>,
    args_json: String,
    created_at: i64,
}

impl StoredRow {
    fn from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<Self> {
        Ok(Self {
            intent_id: row.get(0)?,
            account_id: row.get(1)?,
            op: row.get(2)?,
            state: row.get(3)?,
            target_json: row.get(4)?,
            quarantine_reason: row.get(5)?,
            last_error: row.get(6)?,
            args_json: row.get(7)?,
            created_at: row.get(8)?,
        })
    }

    /// The typed row; an unknown state or a malformed payload is
    /// `Corrupt`, never a defaulted value (spec/02's schema contract).
    fn into_intent(self) -> Result<StoredIntent, StoreError> {
        let kind = IntentKind::parse(&self.op).ok_or_else(|| {
            StoreError::corrupt(format!("an intent's op {:?} is unknown", self.op))
        })?;
        let state = IntentState::parse(&self.state).ok_or_else(|| {
            StoreError::corrupt(format!("an intent's state {:?} is unknown", self.state))
        })?;
        let quarantine_reason = self
            .quarantine_reason
            .map(|reason| {
                QuarantineReason::parse(&reason).ok_or_else(|| {
                    StoreError::corrupt(format!(
                        "an intent's quarantine_reason {reason:?} is unknown"
                    ))
                })
            })
            .transpose()?;
        Ok(StoredIntent {
            intent_id: self.intent_id,
            account_id: self.account_id,
            kind,
            state,
            target_occurrence_ids: parse_target(&self.target_json)?,
            quarantine_reason,
            last_error: self.last_error,
            effect: parse_effect(kind, &self.args_json)?,
            created_at: self.created_at,
        })
    }
}

/// `target_json`'s occurrence ids: `{"v":1,"occurrence_ids":[…]}`.
fn parse_target(text: &str) -> Result<Vec<i64>, StoreError> {
    let malformed = || StoreError::corrupt(format!("an intent's target_json is malformed: {text}"));
    let json: Json = serde_json::from_str(text).map_err(|_| malformed())?;
    let object = json.as_object().ok_or_else(malformed)?;
    if object.get("v").and_then(Json::as_u64) != Some(1) {
        return Err(malformed());
    }
    let ids = object
        .get("occurrence_ids")
        .and_then(Json::as_array)
        .ok_or_else(malformed)?;
    ids.iter()
        .map(|id| id.as_i64().ok_or_else(malformed))
        .collect()
}

/// `args_json` as the operation's typed effect (spec/02's `args_json`
/// examples: `{"add":["\\Seen"],"remove":[]}`, a destination mailbox).
fn parse_effect(kind: IntentKind, text: &str) -> Result<IntentEffect, StoreError> {
    let malformed = || StoreError::corrupt(format!("an intent's args_json is malformed: {text}"));
    if matches!(
        kind,
        IntentKind::ExpungeScoped | IntentKind::Append | IntentKind::Submit
    ) {
        return Ok(IntentEffect::Reserved {
            args_json: text.to_owned(),
        });
    }
    let json: Json = serde_json::from_str(text).map_err(|_| malformed())?;
    let object = json.as_object().ok_or_else(malformed)?;
    if object.get("v").and_then(Json::as_u64) != Some(1) {
        return Err(malformed());
    }
    match kind {
        IntentKind::SetFlags => Ok(IntentEffect::SetFlags {
            add: string_array(object, "add").ok_or_else(malformed)?,
            remove: string_array(object, "remove").ok_or_else(malformed)?,
        }),
        IntentKind::Move | IntentKind::Copy => {
            let mailbox_id = object
                .get("to_mailbox_id")
                .and_then(Json::as_i64)
                .ok_or_else(malformed)?;
            Ok(IntentEffect::ToMailbox { mailbox_id })
        }
        IntentKind::DeleteToTrash => Ok(IntentEffect::DeleteToTrash),
        IntentKind::ExpungeScoped | IntentKind::Append | IntentKind::Submit => unreachable!(),
    }
}

/// A stored JSON array of strings.
fn string_array(object: &serde_json::Map<String, Json>, key: &str) -> Option<Vec<String>> {
    let ids = object.get(key)?.as_array()?;
    ids.iter()
        .map(|value| value.as_str().map(str::to_owned))
        .collect()
}

fn sql_error(error: rusqlite::Error) -> StoreError {
    classify(error, "an intent read")
}

#[cfg(test)]
mod tests;
