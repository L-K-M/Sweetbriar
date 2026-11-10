//! The engine's restore (spec/03 "Restore" step 4, plan M1.8g3): quiesce
//! everything that uses the store, swap the qualified stage in under the
//! store lock, and resume on the restored store, or on the same one when
//! the swap was refused.
//!
//! ```text
//!   quiesce: account workers, index and received jobs stopped;
//!            new store work refused (Unavailable); running work ends
//!   swap:    Resources::swap_restore (checks, close, marker, renames, reopen)
//!   ├─ restored: observers on the new writer, windows ResetRequired,
//!   │            memo cleared, jobs resumed fresh, open-time recovery
//!   ├─ refused:  the same store and stage back, jobs resumed as they were
//!   │            (no inventory or capture ran: the restore refused first)
//!   └─ failed:   the engine ends; the next open resolves the marker
//! ```

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use sb_resources::{QualifiedStage, Resources, ResourcesSwapError};

use super::{Engine, StoreAfterRestore, join_failed};
use crate::EngineError;

/// Why [`Engine::restore`] did not hand back a restored engine.
pub enum RestoreError {
    /// Refused before the store closed: nothing moved. The engine comes
    /// back running on the same store, and the stage with it.
    Refused {
        cause: EngineError,
        engine: Box<Engine>,
        stage: Box<QualifiedStage>,
    },
    /// The store had closed, or the engine could not resume: this engine
    /// is gone, with the stage, and the next open resolves whatever the
    /// swap left (spec/03).
    Failed(EngineError),
}

impl std::fmt::Debug for RestoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused { cause, .. } => f.debug_struct("Refused").field("cause", cause).finish(),
            Self::Failed(cause) => f.debug_tuple("Failed").field(cause).finish(),
        }
    }
}

impl std::fmt::Display for RestoreError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Self::Refused { cause, .. } => write!(f, "the restore was refused: {cause}"),
            Self::Failed(cause) => write!(f, "the restore failed: {cause}"),
        }
    }
}

impl std::error::Error for RestoreError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Refused { cause, .. } | Self::Failed(cause) => Some(cause),
        }
    }
}

