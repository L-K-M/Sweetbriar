//! Proton event application (spec/04-sync-proton "Sync loop" step 2):
//! one events response, or one budget-sized chunk of it, applied in the
//! caller's transaction, the event cursor moving with its last chunk.
//!
//! Every message change carries the message's full metadata, so a
//! change replaces what the store holds exactly as a listed page does,
//! and replaying a chunk after a crash changes nothing more. A delete
//! withholds the message's current occurrences as `vanish_pending`, and
//! the breaker classifies the response's deletes with its last chunk.

use std::collections::BTreeSet;

use rusqlite::Transaction;
use sb_types::time::Timestamp;

use super::breaker::{self, Classified};
use super::listing::{
    ListedMessage, ListedPageStats, ObservedLabel, invalid, live_mailboxes,
    reconcile_proton_labels, store_message, store_reread_message,
};
use super::{ProtonListingState, require_account, require_revision, set_event_cursor};
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
    /// The breaker's classification, on a response's last chunk.
    pub breaker: Classified,
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
            ProtonChange::Upsert(message) => {
                store_message(
                    tx,
                    chunk.account_id,
                    message,
                    &mailboxes,
                    now,
                    &mut applied.messages,
                    &mut changed,
                )?;
            }
            ProtonChange::Delete(id) => {
                applied.withheld += breaker::record_delete(
                    tx,
                    chunk.account_id,
                    chunk.from_cursor,
                    id,
                    &mut changed,
                )?;
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
            // The response is whole: its deletes are one breaker pass.
            applied.breaker =
                breaker::classify_deletes(tx, chunk.account_id, breaker::Pass::ResponseEnd, now)?;
            set_event_cursor(tx, chunk.account_id, cursor, chunk.expected_revision)?.revision
        }
        None => account.revision,
    };
    Ok(applied)
}

/// One message's re-read metadata, as the refused body's re-read
/// returns it (plan M2.16d).
pub struct RereadMessage<'a> {
    pub account_id: &'a str,
    pub message: &'a ListedMessage,
    /// The account row's revision the metadata was read under.
    pub expected_revision: i64,
}

/// Apply one message's re-read metadata (spec/04 "Sync loop" step
/// 3): its occurrences follow its labels, as an event's change would,
/// and its headers and list rows refresh. The event cursor doesn't
/// move, the breaker doesn't count it, and `proton_disappearances` is
/// left alone; only a delete event or a complete listing removes a
/// message. Refused unless the account's listing is complete and
/// still at `expected_revision`; the write pays the same budget as a
/// listed page.
pub fn apply_reread_message(
    tx: &Transaction,
    reread: &RereadMessage<'_>,
    now: Timestamp,
) -> Result<ListedPageStats, StoreError> {
    let account = require_account(tx, reread.account_id)?;
    require_revision(&account, reread.expected_revision)?;
    if account.listing_state != ProtonListingState::Complete {
        return Err(StoreError::conflict(
            "the Proton account is not done listing",
        ));
    }
    let mailboxes = live_mailboxes(tx, reread.account_id)?;
    let mut remaining = MAX_HEADER_BATCH_BYTES;
    remaining = remaining
        .checked_sub(reread.message.raw_metadata.len() + 1024)
        .ok_or_else(|| invalid("a re-read message exceeds the write budget"))?;
    let mut stats = ListedPageStats::default();
    let mut changed = BTreeSet::new();
    store_reread_message(
        tx,
        reread.account_id,
        reread.message,
        &mailboxes,
        now,
        &mut stats,
        &mut changed,
    )?;
    if list_rows::refresh_priced(tx, &changed, &mut |bytes| {
        remaining = remaining
            .checked_sub(bytes)
            .ok_or_else(|| invalid("a re-read message's list rows exceed the write budget"))?;
        Ok(())
    })? {
        list_rows::advance_generation(tx)?;
    }
    if stats.new_messages > 0 {
        crate::threads::note_new_messages(tx)?;
        crate::seen::note_inputs(tx)?;
    }
    Ok(stats)
}

#[cfg(test)]
mod tests;
