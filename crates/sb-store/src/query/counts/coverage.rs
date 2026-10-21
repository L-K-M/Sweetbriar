//! Query-independent coverage facts, captured in the caller's read snapshot.

use rusqlite::{Transaction, params_from_iter};

use super::{SOURCE, SearchScope, nonnegative_count, scoped, sql_error};
use crate::StoreError;
use crate::index::{rebuild, search_generation, target_version};
use rebuild::{RebuildPhase, SearchIoCode};

/// Owned coverage values; fields are public for the facade/engine's mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSearchCoverage {
    pub generation: u64,
    pub extractor_version: u32,
    pub total_messages: u64,
    pub headers_synced: u64,
    pub bodies_acquired: u64,
    pub indexed: u64,
    pub partial: u64,
    pub failed: u64,
    pub pending: u64,
    pub queue_length: u64,
    pub index_state: SearchIndexState,
    pub rebuild: Option<SearchRebuildProgress>,
    pub limitations: Vec<SearchLimitation>,
}

/// The operational state; Resources supplies the runtime capability verdict.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchIndexState {
    Ready,
    Running,
    PausedByUser,
    PausedIo {
        code: SearchIoCode,
        message: String,
        retry_at: i64,
    },
    BlockedNewerExtractor,
}

/// Active or most-recently-completed progress, independent of coverage scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRebuildProgress {
    pub job_id: String,
    pub phase: RebuildPhase,
    pub processed: u64,
    pub pending_documents: u64,
}

/// V1 limitations remain visible even when indexing is complete.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchLimitation {
    CjkUnsegmented,
    AttachmentTextUnavailable,
}

/// Scoped message facts plus global operational metadata, without query gates.
pub fn read_search_coverage(
    tx: &Transaction,
    scope: SearchScope,
) -> Result<StoredSearchCoverage, StoreError> {
    let (scope, mut values) = scoped(tx, scope)?;
    let generation = nonnegative_count(search_generation(tx)?)?;
    let extractor_version = target_version(tx)?;
    let status = rebuild::search_index_status(tx)?;
    let job = rebuild::latest_rebuild(tx)?;
    let resetting = job
        .as_ref()
        .is_some_and(|job| matches!(job.phase(), RebuildPhase::Reset | RebuildPhase::Enqueue));
    values.push(extractor_version.into());
    let version = values.len();
    let sql = format!(
        "SELECT count(*),
         coalesce(sum(h.message_id IS NOT NULL OR s.parse_state IN ('ok','partial')),0),
         coalesce(sum(a.object_state='published'),0),
         coalesce(sum(a.object_state='published' AND d.extractor_version=?{version} AND d.state='indexed'),0),
         coalesce(sum(a.object_state='published' AND d.extractor_version=?{version} AND d.state='partial'),0),
         coalesce(sum(a.object_state='published' AND d.extractor_version=?{version} AND d.state='failed'),0),
         count(DISTINCT CASE WHEN q.doc_id IS NOT NULL THEN lm.acquisition_id END)
         FROM {SOURCE} LEFT JOIN d_search_docs d ON d.doc_id=lm.acquisition_id
         LEFT JOIN d_index_queue q ON q.doc_id=lm.acquisition_id WHERE {scope}"
    );
    let raw: [i64; 7] = tx
        .query_row(&sql, params_from_iter(&values), |row| {
            Ok([
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
            ])
        })
        .map_err(sql_error)?;
    let mut counts = [0; 7];
    for (count, raw) in counts.iter_mut().zip(raw) {
        *count = nonnegative_count(raw)?;
    }
    let [
        total_messages,
        headers_synced,
        bodies_acquired,
        mut indexed,
        mut partial,
        mut failed,
        queue_length,
    ] = counts;
    if resetting {
        indexed = 0;
        partial = 0;
        failed = 0;
    }
    let pending = total_messages
        .checked_sub(indexed)
        .and_then(|left| left.checked_sub(partial))
        .and_then(|left| left.checked_sub(failed))
        .ok_or_else(|| StoreError::corrupt("search coverage states exceed total messages"))?;
    let global_queue = tx
        .query_row("SELECT count(*) FROM d_index_queue", [], |row| row.get(0))
        .map_err(sql_error)
        .and_then(nonnegative_count)?;
    let index_state = if status.user_paused() {
        SearchIndexState::PausedByUser
    } else if let Some(pause) = status.io_pause() {
        SearchIndexState::PausedIo {
            code: pause.code(),
            message: pause.message().into(),
            retry_at: pause.retry_at().as_millis(),
        }
    } else if resetting || global_queue > 0 {
        SearchIndexState::Running
    } else {
        SearchIndexState::Ready
    };
    let rebuild = job
        .map(|job| {
            Ok(SearchRebuildProgress {
                job_id: job.id().into(),
                phase: job.phase(),
                processed: job
                    .phase_count()
                    .try_into()
                    .map_err(|_| StoreError::corrupt("negative rebuild progress"))?,
                pending_documents: if job.phase() == RebuildPhase::Complete {
                    0
                } else {
                    global_queue
                },
            })
        })
        .transpose()?;
    Ok(StoredSearchCoverage {
        generation,
        extractor_version,
        total_messages,
        headers_synced,
        bodies_acquired,
        indexed,
        partial,
        failed,
        pending,
        queue_length,
        index_state,
        rebuild,
        limitations: vec![
            SearchLimitation::CjkUnsegmented,
            SearchLimitation::AttachmentTextUnavailable,
        ],
    })
}
