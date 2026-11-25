//! Sending (spec/05 "Outbox state machine", spec/07 "Commands"):
//! the `send_draft` and `cancel_send` commands, the `outcome_unknown`
//! resolutions, `confirm_send_route` and `retry_rejected_recipients`.
//! `send_draft` composes the frozen MIME outside any transaction,
//! publishes it, then queues the `held` attempt and its recipients in
//! one store transaction; `cancel_send` moves a `held` or `queued`
//! attempt to `cancelled`. A resolution records the user's answer and,
//! for `send_again`, re-queues the frozen bytes under a new attempt.

use std::sync::Arc;

use sb_resources::{
    AccountKind, AttemptState, ComposeAddress, ComposeAttachment, ComposeError, ComposeFormat,
    ComposeIdentity, ComposeInput, DraftState, IdentityRoute, MimeComposeFormat, OutBoxRecipient,
    PublishedObject, Receipt, Resources, SendAttempt, SentCopy, SourceIdentityInputs,
    SourceMessageHeaders, StoreError, StoredDraft, StoredRevision, Timestamp, Transaction,
    UnknownResolution, WritePriority, cancel_send, compose, compose_catalog, confirm_send_route,
    draft, out_box as stored_out_box, replay, resolve_unknown_send, retry_rejected_recipients,
    run_once, send_draft,
};

use crate::drafts::{encode_receipt_id, receipt_id};
use crate::engine::validate_idempotency_key;
use crate::identity::{ComposeContext, CrossAccountDecision, cross_account_decision};
use crate::recipients::refuse_blocked;
use crate::settings::{Setting, UNDO_SECONDS_KEY};
use crate::{Engine, EngineError};

/// The commands' names in `command_receipts` (spec/07).
const SEND_DRAFT: &str = "send_draft";
const CANCEL_SEND: &str = "cancel_send";
const RESOLVE_UNKNOWN_SEND: &str = "resolve_unknown_send";
const CONFIRM_SEND_ROUTE: &str = "confirm_send_route";
const RETRY_REJECTED_RECIPIENTS: &str = "retry_rejected_recipients";

/// The `User-Agent` header's value (spec/05).
const USER_AGENT: &str = concat!("Sweetbriar/", env!("CARGO_PKG_VERSION"));

/// What `send_draft` answers: its receipt and the attempt id the
/// receipt names, so a replayed key still names the first attempt.
#[derive(Debug, Clone)]
pub struct SendQueued {
    receipt: Receipt,
    attempt_id: String,
}

impl SendQueued {
    /// The durable receipt (`{"attempt_id": ...}`).
    pub fn receipt(&self) -> &Receipt {
        &self.receipt
    }

    /// The queued attempt's id.
    pub fn attempt_id(&self) -> &str {
        &self.attempt_id
    }
}

/// The cross-account warning for one draft revision (spec/05, 07):
/// the receiving occurrence's account, the route identity's account,
/// and the acknowledgement `send_draft` accepts for exactly that
/// revision's identity and recipients. The core never stores the
/// acknowledgement (spec/05).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrossAccountNotice {
    pub source_account_id: String,
    pub route_account_id: String,
    pub ack: String,
}

/// What `resolve_unknown_send` answers.
#[derive(Debug, Clone)]
pub struct ResolvedUnknown {
    receipt: Receipt,
    resolution: UnknownResolution,
    new_attempt_id: Option<String>,
}

impl ResolvedUnknown {
    /// The durable receipt.
    pub fn receipt(&self) -> &Receipt {
        &self.receipt
    }

    /// The recorded resolution.
    pub fn resolution(&self) -> UnknownResolution {
        self.resolution
    }

    /// The new attempt's id for `send_again`, else `None`.
    pub fn new_attempt_id(&self) -> Option<&str> {
        self.new_attempt_id.as_deref()
    }
}

