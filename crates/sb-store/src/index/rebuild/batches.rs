//! Bounded rebuild phases, with checkpoints committed beside their effects.
//! The caller yields to foreground writes between every batch (spec/03, 06).

use rusqlite::{Transaction, params};

use super::{IndexFence, RebuildJob, RebuildPhase, rebuild_job, status};
use crate::index::{IndexReason, advance_generation, check_count, enqueue_one, invalid, sql_error};
use crate::{StoreError, Timestamp};

impl RebuildPhase {
    fn as_str(self) -> &'static str {
        match self {
            Self::Reset => "reset",
            Self::Enqueue => "enqueue",
            Self::Index => "index",
            Self::Complete => "complete",
        }
    }
}

fn check_scan_limit(limit: usize) -> Result<(), StoreError> {
    check_count(limit)?;
    if limit == 0 {
        return Err(invalid("a rebuild scan limit must be positive"));
    }
    Ok(())
}

/// Accepted Index-phase publications count toward the active job's
/// committed phase count, in the same transaction as publication.
pub(crate) fn count_accepted(
    tx: &Transaction,
    fence: IndexFence,
    accepted: usize,
) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE search_rebuild_jobs SET phase_count = phase_count + ?2
         WHERE epoch = ?1 AND phase = 'index'",
        params![fence.epoch, accepted as i64],
    )
    .map_err(sql_error)?;
    Ok(())
}

fn gated_job(
    tx: &Transaction,
    job_id: &str,
    epoch: i64,
    phase: RebuildPhase,
) -> Result<Option<RebuildJob>, StoreError> {
    let job = rebuild_job(tx, job_id)?;
    let state = status(tx)?;
    let open = state.epoch == epoch && job.epoch == epoch && job.phase == phase && !state.paused();
    Ok(if open { Some(job) } else { None })
}

/// Delete up to `limit` (at most 200) derived documents, issuing FTS
/// 'delete' with each old indexed/partial row before deleting its docs row.
/// An exhausted scan transitions to Enqueue and resets phase progress.
/// Stale/paused calls write nothing.
pub fn rebuild_reset_batch(
    tx: &Transaction,
    job_id: &str,
    epoch: i64,
    limit: usize,
) -> Result<usize, StoreError> {
    reset_batch_with_cut(tx, job_id, epoch, limit, &mut || Ok(()))
}

pub(super) fn reset_batch_with_cut(
    tx: &Transaction,
    job_id: &str,
    epoch: i64,
    limit: usize,
    cut: &mut impl FnMut() -> Result<(), StoreError>,
) -> Result<usize, StoreError> {
    check_scan_limit(limit)?;
    cut()?;
    let Some(job) = gated_job(tx, job_id, epoch, RebuildPhase::Reset)? else {
        return Ok(0);
    };
    let ids: Vec<i64> = tx
        .prepare("SELECT doc_id FROM d_search_docs WHERE doc_id > ?1 ORDER BY doc_id LIMIT ?2")
        .map_err(sql_error)?
        .query_map(params![job.doc_cursor, limit as i64], |row| row.get(0))
        .map_err(sql_error)?
        .collect::<Result<_, _>>()
        .map_err(sql_error)?;
    let through = ids.last().copied().unwrap_or(job.doc_cursor);
    if !ids.is_empty() {
        cut()?;
        tx.execute(
            "INSERT INTO d_search_fts (d_search_fts, rowid, subject, from_text, to_text,
               cc_text, body_authored, body_quoted, body_signature, attachment_names)
             SELECT 'delete', doc_id, subject, from_text, to_text, cc_text, body_authored,
               body_quoted, body_signature, attachment_names FROM d_search_docs
             WHERE doc_id > ?1 AND doc_id <= ?2 AND state IN ('indexed','partial')",
            params![job.doc_cursor, through],
        )
        .map_err(sql_error)?;
        cut()?;
        tx.execute(
            "DELETE FROM d_search_docs WHERE doc_id > ?1 AND doc_id <= ?2",
            params![job.doc_cursor, through],
        )
        .map_err(sql_error)?;
    }
    let (phase, phase_count) = if ids.len() < limit {
        (RebuildPhase::Enqueue, 0)
    } else {
        (
            RebuildPhase::Reset,
            job.phase_count.saturating_add(ids.len() as i64),
        )
    };
    cut()?;
    tx.execute(
        "UPDATE search_rebuild_jobs SET doc_cursor = ?2, phase = ?3, phase_count = ?4
         WHERE job_id = ?1",
        params![job_id, through, phase.as_str(), phase_count],
    )
    .map_err(sql_error)?;
    cut()?;
    advance_generation(tx)?;
    Ok(ids.len())
}

