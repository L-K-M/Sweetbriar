//! Sending (spec/02 "Sending", spec/05 "Outbox state machine",
//! spec/07 "Commands"): the `send_draft` queue insert, the
//! `cancel_send` transition, the `outcome_unknown` resolutions, the
//! route confirmation and the cross-account source read, each in one
//! transaction where it writes. The schema is migration 1's with
//! migration 34's Proton columns.

use std::collections::{HashMap, HashSet};

use rusqlite::{OptionalExtension, Transaction, params, params_from_iter};
use sb_types::time::Timestamp;

use crate::StoreError;
use crate::accounts::IdentitySendable;
use crate::drafts::{AttemptState, DraftRecipient, DraftState};
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

/// One attachment the attempt has already uploaded to Proton's draft,
/// in the frozen MIME's order (spec/02, 05 step 3). The stored JSON
/// keeps the four fields spec/02 names, so the array position is the
/// attachment index.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtonUpload {
    pub index: usize,
    /// Proton's attachment id, which the send packages key on.
    pub id: String,
    pub name: String,
    pub mime_type: String,
    pub size: u64,
}

/// The `proton_uploads_json` shape (spec/02).
#[derive(serde::Serialize, serde::Deserialize)]
struct StoredUploads {
    v: u32,
    attachments: Vec<StoredUpload>,
}

#[derive(serde::Serialize, serde::Deserialize)]
struct StoredUpload {
    id: String,
    name: String,
    mime_type: String,
    size: u64,
}

/// Parse `proton_uploads_json`, numbering each attachment by its array
/// position; a missing value is none.
fn parse_uploads(json: Option<&str>) -> Result<Vec<ProtonUpload>, StoreError> {
    let Some(json) = json else {
        return Ok(Vec::new());
    };
    let stored: StoredUploads = serde_json::from_str(json).map_err(|_| {
        StoreError::corrupt("a send attempt's Proton uploads are not the stored shape")
    })?;
    if stored.v != 1 {
        return Err(StoreError::corrupt(format!(
            "a send attempt's Proton uploads have version {}",
            stored.v
        )));
    }
    Ok(stored
        .attachments
        .into_iter()
        .enumerate()
        .map(|(index, upload)| ProtonUpload {
            index,
            id: upload.id,
            name: upload.name,
            mime_type: upload.mime_type,
            size: upload.size,
        })
        .collect())
}

/// Whether a claimed attempt answers another message or forwards it
/// (spec/05 step 4's `Action`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SendParentKind {
    Reply,
    Forward,
}

impl SendParentKind {
    fn from_stored(name: &str) -> Result<Self, StoreError> {
        match name {
            "reply" => Ok(Self::Reply),
            "forward" => Ok(Self::Forward),
            _ => Err(StoreError::corrupt(format!(
                "a send attempt's parent kind is '{name}'"
            ))),
        }
    }
}

/// The message a claimed attempt replies to or forwards, when it is in
/// the same account and Proton knows its id (spec/05 step 4's
/// `ParentID`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendParent {
    pub provider_message_id: String,
    pub kind: SendParentKind,
}

/// One recipient of a claimed attempt: the address as first written,
/// its canonical form and its field (spec/02).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedRecipient {
    pub address: String,
    pub canonical: String,
    pub kind: RecipientKind,
}

/// The attempt the account worker claimed, committed `submitting`
/// before any network I/O (spec/05 step 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClaimedSend {
    pub attempt_id: String,
    pub draft_id: String,
    pub revision_id: String,
    pub account_id: String,
    pub identity_id: String,
    pub envelope_from: String,
    pub message_id_header: String,
    pub mime_sha256: String,
    pub recipients: Vec<ClaimedRecipient>,
    pub proton_draft_id: Option<String>,
    pub uploads: Vec<ProtonUpload>,
    pub parent: Option<SendParent>,
}

/// A definite pre-acceptance failure (spec/05 step 4): Proton's code
/// and message, or a local reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendFailure {
    pub code: Option<i64>,
    pub message: String,
    pub rejected: Option<SendFailureRejection>,
}

/// The recipient a failure refuses permanently and its stored reason.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendFailureRejection {
    pub canonical: String,
    pub reason: String,
}

