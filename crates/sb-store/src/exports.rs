//! Durable export capture, independent of scope resolution and file output.
//! Payload chunks are immutable; every mutation runs on the sole writer.

use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::Value;

use crate::{StoreError, Timestamp};

mod stream;
pub use stream::ExportChunkWriter;

pub const MAX_EXPORT_CHUNK_BYTES: usize = 65_536;
pub const MAX_EXPORT_CHUNKS: usize = 30;
pub const MAX_EXPORT_JSON_BYTES: usize = 4_096;
pub const MAX_EXPORT_CHANGED_BYTES: usize = 2_097_152;
const MAX_JOB_ID_BYTES: usize = 128;
const FORMAT_VERSION: u64 = 1;
const INITIAL_CHECKPOINT: &str = "{\"v\":1}";
// Two maximum job images (request/checkpoint, key/index and framing),
// plus each chunk's payload, duplicated job key and row/index framing.
const JOB_IMAGE_RESERVE: usize = 32_768;
const CHUNK_IMAGE_FRAMING: usize = 1_024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ExportJobState {
    Capturing,
    Ready,
    Running,
    Completed,
    Cancelled,
    Failed,
}

impl ExportJobState {
    fn name(self) -> &'static str {
        match self {
            Self::Capturing => "capturing",
            Self::Ready => "ready",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }

    fn parse(name: &str) -> Result<Self, StoreError> {
        match name {
            "capturing" => Ok(Self::Capturing),
            "ready" => Ok(Self::Ready),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "cancelled" => Ok(Self::Cancelled),
            "failed" => Ok(Self::Failed),
            _ => Err(StoreError::corrupt("unknown export job state")),
        }
    }

    fn permits(self, next: Self) -> bool {
        if matches!(self, Self::Completed | Self::Cancelled | Self::Failed) {
            return false;
        }
        matches!(next, Self::Cancelled | Self::Failed)
            || matches!(
                (self, next),
                (Self::Capturing, Self::Capturing | Self::Ready)
                    | (Self::Ready, Self::Running)
                    | (Self::Running, Self::Running | Self::Completed)
            )
    }
}

#[derive(Debug, Clone)]
pub struct ExportJob {
    id: String,
    state: ExportJobState,
    request_json: String,
    checkpoint_json: String,
    revision: i64,
    captured_chunks: u64,
}

impl ExportJob {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn state(&self) -> ExportJobState {
        self.state
    }
    pub fn request_json(&self) -> &str {
        &self.request_json
    }
    pub fn checkpoint_json(&self) -> &str {
        &self.checkpoint_json
    }
    pub fn revision(&self) -> i64 {
        self.revision
    }
    pub fn captured_chunks(&self) -> u64 {
        self.captured_chunks
    }
}

/// Creation replay preserves the original capture and its progress.
pub fn create_export_job(
    tx: &Transaction,
    id: &str,
    request_json: &str,
    now: Timestamp,
) -> Result<ExportJob, StoreError> {
    if id.is_empty() || id.len() > MAX_JOB_ID_BYTES {
        return Err(invalid(
            "an export job id must be 1 through 128 UTF-8 bytes",
        ));
    }
    let request = canonical_object(request_json)?;
    match crate::index::rebuild::job_kind(tx, id) {
        Ok(crate::index::rebuild::JobKind::Export) => {
            let job = export_job(tx, id)?;
            if job.request_json != request {
                return Err(StoreError::conflict("export job request changed"));
            }
            return Ok(job);
        }
        Ok(_) => return Err(StoreError::conflict("job id names another job kind")),
        Err(StoreError::NotFound { .. }) => {}
        Err(error) => return Err(error),
    }
    tx.execute(
        "INSERT INTO export_jobs
         (job_id, state, request_json, checkpoint_json, created_at, updated_at)
         VALUES (?1, 'capturing', ?2, ?3, ?4, ?4)",
        params![id, request, INITIAL_CHECKPOINT, now.as_millis()],
    )
    .map_err(sql_error)?;
    export_job(tx, id)
}

