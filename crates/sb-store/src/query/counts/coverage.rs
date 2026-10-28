//! Query-independent coverage facts, captured in the caller's read snapshot.

use rusqlite::{Transaction, params_from_iter, types::Value};

use super::{SearchScope, candidates, nonnegative_count, scoped, sql_error};
use crate::StoreError;
use crate::index::{rebuild, search_generation, target_version};
use crate::message_source::{self, MESSAGES, STORED_BOUND, Source};
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

/// The scope scan behind coverage's message counts, valid only for the store
/// it was read from and its scope, search generation and extractor target
/// (spec/07: the generation advances with every input the scan reads).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CoverageCounts {
    scope: SearchScope,
    generation: u64,
    extractor_version: u32,
    counts: [u64; 7],
    /// Scoped candidates in classes no index enumerates (spec/06): a dangling
    /// acquisition reference, or a published object with neither a document
    /// nor a queue row. Internal guard state for driven reads.
    forbidden: u64,
    /// Scoped messages with an acquisition but no header row, binding or
    /// summary, which address drivers cannot enumerate (06, M1.6r2d).
    summaryless: u64,
}

impl CoverageCounts {
    pub fn scope(&self) -> &SearchScope {
        &self.scope
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub(super) fn extractor_version(&self) -> u32 {
        self.extractor_version
    }

    pub(super) fn total_messages(&self) -> u64 {
        self.counts[0]
    }

    pub(super) fn forbidden(&self) -> u64 {
        self.forbidden
    }

    pub(super) fn summaryless(&self) -> u64 {
        self.summaryless
    }
}

/// Scoped message facts plus global operational metadata, without query gates.
pub fn read_search_coverage(
    tx: &Transaction,
    scope: SearchScope,
) -> Result<StoredSearchCoverage, StoreError> {
    read_search_coverage_reusing(tx, scope, None).map(|(coverage, _)| coverage)
}

/// Coverage that reuses `kept` scope counts when they match this snapshot's
/// scope, generation and target, else scans; it always checks the scope's
/// bindings and reads the global state afresh. Returns the counts used.
pub fn read_search_coverage_reusing(
    tx: &Transaction,
    scope: SearchScope,
    kept: Option<&CoverageCounts>,
) -> Result<(StoredSearchCoverage, CoverageCounts), StoreError> {
    let (filter, values) = scoped(tx, scope.clone())?;
    check_bindings(tx, filter, &values)?;
    let generation = nonnegative_count(search_generation(tx)?)?;
    let extractor_version = target_version(tx)?;
    let status = rebuild::search_index_status(tx)?;
    let job = rebuild::latest_rebuild(tx)?;
    let resetting = job
        .as_ref()
        .is_some_and(|job| matches!(job.phase(), RebuildPhase::Reset | RebuildPhase::Enqueue));
    let scanned = match kept {
        Some(kept)
            if kept.scope == scope
                && kept.generation == generation
                && kept.extractor_version == extractor_version =>
        {
            kept.clone()
        }
        _ => {
            let Scan {
                counts,
                forbidden,
                summaryless,
            } = scan(tx, filter, values, extractor_version)?;
            CoverageCounts {
                scope,
                generation,
                extractor_version,
                counts,
                forbidden,
                summaryless,
            }
        }
    };
    let counts = scanned.counts;
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
    let coverage = StoredSearchCoverage {
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
    };
    Ok((coverage, scanned))
}

/// Total, headers synced, bodies acquired, indexed, partial, failed and
/// queued acquisitions over the scope, and its driven-read guard counts.
struct Scan {
    counts: [u64; 7],
    forbidden: u64,
    summaryless: u64,
}

fn scan(
    tx: &Transaction,
    filter: &str,
    mut values: Vec<Value>,
    extractor_version: u32,
) -> Result<Scan, StoreError> {
    // Queued acquisitions in scope, driven from the queue: usually few.
    let queued = format!(
        "SELECT count(DISTINCT lm.acquisition_id) FROM d_index_queue q
         CROSS JOIN {MESSAGES} ON lm.acquisition_id=q.doc_id
         CROSS JOIN d_list_rows r ON r.message_id=lm.message_id WHERE {filter}"
    );
    let queued: i64 = tx
        .query_row(&queued, params_from_iter(&values), |row| row.get(0))
        .map_err(sql_error)?;

    // One pass over the scope. Headers count as synced from a header row or
    // an ok/partial parse; a bound message parses as ok (02), so its binding
    // is looked up only when neither holds. SQLite keeps unused LEFT JOINs in
    // aggregates, so the scan joins only what it reads. Dropping the others
    // keeps every count because each joined on a unique key (one binding per
    // message, one assembly per binding, one acquisition per message).
    values.push(extractor_version.into());
    let version = values.len();
    let sql = format!(
        "SELECT count(*),
         coalesce(sum(CASE WHEN h.message_id IS NOT NULL OR s.parse_state IN ('ok','partial') THEN 1
           WHEN {STORED_BOUND} THEN 1 ELSE 0 END),0),
         coalesce(sum(a.object_state='published'),0),
         coalesce(sum(a.object_state='published' AND d.extractor_version=?{version} AND d.state='indexed'),0),
         coalesce(sum(a.object_state='published' AND d.extractor_version=?{version} AND d.state='partial'),0),
         coalesce(sum(a.object_state='published' AND d.extractor_version=?{version} AND d.state='failed'),0),
         coalesce(sum(lm.acquisition_id IS NOT NULL AND a.acquisition_id IS NULL),0)
         + coalesce(sum(CASE WHEN a.object_state='published' AND d.doc_id IS NULL
           THEN NOT EXISTS(SELECT 1 FROM d_index_queue q WHERE q.doc_id=lm.acquisition_id)
           ELSE 0 END),0),
         coalesce(sum(CASE WHEN lm.acquisition_id IS NOT NULL AND h.message_id IS NULL
           AND s.acquisition_id IS NULL THEN NOT {STORED_BOUND} ELSE 0 END),0)
         FROM {} LEFT JOIN d_search_docs d ON d.doc_id=lm.acquisition_id WHERE {filter}",
        candidates("d_list_rows r", "LEFT JOIN d_message_summary s ON s.acquisition_id=lm.acquisition_id")
    );
    let (raw, forbidden, summaryless): ([i64; 6], i64, i64) = tx
        .query_row(&sql, params_from_iter(&values), |row| {
            Ok((
                [
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ],
                row.get(6)?,
                row.get(7)?,
            ))
        })
        .map_err(sql_error)?;
    let mut counts = [0; 7];
    for (count, raw) in counts.iter_mut().zip(raw.into_iter().chain([queued])) {
        *count = nonnegative_count(raw)?;
    }
    Ok(Scan {
        counts,
        forbidden: nonnegative_count(forbidden)?,
        summaryless: nonnegative_count(summaryless)?,
    })
}

/// Whether any binding in `filter`'s scope is invalid: each binding seeks
/// its message's rows, never the scope's (a mailbox's index would scan the
/// mailbox per binding).
pub(super) fn bindings_check(filter: &str) -> String {
    format!(
        "SELECT EXISTS(SELECT 1 FROM {}
         CROSS JOIN d_list_rows r INDEXED BY d_list_rows_by_message ON r.message_id=lm.message_id
         WHERE {filter} AND ({})<0)",
        message_source::bindings(),
        Source::Stored.validity()
    )
}

/// Fail with `Corrupt` when a scoped binding is invalid (02), at a cost per
/// binding rather than per message.
pub(super) fn check_bindings(
    tx: &Transaction,
    filter: &str,
    values: &[Value],
) -> Result<(), StoreError> {
    let invalid: bool = tx
        .query_row(&bindings_check(filter), params_from_iter(values), |row| {
            row.get(0)
        })
        .map_err(sql_error)?;
    if invalid {
        return Err(sql_error(message_source::invalid()));
    }
    Ok(())
}
