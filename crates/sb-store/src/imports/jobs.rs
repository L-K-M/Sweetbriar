use rusqlite::{OptionalExtension, Transaction, params};

use super::{INITIAL_CHECKPOINT, canonical_object, invalid, sql_error};
use crate::{StoreError, Timestamp};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ImportJobState {
    Inventory,
    AwaitingConfirmation,
    Running,
    Completed,
    Cancelled,
    Failed,
}

impl ImportJobState {
    fn name(self) -> &'static str {
        match self {
            Self::Inventory => "inventory",
            Self::AwaitingConfirmation => "awaiting_confirmation",
            Self::Running => "running",
            Self::Completed => "completed",
            Self::Cancelled => "cancelled",
            Self::Failed => "failed",
        }
    }

    fn parse(state: &str) -> Result<Self, StoreError> {
        match state {
            "inventory" => Ok(Self::Inventory),
            "awaiting_confirmation" => Ok(Self::AwaitingConfirmation),
            "running" => Ok(Self::Running),
            "completed" => Ok(Self::Completed),
            "cancelled" => Ok(Self::Cancelled),
            "failed" => Ok(Self::Failed),
            _ => Err(StoreError::corrupt("unknown import job state")),
        }
    }

    fn terminal(self) -> bool {
        matches!(self, Self::Completed | Self::Cancelled | Self::Failed)
    }

    fn permits(self, next: Self) -> bool {
        if self.terminal() {
            return false;
        }
        if matches!(next, Self::Cancelled | Self::Failed) {
            return true;
        }
        matches!(
            (self, next),
            (
                Self::Inventory,
                Self::Inventory | Self::AwaitingConfirmation
            ) | (Self::AwaitingConfirmation, Self::Running)
                | (Self::Running, Self::Running | Self::Completed)
        )
    }
}

#[derive(Debug, Clone)]
pub struct ImportJob {
    id: String,
    state: ImportJobState,
    request_json: String,
    checkpoint_json: String,
    revision: i64,
}

impl ImportJob {
    pub fn id(&self) -> &str {
        &self.id
    }
    pub fn state(&self) -> ImportJobState {
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
}

#[derive(Debug, Clone)]
pub struct ImportJobSource {
    source_json: String,
    checkpoint_json: String,
}

impl ImportJobSource {
    pub fn source_json(&self) -> &str {
        &self.source_json
    }
    pub fn checkpoint_json(&self) -> &str {
        &self.checkpoint_json
    }
}

pub fn create_import_job(
    tx: &Transaction,
    id: &str,
    request_json: &str,
    now: Timestamp,
) -> Result<ImportJob, StoreError> {
    if id.is_empty() {
        return Err(invalid("an import job id is required"));
    }
    let request = canonical_object(request_json)?;
    tx.execute(
        "INSERT INTO import_jobs
         (job_id, state, request_json, checkpoint_json, created_at, updated_at)
         VALUES (?1, 'inventory', ?2, ?3, ?4, ?4)",
        params![id, request, INITIAL_CHECKPOINT, now.as_millis()],
    )
    .map_err(sql_error)?;
    import_job(tx, id)
}

pub fn import_job(tx: &Transaction, id: &str) -> Result<ImportJob, StoreError> {
    let row = tx.query_row(
        "SELECT state, request_json, checkpoint_json, revision FROM import_jobs WHERE job_id = ?1",
        [id],
        |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?,
                  row.get::<_, String>(2)?, row.get::<_, i64>(3)?)),
    ).optional().map_err(sql_error)?;
    let (state, request_json, checkpoint_json, revision) =
        row.ok_or_else(|| StoreError::not_found("import job"))?;
    Ok(ImportJob {
        id: id.to_owned(),
        state: ImportJobState::parse(&state)?,
        request_json,
        checkpoint_json,
        revision,
    })
}

