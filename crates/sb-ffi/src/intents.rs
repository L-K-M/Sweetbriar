//! `intents(filter)` and the intent health actions over FFI (spec/07
//! "Other queries", "Intents"): records and enums mirroring
//! `sb-service`'s DTOs field by field, the exported query and the two
//! exported commands. Cursors cross as opaque strings the shell
//! carries unchanged.

use std::sync::Arc;

use crate::api::{Core, Receipt};
use crate::cancel::CancelToken;
use crate::error::CoreError;

/// The states the health surface lists (spec/02).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum IntentState {
    Parked,
    Quarantined,
}

/// What the intent does (spec/02 `intents.op`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
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
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum QuarantineReason {
    EpochChange,
    Restore,
    AmbiguousMatch,
    ConfigChange,
    Breaker,
}

/// The intent's effect as stored.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
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
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
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
#[derive(Debug, Clone, Default, PartialEq, Eq, uniffi::Record)]
pub struct IntentFilter {
    pub account_id: Option<String>,
    pub state: Option<IntentState>,
}

/// Where an intent page starts. Cursors are the core's opaque strings.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum IntentAnchor {
    Top,
    /// Rows after the cursor's row in the oldest-first order.
    After {
        cursor: String,
    },
}

/// One page of parked or quarantined intents, oldest first.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct IntentPage {
    pub rows: Vec<Intent>,
    /// The last row's cursor; `None` when the page is empty.
    pub after: Option<String>,
    /// Whether more rows follow the page's last one.
    pub has_more: bool,
}

impl IntentFilter {
    fn into_service(self) -> sb_service::IntentFilter {
        sb_service::IntentFilter {
            account_id: self.account_id,
            state: self.state.map(|state| match state {
                IntentState::Parked => sb_service::IntentState::Parked,
                IntentState::Quarantined => sb_service::IntentState::Quarantined,
            }),
        }
    }
}

impl IntentAnchor {
    fn into_service(self) -> sb_service::IntentAnchor {
        match self {
            Self::Top => sb_service::IntentAnchor::Top,
            Self::After { cursor } => sb_service::IntentAnchor::After(cursor),
        }
    }
}

impl IntentEffect {
    fn from_service(effect: sb_service::IntentEffect) -> Self {
        match effect {
            sb_service::IntentEffect::SetFlags { add, remove } => Self::SetFlags { add, remove },
            sb_service::IntentEffect::ToMailbox { mailbox_id } => Self::ToMailbox { mailbox_id },
            sb_service::IntentEffect::DeleteToTrash => Self::DeleteToTrash,
            sb_service::IntentEffect::Reserved { args_json } => Self::Reserved { args_json },
        }
    }
}

impl Intent {
    fn from_service(intent: sb_service::Intent) -> Self {
        Self {
            intent_id: intent.intent_id,
            account_id: intent.account_id,
            kind: match intent.kind {
                sb_service::IntentKind::SetFlags => IntentKind::SetFlags,
                sb_service::IntentKind::Move => IntentKind::Move,
                sb_service::IntentKind::Copy => IntentKind::Copy,
                sb_service::IntentKind::DeleteToTrash => IntentKind::DeleteToTrash,
                sb_service::IntentKind::ExpungeScoped => IntentKind::ExpungeScoped,
                sb_service::IntentKind::Append => IntentKind::Append,
                sb_service::IntentKind::Submit => IntentKind::Submit,
            },
            state: match intent.state {
                sb_service::IntentState::Parked => IntentState::Parked,
                sb_service::IntentState::Quarantined => IntentState::Quarantined,
            },
            target_occurrence_ids: intent.target_occurrence_ids,
            quarantine_reason: intent.quarantine_reason.map(|reason| match reason {
                sb_service::QuarantineReason::EpochChange => QuarantineReason::EpochChange,
                sb_service::QuarantineReason::Restore => QuarantineReason::Restore,
                sb_service::QuarantineReason::AmbiguousMatch => QuarantineReason::AmbiguousMatch,
                sb_service::QuarantineReason::ConfigChange => QuarantineReason::ConfigChange,
                sb_service::QuarantineReason::Breaker => QuarantineReason::Breaker,
            }),
            last_error: intent.last_error,
            effect: IntentEffect::from_service(intent.effect),
            created_at_ms: intent.created_at_ms,
        }
    }
}

impl IntentPage {
    fn from_service(page: sb_service::IntentPage) -> Self {
        Self {
            rows: page.rows.into_iter().map(Intent::from_service).collect(),
            after: page.after,
            has_more: page.has_more,
        }
    }
}

#[uniffi::export]
impl Core {
    /// `intents(filter)` (spec/07): parked and quarantined intents,
    /// oldest first, at most `limit` rows. A query: the token races the
    /// core's lock and the read itself.
    pub async fn intents(
        &self,
        token: Arc<CancelToken>,
        filter: IntentFilter,
        anchor: IntentAnchor,
        limit: u32,
    ) -> Result<IntentPage, CoreError> {
        let page = self
            .query(&token, async |core| {
                core.intents(filter.into_service(), anchor.into_service(), limit)
                    .await
            })
            .await?;
        Ok(IntentPage::from_service(page))
    }

    /// `release_intents(key, ids)` (spec/07 "Intents",
    /// spec/04-sync-proton "Health actions") under a key the shell
    /// made: every named parked or quarantined intent becomes pending
    /// again, ready for the account worker's next poll. All or nothing.
    /// A command: a repeated key answers its stored receipt and
    /// releases nothing again.
    pub async fn release_intents(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        intent_ids: Vec<String>,
    ) -> Result<Receipt, CoreError> {
        let receipt = self
            .command(&token, async |core| {
                core.release_intents(&idempotency_key, intent_ids).await
            })
            .await?;
        Ok(Receipt::from_service(&receipt))
    }

    /// `cancel_intents(key, ids)` (spec/07 "Intents",
    /// spec/04-sync-proton "Health actions") under a key the shell
    /// made: every named pending, parked or quarantined intent becomes
    /// cancelled, and a flag intent's local effect is reverted unless a
    /// later live intent changes the same flag. All or nothing. A
    /// command: a repeated key answers its stored receipt and cancels
    /// nothing again.
    pub async fn cancel_intents(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        intent_ids: Vec<String>,
    ) -> Result<Receipt, CoreError> {
        let receipt = self
            .command(&token, async |core| {
                core.cancel_intents(&idempotency_key, intent_ids).await
            })
            .await?;
        Ok(Receipt::from_service(&receipt))
    }
}

#[cfg(test)]
mod tests;
