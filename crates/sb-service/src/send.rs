//! `Core`'s sending surface (spec/05 "Outbox state machine", spec/07
//! "Commands"): the `send_draft`, `cancel_send`,
//! `resolve_unknown_send`, `confirm_send_route` and
//! `retry_rejected_recipients` commands as service-owned DTOs. No
//! engine or store type crosses into L1 (spec/01 rule 3).

use crate::{Core, CoreError, Receipt};

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
}