impl Engine {
    /// `send_draft(draft_id, revision_id, acks)` (spec/05, 07): queue
    /// the draft's current revision as a `held` attempt. A
    /// non-current revision, a non-`editing` draft, a live or
    /// already-sent attempt are `Conflict`. `acks` must carry the
    /// cross-account acknowledgement for the reply's identity and
    /// recipients; a missing one is `Conflict` naming it. A recipient
    /// the recipient check blocks is `Conflict` naming
    /// `recipient_fence`, whatever the `acks`.
    pub async fn send_draft(
        &self,
        idempotency_key: &str,
        draft_id: &str,
        revision_id: &str,
        acks: &[String],
    ) -> Result<SendQueued, EngineError> {
        validate_idempotency_key(idempotency_key)?;
        if draft_id.is_empty() {
            return Err(EngineError::invalid_input(
                "draft_id",
                "a draft id is required",
            ));
        }
        if revision_id.is_empty() {
            return Err(EngineError::invalid_input(
                "revision_id",
                "a revision id is required",
            ));
        }
        // A replayed key answers its first attempt before the checks
        // below, which that attempt's `queued` draft would now fail,
        // and before composing bytes nothing would reference (07).
        let replay_key = idempotency_key.to_owned();
        let replayed = self
            .read_through(move |resources| resources.read(|tx| replay(tx, &replay_key, SEND_DRAFT)))
            .await?;
        if let Some(receipt) = replayed {
            return Ok(SendQueued {
                attempt_id: receipt_id(&receipt, "attempt_id")?,
                receipt,
            });
        }
        let undo_seconds = undo_seconds(&self.settings().await?)?;
        let (draft_id, revision_id) = (draft_id.to_owned(), revision_id.to_owned());
        let read_id = draft_id.clone();
        let stored = self
            .read_through(move |resources| resources.read(|tx| draft(tx, &read_id)))
            .await?
            .ok_or_else(|| EngineError::NotFound {
                what: format!("draft {draft_id}"),
            })?;
        let revision = stored
            .revision
            .clone()
            .ok_or_else(|| EngineError::Conflict {
                detail: format!("draft {} has no revision to send", stored.draft_id),
            })?;
        if revision.revision_id != revision_id {
            return Err(EngineError::Conflict {
                detail: format!(
                    "draft {}'s current revision is not {revision_id}",
                    stored.draft_id
                ),
            });
        }
        if stored.state != DraftState::Editing {
            return Err(EngineError::Conflict {
                detail: format!("draft {} is {}", stored.draft_id, stored.state.name()),
            });
        }
        let identity_id = revision.identity_id.clone().ok_or_else(|| {
            EngineError::invalid_input("identity_id", "the revision has no sending identity")
        })?;
        let catalog = self
            .read_through(|resources| resources.read(|tx| compose_catalog(tx, None)))
            .await?;
        let identity = catalog
            .identities()
            .iter()
            .find(|identity| identity.identity_id() == identity_id)
            .cloned()
            .ok_or_else(|| {
                EngineError::invalid_input("identity_id", "the revision's identity is gone")
            })?;
        if identity.account_kind() == AccountKind::Local || !identity.sendable().can_send() {
            return Err(EngineError::invalid_input(
                "identity_id",
                "the revision has no identity that can send",
            ));
        }
        let account_id = revision.account_id.clone().ok_or_else(|| {
            EngineError::internal("the revision's route disappeared while sending")
        })?;
        // The cross-account check (spec/05): a reply whose identity's
        // account differs from the receiving occurrence's needs the
        // acknowledgement for this revision's identity and recipients.
        // Local-archive sources are exempt; the store's own rule
        // handles them here.
        let route = identity_route(&identity, &account_id);
        let decision = self
            .revision_cross_account(&stored, &revision, &route)
            .await?;
        if let Some(decision) = decision.as_ref()
            && !acks.iter().any(|ack| ack == &decision.ack)
        {
            return Err(EngineError::Conflict {
                detail: "this reply sends from another account; acknowledge cross_account"
                    .to_owned(),
            });
        }
        // The recipient check (spec/05), before composing bytes nothing
        // would reference; `acks` don't override it. The queuing
        // transaction checks again, so an allowance removed meanwhile
        // still refuses.
        let fence_revision = revision.clone();
        self.read_through(move |resources| {
            resources.read(|tx| refuse_blocked(tx, &fence_revision))
        })
        .await?;
        let (in_reply_to, references) = self
            .read_through(move |resources| {
                resources.read(|tx| {
                    sb_resources::reply_headers(
                        tx,
                        stored.reply_to_occurrence_id,
                        stored.forward_of_message_id,
                    )
                })
            })
            .await?;
        let attempt_id = self.resources().new_uuid_string();
        let now = self.resources().now();
        let resources = Arc::clone(self.resources());
        let compose_revision = revision.clone();
        let compose_identity = identity.clone();
        let (message_id, object) = self
            .compute(move || {
                compose_and_publish(
                    &resources,
                    &compose_revision,
                    &compose_identity,
                    in_reply_to,
                    references,
                    now,
                )
            })
            .await?;
        let attempt = SendAttempt {
            attempt_id: attempt_id.clone(),
            draft_id,
            revision_id: revision.revision_id.clone(),
            account_id,
            identity_id,
            mime_sha256: object.id().sha256_hex(),
            message_id_header: message_id,
            envelope_from: identity.email().to_owned(),
            to: revision.to.clone(),
            cc: revision.cc.clone(),
            bcc: revision.bcc.clone(),
        };
        let key = idempotency_key.to_owned();
        let fenced = revision;
        let receipt = self
            .resources()
            .write(WritePriority::User, move |tx| {
                let receipt = run_once(tx, &key, SEND_DRAFT, now, |tx| {
                    refuse_blocked(tx, &fenced)?;
                    send_draft(tx, &attempt, &object, undo_seconds, now)?;
                    Ok(encode_receipt_id("attempt_id", &attempt.attempt_id))
                })?;
                Ok(receipt)
            })
            .await?;
        // The account's worker may be waiting out a poll interval; a
        // queued attempt is worth claiming now (spec/05 step 2).
        self.account_jobs().wake();
        Ok(SendQueued {
            attempt_id: receipt_id(&receipt, "attempt_id")?,
            receipt,
        })
    }

