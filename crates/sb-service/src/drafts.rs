//! `Core`'s drafts surface (spec/02 "Drafts", spec/05 "Composing",
//! spec/07 "Commands", "Other queries"): the `create_draft`,
//! `save_draft` and `discard_draft` commands — one counted command
//! each under an idempotency key, so a repeated key returns the stored
//! receipt — the `drafts()`/`draft()` queries, and attachment
//! publication. The DTOs are service-owned mirrors of the engine's
//! records — nothing from below crosses into L1 (spec/01 rule 3) — so
//! the methods convert field by field. A published attachment's
//! reservation stays inside the service's [`DraftAttachmentObject`]
//! until a save spends it.

use std::sync::Arc;

use crate::{Core, CoreError, Receipt};

/// `create_draft`'s `context` (spec/07): `New` or the source
/// occurrence a reply, reply-all or forward answers. A forward
/// resolves to the logical message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftSource {
    New,
    Reply { occurrence_id: i64 },
    ReplyAll { occurrence_id: i64 },
    Forward { occurrence_id: i64 },
}

impl DraftSource {
    fn into_engine(self) -> sb_engine::DraftSource {
        match self {
            Self::New => sb_engine::DraftSource::New,
            Self::Reply { occurrence_id } => sb_engine::DraftSource::Reply { occurrence_id },
            Self::ReplyAll { occurrence_id } => sb_engine::DraftSource::ReplyAll { occurrence_id },
            Self::Forward { occurrence_id } => sb_engine::DraftSource::Forward { occurrence_id },
        }
    }
}

/// `compose_format`'s words (spec/02, spec/05 "Formats").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposeFormat {
    Plain,
    Markdown,
}

impl ComposeFormat {
    fn into_engine(self) -> sb_engine::ComposeFormat {
        match self {
            Self::Plain => sb_engine::ComposeFormat::Plain,
            Self::Markdown => sb_engine::ComposeFormat::Markdown,
        }
    }

    fn from_engine(format: sb_engine::ComposeFormat) -> Self {
        match format {
            sb_engine::ComposeFormat::Plain => Self::Plain,
            sb_engine::ComposeFormat::Markdown => Self::Markdown,
        }
    }
}

/// The spec/02 draft state names, read back.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftState {
    Editing,
    Queued,
    Sent,
    Discarded,
}

impl DraftState {
    fn from_engine(state: sb_engine::DraftState) -> Self {
        match state {
            sb_engine::DraftState::Editing => Self::Editing,
            sb_engine::DraftState::Queued => Self::Queued,
            sb_engine::DraftState::Sent => Self::Sent,
            sb_engine::DraftState::Discarded => Self::Discarded,
        }
    }
}

/// A send attempt's state word (spec/02 `send_attempts`). A draft
/// reports only a live one (spec/05).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttemptState {
    Held,
    Queued,
    Submitting,
    Accepted,
    Failed,
    OutcomeUnknown,
    Cancelled,
}

impl AttemptState {
    fn from_engine(state: sb_engine::AttemptState) -> Self {
        match state {
            sb_engine::AttemptState::Held => Self::Held,
            sb_engine::AttemptState::Queued => Self::Queued,
            sb_engine::AttemptState::Submitting => Self::Submitting,
            sb_engine::AttemptState::Accepted => Self::Accepted,
            sb_engine::AttemptState::Failed => Self::Failed,
            sb_engine::AttemptState::OutcomeUnknown => Self::OutcomeUnknown,
            sb_engine::AttemptState::Cancelled => Self::Cancelled,
        }
    }
}

/// One address slot on a revision, the spec/02 `{address, name}`
/// shape; `name` is `None` when the address stands alone.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftRecipient {
    pub address: String,
    pub name: Option<String>,
}

impl DraftRecipient {
    fn into_engine(self) -> sb_engine::DraftRecipient {
        sb_engine::DraftRecipient {
            address: self.address,
            name: self.name,
        }
    }

    fn from_engine(recipient: sb_engine::DraftRecipient) -> Self {
        Self {
            address: recipient.address,
            name: recipient.name,
        }
    }
}

/// One stored attachment reference (spec/02 `attachments_json`): a
/// published object's identity and size.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftAttachmentRef {
    pub object_sha256: String,
    pub filename: String,
    pub media_type: String,
    pub size: u64,
}

