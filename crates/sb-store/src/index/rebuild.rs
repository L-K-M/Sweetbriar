//! Durable rebuild jobs, worker pause state and the writer-side fences
//! (spec/06 "Queue, versions and recovery", spec/07 "Search rebuild
//! controls"). `search_index_state` is one checked singleton row;
//! `search_rebuild_jobs` rows are kept, so completed jobs remain
//! distinguishable from unknown ids. Every mutation runs inside the
//! writer's transaction. Publication writes nothing and leaves generation
//! untouched for stale epochs, blocked phases or a pause. Low-priority
//! submission and yielding between
//! batches are the caller's job (spec/03); there is no worker here.

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use super::{IndexFence, advance_generation, invalid, sql_error, target_version};
use crate::{StoreError, Timestamp};

/// The durable kind a job id names: its table (spec/07). Migration 6's
/// cross-kind triggers make one id unable to live in both.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum JobKind {
    Import,
    SearchRebuild,
}

/// Which kind `id` is, for `cancel_job`-style dispatch. An id in neither
/// table is `NotFound`.
pub fn job_kind(conn: &Connection, id: &str) -> Result<JobKind, StoreError> {
    let exists = |table: &str| -> Result<bool, StoreError> {
        conn.query_row(
            &format!("SELECT EXISTS(SELECT 1 FROM {table} WHERE job_id = ?1)"),
            [id],
            |row| row.get(0),
        )
        .map_err(sql_error)
    };
    if exists("import_jobs")? {
        return Ok(JobKind::Import);
    }
    if exists("search_rebuild_jobs")? {
        return Ok(JobKind::SearchRebuild);
    }
    Err(StoreError::not_found("job"))
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RebuildPhase {
    Reset,
    Enqueue,
    Index,
    Complete,
}

impl RebuildPhase {
    fn parse(phase: &str) -> Result<Self, StoreError> {
        match phase {
            "reset" => Ok(Self::Reset),
            "enqueue" => Ok(Self::Enqueue),
            "index" => Ok(Self::Index),
            "complete" => Ok(Self::Complete),
            _ => Err(StoreError::corrupt("unknown search rebuild phase")),
        }
    }
}

/// A kept rebuild record (spec/07): id, epoch, phase, scan cursors and
/// the committed document count of the current phase.
#[derive(Debug, Clone)]
pub struct RebuildJob {
    job_id: String,
    epoch: i64,
    phase: RebuildPhase,
    doc_cursor: i64,
    acq_cursor: i64,
    phase_count: i64,
}

impl RebuildJob {
    pub fn id(&self) -> &str {
        &self.job_id
    }
    pub fn epoch(&self) -> i64 {
        self.epoch
    }
    pub fn phase(&self) -> RebuildPhase {
        self.phase
    }
    pub fn doc_cursor(&self) -> i64 {
        self.doc_cursor
    }
    pub fn acq_cursor(&self) -> i64 {
        self.acq_cursor
    }
    pub fn phase_count(&self) -> i64 {
        self.phase_count
    }

    fn active(&self) -> bool {
        self.phase != RebuildPhase::Complete
    }
}

/// spec/07's `SearchIoCode`: why the worker stopped on I/O.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchIoCode {
    StorageFull,
    PermissionDenied,
    StorageUnavailable,
    OtherIo,
}

impl SearchIoCode {
    fn as_str(self) -> &'static str {
        match self {
            Self::StorageFull => "storage_full",
            Self::PermissionDenied => "permission_denied",
            Self::StorageUnavailable => "storage_unavailable",
            Self::OtherIo => "other_io",
        }
    }

    fn parse(code: &str) -> Result<Self, StoreError> {
        match code {
            "storage_full" => Ok(Self::StorageFull),
            "permission_denied" => Ok(Self::PermissionDenied),
            "storage_unavailable" => Ok(Self::StorageUnavailable),
            "other_io" => Ok(Self::OtherIo),
            _ => Err(StoreError::corrupt("unknown search I/O code")),
        }
    }
}

/// A recorded I/O pause: the sanitized actionable error and its UTC
/// millisecond retry deadline. The persisted attempt counter belongs to
/// [`SearchIndexStatus`], so clearing a pause does not hide retry history.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IoPause {
    code: SearchIoCode,
    message: String,
    retry_at: Timestamp,
}

impl IoPause {
    pub fn code(&self) -> SearchIoCode {
        self.code
    }
    pub fn message(&self) -> &str {
        &self.message
    }
    pub fn retry_at(&self) -> Timestamp {
        self.retry_at
    }
}

/// The singleton `search_index_state` row: current epoch and both pause
/// states. User and I/O pauses coexist; neither clears the other.
#[derive(Debug, Clone)]
pub struct SearchIndexStatus {
    epoch: i64,
    user_paused: bool,
    io_pause: Option<IoPause>,
    io_attempts: i64,
}

