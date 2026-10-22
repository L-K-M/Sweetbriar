//! Durable index commands over the existing receipt and cancellation boundary.

use super::{Arc, CancelToken, Core, CoreError};
use crate::Receipt;

/// The stored start receipt and actual new or reused durable job id.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SearchRebuildStarted {
    pub receipt: Receipt,
    pub job_id: String,
}

#[cfg(test)]
mod tests;

impl SearchRebuildStarted {
    fn from_service(value: &sb_service::SearchRebuildStarted) -> Self {
        Self {
            receipt: Receipt::from_service(&value.receipt),
            job_id: value.job_id.clone(),
        }
    }
}

#[uniffi::export]
impl Core {
    /// Start or reuse the active rebuild without releasing either hold.
    /// Once begun, request cancellation cannot cancel the durable job or its receipt.
    pub async fn start_search_rebuild(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
    ) -> Result<SearchRebuildStarted, CoreError> {
        let value = self
            .command(&token, async |core| {
                core.start_search_rebuild(&idempotency_key).await
            })
            .await?;
        Ok(SearchRebuildStarted::from_service(&value))
    }

    /// Release the matching rebuild's user hold, preserving I/O holds and progress.
    /// Completed ids are no-ops; unknown ids are NotFound, other kinds InvalidInput.
    /// A begun command returns its stored receipt despite request cancellation.
    pub async fn resume_search_rebuild(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        job_id: String,
    ) -> Result<Receipt, CoreError> {
        let value = self
            .command(&token, async |core| {
                core.resume_search_rebuild(&idempotency_key, &job_id).await
            })
            .await?;
        Ok(Receipt::from_service(&value))
    }

    /// Request a global I/O retry; never release a user hold or invent a job id.
    /// A begun command returns its receipt despite request cancellation; idle retry is a no-op.
    pub async fn retry_search_index(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
    ) -> Result<Receipt, CoreError> {
        let value = self
            .command(&token, async |core| {
                core.retry_search_index(&idempotency_key).await
            })
            .await?;
        Ok(Receipt::from_service(&value))
    }
}
