//! Sending (spec/02 "Sending", spec/05 "Outbox state machine"):
//! the `send_draft` queue insert and the `cancel_send` transition,
//! each in one transaction, plus the reply headers a compose reads.
//! The schema is migration 1's with migration 34's Proton columns.

use std::collections::HashSet;

use rusqlite::{OptionalExtension, Transaction, params};
use sb_types::time::Timestamp;

use crate::StoreError;
use crate::drafts::{DraftRecipient, DraftState};
use crate::objects::{PublishedObject, ReservationRelease, release_reservation};
use crate::query::AddressOperand;
use crate::schema::classify;

/// One `send_draft`'s frozen attempt: the route and envelope the
/// engine composed, and the recipients exactly as the revision stored
/// them. The store canonicalizes and deduplicates the recipients
/// (spec/02, 05).
#[derive(Clone)]
pub struct SendAttempt {
    pub attempt_id: String,
    pub draft_id: String,
    pub revision_id: String,
    pub account_id: String,
    pub identity_id: String,
    pub mime_sha256: String,
    pub message_id_header: String,
    pub envelope_from: String,
    pub to: Vec<DraftRecipient>,
    pub cc: Vec<DraftRecipient>,
    pub bcc: Vec<DraftRecipient>,
}

/// A reply's source occurrence message, or a forward's logical
/// message, with its `In-Reply-To` and `References` (spec/05). A
/// message fetched before any body still carries its headers.
pub fn reply_headers(
    tx: &Transaction,
    reply_to_occurrence_id: Option<i64>,
    forward_of_message_id: Option<i64>,
) -> Result<(Option<String>, Vec<String>), StoreError> {
    let err = |e| classify(e, "read a draft's source headers");
    let source_message = match reply_to_occurrence_id {
        Some(occurrence_id) => tx
            .query_row(
                "SELECT message_id FROM occurrences WHERE occurrence_id = ?1",
                params![occurrence_id],
                |row| row.get::<_, i64>(0),
            )
            .optional()
            .map_err(err)?,
        None => forward_of_message_id,
    };
    let Some(message_id) = source_message else {
        return Ok((None, Vec::new()));
    };
    let row = tx
        .query_row(
            "SELECT in_reply_to, references_json FROM message_headers WHERE message_id = ?1",
            params![message_id],
            |row| {
                Ok((
                    row.get::<_, Option<String>>(0)?,
                    row.get::<_, Option<String>>(1)?,
                ))
            },
        )
        .optional()
        .map_err(err)?;
    match row {
        Some((in_reply_to, references)) => {
            let references = match references {
                Some(json) => serde_json::from_str::<Vec<String>>(&json).map_err(|_| {
                    StoreError::corrupt("a message's references are not a JSON array of ids")
                })?,
                None => Vec::new(),
            };
            Ok((in_reply_to, references))
        }
        None => Ok((None, Vec::new())),
    }
}

