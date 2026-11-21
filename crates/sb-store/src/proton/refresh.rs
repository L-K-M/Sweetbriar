//! What a complete Proton listing missed (spec/04-sync-proton
//! "Refresh" steps 3 and 4). Each listed page notes its messages in
//! `proton_listed`. Before the listing completes, the stored messages
//! it missed are withheld as `vanish_pending` in budget-sized writes
//! and recorded as unconfirmed disappearances. One listing isn't
//! enough evidence: a second complete listing that misses them too, or
//! a delete event, confirms them, and the breaker classifies them then.
//! A listing that misses more than the breaker's thresholds allow
//! opens one `epoch_change` event, which holds what it missed.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Transaction, params};
use sb_types::time::Timestamp;

use super::breaker::{self, Evidence, Pass};
use super::listing::invalid;
use super::{ProtonListingState, require_account, require_revision};
use crate::schema::classify;
use crate::sync::{MAX_HEADER_BATCH, MAX_HEADER_BATCH_BYTES};
use crate::{StoreError, list_rows};

/// Note a message the current listing listed.
pub(super) fn note_listed(
    tx: &Transaction,
    account_id: &str,
    message_id: i64,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT OR IGNORE INTO proton_listed (message_id, account_id) VALUES (?1, ?2)",
        params![message_id, account_id],
    )
    .map_err(|e| classify(e, "note a listed Proton message"))?;
    Ok(())
}

/// Forget what a listing listed: it starts again or completed.
pub(super) fn forget_listed(tx: &Transaction, account_id: &str) -> Result<(), StoreError> {
    tx.execute(
        "DELETE FROM proton_listed WHERE account_id = ?1",
        params![account_id],
    )
    .map_err(|e| classify(e, "forget a Proton listing's messages"))?;
    Ok(())
}

/// Withhold up to [`MAX_HEADER_BATCH`] stored messages the listing
/// missed, once its last page is committed (spec/04-sync-proton
/// "Refresh" step 3), and as many of them as the write budget allows:
/// a message whose list rows would overrun it waits for the next call.
/// Only while listing, under the revision the listing holds. Returns
/// how many it withheld: call again until none remain, then finish the
/// listing.
pub fn withhold_unlisted(
    tx: &Transaction,
    account_id: &str,
    expected_revision: i64,
) -> Result<usize, StoreError> {
    let account = require_account(tx, account_id)?;
    require_revision(&account, expected_revision)?;
    let (ProtonListingState::Listing, Some(cursor)) =
        (account.listing_state, account.pending_cursor)
    else {
        return Err(StoreError::conflict("the Proton account is not listing"));
    };

    let mut remaining = MAX_HEADER_BATCH_BYTES;
    let mut withheld = 0;
    let mut rows_changed = false;
    for message_id in unlisted(tx, account_id, MAX_HEADER_BATCH)? {
        // One message at a time under a savepoint, so the one that
        // overruns the budget is undone whole.
        savepoint(tx, "SAVEPOINT withhold_unlisted")?;
        let mut changed = BTreeSet::new();
        breaker::withhold(
            tx,
            account_id,
            message_id,
            &cursor,
            Evidence::Listing,
            &mut changed,
        )?;
        let mut spent = 0usize;
        let priced = list_rows::refresh_priced(tx, &changed, &mut |bytes| {
            spent = spent.saturating_add(bytes);
            if spent > remaining {
                return Err(invalid("withheld Proton list rows exceed the write budget"));
            }
            Ok(())
        });
        match priced {
            Ok(changed) => {
                savepoint(tx, "RELEASE withhold_unlisted")?;
                remaining -= spent;
                withheld += 1;
                rows_changed |= changed;
            }
            // The next call starts with this message and a whole budget.
            Err(_) if spent > remaining && withheld > 0 => {
                savepoint(tx, "ROLLBACK TO withhold_unlisted")?;
                savepoint(tx, "RELEASE withhold_unlisted")?;
                break;
            }
            Err(error) => return Err(error),
        }
    }
    if rows_changed {
        list_rows::advance_generation(tx)?;
    }
    Ok(withheld)
}

