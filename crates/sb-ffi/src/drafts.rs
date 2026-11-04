//! Draft queries and commands over FFI (spec/07 "Commands", "Other
//! queries"): the service-owned draft DTOs cross as this crate's own
//! records, field by field, and `create_draft` and `discard_draft`
//! each take the token and key the shell made. Attachment bytes
//! publish through the opaque [`DraftAttachmentObject`] handle, whose
//! reservation a save (M1.9b2b3) spends.

use crate::api::Receipt;
use crate::{CancelToken, Core, CoreError};
use std::io::Cursor;
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ComposeFormat {
    Plain,
    Markdown,
}

impl From<sb_service::ComposeFormat> for ComposeFormat {
    fn from(value: sb_service::ComposeFormat) -> Self {
        match value {
            sb_service::ComposeFormat::Plain => Self::Plain,
            sb_service::ComposeFormat::Markdown => Self::Markdown,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum DraftState {
    Editing,
    Queued,
    Sent,
    Discarded,
}

impl From<sb_service::DraftState> for DraftState {
    fn from(value: sb_service::DraftState) -> Self {
        match value {
            sb_service::DraftState::Editing => Self::Editing,
            sb_service::DraftState::Queued => Self::Queued,
            sb_service::DraftState::Sent => Self::Sent,
            sb_service::DraftState::Discarded => Self::Discarded,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum AttemptState {
    Held,
    Queued,
    Submitting,
    Accepted,
    Failed,
    OutcomeUnknown,
    Cancelled,
}

impl From<sb_service::AttemptState> for AttemptState {
    fn from(value: sb_service::AttemptState) -> Self {
        match value {
            sb_service::AttemptState::Held => Self::Held,
            sb_service::AttemptState::Queued => Self::Queued,
            sb_service::AttemptState::Submitting => Self::Submitting,
            sb_service::AttemptState::Accepted => Self::Accepted,
            sb_service::AttemptState::Failed => Self::Failed,
            sb_service::AttemptState::OutcomeUnknown => Self::OutcomeUnknown,
            sb_service::AttemptState::Cancelled => Self::Cancelled,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DraftRecipient {
    pub address: String,
    pub name: Option<String>,
}
impl From<sb_service::DraftRecipient> for DraftRecipient {
    fn from(value: sb_service::DraftRecipient) -> Self {
        Self {
            address: value.address,
            name: value.name,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DraftAttachmentRef {
    pub object_sha256: String,
    pub filename: String,
    pub media_type: String,
    pub size: u64,
}
impl From<sb_service::DraftAttachmentRef> for DraftAttachmentRef {
    fn from(value: sb_service::DraftAttachmentRef) -> Self {
        Self {
            object_sha256: value.object_sha256,
            filename: value.filename,
            media_type: value.media_type,
            size: value.size,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct LiveAttempt {
    pub attempt_id: String,
    pub state: AttemptState,
}
impl From<sb_service::LiveAttempt> for LiveAttempt {
    fn from(value: sb_service::LiveAttempt) -> Self {
        Self {
            attempt_id: value.attempt_id,
            state: value.state.into(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DraftRevision {
    pub revision_id: String,
    pub seq: i64,
    pub base_revision_id: Option<String>,
    pub account_id: Option<String>,
    pub identity_id: Option<String>,
    pub format: ComposeFormat,
    pub plain_flowed: bool,
    pub subject: String,
    pub to: Vec<DraftRecipient>,
    pub cc: Vec<DraftRecipient>,
    pub bcc: Vec<DraftRecipient>,
    pub body_source: String,
    pub attachments: Vec<DraftAttachmentRef>,
    pub created_at_ms: i64,
}
impl From<sb_service::DraftRevision> for DraftRevision {
    fn from(value: sb_service::DraftRevision) -> Self {
        Self {
            revision_id: value.revision_id,
            seq: value.seq,
            base_revision_id: value.base_revision_id,
            account_id: value.account_id,
            identity_id: value.identity_id,
            format: value.format.into(),
            plain_flowed: value.plain_flowed,
            subject: value.subject,
            to: value.to.into_iter().map(Into::into).collect(),
            cc: value.cc.into_iter().map(Into::into).collect(),
            bcc: value.bcc.into_iter().map(Into::into).collect(),
            body_source: value.body_source,
            attachments: value.attachments.into_iter().map(Into::into).collect(),
            created_at_ms: value.created_at_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Draft {
    pub draft_id: String,
    pub state: DraftState,
    pub reply_to_occurrence_id: Option<i64>,
    pub forward_of_message_id: Option<i64>,
    pub revision: Option<DraftRevision>,
    pub attempt: Option<LiveAttempt>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}
impl From<sb_service::Draft> for Draft {
    fn from(value: sb_service::Draft) -> Self {
        Self {
            draft_id: value.draft_id,
            state: value.state.into(),
            reply_to_occurrence_id: value.reply_to_occurrence_id,
            forward_of_message_id: value.forward_of_message_id,
            revision: value.revision.map(Into::into),
            attempt: value.attempt.map(Into::into),
            created_at_ms: value.created_at_ms,
            updated_at_ms: value.updated_at_ms,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DraftSummary {
    pub draft_id: String,
    pub state: DraftState,
    pub account_id: Option<String>,
    pub identity_id: Option<String>,
    pub subject: String,
    pub to: Vec<DraftRecipient>,
    pub cc: Vec<DraftRecipient>,
    pub bcc: Vec<DraftRecipient>,
    pub updated_at_ms: i64,
    pub attempt: Option<LiveAttempt>,
}
impl From<sb_service::DraftSummary> for DraftSummary {
    fn from(value: sb_service::DraftSummary) -> Self {
        Self {
            draft_id: value.draft_id,
            state: value.state.into(),
            account_id: value.account_id,
            identity_id: value.identity_id,
            subject: value.subject,
            to: value.to.into_iter().map(Into::into).collect(),
            cc: value.cc.into_iter().map(Into::into).collect(),
            bcc: value.bcc.into_iter().map(Into::into).collect(),
            updated_at_ms: value.updated_at_ms,
            attempt: value.attempt.map(Into::into),
        }
    }
}

/// `create_draft`'s `context` (spec/07): `New`, or the source
/// occurrence a reply, reply-all or forward answers. The service
/// resolves a forward to its logical message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum DraftSource {
    New,
    Reply { occurrence_id: i64 },
    ReplyAll { occurrence_id: i64 },
    Forward { occurrence_id: i64 },
}

impl From<DraftSource> for sb_service::DraftSource {
    fn from(value: DraftSource) -> Self {
        match value {
            DraftSource::New => Self::New,
            DraftSource::Reply { occurrence_id } => Self::Reply { occurrence_id },
            DraftSource::ReplyAll { occurrence_id } => Self::ReplyAll { occurrence_id },
            DraftSource::Forward { occurrence_id } => Self::Forward { occurrence_id },
        }
    }
}

/// What `create_draft` answers: its receipt and the draft id the
/// receipt names — the stored id on a replayed key.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DraftCreated {
    pub receipt: Receipt,
    pub draft_id: String,
}

impl From<sb_service::DraftCreated> for DraftCreated {
    fn from(value: sb_service::DraftCreated) -> Self {
        Self {
            receipt: Receipt::from_service(&value.receipt),
            draft_id: value.draft_id,
        }
    }
}

/// One attachment publication's input (spec/02 "Drafts", spec/08
/// "Attachments"): the exact bytes to publish durably under the data
/// root before the revision that references them. Only bytes cross —
/// the caller names no object, so the published identity cannot be
/// forged.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct DraftAttachmentPublish {
    /// The attachment's bytes, as the shell read them.
    pub bytes: Vec<u8>,
}

/// A published draft attachment (spec/02): the opaque handle a later
/// save spends. It wraps the service's publication and keeps its
/// one-shot reservation, so a caller cannot rebuild one from an
/// identity — only [`Core::publish_draft_attachment`] makes one, and
/// the getters report only what was published.
#[derive(Debug, uniffi::Object)]
pub struct DraftAttachmentObject {
    /// The service-owned publication and the reservation a save
    /// spends; no engine or store type crosses.
    inner: sb_service::DraftAttachmentObject,
}

#[uniffi::export]
impl DraftAttachmentObject {
    /// The published object's SHA-256: 64 lowercase hex digits.
    pub fn object_sha256(&self) -> String {
        self.inner.object_sha256.clone()
    }

    /// The published object's length in bytes.
    pub fn byte_length(&self) -> u64 {
        self.inner.byte_length
    }
}

#[uniffi::export]
impl Core {
    /// List local drafts, newest first.
    pub async fn drafts(&self, token: Arc<CancelToken>) -> Result<Vec<DraftSummary>, CoreError> {
        Ok(self
            .query(&token, async |core| core.drafts().await)
            .await?
            .into_iter()
            .map(Into::into)
            .collect())
    }
    /// Load a draft and its current revision.
    pub async fn draft(
        &self,
        token: Arc<CancelToken>,
        draft_id: String,
    ) -> Result<Draft, CoreError> {
        Ok(self
            .query(&token, async |core| core.draft(&draft_id).await)
            .await?
            .into())
    }

    /// `create_draft(context)` (spec/07 "Commands"): a new `editing`
    /// draft under an idempotency key the shell made (`UUID()` in
    /// Swift) — a repeated key returns the stored receipt and its
    /// draft id. A reply, reply-all or forward context's source
    /// occurrence must exist (`NotFound`); a forward resolves to the
    /// logical message. Like every command it honours the token only
    /// until it holds the core; once begun it runs to completion.
    pub async fn create_draft(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        context: DraftSource,
    ) -> Result<DraftCreated, CoreError> {
        let created = self
            .command(&token, async |core| {
                core.create_draft(&idempotency_key, context.into()).await
            })
            .await?;
        Ok(DraftCreated::from(created))
    }

    /// `discard_draft(id)` (spec/07 "Commands", spec/05): refuse
    /// `Conflict` while a live attempt exists or the draft already
    /// sits at a terminal state; otherwise mark it `discarded`,
    /// keeping every revision. The receipt answers
    /// `{"draft_id": ...}`.
    pub async fn discard_draft(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        draft_id: String,
    ) -> Result<Receipt, CoreError> {
        let receipt = self
            .command(&token, async |core| {
                core.discard_draft(&idempotency_key, &draft_id).await
            })
            .await?;
        Ok(Receipt::from_service(&receipt))
    }

    /// Publish one attachment's bytes for a later save (spec/02:
    /// attachments are published to the object store before the
    /// revision that references them). The bytes cross as
    /// [`DraftAttachmentPublish`]'s `bytes`; the returned handle
    /// reports the published object's identity and length and holds
    /// the reservation a save spends. Like every command it honours
    /// the token only until it holds the core: once begun it runs to
    /// completion and publishes durably, so a cancel or a dropped
    /// caller cannot strand a half-written object.
    pub async fn publish_draft_attachment(
        &self,
        token: Arc<CancelToken>,
        input: DraftAttachmentPublish,
    ) -> Result<Arc<DraftAttachmentObject>, CoreError> {
        let object = self
            .command(&token, async |core| {
                core.publish_draft_attachment(Cursor::new(input.bytes))
                    .await
            })
            .await?;
        Ok(Arc::new(DraftAttachmentObject { inner: object }))
    }
}

#[cfg(test)]
mod tests;
