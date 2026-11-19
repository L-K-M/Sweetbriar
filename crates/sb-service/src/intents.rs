//! `intents(filter)` (spec/07 "Other queries"): service-owned DTOs and
//! `Core`'s query, mirroring the engine's types field by field
//! (spec/01 rule 3). Cursors cross as opaque strings.

use crate::{Core, CoreError};

/// The states the health surface lists (spec/02).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentState {
    Parked,
    Quarantined,
}

/// What the intent does (spec/02 `intents.op`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IntentKind {
    SetFlags,
    Move,
    Copy,
    DeleteToTrash,
    /// Reserved, not produced in the pilot (spec/02).
    ExpungeScoped,
    /// Reserved, not produced in the pilot (spec/02).
    Append,
    /// Reserved, not produced in the pilot; sends use send attempts.
    Submit,
}

/// Why a quarantined intent waits for the user (spec/02).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum QuarantineReason {
    EpochChange,
    Restore,
    AmbiguousMatch,
    ConfigChange,
    Breaker,
}

/// The intent's effect as stored: flag deltas, a destination mailbox,
/// a trash move, or a reserved operation's raw args.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntentEffect {
    SetFlags {
        add: Vec<String>,
        remove: Vec<String>,
    },
    ToMailbox {
        mailbox_id: i64,
    },
    DeleteToTrash,
    /// A reserved operation, not produced in the pilot (spec/02).
    Reserved {
        args_json: String,
    },
}

/// One parked or quarantined intent as the health surface shows it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Intent {
    pub intent_id: String,
    pub account_id: String,
    pub kind: IntentKind,
    pub state: IntentState,
    /// The target occurrence ids `target_json` holds.
    pub target_occurrence_ids: Vec<i64>,
    /// Set on a quarantined intent.
    pub quarantine_reason: Option<QuarantineReason>,
    /// A parked intent's terminal error, when one was recorded.
    pub last_error: Option<String>,
    pub effect: IntentEffect,
    pub created_at_ms: i64,
}

/// Which parked or quarantined intents a page reads; `None` leaves a
/// dimension unfiltered.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct IntentFilter {
    pub account_id: Option<String>,
    pub state: Option<IntentState>,
}

/// Where an intent page starts. Cursors are the engine's opaque
/// strings; the shell carries them unchanged.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum IntentAnchor {
    Top,
    /// Rows after the cursor's row in the oldest-first order.
    After(String),
}

/// One page of parked or quarantined intents, oldest first.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IntentPage {
    pub rows: Vec<Intent>,
    /// The last row's cursor; `None` when the page is empty.
    pub after: Option<String>,
    /// Whether more rows follow the page's last one.
    pub has_more: bool,
}

impl IntentFilter {
    fn into_engine(self) -> sb_engine::IntentFilter {
        sb_engine::IntentFilter {
            account_id: self.account_id,
            state: self.state.map(|state| match state {
                IntentState::Parked => sb_engine::IntentState::Parked,
                IntentState::Quarantined => sb_engine::IntentState::Quarantined,
            }),
        }
    }
}

impl IntentAnchor {
    fn into_engine(self) -> sb_engine::IntentAnchor {
        match self {
            Self::Top => sb_engine::IntentAnchor::Top,
            Self::After(cursor) => sb_engine::IntentAnchor::After(cursor.into()),
        }
    }
}

impl Intent {
    fn from_engine(intent: sb_engine::StoredIntent) -> Self {
        Self {
            intent_id: intent.intent_id,
            account_id: intent.account_id,
            kind: match intent.kind {
                sb_engine::IntentKind::SetFlags => IntentKind::SetFlags,
                sb_engine::IntentKind::Move => IntentKind::Move,
                sb_engine::IntentKind::Copy => IntentKind::Copy,
                sb_engine::IntentKind::DeleteToTrash => IntentKind::DeleteToTrash,
                sb_engine::IntentKind::ExpungeScoped => IntentKind::ExpungeScoped,
                sb_engine::IntentKind::Append => IntentKind::Append,
                sb_engine::IntentKind::Submit => IntentKind::Submit,
            },
            state: match intent.state {
                sb_engine::IntentState::Parked => IntentState::Parked,
                sb_engine::IntentState::Quarantined => IntentState::Quarantined,
            },
            target_occurrence_ids: intent.target_occurrence_ids,
            quarantine_reason: intent.quarantine_reason.map(|reason| match reason {
                sb_engine::QuarantineReason::EpochChange => QuarantineReason::EpochChange,
                sb_engine::QuarantineReason::Restore => QuarantineReason::Restore,
                sb_engine::QuarantineReason::AmbiguousMatch => QuarantineReason::AmbiguousMatch,
                sb_engine::QuarantineReason::ConfigChange => QuarantineReason::ConfigChange,
                sb_engine::QuarantineReason::Breaker => QuarantineReason::Breaker,
            }),
            last_error: intent.last_error,
            effect: match intent.effect {
                sb_engine::IntentEffect::SetFlags { add, remove } => {
                    IntentEffect::SetFlags { add, remove }
                }
                sb_engine::IntentEffect::ToMailbox { mailbox_id } => {
                    IntentEffect::ToMailbox { mailbox_id }
                }
                sb_engine::IntentEffect::DeleteToTrash => IntentEffect::DeleteToTrash,
                sb_engine::IntentEffect::Reserved { args_json } => {
                    IntentEffect::Reserved { args_json }
                }
            },
            created_at_ms: intent.created_at,
        }
    }
}

impl IntentPage {
    fn from_engine(page: &sb_engine::IntentPage) -> Self {
        Self {
            rows: page.rows.iter().cloned().map(Intent::from_engine).collect(),
            after: page.after.as_ref().map(|cursor| cursor.as_str().to_owned()),
            has_more: page.has_more,
        }
    }
}

impl Core {
    /// `intents(filter)` (spec/07): parked and quarantined intents,
    /// oldest first, at most `limit` rows.
    pub async fn intents(
        &self,
        filter: IntentFilter,
        anchor: IntentAnchor,
        limit: u32,
    ) -> Result<IntentPage, CoreError> {
        let page = self
            .engine()
            .intents(filter.into_engine(), anchor.into_engine(), limit)
            .await?;
        Ok(IntentPage::from_engine(&page))
    }
}

#[cfg(test)]
mod tests;
