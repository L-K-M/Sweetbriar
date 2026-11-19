//! Sending (spec/02 "Sending", spec/05 "Outbox state machine",
//! spec/07 "Commands"): the `send_draft` queue insert, the
//! `cancel_send` transition, the `outcome_unknown` resolutions, the
//! route confirmation and the cross-account source read, each in one
//! transaction where it writes. The schema is migration 1's with
//! migration 34's Proton columns.

use std::collections::HashSet;

use rusqlite::{OptionalExtension, Transaction, params};
use sb_types::time::Timestamp;

use crate::StoreError;
use crate::accounts::IdentitySendable;
use crate::drafts::{DraftRecipient, DraftState};
use crate::identities::account_kind;
use crate::objects::{PublishedObject, ReservationRelease, release_reservation};
use crate::query::AddressOperand;
use crate::schema::classify;
use crate::sidebar::AccountKind;

/// The user's answer to an `outcome_unknown` attempt (spec/05 step 6).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum UnknownResolution {
    MarkedSent,
    SendAgain,
    KeptAsDraft,
}

impl UnknownResolution {
    /// The stored word (spec/02 `send_attempts.resolution`).
    pub fn as_str(self) -> &'static str {
        match self {
            Self::MarkedSent => "marked_sent",
            Self::SendAgain => "send_again",
            Self::KeptAsDraft => "kept_as_draft",
        }
    }

    /// Parse a stored resolution word.
    pub fn from_stored(name: &str) -> Result<Self, StoreError> {
        match name {
            "marked_sent" => Ok(Self::MarkedSent),
            "send_again" => Ok(Self::SendAgain),
            "kept_as_draft" => Ok(Self::KeptAsDraft),
            _ => Err(StoreError::corrupt(format!(
                "a send attempt's resolution is '{name}'"
            ))),
        }
    }
}

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

/// Move a draft's state only while the attempt's revision is still its
/// current one (spec/02, 05): a newer revision keeps its own state.
fn set_draft_state(
    tx: &Transaction,
    draft_id: &str,
    revision_id: &str,
    state: &str,
    now_ms: i64,
) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE drafts SET state = ?3, updated_at = ?4
         WHERE draft_id = ?1 AND current_revision_id = ?2",
        params![draft_id, revision_id, state, now_ms],
    )
    .map_err(|e| classify(e, "move a draft's state"))?;
    Ok(())
}

/// Count each recipient that becomes `sent` in `seen_addresses`
/// (spec/02): insert the canonical row with the attempt's spelling and
/// now when absent, otherwise add one and keep the stored observation.
fn add_sent_to_counts(
    tx: &Transaction,
    recipients: &[(String, String)],
    account_id: &str,
    now_ms: i64,
) -> Result<(), StoreError> {
    for (canonical, address) in recipients {
        tx.execute(
            "INSERT INTO seen_addresses
               (canonical, email, display_name, last_seen_at, times_sent_to,
                times_received, last_account_id)
             VALUES (?1, ?2, NULL, ?3, 1, 0, ?4)
             ON CONFLICT(canonical) DO UPDATE SET
               times_sent_to = times_sent_to + 1,
               last_account_id = excluded.last_account_id",
            params![canonical, address, now_ms, account_id],
        )
        .map_err(|e| classify(e, "count a sent-to address"))?;
    }
    Ok(())
}

