//! Engine-level restore (spec/03 "Restore" steps 4–5).
//!
//! Verification, staging and qualification run while the engine stays
//! live — a restore of a large backup must not freeze the app. Only the
//! swap itself quiesces: `BlockingWork::quiesce` refuses new commands
//! (`Unavailable`) and drains running ones, the workers unwind through
//! their `closing` checks, every SQLite connection closes, the stage
//! becomes the data root, and the restored store reopens under the
//! lease that never left this process. Windows and subscriptions see a
//! generation bump — every stale read answers `ResetRequired`, exactly
//! like a reopened archive.
//!
//! A failure after the close leaves the durable marker behind;
//! `RestoringResources::resume` resolves it the way a fresh open
//! would, so the engine comes back on whichever image survived.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use sb_resources::{Resources, RestoreMetadata, VerifiedExternalBackup};

use crate::engine::{BlockingWork, Engine, announce_generations, join_failed};
use crate::error::EngineError;

/// Clear the restore slot when the call ends, whatever it returned.
struct RestoreGate<'a>(&'a Engine);

impl Drop for RestoreGate<'_> {
    fn drop(&mut self) {
        self.0.end_restore();
    }
}

/// Open the blocking gate again when the swap phase ends — success or
/// failure — so commands stop seeing `Unavailable` once the engine
/// holds a store again. On an unrecoverable close the guard's owner
/// keeps it dropped-but-alive the same way; commands meet
/// `Unavailable` rather than a panic, and `shutdown` still works.
struct ResumeGate(Arc<BlockingWork>);

impl Drop for ResumeGate {
    fn drop(&mut self) {
        self.0.resume();
    }
}

impl Engine {
    /// Restore the store from `generation` inside `parent` (a backup
    /// image directory, internal or external; spec/03 "Restore").
    ///
    /// New commands return `Unavailable` once the swap phase starts;
    /// an already running command finishes first. On success every
    /// open window and subscription is stale and must reset. The
    /// restored image's paused accounts stay paused until the user
    /// resumes them; quarantined intents release only after each
    /// account's reconcile — this call performs no sends, moves,
    /// flags or deletes itself.
    pub async fn restore_backup(&self, parent: &Path, generation: &str) -> Result<(), EngineError> {
        if !self.begin_restore() {
            return Err(EngineError::unavailable("a restore is already in progress"));
        }
        let _gate = RestoreGate(self);
        self.run_restore(parent.to_owned(), generation.to_owned())
            .await
    }

    /// The blocking half of [`Engine::restore_backup`], off the caller
    /// thread: the store work is synchronous, so it runs on the
    /// engine's blocking pool like every other store command.
    async fn run_restore(&self, parent: PathBuf, generation: String) -> Result<(), EngineError> {
        let resources = self.resources();
        let blocking = Arc::clone(self.blocking());
        let windows = Arc::clone(self.windows());
        let index = Arc::clone(self.indexer());
        let received = Arc::clone(self.received());
        let imports = Arc::clone(self.imports());
        let leases = Arc::clone(self.rendition_leases());
        let search_memo = Arc::clone(self.search_memo());
        let cell = self.resources_cell();
        let job = self.runtime_handle().spawn_blocking(move || {
            let cancel = AtomicBool::new(false);
            // Steps 1–3 run live: verify the image, copy it into a
            // private sibling, and quarantine it inside that copy.
            let Some(selection) = resources
                .verify_external_backup(&parent, &generation, &cancel)
                .map_err(EngineError::from)?
            else {
                return Err(EngineError::invalid_input(
                    "generation",
                    "no verified backup of that name",
                ));
            };
            let metadata = restore_metadata(&selection)?;
            let prepared = resources
                .stage_restore(selection, &parent.join(&generation), metadata, &cancel)
                .map_err(EngineError::from)?;
            let stage = resources
                .qualify_restore_stage(prepared, &cancel)
                .map_err(EngineError::from)?;
            // Step 4: quiesce. Stopping the workers first wakes the
            // parked ones; `paused` unwinds the rest, and the drain
            // returns only when every counted task released its store.
            index.stop();
            received.stop();
            blocking.quiesce();
            let _resume = ResumeGate(blocking);
            windows.release_selections();
            leases.close();
            // The handle I captured for steps 1–3 is a stale second
            // clone now; let it go before unwrapping the cell's.
            let data_root = resources.data_root().to_owned();
            drop(resources);
            // Detach the handle: counted work is drained, so a failed
            // unwrap means an uncounted caller still holds a clone —
            // put it back, reopen the gate, and let a retry decide.
            let held = {
                let mut slot = cell.lock().expect("the resources lock poisoned");
                match slot.take() {
                    Some(held) => match Arc::try_unwrap(held) {
                        Ok(held) => held,
                        Err(held) => {
                            *slot = Some(held);
                            return Err(EngineError::unavailable(
                                "a restore could not isolate the store; retry",
                            ));
                        }
                    },
                    None => return Err(EngineError::unavailable("the store is closing")),
                }
            };
            // Close, swap, reopen. The lease survives a `swap_restore`
            // error, so `resume` still reopens whichever image the
            // marker protocol resolved. Only a close or resume failure
            // spends the lease — then a plain open lands on the same
            // recovery path a fresh start would.
            let mut failure = None;
            let reopened = match held.close_for_restore() {
                Ok(restoring) => {
                    if let Err(error) = restoring.swap_restore(stage, &cancel) {
                        failure = Some(EngineError::from(error));
                    }
                    match restoring.resume() {
                        Ok(reopened) => Some(reopened),
                        Err(error) => {
                            failure = failure.or_else(|| Some(EngineError::from(error)));
                            None
                        }
                    }
                }
                Err(error) => {
                    failure = Some(EngineError::from(error));
                    None
                }
            };
            let restored = match reopened {
                Some(restored) => restored,
                None => Resources::open(&data_root).map_err(EngineError::from)?,
            };
            // The epoch moves first: every pre-restore window and
            // subscription is stale whatever the new image reports.
            windows.restored();
            announce_generations(&windows, &received, &restored)?;
            *cell.lock().expect("the resources lock poisoned") = Some(Arc::new(restored));
            imports.reset();
            index.reset();
            received.reset();
            search_memo.reset();
            match failure {
                Some(error) => Err(error),
                None => Ok(()),
            }
        });
        match job.await {
            Ok(Ok(())) => {
                // The restored image's own recovery: the same kicks
                // `open` gives a fresh engine.
                self.recover_imports()?;
                self.schedule_threads();
                self.schedule_index();
                self.schedule_received();
                Ok(())
            }
            Ok(Err(error)) => Err(error),
            Err(join) => Err(join_failed("restore the backup", join)),
        }
    }
}

/// The [`RestoreMetadata`] a verified image carries: what the manifest
/// declared, bound to the exact bytes verification hashed.
fn restore_metadata(selection: &VerifiedExternalBackup) -> Result<RestoreMetadata, EngineError> {
    let convert = |value: u64, field: &'static str| {
        i64::try_from(value).map_err(|_| {
            EngineError::internal(format!("the backup manifest's {field} overflows i64"))
        })
    };
    Ok(RestoreMetadata {
        manifest_sha256: selection.manifest_sha256(),
        object_count: convert(selection.object_count(), "object count")?,
        total_bytes: convert(selection.total_bytes(), "total bytes")?,
        completed_at: selection.created_at(),
    })
}