    /// `cross_account_notice(draft_id, revision_id)` (spec/07): the
    /// cross-account warning for that revision of a reply or reply-all,
    /// or `None` when none applies (spec/05). A revision that isn't the
    /// draft's current one is `Conflict`, an unknown draft `NotFound`.
    /// The acknowledgement is the token `send_draft`'s `acks` takes for
    /// the same revision, computed by the same helper.
    pub async fn cross_account_notice(
        &self,
        draft_id: &str,
        revision_id: &str,
    ) -> Result<Option<CrossAccountNotice>, EngineError> {
        let (draft_id, revision_id) = (draft_id.to_owned(), revision_id.to_owned());
        let read_id = draft_id.clone();
        let stored = self
            .read_through(move |resources| resources.read(|tx| draft(tx, &read_id)))
            .await?
            .ok_or_else(|| EngineError::NotFound {
                what: format!("draft {draft_id}"),
            })?;
        let revision = stored
            .revision
            .clone()
            .ok_or_else(|| EngineError::Conflict {
                detail: format!("draft {} has no revision to send", stored.draft_id),
            })?;
        if revision.revision_id != revision_id {
            return Err(EngineError::Conflict {
                detail: format!(
                    "draft {}'s current revision is not {revision_id}",
                    stored.draft_id
                ),
            });
        }
        let catalog = self
            .read_through(|resources| resources.read(|tx| compose_catalog(tx, None)))
            .await?;
        // Without a routed identity there is nothing to warn about yet:
        // `send_draft` refuses such a revision on its own (spec/07).
        let route = match (
            revision.identity_id.as_deref(),
            revision.account_id.as_deref(),
        ) {
            (Some(identity_id), Some(account_id)) => catalog
                .identities()
                .iter()
                .find(|identity| identity.identity_id() == identity_id)
                .map(|identity| identity_route(identity, account_id)),
            _ => None,
        };
        let Some(route) = route else {
            return Ok(None);
        };
        let decision = self
            .revision_cross_account(&stored, &revision, &route)
            .await?;
        Ok(decision.map(|decision| CrossAccountNotice {
            source_account_id: decision.warning.source_account_id,
            route_account_id: decision.warning.route_account_id,
            ack: decision.ack,
        }))
    }

