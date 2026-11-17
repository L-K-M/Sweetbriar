//! Proton event application (spec/04-sync-proton "Sync loop" step 2):
//! one events response, or one budget-sized chunk of it, applied in the
//! caller's transaction, the event cursor moving with its last chunk.
//!
//! Every message change carries the message's full metadata, so a
//! change replaces what the store holds exactly as a listed page does,
//! and replaying a chunk after a crash changes nothing more. A delete
//! withholds the message's current occurrences as `vanish_pending`
//! until the breaker classifies it (plan M2.15c2); nothing is lost.

use std::collections::BTreeSet;

use rusqlite::{OptionalExtension, Transaction, params};
use sb_types::time::Timestamp;

use super::listing::{
    ListedMessage, ListedPageStats, ObservedLabel, current_occurrences, invalid, live_mailboxes,
    reconcile_proton_labels, store_message,
};
use super::{ProtonListingState, require_account, require_revision, set_event_cursor};
use crate::schema::classify;
use crate::sync::{MAX_HEADER_BATCH, MAX_HEADER_BATCH_BYTES};
use crate::{StoreError, list_rows};

/// The write budget a delete is charged: its occurrence updates and
/// list rows, which the list-row pricing below counts again.
const DELETE_COST: usize = 256;

/// One message change from an events response, in the response's
/// order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtonChange {
    /// Create, update or flag update: the message's full metadata.
    Upsert(Box<ListedMessage>),
    /// A delete, by Proton message id.
    Delete(String),
}

/// One chunk of an events response.
#[derive(Debug, Clone, Copy)]
pub struct EventChunk<'a> {
    pub account_id: &'a str,
    /// The event cursor the response was read from; the chunk is
    /// refused unless the account still stands there.
    pub from_cursor: &'a str,
    /// The account's labels, re-listed when the response changed one,
    /// reconciled before the messages.
    pub labels: Option<&'a [ObservedLabel]>,
    pub changes: &'a [ProtonChange],
    /// The response's new cursor, on its last chunk only.
    pub next_cursor: Option<&'a str>,
    /// The account row's revision the response was read under.
    pub expected_revision: i64,
}

/// What one chunk changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct AppliedEvents {
    pub messages: ListedPageStats,
    /// Occurrences a delete withheld as `vanish_pending`.
    pub withheld: usize,
    /// The account row's revision after the chunk.
    pub revision: i64,
}

/// Apply one chunk of an events response (spec/04-sync-proton "Sync
/// loop" step 2). Only after a complete listing, at the cursor the
/// response was read from, and under the revision it was read with.
pub fn apply_event_chunk(
    tx: &Transaction,
    chunk: &EventChunk<'_>,
    now: Timestamp,
) -> Result<AppliedEvents, StoreError> {
    if chunk.changes.len() > MAX_HEADER_BATCH {
        return Err(invalid(&format!(
            "an events chunk holds at most {MAX_HEADER_BATCH} changes"
        )));
    }
    let account = require_account(tx, chunk.account_id)?;
    require_revision(&account, chunk.expected_revision)?;
    if account.listing_state != ProtonListingState::Complete
        || account.event_cursor.as_deref() != Some(chunk.from_cursor)
    {
        return Err(StoreError::conflict(
            "the Proton account is not at this event cursor",
        ));
    }
    if let Some(labels) = chunk.labels {
        reconcile_proton_labels(tx, chunk.account_id, labels)?;
    }
    let mailboxes = live_mailboxes(tx, chunk.account_id)?;

    let mut remaining = MAX_HEADER_BATCH_BYTES;
    let mut applied = AppliedEvents::default();
    let mut changed = BTreeSet::new();
    for change in chunk.changes {
        let cost = match change {
            ProtonChange::Upsert(message) => message.raw_metadata.len() + 1024,
            ProtonChange::Delete(_) => DELETE_COST,
        };
        remaining = remaining
            .checked_sub(cost)
            .ok_or_else(|| invalid("an events chunk exceeds the write budget"))?;
        match change {
            ProtonChange::Upsert(message) => store_message(
                tx,
                chunk.account_id,
                message,
                &mailboxes,
                now,
                &mut applied.messages,
                &mut changed,
            )?,
            ProtonChange::Delete(id) => {
                applied.withheld += withhold(tx, chunk.account_id, id, &mut changed)?;
            }
        }
    }

    if list_rows::refresh_priced(tx, &changed, &mut |bytes| {
        remaining = remaining
            .checked_sub(bytes)
            .ok_or_else(|| invalid("an events chunk's list rows exceed the write budget"))?;
        Ok(())
    })? {
        list_rows::advance_generation(tx)?;
    }
    if applied.messages.new_messages > 0 {
        crate::threads::note_new_messages(tx)?;
        crate::seen::note_inputs(tx)?;
    }
    applied.revision = match chunk.next_cursor {
        Some(cursor) => {
            set_event_cursor(tx, chunk.account_id, cursor, chunk.expected_revision)?.revision
        }
        None => account.revision,
    };
    Ok(applied)
}

/// Withhold a deleted message's current occurrences; an unknown id
/// changes nothing.
fn withhold(
    tx: &Transaction,
    account_id: &str,
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
    let mut withheld = 0;
    for (occurrence_id, _) in current_occurrences(tx, message_id)? {
        let updated = tx
            .execute(
                "UPDATE occurrences SET state = 'vanish_pending'
                 WHERE occurrence_id = ?1 AND state = 'present'",
                params![occurrence_id],
            )
            .map_err(|e| classify(e, "withhold a deleted Proton occurrence"))?;
        if updated > 0 {
            changed.insert(occurrence_id);
            withheld += 1;
        }
    }
    Ok(withheld)
}

#[cfg(test)]
mod tests;
