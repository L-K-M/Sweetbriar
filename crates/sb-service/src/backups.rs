//! `list_backups()` and `start_backup()` (spec/07 "Commands", Backup;
//! plan M1.8i1, M1.8i3): the backup generations the store recorded, as
//! the shell lists them, and one taken now.

use std::path::PathBuf;

use crate::{Core, CoreError, Receipt};

/// What `start_backup` answers: the command's stored receipt and the
/// generation it names.
#[derive(Debug)]
pub struct BackupStarted {
    pub receipt: Receipt,
    /// The generation the key started, now or when it was first used.
    pub generation_id: String,
    /// The copy this call started, to wait on. `None` when the key had
    /// started its generation before: nothing was started now, and
    /// `list_backups` says what became of that one.
    pub copy: Option<BackupCopy>,
}

/// A started backup's copy. It runs in the core whoever holds this:
/// dropping it, or the wait, stops nothing.
#[derive(Debug)]
pub struct BackupCopy {
    copy: sb_engine::BackupCopy,
}

impl BackupCopy {
    /// Wait for the copy to end, and learn how.
    pub async fn finished(self) -> BackupEnd {
        BackupEnd::from_engine(self.copy.finished().await)
    }
}

/// How a started backup's copy ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupEnd {
    /// The generation is complete. `after` is an error from the work
    /// that follows completion (retention): the generation stands, and
    /// the next backup tries that work again.
    Complete { after: Option<String> },
    /// A shutdown stopped the copy at its next object. The generation
    /// stays in progress until the next backup marks it failed and
    /// removes what it left.
    Stopped,
    /// The copy failed, with the error's text. A core that turned
    /// read-only under the copy fails it, and the text names the
    /// finding. The generation is left like a stopped one's.
    Failed(String),
}

impl BackupEnd {
    fn from_engine(end: sb_engine::BackupEnd) -> Self {
        match end {
            sb_engine::BackupEnd::Complete { after } => Self::Complete { after },
            sb_engine::BackupEnd::Stopped => Self::Stopped,
            sb_engine::BackupEnd::Failed(detail) => Self::Failed(detail),
        }
    }
}

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

    /// `start_backup()` (spec/07): take a backup generation now, in
    /// the data root's `backups/`. It answers once the generation's
    /// snapshot is taken; the copy goes on in the core, and
    /// [`BackupCopy::finished`] waits for it.
    ///
    /// A repeated key answers its receipt and starts nothing.
    /// `Conflict` while another backup, a generation check or a
    /// restore is under way; `Corrupt` on a read-only core, which
    /// refuses before it looks at the key.
    pub async fn start_backup(&self, idempotency_key: &str) -> Result<BackupStarted, CoreError> {
        let started: sb_engine::BackupStarted = self.engine().start_backup(idempotency_key).await?;
        Ok(BackupStarted {
            receipt: Receipt::from_engine(started.receipt()),
            generation_id: started.generation_id().to_owned(),
            copy: started.into_copy().map(|copy| BackupCopy { copy }),
        })
    }
}

#[cfg(test)]
mod tests;