/// Proton's code and message as `final_reply` (spec/02).
fn final_reply(code: Option<i64>, message: &str) -> String {
    match code {
        Some(code) => format!("{code} {message}"),
        None => message.to_owned(),
    }
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

/// `queue_due_held(now)` (spec/05 step 2): every `held` attempt whose
/// undo window has passed becomes `queued`, all accounts, in one
/// statement. The `held` guard lets a cancel that committed first win.
/// Returns how many attempts moved.
pub fn queue_due_held(tx: &Transaction, now: Timestamp) -> Result<usize, StoreError> {
    let moved = tx
        .execute(
            "UPDATE send_attempts SET state = 'queued', updated_at = ?2
             WHERE state = 'held' AND hold_until IS NOT NULL AND hold_until <= ?1",
            params![now.as_millis(), now.as_millis()],
        )
        .map_err(|e| classify(e, "queue due held send attempts"))?;
    Ok(moved)
}

/// The earliest `hold_until` among the account's `held` attempts, or
/// `None` (spec/05 step 2). The account worker's poll deadline: a
/// window that ends between two polls is observed without waiting the
/// whole interval.
pub fn earliest_hold_until(
    tx: &Transaction,
    account_id: &str,
) -> Result<Option<Timestamp>, StoreError> {
    let hold_until: Option<i64> = tx
        .query_row(
            "SELECT MIN(hold_until) FROM send_attempts
             WHERE account_id = ?1 AND state = 'held' AND hold_until IS NOT NULL",
            params![account_id],
            |row| row.get(0),
        )
        .map_err(|e| classify(e, "read the earliest held send window"))?;
    Ok(hold_until.map(Timestamp::from_millis))
}

/// `recover_proton_submitting(now)` (spec/05 step 3): a Proton
/// attempt a shutdown or crash left in `submitting`, recovered before
/// any claim. One with `send_requested_at` committed became
/// `outcome_unknown` (its recipients `unknown`, its draft `queued`
/// under the current-revision rule) and is never retried; one without
/// it never issued the send request, so it returns to `queued` and
/// keeps its recorded draft and uploads. A generic attempt is left
/// alone until SMTP records its own marker. Returns how many attempts
/// became unknown.
pub fn recover_proton_submitting(tx: &Transaction, now: Timestamp) -> Result<usize, StoreError> {
    let err = |e| classify(e, "recover a Proton submission");
    let now_ms = now.as_millis();
    // The unknowned attempts' draft and revision, so the draft's state
    // can follow the current-revision rule (spec/02, 05).
    let mut statement = tx
        .prepare(
            "SELECT s.attempt_id, s.draft_id, s.revision_id
             FROM send_attempts s
             JOIN accounts a ON a.account_id = s.account_id
             WHERE s.state = 'submitting' AND s.send_requested_at IS NOT NULL
               AND a.provider = 'proton_api'",
        )
        .map_err(err)?;
    let unknown: Vec<(String, String, String)> = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .map_err(err)?
        .collect::<Result<_, _>>()
        .map_err(err)?;
    drop(statement);
    for (attempt_id, draft_id, revision_id) in &unknown {
        let applied = tx
            .execute(
                "UPDATE send_attempts SET state = 'outcome_unknown', updated_at = ?2
                 WHERE attempt_id = ?1 AND state = 'submitting'",
                params![attempt_id, now_ms],
            )
            .map_err(err)?;
        if applied == 0 {
            continue;
        }
        tx.execute(
            "UPDATE send_recipients SET state = 'unknown' WHERE attempt_id = ?1",
            params![attempt_id],
        )
        .map_err(err)?;
        set_draft_state(tx, draft_id, revision_id, "queued", now_ms)?;
    }
    tx.execute(
        "UPDATE send_attempts SET state = 'queued', updated_at = ?2
         WHERE state = 'submitting' AND send_requested_at IS NULL
           AND account_id IN (SELECT account_id FROM accounts WHERE provider = 'proton_api')",
        params![now_ms, now_ms],
    )
    .map_err(err)?;
    Ok(unknown.len())
}

/// One unresolved Proton attempt's evidence target (spec/05 step 6):
/// the attempt and the Proton draft the worker reads.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnknownSend {
    pub attempt_id: String,
    pub proton_draft_id: String,
}

/// `unknown_proton_attempts(account_id)` (spec/05 step 6): every
/// unresolved `outcome_unknown` Proton attempt of the account with a
/// recorded `proton_draft_id`, oldest first, so the worker can read
/// each draft and observe what Proton holds.
pub fn unknown_proton_attempts(
    tx: &Transaction,
    account_id: &str,
) -> Result<Vec<UnknownSend>, StoreError> {
    let err = |e| classify(e, "read the unresolved Proton attempts");
    let mut statement = tx
        .prepare(
            "SELECT s.attempt_id, s.proton_draft_id
             FROM send_attempts s
             JOIN accounts a ON a.account_id = s.account_id
             WHERE s.account_id = ?1 AND a.provider = 'proton_api'
               AND s.state = 'outcome_unknown' AND s.resolution IS NULL
               AND s.proton_draft_id IS NOT NULL
             ORDER BY s.updated_at, s.attempt_id",
        )
        .map_err(err)?;
    let rows = statement
        .query_map(params![account_id], |row| {
            Ok(UnknownSend {
                attempt_id: row.get(0)?,
                proton_draft_id: row.get(1)?,
            })
        })
        .map_err(err)?;
    rows.collect::<Result<_, _>>().map_err(err)
}

