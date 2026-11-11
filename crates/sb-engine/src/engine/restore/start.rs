//! `start_restore` (spec/07 "Commands", Backup; plan M1.8i5): the swap
//! as a command under an idempotency key.
//!
//! ```text
//!   the key      spent on a restore: its receipt, and nothing moves
//!   the swap     Engine::restore: quiesce, swap, resume
//!   the receipt  written in the restored store
//! ```
//!
//! The receipt can't commit with the swap. A stage's bytes are fixed
//! when it qualifies, and the swap itself is two renames. So it is
//! written once the restored store is open. A process that dies in
//! between leaves a restored store without the receipt, so a retry
//! cannot rely on replay; a restore deletes nothing (spec/03).

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use super::{PreparedRestore, RestoreError};
use crate::engine::validate_idempotency_key;
use crate::{Engine, EngineError, Receipt};

/// What [`Engine::start_restore`] answers when nothing refused or
/// failed.
pub struct Restored {
    /// The engine on the restored store, or as it was when the key had
    /// already been spent on a restore.
    pub engine: Engine,
    /// The command's stored receipt, `{"generation_id":...}`. An error
    /// means the store was restored and its receipt could not be
    /// recorded. A retry may restore again; a key the restored store
    /// already holds for another command is refused instead.
    pub receipt: Result<Receipt, EngineError>,
}

impl Engine {
    /// `start_restore` (spec/07): swap `prepared` in as this engine's
    /// store ([`Engine::restore`]) as a command under
    /// `idempotency_key`.
    ///
    /// A key this store already holds a restore's receipt for answers
    /// that receipt, and nothing moves: the engine comes back as it
    /// is, and `prepared` is dropped unused. A key spent on another
    /// command, an unusable key and a store that can't say are
    /// refusals, before anything stops. So is every refusal of the
    /// swap. A refusal hands back the engine, running as before, and
    /// `prepared`, and spends no key.
    ///
    /// After the swap the receipt is written in the restored store.
    /// If that fails the restore still stands, and the answer says so.
    /// Await it to the end, like [`Engine::restore`].
    pub async fn start_restore(
        self,
        idempotency_key: &str,
        prepared: PreparedRestore,
        cancelled: Arc<AtomicBool>,
    ) -> Result<Restored, RestoreError<PreparedRestore>> {
        let spent = self.restore_receipt(idempotency_key).await;
        match spent {
            Ok(None) => {}
            Ok(Some(receipt)) => {
                return Ok(Restored {
                    engine: self,
                    receipt: Ok(receipt),
                });
            }
            Err(cause) => {
                return Err(RestoreError::Refused {
                    cause,
                    engine: Box::new(self),
                    stage: Box::new(prepared),
                });
            }
        }

        let PreparedRestore {
            generation_id,
            stage,
        } = prepared;
        let engine = match self.restore(stage, cancelled).await {
            Ok(engine) => engine,
            Err(RestoreError::Refused {
                cause,
                engine,
                stage,
            }) => {
                let prepared = PreparedRestore {
                    generation_id,
                    stage: *stage,
                };
                return Err(RestoreError::Refused {
                    cause,
                    engine,
                    stage: Box::new(prepared),
                });
            }
            Err(RestoreError::Failed(cause)) => return Err(RestoreError::Failed(cause)),
        };

        let key = idempotency_key.to_owned();
        let receipt = engine
            .command(move |worker| async move {
                let recorded = worker
                    .blocking(move |resources| resources.record_restore(&key, &generation_id))
                    .await??;
                Ok(recorded)
            })
            .await;
        Ok(Restored { engine, receipt })
    }

    /// What a valid `key` already answers for a restore in this store.
    /// A key spent on another command is `Conflict`. Read-only stores
    /// answer too; the native facade calls this before preparing.
    pub async fn restore_receipt(&self, key: &str) -> Result<Option<Receipt>, EngineError> {
        validate_idempotency_key(key)?;
        let key = key.to_owned();
        self.read_through(move |resources| resources.restore_receipt(&key))
            .await
    }
}
