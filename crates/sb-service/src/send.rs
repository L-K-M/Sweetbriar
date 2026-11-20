//! `Core`'s sending surface (spec/05 "Outbox state machine", spec/07
//! "Commands"): the `send_draft`, `cancel_send`,
//! `resolve_unknown_send`, `confirm_send_route` and
//! `retry_rejected_recipients` commands as service-owned DTOs. No
//! engine or store type crosses into L1 (spec/01 rule 3).

use crate::{AttemptState, Core, CoreError, Receipt};

/// The `sent_copy` column's words (spec/02 "Sending"): how the
/// account's Sent copy is produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SentCopy {
    ServerSaves,
    Pending,
    Filed,
    Failed,
}

impl SentCopy {
    fn from_engine(sent_copy: sb_engine::SentCopy) -> Self {
        match sent_copy {
            sb_engine::SentCopy::ServerSaves => Self::ServerSaves,
            sb_engine::SentCopy::Pending => Self::Pending,
            sb_engine::SentCopy::Filed => Self::Filed,
            sb_engine::SentCopy::Failed => Self::Failed,
        }
    }
}

/// A recipient's address field (spec/02 `send_recipients.kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecipientKind {
    To,
    Cc,
    Bcc,
}

impl RecipientKind {
    fn from_engine(kind: sb_engine::RecipientKind) -> Self {
        match kind {
            sb_engine::RecipientKind::To => Self::To,
            sb_engine::RecipientKind::Cc => Self::Cc,
            sb_engine::RecipientKind::Bcc => Self::Bcc,
        }
    }
}

/// A recipient's stored state (spec/02 `send_recipients.state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    fn from_engine(state: sb_engine::RecipientState) -> Self {
        match state {
            sb_engine::RecipientState::Pending => Self::Pending,
            sb_engine::RecipientState::RcptAccepted => Self::RcptAccepted,
            sb_engine::RecipientState::RejectedPermanent => Self::RejectedPermanent,
            sb_engine::RecipientState::RejectedTransient => Self::RejectedTransient,
            sb_engine::RecipientState::Sent => Self::Sent,
            sb_engine::RecipientState::Unsent => Self::Unsent,
            sb_engine::RecipientState::Unknown => Self::Unknown,
        }
    }
}

/// One recipient on an Out Box row (spec/07): the address as first
/// written, its field and its stored state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutBoxRecipient {
    pub address: String,
    pub kind: RecipientKind,
    pub state: RecipientState,
}

impl OutBoxRecipient {
    fn from_engine(recipient: sb_engine::OutBoxRecipient) -> Self {
        Self {
            address: recipient.address,
            kind: RecipientKind::from_engine(recipient.kind),
            state: RecipientState::from_engine(recipient.state),
        }
    }
}

/// One Out Box row (spec/07 `out_box()`): the attempt's frozen
/// revision subject, recipients, state and stored times.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct OutBoxRow {
    pub attempt_id: String,
    pub draft_id: String,
    pub account_id: String,
    pub identity_id: String,
    pub subject: String,
    pub recipients: Vec<OutBoxRecipient>,
    pub state: AttemptState,
    pub created_at_ms: i64,
    pub updated_at_ms: i64,
    pub hold_until_ms: Option<i64>,
    pub resolved_at_ms: Option<i64>,
    pub send_requested_at_ms: Option<i64>,
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
    /// shows none until its next check. M4.3f fills this from those
    /// observations; it is empty until then.
    pub evidence: Vec<String>,
}

impl OutBoxRow {
    fn from_engine(row: sb_engine::OutBoxRow) -> Self {
        Self {
            attempt_id: row.attempt_id,
            draft_id: row.draft_id,
            account_id: row.account_id,
            identity_id: row.identity_id,
            subject: row.subject,
            recipients: row
                .recipients
                .into_iter()
                .map(OutBoxRecipient::from_engine)
                .collect(),
            state: AttemptState::from_engine(row.state),
            created_at_ms: row.created_at.as_millis(),
            updated_at_ms: row.updated_at.as_millis(),
            hold_until_ms: row.hold_until.map(|time| time.as_millis()),
            resolved_at_ms: row.resolved_at.map(|time| time.as_millis()),
            send_requested_at_ms: row.send_requested_at.map(|time| time.as_millis()),
            proton_draft_id: row.proton_draft_id,
            final_reply: row.final_reply,
            resolution: row.resolution.map(UnknownResolution::from_engine),
            sent_copy: SentCopy::from_engine(row.sent_copy),
            waits_for_confirmation: row.waits_for_confirmation,
            sent_copy_appeared: row.sent_copy_appeared,
            sent_copy_warning: row.sent_copy_warning,
            evidence: row.evidence,
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

impl OutBox {
    fn from_engine(out_box: sb_engine::OutBox) -> Self {
        Self {
            live: out_box
                .live
                .into_iter()
                .map(OutBoxRow::from_engine)
                .collect(),
            history: out_box
                .history
                .into_iter()
                .map(OutBoxRow::from_engine)
                .collect(),
        }
    }
}

/// What `send_draft` answers: its receipt and the attempt id the
/// receipt names, so a replayed key still names the first attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendQueued {
    pub receipt: Receipt,
    pub attempt_id: String,
}

/// The user's answer to an `outcome_unknown` attempt (spec/05 step 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownResolution {
    MarkedSent,
    SendAgain,
    KeptAsDraft,
}

impl UnknownResolution {
    fn into_engine(self) -> sb_engine::UnknownResolution {
        match self {
            Self::MarkedSent => sb_engine::UnknownResolution::MarkedSent,
            Self::SendAgain => sb_engine::UnknownResolution::SendAgain,
            Self::KeptAsDraft => sb_engine::UnknownResolution::KeptAsDraft,
        }
    }

