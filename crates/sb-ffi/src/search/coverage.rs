//! Complete typed coverage values over the existing cancellable query boundary.

use super::{Arc, CancelToken, Core, CoreError};

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum SearchScope {
    All,
    Account { account_id: String },
    Mailbox { mailbox_id: i64 },
}

impl SearchScope {
    pub(super) fn into_service(self) -> sb_service::SearchScope {
        match self {
            Self::All => sb_service::SearchScope::All,
            Self::Account { account_id } => sb_service::SearchScope::Account(account_id),
            Self::Mailbox { mailbox_id } => sb_service::SearchScope::Mailbox(mailbox_id),
        }
    }
}

/// Scoped logical-message counts and global worker state from one snapshot.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Coverage {
    pub generation: u64,
    pub extractor_version: u32,
    pub total_messages: u64,
    pub headers_synced: u64,
    pub bodies_acquired: u64,
    pub indexed: u64,
    pub partial: u64,
    pub failed: u64,
    pub pending: u64,
    pub queue_length: u64,
    pub index_state: SearchIndexState,
    pub rebuild: Option<SearchRebuildProgress>,
    pub limitations: Vec<SearchLimitation>,
}

impl Coverage {
    pub(super) fn from_service(value: &sb_service::Coverage) -> Self {
        Self {
            generation: value.generation,
            extractor_version: value.extractor_version,
            total_messages: value.total_messages,
            headers_synced: value.headers_synced,
            bodies_acquired: value.bodies_acquired,
            indexed: value.indexed,
            partial: value.partial,
            failed: value.failed,
            pending: value.pending,
            queue_length: value.queue_length,
            index_state: SearchIndexState::from_service(&value.index_state),
            rebuild: value
                .rebuild
                .as_ref()
                .map(|progress| SearchRebuildProgress {
                    job_id: progress.job_id.clone(),
                    phase: match progress.phase {
                        sb_service::SearchRebuildPhase::Reset => SearchRebuildPhase::Reset,
                        sb_service::SearchRebuildPhase::Enqueue => SearchRebuildPhase::Enqueue,
                        sb_service::SearchRebuildPhase::Index => SearchRebuildPhase::Index,
                        sb_service::SearchRebuildPhase::Complete => SearchRebuildPhase::Complete,
                    },
                    processed: progress.processed,
                    pending_documents: progress.pending_documents,
                }),
            limitations: value
                .limitations
                .iter()
                .map(|limitation| match limitation {
                    sb_service::SearchLimitation::CjkUnsegmented => {
                        SearchLimitation::CjkUnsegmented
                    }
                    sb_service::SearchLimitation::AttachmentTextUnavailable => {
                        SearchLimitation::AttachmentTextUnavailable
                    }
                })
                .collect(),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum SearchIndexState {
    Ready,
    Running,
    PausedByUser,
    PausedIo {
        code: SearchIoCode,
        message: String,
        retry_at: i64,
    },
    BlockedNewerExtractor,
}

impl SearchIndexState {
    fn from_service(value: &sb_service::SearchIndexState) -> Self {
        match value {
            sb_service::SearchIndexState::Ready => Self::Ready,
            sb_service::SearchIndexState::Running => Self::Running,
            sb_service::SearchIndexState::PausedByUser => Self::PausedByUser,
            sb_service::SearchIndexState::BlockedNewerExtractor => Self::BlockedNewerExtractor,
            sb_service::SearchIndexState::PausedIo {
                code,
                message,
                retry_at,
            } => Self::PausedIo {
                code: match code {
                    sb_service::SearchIoCode::StorageFull => SearchIoCode::StorageFull,
                    sb_service::SearchIoCode::PermissionDenied => SearchIoCode::PermissionDenied,
                    sb_service::SearchIoCode::StorageUnavailable => {
                        SearchIoCode::StorageUnavailable
                    }
                    sb_service::SearchIoCode::OtherIo => SearchIoCode::OtherIo,
                },
                message: message.clone(),
                retry_at: *retry_at,
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SearchIoCode {
    StorageFull,
    PermissionDenied,
    StorageUnavailable,
    OtherIo,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SearchRebuildProgress {
    pub job_id: String,
    pub phase: SearchRebuildPhase,
    pub processed: u64,
    pub pending_documents: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SearchRebuildPhase {
    Reset,
    Enqueue,
    Index,
    Complete,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SearchLimitation {
    CjkUnsegmented,
    AttachmentTextUnavailable,
}

#[uniffi::export]
impl Core {
    /// Query scoped coverage, including global progress while Search is unavailable.
    pub async fn coverage(
        &self,
        token: Arc<CancelToken>,
        scope: SearchScope,
    ) -> Result<Coverage, CoreError> {
        let value = self
            .query(&token, async |core| {
                core.coverage(scope.into_service()).await
            })
            .await?;
        Ok(Coverage::from_service(&value))
    }
}

#[cfg(test)]
mod tests;