/// `claim_send_attempt(account_id, now)` (spec/05 step 3): the oldest
/// `queued` attempt of the account whose `config_generation` is still
/// the account's current one becomes `submitting`, committed before any
/// network I/O. A stale generation is skipped and stays `queued` (the
/// Out Box reports it as waiting for `confirm_send_route`). The state,
/// account and generation guards let a cancel or another transition
/// that committed first win; a lost race is `None`, never a panic.
pub fn claim_send_attempt(
    tx: &Transaction,
    account_id: &str,
    now: Timestamp,
) -> Result<Option<ClaimedSend>, StoreError> {
    let err = |e| classify(e, "claim a send attempt");
    let now_ms = now.as_millis();
    let candidate = tx
        .query_row(
            "SELECT s.attempt_id, s.draft_id, s.revision_id, s.account_id, s.identity_id,
                    s.envelope_from, s.message_id_header, s.mime_sha256, s.proton_draft_id,
                    s.config_generation
             FROM send_attempts s
             JOIN accounts a ON a.account_id = s.account_id
             WHERE s.account_id = ?1 AND s.state = 'queued'
               AND s.config_generation = a.config_generation
             ORDER BY s.created_at, s.attempt_id
             LIMIT 1",
            params![account_id],
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
                    row.get::<_, i64>(9)?,
                ))
            },
        )
        .optional()
        .map_err(err)?;
    let Some((
        attempt_id,
        draft_id,
        revision_id,
        attempt_account,
        identity_id,
        envelope_from,
        message_id_header,
        mime_sha256,
        proton_draft_id,
        config_generation,
    )) = candidate
    else {
        return Ok(None);
    };
    let claimed = tx
        .execute(
            "UPDATE send_attempts SET state = 'submitting', updated_at = ?2
             WHERE attempt_id = ?1 AND account_id = ?3 AND state = 'queued'
               AND config_generation = ?4",
            params![attempt_id, now_ms, account_id, config_generation],
        )
        .map_err(err)?;
    if claimed == 0 {
        return Ok(None);
    }
    let recipients = claim_recipients(tx, &attempt_id)?;
    let uploads_json: Option<String> = tx
        .query_row(
            "SELECT proton_uploads_json FROM send_attempts WHERE attempt_id = ?1",
            params![attempt_id],
            |row| row.get(0),
        )
        .map_err(err)?;
    let uploads = parse_uploads(uploads_json.as_deref())?;
    let parent = claim_parent(tx, &draft_id, &attempt_account)?;
    Ok(Some(ClaimedSend {
        attempt_id,
        draft_id,
        revision_id,
        account_id: attempt_account,
        identity_id,
        envelope_from,
        message_id_header,
        mime_sha256,
        recipients,
        proton_draft_id,
        uploads,
        parent,
    }))
}

/// The claimed attempt's recipients, in canonical order (spec/02).
fn claim_recipients(
    tx: &Transaction,
    attempt_id: &str,
) -> Result<Vec<ClaimedRecipient>, StoreError> {
    let err = |e| classify(e, "read a claimed attempt's recipients");
    let mut statement = tx
        .prepare(
            "SELECT canonical, address, kind FROM send_recipients
             WHERE attempt_id = ?1 ORDER BY canonical",
        )
        .map_err(err)?;
    let rows = statement
        .query_map(params![attempt_id], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
            ))
        })
        .map_err(err)?;
    rows.map(|row| {
        let (canonical, address, kind) = row.map_err(err)?;
        Ok(ClaimedRecipient {
            address,
            canonical,
            kind: RecipientKind::from_stored(&kind)?,
        })
    })
    .collect()
}

/// The draft's reply or forward source when it is a message in the
/// same account with a Proton id (spec/05 step 4). A local or
/// cross-account source, or one Proton never listed, is `None`.
fn claim_parent(
    tx: &Transaction,
    draft_id: &str,
    account_id: &str,
) -> Result<Option<SendParent>, StoreError> {
    let err = |e| classify(e, "read a claimed attempt's parent");
    let row = tx
        .query_row(
            "SELECT lm.provider_key,
                    CASE WHEN d.reply_to_occurrence_id IS NOT NULL
                         THEN 'reply' ELSE 'forward' END
             FROM drafts d
             LEFT JOIN occurrences o ON o.occurrence_id = d.reply_to_occurrence_id
             LEFT JOIN logical_messages lm
               ON lm.message_id = COALESCE(o.message_id, d.forward_of_message_id)
              AND lm.account_id = ?1
             WHERE d.draft_id = ?2",
            params![account_id, draft_id],
            |row| Ok((row.get::<_, Option<String>>(0)?, row.get::<_, String>(1)?)),
        )
        .optional()
        .map_err(err)?;
    match row {
        Some((Some(provider_message_id), kind)) => Ok(Some(SendParent {
            provider_message_id,
            kind: SendParentKind::from_stored(&kind)?,
        })),
        _ => Ok(None),
    }
}

/// `record_proton_draft(attempt_id, proton_draft_id)` (spec/05 step
/// 2): commit the draft Proton created, only while `submitting`, and
/// only when none is recorded yet, so a retried attempt reuses its
/// draft instead of naming a second one. Returns whether it applied.
pub fn record_proton_draft(
    tx: &Transaction,
    attempt_id: &str,
    proton_draft_id: &str,
) -> Result<bool, StoreError> {
    let applied = tx
        .execute(
            "UPDATE send_attempts SET proton_draft_id = ?2
             WHERE attempt_id = ?1 AND state = 'submitting' AND proton_draft_id IS NULL",
            params![attempt_id, proton_draft_id],
        )
        .map_err(|e| classify(e, "record the Proton draft"))?;
    Ok(applied == 1)
}

