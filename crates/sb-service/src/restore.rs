//! A restore in its two calls (spec/07 "Commands", Backup; plan
//! M1.8i4 and M1.8i5).
//!
//! ```text
//!   prepare_restore(&self)   verify, stage, qualify; the core in use
//!   restore(self)            quiesce, swap, resume; the core consumed
//!                            and handed back, restored or as it was
//! ```
//!
//! Preparing can take as long as copying the archive and changes
//! nothing; the swap is short and replaces the store. The second call
//! is the command: it takes the idempotency key.

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use crate::{Core, CoreError, Receipt};

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

/// What [`Core::restore`] answers when nothing refused or failed.
#[derive(Debug)]
pub struct Restored {
    /// The core on the restored store, or as it was when the key had
    /// already run a restore.
    pub core: Core,
    /// The command's stored receipt, `{"generation_id":...}`. An error
    /// means the store was restored and its receipt could not be
    /// recorded: the same key would restore again.
    pub receipt: Result<Receipt, CoreError>,
}

/// Why [`Core::restore`] handed back no restored core.
#[derive(Debug)]
pub enum RestoreFailure {
    /// Refused before the store closed. Nothing moved and no key was
    /// spent: the core is back, running as before, with the prepared
    /// restore, which a later call can swap in.
    Refused {
        cause: CoreError,
        core: Box<Core>,
        prepared: Box<PreparedRestore>,
    },
    /// The store had closed, or the core could not resume on it. The
    /// core is gone. This can't say whether the swap went through:
    /// the next `open_core` settles what it left (spec/03). Nothing
    /// was deleted either way.
    Failed(CoreError),
}

impl std::fmt::Display for RestoreFailure {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused { cause, .. } => write!(f, "the restore was refused: {cause}"),
            Self::Failed(cause) => write!(f, "the restore failed: {cause}"),
        }
    }
}

impl std::error::Error for RestoreFailure {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Refused { cause, .. } | Self::Failed(cause) => Some(cause),
        }
    }
}

impl Core {
    /// `start_restore` (spec/07), the command: swap `prepared` in as
    /// this core's store (03 "Restore" step 4) under
    /// `idempotency_key`. It consumes the core and hands one back.
    ///
    /// On success the core runs on the restored store: every window
    /// and subscription opened before answers `ResetRequired` once,
    /// selections are released and every account is paused. The
    /// replaced store stays beside the data root under its preserved
    /// name. The receipt is written in the restored store.
    ///
    /// A key that already ran a restore in this store answers its
    /// receipt, and nothing moves; `prepared` is dropped unused. A key
    /// spent on another command is a refusal (`Conflict`), like an
    /// unusable one (`InvalidInput`), an import's inventory or an
    /// export's capture in progress (`Conflict`), a raised `cancelled`
    /// (`Unavailable`) and a stage the swap's own checks turn down.
    ///
    /// Await it to the end. Dropping the future stops nothing and
    /// drops the core it would have answered with. To cancel, raise
    /// `cancelled` and keep awaiting.
    pub async fn restore(
        self,
        idempotency_key: &str,
        prepared: PreparedRestore,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Restored, RestoreFailure> {
        let swapped: Result<
            sb_engine::Restored,
            sb_engine::RestoreError<sb_engine::PreparedRestore>,
        > = self
            .into_engine()
            .start_restore(idempotency_key, prepared.prepared, cancelled)
            .await;
        match swapped {
            Ok(restored) => Ok(Restored {
                receipt: match restored.receipt {
                    Ok(receipt) => Ok(Receipt::from_engine(&receipt)),
                    Err(cause) => Err(cause.into()),
                },
                core: Core::around(restored.engine),
            }),
            Err(sb_engine::RestoreError::Refused {
                cause,
                engine,
                stage,
            }) => Err(RestoreFailure::Refused {
                cause: cause.into(),
                core: Box::new(Core::around(*engine)),
                prepared: Box::new(PreparedRestore { prepared: *stage }),
            }),
            Err(sb_engine::RestoreError::Failed(cause)) => {
                Err(RestoreFailure::Failed(cause.into()))
            }
        }
    }
}

#[cfg(test)]
mod tests;
