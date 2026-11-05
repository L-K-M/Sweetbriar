//! Draft queries over FFI.

use crate::{CancelToken, Core, CoreError};
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
}

#[cfg(test)]
mod tests;