    fn from_engine(resolution: sb_engine::UnknownResolution) -> Self {
        match resolution {
            sb_engine::UnknownResolution::MarkedSent => Self::MarkedSent,
            sb_engine::UnknownResolution::SendAgain => Self::SendAgain,
            sb_engine::UnknownResolution::KeptAsDraft => Self::KeptAsDraft,
        }
    }
}

/// What `resolve_unknown_send` answers.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ResolvedUnknown {
    pub receipt: Receipt,
    pub resolution: UnknownResolution,
    pub new_attempt_id: Option<String>,
}

impl Core {
    /// `send_draft(draft_id, revision_id, acks)` (spec/07): queue the
    /// draft's current revision as a `held` attempt; a repeated key
    /// returns its receipt. A non-current revision, a `sent` or
    /// `discarded` draft, and a live or already-sent attempt are
    /// `Conflict`. `acks` carries the cross-account acknowledgement.
    pub async fn send_draft(
        &self,
        idempotency_key: &str,
        draft_id: &str,
        revision_id: &str,
        acks: Vec<String>,
    ) -> Result<SendQueued, CoreError> {
        let queued = self
            .engine()
            .send_draft(idempotency_key, draft_id, revision_id, &acks)
            .await?;
        Ok(SendQueued {
            receipt: Receipt::from_engine(queued.receipt()),
            attempt_id: queued.attempt_id().to_owned(),
        })
    }

    /// `cancel_send(attempt_id)` (spec/07 "Commands"): a `held` or
    /// `queued` attempt becomes `cancelled` and its draft returns to
    /// `editing` while the attempt's revision is still current. Any
    /// other state is `Conflict`; an unknown attempt is `NotFound`.
    pub async fn cancel_send(
        &self,
        idempotency_key: &str,
        attempt_id: &str,
    ) -> Result<Receipt, CoreError> {
        let receipt = self
            .engine()
            .cancel_send(idempotency_key, attempt_id)
            .await?;
        Ok(Receipt::from_engine(&receipt))
    }

    /// `resolve_unknown_send(attempt_id, resolution)` (spec/07): record
    /// the user's answer to an unresolved `outcome_unknown` attempt,
    /// exactly once. `marked_sent` moves its `unknown` recipients to
    /// `sent`, counts them and sets the draft `sent`; `kept_as_draft`
    /// returns the draft to `editing`; `send_again` queues a new attempt.
    /// A second resolution is `Conflict`.
    pub async fn resolve_unknown_send(
        &self,
        idempotency_key: &str,
        attempt_id: &str,
        resolution: UnknownResolution,
    ) -> Result<ResolvedUnknown, CoreError> {
        let resolved = self
            .engine()
            .resolve_unknown_send(idempotency_key, attempt_id, resolution.into_engine())
            .await?;
        Ok(ResolvedUnknown {
            receipt: Receipt::from_engine(resolved.receipt()),
            resolution: UnknownResolution::from_engine(resolved.resolution()),
            new_attempt_id: resolved.new_attempt_id().map(str::to_owned),
        })
    }

    /// `confirm_send_route(attempt_id)` (spec/07): a `queued` attempt
    /// adopts the account's current generation. An unsendable identity
    /// or any other state is `Conflict`.
    pub async fn confirm_send_route(
        &self,
        idempotency_key: &str,
        attempt_id: &str,
    ) -> Result<Receipt, CoreError> {
        let receipt = self
            .engine()
            .confirm_send_route(idempotency_key, attempt_id)
            .await?;
        Ok(Receipt::from_engine(&receipt))
    }

    /// `retry_rejected_recipients(attempt_id)` (spec/07): refused for a
    /// Proton attempt; nothing is sent.
    pub async fn retry_rejected_recipients(
        &self,
        idempotency_key: &str,
        attempt_id: &str,
    ) -> Result<Receipt, CoreError> {
        let receipt = self
            .engine()
            .retry_rejected_recipients(idempotency_key, attempt_id)
            .await?;
        Ok(Receipt::from_engine(&receipt))
    }

    /// `out_box()` (spec/07 "Other queries"): every live attempt, then
    /// the last 30 days' history, each newest first by update time.
    /// The Proton evidence is in-memory and not yet produced (M4.3f),
    /// so every row's evidence is empty for now.
    pub async fn out_box(&self) -> Result<OutBox, CoreError> {
        Ok(OutBox::from_engine(self.engine().out_box().await?))
    }
}