    /// The cross-account decision for one draft revision sent through
    /// `route` (spec/05). `send_draft` checks its `acks` against it and
    /// `cross_account_notice` hands out its `ack`, so the two cannot
    /// drift.
    async fn revision_cross_account(
        &self,
        stored: &StoredDraft,
        revision: &StoredRevision,
        route: &IdentityRoute,
    ) -> Result<Option<CrossAccountDecision>, EngineError> {
        let source_account = match stored.reply_to_occurrence_id {
            Some(occurrence_id) => Some(
                self.read_through(move |resources| {
                    resources.read(|tx| sb_resources::reply_source_account(tx, occurrence_id))
                })
                .await?,
            ),
            None => None,
        };
        let context = if stored.reply_to_occurrence_id.is_some() {
            ComposeContext::Reply
        } else if stored.forward_of_message_id.is_some() {
            ComposeContext::Forward
        } else {
            ComposeContext::New
        };
        let source = source_account.map(|(account_id, account_kind)| SourceIdentityInputs {
            account_id,
            account_kind,
            headers: SourceMessageHeaders::default(),
            sent_identity_id: None,
        });
        Ok(cross_account_decision(
            context,
            Some(route),
            source.as_ref(),
            &route.identity_id,
            &revision.to,
            &revision.cc,
            &revision.bcc,
        ))
    }

    /// `cancel_send(attempt_id)` (spec/05, 07): a `held` or `queued`
    /// attempt becomes `cancelled` and its draft returns to `editing`
    /// while the attempt's revision is still current. Any other state
    /// is `Conflict`; an unknown attempt is `NotFound`. The receipt
    /// answers `{"attempt_id": ...}`.
    pub async fn cancel_send(
        &self,
        idempotency_key: &str,
        attempt_id: &str,
    ) -> Result<Receipt, EngineError> {
        validate_idempotency_key(idempotency_key)?;
        if attempt_id.is_empty() {
            return Err(EngineError::invalid_input(
                "attempt_id",
                "an attempt id is required",
            ));
        }
        let now = self.resources().now();
        let (key, id) = (idempotency_key.to_owned(), attempt_id.to_owned());
        let receipt = self
            .resources()
            .write(WritePriority::User, move |tx| {
                let receipt = run_once(tx, &key, CANCEL_SEND, now, |tx| {
                    cancel_send(tx, &id, now)?;
                    Ok(encode_receipt_id("attempt_id", &id))
                })?;
                Ok(receipt)
            })
            .await?;
        receipt_id(&receipt, "attempt_id")?;
        Ok(receipt)
    }

    /// `resolve_unknown_send(attempt_id, resolution)` (spec/05 step 6,
    /// 07): record the user's answer to an unresolved `outcome_unknown`
    /// attempt, exactly once. `marked_sent` moves its `unknown`
    /// recipients to `sent` and sets the draft `sent`; `kept_as_draft`
    /// returns the draft to `editing`; `send_again` re-queues the
    /// frozen bytes. A second resolution is `Conflict`.
    pub async fn resolve_unknown_send(
        &self,
        idempotency_key: &str,
        attempt_id: &str,
        resolution: UnknownResolution,
    ) -> Result<ResolvedUnknown, EngineError> {
        validate_idempotency_key(idempotency_key)?;
        if attempt_id.is_empty() {
            return Err(EngineError::invalid_input(
                "attempt_id",
                "an attempt id is required",
            ));
        }
        // A replayed key answers the resolution it recorded before
        // the attempt's now-resolved state would refuse it.
        let replay_key = idempotency_key.to_owned();
        let replayed = self
            .read_through(move |resources| {
                resources.read(|tx| replay(tx, &replay_key, RESOLVE_UNKNOWN_SEND))
            })
            .await?;
        if let Some(receipt) = replayed {
            let (_, resolution, new_attempt_id) = resolution_from_receipt(&receipt)?;
            return Ok(ResolvedUnknown {
                receipt,
                resolution,
                new_attempt_id,
            });
        }
        let undo_seconds = match resolution {
            UnknownResolution::SendAgain => undo_seconds(&self.settings().await?)?,
            _ => 0,
        };
        let new_attempt_id = match resolution {
            UnknownResolution::SendAgain => Some(self.resources().new_uuid_string()),
            _ => None,
        };
        let now = self.resources().now();
        let (key, id) = (idempotency_key.to_owned(), attempt_id.to_owned());
        let receipt = self
            .resources()
            .write(WritePriority::User, move |tx| {
                let receipt = run_once(tx, &key, RESOLVE_UNKNOWN_SEND, now, |tx| {
                    let created = resolve_unknown_send(
                        tx,
                        &id,
                        resolution,
                        new_attempt_id.as_deref(),
                        undo_seconds,
                        now,
                    )?;
                    Ok(encode_resolution_receipt(
                        &id,
                        resolution,
                        created.as_deref(),
                    ))
                })?;
                Ok(receipt)
            })
            .await?;
        let (_, resolution, new_attempt_id) = resolution_from_receipt(&receipt)?;
        if resolution == UnknownResolution::SendAgain {
            // "Send again" queues a new attempt; the worker may be
            // waiting out a poll interval (spec/05 step 2).
            self.account_jobs().wake();
        }
        Ok(ResolvedUnknown {
            receipt,
            resolution,
            new_attempt_id,
        })
    }