impl SearchIndexStatus {
    pub fn epoch(&self) -> i64 {
        self.epoch
    }
    pub fn user_paused(&self) -> bool {
        self.user_paused
    }
    pub fn io_pause(&self) -> Option<&IoPause> {
        self.io_pause.as_ref()
    }
    /// Retry history remains available after clearing the I/O pause.
    pub fn io_attempts(&self) -> i64 {
        self.io_attempts
    }

    fn paused(&self) -> bool {
        self.user_paused || self.io_pause.is_some()
    }
}

/// The store bound on the caller-sanitized I/O error message (07).
pub const MAX_IO_MESSAGE_BYTES: usize = 512;

fn status(conn: &Connection) -> Result<SearchIndexStatus, StoreError> {
    let row = conn
        .query_row(
            "SELECT epoch, user_paused, io_code, io_message, io_retry_at, io_attempts
             FROM search_index_state WHERE id = 0",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, Option<i64>>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )
        .optional()
        .map_err(sql_error)?;
    let Some((epoch, user_paused, code, message, retry_at, attempts)) = row else {
        return Err(StoreError::corrupt(
            "search_index_state has no singleton row",
        ));
    };
    let io_pause = match (code, message, retry_at) {
        (Some(code), Some(message), Some(retry_at)) => Some(IoPause {
            code: SearchIoCode::parse(&code)?,
            message,
            retry_at: Timestamp::from_millis(retry_at),
        }),
        (None, None, None) => None,
        _ => return Err(StoreError::corrupt("unpaired search I/O pause fields")),
    };
    Ok(SearchIndexStatus {
        epoch,
        user_paused: user_paused != 0,
        io_pause,
        io_attempts: attempts,
    })
}

/// The `search_index_state` row, for coverage/progress reads.
pub fn search_index_status(conn: &Connection) -> Result<SearchIndexStatus, StoreError> {
    status(conn)
}

type JobRow = (String, i64, String, i64, i64, i64);

fn job_row(row: JobRow) -> Result<RebuildJob, StoreError> {
    let (job_id, epoch, phase, doc_cursor, acq_cursor, phase_count) = row;
    Ok(RebuildJob {
        job_id,
        epoch,
        phase: RebuildPhase::parse(&phase)?,
        doc_cursor,
        acq_cursor,
        phase_count,
    })
}

const JOB_COLUMNS: &str = "job_id, epoch, phase, doc_cursor, acq_cursor, phase_count";

fn query_job(
    conn: &Connection,
    sql: &str,
    args: impl rusqlite::Params,
) -> Result<Option<RebuildJob>, StoreError> {
    conn.query_row(sql, args, |row| {
        Ok((
            row.get::<_, String>(0)?,
            row.get::<_, i64>(1)?,
            row.get::<_, String>(2)?,
            row.get::<_, i64>(3)?,
            row.get::<_, i64>(4)?,
            row.get::<_, i64>(5)?,
        ))
    })
    .optional()
    .map_err(sql_error)?
    .map(job_row)
    .transpose()
}

/// A rebuild job by id. An id naming an import job is `InvalidInput`;
/// an id in neither table is `NotFound` (spec/07 kind dispatch).
pub fn rebuild_job(conn: &Connection, id: &str) -> Result<RebuildJob, StoreError> {
    match job_kind(conn, id)? {
        JobKind::SearchRebuild => query_job(
            conn,
            &format!("SELECT {JOB_COLUMNS} FROM search_rebuild_jobs WHERE job_id = ?1"),
            [id],
        )?
        .ok_or_else(|| StoreError::corrupt("search rebuild job vanished")),
        JobKind::Import => Err(invalid("job id names an import job, not a search rebuild")),
    }
}

/// The active or most recently completed rebuild: the highest epoch.
/// Kept completed rows let old ids answer cancel/resume harmlessly.
pub fn latest_rebuild(conn: &Connection) -> Result<Option<RebuildJob>, StoreError> {
    query_job(
        conn,
        &format!("SELECT {JOB_COLUMNS} FROM search_rebuild_jobs ORDER BY epoch DESC LIMIT 1"),
        [],
    )
}

/// The fence a pending-batch read captures: current epoch and target.
pub(crate) fn fence(conn: &Connection) -> Result<IndexFence, StoreError> {
    Ok(IndexFence {
        epoch: status(conn)?.epoch,
        version: target_version(conn)?,
    })
}

/// Whether the writer may publish under `fence` (spec/06): epoch and
/// target still current, neither pause set, and either no live rebuild
/// or the latest job is in `index` at this epoch. Publication is
/// refused during `reset`/`enqueue` even when the revision still matches.
pub(crate) fn publication_open(tx: &Transaction, fence: IndexFence) -> Result<bool, StoreError> {
    if target_version(tx)? != fence.version {
        return Ok(false);
    }
    let state = status(tx)?;
    if state.epoch != fence.epoch || state.paused() {
        return Ok(false);
    }
    Ok(match latest_rebuild(tx)? {
        Some(job) if job.active() => job.phase == RebuildPhase::Index,
        _ => true,
    })
}

/// Start-or-restart (spec/06, 07): short metadata only, with no corpus
/// scan, clear or enqueue. Same-version start while a job is active
/// returns that job untouched (no reset, epoch change or resume); a
/// higher target restarts it at `reset` with a fresh epoch and the same
/// id, preserving pauses. After completion a start takes a fresh id and
/// epoch. The stored target is never downgraded. Returns the job id.
pub fn start_search_rebuild(
    tx: &Transaction,
    job_id: &str,
    target: u32,
) -> Result<String, StoreError> {
    if job_id.is_empty() {
        return Err(invalid("a rebuild job id is required"));
    }
    let stored = target_version(tx)?;
    if target < stored {
        return Err(invalid(
            "rebuild target is below the stored extractor version",
        ));
    }
    if let Some(job) = latest_rebuild(tx)?
        && job.active()
    {
        if target == stored {
            return Ok(job.job_id);
        }
        let epoch = next_epoch(tx)?;
        tx.execute(
            "UPDATE search_rebuild_jobs SET epoch = ?2, phase = 'reset',
               doc_cursor = 0, acq_cursor = 0, phase_count = 0
             WHERE job_id = ?1",
            params![job.job_id, epoch],
        )
        .map_err(sql_error)?;
        set_target_version(tx, target)?;
        advance_generation(tx)?;
        return Ok(job.job_id);
    }
    match job_kind(tx, job_id) {
        Err(StoreError::NotFound { .. }) => (),
        Ok(_) => return Err(invalid("job id already names a job")),
        Err(e) => return Err(e),
    }
    let epoch = next_epoch(tx)?;
    tx.execute(
        "INSERT INTO search_rebuild_jobs (job_id, epoch, phase) VALUES (?1, ?2, 'reset')",
        params![job_id, epoch],
    )
    .map_err(sql_error)?;
    if target > stored {
        set_target_version(tx, target)?;
    }
    advance_generation(tx)?;
    Ok(job_id.to_owned())
}

/// The next monotonic rebuild epoch, committed on `search_index_state`.
fn next_epoch(tx: &Transaction) -> Result<i64, StoreError> {
    let epoch = status(tx)?
        .epoch
        .checked_add(1)
        .ok_or_else(|| StoreError::corrupt("rebuild epoch exhausted"))?;
    tx.execute(
        "UPDATE search_index_state SET epoch = ?1 WHERE id = 0",
        [epoch],
    )
    .map_err(sql_error)?;
    Ok(epoch)
}

fn set_target_version(tx: &Transaction, target: u32) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE store_meta SET value = ?1 WHERE key = 'search_extractor_version'",
        [target.to_string()],
    )
    .map_err(sql_error)?;
    Ok(())
}

