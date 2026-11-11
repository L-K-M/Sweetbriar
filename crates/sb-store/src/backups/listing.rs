//! The recorded generations as `list_backups` shows them (spec/07
//! "Commands", plan M1.8i1): the rows as stored, newest first.
//!
//! Nothing here opens a generation's directory. An entry says what the
//! store recorded; whether the bytes are still there is the weekly
//! check's verdict on the row and, when one is chosen, a restore's
//! verification.

use std::path::PathBuf;

use rusqlite::Connection;

use super::GenerationVerdict;
use crate::schema::classify;
use crate::{StoreError, Timestamp};

/// How many failed generations a listing names at most. A failed row
/// stays as evidence (spec/03) and nothing ever removes one, so their
/// number has no bound; complete ones are bounded by retention.
pub const FAILED_GENERATIONS_LISTED: usize = 20;

/// A generation's recorded state (spec/03 "Backup generations").
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum BackupGenerationState {
    /// Its copy runs, or stopped part way and waits for the next
    /// backup to fail it and remove what it left.
    InProgress,
    /// Published: the manifest was written last and the row says so.
    Complete,
    /// Interrupted. Its directory is removed; the row stays.
    Failed,
}

impl BackupGenerationState {
    fn parse(text: &str) -> Option<Self> {
        match text {
            "in_progress" => Some(Self::InProgress),
            "complete" => Some(Self::Complete),
            "failed" => Some(Self::Failed),
            _ => None,
        }
    }
}

/// The weekly check's latest verdict on a generation, and when it was
/// reached.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct GenerationVerification {
    pub verdict: GenerationVerdict,
    pub checked_at: Timestamp,
}

/// One recorded backup generation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct BackupGeneration {
    pub generation_id: String,
    pub state: BackupGenerationState,
    pub started_at: Timestamp,
    /// When it was published.
    pub completed_at: Option<Timestamp>,
    /// The generation's directory as recorded. A restore rewrites the
    /// ones inside the data root to the preserved root (spec/03).
    pub location: PathBuf,
    /// The SHA-256 of its manifest, 64 hex digits; with the two counts,
    /// what a restore's staging checks the manifest against.
    pub manifest_sha256: Option<String>,
    pub object_count: Option<u64>,
    /// The database's bytes plus every object's.
    pub total_bytes: Option<u64>,
    /// `None` until the weekly check has read it.
    pub verification: Option<GenerationVerification>,
}

/// Every complete and in-progress generation, and the failed ones
/// started since the newest complete one (the newest
/// [`FAILED_GENERATIONS_LISTED`] of them): newest first, a tie by the
/// greater id first, as retention breaks it.
pub(crate) fn generations(conn: &Connection) -> Result<Vec<BackupGeneration>, StoreError> {
    let what = "list the backup generations";
    let mut statement = conn
        .prepare(
            "SELECT generation_id, state, started_at, completed_at, location,
                    manifest_sha256, object_count, total_bytes, verified_at, verification
             FROM backup_generations
             ORDER BY started_at DESC, generation_id DESC",
        )
        .map_err(|e| classify(e, what))?;
    let mut rows = statement.query([]).map_err(|e| classify(e, what))?;

    let mut listed = Vec::new();
    let mut failed = 0;
    let mut complete_seen = false;
    while let Some(row) = rows.next().map_err(|e| classify(e, what))? {
        let read = |e| classify(e, what);
        let generation_id: String = row.get(0).map_err(read)?;
        let corrupt = || {
            StoreError::corrupt(format!(
                "backup_generations row {generation_id:?} is malformed"
            ))
        };
        let state: String = row.get(1).map_err(read)?;
        let state = BackupGenerationState::parse(&state).ok_or_else(corrupt)?;
        match state {
            BackupGenerationState::Complete => complete_seen = true,
            BackupGenerationState::InProgress => {}
            BackupGenerationState::Failed => {
                // Left out before the rest of the row is read: a row
                // the list doesn't show can't make it `Corrupt`, and
                // the list is how a restore's generation is chosen.
                if complete_seen || failed == FAILED_GENERATIONS_LISTED {
                    continue;
                }
                failed += 1;
            }
        }
        let count = |index| -> Result<Option<u64>, StoreError> {
            let stored: Option<i64> = row.get(index).map_err(read)?;
            stored
                .map(|value| u64::try_from(value).map_err(|_| corrupt()))
                .transpose()
        };
        let verified_at: Option<i64> = row.get(8).map_err(read)?;
        let verdict: Option<String> = row.get(9).map_err(read)?;
        let verification = match (verified_at, verdict) {
            (None, None) => None,
            (Some(at), Some(verdict)) => Some(GenerationVerification {
                verdict: GenerationVerdict::parse(&verdict).ok_or_else(corrupt)?,
                checked_at: Timestamp::from_millis(at),
            }),
            _ => return Err(corrupt()),
        };
        let completed_at: Option<i64> = row.get(3).map_err(read)?;
        listed.push(BackupGeneration {
            state,
            started_at: Timestamp::from_millis(row.get(2).map_err(read)?),
            completed_at: completed_at.map(Timestamp::from_millis),
            location: PathBuf::from(row.get::<_, String>(4).map_err(read)?),
            manifest_sha256: row.get(5).map_err(read)?,
            object_count: count(6)?,
            total_bytes: count(7)?,
            verification,
            generation_id,
        });
    }
    Ok(listed)
}

#[cfg(test)]
mod tests;