    /// `confirm_send_route(attempt_id)` (spec/05 step 3, 07): a
    /// `queued` attempt adopts the account's current generation. An
    /// unsendable identity or any other state is `Conflict`; an unknown
    /// attempt is `NotFound`.
    pub async fn confirm_send_route(
        &self,
        idempotency_key: &str,
        attempt_id: &str,
    ) -> Result<Receipt, EngineError> {
        let id = attempt_id.to_owned();
        let receipt = self
            .attempt_command(
                idempotency_key,
                attempt_id,
                CONFIRM_SEND_ROUTE,
                move |tx, now| confirm_send_route(tx, &id, now),
            )
            .await?;
        // The attempt's generation now matches; the worker may be
        // waiting out a poll interval (spec/05 step 3).
        self.account_jobs().wake();
        Ok(receipt)
    }

    /// `retry_rejected_recipients(attempt_id)` (spec/05 step 5, 07):
    /// refused for a Proton attempt, which has no partial acceptance.
    /// An unknown attempt is `NotFound`.
    pub async fn retry_rejected_recipients(
        &self,
        idempotency_key: &str,
        attempt_id: &str,
    ) -> Result<Receipt, EngineError> {
        let id = attempt_id.to_owned();
        self.attempt_command(
            idempotency_key,
            attempt_id,
            RETRY_REJECTED_RECIPIENTS,
            move |tx, _| retry_rejected_recipients(tx, &id),
        )
        .await
    }

    /// `out_box()` (spec/07): every live attempt, then the last 30
    /// days' history, each newest first by update time, each row
    /// carrying the latest Proton evidence observed since launch
    /// (spec/05 step 6).
    pub async fn out_box(&self) -> Result<OutBox, EngineError> {
        let now = self.resources().now();
        let evidence = self.send_evidence.snapshot();
        let stored = self
            .read_through(move |resources| resources.read(move |tx| stored_out_box(tx, now)))
            .await?;
        let row = |row: sb_resources::OutBoxRow| {
            let observed = evidence.get(&row.attempt_id).cloned().unwrap_or_default();
            OutBoxRow::from_stored(row, observed)
        };
        Ok(OutBox {
            live: stored.live.into_iter().map(row).collect(),
            history: stored.history.into_iter().map(row).collect(),
        })
    }

    /// Run a counted attempt command under `command`.
    async fn attempt_command(
        &self,
        idempotency_key: &str,
        attempt_id: &str,
        command: &'static str,
        effect: impl FnOnce(&Transaction, Timestamp) -> Result<(), StoreError> + Send + 'static,
    ) -> Result<Receipt, EngineError> {
        validate_idempotency_key(idempotency_key)?;
        if attempt_id.is_empty() {
            return Err(EngineError::invalid_input(
                "attempt_id",
                "an attempt id is required",
            ));
        }
        let now = self.resources().now();
        let (key, id) = (idempotency_key.to_owned(), attempt_id.to_owned());
        let receipt = self
            .resources()
            .write(WritePriority::User, move |tx| {
                let receipt = run_once(tx, &key, command, now, |tx| {
                    effect(tx, now)?;
                    Ok(encode_receipt_id("attempt_id", &id))
                })?;
                Ok(receipt)
            })
            .await?;
        receipt_id(&receipt, "attempt_id")?;
        Ok(receipt)
    }
}

