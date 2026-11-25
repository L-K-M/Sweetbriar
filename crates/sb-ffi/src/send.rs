//! Sending commands and the Out Box over FFI (spec/07 "Commands",
//! "Other queries"): the service-owned send DTOs cross as this
//! crate's own records, field by field, and the five commands each
//! take the token and idempotency key the shell made. `out_box` is a
//! query, so its token races every wait (spec/07).

use crate::api::Receipt;
use crate::drafts::AttemptState;
use crate::{CancelToken, Core, CoreError};
use std::sync::Arc;

/// The `sent_copy` column's words (spec/02 "Sending"): how the
/// account's Sent copy is produced.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SentCopy {
    ServerSaves,
    Pending,
    Filed,
    Failed,
}

impl From<sb_service::SentCopy> for SentCopy {
    fn from(value: sb_service::SentCopy) -> Self {
        match value {
            sb_service::SentCopy::ServerSaves => Self::ServerSaves,
            sb_service::SentCopy::Pending => Self::Pending,
            sb_service::SentCopy::Filed => Self::Filed,
            sb_service::SentCopy::Failed => Self::Failed,
        }
    }
}

/// A recipient's address field (spec/02 `send_recipients.kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum RecipientKind {
    To,
    Cc,
    Bcc,
}

impl From<sb_service::RecipientKind> for RecipientKind {
    fn from(value: sb_service::RecipientKind) -> Self {
        match value {
            sb_service::RecipientKind::To => Self::To,
            sb_service::RecipientKind::Cc => Self::Cc,
            sb_service::RecipientKind::Bcc => Self::Bcc,
        }
    }
}

/// A recipient's stored state (spec/02 `send_recipients.state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum RecipientState {
    Pending,
    RcptAccepted,
    RejectedPermanent,
    RejectedTransient,
    Sent,
    Unsent,
    Unknown,
}

impl From<sb_service::RecipientState> for RecipientState {
    fn from(value: sb_service::RecipientState) -> Self {
        match value {
            sb_service::RecipientState::Pending => Self::Pending,
            sb_service::RecipientState::RcptAccepted => Self::RcptAccepted,
            sb_service::RecipientState::RejectedPermanent => Self::RejectedPermanent,
            sb_service::RecipientState::RejectedTransient => Self::RejectedTransient,
            sb_service::RecipientState::Sent => Self::Sent,
            sb_service::RecipientState::Unsent => Self::Unsent,
            sb_service::RecipientState::Unknown => Self::Unknown,
        }
    }
}

/// A Proton recipient's package scheme (spec/02 "Sending", spec/05
/// step 4): `internal` encrypted to its Proton keys, `clear` an
/// external address sent without end-to-end encryption. None before
/// the send request commits and for generic attempts.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum RecipientScheme {
    Internal,
    Clear,
}

impl From<sb_service::RecipientScheme> for RecipientScheme {
    fn from(value: sb_service::RecipientScheme) -> Self {
        match value {
            sb_service::RecipientScheme::Internal => Self::Internal,
            sb_service::RecipientScheme::Clear => Self::Clear,
        }
    }
}

/// One recipient on an Out Box row (spec/07): the address as first
/// written, its field, its stored state and, once the send request
/// committed, its Proton scheme.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct OutBoxRecipient {
    pub address: String,
    pub kind: RecipientKind,
    pub state: RecipientState,
    pub scheme: Option<RecipientScheme>,
}

impl From<sb_service::OutBoxRecipient> for OutBoxRecipient {
    fn from(value: sb_service::OutBoxRecipient) -> Self {
        Self {
            address: value.address,
            kind: value.kind.into(),
            state: value.state.into(),
            scheme: value.scheme.map(Into::into),
        }
    }
}

/// One Out Box row (spec/07 `out_box()`): the attempt's frozen
/// revision subject, recipients, state and stored times, plus the
/// Sent-copy reconciliation and the Proton evidence observed since
/// launch (spec/05 step 6).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct OutBoxRow {
    pub attempt_id: String,
    pub draft_id: String,
    pub account_id: String,
    pub identity_id: String,
    /// The sending address the attempt froze (`envelope_from`, spec/07).
    pub envelope_from: String,
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
    /// shows none until its next check.
    pub evidence: Vec<String>,
}

