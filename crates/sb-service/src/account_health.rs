//! `account_health()` and `subscribe_account_health()` (spec/07;
//! plan M2.7b2): service-owned DTO mirrors, converted field by field
//! (spec/01 rule 3).

use crate::search::Coverage;
use crate::{AccountState, Core, CoreError, ProtonListingState};

/// One account's condition (spec/04's table, spec/07).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountCondition {
    Paused,
    Disconnected,
    SignInAgain,
    Verify,
    VersionRefused { message: String },
    SecretStore { reason: String },
    Offline { next_retry_ms: i64 },
    NotAnswering { next_retry_ms: i64 },
    RateLimited { until_ms: i64 },
    Healthy,
}

/// One open breaker event as the health surface lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountBreakerEvent {
    pub event_id: String,
    pub mailbox_id: Option<i64>,
    pub disappeared: u64,
    pub detected_at_ms: i64,
}

/// One live mailbox's count against Proton's (spec/04-sync-proton
/// "Coverage").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtonMailboxCoverage {
    pub mailbox_id: i64,
    pub name: String,
    /// Present occurrences in the mailbox.
    pub local: u64,
    /// Proton's total from the last finished listing; none before one.
    pub remote_total: Option<u64>,
}

/// A Proton account's sync coverage (spec/04-sync-proton "Coverage").
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtonCoverage {
    pub listing: ProtonListingState,
    /// The running listing is the resync a Proton `Refresh` asked for.
    pub resync: bool,
    /// Live mailboxes, by name.
    pub mailboxes: Vec<ProtonMailboxCoverage>,
    pub messages: u64,
    pub with_body: u64,
    pub body_failures: u64,
}

impl ProtonCoverage {
    fn from_engine(coverage: sb_engine::ProtonCoverage) -> Self {
        Self {
            listing: ProtonListingState::from_engine(coverage.listing),
            resync: coverage.resync,
            mailboxes: coverage
                .mailboxes
                .into_iter()
                .map(|mailbox| ProtonMailboxCoverage {
                    mailbox_id: mailbox.mailbox_id,
                    name: mailbox.remote_name,
                    local: mailbox.local,
                    remote_total: mailbox.remote_total,
                })
                .collect(),
            messages: coverage.messages,
            with_body: coverage.with_body,
            body_failures: coverage.body_failures,
        }
    }
}

/// One account's health (spec/07 `account_health`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountHealth {
    pub account_id: String,
    pub state: AccountState,
    pub state_reason: Option<String>,
    pub condition: AccountCondition,
    pub warnings: Vec<String>,
    pub breaker_events: Vec<AccountBreakerEvent>,
    pub coverage: Coverage,
    /// A Proton account's sync coverage; none for another kind, or
    /// before the account's Proton row exists.
    pub proton: Option<ProtonCoverage>,
    pub last_sync_ms: Option<i64>,
}

impl AccountCondition {
    pub(crate) fn from_engine(condition: sb_engine::AccountCondition) -> Self {
        match condition {
            sb_engine::AccountCondition::Paused => Self::Paused,
            sb_engine::AccountCondition::Disconnected => Self::Disconnected,
            sb_engine::AccountCondition::SignInAgain => Self::SignInAgain,
            sb_engine::AccountCondition::Verify => Self::Verify,
            sb_engine::AccountCondition::VersionRefused { message } => {
                Self::VersionRefused { message }
            }
            sb_engine::AccountCondition::SecretStore { reason } => Self::SecretStore { reason },
            sb_engine::AccountCondition::Offline { next_retry_ms } => {
                Self::Offline { next_retry_ms }
            }
            sb_engine::AccountCondition::NotAnswering { next_retry_ms } => {
                Self::NotAnswering { next_retry_ms }
            }
            sb_engine::AccountCondition::RateLimited { until_ms } => Self::RateLimited { until_ms },
            sb_engine::AccountCondition::Healthy => Self::Healthy,
        }
    }
}

impl AccountHealth {
    fn from_engine(health: sb_engine::AccountHealth) -> Self {
        Self {
            account_id: health.account_id,
            state: match health.state {
                sb_engine::AccountState::Active => AccountState::Active,
                sb_engine::AccountState::Paused => AccountState::Paused,
                sb_engine::AccountState::NeedsAttention => AccountState::NeedsAttention,
                sb_engine::AccountState::Disconnected => AccountState::Disconnected,
            },
            state_reason: health.state_reason,
            condition: AccountCondition::from_engine(health.condition),
            warnings: health.warnings,
            breaker_events: health
                .breaker_events
                .into_iter()
                .map(|event| AccountBreakerEvent {
                    event_id: event.event_id,
                    mailbox_id: event.mailbox_id,
                    disappeared: event.disappeared,
                    detected_at_ms: event.detected_at_ms,
                })
                .collect(),
            coverage: Coverage::from_engine(&health.coverage),
            proton: health.proton.map(ProtonCoverage::from_engine),
            last_sync_ms: health.last_sync_ms,
        }
    }
}

impl Core {
    /// `account_health()` (spec/07): every account, in `accounts()`
    /// order, with its state, condition, warnings, open breaker events,
    /// coverage and last sync time.
    pub async fn account_health(&self) -> Result<Vec<AccountHealth>, CoreError> {
        let health = self.engine().account_health().await?;
        Ok(health.into_iter().map(AccountHealth::from_engine).collect())
    }

    /// `subscribe_account_health` (spec/07): changes of
    /// `account_health()` from now on. Subscribe first, then read
    /// `account_health()`, and none is missed.
    pub fn subscribe_account_health(&self) -> AccountHealthSubscription {
        AccountHealthSubscription(self.engine().subscribe_account_health())
    }
}

/// The account-health stream (spec/07): one in-flight `next` at a
/// time, in-memory only.
pub struct AccountHealthSubscription(sb_engine::AccountHealthSubscription);

impl AccountHealthSubscription {
    /// Wait until `account_health()` changed since the last answer;
    /// several changes are one answer. Shutdown ends a wait with
    /// `Unavailable`; a cancelled call moves nothing.
    pub async fn next(&self) -> Result<(), CoreError> {
        Ok(self.0.next().await?)
    }
}