/// `send_draft`'s one transaction (spec/05): re-check that the revision
/// is the draft's current one and the draft is `editing`, that the
/// revision has no live attempt and none `accepted` or resolved
/// `marked_sent`, then insert the `held` attempt with its `pending`
/// recipients, set the draft `queued`, and spend the publication's
/// reservation. The `send_attempts_live` unique index backs the
/// live-attempt check up. A failure rolls the whole transaction back,
/// so a refused send leaves no attempt and no draft state.
pub fn send_draft(
    tx: &Transaction,
    attempt: &SendAttempt,
    object: &PublishedObject,
    undo_seconds: i64,
    now: Timestamp,
) -> Result<(), StoreError> {
    if undo_seconds <= 0 {
        return Err(StoreError::InvalidInput {
            message: "the undo-send window must be positive".to_owned(),
        });
    }
    let err = |e| classify(e, "queue a send attempt");
    let (state, current): (String, Option<String>) = tx
        .query_row(
            "SELECT state, current_revision_id FROM drafts WHERE draft_id = ?1",
            params![attempt.draft_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(err)?
        .ok_or_else(|| StoreError::not_found(format!("draft {}", attempt.draft_id)))?;
    if current.as_deref() != Some(attempt.revision_id.as_str()) {
        return Err(StoreError::conflict(format!(
            "draft {}'s current revision is not {}",
            attempt.draft_id, attempt.revision_id
        )));
    }
    if DraftState::from_stored(&state)? != DraftState::Editing {
        return Err(StoreError::conflict(format!(
            "draft {} is {state}, not editing",
            attempt.draft_id
        )));
    }
    let live: Option<String> = tx
        .query_row(
            "SELECT attempt_id FROM send_attempts WHERE revision_id = ?1 AND (
                state IN ('held','queued','submitting')
                OR (state = 'outcome_unknown' AND resolution IS NULL)) LIMIT 1",
            params![attempt.revision_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(err)?;
    if live.is_some() {
        return Err(StoreError::conflict(format!(
            "revision {} already has a live send attempt",
            attempt.revision_id
        )));
    }
    let sent: Option<String> = tx
        .query_row(
            "SELECT attempt_id FROM send_attempts WHERE revision_id = ?1 AND (
                state = 'accepted'
                OR (state = 'outcome_unknown' AND resolution = 'marked_sent')) LIMIT 1",
            params![attempt.revision_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(err)?;
    if sent.is_some() {
        return Err(StoreError::conflict(format!(
            "revision {} was already sent",
            attempt.revision_id
        )));
    }
    let route: Option<(Option<String>, Option<String>)> = tx
        .query_row(
            "SELECT account_id, identity_id FROM draft_revisions
             WHERE revision_id = ?1 AND draft_id = ?2",
            params![attempt.revision_id, attempt.draft_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(err)?;
    let (revision_account, revision_identity) =
        route.ok_or_else(|| StoreError::not_found(format!("revision {}", attempt.revision_id)))?;
    if revision_account.as_deref() != Some(attempt.account_id.as_str())
        || revision_identity.as_deref() != Some(attempt.identity_id.as_str())
    {
        return Err(StoreError::conflict(
            "the attempt's route does not match its revision's snapshot",
        ));
    }
    let (provider, config_generation): (String, i64) = tx
        .query_row(
            "SELECT provider, config_generation FROM accounts WHERE account_id = ?1",
            params![attempt.account_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(err)?
        .ok_or_else(|| StoreError::not_found(format!("account {}", attempt.account_id)))?;
    let sent_copy = if provider == "proton_api" {
        "server_saves"
    } else {
        "pending"
    };
    let hold_until = now
        .as_millis()
        .checked_add(
            undo_seconds
                .checked_mul(1000)
                .ok_or_else(|| StoreError::internal("the undo-send window overflows"))?,
        )
        .ok_or_else(|| StoreError::internal("the hold_until timestamp overflows"))?;
    let now_ms = now.as_millis();
    tx.execute(
        "INSERT INTO send_attempts (attempt_id, draft_id, revision_id, account_id,
            config_generation, identity_id, mime_sha256, message_id_header, envelope_from,
            state, hold_until, sent_copy, created_at, updated_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'held', ?10, ?11, ?12, ?12)",
        params![
            attempt.attempt_id,
            attempt.draft_id,
            attempt.revision_id,
            attempt.account_id,
            config_generation,
            attempt.identity_id,
            attempt.mime_sha256,
            attempt.message_id_header,
            attempt.envelope_from,
            hold_until,
            sent_copy,
            now_ms
        ],
    )
    .map_err(err)?;
    insert_recipients(
        tx,
        &attempt.attempt_id,
        &attempt.to,
        &attempt.cc,
        &attempt.bcc,
    )?;
    tx.execute(
        "UPDATE drafts SET state = 'queued', updated_at = ?2 WHERE draft_id = ?1",
        params![attempt.draft_id, now_ms],
    )
    .map_err(|e| classify(e, "queue a draft"))?;
    // The frozen bytes are now referenced by the attempt; the
    // publication's one-shot reservation is spent here (spec/03).
    release_reservation(tx, object, ReservationRelease::Required)?;
    Ok(())
}

/// One row per canonical address (spec/02, 05): To, then Cc, then Bcc,
/// so the first spelling and the most visible kind win. `a@X.NET` in
/// To and `a@x.net` in Cc are one recipient.
fn insert_recipients(
    tx: &Transaction,
    attempt_id: &str,
    to: &[DraftRecipient],
    cc: &[DraftRecipient],
    bcc: &[DraftRecipient],
) -> Result<(), StoreError> {
    let mut seen: HashSet<String> = HashSet::new();
    for (kind, recipients) in [("to", to), ("cc", cc), ("bcc", bcc)] {
        for recipient in recipients {
            let address = AddressOperand::parse_received(&recipient.address).map_err(|_| {
                StoreError::InvalidInput {
                    message: format!("recipient '{}' is not a valid address", recipient.address),
                }
            })?;
            if !seen.insert(address.key().to_owned()) {
                continue;
            }
            tx.execute(
                "INSERT INTO send_recipients (attempt_id, canonical, address, kind, state)
                 VALUES (?1, ?2, ?3, ?4, 'pending')",
                params![attempt_id, address.key(), recipient.address, kind],
            )
            .map_err(|e| classify(e, "record a send recipient"))?;
        }
    }
    Ok(())
}

/// `cancel_send` (spec/05): a `held` or `queued` attempt becomes
/// `cancelled`, and its draft returns to `editing` only while the
/// attempt's revision is still the draft's current one. Any other state
/// is `Conflict`; an unknown attempt is `NotFound`. The state guard on
/// the update lets a racing transition win.
pub fn cancel_send(tx: &Transaction, attempt_id: &str, now: Timestamp) -> Result<(), StoreError> {
    let err = |e| classify(e, "cancel a send attempt");
    let (draft_id, revision_id, state): (String, String, String) = tx
        .query_row(
            "SELECT draft_id, revision_id, state FROM send_attempts WHERE attempt_id = ?1",
            params![attempt_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(err)?
        .ok_or_else(|| StoreError::not_found(format!("send attempt {attempt_id}")))?;
    if state != "held" && state != "queued" {
        return Err(StoreError::conflict(format!(
            "a send attempt in state {state} can't be cancelled"
        )));
    }
    let cancelled = tx
        .execute(
            "UPDATE send_attempts SET state = 'cancelled', updated_at = ?2
             WHERE attempt_id = ?1 AND state IN ('held','queued')",
            params![attempt_id, now.as_millis()],
        )
        .map_err(err)?;
    if cancelled == 0 {
        return Err(StoreError::conflict(format!(
            "send attempt {attempt_id} is no longer held or queued"
        )));
    }
    tx.execute(
        "UPDATE drafts SET state = 'editing', updated_at = ?2
         WHERE draft_id = ?1 AND current_revision_id = ?3",
        params![draft_id, now.as_millis(), revision_id],
    )
    .map_err(|e| classify(e, "return a draft to editing"))?;
    Ok(())
}

#[cfg(test)]
mod tests;
