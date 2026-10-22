//! Durable Search index controls (spec/07); Engine owns routing and accepted work.

use crate::{Core, CoreError, Receipt};

/// A stored start receipt and the actual new or reused durable job it names.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRebuildStarted {
    pub receipt: Receipt,
    pub job_id: String,
}

impl Core {
    /// Start or reuse the active singleton; start never releases a hold.
    pub async fn start_search_rebuild(
        &self,
        idempotency_key: &str,
    ) -> Result<SearchRebuildStarted, CoreError> {
        let started = self.engine().start_search_rebuild(idempotency_key).await?;
        Ok(SearchRebuildStarted {
            receipt: Receipt::from_engine(started.receipt()),
            job_id: started.job_id().to_owned(),
        })
    }

    /// Release this rebuild's user hold, preserving I/O holds and committed progress.
    pub async fn resume_search_rebuild(
        &self,
        idempotency_key: &str,
        job_id: &str,
    ) -> Result<Receipt, CoreError> {
        let receipt = self
            .engine()
            .resume_search_rebuild(idempotency_key, job_id)
            .await?;
        Ok(Receipt::from_engine(&receipt))
    }

    /// Request an immediate I/O retry; this never releases a user hold.
    pub async fn retry_search_index(&self, idempotency_key: &str) -> Result<Receipt, CoreError> {
        let receipt = self.engine().retry_search_index(idempotency_key).await?;
        Ok(Receipt::from_engine(&receipt))
    }
}
