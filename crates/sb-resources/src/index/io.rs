//! Publication I/O reports (spec/06 "Publication I/O reports"): one
//! transactional façade over the durable pause records. A report
//! carries typed context — an uncaptured metadata fault, a captured
//! batch fence, or a captured document fence/token — and a typed
//! `Unavailable` cause, and the writer decides the outcome in the
//! same transaction that reads the current singleton, target and
//! latest job. Recovery commits exactly one validated generation so
//! the engine's failed-writer read gate may clear only afterwards.
//! Timers, scheduling and the attempts themselves stay in the engine.

use sb_mime::SEARCH_EXTRACTOR_VERSION;
use sb_store::index::rebuild::{
    RebuildPhase, SearchIoCode, begin_index_retry, commit_index_recovery, latest_rebuild,
    pause_for_io_if_current, search_index_status,
};
use sb_store::index::{IndexFence, PendingDocument, target_version};
use sb_store::{StoreError, Timestamp, Transaction};

use super::{search_io_code, search_io_message};

/// What the reported failure was captured against (spec/06): an
/// uncaptured metadata fault, a batch's captured fence, or one
/// document's captured fence and exact queue token.
#[derive(Debug, Clone, Copy)]
pub enum IndexIoContext<'a> {
    /// A fault with no captured fence (a metadata read/write): the
    /// report always evaluates the store's current state.
    Metadata,
    /// A fault under a captured batch fence, with no document token.
    Batch {
        /// The fence the failed work was captured under.
        fence: IndexFence,
    },
    /// A fault under a captured fence and one document's exact token.
    Document {
        /// The fence the failed work was captured under.
        fence: IndexFence,
        /// The failing document's exact queue token.
        pending: &'a PendingDocument,
    },
}

/// The transaction's typed verdict (spec/06). None of these names a
/// scheduler state: the engine decides what to do with them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexIoUpdate {
    /// A user hold, an unsupported target or a reset/enqueue phase
    /// refuses publication retries; holds are not released and no
    /// unsupported phase work starts.
    Blocked,
    /// A durable I/O pause stands. A newly recorded report carries its
    /// fresh deadline; an existing future hold keeps its recorded one.
    Paused {
        /// The UTC-millisecond deadline the durable record holds.
        retry_at: Timestamp,
    },
    /// A due metadata retry began under the current fence; persisted
    /// attempt history is preserved.
    RetryBegun,
    /// The captured fence or token no longer matches the store, or a
    /// due pause postdates a captured failure: recapture before
    /// reporting again.
    Stale,
}

/// The shared bounded-backoff deadline (spec/06): one second, doubled
/// per persisted attempt, capped at one minute, with a saturating
/// timestamp. Persisted reports and the engine's volatile writer wait
/// compute the same value; no timer lives here.
pub fn index_retry_at(now: Timestamp, attempts: u32) -> Timestamp {
    let delay = (1_000i64 << attempts.saturating_sub(1).min(6)).min(60_000);
    Timestamp::from_millis(now.as_millis().saturating_add(delay))
}

/// Report an I/O failure inside one writer transaction (spec/06).
/// Only a typed `Unavailable` cause is accepted, classified by
/// [`search_io_code`]; anything else is `InvalidInput` — error text
/// is never parsed or reclassified. A recorded pause or a begun due
/// retry advances `search_generation` once; blocked, preserved and
/// stale reports write nothing.
pub fn report_index_io_failure(
    tx: &Transaction,
    context: IndexIoContext<'_>,
    error: &StoreError,
    now: Timestamp,
) -> Result<IndexIoUpdate, StoreError> {
    Ok(report(tx, context, io_cause(error)?, checked_now(now)?)?.update)
}

/// The recovery half of [`report_index_io_failure`], same arguments
/// and result (spec/06): after the report logic runs, exactly one
/// validated recovery generation must commit. An accepted pause or
/// begun retry already advanced it on fully validated state, so the
/// barrier is not taken twice; any other outcome republishes the
/// protected current status through `commit_index_recovery`, which
/// revalidates the singleton, target and latest job. The whole
/// operation rolls back on any error, preserving the original typed
/// cause — a probe is not successful indexing.
pub fn recover_index_io_failure(
    tx: &Transaction,
    context: IndexIoContext<'_>,
    error: &StoreError,
    now: Timestamp,
) -> Result<IndexIoUpdate, StoreError> {
    let outcome = report(tx, context, io_cause(error)?, checked_now(now)?)?;
    if !outcome.committed {
        commit_index_recovery(tx)?;
    }
    Ok(outcome.update)
}