/// `record_proton_upload(attempt_id, upload)` (spec/05 step 3):
/// append one uploaded attachment to `proton_uploads_json`, only while
/// `submitting`. `upload.index` must be the next position in the
/// frozen MIME's order. Returns whether it applied.
pub fn record_proton_upload(
    tx: &Transaction,
    attempt_id: &str,
    upload: &ProtonUpload,
) -> Result<bool, StoreError> {
    let err = |e| classify(e, "record a Proton upload");
    let (state, json): (String, Option<String>) = tx
        .query_row(
            "SELECT state, proton_uploads_json FROM send_attempts WHERE attempt_id = ?1",
            params![attempt_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(err)?
        .ok_or_else(|| StoreError::not_found(format!("send attempt {attempt_id}")))?;
    if state != "submitting" {
        return Ok(false);
    }
    let mut stored = match json.as_deref() {
        None => StoredUploads {
            v: 1,
            attachments: Vec::new(),
        },
        Some(json) => serde_json::from_str(json).map_err(|_| {
            StoreError::corrupt("a send attempt's Proton uploads are not the stored shape")
        })?,
    };
    if stored.v != 1 {
        return Err(StoreError::corrupt(format!(
            "a send attempt's Proton uploads have version {}",
            stored.v
        )));
    }
    if upload.index != stored.attachments.len() {
        return Err(StoreError::InvalidInput {
            message: format!(
                "attachment {} is not the next upload in the frozen MIME's order",
                upload.index
            ),
        });
    }
    stored.attachments.push(StoredUpload {
        id: upload.id.clone(),
        name: upload.name.clone(),
        mime_type: upload.mime_type.clone(),
        size: upload.size,
    });
    let json = serde_json::to_string(&stored)
        .map_err(|_| StoreError::internal("encode a send attempt's Proton uploads"))?;
    let applied = tx
        .execute(
            "UPDATE send_attempts SET proton_uploads_json = ?2
             WHERE attempt_id = ?1 AND state = 'submitting'",
            params![attempt_id, json],
        )
        .map_err(err)?;
    Ok(applied == 1)
}

/// `commit_send_requested(attempt_id, now)` (spec/05 step 4): stamp
/// `send_requested_at` just before the send request, only while
/// `submitting` with a recorded draft. Returns whether it applied.
pub fn commit_send_requested(
    tx: &Transaction,
    attempt_id: &str,
    now: Timestamp,
) -> Result<bool, StoreError> {
    let applied = tx
        .execute(
            "UPDATE send_attempts SET send_requested_at = ?2, updated_at = ?2
             WHERE attempt_id = ?1 AND state = 'submitting' AND proton_draft_id IS NOT NULL",
            params![attempt_id, now.as_millis()],
        )
        .map_err(|e| classify(e, "record the send request"))?;
    Ok(applied == 1)
}

/// `requeue_before_request(attempt_id, now)` (spec/05 step 4): a
/// connection never established or any failure before the send request
/// returns the attempt to `queued` and clears `send_requested_at`,
/// keeping the recorded draft and uploads for the retry. Returns
/// whether it applied.
pub fn requeue_before_request(
    tx: &Transaction,
    attempt_id: &str,
    now: Timestamp,
) -> Result<bool, StoreError> {
    let err = |e| classify(e, "requeue a send attempt");
    let applied = tx
        .execute(
            "UPDATE send_attempts
             SET state = 'queued', send_requested_at = NULL, updated_at = ?2
             WHERE attempt_id = ?1 AND state = 'submitting'",
            params![attempt_id, now.as_millis()],
        )
        .map_err(err)?;
    if applied == 1 {
        // The retry's commit writes the scheme again, so a requeued
        // attempt shows no stale scheme (spec/02, 05 step 5).
        tx.execute(
            "UPDATE send_recipients SET scheme = NULL WHERE attempt_id = ?1",
            params![attempt_id],
        )
        .map_err(err)?;
    }
    Ok(applied == 1)
}

/// `record_recipient_schemes(attempt_id, schemes)` (spec/05 step 4):
/// write each Proton recipient's package scheme, in the transaction
/// that commits `send_requested_at`, so the Out Box says what the
/// request carried (spec/02). A canonical with no row is a programming
/// bug: the engine passes the attempt's own recipients.
pub fn record_recipient_schemes(
    tx: &Transaction,
    attempt_id: &str,
    schemes: &[(String, RecipientScheme)],
) -> Result<(), StoreError> {
    for (canonical, scheme) in schemes {
        let applied = tx
            .execute(
                "UPDATE send_recipients SET scheme = ?3
                 WHERE attempt_id = ?1 AND canonical = ?2",
                params![attempt_id, canonical, scheme.as_str()],
            )
            .map_err(|e| classify(e, "record the recipient schemes"))?;
        if applied != 1 {
            return Err(StoreError::internal(format!(
                "send attempt {attempt_id} has no recipient {canonical}"
            )));
        }
    }
    Ok(())
}

/// The attempt's draft, revision and account, or `NotFound`.
fn attempt_row(tx: &Transaction, attempt_id: &str) -> Result<(String, String, String), StoreError> {
    let err = |e| classify(e, "read a send attempt");
    tx.query_row(
        "SELECT draft_id, revision_id, account_id FROM send_attempts
         WHERE attempt_id = ?1",
        params![attempt_id],
        |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    )
    .optional()
    .map_err(err)?
    .ok_or_else(|| StoreError::not_found(format!("send attempt {attempt_id}")))
}

/// `mark_send_accepted(attempt_id, now)` (spec/05 step 4): a
/// `submitting` attempt, or an unresolved `outcome_unknown` the
/// evidence check resolved as sent, becomes `accepted`; its recipients
/// become `sent`, each counted in `seen_addresses`, and the draft
/// `sent` under the current-revision rule, all in one transaction.
pub fn mark_send_accepted(
    tx: &Transaction,
    attempt_id: &str,
    now: Timestamp,
) -> Result<bool, StoreError> {
    let err = |e| classify(e, "accept a send");
    let now_ms = now.as_millis();
    let (draft_id, revision_id, account_id) = attempt_row(tx, attempt_id)?;
    let applied = tx
        .execute(
            "UPDATE send_attempts SET state = 'accepted', updated_at = ?2
             WHERE attempt_id = ?1
               AND (state = 'submitting'
                    OR (state = 'outcome_unknown' AND resolution IS NULL))",
            params![attempt_id, now_ms],
        )
        .map_err(err)?;
    if applied == 0 {
        return Ok(false);
    }
    let recipients: Vec<(String, String)> = {
        let mut statement = tx
            .prepare(
                "SELECT canonical, address FROM send_recipients
                 WHERE attempt_id = ?1 ORDER BY canonical",
            )
            .map_err(err)?;
        let rows = statement
            .query_map(params![attempt_id], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(err)?;
        rows.collect::<Result<_, _>>().map_err(err)?
    };
    tx.execute(
        "UPDATE send_recipients SET state = 'sent' WHERE attempt_id = ?1",
        params![attempt_id],
    )
    .map_err(err)?;
    add_sent_to_counts(tx, &recipients, &account_id, now_ms)?;
    set_draft_state(tx, &draft_id, &revision_id, "sent", now_ms)?;
    Ok(true)
}

/// `mark_send_failed(attempt_id, failure, now)` (spec/05 step 4): a
/// `submitting` attempt becomes `failed` with the reply in
/// `final_reply`; its recipients become `unsent`, except an optional one
/// set `rejected_permanent`, and the draft returns to `editing` under
/// the current-revision rule, all in one transaction.
pub fn mark_send_failed(
    tx: &Transaction,
    attempt_id: &str,
    failure: &SendFailure,
    now: Timestamp,
) -> Result<bool, StoreError> {
    let err = |e| classify(e, "fail a send");
    let now_ms = now.as_millis();
    let (draft_id, revision_id, _account) = attempt_row(tx, attempt_id)?;
    // A rejected address that is not on the attempt is the caller's
    // mistake; refuse it before any row changes.
    if let Some(rejected) = &failure.rejected {
        let known = tx
            .query_row(
                "SELECT 1 FROM send_recipients
                 WHERE attempt_id = ?1 AND canonical = ?2 LIMIT 1",
                params![attempt_id, rejected.canonical],
                |_| Ok(()),
            )
            .optional()
            .map_err(err)?;
        if known.is_none() {
            return Err(StoreError::InvalidInput {
                message: format!(
                    "recipient {} is not on send attempt {attempt_id}",
                    rejected.canonical
                ),
            });
        }
    }
    let applied = tx
        .execute(
            "UPDATE send_attempts
             SET state = 'failed', final_reply = ?2, updated_at = ?3
             WHERE attempt_id = ?1 AND state = 'submitting'",
            params![
                attempt_id,
                final_reply(failure.code, &failure.message),
                now_ms
            ],
        )
        .map_err(err)?;
    if applied == 0 {
        return Ok(false);
    }
    tx.execute(
        "UPDATE send_recipients SET state = 'unsent' WHERE attempt_id = ?1",
        params![attempt_id],
    )
    .map_err(err)?;
    if let Some(rejected) = &failure.rejected {
        tx.execute(
            "UPDATE send_recipients
             SET state = 'rejected_permanent', rcpt_reply = ?3
             WHERE attempt_id = ?1 AND canonical = ?2",
            params![attempt_id, rejected.canonical, rejected.reason],
        )
        .map_err(err)?;
    }
    set_draft_state(tx, &draft_id, &revision_id, "editing", now_ms)?;
    Ok(true)
}

/// `mark_outcome_unknown(attempt_id, now)` (spec/05 step 4): a
/// `submitting` attempt whose send reply was lost becomes
/// `outcome_unknown` and its recipients `unknown`; the draft reads
/// `queued` under the current-revision rule.
pub fn mark_outcome_unknown(
    tx: &Transaction,
    attempt_id: &str,
    now: Timestamp,
) -> Result<bool, StoreError> {
    let err = |e| classify(e, "mark a send's outcome unknown");
    let now_ms = now.as_millis();
    let (draft_id, revision_id, _account) = attempt_row(tx, attempt_id)?;
    let applied = tx
        .execute(
            "UPDATE send_attempts SET state = 'outcome_unknown', updated_at = ?2
             WHERE attempt_id = ?1 AND state = 'submitting'",
            params![attempt_id, now_ms],
        )
        .map_err(err)?;
    if applied == 0 {
        return Ok(false);
    }
    tx.execute(
        "UPDATE send_recipients SET state = 'unknown' WHERE attempt_id = ?1",
        params![attempt_id],
    )
    .map_err(err)?;
    set_draft_state(tx, &draft_id, &revision_id, "queued", now_ms)?;
    Ok(true)
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

/// The `sent_copy` column's words (spec/02 "Sending"): how the
/// account's Sent copy is produced. Proton saves it itself at insert;
/// a generic account's is `pending` until it is filed.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum SentCopy {
    ServerSaves,
    Pending,
    Filed,
    Failed,
}

impl SentCopy {
    fn from_stored(name: &str) -> Result<Self, StoreError> {
        match name {
            "server_saves" => Ok(Self::ServerSaves),
            "pending" => Ok(Self::Pending),
            "filed" => Ok(Self::Filed),
            "failed" => Ok(Self::Failed),
            _ => Err(StoreError::corrupt(format!(
                "a send attempt's sent_copy is '{name}'"
            ))),
        }
    }

    /// The stored word.
    pub fn name(self) -> &'static str {
        match self {
            Self::ServerSaves => "server_saves",
            Self::Pending => "pending",
            Self::Filed => "filed",
            Self::Failed => "failed",
        }
    }
}

/// A recipient's address field (spec/02 `send_recipients.kind`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecipientKind {
    To,
    Cc,
    Bcc,
}

impl RecipientKind {
    fn from_stored(name: &str) -> Result<Self, StoreError> {
        match name {
            "to" => Ok(Self::To),
            "cc" => Ok(Self::Cc),
            "bcc" => Ok(Self::Bcc),
            _ => Err(StoreError::corrupt(format!(
                "a send recipient's kind is '{name}'"
            ))),
        }
    }

    /// The stored word.
    pub fn name(self) -> &'static str {
        match self {
            Self::To => "to",
            Self::Cc => "cc",
            Self::Bcc => "bcc",
        }
    }
}

