//! Preparing a restore (spec/07 "Commands", Backup; plan M1.8i4): a
//! recorded generation is verified, copied beside the data root and
//! qualified while the core stays in use. The swap that puts it in
//! the store's place is the next step (M1.8i5).

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::{Core, CoreError};

/// A generation ready to be swapped in (spec/03 "Restore"): verified
/// against its manifest, staged beside the data root and qualified.
///
/// It holds the store's backup operation until it is swapped in or
/// dropped: meanwhile `start_backup` and another `prepare_restore` are
/// `Conflict`, and the hourly backup waits. Dropping it deletes
/// nothing; the stage stays beside the data root and is never used
/// again.
#[derive(Debug)]
pub struct PreparedRestore {
    prepared: sb_engine::PreparedRestore,
}

impl PreparedRestore {
    /// The generation this was prepared from.
    pub fn generation_id(&self) -> &str {
        self.prepared.generation_id()
    }
}

impl Core {
    /// Prepare a restore of the recorded generation `generation_id`
    /// (`list_backups` names them). The core stays in use throughout
    /// and its data doesn't change. It can take as long as copying the
    /// archive.
    ///
    /// `cancelled` is this call's own flag: raise it and keep
    /// awaiting, and the call answers `Cancelled`. Dropping the future
    /// stops nothing. `NotFound` for an id no generation has;
    /// `Conflict` for a generation that isn't complete, and while a
    /// backup, a generation check or another restore is under way;
    /// `Unavailable` when the generation's directory can't be reached;
    /// `Corrupt` when it no longer matches its manifest. A cancelled
    /// or failed preparation leaves what it staged beside the data
    /// root. It works on a read-only core: a restore is the way out.
    pub async fn prepare_restore(
        &self,
        generation_id: &str,
        cancelled: Arc<AtomicBool>,
    ) -> Result<PreparedRestore, CoreError> {
        let prepared: Option<sb_engine::PreparedRestore> = self
            .engine()
            .prepare_restore(generation_id, cancelled)
            .await?;
        prepared
            .map(|prepared| PreparedRestore { prepared })
            .ok_or(CoreError::Cancelled)
    }
}

#[cfg(test)]
mod tests;
