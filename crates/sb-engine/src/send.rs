//! Sending (spec/05 "Outbox state machine", spec/07 "Commands"): the
//! `send_draft` and `cancel_send` commands. `send_draft` composes the
//! frozen MIME outside any transaction, publishes it, then queues the
//! `held` attempt and its recipients in one store transaction;
//! `cancel_send` moves a `held` or `queued` attempt to `cancelled`.
//! The cross-account acknowledgement is M4.3b's check.

use std::sync::Arc;

use sb_resources::{
    AccountKind, ComposeAddress, ComposeAttachment, ComposeError, ComposeFormat, ComposeIdentity,
    ComposeInput, DraftState, MimeComposeFormat, PublishedObject, Receipt, Resources, SendAttempt,
    StoredRevision, Timestamp, WritePriority, cancel_send, compose, compose_catalog, draft, replay,
    run_once, send_draft,
};

use crate::drafts::{encode_receipt_id, receipt_id};
use crate::engine::validate_idempotency_key;
use crate::settings::{Setting, UNDO_SECONDS_KEY};
use crate::{Engine, EngineError};

/// The commands' names in `command_receipts` (spec/07).
const SEND_DRAFT: &str = "send_draft";
const CANCEL_SEND: &str = "cancel_send";

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

impl Engine {
    /// `send_draft(draft_id, revision_id, acks)` (spec/05, 07): queue
    /// the draft's current revision as a `held` attempt. `revision_id`
    /// must be the draft's current revision and the draft `editing`,
    /// else `Conflict`; a revision with a live attempt, or one already
    /// `accepted` or resolved `marked_sent`, is `Conflict` too. The
    /// frozen MIME is composed and published first; the store
    /// transaction then re-checks and inserts the attempt, its
    /// recipients and the draft's `queued` state, spending the
    /// publication's reservation. `acks` is accepted and carried for
    /// M4.3b's acknowledgement check; none is checked here.
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
        let _ = acks;
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
        let account_id = revision.account_id.clone().ok_or_else(|| {
            EngineError::internal("the revision's route disappeared while sending")
        })?;
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
        let receipt = self
            .resources()
            .write(WritePriority::User, move |tx| {
                let receipt = run_once(tx, &key, SEND_DRAFT, now, |tx| {
                    send_draft(tx, &attempt, &object, undo_seconds, now)?;
                    Ok(encode_receipt_id("attempt_id", &attempt.attempt_id))
                })?;
                Ok(receipt)
            })
            .await?;
        Ok(SendQueued {
            attempt_id: receipt_id(&receipt, "attempt_id")?,
            receipt,
        })
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
            content_id: None,
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

#[cfg(test)]
mod tests;