impl DraftAttachmentRef {
    fn from_engine(reference: sb_engine::AttachmentRef) -> Self {
        Self {
            object_sha256: reference.object_sha256,
            filename: reference.filename,
            media_type: reference.media_type,
            size: reference.size,
        }
    }
}

/// A live send attempt as a draft reports it (spec/07 "any live
/// attempt's state").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveAttempt {
    pub attempt_id: String,
    pub state: AttemptState,
}

impl LiveAttempt {
    fn from_engine(attempt: &sb_engine::LiveAttempt) -> Self {
        Self {
            attempt_id: attempt.attempt_id.clone(),
            state: AttemptState::from_engine(attempt.state),
        }
    }
}

/// The current revision's full content, as `draft()` returns it
/// (spec/07).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftRevision {
    pub revision_id: String,
    pub seq: i64,
    /// The revision this one wrote over (`None` for seq 1).
    pub base_revision_id: Option<String>,
    /// The route snapshot at save (spec/02); `None` on a draft with
    /// no sending identity.
    pub account_id: Option<String>,
    pub identity_id: Option<String>,
    pub format: ComposeFormat,
    /// `false` is the per-draft literal plain-text option (spec/05).
    pub plain_flowed: bool,
    pub subject: String,
    pub to: Vec<DraftRecipient>,
    pub cc: Vec<DraftRecipient>,
    pub bcc: Vec<DraftRecipient>,
    /// Markdown or plain source as composed (spec/05).
    pub body_source: String,
    pub attachments: Vec<DraftAttachmentRef>,
    /// When the revision committed, milliseconds since the epoch.
    pub created_at_ms: i64,
}

impl DraftRevision {
    fn from_engine(revision: &sb_engine::StoredRevision) -> Self {
        Self {
            revision_id: revision.revision_id.clone(),
            seq: revision.seq,
            base_revision_id: revision.base_revision_id.clone(),
            account_id: revision.account_id.clone(),
            identity_id: revision.identity_id.clone(),
            format: ComposeFormat::from_engine(revision.format),
            plain_flowed: revision.plain_flowed,
            subject: revision.subject.clone(),
            to: revision
                .to
                .iter()
                .cloned()
                .map(DraftRecipient::from_engine)
                .collect(),
            cc: revision
                .cc
                .iter()
                .cloned()
                .map(DraftRecipient::from_engine)
                .collect(),
            bcc: revision
                .bcc
                .iter()
                .cloned()
                .map(DraftRecipient::from_engine)
                .collect(),
            body_source: revision.body_source.clone(),
            attachments: revision
                .attachments
                .iter()
                .cloned()
                .map(DraftAttachmentRef::from_engine)
                .collect(),
            created_at_ms: revision.created_at.as_millis(),
        }
    }
}

/// One draft with its current revision, as `draft(draft_id)` returns
/// it (spec/07) — a compose window restores against it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    pub draft_id: String,
    pub state: DraftState,
    /// `Some` when created as a reply or reply-all (spec/07 `context`).
    pub reply_to_occurrence_id: Option<i64>,
    /// `Some` when created as a forward.
    pub forward_of_message_id: Option<i64>,
    /// The current revision; `None` until the first save.
    pub revision: Option<DraftRevision>,
    /// Any live send attempt (spec/07).
    pub attempt: Option<LiveAttempt>,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
}

impl Draft {
    fn from_engine(draft: &sb_engine::StoredDraft) -> Self {
        Self {
            draft_id: draft.draft_id.clone(),
            state: DraftState::from_engine(draft.state),
            reply_to_occurrence_id: draft.reply_to_occurrence_id,
            forward_of_message_id: draft.forward_of_message_id,
            revision: draft.revision.as_ref().map(DraftRevision::from_engine),
            attempt: draft.attempt.as_ref().map(LiveAttempt::from_engine),
            created_at_ms: draft.created_at.as_millis(),
            updated_at_ms: draft.updated_at.as_millis(),
        }
    }
}

/// One `drafts()` row (spec/07): state, route, subject, recipients,
/// update time and any live attempt's state — the sidebar's Drafts
/// (local) shape.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftSummary {
    pub draft_id: String,
    pub state: DraftState,
    /// The current revision's route; `None` while the draft names no
    /// sending identity (spec/02).
    pub account_id: Option<String>,
    pub identity_id: Option<String>,
    pub subject: String,
    pub to: Vec<DraftRecipient>,
    pub cc: Vec<DraftRecipient>,
    pub bcc: Vec<DraftRecipient>,
    pub updated_at_ms: i64,
    pub attempt: Option<LiveAttempt>,
}

