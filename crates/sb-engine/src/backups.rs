//! Backup generations through the engine (spec/07 "Commands", Backup;
//! plan M1.8i). The store takes, retains, checks and restores them
//! (spec/03 "Backup generations", "Restore"); the scheduler takes one
//! an hour while there were changes ([`crate::maintenance`]). This is
//! what a shell asks: which generations there are, and to take one now.

use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use sb_resources::{
    BackupDestination, BackupGeneration, BackupGenerationState, BackupReceipt, Receipt, Resources,
    StartedBackup, StoreError, receipt_generation_id,
};
use tokio::sync::oneshot;

use crate::engine::{Spawn, validate_idempotency_key};
use crate::{Engine, EngineError};

/// What `start_backup` answers: the command's receipt and the
/// generation it names.
#[derive(Debug)]
pub struct BackupStarted {
    receipt: Receipt,
    generation_id: String,
    copy: Option<BackupCopy>,
}

impl BackupStarted {
    pub fn receipt(&self) -> &Receipt {
        &self.receipt
    }

    /// The generation the key started, now or when it was first used.
    pub fn generation_id(&self) -> &str {
        &self.generation_id
    }

    /// The copy this call started, to wait on. `None` when the key had
    /// started its generation before: nothing was started now, and
    /// `list_backups` says what became of that one.
    pub fn into_copy(self) -> Option<BackupCopy> {
        self.copy
    }
}

/// A started backup's copy. It runs in the engine whoever holds this:
/// dropping it, or the wait, stops nothing.
#[derive(Debug)]
pub struct BackupCopy {
    ended: oneshot::Receiver<BackupEnd>,
}

impl BackupCopy {
    /// Wait for the copy to end, and learn how.
    pub async fn finished(self) -> BackupEnd {
        self.ended.await.unwrap_or_else(|_| {
            BackupEnd::Failed("the backup's copy ended without saying how".to_owned())
        })
    }
}

/// How a started backup's copy ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BackupEnd {
    /// The generation is complete. `after` is an error from the work
    /// that follows completion (retention): the generation stands, and
    /// the next backup tries that work again.
    Complete { after: Option<String> },
    /// A shutdown stopped the copy at its next object. The generation
    /// stays in progress until the next backup marks it failed and
    /// removes what it left.
    Stopped,
    /// The copy failed, with the error's text. Only the stop's own
    /// answer is a stop: an I/O error under one is a failure. So is a
    /// store that turned read-only under the copy, whichever the copy
    /// met first, the store's refusal or the stop its latch raises:
    /// the text names the finding. The generation is left like a
    /// stopped one's.
    Failed(String),
}

/// The copies `start_backup` started that still run. They are long
/// work: the integrity check waits for an engine without one.
#[derive(Default)]
pub(crate) struct BackupCopies {
    running: AtomicUsize,
    /// Called on the copy's thread once the command has answered,
    /// before the first object: a test holds the copy here.
    #[cfg(test)]
    pub(crate) before_copy: std::sync::Mutex<Option<Arc<dyn Fn() + Send + Sync>>>,
}

impl BackupCopies {
    pub(crate) fn running(&self) -> bool {
        self.running.load(Ordering::SeqCst) > 0
    }

    fn begin(self: &Arc<Self>) -> Copying {
        self.running.fetch_add(1, Ordering::SeqCst);
        Copying(Arc::clone(self))
    }
}

/// One started backup, from before its snapshot until its copy ends.
struct Copying(Arc<BackupCopies>);

impl Drop for Copying {
    fn drop(&mut self) {
        self.0.running.fetch_sub(1, Ordering::SeqCst);
    }
}

/// How the copy ended, from its result, the stop flag as it returned
/// and the generation's row.
fn copy_end(
    resources: &Resources,
    generation_id: &str,
    result: Result<BackupReceipt, StoreError>,
    stopped: bool,
) -> BackupEnd {
    let error = match result {
        Ok(_) => return BackupEnd::Complete { after: None },
        Err(error) => error,
    };
    // Completion commits before retention runs, and retention's error
    // comes back through the same result. The row says which it was; a
    // row that can't be read leaves the error to speak for itself.
    let complete = resources.backup_generations().is_ok_and(|generations| {
        generations.iter().any(|generation| {
            generation.generation_id == generation_id
                && generation.state == BackupGenerationState::Complete
        })
    });
    if complete {
        return BackupEnd::Complete {
            after: Some(error.to_string()),
        };
    }
    match error {
        // What a copy the stop ended answers at its next object. A
        // store that latches raises the stop too, after it starts
        // refusing: the copy meets either first, and the finding is
        // the answer both ways.
        StoreError::Unavailable { io_kind: None, .. } if stopped => {
            match resources.integrity_latch() {
                None => BackupEnd::Stopped,
                Some(latch) => BackupEnd::Failed(
                    StoreError::Corrupt {
                        detail: format!(
                            "the store is read-only after an integrity finding, so it can't \
                             copy a backup generation: {}",
                            latch.detail
                        ),
                    }
                    .to_string(),
                ),
            }
        }
        other => BackupEnd::Failed(other.to_string()),
    }
}

