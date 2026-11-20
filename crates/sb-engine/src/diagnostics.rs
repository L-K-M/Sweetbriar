//! `diagnostics_package()` (spec/10 "Privacy", spec/07; plan M2.7c4):
//! one document's worth of store and account facts for the shell to
//! preview, with addresses masked again as it is assembled. The app and
//! OS versions come from the shell; the core's own version is this
//! crate's `CARGO_PKG_VERSION`. The core writes no file.
//!
//! ```text
//!   accounts()/account_health() ─► per-account rows
//!   schema_version + proton_accounts + intent counts ─► one read
//!   integrity latch (memory) ─► the store's state
//! ```

use std::collections::HashMap;

use sb_resources::proton_sync::ProtonListingState;
use sb_resources::{
    AccountKind, AccountState, Coverage, IntentCounts, ProtonAccount, accounts, intent_counts,
    proton_account, redact_addresses, schema_version,
};
use serde_json::Value as Json;

use crate::{AccountBreakerEvent, AccountCondition, Engine, EngineError, Integrity};

/// The diagnostics package's data (spec/10): everything the document
/// reports, addresses already masked. Not serialized here; the service
/// owns the document's shape (spec/01 rule 3).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticsPackage {
    pub app_version: String,
    pub os_version: String,
    pub core_version: String,
    pub schema_version: i64,
    pub integrity: DiagnosticsIntegrity,
    pub accounts: Vec<DiagnosticsAccount>,
}

/// The store's integrity state (spec/03 "Integrity"). The finding's
/// copy path stays out: a diagnostics document holds no file paths
/// under the home folder (spec/10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagnosticsIntegrity {
    Sound,
    ReadOnly {
        detail: String,
        detected_at_ms: Option<i64>,
    },
}

/// One account as the package reports it (spec/10). No display name:
/// the account's id, kind, state and condition only.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticsAccount {
    pub account_id: String,
    pub kind: AccountKind,
    pub state: AccountState,
    /// The stored `state_reason`, masked; absent when none is set.
    pub state_reason: Option<String>,
    pub condition: AccountCondition,
    pub breaker_events: Vec<AccountBreakerEvent>,
    pub parked_intents: u64,
    pub quarantined_intents: u64,
    pub coverage: DiagnosticsCoverage,
    /// Set only for a Proton account (spec/02 `proton_accounts`).
    pub proton: Option<DiagnosticsProton>,
}

/// The coverage counts (spec/07 `coverage(scope)`), not the operational
/// state that produced them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticsCoverage {
    pub total_messages: u64,
    pub headers_synced: u64,
    pub bodies_acquired: u64,
    pub indexed: u64,
    pub partial: u64,
    pub failed: u64,
    pub pending: u64,
    pub queue_length: u64,
}

/// A Proton account's diagnostics fields (spec/02 `proton_accounts`,
/// spec/10): the last refusal, the listing state and the refresh count.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticsProton {
    pub listing_state: ProtonListingState,
    pub refresh_count: i64,
    pub refusal: Option<DiagnosticsRefusal>,
}

/// `diagnostics_json` as the package reports it (spec/02), its message
/// masked again at assembly.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticsRefusal {
    pub code: Option<i64>,
    pub message: Option<String>,
    pub at_ms: Option<i64>,
}

