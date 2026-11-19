//! Proton's breaker (spec/04-sync-proton "The breaker", 04-sync-imap
//! "The breaker"): a delete event withholds the message as
//! `vanish_pending` and records it in `proton_disappearances`; at the
//! end of each events response the undecided deletes are classified
//! per mailbox of the message's location. Retention deletes (from
//! Trash or Spam) apply; others apply under the thresholds and open a
//! `breaker_event` above them, which holds them until the user accepts
//! or keeps it, or they reappear.
//!
//! While an account has an `open` event its deletes are recorded but
//! not classified: destructive reconciliation is paused. A message only
//! a listing missed waits, unconfirmed, for a second listing or a
//! delete event (`refresh`). Explained
//! deletes (a matching `done` intent) arrive with M3's intents.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{OptionalExtension, Transaction, params};
use sb_types::time::Timestamp;

use super::listing::current_occurrences;
use crate::StoreError;
use crate::schema::classify;

/// More unexplained disappearances than this in one mailbox in one
/// pass open an event (04-sync-imap "The breaker").
pub const MAX_DISAPPEARANCES_PER_MAILBOX: usize = 25;
/// A mailbox holding at least this many messages opens an event when
/// more than half of them disappear in one pass.
pub const MIN_MAILBOX_FOR_SHARE: usize = 8;

/// What a classification did.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Classified {
    /// Deletes applied: their occurrences became `vanished`.
    pub applied: usize,
    /// Events opened.
    pub opened: usize,
}

/// Withhold a deleted message's current occurrences and record the
/// delete for the breaker, with the cursor of the response reporting
/// it. An unknown id, or a message with no current occurrence, changes
/// nothing; a delete already recorded keeps its first location, and
/// confirms a disappearance only a listing saw. Returns the
/// occurrences withheld.
pub(super) fn record_delete(
    tx: &Transaction,
    account_id: &str,
    from_cursor: &str,
    provider_key: &str,
    changed: &mut BTreeSet<i64>,
) -> Result<usize, StoreError> {
    let message_id: Option<i64> = tx
        .query_row(
            "SELECT message_id FROM logical_messages WHERE account_id = ?1 AND provider_key = ?2",
            params![account_id, provider_key],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| classify(e, "find a deleted Proton message"))?;
    let Some(message_id) = message_id else {
        return Ok(0);
    };
    withhold(
        tx,
        account_id,
        message_id,
        from_cursor,
        Evidence::Delete,
        changed,
    )
}

/// What says a message disappeared (spec/04-sync-proton "Refresh").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Evidence {
    /// A delete event, or a second complete listing without it: the
    /// breaker classifies it.
    Delete,
    /// One complete listing without it: held until a second listing or
    /// a delete event confirms it.
    Listing,
}

/// Withhold a message's current occurrences as `vanish_pending` and
/// record its disappearance at `pass_cursor`. A message with no
/// current occurrence changes nothing. Returns the occurrences
/// withheld.
pub(super) fn withhold(
    tx: &Transaction,
    account_id: &str,
    message_id: i64,
    pass_cursor: &str,
    evidence: Evidence,
    changed: &mut BTreeSet<i64>,
) -> Result<usize, StoreError> {
    let current = current_occurrences(tx, message_id)?;
    if current.is_empty() {
        return Ok(0);
    }
    // A Proton message's location is its one folder; All Mail, Starred
    // and labels are views of it.
    tx.execute(
        "INSERT INTO proton_disappearances (message_id, account_id, pass_cursor, mailbox_id, confirmed)
         VALUES (?1, ?2, ?3, (SELECT o.mailbox_id FROM occurrences o
                          JOIN mailboxes m ON m.mailbox_id = o.mailbox_id
                          WHERE o.message_id = ?1 AND o.state IN ('present', 'vanish_pending')
                            AND m.kind = 'folder'
                          ORDER BY o.occurrence_id LIMIT 1), ?4)
         ON CONFLICT (message_id) DO UPDATE SET confirmed = 1, pass_cursor = excluded.pass_cursor
           WHERE excluded.confirmed = 1 AND confirmed = 0 AND event_id IS NULL",
        params![
            message_id,
            account_id,
            pass_cursor,
            evidence == Evidence::Delete
        ],
    )
    .map_err(|e| classify(e, "record a disappeared Proton message"))?;

    let mut withheld = 0;
    for (occurrence_id, _) in current {
        let updated = tx
            .execute(
                "UPDATE occurrences SET state = 'vanish_pending'
                 WHERE occurrence_id = ?1 AND state = 'present'",
                params![occurrence_id],
            )
            .map_err(|e| classify(e, "withhold a disappeared Proton occurrence"))?;
        if updated > 0 {
            changed.insert(occurrence_id);
            withheld += 1;
        }
    }
    Ok(withheld)
}

