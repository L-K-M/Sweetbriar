//! `account_health()` and `subscribe_account_health()` (spec/07;
//! plan M2.7b2): per account, its state, condition (spec/04's table),
//! warnings, open breaker events, coverage, a Proton account's sync
//! coverage and last sync time. Retry and sync times live in worker
//! memory, cleared with the worker; the stored rows have no observer,
//! so their writers bump the generation.

use std::collections::HashMap;
use std::sync::Arc;
use std::sync::Mutex;

use sb_resources::proton_sync::{ProtonCoverage, open_breaker_events, proton_coverage};
use sb_resources::{
    APP_IDENTITY_REFUSED_REASON, AccountKind, AccountState, HUMAN_VERIFICATION_REASON,
    SESSION_REFUSED_REASON, StoredAccount, accounts, diagnostics_message, proton_account,
};

use crate::windows::{AccountHealthSubscription, Windows};
use crate::{Coverage, Engine, EngineError, SearchScope};

/// How the running worker last left an active account; in memory only.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum LiveCondition {
    /// Nothing has failed since the last successful sync.
    Synced,
    /// A network error or timeout: the next retry's time.
    Offline { next_retry_ms: i64 },
    /// Proton answered 5xx or a response didn't decode.
    NotAnswering { next_retry_ms: i64 },
    /// Proton asked Sweetbriar to slow down, until this time.
    RateLimited { until_ms: i64 },
}

/// One account's worker status: the last successful sync, if any, and
/// its current condition.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) struct LiveStatus {
    pub last_sync_ms: Option<i64>,
    pub condition: LiveCondition,
}

/// The per-account worker statuses, and the generation every change
/// bumps. Shared with the workers through `crate::engine::Worker`.
pub(crate) struct WorkerStatuses {
    statuses: Mutex<HashMap<String, LiveStatus>>,
    windows: Arc<Windows>,
}

impl WorkerStatuses {
    pub(crate) fn new(windows: Arc<Windows>) -> Self {
        Self {
            statuses: Mutex::new(HashMap::new()),
            windows,
        }
    }

    /// Record the status, waking subscriptions when it changed.
    pub(crate) fn set(&self, account_id: &str, status: LiveStatus) {
        let changed = {
            let mut statuses = self
                .statuses
                .lock()
                .expect("the worker status lock poisoned");
            if statuses.get(account_id) == Some(&status) {
                false
            } else {
                statuses.insert(account_id.to_owned(), status);
                true
            }
        };
        if changed {
            self.windows.account_health_moved();
        }
    }

    /// A command or worker write changed a stored row the query reads.
    pub(crate) fn rows_changed(&self) {
        self.windows.account_health_moved();
    }

    /// The worker stopped: the condition comes from the stored state.
    pub(crate) fn clear(&self, account_id: &str) {
        let cleared = {
            let mut statuses = self
                .statuses
                .lock()
                .expect("the worker status lock poisoned");
            statuses.remove(account_id).is_some()
        };
        if cleared {
            self.windows.account_health_moved();
        }
    }

    pub(crate) fn get(&self, account_id: &str) -> Option<LiveStatus> {
        self.statuses
            .lock()
            .expect("the worker status lock poisoned")
            .get(account_id)
            .copied()
    }
}

/// One account's condition (spec/04's table, spec/07).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AccountCondition {
    Paused,
    Disconnected,
    SignInAgain,
    Verify,
    /// Proton refused this version of Sweetbriar (2064, 5003), with
    /// Proton's message from `diagnostics_json` (empty if none was
    /// recorded).
    VersionRefused {
        message: String,
    },
    /// A secret-store pause (spec/10), with the stored reason.
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

/// One open breaker event as the health surface lists it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountBreakerEvent {
    pub event_id: String,
    pub mailbox_id: Option<i64>,
    pub disappeared: u64,
    pub detected_at_ms: i64,
}

/// One account's health (spec/07 `account_health`), in `accounts()` order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AccountHealth {
    pub account_id: String,
    pub state: AccountState,
    pub state_reason: Option<String>,
    pub condition: AccountCondition,
    /// Addresses whose keys didn't unlock; empty when none or no session.
    pub warnings: Vec<String>,
    pub breaker_events: Vec<AccountBreakerEvent>,
    pub coverage: Coverage,
    /// A Proton account's sync coverage (spec/04-sync-proton
    /// "Coverage"); none for another kind, or before the account's
    /// Proton row exists.
    pub proton: Option<ProtonCoverage>,
    /// The last successful sync from memory; none if never synced.
    pub last_sync_ms: Option<i64>,
}