fn savepoint(tx: &Transaction, sql: &str) -> Result<(), StoreError> {
    tx.execute_batch(sql)
        .map_err(|e| classify(e, "withhold a Proton message under a savepoint"))
}

/// Up to `limit` of the account's messages with a present occurrence
/// that the current listing didn't list.
pub(super) fn unlisted(
    tx: &Transaction,
    account_id: &str,
    limit: usize,
) -> Result<Vec<i64>, StoreError> {
    let read = |e| classify(e, "read the messages a Proton listing missed");
    let limit = i64::try_from(limit).unwrap_or(i64::MAX);
    let mut statement = tx
        .prepare(
            "SELECT DISTINCT o.message_id FROM occurrences o
             JOIN logical_messages l ON l.message_id = o.message_id
             WHERE l.account_id = ?1 AND o.state = 'present'
               AND NOT EXISTS (SELECT 1 FROM proton_listed p WHERE p.message_id = o.message_id)
             ORDER BY o.message_id LIMIT ?2",
        )
        .map_err(read)?;
    let rows = statement
        .query_map(params![account_id, limit], |row| row.get(0))
        .map_err(read)?;
    rows.collect::<Result<_, _>>().map_err(read)
}

/// Steps 3 and 4 as the listing completes at `cursor`, its pending
/// cursor, once [`withhold_unlisted`] withheld everything it missed.
/// What an earlier listing missed too is confirmed and classified;
/// what only this one missed opens one `epoch_change` event when it
/// breaches a threshold, and otherwise waits.
pub(super) fn conclude(
    tx: &Transaction,
    account_id: &str,
    cursor: &str,
    now: Timestamp,
) -> Result<(), StoreError> {
    if !unlisted(tx, account_id, 1)?.is_empty() {
        return Err(StoreError::conflict(
            "the Proton listing missed messages that aren't withheld yet",
        ));
    }
    // An earlier listing's misses keep their cursor, so a resolution
    // classifies them even before the next events response.
    tx.execute(
        "UPDATE proton_disappearances SET confirmed = 1
         WHERE account_id = ?1 AND confirmed = 0 AND event_id IS NULL AND pass_cursor IS NOT ?2",
        params![account_id, cursor],
    )
    .map_err(|e| classify(e, "confirm what two Proton listings missed"))?;
    breaker::classify_deletes(tx, account_id, Pass::ResponseEnd, now)?;

    let read = |e| classify(e, "read what a Proton listing missed");
    let mut by_mailbox: BTreeMap<Option<i64>, (Option<String>, usize)> = BTreeMap::new();
    let mut missed = Vec::new();
    {
        let mut statement = tx
            .prepare(
                "SELECT d.message_id, d.mailbox_id, m.role FROM proton_disappearances d
                 LEFT JOIN mailboxes m ON m.mailbox_id = d.mailbox_id
                 WHERE d.account_id = ?1 AND d.confirmed = 0 AND d.event_id IS NULL
                   AND d.pass_cursor = ?2
                 ORDER BY d.message_id",
            )
            .map_err(read)?;
        let rows = statement
            .query_map(params![account_id, cursor], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(read)?;
        for row in rows {
            let (message_id, mailbox_id, role): (i64, Option<i64>, Option<String>) =
                row.map_err(read)?;
            missed.push(message_id);
            by_mailbox.entry(mailbox_id).or_insert((role, 0)).1 += 1;
        }
    }
    let mut breached = false;
    for (mailbox_id, (role, count)) in by_mailbox {
        // Proton can empty Trash and Spam by itself: retention.
        if matches!(role.as_deref(), Some("trash" | "junk")) {
            continue;
        }
        // A listing's new misses are unconfirmed, so none is
        // explained yet (spec/04-sync-proton "Refresh" step 3).
        if breaker::breach(tx, mailbox_id, count, 0)?.is_some() {
            breached = true;
            break;
        }
    }
    if breached {
        breaker::open_event(tx, account_id, None, "epoch_change", None, &missed, now)?;
    }
    forget_listed(tx, account_id)
}
