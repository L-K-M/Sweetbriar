//! `recovery(account_id?)` over FFI (spec/07 "Other queries", spec/04
//! "Epoch change"): records and enums mirroring `sb-service`'s DTOs
//! field by field, and the exported query. Cursors cross as opaque
//! strings the shell carries unchanged.

use std::sync::Arc;

use crate::api::Core;
use crate::cancel::CancelToken;
use crate::error::CoreError;

/// The occurrence states the Recovery list shows (spec/07).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum RecoveryState {
    Vanished,
    EpochStale,
    Superseded,
}

/// One Recovery row: the logical message and its last occurrence's
/// mailbox, state and time.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Recovery {
    pub message_id: i64,
    pub account_id: String,
    pub mailbox_id: i64,
    pub mailbox_name: String,
    pub state: RecoveryState,
    /// The last occurrence's `last_seen_at`, epoch milliseconds.
    pub last_seen_at_ms: i64,
    /// The last occurrence's id, the order's tie-break.
    pub occurrence_id: i64,
}

/// Where a Recovery page starts. Cursors are the core's opaque strings.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum RecoveryAnchor {
    Top,
    /// Rows strictly older than the cursor's row.
    After {
        cursor: String,
    },
}

/// One page of the Recovery list, newest first.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct RecoveryPage {
    pub rows: Vec<Recovery>,
    /// The last row's cursor; `None` when the page is empty.
    pub after: Option<String>,
    /// Whether more rows follow the page's last one.
    pub has_more: bool,
}

impl RecoveryAnchor {
    fn into_service(self) -> sb_service::RecoveryAnchor {
        match self {
            Self::Top => sb_service::RecoveryAnchor::Top,
            Self::After { cursor } => sb_service::RecoveryAnchor::After(cursor),
        }
    }
}

impl Recovery {
    fn from_service(row: sb_service::Recovery) -> Self {
        Self {
            message_id: row.message_id,
            account_id: row.account_id,
            mailbox_id: row.mailbox_id,
            mailbox_name: row.mailbox_name,
            state: match row.state {
                sb_service::RecoveryState::Vanished => RecoveryState::Vanished,
                sb_service::RecoveryState::EpochStale => RecoveryState::EpochStale,
                sb_service::RecoveryState::Superseded => RecoveryState::Superseded,
            },
            last_seen_at_ms: row.last_seen_at_ms,
            occurrence_id: row.occurrence_id,
        }
    }
}

impl RecoveryPage {
    fn from_service(page: sb_service::RecoveryPage) -> Self {
        Self {
            rows: page.rows.into_iter().map(Recovery::from_service).collect(),
            after: page.after,
            has_more: page.has_more,
        }
    }
}

#[uniffi::export]
impl Core {
    /// `recovery(account_id?)` (spec/07): logical messages with no
    /// `present` occurrence, newest first, at most `limit` rows. A
    /// query: the token races the core's lock and the read itself.
    pub async fn recovery(
        &self,
        token: Arc<CancelToken>,
        account_id: Option<String>,
        anchor: RecoveryAnchor,
        limit: u32,
    ) -> Result<RecoveryPage, CoreError> {
        let page = self
            .query(&token, async |core| {
                core.recovery(account_id, anchor.into_service(), limit)
                    .await
            })
            .await?;
        Ok(RecoveryPage::from_service(page))
    }
}

#[cfg(test)]
mod tests;