/// The user's hold on index work (spec/07). Survives shutdown, new
/// enqueues and version restarts; only `Resume` releases it, and it
/// never touches the I/O pause.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UserPause {
    Pause,
    Resume,
}

/// Set or release the durable user pause. Commits status and advances
/// `search_generation` once, even with no queue change (spec/07).
pub fn set_user_pause(tx: &Transaction, pause: UserPause) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE search_index_state SET user_paused = ?1 WHERE id = 0",
        [matches!(pause, UserPause::Pause) as i64],
    )
    .map_err(sql_error)?;
    advance_generation(tx)
}

/// Record an I/O pause with its actionable message, UTC-millisecond
/// deadline and bounded-backoff attempt count (spec/07). `attempts`
/// persists across clears so consecutive failures keep backing off.
/// Coexists with the user pause; never clears it.
pub fn pause_for_io(
    tx: &Transaction,
    code: SearchIoCode,
    message: &str,
    retry_at: Timestamp,
    attempts: u32,
) -> Result<(), StoreError> {
    if message.len() > MAX_IO_MESSAGE_BYTES || retry_at.as_millis() < 0 {
        return Err(invalid("invalid search I/O pause"));
    }
    tx.execute(
        "UPDATE search_index_state
         SET io_code = ?1, io_message = ?2, io_retry_at = ?3, io_attempts = ?4
         WHERE id = 0",
        params![
            code.as_str(),
            message,
            retry_at.as_millis(),
            attempts as i64
        ],
    )
    .map_err(sql_error)?;
    advance_generation(tx)
}

/// Leave the I/O pause when a retry starts. Keeps the attempt counter
/// and never touches the user pause. Advances `search_generation`.
pub fn clear_io_pause(tx: &Transaction) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE search_index_state
         SET io_code = NULL, io_message = NULL, io_retry_at = NULL
         WHERE id = 0",
        [],
    )
    .map_err(sql_error)?;
    advance_generation(tx)
}

#[cfg(test)]
mod tests;