impl From<sb_service::OutBoxRow> for OutBoxRow {
    fn from(value: sb_service::OutBoxRow) -> Self {
        Self {
            attempt_id: value.attempt_id,
            draft_id: value.draft_id,
            account_id: value.account_id,
            identity_id: value.identity_id,
            envelope_from: value.envelope_from,
            subject: value.subject,
            recipients: value.recipients.into_iter().map(Into::into).collect(),
            state: value.state.into(),
            created_at_ms: value.created_at_ms,
            updated_at_ms: value.updated_at_ms,
            hold_until_ms: value.hold_until_ms,
            resolved_at_ms: value.resolved_at_ms,
            send_requested_at_ms: value.send_requested_at_ms,
            proton_draft_id: value.proton_draft_id,
            final_reply: value.final_reply,
            resolution: value.resolution.map(Into::into),
            sent_copy: value.sent_copy.into(),
            waits_for_confirmation: value.waits_for_confirmation,
            sent_copy_appeared: value.sent_copy_appeared,
            sent_copy_warning: value.sent_copy_warning,
            evidence: value.evidence,
        }
    }
}

/// The Out Box (spec/07): live attempts, then history, each newest
/// first.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct OutBox {
    pub live: Vec<OutBoxRow>,
    pub history: Vec<OutBoxRow>,
}

impl From<sb_service::OutBox> for OutBox {
    fn from(value: sb_service::OutBox) -> Self {
        Self {
            live: value.live.into_iter().map(Into::into).collect(),
            history: value.history.into_iter().map(Into::into).collect(),
        }
    }
}

/// The cross-account warning for one draft revision (spec/05, 07):
/// the receiving occurrence's account, the route identity's account,
/// and the acknowledgement `send_draft` accepts for exactly that
/// revision's identity and recipients. The acknowledgement is not
/// persisted.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct CrossAccountNotice {
    pub source_account_id: String,
    pub route_account_id: String,
    pub ack: String,
}

impl From<sb_service::CrossAccountNotice> for CrossAccountNotice {
    fn from(value: sb_service::CrossAccountNotice) -> Self {
        Self {
            source_account_id: value.source_account_id,
            route_account_id: value.route_account_id,
            ack: value.ack,
        }
    }
}

/// The recipient check for one draft revision (spec/05 "Recipient
/// check", spec/07): whether the check is ready and each blocked
/// recipient with the sending accounts it has written to.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct RecipientFence {
    pub ready: bool,
    pub blocked: Vec<BlockedRecipient>,
}

impl From<sb_service::RecipientFence> for RecipientFence {
    fn from(value: sb_service::RecipientFence) -> Self {
        Self {
            ready: value.ready,
            blocked: value.blocked.into_iter().map(Into::into).collect(),
        }
    }
}

/// One blocked recipient (spec/05 "Recipient check"): its address as
/// the draft spells it and the sending accounts it has written to.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct BlockedRecipient {
    pub address: String,
    pub account_ids: Vec<String>,
}

impl From<sb_service::BlockedRecipient> for BlockedRecipient {
    fn from(value: sb_service::BlockedRecipient) -> Self {
        Self {
            address: value.address,
            account_ids: value.account_ids,
        }
    }
}

/// One allowance, for Settings (spec/07): the account, the address as
/// the user allowed it and when it was allowed.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct RecipientAllowance {
    pub account_id: String,
    pub address: String,
    pub created_at_ms: i64,
}

impl From<sb_service::RecipientAllowance> for RecipientAllowance {
    fn from(value: sb_service::RecipientAllowance) -> Self {
        Self {
            account_id: value.account_id,
            address: value.address,
            created_at_ms: value.created_at_ms,
        }
    }
}

/// What `send_draft` answers: its receipt and the attempt id the
/// receipt names, so a replayed key still names the first attempt.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SendQueued {
    pub receipt: Receipt,
    pub attempt_id: String,
}

impl From<sb_service::SendQueued> for SendQueued {
    fn from(value: sb_service::SendQueued) -> Self {
        Self {
            receipt: Receipt::from_service(&value.receipt),
            attempt_id: value.attempt_id,
        }
    }
}

/// The user's answer to an `outcome_unknown` attempt (spec/05 step 6).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum UnknownResolution {
    MarkedSent,
    SendAgain,
    KeptAsDraft,
}

impl From<sb_service::UnknownResolution> for UnknownResolution {
    fn from(value: sb_service::UnknownResolution) -> Self {
        match value {
            sb_service::UnknownResolution::MarkedSent => Self::MarkedSent,
            sb_service::UnknownResolution::SendAgain => Self::SendAgain,
            sb_service::UnknownResolution::KeptAsDraft => Self::KeptAsDraft,
        }
    }
}

