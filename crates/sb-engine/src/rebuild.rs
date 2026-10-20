//! Search rebuild controls (spec/07 "Search rebuild controls", "Engine
//! command boundary"): the engine's start/resume/retry commands over the
//! durable job and hold records. `cancel_job` dispatches here by job
//! kind (import.rs); this module owns the other entry points.
//!
//! Every command is one counted worker: the write at User priority,
//! then the worker's wake inside the same body before the reply, so a
//! dropped caller cannot strand an accepted control.

use sb_resources::{
    RebuildPhase, Receipt, SEARCH_EXTRACTOR_VERSION, StoreError, UserPause, WritePriority,
    encode_job_receipt, rebuild_job, request_index_retry, run_once, set_user_pause,
    start_search_rebuild,
};

use crate::{Engine, EngineError};

/// The commands' names in `command_receipts` (spec/07).
const START_SEARCH_REBUILD: &str = "start_search_rebuild";
const RESUME_SEARCH_REBUILD: &str = "resume_search_rebuild";
const RETRY_SEARCH_INDEX: &str = "retry_search_index";

/// What `start_search_rebuild` answers: its receipt and the durable job
/// it actually names — the stored id an active rebuild already had,
/// which needs no UUID shape.
#[derive(Debug, Clone)]
pub struct SearchRebuildStarted {
    receipt: Receipt,
    job_id: String,
}

impl SearchRebuildStarted {
    pub fn receipt(&self) -> &Receipt {
        &self.receipt
    }

    pub fn job_id(&self) -> &str {
        &self.job_id
    }
}

impl Engine {
    /// `start_search_rebuild()` (spec/07): at most one active rebuild
    /// per store. A fresh key starts or reuses the singleton — an
    /// active job, even a held one, keeps its id, epoch, cursors and
    /// holds; a repeated key returns the stored receipt; a start after
    /// completion takes a new id and epoch. Start never resumes.
    pub async fn start_search_rebuild(
        &self,
        idempotency_key: &str,
    ) -> Result<SearchRebuildStarted, EngineError> {
        let candidate = self.resources().new_uuid_string();
        let now = self.resources().now();
        let key = idempotency_key.to_owned();
        self.command(move |worker| async move {
            let receipt = worker
                .resources()
                .write(WritePriority::User, move |tx| {
                    run_once(tx, &key, START_SEARCH_REBUILD, now, |tx| {
                        let id = start_search_rebuild(tx, &candidate, SEARCH_EXTRACTOR_VERSION)?;
                        Ok(encode_job_receipt(&id, None))
                    })
                })
                .await?;
            worker.indexer().wake();
            // The receipt names the actual stored or reused id.
            let job_id = sb_resources::receipt_job_id(&receipt)?;
            Ok(SearchRebuildStarted { receipt, job_id })
        })
        .await
    }

    /// `resume_search_rebuild(job_id)` (spec/07): release only the user
    /// hold, resuming the same job's committed phase — never a fresh
    /// rebuild and never an I/O retry. A completed job is a no-op, an
    /// unknown id `NotFound`, another kind `InvalidInput`; an already
    /// running job is harmless. A completed or replayed id cannot
    /// release another rebuild's hold.
    pub async fn resume_search_rebuild(
        &self,
        idempotency_key: &str,
        job_id: &str,
    ) -> Result<Receipt, EngineError> {
        let now = self.resources().now();
        let (key, id) = (idempotency_key.to_owned(), job_id.to_owned());
        self.command(move |worker| async move {
            let receipt = worker
                .resources()
                .write(WritePriority::User, move |tx| {
                    run_once(tx, &key, RESUME_SEARCH_REBUILD, now, |tx| {
                        resume_in(tx, &id)
                    })
                })
                .await?;
            // A worker parked indefinitely leaves only on a wake.
            worker.indexer().wake();
            Ok(receipt)
        })
        .await
    }

    /// `retry_search_index()` (spec/07): request an immediate attempt at
    /// a held I/O retry — the error, attempts and user hold stay until
    /// the worker's permitted transitions move them. Without an I/O
    /// pause the request is a quiet no-op.
    pub async fn retry_search_index(&self, idempotency_key: &str) -> Result<Receipt, EngineError> {
        let now = self.resources().now();
        let key = idempotency_key.to_owned();
        self.command(move |worker| async move {
            let receipt = worker
                .resources()
                .write(WritePriority::User, move |tx| {
                    run_once(tx, &key, RETRY_SEARCH_INDEX, now, |tx| {
                        request_index_retry(tx, now).map(|_| "{}".to_owned())
                    })
                })
                .await?;
            // A worker parked on a far deadline must hear the request.
            worker.indexer().wake();
            Ok(receipt)
        })
        .await
    }
}

/// The resume's write, run only for a fresh key: `rebuild_job` supplies
/// `NotFound` for an unknown id and `InvalidInput` for another kind.
fn resume_in(tx: &sb_resources::Transaction, id: &str) -> Result<String, StoreError> {
    let job = rebuild_job(tx, id)?;
    if job.phase() == RebuildPhase::Complete {
        return Ok(encode_job_receipt(id, Some("complete")));
    }
    set_user_pause(tx, UserPause::Resume)?;
    Ok(encode_job_receipt(id, Some("running")))
}
