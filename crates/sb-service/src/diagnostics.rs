//! `diagnostics_package()` (spec/10 "Privacy", spec/07; plan M2.7c4):
//! the service-owned mirror of the engine's package, converted field by
//! field (spec/01 rule 3), serialized as the one UTF-8 JSON document the
//! shell previews. The core writes no file.

use serde_json::{Map, Value};

use crate::{AccountBreakerEvent, AccountCondition, AccountKind, AccountState, Core, CoreError};

/// `proton_accounts.listing_state` (spec/02).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ProtonListingState {
    None,
    Listing,
    Complete,
}

/// The diagnostics package (spec/10): the whole document's data.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticsPackage {
    pub app_version: String,
    pub os_version: String,
    pub core_version: String,
    pub schema_version: i64,
    pub integrity: DiagnosticsIntegrity,
    pub accounts: Vec<DiagnosticsAccount>,
}

/// The store's integrity state (spec/03 "Integrity").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DiagnosticsIntegrity {
    Sound,
    ReadOnly {
        detail: String,
        detected_at_ms: Option<i64>,
    },
}

/// One account as the package reports it (spec/10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticsAccount {
    pub account_id: String,
    pub kind: AccountKind,
    pub state: AccountState,
    pub state_reason: Option<String>,
    pub condition: AccountCondition,
    pub breaker_events: Vec<AccountBreakerEvent>,
    pub parked_intents: u64,
    pub quarantined_intents: u64,
    pub coverage: DiagnosticsCoverage,
    pub proton: Option<DiagnosticsProton>,
}

/// The coverage counts (spec/07 `coverage(scope)`).
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

/// A Proton account's diagnostics fields (spec/02, spec/10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticsProton {
    pub listing_state: ProtonListingState,
    pub refresh_count: i64,
    pub refusal: Option<DiagnosticsRefusal>,
}

/// The last refusal as `diagnostics_json` holds it (spec/02), masked.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiagnosticsRefusal {
    pub code: Option<i64>,
    pub message: Option<String>,
    pub at_ms: Option<i64>,
}

impl Core {
    /// `diagnostics_package()` (spec/07, spec/10): the package's text.
    /// `app_version` and `os_version` come from the shell; the core
    /// knows only its own version.
    pub async fn diagnostics_package(
        &self,
        app_version: &str,
        os_version: &str,
    ) -> Result<String, CoreError> {
        let package = self
            .engine()
            .diagnostics_package(app_version, os_version)
            .await?;
        Ok(DiagnosticsPackage::from_engine(package).to_json())
    }
}

impl DiagnosticsPackage {
    fn from_engine(package: sb_engine::DiagnosticsPackage) -> Self {
        Self {
            app_version: package.app_version,
            os_version: package.os_version,
            core_version: package.core_version,
            schema_version: package.schema_version,
            integrity: DiagnosticsIntegrity::from_engine(package.integrity),
            accounts: package
                .accounts
                .into_iter()
                .map(DiagnosticsAccount::from_engine)
                .collect(),
        }
    }

    /// The canonical v1 JSON document (spec/10). Field order is fixed;
    /// absent values are omitted rather than written null.
    fn to_json(&self) -> String {
        let mut root = Map::new();
        root.insert("core_version".to_owned(), self.core_version.clone().into());
        root.insert("app_version".to_owned(), self.app_version.clone().into());
        root.insert("os_version".to_owned(), self.os_version.clone().into());
        root.insert("schema_version".to_owned(), self.schema_version.into());
        root.insert("integrity".to_owned(), self.integrity.to_json());
        root.insert(
            "accounts".to_owned(),
            Value::Array(
                self.accounts
                    .iter()
                    .map(DiagnosticsAccount::to_json)
                    .collect(),
            ),
        );
        Value::Object(root).to_string()
    }
}