/// A message listed or changed again has reappeared: its delete is
/// forgotten, and an `open` or `kept` event left with no message
/// becomes `cleared`.
pub(super) fn reappeared(
    tx: &Transaction,
    message_id: i64,
    now: Timestamp,
) -> Result<(), StoreError> {
    let held: Option<Option<String>> = tx
        .query_row(
            "DELETE FROM proton_disappearances WHERE message_id = ?1 RETURNING event_id",
            params![message_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| classify(e, "forget a reappeared Proton message"))?;
    let Some(Some(event_id)) = held else {
        return Ok(());
    };
    tx.execute(
        "UPDATE breaker_events SET state = 'cleared', resolved_at = ?2
         WHERE event_id = ?1 AND state IN ('open', 'kept')
           AND NOT EXISTS (SELECT 1 FROM proton_disappearances WHERE event_id = ?1)",
        params![event_id, now.as_millis()],
    )
    .map_err(|e| classify(e, "clear a breaker event"))?;
    Ok(())
}

/// Whether the account's destructive reconciliation is paused by an
/// `open` event.
fn paused(tx: &Transaction, account_id: &str) -> Result<bool, StoreError> {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM breaker_events WHERE account_id = ?1 AND state = 'open')",
        params![account_id],
        |row| row.get(0),
    )
    .map_err(|e| classify(e, "read the account's open breaker events"))
}

/// Which undecided deletes a classification takes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Pass {
    /// A response's last chunk: every undecided delete, its own
    /// included.
    ResponseEnd,
    /// A resolved event: only those of completed responses. A response
    /// still being applied in chunks recorded its deletes at the
    /// account's current cursor, and its last chunk classifies them.
    Resolved,
}