/// A recipient's stored state (spec/02 `send_recipients.state`).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecipientState {
    Pending,
    RcptAccepted,
    RejectedPermanent,
    RejectedTransient,
    Sent,
    Unsent,
    Unknown,
}

impl RecipientState {
    fn from_stored(name: &str) -> Result<Self, StoreError> {
        match name {
            "pending" => Ok(Self::Pending),
            "rcpt_accepted" => Ok(Self::RcptAccepted),
            "rejected_permanent" => Ok(Self::RejectedPermanent),
            "rejected_transient" => Ok(Self::RejectedTransient),
            "sent" => Ok(Self::Sent),
            "unsent" => Ok(Self::Unsent),
            "unknown" => Ok(Self::Unknown),
            _ => Err(StoreError::corrupt(format!(
                "a send recipient's state is '{name}'"
            ))),
        }
    }

    /// The stored word.
    pub fn name(self) -> &'static str {
        match self {
            Self::Pending => "pending",
            Self::RcptAccepted => "rcpt_accepted",
            Self::RejectedPermanent => "rejected_permanent",
            Self::RejectedTransient => "rejected_transient",
            Self::Sent => "sent",
            Self::Unsent => "unsent",
            Self::Unknown => "unknown",
        }
    }
}

/// A Proton recipient's package scheme (spec/02 "Sending", spec/05
/// step 4): `internal` encrypted to its Proton keys, `clear` an
/// external address sent without end-to-end encryption. NULL before
/// the send request commits and for generic attempts; a requeue that
/// clears `send_requested_at` clears it too (02).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum RecipientScheme {
    Internal,
    Clear,
}

