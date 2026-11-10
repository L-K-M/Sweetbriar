//! Each scheduled maintenance job's latest run (spec/03 "Scheduled
//! maintenance", plan M1.8h3): one `maintenance_jobs` row per job, so
//! the scheduler picks its cadence back up after a restart and the
//! store shows when each job last ran, how it ended and when it last
//! completed.
//!
//! The rows are the store's records about itself: they are written as
//! bookkeeping ([`crate::changes::Changes::Bookkeeping`]), so running a
//! job never makes the next generation due.

use rusqlite::{Connection, params};

use crate::schema::classify;
use crate::store::await_write;
use crate::writer::{WritePriority, Writer};
use crate::{StoreError, Timestamp};

/// The longest `detail` a row keeps, in bytes (spec/02 checks it);
/// longer text is cut at a character boundary.
pub const MAINTENANCE_DETAIL_MAX: usize = 1024;

/// A scheduled maintenance job (spec/03 "Scheduled maintenance").
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum MaintenanceJob {
    /// The hourly generation while there were changes.
    Backup,
    /// The daily garbage-collection pass.
    Gc,
    /// The weekly object verification.
    Verify,
    /// The weekly `integrity_check`.
    Integrity,
}

impl MaintenanceJob {
    const ALL: [Self; 4] = [Self::Backup, Self::Gc, Self::Verify, Self::Integrity];

    fn as_str(self) -> &'static str {
        match self {
            Self::Backup => "backup",
            Self::Gc => "gc",
            Self::Verify => "verify",
            Self::Integrity => "integrity",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|job| job.as_str() == text)
    }
}

/// How a finished run ended.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum MaintenanceOutcome {
    /// The job did all its work.
    Complete,
    /// A safety gate kept it from starting (GC's gates, spec/03).
    Blocked,
    /// It stopped when asked (restore's quiesce, shutdown).
    Cancelled,
    /// It failed; the detail says why.
    Failed,
}

impl MaintenanceOutcome {
    const ALL: [Self; 4] = [Self::Complete, Self::Blocked, Self::Cancelled, Self::Failed];

    fn as_str(self) -> &'static str {
        match self {
            Self::Complete => "complete",
            Self::Blocked => "blocked",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }

    fn parse(text: &str) -> Option<Self> {
        Self::ALL
            .into_iter()
            .find(|outcome| outcome.as_str() == text)
    }
}

/// The end of a run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaintenanceFinish {
    pub at: Timestamp,
    pub outcome: MaintenanceOutcome,
    /// What it did or why it stopped, at most
    /// [`MAINTENANCE_DETAIL_MAX`] bytes.
    pub detail: Option<String>,
}

/// One job's latest run.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MaintenanceRun {
    pub job: MaintenanceJob,
    pub started_at: Timestamp,
    /// `None` while the run is in flight, or when the process stopped
    /// before it finished: a run this process didn't start is an
    /// interrupted one.
    pub finished: Option<MaintenanceFinish>,
    /// When a run last ended [`MaintenanceOutcome::Complete`].
    pub succeeded_at: Option<Timestamp>,
}

/// Records `job` as started at `at`; the previous run's end and detail
/// go, its last success stays.
pub(crate) fn start(writer: &Writer, job: MaintenanceJob, at: Timestamp) -> Result<(), StoreError> {
    let future = writer.upkeep(WritePriority::Index, move |tx| {
        tx.execute(
            "INSERT INTO maintenance_jobs (job, started_at) VALUES (?1, ?2)
             ON CONFLICT(job) DO UPDATE SET started_at = excluded.started_at,
               finished_at = NULL, outcome = NULL, detail = NULL",
            params![job.as_str(), at.as_millis()],
        )
        .map_err(|e| classify(e, "record a maintenance run's start"))?;
        Ok(())
    });
    await_write(future)
}

/// Records the end of `job`'s run in flight. With none in flight (never
/// started, or already finished) it's `Conflict`, and nothing changes.
pub(crate) fn finish(
    writer: &Writer,
    job: MaintenanceJob,
    outcome: MaintenanceOutcome,
    detail: Option<&str>,
    at: Timestamp,
) -> Result<(), StoreError> {
    let detail = detail.map(|text| bounded(text).to_owned());
    let future = writer.upkeep(WritePriority::Index, move |tx| {
        let updated = tx
            .execute(
                "UPDATE maintenance_jobs SET finished_at = ?2, outcome = ?3, detail = ?4,
                   succeeded_at = CASE WHEN ?3 = 'complete' THEN ?2 ELSE succeeded_at END
                 WHERE job = ?1 AND finished_at IS NULL",
                params![job.as_str(), at.as_millis(), outcome.as_str(), detail],
            )
            .map_err(|e| classify(e, "record a maintenance run's end"))?;
        if updated == 0 {
            return Err(StoreError::conflict(format!(
                "no {} maintenance run is in flight",
                job.as_str()
            )));
        }
        Ok(())
    });
    await_write(future)
}

/// Every job's latest run, in job order; a job that never ran has no
/// entry. A row the schema can't produce is `Corrupt`.
pub(crate) fn runs(conn: &Connection) -> Result<Vec<MaintenanceRun>, StoreError> {
    let what = "read the maintenance runs";
    let mut statement = conn
        .prepare(
            "SELECT job, started_at, finished_at, outcome, detail, succeeded_at
             FROM maintenance_jobs",
        )
        .map_err(|e| classify(e, what))?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, Option<i64>>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, Option<i64>>(5)?,
            ))
        })
        .map_err(|e| classify(e, what))?;

    let mut runs = Vec::new();
    for row in rows {
        let (job, started_at, finished_at, outcome, detail, succeeded_at) =
            row.map_err(|e| classify(e, what))?;
        let corrupt = || StoreError::corrupt(format!("maintenance_jobs row {job:?} is malformed"));
        let job_kind = MaintenanceJob::parse(&job).ok_or_else(corrupt)?;
        let finished = match (finished_at, outcome) {
            (None, None) if detail.is_none() => None,
            (Some(at), Some(outcome)) => {
                let outcome = MaintenanceOutcome::parse(&outcome).ok_or_else(corrupt)?;
                if outcome == MaintenanceOutcome::Complete && succeeded_at != Some(at) {
                    return Err(corrupt());
                }
                Some(MaintenanceFinish {
                    at: Timestamp::from_millis(at),
                    outcome,
                    detail,
                })
            }
            _ => return Err(corrupt()),
        };
        runs.push(MaintenanceRun {
            job: job_kind,
            started_at: Timestamp::from_millis(started_at),
            finished,
            succeeded_at: succeeded_at.map(Timestamp::from_millis),
        });
    }
    runs.sort_by_key(|run| MaintenanceJob::ALL.iter().position(|job| *job == run.job));
    Ok(runs)
}

/// `text` cut to at most [`MAINTENANCE_DETAIL_MAX`] bytes at a
/// character boundary.
fn bounded(text: &str) -> &str {
    if text.len() <= MAINTENANCE_DETAIL_MAX {
        return text;
    }
    let mut end = MAINTENANCE_DETAIL_MAX;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    &text[..end]
}

#[cfg(test)]
mod tests;
