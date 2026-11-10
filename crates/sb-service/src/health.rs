//! `health()` and `subscribe_health` (spec/07 "Queries",
//! "Subscriptions"; plan M1.8h5b3): what the shell must tell the user
//! about the store itself. Today that is one thing: an integrity
//! finding, after which the core is read-only (spec/03 "Integrity").

use std::path::PathBuf;

use crate::{Core, CoreError};

/// The store's health as the shell shows it (spec/07).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Health {
    pub integrity: Integrity,
}

/// Whether an integrity check found the database damaged (spec/03
/// "Integrity").
#[derive(Debug, Clone, PartialEq, Eq)]
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
        /// is published.
        copy: Option<PathBuf>,
    },
}

impl Health {
    fn from_engine(health: sb_engine::Health) -> Self {
        let integrity = match health.integrity {
            sb_engine::Integrity::Sound => Integrity::Sound,
            sb_engine::Integrity::ReadOnly {
                detail,
                detected_at,
                copy,
            } => Integrity::ReadOnly {
                detail,
                // Named, so the engine's item re-export is what this
                // compiles against (spec/01 rule 3).
                detected_at_ms: detected_at.map(|at: sb_engine::Timestamp| at.as_millis()),
                copy,
            },
        };
        Self { integrity }
    }
}

impl Core {
    /// `health()` (spec/07 "Queries"): answered from memory, without
    /// I/O. Never blocks.
    pub fn health(&self) -> Health {
        Health::from_engine(self.engine().health())
    }

    /// `subscribe_health` (spec/07 "Subscriptions"): tells when the
    /// answer of [`Core::health`] changed, pulled with
    /// [`HealthSubscription::next`]. It starts at the changes so far:
    /// subscribe first, then read `health()`, and none is missed.
    /// Never blocks.
    pub fn subscribe_health(&self) -> HealthSubscription {
        HealthSubscription(self.engine().subscribe_health())
    }

    /// Run the store's full integrity check now (the `test-seams`
    /// feature), answering whether it found damage: the layer above
    /// proves its read-only answers before the weekly job exists.
    #[cfg(feature = "test-seams")]
    pub async fn check_integrity_now(&self) -> Result<bool, CoreError> {
        Ok(self.engine().check_integrity_now().await?)
    }
}

/// The health stream (spec/07 "Subscriptions"): one in-flight `next`
/// at a time, in-memory only.
pub struct HealthSubscription(sb_engine::HealthSubscription);

impl HealthSubscription {
    /// Wait until `health()` changed since this subscription's last
    /// answer; several changes are one answer, and the caller reads
    /// `health()` again. Shutdown ends a wait with `Unavailable`; a
    /// cancelled call moves nothing.
    pub async fn next(&self) -> Result<(), CoreError> {
        Ok(self.0.next().await?)
    }
}

#[cfg(test)]
mod tests;