impl RecipientScheme {
    fn from_stored(name: &str) -> Result<Self, StoreError> {
        match name {
            "internal" => Ok(Self::Internal),
            "clear" => Ok(Self::Clear),
            _ => Err(StoreError::corrupt(format!(
                "a send recipient's scheme is '{name}'"
            ))),
        }
    }

    /// The stored word.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Internal => "internal",
            Self::Clear => "clear",
        }
    }
}

/// One recipient on an Out Box row (spec/07): the address as first
/// written, its field, its stored state and, once the send request
/// committed, its Proton scheme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutBoxRecipient {
    pub address: String,
    pub kind: RecipientKind,
    pub state: RecipientState,
    pub scheme: Option<RecipientScheme>,
}

/// One Out Box row (spec/07 `out_box()`): the attempt's frozen
/// revision subject, its recipients, state and stored times. The
/// Proton evidence is not stored, so it is the engine's row, not this
/// one, that carries it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutBoxRow {
    pub attempt_id: String,
    pub draft_id: String,
    pub account_id: String,
    pub identity_id: String,
    pub subject: String,
    pub recipients: Vec<OutBoxRecipient>,
    pub state: AttemptState,
    pub created_at: Timestamp,
    pub updated_at: Timestamp,
    pub hold_until: Option<Timestamp>,
    pub resolved_at: Option<Timestamp>,
    pub send_requested_at: Option<Timestamp>,
    /// The Proton draft the attempt created, which becomes the sent
    /// message (spec/02, 05 "Drafts on the server").
    pub proton_draft_id: Option<String>,
    /// Proton's final reply, code and message (spec/02, 05).
    pub final_reply: Option<String>,
    pub resolution: Option<UnknownResolution>,
    pub sent_copy: SentCopy,
    /// A `queued` attempt whose `config_generation` differs from its
    /// account's current one waits for `confirm_send_route`
    /// (spec/05 step 3).
    pub waits_for_confirmation: bool,
    /// For an accepted attempt whose provider saves the Sent copy:
    /// whether a message whose Proton id is the attempt's
    /// `proton_draft_id` has appeared in the account (spec/05 "The
    /// Sent copy"). False for every other row.
    pub sent_copy_appeared: bool,
    /// An accepted `server_saves` attempt with no Sent copy 24 h
    /// after acceptance: a soft warning, never a resend or a request
    /// (spec/05 "The Sent copy").
    pub sent_copy_warning: bool,
}

