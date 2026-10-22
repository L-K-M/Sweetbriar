//! Complete current-summary ledger flow; automatic scheduling belongs to Engine.

use sb_store::seen;

use crate::{Resources, StoreError, WriteFuture, WritePriority};

/// Durable capability and owed sweep revisions, not FTS coverage.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ReceivedRevisions {
    pub target: u32,
    pub inputs: u64,
    pub swept: u64,
}

#[cfg(test)]
mod tests;

/// Opaque pass-local continuation; only committed publication returns the next one.
#[derive(Debug)]
pub struct ReceivedSweep {
    inner: seen::SeenScan,
}

/// Normalized outside SQLite, with no unchecked constructor or mutable cursor.
#[derive(Debug)]
pub struct ReceivedPreparation {
    inner: seen::SeenPrepared,
}

/// A stale source is recaptured quietly; a failed write returns no continuation.
#[derive(Debug)]
pub enum ReceivedCommit {
    Continue(ReceivedSweep),
    Recapture(ReceivedSweep),
    Complete,
}

impl Resources {
    pub fn received_address_revisions(&self) -> Result<ReceivedRevisions, StoreError> {
        self.read(seen::revisions).map(|value| ReceivedRevisions {
            target: value.target,
            inputs: value.inputs,
            swept: value.swept,
        })
    }

    pub fn begin_received_sweep(&self) -> Result<Option<ReceivedSweep>, StoreError> {
        self.read(seen::begin)
            .map(|scan| scan.map(|inner| ReceivedSweep { inner }))
    }

    /// Blocking short read, then pure decoding/normalization after it releases.
    pub fn prepare_received_step(
        &self,
        sweep: ReceivedSweep,
    ) -> Result<ReceivedPreparation, StoreError> {
        let captured = self.read(|tx| seen::capture(tx, sweep.inner))?;
        seen::prepare(captured).map(|inner| ReceivedPreparation { inner })
    }

    pub fn commit_received_step(
        &self,
        preparation: ReceivedPreparation,
    ) -> WriteFuture<ReceivedCommit> {
        self.write(WritePriority::Index, move |tx| {
            seen::publish(tx, preparation.inner).map(|value| match value {
                seen::SeenCommit::Continue(inner) => {
                    ReceivedCommit::Continue(ReceivedSweep { inner })
                }
                seen::SeenCommit::Recapture(inner) => {
                    ReceivedCommit::Recapture(ReceivedSweep { inner })
                }
                seen::SeenCommit::Complete => ReceivedCommit::Complete,
            })
        })
    }
}