/// One Out Box row as the engine returns it (spec/07): the stored
/// attempt's fields, plus the Proton evidence observed since launch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutBoxRow {
    pub attempt_id: String,
    pub draft_id: String,
    pub account_id: String,
    pub identity_id: String,
    /// The sending address the attempt froze (`envelope_from`): the
    /// route's address when it was queued, not the identity's current one.
    pub envelope_from: String,
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
    /// whether a message with the attempt's `proton_draft_id` has
    /// appeared in the account (spec/05 "The Sent copy").
    pub sent_copy_appeared: bool,
    /// An accepted `server_saves` attempt with no Sent copy 24 h
    /// after acceptance: a soft warning, never a resend (spec/05).
    pub sent_copy_warning: bool,
    /// The Proton evidence observed since launch (spec/05 step 6,
    /// spec/07). Kept in the account worker's memory, so a relaunch
    /// shows none until its next check.
    pub evidence: Vec<String>,
}

impl OutBoxRow {
    /// Copy a stored row, attaching the evidence the worker observed.
    fn from_stored(row: sb_resources::OutBoxRow, evidence: Vec<String>) -> Self {
        Self {
            attempt_id: row.attempt_id,
            draft_id: row.draft_id,
            account_id: row.account_id,
            identity_id: row.identity_id,
            envelope_from: row.envelope_from,
            subject: row.subject,
            recipients: row.recipients,
            state: row.state,
            created_at: row.created_at,
            updated_at: row.updated_at,
            hold_until: row.hold_until,
            resolved_at: row.resolved_at,
            send_requested_at: row.send_requested_at,
            proton_draft_id: row.proton_draft_id,
            final_reply: row.final_reply,
            resolution: row.resolution,
            sent_copy: row.sent_copy,
            waits_for_confirmation: row.waits_for_confirmation,
            sent_copy_appeared: row.sent_copy_appeared,
            sent_copy_warning: row.sent_copy_warning,
            evidence,
        }
    }
}

/// The Out Box (spec/07): live attempts, then history, each newest
/// first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutBox {
    pub live: Vec<OutBoxRow>,
    pub history: Vec<OutBoxRow>,
}

/// The resolution receipt's body: the attempt, the resolution and,
/// for `send_again`, the new attempt's id.
fn encode_resolution_receipt(
    attempt_id: &str,
    resolution: UnknownResolution,
    new_attempt_id: Option<&str>,
) -> String {
    let mut body = serde_json::Map::new();
    body.insert("attempt_id".to_owned(), attempt_id.into());
    body.insert("resolution".to_owned(), resolution.as_str().into());
    if let Some(new_attempt_id) = new_attempt_id {
        body.insert("new_attempt_id".to_owned(), new_attempt_id.into());
    }
    serde_json::Value::Object(body).to_string()
}

/// Read a resolution receipt's answer (spec/07): a replayed key
/// answers the resolution it recorded, never the caller's argument.
/// Malformed stored JSON or a missing field is `Corrupt`.
fn resolution_from_receipt(
    receipt: &Receipt,
) -> Result<(String, UnknownResolution, Option<String>), EngineError> {
    let parsed: serde_json::Value = serde_json::from_str(receipt.result_json())
        .map_err(|_| EngineError::corrupt("a stored resolution receipt is not JSON"))?;
    let attempt_id = parsed["attempt_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .ok_or_else(|| EngineError::corrupt("a stored resolution receipt names no attempt"))?;
    let resolution = parsed["resolution"]
        .as_str()
        .ok_or_else(|| EngineError::corrupt("a stored resolution receipt names no resolution"))?;
    let resolution = UnknownResolution::from_stored(resolution)?;
    let new_attempt_id = parsed["new_attempt_id"]
        .as_str()
        .filter(|id| !id.is_empty())
        .map(str::to_owned);
    Ok((attempt_id.to_owned(), resolution, new_attempt_id))
}

/// The `sending.undo_seconds` setting's effective value (spec/02).
fn undo_seconds(settings: &[Setting]) -> Result<i64, EngineError> {
    let setting = settings
        .iter()
        .find(|setting| setting.key() == UNDO_SECONDS_KEY)
        .ok_or_else(|| EngineError::internal("the registry holds no undo-seconds setting"))?;
    setting
        .value()
        .parse::<i64>()
        .map_err(|_| EngineError::internal("the undo-seconds setting is not an integer"))
}

/// The revision's recipients as composition addresses.
fn addresses(recipients: &[sb_resources::DraftRecipient]) -> Vec<ComposeAddress<'_>> {
    recipients
        .iter()
        .map(|recipient| ComposeAddress {
            name: recipient.name.as_deref(),
            address: &recipient.address,
        })
        .collect()
}