pub fn export_job(tx: &Transaction, id: &str) -> Result<ExportJob, StoreError> {
    let row = tx
        .query_row(
            "SELECT state, request_json, checkpoint_json, revision, captured_chunks,
             (SELECT coalesce(max(ordinal) + 1, 0) FROM export_job_chunks WHERE job_id = ?1)
             FROM export_jobs WHERE job_id = ?1",
            [id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )
        .optional()
        .map_err(sql_error)?;
    let (state, request_json, checkpoint_json, revision, chunks, tail) =
        row.ok_or_else(|| StoreError::not_found("export job"))?;
    // Append-only SQL guards make the indexed tail a complete count witness.
    if chunks != tail {
        return Err(StoreError::corrupt(
            "export chunk count disagrees with its payload",
        ));
    }
    Ok(ExportJob {
        id: id.to_owned(),
        state: ExportJobState::parse(&state)?,
        request_json,
        checkpoint_json,
        revision,
        captured_chunks: u64::try_from(chunks)
            .map_err(|_| StoreError::corrupt("negative export chunk count"))?,
    })
}

/// Append a bounded range, or verify an exact replay without rewriting it.
/// A partially overlapping range must be retried as its original batches.
/// Propagate errors from the writer closure so its whole transaction rolls back.
pub fn append_export_chunks(
    tx: &Transaction,
    id: &str,
    expected_revision: i64,
    from_ordinal: u64,
    chunks: &[Vec<u8>],
    now: Timestamp,
) -> Result<ExportJob, StoreError> {
    if chunks.is_empty() || chunks.len() > MAX_EXPORT_CHUNKS {
        return Err(invalid("an export append holds 1 through 30 chunks"));
    }
    let mut charge = JOB_IMAGE_RESERVE;
    for chunk in chunks {
        if chunk.is_empty() || chunk.len() > MAX_EXPORT_CHUNK_BYTES {
            return Err(invalid("an export chunk holds 1 through 65536 bytes"));
        }
        charge = charge
            .checked_add(chunk.len() + 2 * id.len() + CHUNK_IMAGE_FRAMING)
            .ok_or_else(|| invalid("export append byte charge overflow"))?;
    }
    if charge > MAX_EXPORT_CHANGED_BYTES {
        return Err(invalid("export append exceeds the changed-byte budget"));
    }
    let job = export_job(tx, id)?;
    let end = from_ordinal
        .checked_add(chunks.len() as u64)
        .filter(|end| *end <= i64::MAX as u64)
        .ok_or_else(|| invalid("export chunk ordinal exceeds SQLite's range"))?;
    if from_ordinal < job.captured_chunks {
        if end > job.captured_chunks
            || export_chunks(tx, id, from_ordinal, chunks.len() as u32)? != chunks
        {
            return Err(StoreError::conflict("export payload replay changed"));
        }
        return Ok(job);
    }
    if from_ordinal != job.captured_chunks {
        return Err(invalid("export append skips an ordinal"));
    }
    if job.state != ExportJobState::Capturing || job.revision != expected_revision {
        return Err(StoreError::conflict("export capture changed or is sealed"));
    }
    let revision = next_revision(&job)?;
    let mut statement = tx
        .prepare_cached(
            "INSERT INTO export_job_chunks (job_id, ordinal, payload) VALUES (?1, ?2, ?3)",
        )
        .map_err(sql_error)?;
    for (offset, chunk) in chunks.iter().enumerate() {
        statement
            .execute(params![id, (from_ordinal + offset as u64) as i64, chunk])
            .map_err(sql_error)?;
    }
    tx.execute(
        "UPDATE export_jobs SET captured_chunks = ?2, revision = ?3, updated_at = ?4
         WHERE job_id = ?1",
        params![id, end as i64, revision, now.as_millis()],
    )
    .map_err(sql_error)?;
    export_job(tx, id)
}

pub fn advance_export_job(
    tx: &Transaction,
    id: &str,
    expected_revision: i64,
    next: ExportJobState,
    checkpoint_json: &str,
    now: Timestamp,
) -> Result<ExportJob, StoreError> {
    let job = export_job(tx, id)?;
    if job.revision != expected_revision || !job.state.permits(next) {
        return Err(StoreError::conflict(
            "export job changed or transition is invalid",
        ));
    }
    let checkpoint = canonical_object(checkpoint_json)?;
    tx.execute(
        "UPDATE export_jobs SET state = ?2, checkpoint_json = ?3, revision = ?4, updated_at = ?5
         WHERE job_id = ?1",
        params![
            id,
            next.name(),
            checkpoint,
            next_revision(&job)?,
            now.as_millis()
        ],
    )
    .map_err(sql_error)?;
    export_job(tx, id)
}

/// Bounded ordinal paging, also available while capture is incomplete.
pub fn export_chunks(
    tx: &Transaction,
    id: &str,
    from_ordinal: u64,
    limit: u32,
) -> Result<Vec<Vec<u8>>, StoreError> {
    if !(1..=MAX_EXPORT_CHUNKS as u32).contains(&limit) {
        return Err(invalid("an export page holds 1 through 30 chunks"));
    }
    let job = export_job(tx, id)?;
    if from_ordinal > job.captured_chunks {
        return Err(invalid("export page starts past the captured end"));
    }
    let mut statement = tx
        .prepare_cached(
            "SELECT payload FROM export_job_chunks WHERE job_id = ?1 AND ordinal >= ?2
             ORDER BY ordinal LIMIT ?3",
        )
        .map_err(sql_error)?;
    statement
        .query_map(params![id, from_ordinal as i64, limit], |row| row.get(0))
        .map_err(sql_error)?
        .collect::<Result<_, _>>()
        .map_err(sql_error)
}

fn canonical_object(json: &str) -> Result<String, StoreError> {
    if json.len() > MAX_EXPORT_JSON_BYTES {
        return Err(invalid("export JSON exceeds 4096 UTF-8 bytes"));
    }
    let value: Value =
        serde_json::from_str(json).map_err(|error| invalid(format!("export JSON: {error}")))?;
    if !value.is_object() || value.get("v").and_then(Value::as_u64) != Some(FORMAT_VERSION) {
        return Err(invalid("export JSON must be an object with v: 1"));
    }
    let canonical = value.to_string();
    if canonical.len() > MAX_EXPORT_JSON_BYTES {
        return Err(invalid("canonical export JSON exceeds 4096 UTF-8 bytes"));
    }
    Ok(canonical)
}

fn next_revision(job: &ExportJob) -> Result<i64, StoreError> {
    job.revision
        .checked_add(1)
        .ok_or_else(|| StoreError::corrupt("export job revision overflow"))
}

fn invalid(message: impl Into<String>) -> StoreError {
    StoreError::InvalidInput {
        message: message.into(),
    }
}

fn sql_error(error: rusqlite::Error) -> StoreError {
    crate::schema::classify(error, "export capture storage")
}

#[cfg(test)]
mod tests;