impl DiagnosticsIntegrity {
    fn from_engine(integrity: sb_engine::DiagnosticsIntegrity) -> Self {
        match integrity {
            sb_engine::DiagnosticsIntegrity::Sound => Self::Sound,
            sb_engine::DiagnosticsIntegrity::ReadOnly {
                detail,
                detected_at_ms,
            } => Self::ReadOnly {
                detail,
                detected_at_ms,
            },
        }
    }

    fn to_json(&self) -> Value {
        match self {
            Self::Sound => object([("state", "sound".into())]),
            Self::ReadOnly {
                detail,
                detected_at_ms,
            } => {
                let fields = vec![
                    ("state", "read_only".into()),
                    ("detail", detail.clone().into()),
                    (
                        "detected_at_ms",
                        detected_at_ms.map_or(Value::Null, |at| at.into()),
                    ),
                ];
                object(fields)
            }
        }
    }
}

impl DiagnosticsAccount {
    fn from_engine(account: sb_engine::DiagnosticsAccount) -> Self {
        Self {
            account_id: account.account_id,
            kind: match account.kind {
                sb_engine::AccountKind::Local => AccountKind::Local,
                sb_engine::AccountKind::Imap => AccountKind::Imap,
                sb_engine::AccountKind::Proton => AccountKind::Proton,
            },
            state: match account.state {
                sb_engine::AccountState::Active => AccountState::Active,
                sb_engine::AccountState::Paused => AccountState::Paused,
                sb_engine::AccountState::NeedsAttention => AccountState::NeedsAttention,
                sb_engine::AccountState::Disconnected => AccountState::Disconnected,
            },
            state_reason: account.state_reason,
            condition: AccountCondition::from_engine(account.condition),
            breaker_events: account
                .breaker_events
                .into_iter()
                .map(|event| AccountBreakerEvent {
                    event_id: event.event_id,
                    mailbox_id: event.mailbox_id,
                    disappeared: event.disappeared,
                    detected_at_ms: event.detected_at_ms,
                })
                .collect(),
            parked_intents: account.parked_intents,
            quarantined_intents: account.quarantined_intents,
            coverage: DiagnosticsCoverage::from_engine(&account.coverage),
            proton: account.proton.map(DiagnosticsProton::from_engine),
        }
    }

    fn to_json(&self) -> Value {
        let mut fields = vec![
            ("account_id", self.account_id.clone().into()),
            ("kind", kind_str(self.kind).into()),
            ("state", state_str(self.state).into()),
            ("condition", condition_json(&self.condition)),
            (
                "breaker_events",
                Value::Array(self.breaker_events.iter().map(breaker_json).collect()),
            ),
            ("parked_intents", self.parked_intents.into()),
            ("quarantined_intents", self.quarantined_intents.into()),
            ("coverage", self.coverage.to_json()),
        ];
        if let Some(reason) = &self.state_reason {
            fields.push(("reason", reason.clone().into()));
        }
        if let Some(proton) = &self.proton {
            fields.push(("proton", proton.to_json()));
        }
        object(fields)
    }
}

impl DiagnosticsCoverage {
    fn from_engine(coverage: &sb_engine::DiagnosticsCoverage) -> Self {
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

    fn to_json(&self) -> Value {
        object(vec![
            ("total_messages", self.total_messages.into()),
            ("headers_synced", self.headers_synced.into()),
            ("bodies_acquired", self.bodies_acquired.into()),
            ("indexed", self.indexed.into()),
            ("partial", self.partial.into()),
            ("failed", self.failed.into()),
            ("pending", self.pending.into()),
            ("queue_length", self.queue_length.into()),
        ])
    }
}

impl DiagnosticsProton {
    fn from_engine(proton: sb_engine::DiagnosticsProton) -> Self {
        Self {
            listing_state: match proton.listing_state {
                sb_engine::ProtonListingState::None => ProtonListingState::None,
                sb_engine::ProtonListingState::Listing => ProtonListingState::Listing,
                sb_engine::ProtonListingState::Complete => ProtonListingState::Complete,
            },
            refresh_count: proton.refresh_count,
            refusal: proton.refusal.map(|refusal| DiagnosticsRefusal {
                code: refusal.code,
                message: refusal.message,
                at_ms: refusal.at_ms,
            }),
        }
    }

