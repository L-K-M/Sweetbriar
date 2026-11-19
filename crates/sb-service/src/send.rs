//! `Core`'s sending surface (spec/05 "Outbox state machine", spec/07
//! "Commands"): the `send_draft` and `cancel_send` commands as
//! service-owned DTOs, mirroring the drafts commands. No engine or
//! store type crosses into L1 (spec/01 rule 3).

use crate::{Core, CoreError, Receipt};

/// What `send_draft` answers: its receipt and the attempt id the
/// receipt names, so a replayed key still names the first attempt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendQueued {
    pub receipt: Receipt,
    pub attempt_id: String,
}

impl Core {
    /// `send_draft(draft_id, revision_id, acks)` (spec/07 "Commands"):
    /// queue the draft's current revision as a `held` attempt under an
    /// idempotency key. A repeated key returns the stored receipt and
    /// its attempt id. A revision other than the draft's current one,
    /// a `sent` or `discarded` draft, and a revision with a live or
    /// already-sent attempt are `Conflict`. `acks` carries the warnings
    /// the user confirmed for this revision; M4.3b checks the
    /// cross-account one.
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
}