/// Classify the account's undecided deletes, one pass (spec/04-sync-
/// proton "The breaker"); nothing while an `open` event pauses it.
pub(super) fn classify_deletes(
    tx: &Transaction,
    account_id: &str,
    pass: Pass,
    now: Timestamp,
) -> Result<Classified, StoreError> {
    let mut classified = Classified::default();
    if paused(tx, account_id)? {
        return Ok(classified);
    }
    let read = |e| classify(e, "read the account's undecided Proton deletes");
    let mut by_mailbox: BTreeMap<Option<i64>, (Option<String>, Vec<i64>)> = BTreeMap::new();
    {
        let mut statement = tx
            .prepare(
                "SELECT d.message_id, d.mailbox_id, m.role FROM proton_disappearances d
                 LEFT JOIN mailboxes m ON m.mailbox_id = d.mailbox_id
                 WHERE d.account_id = ?1 AND d.event_id IS NULL AND d.confirmed = 1
                   AND (?2 OR d.pass_cursor IS NOT (SELECT event_cursor FROM proton_accounts
                                                    WHERE account_id = ?1))
                 ORDER BY d.message_id",
            )
            .map_err(read)?;
        let rows = statement
            .query_map(params![account_id, pass == Pass::ResponseEnd], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(read)?;
        for row in rows {
            let (message_id, mailbox_id, role): (i64, Option<i64>, Option<String>) =
                row.map_err(read)?;
            by_mailbox
                .entry(mailbox_id)
                .or_insert_with(|| (role, Vec::new()))
                .1
                .push(message_id);
        }
    }

    for (mailbox_id, (role, messages)) in by_mailbox {
        // Proton can empty Trash and Spam by itself: retention.
        let retention = matches!(role.as_deref(), Some("trash" | "junk"));
        let opens = if retention {
            None
        } else {
            breach(tx, mailbox_id, messages.len())?
        };
        match opens {
            None => {
                for message_id in &messages {
                    vanish(tx, *message_id)?;
                }
                classified.applied += messages.len();
            }
            Some((kind, before)) => {
                open_event(tx, account_id, mailbox_id, kind, before, &messages, now)?;
                classified.opened += 1;
            }
        }
    }
    Ok(classified)
}

/// The event kind and the mailbox's count before the pass, when
/// `disappeared` unexplained deletes from the mailbox breach a
/// threshold (04-sync-imap "Pause, conservative defaults"). A delete
/// with no location counts only against the per-mailbox count.
pub(super) fn breach(
    tx: &Transaction,
    mailbox_id: Option<i64>,
    disappeared: usize,
) -> Result<Option<(&'static str, Option<usize>)>, StoreError> {
    let Some(mailbox_id) = mailbox_id else {
        return Ok(
            (disappeared > MAX_DISAPPEARANCES_PER_MAILBOX).then_some(("bulk_disappearance", None))
        );
    };
    let remaining: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM occurrences WHERE mailbox_id = ?1 AND state = 'present'",
            params![mailbox_id],
            |row| row.get(0),
        )
        .map_err(|e| classify(e, "count a mailbox's present messages"))?;
    let remaining = usize::try_from(remaining).unwrap_or(usize::MAX);
    let before = remaining + disappeared;
    if remaining == 0 {
        return Ok(Some(("folder_emptied", Some(before))));
    }
    let bulk = disappeared > MAX_DISAPPEARANCES_PER_MAILBOX
        || (before >= MIN_MAILBOX_FOR_SHARE && disappeared * 2 > before);
    Ok(bulk.then_some(("bulk_disappearance", Some(before))))
}

/// Apply one delete: the message's withheld occurrences become
/// `vanished`, and the record goes.
fn vanish(tx: &Transaction, message_id: i64) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE occurrences SET state = 'vanished'
         WHERE message_id = ?1 AND state = 'vanish_pending'",
        params![message_id],
    )
    .map_err(|e| classify(e, "apply a Proton delete"))?;
    tx.execute(
        "DELETE FROM proton_disappearances WHERE message_id = ?1",
        params![message_id],
    )
    .map_err(|e| classify(e, "forget an applied Proton delete"))?;
    Ok(())
}

pub(super) fn open_event(
    tx: &Transaction,
    account_id: &str,
    mailbox_id: Option<i64>,
    kind: &str,
    before: Option<usize>,
    messages: &[i64],
    now: Timestamp,
) -> Result<(), StoreError> {
    let write = |e| classify(e, "open a breaker event");
    let opened: i64 = tx
        .query_row(
            "SELECT COUNT(*) FROM breaker_events WHERE account_id = ?1",
            params![account_id],
            |row| row.get(0),
        )
        .map_err(write)?;
    let event_id = format!("proton-{account_id}-{}", opened + 1);
    let details = serde_json::json!({
        "disappeared": messages.len(),
        "mailbox_messages": before,
    });
    tx.execute(
        "INSERT INTO breaker_events (event_id, account_id, mailbox_id, kind, details_json, state, created_at)
         VALUES (?1, ?2, ?3, ?4, ?5, 'open', ?6)",
        params![
            event_id,
            account_id,
            mailbox_id,
            kind,
            details.to_string(),
            now.as_millis()
        ],
    )
    .map_err(write)?;
    for message_id in messages {
        tx.execute(
            "UPDATE proton_disappearances SET event_id = ?1 WHERE message_id = ?2",
            params![event_id, message_id],
        )
        .map_err(write)?;
    }
    Ok(())
}

