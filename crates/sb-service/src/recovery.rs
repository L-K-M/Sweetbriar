//! `recovery(account_id?)` (spec/07 "Other queries", spec/04 "Epoch
//! change"): service-owned DTOs and `Core`'s query, mirroring the
//! engine's types field by field (spec/01 rule 3). Cursors cross as
//! opaque strings.

use crate::{Core, CoreError};

/// The occurrence states the Recovery list shows (spec/07).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RecoveryState {
    Vanished,
    EpochStale,
    Superseded,
}

/// One Recovery row: the logical message and its last occurrence's
/// mailbox, state and time.
#[derive(Debug, Clone, PartialEq, Eq)]
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

/// Where a Recovery page starts. Cursors are the engine's opaque
/// strings; the shell carries them unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum RecoveryAnchor {
    Top,
    /// Rows strictly older than the cursor's row.
    After(String),
}

/// One page of the Recovery list, newest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveryPage {
    pub rows: Vec<Recovery>,
    /// The last row's cursor; `None` when the page is empty.
    pub after: Option<String>,
    /// Whether more rows follow the page's last one.
    pub has_more: bool,
}

impl RecoveryAnchor {
    fn into_engine(self) -> sb_engine::RecoveryAnchor {
        match self {
            Self::Top => sb_engine::RecoveryAnchor::Top,
            Self::After(cursor) => sb_engine::RecoveryAnchor::After(cursor.into()),
        }
    }
}

impl Recovery {
    fn from_engine(row: sb_engine::StoredRecovery) -> Self {
        Self {
            message_id: row.message_id,
            account_id: row.account_id,
            mailbox_id: row.mailbox_id,
            mailbox_name: row.mailbox_name,
            state: match row.state {
                sb_engine::RecoveryState::Vanished => RecoveryState::Vanished,
                sb_engine::RecoveryState::EpochStale => RecoveryState::EpochStale,
                sb_engine::RecoveryState::Superseded => RecoveryState::Superseded,
            },
            last_seen_at_ms: row.last_seen_at,
            occurrence_id: row.occurrence_id,
        }
    }
}

impl RecoveryPage {
    fn from_engine(page: &sb_engine::RecoveryPage) -> Self {
        Self {
            rows: page
                .rows
                .iter()
                .cloned()
                .map(Recovery::from_engine)
                .collect(),
            after: page.after.as_ref().map(|cursor| cursor.as_str().to_owned()),
            has_more: page.has_more,
        }
    }
}

impl Core {
    /// `recovery(account_id?)` (spec/07): logical messages with no
    /// `present` occurrence, newest first, at most `limit` rows. An
    /// unknown account is `NotFound`.
    pub async fn recovery(
        &self,
        account_id: Option<String>,
        anchor: RecoveryAnchor,
        limit: u32,
    ) -> Result<RecoveryPage, CoreError> {
        let account = account_id.as_deref();
        let page = self
            .engine()
            .recovery(account, anchor.into_engine(), limit)
            .await?;
        Ok(RecoveryPage::from_engine(&page))
    }
}

#[cfg(test)]
mod tests;