impl DraftSummary {
    fn from_engine(summary: sb_engine::DraftSummary) -> Self {
        let recipients = |list: Vec<sb_engine::DraftRecipient>| {
            list.into_iter().map(DraftRecipient::from_engine).collect()
        };
        Self {
            draft_id: summary.draft_id,
            state: DraftState::from_engine(summary.state),
            account_id: summary.account_id,
            identity_id: summary.identity_id,
            subject: summary.subject,
            to: recipients(summary.to),
            cc: recipients(summary.cc),
            bcc: recipients(summary.bcc),
            updated_at_ms: summary.updated_at.as_millis(),
            attempt: summary.attempt.as_ref().map(LiveAttempt::from_engine),
        }
    }
}

/// What `create_draft` answers: its receipt and the draft id the
/// receipt names — the stored id, so a replayed key still names the
/// first command's draft.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftCreated {
    pub receipt: Receipt,
    pub draft_id: String,
}

/// What `save_draft` answers: its receipt and the revision id the
/// receipt names — the stored id on a replayed key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DraftSaved {
    pub receipt: Receipt,
    pub revision_id: String,
}

/// A published draft attachment (spec/02: attachments are published
/// to the object store before the revision that references them):
/// the object identity the shell shows, wrapped around the
/// publication reservation a `save_draft` spends. Not a record — the
/// reservation inside keeps the object pinned until the save's
/// transaction consumes it. Clones share that one reservation so an
/// identical request can be retried; a fresh command cannot spend it
/// twice. This opaque descriptor can survive a close and reopen in
/// the same process, but cannot be reconstructed in another process.
#[derive(Debug, Clone)]
pub struct DraftAttachmentObject {
    /// The published object's sha256, 64 lowercase hex.
    pub object_sha256: String,
    /// The published object's length in bytes.
    pub byte_length: u64,
    object: Arc<sb_engine::PublishedObject>,
}

/// A newly published object, or an unchanged reference from the base revision.
#[derive(Debug, Clone)]
pub enum DraftAttachment {
    New {
        object: DraftAttachmentObject,
        filename: String,
        media_type: String,
    },
    Retained(DraftAttachmentRef),
}

/// One save's content (spec/07 `save_draft`'s `content`): everything
/// a revision records but its ids — the engine assigns the revision
/// id and checks the base.
#[derive(Debug, Clone)]
pub struct DraftContent {
    /// The sending identity the route snapshots (`None` keeps the
    /// revision unrouted: a draft with no sending identity, spec/02).
    pub identity_id: Option<String>,
    pub format: ComposeFormat,
    /// `false` is the per-draft literal plain-text option (spec/05).
    pub plain_flowed: bool,
    pub subject: String,
    pub to: Vec<DraftRecipient>,
    pub cc: Vec<DraftRecipient>,
    pub bcc: Vec<DraftRecipient>,
    /// Markdown or plain source as composed (spec/05); the outgoing
    /// MIME is built from it at send.
    pub body_source: String,
    /// The published objects this revision references.
    pub attachments: Vec<DraftAttachment>,
}

impl DraftContent {
    fn into_engine(self) -> sb_engine::DraftContent {
        sb_engine::DraftContent {
            identity_id: self.identity_id,
            format: self.format.into_engine(),
            plain_flowed: self.plain_flowed,
            subject: self.subject,
            to: self
                .to
                .into_iter()
                .map(DraftRecipient::into_engine)
                .collect(),
            cc: self
                .cc
                .into_iter()
                .map(DraftRecipient::into_engine)
                .collect(),
            bcc: self
                .bcc
                .into_iter()
                .map(DraftRecipient::into_engine)
                .collect(),
            body_source: self.body_source,
            attachments: self
                .attachments
                .into_iter()
                .map(|attachment| match attachment {
                    DraftAttachment::New {
                        object,
                        filename,
                        media_type,
                    } => sb_engine::DraftAttachment::New {
                        object: object.object,
                        filename,
                        media_type,
                    },
                    DraftAttachment::Retained(reference) => {
                        sb_engine::DraftAttachment::Retained(sb_engine::AttachmentRef {
                            object_sha256: reference.object_sha256,
                            filename: reference.filename,
                            media_type: reference.media_type,
                            size: reference.size,
                        })
                    }
                })
                .collect(),
        }
    }
}