/// Queue at most 200 acquisitions from current availability, with fresh
/// revisions and the oldest pending age preserved. Ordinary enqueues behind
/// the cursor remain pending. Exhaustion transitions atomically to Index.
pub fn rebuild_enqueue_batch(
    tx: &Transaction,
    job_id: &str,
    epoch: i64,
    limit: usize,
    now: Timestamp,
) -> Result<usize, StoreError> {
    enqueue_batch_with_cut(tx, job_id, epoch, limit, now, &mut || Ok(()))
}

pub(super) fn enqueue_batch_with_cut(
    tx: &Transaction,
    job_id: &str,
    epoch: i64,
    limit: usize,
    now: Timestamp,
    cut: &mut impl FnMut() -> Result<(), StoreError>,
) -> Result<usize, StoreError> {
    check_scan_limit(limit)?;
    cut()?;
    let Some(job) = gated_job(tx, job_id, epoch, RebuildPhase::Enqueue)? else {
        return Ok(0);
    };
    let rows: Vec<(i64, String)> = tx
        .prepare(
            "SELECT acquisition_id, object_state FROM acquisitions WHERE acquisition_id > ?1
             ORDER BY acquisition_id LIMIT ?2",
        )
        .map_err(sql_error)?
        .query_map(params![job.acq_cursor, limit as i64], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .map_err(sql_error)?
        .collect::<Result<_, _>>()
        .map_err(sql_error)?;
    for &(id, ref state) in &rows {
        cut()?;
        let reason = if state == "published" {
            IndexReason::Reextract
        } else {
            IndexReason::Delete
        };
        enqueue_one(tx, id, reason, now)?;
    }
    let cursor = rows.last().map(|(id, _)| *id).unwrap_or(job.acq_cursor);
    let (phase, phase_count) = if rows.len() < limit {
        (RebuildPhase::Index, 0)
    } else {
        (
            RebuildPhase::Enqueue,
            job.phase_count.saturating_add(rows.len() as i64),
        )
    };
    cut()?;
    tx.execute(
        "UPDATE search_rebuild_jobs SET acq_cursor = ?2, phase = ?3, phase_count = ?4
         WHERE job_id = ?1",
        params![job_id, cursor, phase.as_str(), phase_count],
    )
    .map_err(sql_error)?;
    cut()?;
    advance_generation(tx)?;
    Ok(rows.len())
}

/// Only an unpaused, current Index job observing an empty queue may complete.
/// The observation, phase change and generation advance share one transaction.
pub fn complete_rebuild(tx: &Transaction, job_id: &str, epoch: i64) -> Result<bool, StoreError> {
    complete_with_cut(tx, job_id, epoch, &mut || Ok(()))
}

pub(super) fn complete_with_cut(
    tx: &Transaction,
    job_id: &str,
    epoch: i64,
    cut: &mut impl FnMut() -> Result<(), StoreError>,
) -> Result<bool, StoreError> {
    cut()?;
    let Some(_job) = gated_job(tx, job_id, epoch, RebuildPhase::Index)? else {
        return Ok(false);
    };
    let pending: bool = tx
        .query_row("SELECT EXISTS(SELECT 1 FROM d_index_queue)", [], |row| {
            row.get(0)
        })
        .map_err(sql_error)?;
    if pending {
        return Ok(false);
    }
    cut()?;
    tx.execute(
        "UPDATE search_rebuild_jobs SET phase = 'complete' WHERE job_id = ?1",
        [job_id],
    )
    .map_err(sql_error)?;
    cut()?;
    advance_generation(tx)?;
    Ok(true)
}
