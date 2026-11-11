//! `list_backups()` (spec/07 "Commands", Backup; plan M1.8i1): the
//! backup generations the store recorded, as the shell lists them.

use std::path::PathBuf;

use crate::{Core, CoreError};

/// One backup generation as its row stands (spec/03 "Backup
/// generations"). Nothing here was read from the generation's
/// directory: `verification` is the weekly check's last word on its
/// bytes, and a restore verifies the one it is given again.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupGeneration {
    pub generation_id: String,
    pub state: BackupState,
    pub started_at_ms: i64,
    /// When it was published; absent until it is complete.
    pub completed_at_ms: Option<i64>,
    /// The generation's directory. A failed generation's is removed,
    /// and this is where it was; a restore moves the ones inside the
    /// data root along with the replaced store.
    pub location: PathBuf,
    /// How many objects it holds; absent until it is complete.
    pub object_count: Option<u64>,
    /// Its database's bytes plus every object's; absent until it is
    /// complete.
    pub total_bytes: Option<u64>,
    /// Absent until the weekly check has read it.
    pub verification: Option<BackupVerification>,
}

/// A generation's recorded state.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupState {
    /// Its copy runs, or was interrupted and waits for the next backup
    /// to fail it and remove what it left.
    InProgress,
    /// Published. Only a complete generation can be restored.
    Complete,
    /// Interrupted and cleaned up. The row stays as a record.
    Failed,
}

/// The weekly check's latest verdict on a generation's bytes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct BackupVerification {
    pub verdict: BackupVerdict,
    pub checked_at_ms: i64,
}

/// How a generation's bytes held up against its manifest.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackupVerdict {
    /// The manifest, the database and every object match.
    Intact,
    /// A file differs from the manifest.
    Damaged,
    /// The directory or a file in it is gone.
    Missing,
}

impl BackupGeneration {
    fn from_engine(generation: sb_engine::BackupGeneration) -> Self {
        // Named, so the engine's item re-exports are what this compiles
        // against (spec/01 rule 3), not inferred leaf types.
        let millis = |at: sb_engine::Timestamp| at.as_millis();
        let state = match generation.state {
            sb_engine::BackupGenerationState::InProgress => BackupState::InProgress,
            sb_engine::BackupGenerationState::Complete => BackupState::Complete,
            sb_engine::BackupGenerationState::Failed => BackupState::Failed,
        };
        let verification =
            generation
                .verification
                .map(
                    |verification: sb_engine::GenerationVerification| BackupVerification {
                        verdict: match verification.verdict {
                            sb_engine::GenerationVerdict::Intact => BackupVerdict::Intact,
                            sb_engine::GenerationVerdict::Damaged => BackupVerdict::Damaged,
                            sb_engine::GenerationVerdict::Missing => BackupVerdict::Missing,
                        },
                        checked_at_ms: millis(verification.checked_at),
                    },
                );
        Self {
            generation_id: generation.generation_id,
            state,
            started_at_ms: millis(generation.started_at),
            completed_at_ms: generation.completed_at.map(millis),
            location: generation.location,
            object_count: generation.object_count,
            total_bytes: generation.total_bytes,
            verification,
        }
    }
}

impl Core {
    /// `list_backups()` (spec/07): the recorded generations, newest
    /// first. Every complete and in-progress one is listed, and the
    /// failed ones since the newest complete one, at most twenty. A
    /// read-only core answers like any other.
    pub async fn list_backups(&self) -> Result<Vec<BackupGeneration>, CoreError> {
        let generations = self.engine().list_backups().await?;
        Ok(generations
            .into_iter()
            .map(BackupGeneration::from_engine)
            .collect())
    }
}

#[cfg(test)]
mod tests;
