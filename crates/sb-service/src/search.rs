//! Service-owned Search DTOs and checked query conversions (spec/06, spec/07).

use crate::{Core, CoreError};

/// An outer constraint, intersected with the query.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchScope {
    All,
    Account(String),
    Mailbox(i64),
}

impl SearchScope {
    pub(crate) fn into_engine(self) -> sb_engine::SearchScope {
        match self {
            Self::All => sb_engine::SearchScope::All,
            Self::Account(id) => sb_engine::SearchScope::Account(id),
            Self::Mailbox(id) => sb_engine::SearchScope::Mailbox(id),
        }
    }
}

/// Newest first, or native weighted relevance first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchSort {
    Date,
    Relevance,
}

/// Owned query packet; opening a window validates it on the engine's runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchSpec {
    pub query_json: String,
    pub scope: SearchScope,
    pub sort: SearchSort,
}

impl SearchSpec {
    pub(crate) fn into_engine(self) -> sb_engine::SearchSpec {
        let sort = match self.sort {
            SearchSort::Date => sb_engine::SearchSort::Date,
            SearchSort::Relevance => sb_engine::SearchSort::Relevance,
        };
        sb_engine::SearchSpec::new(self.query_json, self.scope.into_engine(), sort)
    }
}

/// Defaults only for dates that omit context; never read from the host.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParseContext {
    pub tz: String,
    /// UTC milliseconds since the Unix epoch.
    pub clock: i64,
}

/// Scoped logical-message facts and global operational state from one snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    pub(crate) fn from_engine(value: &sb_engine::Coverage) -> Self {
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
            index_state: SearchIndexState::from_engine(&value.index_state),
            rebuild: value
                .rebuild
                .as_ref()
                .map(|progress| SearchRebuildProgress {
                    job_id: progress.job_id.clone(),
                    phase: match progress.phase {
                        sb_engine::SearchRebuildPhase::Reset => SearchRebuildPhase::Reset,
                        sb_engine::SearchRebuildPhase::Enqueue => SearchRebuildPhase::Enqueue,
                        sb_engine::SearchRebuildPhase::Index => SearchRebuildPhase::Index,
                        sb_engine::SearchRebuildPhase::Complete => SearchRebuildPhase::Complete,
                    },
                    processed: progress.processed,
                    pending_documents: progress.pending_documents,
                }),
            limitations: value
                .limitations
                .iter()
                .map(|limitation| match limitation {
                    sb_engine::SearchLimitation::CjkUnsegmented => SearchLimitation::CjkUnsegmented,
                    sb_engine::SearchLimitation::AttachmentTextUnavailable => {
                        SearchLimitation::AttachmentTextUnavailable
                    }
                })
                .collect(),
        }
    }
}

/// Worker state is global, even for an empty coverage scope.
#[derive(Debug, Clone, PartialEq, Eq)]
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
    fn from_engine(value: &sb_engine::SearchIndexState) -> Self {
        match value {
            sb_engine::SearchIndexState::Ready => Self::Ready,
            sb_engine::SearchIndexState::Running => Self::Running,
            sb_engine::SearchIndexState::PausedByUser => Self::PausedByUser,
            sb_engine::SearchIndexState::BlockedNewerExtractor => Self::BlockedNewerExtractor,
            sb_engine::SearchIndexState::PausedIo {
                code,
                message,
                retry_at,
            } => Self::PausedIo {
                code: match code {
                    sb_engine::SearchIoCode::StorageFull => SearchIoCode::StorageFull,
                    sb_engine::SearchIoCode::PermissionDenied => SearchIoCode::PermissionDenied,
                    sb_engine::SearchIoCode::StorageUnavailable => SearchIoCode::StorageUnavailable,
                    sb_engine::SearchIoCode::OtherIo => SearchIoCode::OtherIo,
                },
                message: message.clone(),
                retry_at: *retry_at,
            },
        }
    }
}

/// Stable operational I/O categories; messages remain actionable detail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchIoCode {
    StorageFull,
    PermissionDenied,
    StorageUnavailable,
    OtherIo,
}

/// Active or most-recently-completed rebuild, independent of the scope.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchRebuildProgress {
    pub job_id: String,
    pub phase: SearchRebuildPhase,
    pub processed: u64,
    pub pending_documents: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchRebuildPhase {
    Reset,
    Enqueue,
    Index,
    Complete,
}

/// V1 limitations remain visible after all work completes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchLimitation {
    CjkUnsegmented,
    AttachmentTextUnavailable,
}

/// Query-independent coverage plus exact full-scope final unknowns.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchStatus {
    pub coverage: Coverage,
    pub excluded_unknown: u64,
}

impl SearchStatus {
    pub(crate) fn from_engine(value: &sb_engine::SearchStatus) -> Self {
        Self {
            coverage: Coverage::from_engine(value.coverage()),
            excluded_unknown: value.excluded_unknown(),
        }
    }
}

impl Core {
    /// Poll scoped coverage even while Search is unavailable during reset/enqueue.
    pub async fn coverage(&self, scope: SearchScope) -> Result<Coverage, CoreError> {
        Ok(Coverage::from_engine(
            &self.engine().coverage(scope.into_engine()).await?,
        ))
    }

    /// Parse text to canonical v1 JSON, preserving typed UTF8 error spans.
    pub async fn parse_search(
        &self,
        text: String,
        context: ParseContext,
    ) -> Result<String, CoreError> {
        Ok(self
            .engine()
            .parse_search(
                text,
                context.tz,
                sb_engine::Timestamp::from_millis(context.clock),
            )
            .await?)
    }

    /// Validate JSON then return canonical text with explicit date context.
    pub async fn format_search(&self, query_json: String) -> Result<String, CoreError> {
        Ok(self.engine().format_search(query_json).await?)
    }
}

#[cfg(test)]
mod tests;