impl Engine {
    /// `account_health()` (spec/07): every account, by sort order then id.
    pub async fn account_health(&self) -> Result<Vec<AccountHealth>, EngineError> {
        let (accounts, diagnostics, events, mut proton) = self
            .read_through(|resources| {
                resources.read(|tx| {
                    let accounts = accounts(tx)?;
                    let mut diagnostics = HashMap::new();
                    let mut events = HashMap::new();
                    let mut proton = HashMap::new();
                    for account in &accounts {
                        if account.kind != AccountKind::Proton {
                            continue;
                        }
                        diagnostics.insert(
                            account.account_id.clone(),
                            diagnostics_message(tx, &account.account_id)?,
                        );
                        events.insert(
                            account.account_id.clone(),
                            open_breaker_events(tx, &account.account_id)?,
                        );
                        // Like the refusal message, a Proton row that
                        // isn't there yet leaves no coverage.
                        if proton_account(tx, &account.account_id)?.is_some() {
                            proton.insert(
                                account.account_id.clone(),
                                proton_coverage(tx, &account.account_id)?,
                            );
                        }
                    }
                    Ok((accounts, diagnostics, events, proton))
                })
            })
            .await?;
        let warnings = {
            let sessions = self.proton_sessions.lock().await;
            sessions
                .iter()
                .map(|(id, restored)| (id.clone(), locked_addresses(restored.keys.as_ref())))
                .collect::<HashMap<_, _>>()
        };
        let mut health = Vec::with_capacity(accounts.len());
        for account in accounts {
            let id = account.account_id.clone();
            let live = self.worker_statuses().get(&id);
            let condition = condition(
                &account,
                diagnostics.get(&id).and_then(|message| message.as_deref()),
                live,
            );
            let coverage = self.coverage(SearchScope::Account(id.clone())).await?;
            health.push(AccountHealth {
                state_reason: account.state_reason.clone(),
                warnings: warnings.get(&id).cloned().unwrap_or_default(),
                breaker_events: events
                    .get(&id)
                    .map(|events| {
                        events
                            .iter()
                            .map(|event| AccountBreakerEvent {
                                event_id: event.event_id.clone(),
                                mailbox_id: event.mailbox_id,
                                disappeared: event.disappeared,
                                detected_at_ms: event.detected_at_ms,
                            })
                            .collect()
                    })
                    .unwrap_or_default(),
                last_sync_ms: live.and_then(|status| status.last_sync_ms),
                proton: proton.remove(&id),
                account_id: id,
                state: account.state,
                condition,
                coverage,
            });
        }
        Ok(health)
    }

    /// The account-health stream (spec/07): subscribe first, then read.
    pub fn subscribe_account_health(&self) -> AccountHealthSubscription {
        self.windows().subscribe_account_health()
    }
}

/// The addresses whose keys didn't unlock (spec/04 "Keys").
fn locked_addresses(keys: &dyn sb_resources::KeyStore) -> Vec<String> {
    keys.identities()
        .iter()
        .filter(|identity| !identity.unlocked_key)
        .map(|identity| identity.email.clone())
        .collect()
}

/// The condition for one account (spec/04's table): the three Proton
/// reasons match frozen constants; any other stored reason is a pause.
fn condition(
    account: &StoredAccount,
    refusal_message: Option<&str>,
    live: Option<LiveStatus>,
) -> AccountCondition {
    match account.state {
        AccountState::Paused => AccountCondition::Paused,
        AccountState::Disconnected => AccountCondition::Disconnected,
        AccountState::NeedsAttention => match account.state_reason.as_deref() {
            Some(SESSION_REFUSED_REASON) => AccountCondition::SignInAgain,
            Some(HUMAN_VERIFICATION_REASON) => AccountCondition::Verify,
            Some(APP_IDENTITY_REFUSED_REASON) => AccountCondition::VersionRefused {
                message: refusal_message.unwrap_or_default().to_owned(),
            },
            Some(reason) => AccountCondition::SecretStore {
                reason: reason.to_owned(),
            },
            None => AccountCondition::SecretStore {
                reason: String::new(),
            },
        },
        AccountState::Active => match live.map(|status| status.condition) {
            Some(LiveCondition::Offline { next_retry_ms }) => {
                AccountCondition::Offline { next_retry_ms }
            }
            Some(LiveCondition::NotAnswering { next_retry_ms }) => {
                AccountCondition::NotAnswering { next_retry_ms }
            }
            Some(LiveCondition::RateLimited { until_ms }) => {
                AccountCondition::RateLimited { until_ms }
            }
            Some(LiveCondition::Synced) | None => AccountCondition::Healthy,
        },
    }
}