impl Engine {
    /// `start_backup()` (spec/07): take a backup generation now, in the
    /// data root's `backups/`.
    ///
    /// It answers once the generation's snapshot is taken, with the
    /// receipt that committed with its row. The copy goes on in the
    /// engine as counted work: a caller that stops waiting strands
    /// nothing, and shutdown waits for it and stops it at its next
    /// object. A restore never meets one: a qualified stage holds the
    /// backup operation, so no backup starts beside it.
    ///
    /// A repeated key answers its receipt and starts nothing, while
    /// its generation is still being copied too. `Conflict` while
    /// another backup, a generation check or a restore holds the
    /// backup operation; `Corrupt`, naming the finding, on a read-only
    /// store. A generation taken here is one the hourly backup doesn't
    /// take again: it runs while there were changes since the newest
    /// complete one.
    pub async fn start_backup(&self, idempotency_key: &str) -> Result<BackupStarted, EngineError> {
        validate_idempotency_key(idempotency_key)?;
        let key = idempotency_key.to_owned();
        let maintenance = Arc::clone(self.maintenance());
        let copying = self.backup_copies().begin();
        #[cfg(test)]
        let copies = Arc::clone(self.backup_copies());
        let (answer, answered) = oneshot::channel();
        self.spawn_worker(move |worker| async move {
            let copying = copying;
            let started = worker
                .blocking(move |resources| {
                    resources.start_backup(BackupDestination::DataRoot, &key)
                })
                .await;
            let (receipt, snapshot) = match started {
                Ok(Ok(StartedBackup::Started { receipt, snapshot })) => (receipt, snapshot),
                Ok(Ok(StartedBackup::Replayed(receipt))) => {
                    let _ = answer.send(replayed(receipt));
                    return;
                }
                Ok(Err(error)) => {
                    let _ = answer.send(Err(error.into()));
                    return;
                }
                Err(error) => {
                    let _ = answer.send(Err(error));
                    return;
                }
            };
            let generation_id = snapshot.generation_id().to_owned();
            let (end, ended) = oneshot::channel();
            let _ = answer.send(Ok(BackupStarted {
                receipt,
                generation_id: generation_id.clone(),
                copy: Some(BackupCopy { ended }),
            }));
            let copied = worker
                .blocking(move |resources| {
                    #[cfg(test)]
                    {
                        let hook = copies.before_copy.lock().unwrap().clone();
                        if let Some(hook) = hook {
                            hook();
                        }
                    }
                    let stop = maintenance.stop_flag();
                    let result = resources.complete_backup_with(snapshot, stop);
                    copy_end(
                        resources,
                        &generation_id,
                        result,
                        stop.load(Ordering::SeqCst),
                    )
                })
                .await;
            drop(copying);
            let _ = end.send(copied.unwrap_or_else(|error| BackupEnd::Failed(error.to_string())));
        })?;
        answered
            .await
            .map_err(|_| EngineError::internal("a command ended without an answer"))?
    }

    /// `list_backups()` (spec/07): the recorded generations, newest
    /// first. Every complete and in-progress one is listed, and the
    /// failed ones started since the newest complete one, at most
    /// [`sb_resources::FAILED_GENERATIONS_LISTED`]: a failed row stays
    /// as evidence for good, and only the recent ones say anything
    /// about the backups now.
    ///
    /// The answer is the rows as stored. No generation's directory is
    /// opened: whether its bytes are still there is the weekly check's
    /// verdict on the row, and a restore's own verification. A
    /// read-only engine answers like any other.
    pub async fn list_backups(&self) -> Result<Vec<BackupGeneration>, EngineError> {
        self.read_through(|resources| resources.backup_generations())
            .await
    }
}

/// A key that had started its generation: the receipt it stored.
fn replayed(receipt: Receipt) -> Result<BackupStarted, EngineError> {
    let generation_id = receipt_generation_id(&receipt)?;
    Ok(BackupStarted {
        receipt,
        generation_id,
        copy: None,
    })
}

#[cfg(test)]
mod tests;