/// One open event as the account health surface shows it (spec/07
/// `account_health`): its id, the mailbox it names, how many messages
/// disappeared and when it was detected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredBreakerEvent {
    pub event_id: String,
    pub mailbox_id: Option<i64>,
    pub disappeared: u64,
    pub detected_at_ms: i64,
}

/// The account's `open` breaker events, oldest first, with the counts
/// and detection times the health surface lists (spec/07). Only
/// `open` events are listed; one accepted or cleared is not an open
/// question any more.
pub fn open_breaker_events(
    tx: &Transaction,
    account_id: &str,
) -> Result<Vec<StoredBreakerEvent>, StoreError> {
    let mut statement = tx
        .prepare_cached(
            "SELECT event_id, mailbox_id, details_json, created_at
             FROM breaker_events
             WHERE account_id = ?1 AND state = 'open'
             ORDER BY created_at, event_id",
        )
        .map_err(|e| classify(e, "read the account's open breaker events"))?;
    let rows = statement
        .query_map(params![account_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<i64>>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
            ))
        })
        .map_err(|e| classify(e, "read the account's open breaker events"))?;
    let mut events = Vec::new();
    for row in rows {
        let (event_id, mailbox_id, details, created_at) =
            row.map_err(|e| classify(e, "read the account's open breaker events"))?;
        let disappeared = serde_json::from_str::<serde_json::Value>(&details)
            .ok()
            .and_then(|value| value.get("disappeared")?.as_u64())
            .ok_or_else(|| {
                StoreError::corrupt(format!("breaker event {event_id} has no disappeared count"))
            })?;
        events.push(StoredBreakerEvent {
            event_id,
            mailbox_id,
            disappeared,
            detected_at_ms: created_at,
        });
    }
    Ok(events)
}

/// The user's choice on a breaker event (04-sync-imap "While paused").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BreakerResolution {
    /// Apply the disappearances; the event becomes `accepted`. Allowed
    /// on an `open` or `kept` event.
    Accept,
    /// Leave the messages `vanish_pending` and keep watching; the event
    /// becomes `kept`. Allowed on an `open` event.
    Keep,
}

/// Resolve a Proton account's breaker event with the user's choice,
/// then classify the deletes its pause held back. An event in another
/// state is a `Conflict`; an unknown one `NotFound`.
pub fn resolve_breaker_event(
    tx: &Transaction,
    event_id: &str,
    choice: BreakerResolution,
    now: Timestamp,
) -> Result<Classified, StoreError> {
    let event: Option<(String, String)> = tx
        .query_row(
            "SELECT b.account_id, b.state FROM breaker_events b
             JOIN proton_accounts p ON p.account_id = b.account_id
             WHERE b.event_id = ?1",
            params![event_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|e| classify(e, "read a breaker event"))?;
    let Some((account_id, state)) = event else {
        return Err(StoreError::not_found(format!(
            "Proton breaker event {event_id}"
        )));
    };
    let allowed = match choice {
        BreakerResolution::Accept => state == "open" || state == "kept",
        BreakerResolution::Keep => state == "open",
    };
    if !allowed {
        return Err(StoreError::conflict(format!(
            "the breaker event is {state}"
        )));
    }

    let next = match choice {
        BreakerResolution::Accept => {
            let held: Vec<i64> = {
                let read = |e| classify(e, "read a breaker event's messages");
                let mut statement = tx
                    .prepare("SELECT message_id FROM proton_disappearances WHERE event_id = ?1")
                    .map_err(read)?;
                let rows = statement
                    .query_map(params![event_id], |row| row.get(0))
                    .map_err(read)?;
                rows.collect::<Result<_, _>>().map_err(read)?
            };
            for message_id in held {
                vanish(tx, message_id)?;
            }
            "accepted"
        }
        BreakerResolution::Keep => "kept",
    };
    tx.execute(
        "UPDATE breaker_events SET state = ?2, resolved_at = ?3 WHERE event_id = ?1",
        params![event_id, next, now.as_millis()],
    )
    .map_err(|e| classify(e, "resolve a breaker event"))?;
    classify_deletes(tx, &account_id, Pass::Resolved, now)
}

#[cfg(test)]
mod tests;