/// The Out Box (spec/07 `out_box()`): every live attempt, then the
/// last 30 days' history, each newest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutBox {
    pub live: Vec<OutBoxRow>,
    pub history: Vec<OutBoxRow>,
}

/// The history part's window, in days (spec/07).
const OUT_BOX_HISTORY_DAYS: i64 = 30;
const MILLIS_PER_DAY: i64 = 24 * 60 * 60 * 1000;
/// How long an accepted `server_saves` attempt may lack its Sent copy
/// before the Out Box warns (spec/05 "The Sent copy").
const SENT_COPY_WARNING_AFTER_MS: i64 = MILLIS_PER_DAY;
/// Recipients are read in bounded parameter chunks, well under
/// SQLite's host-parameter limit.
const OUT_BOX_RECIPIENT_CHUNK: usize = 400;

/// `out_box()` (spec/07): the live part (every `held`, `queued`,
/// `submitting` and unresolved `outcome_unknown` attempt, and every
/// `failed` one whose draft has no newer attempt) and the history part
/// (the last 30 days by `updated_at`: `accepted`, `cancelled`,
/// resolved `outcome_unknown`, and `failed` ones a newer attempt
/// replaced). Each part is newest first by `updated_at`, ties broken
/// by `attempt_id`. `now` is the read's clock, so the cut is a
/// parameter rather than a host read. "Newer attempt" means a later
/// attempt for the same draft by `created_at`, ties broken by
/// `attempt_id`.
pub fn out_box(tx: &Transaction, now: Timestamp) -> Result<OutBox, StoreError> {
    let err = |e| classify(e, "read the out box");
    let cutoff = now
        .as_millis()
        .checked_sub(OUT_BOX_HISTORY_DAYS * MILLIS_PER_DAY)
        .ok_or_else(|| StoreError::internal("the out box's history cutoff overflows"))?;
    let mut statement = tx
        .prepare(
            "WITH candidates AS (
                SELECT s.attempt_id, s.draft_id, s.account_id, s.identity_id, r.subject,
                       s.state, s.created_at, s.updated_at, s.hold_until, s.resolved_at,
                       s.send_requested_at, s.proton_draft_id, s.final_reply, s.resolution,
                       s.sent_copy, s.config_generation, a.config_generation AS account_generation,
                       (SELECT count(*) FROM logical_messages lm
                        JOIN occurrences o ON o.message_id = lm.message_id
                         AND o.account_id = lm.account_id AND o.state = 'present'
                        JOIN mailboxes mb ON mb.mailbox_id = o.mailbox_id AND mb.role = 'sent'
                        WHERE lm.account_id = s.account_id
                          AND lm.provider_key = s.proton_draft_id) AS sent_copy_hits,
                       (SELECT count(*) FROM send_attempts n
                        WHERE n.draft_id = s.draft_id
                          AND (n.created_at > s.created_at
                               OR (n.created_at = s.created_at AND n.attempt_id > s.attempt_id)))
                           AS newer_attempts
                FROM send_attempts s
                JOIN draft_revisions r
                  ON r.revision_id = s.revision_id AND r.draft_id = s.draft_id
                JOIN accounts a ON a.account_id = s.account_id
             )
             SELECT attempt_id, draft_id, account_id, identity_id, subject, state,
                    created_at, updated_at, hold_until, resolved_at, send_requested_at,
                    proton_draft_id, final_reply, resolution, sent_copy, newer_attempts,
                    config_generation, account_generation, sent_copy_hits
             FROM candidates
             WHERE state IN ('held','queued','submitting')
                OR (state = 'outcome_unknown' AND resolution IS NULL)
                OR (state = 'failed' AND newer_attempts = 0)
                OR (updated_at >= ?1 AND (
                     state IN ('accepted','cancelled')
                     OR (state = 'outcome_unknown' AND resolution IS NOT NULL)
                     OR (state = 'failed' AND newer_attempts > 0)))",
        )
        .map_err(err)?;
    let mut rows = statement.query(params![cutoff]).map_err(err)?;
    let mut candidates: Vec<(bool, OutBoxRow)> = Vec::new();
    while let Some(row) = rows.next().map_err(err)? {
        let attempt_id: String = row.get(0).map_err(err)?;
        let state: String = row.get(5).map_err(err)?;
        let resolution: Option<String> = row.get(13).map_err(err)?;
        let newer_attempts: i64 = row.get(15).map_err(err)?;
        let config_generation: i64 = row.get(16).map_err(err)?;
        let account_generation: i64 = row.get(17).map_err(err)?;
        let state = AttemptState::from_stored(&state)?;
        let resolution = match resolution {
            Some(word) => Some(UnknownResolution::from_stored(&word)?),
            None => None,
        };
        let live = match state {
            AttemptState::Held | AttemptState::Queued | AttemptState::Submitting => true,
            AttemptState::OutcomeUnknown => resolution.is_none(),
            AttemptState::Failed => newer_attempts == 0,
            AttemptState::Accepted | AttemptState::Cancelled => false,
        };
        let sent_copy = SentCopy::from_stored(&row.get::<_, String>(14).map_err(err)?)?;
        let sent_copy_hits: i64 = row.get(18).map_err(err)?;
        let proton_draft_id: Option<String> = row.get(11).map_err(err)?;
        let updated_at = Timestamp::from_millis(row.get(7).map_err(err)?);
        // The Sent copy question only applies to an accepted attempt
        // whose provider saves it, and only when a draft id can name
        // the message the sync inserted.
        let reconciles_sent_copy = state == AttemptState::Accepted
            && sent_copy == SentCopy::ServerSaves
            && proton_draft_id.is_some();
        let sent_copy_appeared = reconciles_sent_copy && sent_copy_hits > 0;
        let sent_copy_warning = reconciles_sent_copy
            && sent_copy_hits == 0
            && now.as_millis().saturating_sub(updated_at.as_millis()) >= SENT_COPY_WARNING_AFTER_MS;
        candidates.push((
            live,
            OutBoxRow {
                recipients: Vec::new(),
                attempt_id,
                draft_id: row.get(1).map_err(err)?,
                account_id: row.get(2).map_err(err)?,
                identity_id: row.get(3).map_err(err)?,
                subject: row.get(4).map_err(err)?,
                state,
                created_at: Timestamp::from_millis(row.get(6).map_err(err)?),
                updated_at,
                hold_until: row
                    .get::<_, Option<i64>>(8)
                    .map_err(err)?
                    .map(Timestamp::from_millis),
                resolved_at: row
                    .get::<_, Option<i64>>(9)
                    .map_err(err)?
                    .map(Timestamp::from_millis),
                send_requested_at: row
                    .get::<_, Option<i64>>(10)
                    .map_err(err)?
                    .map(Timestamp::from_millis),
                proton_draft_id,
                final_reply: row.get(12).map_err(err)?,
                resolution,
                sent_copy,
                waits_for_confirmation: state == AttemptState::Queued
                    && config_generation != account_generation,
                sent_copy_appeared,
                sent_copy_warning,
            },
        ));
    }
    let recipients = out_box_recipients(tx, &candidates)?;
    for (_, row) in &mut candidates {
        let attempt_id = row.attempt_id.clone();
        row.recipients = recipients.get(&attempt_id).cloned().unwrap_or_default();
    }
    let mut live: Vec<OutBoxRow> = Vec::new();
    let mut history: Vec<OutBoxRow> = Vec::new();
    for (is_live, row) in candidates {
        if is_live {
            live.push(row);
        } else {
            history.push(row);
        }
    }
    let by_newest = |a: &OutBoxRow, b: &OutBoxRow| {
        b.updated_at
            .cmp(&a.updated_at)
            .then_with(|| b.attempt_id.cmp(&a.attempt_id))
    };
    live.sort_by(by_newest);
    history.sort_by(by_newest);
    Ok(OutBox { live, history })
}