impl From<UnknownResolution> for sb_service::UnknownResolution {
    fn from(value: UnknownResolution) -> Self {
        match value {
            UnknownResolution::MarkedSent => Self::MarkedSent,
            UnknownResolution::SendAgain => Self::SendAgain,
            UnknownResolution::KeptAsDraft => Self::KeptAsDraft,
        }
    }
}

/// What `resolve_unknown_send` answers.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ResolvedUnknown {
    pub receipt: Receipt,
    pub resolution: UnknownResolution,
    pub new_attempt_id: Option<String>,
}

impl From<sb_service::ResolvedUnknown> for ResolvedUnknown {
    fn from(value: sb_service::ResolvedUnknown) -> Self {
        Self {
            receipt: Receipt::from_service(&value.receipt),
            resolution: value.resolution.into(),
            new_attempt_id: value.new_attempt_id,
        }
    }
}

#[uniffi::export]
impl Core {
    /// `send_draft(draft_id, revision_id, acks)` (spec/07 "Commands"):
    /// queue the draft's current revision as a `held` attempt under an
    /// idempotency key the shell made (`UUID()` in Swift); a repeated
    /// key returns its receipt and the first attempt id. A
    /// non-current revision, a `sent` or `discarded` draft, a live or
    /// already-sent attempt, and a missing cross-account acknowledgement
    /// are `Conflict`. Like every command it honours the token only
    /// until it holds the core; once begun it runs to completion.
    pub async fn send_draft(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        draft_id: String,
        revision_id: String,
        acks: Vec<String>,
    ) -> Result<SendQueued, CoreError> {
        let queued = self
            .command(&token, async |core| {
                core.send_draft(&idempotency_key, &draft_id, &revision_id, acks)
                    .await
            })
            .await?;
        Ok(SendQueued::from(queued))
    }

    /// `cancel_send(attempt_id)` (spec/07 "Commands"): a `held` or
    /// `queued` attempt becomes `cancelled` and its draft returns to
    /// `editing`; any other state is `Conflict`, an unknown attempt
    /// `NotFound`. A repeated key replays its receipt rather than
    /// meeting the cancelled state. The receipt answers
    /// `{"attempt_id": ...}`.
    pub async fn cancel_send(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        attempt_id: String,
    ) -> Result<Receipt, CoreError> {
        let receipt = self
            .command(&token, async |core| {
                core.cancel_send(&idempotency_key, &attempt_id).await
            })
            .await?;
        Ok(Receipt::from_service(&receipt))
    }

    /// `resolve_unknown_send(attempt_id, resolution)` (spec/07
    /// "Commands"): record the user's answer to an unresolved
    /// `outcome_unknown` attempt, exactly once; a second resolution is
    /// `Conflict`. `marked_sent` counts the unknown recipients as sent
    /// and sets the draft `sent`; `kept_as_draft` returns it to
    /// `editing`; `send_again` queues a new attempt and answers its id.
    /// A repeated key replays the first answer, new attempt included.
    pub async fn resolve_unknown_send(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        attempt_id: String,
        resolution: UnknownResolution,
    ) -> Result<ResolvedUnknown, CoreError> {
        let resolved = self
            .command(&token, async |core| {
                core.resolve_unknown_send(&idempotency_key, &attempt_id, resolution.into())
                    .await
            })
            .await?;
        Ok(ResolvedUnknown::from(resolved))
    }

    /// `confirm_send_route(attempt_id)` (spec/07 "Commands"): a
    /// `queued` attempt adopts the account's current generation. An
    /// unsendable identity or any other state is `Conflict`. A repeated
    /// key replays its receipt. The receipt answers `{"attempt_id": ...}`.
    pub async fn confirm_send_route(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        attempt_id: String,
    ) -> Result<Receipt, CoreError> {
        let receipt = self
            .command(&token, async |core| {
                core.confirm_send_route(&idempotency_key, &attempt_id).await
            })
            .await?;
        Ok(Receipt::from_service(&receipt))
    }