/// The typed I/O cause, or `InvalidInput` for any non-`Unavailable`
/// input: a reported failure is only ever a store I/O failure.
fn io_cause(error: &StoreError) -> Result<SearchIoCode, StoreError> {
    search_io_code(error).ok_or_else(|| StoreError::InvalidInput {
        message: "a search I/O report needs an unavailable store failure".into(),
    })
}

fn checked_now(now: Timestamp) -> Result<Timestamp, StoreError> {
    if now.as_millis() < 0 {
        return Err(StoreError::InvalidInput {
            message: "invalid search I/O report time".into(),
        });
    }
    Ok(now)
}

/// What the shared body decided: the typed update plus whether it
/// already committed the transaction's one generation advance.
struct Reported {
    update: IndexIoUpdate,
    committed: bool,
}

fn report(
    tx: &Transaction,
    context: IndexIoContext<'_>,
    code: SearchIoCode,
    now: Timestamp,
) -> Result<Reported, StoreError> {
    // The singleton, the stored target and the latest job's metadata
    // all validate inside this same transaction, recovery included.
    let status = search_index_status(tx)?;
    let target = target_version(tx)?;
    let job = latest_rebuild(tx)?;
    const BLOCKED: Reported = Reported {
        update: IndexIoUpdate::Blocked,
        committed: false,
    };
    // Reconciliation must raise older targets before publication;
    // neither an older nor a newer target can retry with this build.
    if target != SEARCH_EXTRACTOR_VERSION
        || status.user_paused()
        || job.is_some_and(|job| matches!(job.phase(), RebuildPhase::Reset | RebuildPhase::Enqueue))
    {
        return Ok(BLOCKED);
    }
    if let Some(pause) = status.io_pause() {
        if pause.retry_at().as_millis() > now.as_millis() {
            // The newer recorded failure keeps its cause, deadline
            // and history; this report changes nothing.
            return Ok(Reported {
                update: IndexIoUpdate::Paused {
                    retry_at: pause.retry_at(),
                },
                committed: false,
            });
        }
        return Ok(match context {
            // A due metadata retry may begin under the current
            // epoch and this build's capability, attempts intact.
            IndexIoContext::Metadata => {
                let fence = IndexFence {
                    epoch: status.epoch(),
                    version: SEARCH_EXTRACTOR_VERSION,
                };
                if begin_index_retry(tx, fence, now)? {
                    Reported {
                        update: IndexIoUpdate::RetryBegun,
                        committed: true,
                    }
                } else {
                    BLOCKED
                }
            }
            // The recorded pause postdates this captured failure:
            // the driver recaptures, never clearing the hold.
            IndexIoContext::Batch { .. } | IndexIoContext::Document { .. } => Reported {
                update: IndexIoUpdate::Stale,
                committed: false,
            },
        });
    }
    let (fence, token, captured) = match context {
        IndexIoContext::Metadata => (
            IndexFence {
                epoch: status.epoch(),
                version: SEARCH_EXTRACTOR_VERSION,
            },
            None,
            false,
        ),
        IndexIoContext::Batch { fence } => (fence, None, true),
        IndexIoContext::Document { fence, pending } => (fence, Some(pending), true),
    };
    let attempts = u32::try_from(status.io_attempts())
        .unwrap_or(u32::MAX)
        .saturating_add(1);
    let retry_at = index_retry_at(now, attempts);
    if pause_for_io_if_current(
        tx,
        fence,
        token,
        code,
        search_io_message(code),
        retry_at,
        attempts,
    )? {
        return Ok(Reported {
            update: IndexIoUpdate::Paused { retry_at },
            committed: true,
        });
    }
    Ok(if captured {
        Reported {
            update: IndexIoUpdate::Stale,
            committed: false,
        }
    } else {
        BLOCKED
    })
}

#[cfg(test)]
mod tests;