impl Core {
    /// `create_draft(context)` (spec/07 "Commands"): a new `editing`
    /// draft under an idempotency key — a repeated key returns the
    /// stored receipt and its draft id. A reply, reply-all or forward
    /// `context`'s source occurrence must exist (`NotFound`); a
    /// forward resolves to the logical message.
    pub async fn create_draft(
        &self,
        idempotency_key: &str,
        context: DraftSource,
    ) -> Result<DraftCreated, CoreError> {
        let created = self
            .engine()
            .create_draft(idempotency_key, context.into_engine())
            .await?;
        Ok(DraftCreated {
            receipt: Receipt::from_engine(created.receipt()),
            draft_id: created.draft_id().to_owned(),
        })
    }

    /// `save_draft(draft_id, base_revision_id, content)` (spec/07
    /// "Commands"): append an immutable revision and make it the
    /// draft's current one, in the transaction that also spends each
    /// attachment's publication reservation. `base_revision_id` must
    /// name the current revision (`None` before the first save);
    /// anything else is `Conflict`. Retained attachments must exactly
    /// match references in that base revision; new attachments spend
    /// their publication reservations. An unknown draft or identity is
    /// `NotFound`; a `sent` or `discarded` draft refuses the same way
    /// as a wrong base. The receipt answers `{"revision_id": ...}`.
    pub async fn save_draft(
        &self,
        idempotency_key: &str,
        draft_id: &str,
        base_revision_id: Option<&str>,
        content: DraftContent,
    ) -> Result<DraftSaved, CoreError> {
        let saved = self
            .engine()
            .save_draft(
                idempotency_key,
                draft_id,
                base_revision_id,
                content.into_engine(),
            )
            .await?;
        Ok(DraftSaved {
            receipt: Receipt::from_engine(saved.receipt()),
            revision_id: saved.revision_id().to_owned(),
        })
    }

    /// `discard_draft(id)` (spec/07 "Commands", spec/05): refuse
    /// `Conflict` while a live attempt exists or the draft already
    /// sits at a terminal state; otherwise mark it `discarded`,
    /// keeping every revision. The receipt answers
    /// `{"draft_id": ...}`.
    pub async fn discard_draft(
        &self,
        idempotency_key: &str,
        draft_id: &str,
    ) -> Result<Receipt, CoreError> {
        let receipt = self
            .engine()
            .discard_draft(idempotency_key, draft_id)
            .await?;
        Ok(Receipt::from_engine(&receipt))
    }

    /// `drafts()` (spec/07 "Other queries"): every `editing` or
    /// `queued` draft, newest update first — the sidebar's Drafts
    /// (local) rows.
    pub async fn drafts(&self) -> Result<Vec<DraftSummary>, CoreError> {
        Ok(self
            .engine()
            .drafts()
            .await?
            .into_iter()
            .map(DraftSummary::from_engine)
            .collect())
    }

    /// `draft(draft_id)` (spec/07 "Other queries"): the draft with
    /// its current revision's full content and any live attempt, in
    /// any state — a compose window restores against it. An unknown
    /// id is `NotFound`.
    pub async fn draft(&self, draft_id: &str) -> Result<Draft, CoreError> {
        Ok(Draft::from_engine(&self.engine().draft(draft_id).await?))
    }

    /// Publish one attachment's bytes for a later save (spec/02:
    /// attachments are published to the object store before the
    /// revision that references them). The returned
    /// [`DraftAttachmentObject`]'s reservation pins the object until
    /// the save's transaction spends it. Blocking file I/O on the
    /// engine's blocking pool, never inside a write transaction.
    pub async fn publish_draft_attachment(
        &self,
        bytes: impl std::io::Read + Send + 'static,
    ) -> Result<DraftAttachmentObject, CoreError> {
        let object = self.engine().publish_draft_attachment(bytes).await?;
        Ok(DraftAttachmentObject {
            object_sha256: object.id().sha256_hex(),
            byte_length: object.id().byte_length(),
            object: Arc::new(object),
        })
    }
}