/// Read the recipients of every candidate attempt, grouped by
/// attempt, in canonical order. The chunked `IN` keeps the host
/// parameter count bounded however large the Out Box grows.
fn out_box_recipients(
    tx: &Transaction,
    candidates: &[(bool, OutBoxRow)],
) -> Result<HashMap<String, Vec<OutBoxRecipient>>, StoreError> {
    let err = |e| classify(e, "read the out box's recipients");
    let mut by_attempt: HashMap<String, Vec<OutBoxRecipient>> = HashMap::new();
    let attempt_ids: Vec<String> = candidates
        .iter()
        .map(|(_, row)| row.attempt_id.clone())
        .collect();
    for chunk in attempt_ids.chunks(OUT_BOX_RECIPIENT_CHUNK) {
        let placeholders = vec!["?"; chunk.len()].join(",");
        let sql = format!(
            "SELECT attempt_id, address, kind, state, scheme FROM send_recipients
             WHERE attempt_id IN ({placeholders}) ORDER BY attempt_id, canonical"
        );
        let mut statement = tx.prepare(&sql).map_err(err)?;
        let rows = statement
            .query_map(params_from_iter(chunk), |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, Option<String>>(4)?,
                ))
            })
            .map_err(err)?;
        for row in rows {
            let (attempt_id, address, kind, state, scheme) = row.map_err(err)?;
            let scheme = match scheme {
                Some(word) => Some(RecipientScheme::from_stored(&word)?),
                None => None,
            };
            let recipient = OutBoxRecipient {
                address,
                kind: RecipientKind::from_stored(&kind)?,
                state: RecipientState::from_stored(&state)?,
                scheme,
            };
            by_attempt.entry(attempt_id).or_default().push(recipient);
        }
    }
    Ok(by_attempt)
}

#[cfg(test)]
mod tests;