impl Engine {
    /// `diagnostics_package()` (spec/07, spec/10): every account in
    /// `accounts()` order with its health and counts, plus the store's
    /// schema version and integrity state. The app and OS versions are
    /// the shell's, since the core does not know them. Every string
    /// from stored or server data passes [`redact_addresses`] again.
    pub async fn diagnostics_package(
        &self,
        app_version: &str,
        os_version: &str,
    ) -> Result<DiagnosticsPackage, EngineError> {
        let mut health = self
            .account_health()
            .await?
            .into_iter()
            .map(|row| (row.account_id.clone(), row))
            .collect::<HashMap<_, _>>();
        let (schema_version, accounts, counts, proton) = self
            .read_through(|resources| {
                resources.read(|tx| {
                    let version = schema_version(tx)?;
                    let accounts = accounts(tx)?;
                    let counts = intent_counts(tx)?
                        .into_iter()
                        .map(|count| (count.account_id.clone(), count))
                        .collect::<HashMap<String, IntentCounts>>();
                    let mut proton = HashMap::new();
                    for account in &accounts {
                        if account.kind != AccountKind::Proton {
                            continue;
                        }
                        if let Some(row) = proton_account(tx, &account.account_id)? {
                            proton.insert(account.account_id.clone(), row);
                        }
                    }
                    Ok((version, accounts, counts, proton))
                })
            })
            .await?;

        let mut rows = Vec::with_capacity(accounts.len());
        for account in accounts {
            // Health and this read are separate snapshots: an account
            // that appeared or vanished between them waits for the
            // next package rather than half-reporting one.
            let Some(row) = health.remove(&account.account_id) else {
                continue;
            };
            rows.push(DiagnosticsAccount {
                account_id: account.account_id,
                kind: account.kind,
                state: row.state,
                state_reason: row.state_reason.map(|reason| redact_addresses(&reason)),
                condition: mask_condition(row.condition),
                breaker_events: row.breaker_events,
                parked_intents: counts.get(&row.account_id).map_or(0, |count| count.parked),
                quarantined_intents: counts
                    .get(&row.account_id)
                    .map_or(0, |count| count.quarantined),
                coverage: DiagnosticsCoverage::from_coverage(&row.coverage),
                proton: proton.get(&row.account_id).map(proton_diagnostics),
            });
        }

        Ok(DiagnosticsPackage {
            app_version: app_version.to_owned(),
            os_version: os_version.to_owned(),
            core_version: env!("CARGO_PKG_VERSION").to_owned(),
            schema_version,
            integrity: mask_integrity(self.health().integrity),
            accounts: rows,
        })
    }
}

impl DiagnosticsCoverage {
    fn from_coverage(coverage: &Coverage) -> Self {
        Self {
            total_messages: coverage.total_messages,
            headers_synced: coverage.headers_synced,
            bodies_acquired: coverage.bodies_acquired,
            indexed: coverage.indexed,
            partial: coverage.partial,
            failed: coverage.failed,
            pending: coverage.pending,
            queue_length: coverage.queue_length,
        }
    }
}

/// The Proton row's diagnostics fields, its refusal parsed and masked.
fn proton_diagnostics(account: &ProtonAccount) -> DiagnosticsProton {
    DiagnosticsProton {
        listing_state: account.listing_state,
        refresh_count: account.refresh_count,
        refusal: account.diagnostics_json.as_deref().and_then(parse_refusal),
    }
}

/// `{"v":1,"code":n,"message":"…","at":ms}` (spec/02). A value this
/// version can't parse contributes no refusal rather than failing the
/// whole package.
fn parse_refusal(raw: &str) -> Option<DiagnosticsRefusal> {
    let value: Json = serde_json::from_str(raw).ok()?;
    let code = value.get("code").and_then(Json::as_i64);
    let message = value
        .get("message")
        .and_then(Json::as_str)
        .map(redact_addresses);
    let at_ms = value.get("at").and_then(Json::as_i64);
    // Valid JSON with none of spec/02's refusal keys is not a
    // refusal at all: an all-null row would read as one that lost
    // every field.
    if code.is_none() && message.is_none() && at_ms.is_none() {
        return None;
    }
    Some(DiagnosticsRefusal {
        code,
        message,
        at_ms,
    })
}

/// The condition's stored strings, masked like every other value.
fn mask_condition(condition: AccountCondition) -> AccountCondition {
    match condition {
        AccountCondition::VersionRefused { message } => AccountCondition::VersionRefused {
            message: redact_addresses(&message),
        },
        AccountCondition::SecretStore { reason } => AccountCondition::SecretStore {
            reason: redact_addresses(&reason),
        },
        other => other,
    }
}

/// Map the store's integrity latch, dropping the copy path.
fn mask_integrity(latch: Integrity) -> DiagnosticsIntegrity {
    match latch {
        Integrity::Sound => DiagnosticsIntegrity::Sound,
        Integrity::ReadOnly {
            detail,
            detected_at,
            ..
        } => DiagnosticsIntegrity::ReadOnly {
            detail: redact_addresses(&detail),
            detected_at_ms: detected_at.map(|at| at.as_millis()),
        },
    }
}

#[cfg(test)]
mod tests;
