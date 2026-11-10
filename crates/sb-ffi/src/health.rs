//! `health()` and `subscribe_health` over FFI (spec/07 "Queries",
//! "Subscriptions"; plan M1.8h5b3): whether the store is read-only
//! after an integrity finding (spec/03 "Integrity"), and a
//! Rust-owned subscription that tells the shell when that changes.

use std::sync::Arc;

use crate::api::Core;
use crate::cancel::{CancelToken, until_cancelled};
use crate::error::CoreError;

/// The store's health as the shell shows it (spec/07).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Health {
    pub integrity: Integrity,
}

/// Whether an integrity check found the database damaged (spec/03
/// "Integrity").
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum Integrity {
    /// No finding is recorded.
    Sound,
    /// A finding latched the store. Queries go on; every command that
    /// writes is `Corrupt`; no account syncs and no background job
    /// runs. Only a restore ends it.
    ReadOnly {
        /// The finding, or why its record can't be read.
        detail: String,
        /// When the check found it, milliseconds since the epoch;
        /// absent when the record can't be read.
        detected_at_ms: Option<i64>,
        /// The copy of the database files under `recovery/`, once it
        /// is published. A string for display: paths don't cross FFI
        /// (spec/07 "DTOs").
        copy_path: Option<String>,
    },
}

impl Health {
    fn from_service(health: sb_service::Health) -> Self {
        let integrity = match health.integrity {
            sb_service::Integrity::Sound => Integrity::Sound,
            sb_service::Integrity::ReadOnly {
                detail,
                detected_at_ms,
                copy,
            } => Integrity::ReadOnly {
                detail,
                detected_at_ms,
                copy_path: copy.map(|path| path.to_string_lossy().into_owned()),
            },
        };
        Self { integrity }
    }
}

/// The health stream (spec/07 "Subscriptions"): one in-flight `next`
/// at a time, in-memory only.
#[derive(uniffi::Object)]
pub struct HealthSubscription {
    inner: sb_service::HealthSubscription,
}

#[uniffi::export]
impl HealthSubscription {
    /// Wait until `health()` changed since this subscription's last
    /// answer; the shell then reads `health()` again. One call at a
    /// time: a second concurrent call is `InvalidInput`. A cancelled
    /// call answers `Cancelled` and moves nothing, so the next call
    /// still sees what it missed. Shutdown ends a wait with
    /// `Unavailable`.
    pub async fn next(&self, token: Arc<CancelToken>) -> Result<(), CoreError> {
        until_cancelled(&token, self.inner.next()).await??;
        Ok(())
    }
}

#[uniffi::export]
impl Core {
    /// `health()` (spec/07 "Queries"): whether the store is read-only
    /// after an integrity finding, with the finding, its time and its
    /// copy. Answered from memory. A query: the token races the
    /// core's lock.
    pub async fn health(&self, token: Arc<CancelToken>) -> Result<Health, CoreError> {
        let health = self.query(&token, async |core| Ok(core.health())).await?;
        Ok(Health::from_service(health))
    }

    /// `subscribe_health` (spec/07 "Subscriptions"): changes of
    /// `health()` from now on. Subscribe first, then read `health()`,
    /// and none is missed. A query: the token races the core's lock.
    pub async fn subscribe_health(
        &self,
        token: Arc<CancelToken>,
    ) -> Result<Arc<HealthSubscription>, CoreError> {
        let inner = self
            .query(&token, async |core| Ok(core.subscribe_health()))
            .await?;
        Ok(Arc::new(HealthSubscription { inner }))
    }
}

#[cfg(test)]
mod tests;