    /// `retry_rejected_recipients(attempt_id)` (spec/07 "Commands"):
    /// a Proton attempt is `Conflict`, having no partially accepted
    /// recipients; an unknown attempt is `NotFound`. A repeated key
    /// replays its receipt. The receipt answers `{"attempt_id": ...}`.
    pub async fn retry_rejected_recipients(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        attempt_id: String,
    ) -> Result<Receipt, CoreError> {
        let receipt = self
            .command(&token, async |core| {
                core.retry_rejected_recipients(&idempotency_key, &attempt_id)
                    .await
            })
            .await?;
        Ok(Receipt::from_service(&receipt))
    }

    /// `cross_account_notice(draft_id, revision_id)` (spec/07 "Other
    /// queries"): the cross-account warning for that revision of a
    /// reply or reply-all, or `None` when none applies (spec/05). A
    /// revision that isn't the draft's current one is `Conflict`, an
    /// unknown draft `NotFound`. A query: the token races the core's
    /// read lock and the read itself (spec/07).
    pub async fn cross_account_notice(
        &self,
        token: Arc<CancelToken>,
        draft_id: String,
        revision_id: String,
    ) -> Result<Option<CrossAccountNotice>, CoreError> {
        let notice = self
            .query(&token, async |core| {
                core.cross_account_notice(&draft_id, &revision_id).await
            })
            .await?;
        Ok(notice.map(Into::into))
    }

    /// `recipient_fence(draft_id, revision_id)` (spec/07 "Other
    /// queries", spec/05 "Recipient check"): which recipients of the
    /// draft's current revision are blocked for its route's account. A
    /// revision without a route blocks nothing yet; a revision that
    /// isn't the draft's current one is `Conflict`, an unknown draft
    /// `NotFound`. A query: the token races the core's read lock and
    /// the read itself (spec/07).
    pub async fn recipient_fence(
        &self,
        token: Arc<CancelToken>,
        draft_id: String,
        revision_id: String,
    ) -> Result<RecipientFence, CoreError> {
        let fence = self
            .query(&token, async |core| {
                core.recipient_fence(&draft_id, &revision_id).await
            })
            .await?;
        Ok(fence.into())
    }

    /// `recipient_allowances()` (spec/07 "Other queries"): every
    /// allowance, ordered by account then canonical address, for
    /// Settings (spec/05 "Recipient check"). A query: the token races
    /// the core's read lock and the read itself (spec/07).
    pub async fn recipient_allowances(
        &self,
        token: Arc<CancelToken>,
    ) -> Result<Vec<RecipientAllowance>, CoreError> {
        let allowances = self
            .query(&token, async |core| core.recipient_allowances().await)
            .await?;
        Ok(allowances.into_iter().map(Into::into).collect())
    }

    /// `allow_recipient(idempotency_key, account_id, address)`
    /// (spec/07 "Commands", spec/05 "Recipient check"): one command
    /// under an idempotency key the shell made (`UUID()` in Swift); a
    /// repeated key returns its receipt. Allowing an allowed address
    /// writes nothing; the local archive is `InvalidInput`, an unknown
    /// account `NotFound`. Like every command it honours the token only
    /// until it holds the core; once begun it runs to completion.
    pub async fn allow_recipient(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        account_id: String,
        address: String,
    ) -> Result<Receipt, CoreError> {
        let receipt = self
            .command(&token, async |core| {
                core.allow_recipient(&idempotency_key, &account_id, &address)
                    .await
            })
            .await?;
        Ok(Receipt::from_service(&receipt))
    }

    /// `remove_recipient_allowance(idempotency_key, account_id,
    /// address)` (spec/07 "Commands", spec/05 "Recipient check"):
    /// revoke an allowance, so the block applies again. An address that
    /// isn't allowed is `NotFound`; the local archive `InvalidInput`.
    /// Like every command it honours the token only until it holds the
    /// core; once begun it runs to completion.
    pub async fn remove_recipient_allowance(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        account_id: String,
        address: String,
    ) -> Result<Receipt, CoreError> {
        let receipt = self
            .command(&token, async |core| {
                core.remove_recipient_allowance(&idempotency_key, &account_id, &address)
                    .await
            })
            .await?;
        Ok(Receipt::from_service(&receipt))
    }

    /// `out_box()` (spec/07 "Other queries"): every live attempt, then
    /// the last 30 days' history, each newest first by update time.
    /// A query: the token races the core's read lock and the read
    /// itself (spec/07).
    pub async fn out_box(&self, token: Arc<CancelToken>) -> Result<OutBox, CoreError> {
        Ok(self
            .query(&token, async |core| core.out_box().await)
            .await?
            .into())
    }
}

#[cfg(test)]
mod tests;