/// `resolve_unknown_send(attempt_id, resolution)` (spec/05 step 6):
/// record the user's answer on an unresolved `outcome_unknown`
/// attempt, exactly once. `marked_sent` moves its `unknown`
/// recipients to `sent`, counts them and sets the draft `sent`;
/// `kept_as_draft` returns the draft to `editing`; `send_again`
/// re-queues the frozen bytes. A second resolution is `Conflict`.
pub fn resolve_unknown_send(
    tx: &Transaction,
    attempt_id: &str,
    resolution: UnknownResolution,
    new_attempt_id: Option<&str>,
    undo_seconds: i64,
    now: Timestamp,
) -> Result<Option<String>, StoreError> {
    let err = |e| classify(e, "resolve an unknown send");
    let row = tx
        .query_row(
            "SELECT draft_id, revision_id, account_id, identity_id, mime_sha256,
                    message_id_header, envelope_from, state, resolution
             FROM send_attempts WHERE attempt_id = ?1",
            params![attempt_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, Option<String>>(8)?,
                ))
            },
        )
        .optional()
        .map_err(err)?
        .ok_or_else(|| StoreError::not_found(format!("send attempt {attempt_id}")))?;
    let (
        draft_id,
        revision_id,
        account_id,
        identity_id,
        mime_sha256,
        message_id_header,
        envelope_from,
        state,
        stored_resolution,
    ) = row;
    if state != "outcome_unknown" {
        return Err(StoreError::conflict(format!(
            "a send attempt in state {state} has no unknown outcome to resolve"
        )));
    }
    if stored_resolution.is_some() {
        return Err(StoreError::conflict(format!(
            "send attempt {attempt_id} is already resolved"
        )));
    }
    let now_ms = now.as_millis();
    // The resolution commits first: the attempt becomes history even
    // when a newer revision keeps the draft's state.
    let resolved = tx
        .execute(
            "UPDATE send_attempts SET resolution = ?2, resolved_at = ?3, updated_at = ?3
             WHERE attempt_id = ?1 AND state = 'outcome_unknown' AND resolution IS NULL",
            params![attempt_id, resolution.as_str(), now_ms],
        )
        .map_err(err)?;
    if resolved == 0 {
        return Err(StoreError::conflict(format!(
            "send attempt {attempt_id} is no longer unresolved"
        )));
    }
    let created = match resolution {
        UnknownResolution::MarkedSent => {
            let unknown: Vec<(String, String)> = {
                let mut statement = tx
                    .prepare(
                        "SELECT canonical, address FROM send_recipients
                         WHERE attempt_id = ?1 AND state = 'unknown'",
                    )
                    .map_err(err)?;
                let rows = statement
                    .query_map(params![attempt_id], |row| Ok((row.get(0)?, row.get(1)?)))
                    .map_err(err)?;
                rows.collect::<Result<_, _>>().map_err(err)?
            };
            tx.execute(
                "UPDATE send_recipients SET state = 'sent'
                 WHERE attempt_id = ?1 AND state = 'unknown'",
                params![attempt_id],
            )
            .map_err(err)?;
            add_sent_to_counts(tx, &unknown, &account_id, now_ms)?;
            set_draft_state(tx, &draft_id, &revision_id, "sent", now_ms)?;
            None
        }
        UnknownResolution::KeptAsDraft => {
            set_draft_state(tx, &draft_id, &revision_id, "editing", now_ms)?;
            None
        }
        UnknownResolution::SendAgain => {
            let new_attempt_id = new_attempt_id.ok_or_else(|| {
                StoreError::internal("resolving as send again needs a new attempt id")
            })?;
            if undo_seconds <= 0 {
                return Err(StoreError::InvalidInput {
                    message: "the undo-send window must be positive".to_owned(),
                });
            }
            let (provider, config_generation): (String, i64) = tx
                .query_row(
                    "SELECT provider, config_generation FROM accounts WHERE account_id = ?1",
                    params![account_id],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(err)?;
            let sent_copy = if provider == "proton_api" {
                "server_saves"
            } else {
                "pending"
            };
            let hold_until = now_ms
                .checked_add(
                    undo_seconds
                        .checked_mul(1000)
                        .ok_or_else(|| StoreError::internal("the undo-send window overflows"))?,
                )
                .ok_or_else(|| StoreError::internal("the hold_until timestamp overflows"))?;
            tx.execute(
                "INSERT INTO send_attempts (attempt_id, draft_id, revision_id, account_id,
                    config_generation, identity_id, mime_sha256, message_id_header,
                    envelope_from, state, hold_until, sent_copy, created_at, updated_at)
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'held', ?10, ?11, ?12, ?12)",
                params![
                    new_attempt_id,
                    draft_id,
                    revision_id,
                    account_id,
                    config_generation,
                    identity_id,
                    mime_sha256,
                    message_id_header,
                    envelope_from,
                    hold_until,
                    sent_copy,
                    now_ms
                ],
            )
            .map_err(err)?;
            // The frozen recipients start over as pending; the old
            // attempt's rows stay as its history.
            tx.execute(
                "INSERT INTO send_recipients (attempt_id, canonical, address, kind, state)
                 SELECT ?1, canonical, address, kind, 'pending'
                 FROM send_recipients WHERE attempt_id = ?2",
                params![new_attempt_id, attempt_id],
            )
            .map_err(err)?;
            set_draft_state(tx, &draft_id, &revision_id, "queued", now_ms)?;
            Some(new_attempt_id.to_owned())
        }
    };
    Ok(created)
}