/// Recover only unfinished work. Awaiting-confirmation jobs remain idle.
pub fn recoverable_import_jobs(tx: &Transaction) -> Result<Vec<ImportJob>, StoreError> {
    let mut statement = tx.prepare(
        "SELECT job_id FROM import_jobs WHERE state IN ('inventory','awaiting_confirmation','running')
         ORDER BY created_at, job_id",
    ).map_err(sql_error)?;
    let ids = statement
        .query_map([], |row| row.get::<_, String>(0))
        .map_err(sql_error)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(sql_error)?;
    ids.iter().map(|id| import_job(tx, id)).collect()
}

pub fn advance_import_job(
    tx: &Transaction,
    id: &str,
    expected_revision: i64,
    next: ImportJobState,
    checkpoint_json: &str,
    now: Timestamp,
) -> Result<ImportJob, StoreError> {
    let current = import_job(tx, id)?;
    if current.revision != expected_revision || !current.state.permits(next) {
        return Err(StoreError::conflict(
            "import job changed or transition is invalid",
        ));
    }
    let checkpoint = canonical_object(checkpoint_json)?;
    let revision = expected_revision
        .checked_add(1)
        .ok_or_else(|| StoreError::corrupt("import job revision overflow"))?;
    tx.execute(
        "UPDATE import_jobs SET state = ?2, checkpoint_json = ?3, revision = ?4, updated_at = ?5
         WHERE job_id = ?1 AND revision = ?6",
        params![
            id,
            next.name(),
            checkpoint,
            revision,
            now.as_millis(),
            expected_revision
        ],
    )
    .map_err(sql_error)?;
    import_job(tx, id)
}

pub fn record_import_source(
    tx: &Transaction,
    job_id: &str,
    ordinal: u64,
    source_json: &str,
) -> Result<(), StoreError> {
    if import_job(tx, job_id)?.state != ImportJobState::Inventory {
        return Err(StoreError::conflict("inventory is already closed"));
    }
    let ordinal = source_ordinal(ordinal)?;
    let source = canonical_object(source_json)?;
    tx.execute(
        "INSERT INTO import_job_sources (job_id, ordinal, source_json, checkpoint_json)
         VALUES (?1, ?2, ?3, ?4) ON CONFLICT(job_id, ordinal) DO NOTHING",
        params![job_id, ordinal, source, INITIAL_CHECKPOINT],
    )
    .map_err(sql_error)?;
    if import_job_source(tx, job_id, ordinal as u64)?.source_json != source {
        return Err(StoreError::conflict("import source inventory changed"));
    }
    Ok(())
}

pub fn import_job_source(
    tx: &Transaction,
    job_id: &str,
    ordinal: u64,
) -> Result<ImportJobSource, StoreError> {
    tx.query_row(
        "SELECT source_json, checkpoint_json FROM import_job_sources WHERE job_id = ?1 AND ordinal = ?2",
        params![job_id, source_ordinal(ordinal)?],
        |row| Ok(ImportJobSource { source_json: row.get(0)?, checkpoint_json: row.get(1)? }),
    ).optional().map_err(sql_error)?.ok_or_else(|| StoreError::not_found("import job source"))
}

pub(super) fn source_progress(
    tx: &Transaction,
    job_id: &str,
    ordinal: u64,
    checkpoint: &str,
) -> Result<(), StoreError> {
    let checkpoint = canonical_object(checkpoint)?;
    let changed = tx
        .execute(
            "UPDATE import_job_sources SET checkpoint_json = ?3 WHERE job_id = ?1 AND ordinal = ?2",
            params![job_id, source_ordinal(ordinal)?, checkpoint],
        )
        .map_err(sql_error)?;
    if changed == 0 {
        return Err(StoreError::not_found("import job source"));
    }
    Ok(())
}

fn source_ordinal(ordinal: u64) -> Result<i64, StoreError> {
    i64::try_from(ordinal).map_err(|_| invalid("import source ordinal exceeds SQLite's range"))
}
