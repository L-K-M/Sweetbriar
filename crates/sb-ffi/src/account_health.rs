//! `account_health()` and `subscribe_account_health()` over FFI
//! (spec/07; plan M2.7b2): per-account state, condition, warnings,
//! open breaker events, coverage, a Proton account's sync coverage and
//! last sync time, and a Rust-owned subscription that tells the shell
//! when one changed.

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
    Verify,
    VersionRefused { message: String },
    SecretStore { reason: String },
    Offline { next_retry_ms: i64 },
    NotAnswering { next_retry_ms: i64 },
    RateLimited { until_ms: i64 },
    Healthy,
}

impl AccountCondition {
    fn from_service(condition: sb_service::AccountCondition) -> Self {
        match condition {
            sb_service::AccountCondition::Paused => Self::Paused,
            sb_service::AccountCondition::Disconnected => Self::Disconnected,
            sb_service::AccountCondition::SignInAgain => Self::SignInAgain,
            sb_service::AccountCondition::Verify => Self::Verify,
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

/// A Proton account's listing (spec/02 `proton_accounts.listing_state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ProtonListing {
    None,
    Listing,
    Complete,
}

/// One live mailbox's count against Proton's (spec/04-sync-proton
/// "Coverage").
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ProtonMailboxCoverage {
    pub mailbox_id: i64,
    pub name: String,
    /// Present occurrences in the mailbox.
    pub local: u64,
    /// Proton's total from the last finished listing; none before one.
    pub remote_total: Option<u64>,
}

/// A Proton account's sync coverage (spec/04-sync-proton "Coverage"):
/// what "Proton messages synced" shows.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ProtonCoverage {
    pub listing: ProtonListing,
    /// The running listing is the resync a Proton `Refresh` asked for.
    pub resync: bool,
    /// Live mailboxes, by name.
    pub mailboxes: Vec<ProtonMailboxCoverage>,
    pub messages: u64,
    pub with_body: u64,
    pub body_failures: u64,
}

impl ProtonCoverage {
    fn from_service(coverage: sb_service::ProtonCoverage) -> Self {
        Self {
            listing: match coverage.listing {
                sb_service::ProtonListingState::None => ProtonListing::None,
                sb_service::ProtonListingState::Listing => ProtonListing::Listing,
                sb_service::ProtonListingState::Complete => ProtonListing::Complete,
            },
            resync: coverage.resync,
            mailboxes: coverage
                .mailboxes
                .into_iter()
                .map(|mailbox| ProtonMailboxCoverage {
                    mailbox_id: mailbox.mailbox_id,
                    name: mailbox.name,
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
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AccountHealth {
    pub account_id: String,
    pub state: AccountState,
    pub state_reason: Option<String>,
    pub condition: AccountCondition,
    pub warnings: Vec<String>,
    pub breaker_events: Vec<AccountBreakerEvent>,
    pub coverage: Coverage,
    /// A Proton account's sync coverage; none for another kind.
    pub proton: Option<ProtonCoverage>,
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
            proton: health.proton.map(ProtonCoverage::from_service),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// Every field and listing state crosses unchanged.
    #[test]
    fn proton_coverage_maps_every_field() {
        let listings = [
            (sb_service::ProtonListingState::None, ProtonListing::None),
            (
                sb_service::ProtonListingState::Listing,
                ProtonListing::Listing,
            ),
            (
                sb_service::ProtonListingState::Complete,
                ProtonListing::Complete,
            ),
        ];
        for (listing, expected) in listings {
            let mapped = ProtonCoverage::from_service(sb_service::ProtonCoverage {
                listing,
                resync: listing == sb_service::ProtonListingState::Listing,
                mailboxes: vec![
                    sb_service::ProtonMailboxCoverage {
                        mailbox_id: 7,
                        name: "Inbox".to_owned(),
                        local: 3,
                        remote_total: Some(4),
                    },
                    sb_service::ProtonMailboxCoverage {
                        mailbox_id: 9,
                        name: "Travel".to_owned(),
                        local: 0,
                        remote_total: None,
                    },
                ],
                messages: 11,
                with_body: 10,
                body_failures: 1,
            });
            assert_eq!(
                mapped,
                ProtonCoverage {
                    listing: expected,
                    resync: expected == ProtonListing::Listing,
                    mailboxes: vec![
                        ProtonMailboxCoverage {
                            mailbox_id: 7,
                            name: "Inbox".to_owned(),
                            local: 3,
                            remote_total: Some(4),
                        },
                        ProtonMailboxCoverage {
                            mailbox_id: 9,
                            name: "Travel".to_owned(),
                            local: 0,
                            remote_total: None,
                        },
                    ],
                    messages: 11,
                    with_body: 10,
                    body_failures: 1,
                }
            );
        }
    }
}
