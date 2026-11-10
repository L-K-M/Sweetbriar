//! `health()` (spec/07 "Queries"; plan M1.8h5b2): what the shell must
//! tell the user about the store itself. Today that is one thing: an
//! integrity finding, after which the engine is read-only (spec/03
//! "Integrity").
//!
//! ```text
//!   store latches ─► observer ─► long work and workers stop
//!                            └─► health subscriptions wake ─► health()
//!   restore swaps a store without a latch in ─► the same wake
//! ```

use std::path::PathBuf;

use sb_resources::Timestamp;

use crate::Engine;

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
        /// When the check found it; absent when the record can't be
        /// read.
        detected_at: Option<Timestamp>,
        /// The copy of the database files under `recovery/`, once it
        /// is published: absent while it is being made and when it
        /// could not be.
        copy: Option<PathBuf>,
    },
}

impl Engine {
    /// `health()` (spec/07), answered from the store's memory with no
    /// I/O. It reports a finding from the moment the store refuses new
    /// file work, which is just before health subscriptions wake.
    pub fn health(&self) -> Health {
        let integrity = match self.resources().integrity_latch() {
            None => Integrity::Sound,
            Some(latch) => Integrity::ReadOnly {
                detail: latch.detail,
                detected_at: latch.detected_at,
                copy: latch.copy,
            },
        };
        Health { integrity }
    }
}

#[cfg(test)]
mod tests;
