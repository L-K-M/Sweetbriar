//! `account_health()` and `subscribe_account_health()` over FFI
//! (spec/07; plan M2.7b2): per-account state, condition, warnings,
//! open breaker events, coverage and last sync time, and a
//! Rust-owned subscription that tells the shell when one changed.

use std::sync::Arc;

use crate::api::Core;
use crate::cancel::{CancelToken, until_cancelled};
use crate::error::CoreError;
use crate::search::Coverage;
use crate::sidebar::AccountState;

/// One account's condition (spec/04's "Account states and health"
/// table).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum AccountCondition {
    Paused,
    Disconnected,
    SignInAgain,
    /// Proton asked for human verification; whether the engine still
    /// holds the challenge decides "Verify…" or "Try Again".
    Verify {
        challenge_held: bool,
    },
    VersionRefused {
        message: String,
    },
    SecretStore {
        reason: String,
    },
    Offline {
        next_retry_ms: i64,
    },
    NotAnswering {
        next_retry_ms: i64,
    },
    RateLimited {
        until_ms: i64,
    },
    Healthy,
}

impl AccountCondition {
    fn from_service(condition: sb_service::AccountCondition) -> Self {
        match condition {
            sb_service::AccountCondition::Paused => Self::Paused,
            sb_service::AccountCondition::Disconnected => Self::Disconnected,
            sb_service::AccountCondition::SignInAgain => Self::SignInAgain,
            sb_service::AccountCondition::Verify { challenge_held } => {
                Self::Verify { challenge_held }
            }
            sb_service::AccountCondition::VersionRefused { message } => {
                Self::VersionRefused { message }
            }
            sb_service::AccountCondition::SecretStore { reason } => Self::SecretStore { reason },
            sb_service::AccountCondition::Offline { next_retry_ms } => {
                Self::Offline { next_retry_ms }
            }
            sb_service::AccountCondition::NotAnswering { next_retry_ms } => {
                Self::NotAnswering { next_retry_ms }
            }
            sb_service::AccountCondition::RateLimited { until_ms } => {
                Self::RateLimited { until_ms }
            }
            sb_service::AccountCondition::Healthy => Self::Healthy,
        }
    }
}

/// One open breaker event as the health surface lists it.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AccountBreakerEvent {
    pub event_id: String,
    pub mailbox_id: Option<i64>,
    pub disappeared: u64,
    pub detected_at_ms: i64,
}

/// One account's health (spec/07 `account_health`).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AccountHealth {
    pub account_id: String,
    pub state: AccountState,
    pub state_reason: Option<String>,
    pub condition: AccountCondition,
    pub warnings: Vec<String>,
    pub breaker_events: Vec<AccountBreakerEvent>,
    pub coverage: Coverage,
    pub last_sync_ms: Option<i64>,
}

impl AccountHealth {
    fn from_service(health: sb_service::AccountHealth) -> Self {
        Self {
            account_id: health.account_id,
            state: match health.state {
                sb_service::AccountState::Active => AccountState::Active,
                sb_service::AccountState::Paused => AccountState::Paused,
                sb_service::AccountState::NeedsAttention => AccountState::NeedsAttention,
                sb_service::AccountState::Disconnected => AccountState::Disconnected,
            },
            state_reason: health.state_reason,
            condition: AccountCondition::from_service(health.condition),
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
            coverage: Coverage::from_service(&health.coverage),
            last_sync_ms: health.last_sync_ms,
        }
    }
}

/// The account-health stream (spec/07): one in-flight `next` at a
/// time, in-memory only.
#[derive(uniffi::Object)]
pub struct AccountHealthSubscription {
    inner: sb_service::AccountHealthSubscription,
}

#[uniffi::export]
impl AccountHealthSubscription {
    /// Wait until `account_health()` changed since the last answer;
    /// the shell then reads it again. One call at a time: a second is
    /// `InvalidInput`. A cancelled call answers `Cancelled` and moves
    /// nothing; shutdown ends a wait with `Unavailable`.
    pub async fn next(&self, token: Arc<CancelToken>) -> Result<(), CoreError> {
        until_cancelled(&token, self.inner.next()).await??;
        Ok(())
    }
}

#[uniffi::export]
impl Core {
    /// `account_health()` (spec/07 "Queries"): every account, in
    /// `accounts()` order, with its condition and the rest. A query:
    /// the token races the core's lock.
    pub async fn account_health(
        &self,
        token: Arc<CancelToken>,
    ) -> Result<Vec<AccountHealth>, CoreError> {
        let health = self
            .query(&token, async |core| core.account_health().await)
            .await?;
        Ok(health
            .into_iter()
            .map(AccountHealth::from_service)
            .collect())
    }

    /// `subscribe_account_health` (spec/07 "Subscriptions"): changes
    /// of `account_health()` from now on. Subscribe first, then read
    /// it, and none is missed.
    pub async fn subscribe_account_health(
        &self,
        token: Arc<CancelToken>,
    ) -> Result<Arc<AccountHealthSubscription>, CoreError> {
        let inner = self
            .query(&token, async |core| Ok(core.subscribe_account_health()))
            .await?;
        Ok(Arc::new(AccountHealthSubscription { inner }))
    }
}