    fn to_json(&self) -> Value {
        let mut fields = vec![
            ("listing_state", listing_str(self.listing_state).into()),
            ("refresh_count", self.refresh_count.into()),
        ];
        if let Some(refusal) = &self.refusal {
            let mut refusal_fields = Vec::new();
            if let Some(code) = refusal.code {
                refusal_fields.push(("code", code.into()));
            }
            if let Some(message) = &refusal.message {
                refusal_fields.push(("message", message.clone().into()));
            }
            if let Some(at_ms) = refusal.at_ms {
                refusal_fields.push(("at_ms", at_ms.into()));
            }
            fields.push(("last_refusal", object(refusal_fields)));
        }
        object(fields)
    }
}

/// One breaker event's report row (spec/07): id, mailbox, count, time.
fn breaker_json(event: &AccountBreakerEvent) -> Value {
    object(vec![
        ("id", event.event_id.clone().into()),
        (
            "mailbox_id",
            event.mailbox_id.map_or(Value::Null, |id| id.into()),
        ),
        ("count", event.disappeared.into()),
        ("detected_at_ms", event.detected_at_ms.into()),
    ])
}

/// The condition as a tag, with its payload for the ones that carry one.
fn condition_json(condition: &AccountCondition) -> Value {
    let (tag, payload): (&str, Option<Value>) = match condition {
        AccountCondition::Paused => ("paused", None),
        AccountCondition::Disconnected => ("disconnected", None),
        AccountCondition::SignInAgain => ("sign_in_again", None),
        AccountCondition::Verify => ("verify", None),
        AccountCondition::Healthy => ("healthy", None),
        AccountCondition::VersionRefused { message } => (
            "version_refused",
            Some(object(vec![("message", message.clone().into())])),
        ),
        AccountCondition::SecretStore { reason } => (
            "secret_store",
            Some(object(vec![("reason", reason.clone().into())])),
        ),
        AccountCondition::Offline { next_retry_ms } => (
            "offline",
            Some(object(vec![("next_retry_ms", (*next_retry_ms).into())])),
        ),
        AccountCondition::NotAnswering { next_retry_ms } => (
            "not_answering",
            Some(object(vec![("next_retry_ms", (*next_retry_ms).into())])),
        ),
        AccountCondition::RateLimited { until_ms } => (
            "rate_limited",
            Some(object(vec![("until_ms", (*until_ms).into())])),
        ),
    };
    match payload {
        None => Value::String(tag.to_owned()),
        Some(payload) => object(vec![(tag, payload)]),
    }
}

fn kind_str(kind: AccountKind) -> &'static str {
    match kind {
        AccountKind::Local => "local",
        AccountKind::Imap => "imap",
        AccountKind::Proton => "proton",
    }
}

fn state_str(state: AccountState) -> &'static str {
    match state {
        AccountState::Active => "active",
        AccountState::Paused => "paused",
        AccountState::NeedsAttention => "needs_attention",
        AccountState::Disconnected => "disconnected",
    }
}

fn listing_str(state: ProtonListingState) -> &'static str {
    match state {
        ProtonListingState::None => "none",
        ProtonListingState::Listing => "listing",
        ProtonListingState::Complete => "complete",
    }
}

/// A JSON object from `(key, value)` pairs, in order.
fn object<'a>(fields: impl IntoIterator<Item = (&'a str, Value)>) -> Value {
    Value::Object(Map::from_iter(
        fields
            .into_iter()
            .map(|(key, value)| (key.to_owned(), value)),
    ))
}

#[cfg(test)]
mod tests;