impl Engine {
    /// Restore `stage` as this engine's store (spec/03 "Restore" step 4):
    /// quiesce, swap and resume. Commands and reads answer `Unavailable`
    /// meanwhile; one in progress finishes first. On success every open
    /// window and subscription answers `ResetRequired` once, the
    /// selections are released, and every account is paused. A refusal
    /// hands this engine back running as before, with the stage. Blocks
    /// on the engine's own blocking pool, never the caller's thread.
    pub async fn restore(
        self,
        stage: QualifiedStage,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Engine, RestoreError> {
        let handle = self.runtime.handle().clone();
        match handle
            .spawn_blocking(move || self.restore_blocking(stage, &cancelled))
            .await
        {
            Ok(outcome) => outcome,
            // A panic is a bug; the engine went with it.
            Err(join) => Err(RestoreError::Failed(join_failed("restore the store", join))),
        }
    }

    fn restore_blocking(
        mut self,
        stage: QualifiedStage,
        cancelled: &AtomicBool,
    ) -> Result<Engine, RestoreError> {
        // An import's inventory and an export's capture resume from no
        // checkpoint, so the quiesce must not interrupt one: hold new
        // ones off, and refuse while one is in flight, before anything
        // stops. The hold lasts until the engine has resumed.
        let _starts = self.imports.hold_starts();
        match self.unresumable_jobs() {
            Ok(false) => {}
            Ok(true) => {
                return Err(RestoreError::Refused {
                    cause: EngineError::Conflict {
                        detail: "an import's inventory or an export's capture is in progress; \
                                 let it finish or cancel it, then restore"
                            .into(),
                    },
                    engine: Box::new(self),
                    stage: Box::new(stage),
                });
            }
            Err(cause) => {
                return Err(RestoreError::Refused {
                    cause,
                    engine: Box::new(self),
                    stage: Box::new(stage),
                });
            }
        }
        // The quiesce: long work stops at its next safe point, new store
        // work is refused, and every running task ends, so this engine
        // holds the store's only handle (spec/03).
        self.accounts.stop();
        self.index.stop();
        self.received.stop();
        // The backup and GC schedule stops at its next step (h3b); a
        // backup's snapshot, which can't stop part way, finishes first.
        self.maintenance.stop();
        self.blocking.quiesce_and_wait();
        let shared = self
            .resources
            .take()
            .expect("the store handle lives until shutdown");
        let resources = match Arc::try_unwrap(shared) {
            Ok(resources) => resources,
            Err(shared) => {
                self.resources = Some(shared);
                let cause = EngineError::internal("a store handle outlived the restore's quiesce");
                return self.resumed(cause, stage);
            }
        };
        match resources.swap_restore(stage, cancelled) {
            Ok(restored) => self.install_restored(restored),
            Err(ResourcesSwapError::Refused {
                cause,
                resources,
                stage,
            }) => {
                self.resources = Some(Arc::new(*resources));
                self.resumed(cause.into(), *stage)
            }
            Err(ResourcesSwapError::Failed(cause)) => Err(RestoreError::Failed(cause.into())),
        }
    }

    /// The restored store in place: its writer is new, so the observers
    /// register again; the windows learn the store was replaced; nothing
    /// kept from the replaced store survives; then the engine resumes.
    fn install_restored(mut self, restored: Resources) -> Result<Engine, RestoreError> {
        self.resources = Some(Arc::new(restored));
        let generations = self
            .observe_store()
            .map_err(|error| RestoreError::Failed(error.into()))?;
        self.windows.store_replaced(generations);
        self.search_memo.clear();
        self.imports.forget_reports();
        // Body and attachment leases hold the replaced store's
        // renditions: their holders republish from the restored one.
        self.leases.close();
        self.resume(StoreAfterRestore::Replaced)
            .map(|()| self)
            .map_err(RestoreError::Failed)
    }

    /// Resume on the same store after a refusal: the refusal's cause and
    /// the stage go back with the engine, unless resuming itself failed.
    fn resumed(
        mut self,
        cause: EngineError,
        stage: QualifiedStage,
    ) -> Result<Engine, RestoreError> {
        match self.resume(StoreAfterRestore::Same) {
            Ok(()) => Err(RestoreError::Refused {
                cause,
                engine: Box::new(self),
                stage: Box::new(stage),
            }),
            Err(error) => Err(RestoreError::Failed(error)),
        }
    }

    /// End the quiesce and run what an open runs: import recovery, the
    /// index target's reconciliation and every scheduler. Account workers
    /// start only for accounts that sync; a restored store's are paused.
    ///
    /// On a latched store (the same one after a refusal: a swap leaves
    /// the latch behind with the replaced root) the engine stays
    /// read-only: only reads and commands are admitted again, and the
    /// workers stay stopped. A swapped-in store without a latch ends
    /// read-only mode. No check runs during a quiesce, so the latch
    /// read here can't change before the workers start.
    fn resume(&mut self, store: StoreAfterRestore) -> Result<(), EngineError> {
        if self.resources().integrity_latch().is_some() {
            self.blocking.resume();
            return self.recover_imports();
        }
        if self.read_only() {
            self.blocking.set_read_only(false);
            self.windows.health_moved();
        }
        self.accounts.resume();
        self.index.resume(store);
        self.received.resume(store);
        self.maintenance.resume();
        self.blocking.resume();
        self.recover_imports()?;
        self.reconcile_index()?;
        self.schedule_threads();
        self.schedule_index();
        self.schedule_received();
        self.schedule_accounts();
        // The restored store's records decide its cadence.
        if self.maintenance.armed() {
            self.schedule_maintenance();
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests;
