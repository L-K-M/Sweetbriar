//! Owned native backup listings (spec/07; M1.8j2). Rows and retained
//! names only: no generation/entry is opened or qualified here.

use std::path::PathBuf;
use std::sync::Arc;

use crate::{CancelToken, Core, CoreError};

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum BackupState {
    InProgress,
    Complete,
    Failed,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum BackupVerdict {
    Intact,
    Damaged,
    Missing,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Record)]
pub struct BackupVerification {
    pub verdict: BackupVerdict,
    pub checked_at_ms: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct BackupGeneration {
    pub generation_id: String,
    pub state: BackupState,
    pub started_at_ms: i64,
    pub completed_at_ms: Option<i64>,
    pub location: String,
    pub object_count: Option<u64>,
    pub total_bytes: Option<u64>,
    pub verification: Option<BackupVerification>,
}

impl BackupGeneration {
    fn from_service(value: sb_service::BackupGeneration) -> Result<Self, CoreError> {
        Ok(Self {
            generation_id: value.generation_id,
            state: match value.state {
                sb_service::BackupState::InProgress => BackupState::InProgress,
                sb_service::BackupState::Complete => BackupState::Complete,
                sb_service::BackupState::Failed => BackupState::Failed,
            },
            started_at_ms: value.started_at_ms,
            completed_at_ms: value.completed_at_ms,
            location: path_text(value.location)?,
            object_count: value.object_count,
            total_bytes: value.total_bytes,
            verification: value.verification.map(|verification| BackupVerification {
                verdict: match verification.verdict {
                    sb_service::BackupVerdict::Intact => BackupVerdict::Intact,
                    sb_service::BackupVerdict::Damaged => BackupVerdict::Damaged,
                    sb_service::BackupVerdict::Missing => BackupVerdict::Missing,
                },
                checked_at_ms: verification.checked_at_ms,
            }),
        })
    }
}

/// A name, not proof of ownership, type, completeness or readiness.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct RestoreArtifact {
    pub path: String,
    pub kind: RestoreArtifactKind,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum RestoreArtifactKind {
    Preserved,
    Staged,
}

impl RestoreArtifact {
    fn from_service(value: sb_service::RestoreArtifact) -> Result<Self, CoreError> {
        Ok(Self {
            path: path_text(value.path)?,
            kind: match value.kind {
                sb_service::RestoreArtifactKind::Preserved => RestoreArtifactKind::Preserved,
                sb_service::RestoreArtifactKind::Staged => RestoreArtifactKind::Staged,
            },
        })
    }
}

fn path_text(path: PathBuf) -> Result<String, CoreError> {
    path.into_os_string()
        .into_string()
        .map_err(|_| CoreError::Unavailable {
            reason: "A backup or retained path is not valid UTF-8".into(),
        })
}

#[uniffi::export]
impl Core {
    /// Recorded rows, newest first, with the Service's failed-row bound.
    /// Missing generation directories are still listed. Works read-only.
    pub async fn list_backups(
        &self,
        token: Arc<CancelToken>,
    ) -> Result<Vec<BackupGeneration>, CoreError> {
        self.query(&token, async |core| core.list_backups().await)
            .await?
            .into_iter()
            .map(BackupGeneration::from_service)
            .collect()
    }

    /// Sorted retained names beside the resolved root. Never follows,
    /// opens, deletes or authorizes reusing an entry. Works read-only.
    pub async fn list_restore_artifacts(
        &self,
        token: Arc<CancelToken>,
    ) -> Result<Vec<RestoreArtifact>, CoreError> {
        self.query(&token, async |core| core.list_restore_artifacts().await)
            .await?
            .into_iter()
            .map(RestoreArtifact::from_service)
            .collect()
    }
}

#[cfg(test)]
mod tests;