/// `confirm_send_route(attempt_id)` (spec/05 step 3): a `queued`
/// attempt adopts the account's current generation; an unsendable
/// identity or any other state is `Conflict`.
pub fn confirm_send_route(
    tx: &Transaction,
    attempt_id: &str,
    now: Timestamp,
) -> Result<(), StoreError> {
    let err = |e| classify(e, "confirm a send route");
    let (account_id, identity_id, state): (String, String, String) = tx
        .query_row(
            "SELECT account_id, identity_id, state FROM send_attempts WHERE attempt_id = ?1",
            params![attempt_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(err)?
        .ok_or_else(|| StoreError::not_found(format!("send attempt {attempt_id}")))?;
    if state != "queued" {
        return Err(StoreError::conflict(format!(
            "a send attempt in state {state} waits for no route confirmation"
        )));
    }
    let (sendable, kind): (String, String) = tx
        .query_row(
            "SELECT i.sendable, a.kind FROM identities i
             JOIN accounts a ON a.account_id = i.account_id
             WHERE i.identity_id = ?1 AND i.account_id = ?2",
            params![identity_id, account_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(err)?
        .ok_or_else(|| StoreError::not_found(format!("identity {identity_id}")))?;
    if account_kind(&kind)? == AccountKind::Local
        || !IdentitySendable::from_stored(&sendable)?.can_send()
    {
        return Err(StoreError::conflict(format!(
            "identity {identity_id} can no longer send"
        )));
    }
    let config_generation: i64 = tx
        .query_row(
            "SELECT config_generation FROM accounts WHERE account_id = ?1",
            params![account_id],
            |row| row.get(0),
        )
        .map_err(err)?;
    let confirmed = tx
        .execute(
            "UPDATE send_attempts SET config_generation = ?2, updated_at = ?3
             WHERE attempt_id = ?1 AND state = 'queued'",
            params![attempt_id, config_generation, now.as_millis()],
        )
        .map_err(err)?;
    if confirmed == 0 {
        return Err(StoreError::conflict(format!(
            "send attempt {attempt_id} is no longer queued"
        )));
    }
    Ok(())
}

/// `retry_rejected_recipients(attempt_id)` (spec/05 step 5): Proton
/// sends a whole message, so there is no partial acceptance to retry.
/// An unknown attempt is `NotFound`.
pub fn retry_rejected_recipients(tx: &Transaction, attempt_id: &str) -> Result<(), StoreError> {
    let err = |e| classify(e, "retry rejected recipients");
    let provider: String = tx
        .query_row(
            "SELECT a.provider FROM send_attempts s
             JOIN accounts a ON a.account_id = s.account_id
             WHERE s.attempt_id = ?1",
            params![attempt_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(err)?
        .ok_or_else(|| StoreError::not_found(format!("send attempt {attempt_id}")))?;
    if provider == "proton_api" {
        return Err(StoreError::conflict(
            "a Proton attempt has no partially accepted recipients to retry",
        ));
    }
    Err(StoreError::conflict(
        "retrying rejected recipients is not supported for this account yet",
    ))
}

/// The account and kind of a reply's receiving occurrence (spec/05
/// "Cross-account check"); a missing row is `NotFound`.
pub fn reply_source_account(
    tx: &Transaction,
    occurrence_id: i64,
) -> Result<(String, AccountKind), StoreError> {
    let err = |e| classify(e, "read a reply's source account");
    let (account_id, kind): (String, String) = tx
        .query_row(
            "SELECT o.account_id, a.kind FROM occurrences o
             JOIN accounts a ON a.account_id = o.account_id
             WHERE o.occurrence_id = ?1",
            params![occurrence_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(err)?
        .ok_or_else(|| StoreError::not_found(format!("occurrence {occurrence_id}")))?;
    Ok((account_id, account_kind(&kind)?))
}

#[cfg(test)]
mod tests;