/// Compose the revision's frozen MIME and publish it (spec/05): the
/// Message-ID's domain is the identity's address domain, the headers
/// carry no `Bcc`, and a reply or forward keeps its source's
/// `In-Reply-To` and `References`. Blocking object reads run on the
/// caller's blocking pool.
fn compose_and_publish(
    resources: &Resources,
    revision: &StoredRevision,
    identity: &ComposeIdentity,
    in_reply_to: Option<String>,
    references: Vec<String>,
    now: Timestamp,
) -> Result<(String, PublishedObject), EngineError> {
    let email = identity.email();
    let domain = email
        .rsplit_once('@')
        .map(|(_, domain)| domain)
        .filter(|domain| !domain.is_empty())
        .ok_or_else(|| {
            EngineError::invalid_input("identity_id", "the identity's address has no domain")
        })?;
    let message_id = format!("{}@{}", resources.new_uuid_string(), domain);
    let to = addresses(&revision.to);
    let cc = addresses(&revision.cc);
    let bcc = addresses(&revision.bcc);
    let mut data = Vec::with_capacity(revision.attachments.len());
    for reference in &revision.attachments {
        let id = sb_resources::ObjectId::from_hex(&reference.object_sha256, reference.size)?;
        data.push(resources.read_object(&id)?);
    }
    let attachments: Vec<ComposeAttachment> = revision
        .attachments
        .iter()
        .zip(&data)
        .map(|(reference, bytes)| ComposeAttachment {
            name: &reference.filename,
            mime_type: &reference.media_type,
            data: bytes,
            content_id: reference.content_id.as_deref(),
        })
        .collect();
    let references: Vec<&str> = references.iter().map(String::as_str).collect();
    let format = match revision.format {
        ComposeFormat::Plain => MimeComposeFormat::Plain {
            literal: !revision.plain_flowed,
        },
        ComposeFormat::Markdown => MimeComposeFormat::Markdown,
    };
    let input = ComposeInput {
        from: ComposeAddress {
            name: Some(identity.display_name()),
            address: email,
        },
        to: &to,
        cc: &cc,
        bcc: &bcc,
        reply_to: identity.reply_to().map(|address| ComposeAddress {
            name: None,
            address,
        }),
        subject: &revision.subject,
        body: &revision.body_source,
        format,
        attachments: &attachments,
        message_id: &message_id,
        date: now.as_millis() / 1000,
        in_reply_to: in_reply_to.as_deref(),
        references: &references,
        user_agent: USER_AGENT,
    };
    let composed = compose(&input).map_err(compose_error)?;
    let object = resources.publish_object(composed.bytes)?;
    Ok((message_id, object))
}

/// A composition refusal names the field the caller can fix: the
/// attachment limits or the draft's content.
fn compose_error(error: ComposeError) -> EngineError {
    let field = match error {
        ComposeError::TooLarge { .. } | ComposeError::TooManyFiles { .. } => "attachments",
        ComposeError::InvalidInput(_) => "draft",
    };
    EngineError::invalid_input(field, error.to_string())
}

/// The route `send_draft` sends a revision through: the catalog
/// identity and the account the revision recorded (spec/07).
fn identity_route(identity: &ComposeIdentity, account_id: &str) -> IdentityRoute {
    IdentityRoute {
        identity_id: identity.identity_id().to_owned(),
        account_id: account_id.to_owned(),
        email: identity.email().to_owned(),
        is_default: identity.is_default(),
        sendable: identity.sendable(),
        account_kind: identity.account_kind(),
    }
}

#[cfg(test)]
mod tests;
