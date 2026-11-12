//! One native restore with explicit ownership/cancellation (spec/07; M1.8j4).

use std::sync::Arc;

use crate::api::Receipt;
use crate::cancel::until_cancelled;
use crate::{CancelToken, Core, CoreError};

/// A receipt error does not undo a swap. Only Closed leaves no usable core.
#[derive(Debug, Clone, uniffi::Enum)]
pub enum RestoreOutcome {
    Restored { receipt: Receipt },
    Replayed { receipt: Receipt },
    ReceiptUnrecorded { cause: CoreError },
    Refused { cause: CoreError },
    Closed { cause: CoreError },
}

#[uniffi::export]
impl Core {
    /// Replay before preparation; otherwise verify/stage with the core usable,
    /// then exclusively consume and reinstall it. Cancel by flag, not by
    /// dropping either begun operation. Retained stages are never reused.
    pub async fn start_restore(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        generation_id: String,
    ) -> Result<RestoreOutcome, CoreError> {
        let flag = token.flag();
        let prepared = {
            let guard = until_cancelled(&token, self.service.read()).await?;
            let core = guard
                .as_ref()
                .ok_or_else(CoreError::unavailable_shut_down)?;
            if let Some(receipt) = core.restore_receipt(&idempotency_key).await? {
                return Ok(RestoreOutcome::Replayed {
                    receipt: Receipt::from_service(&receipt),
                });
            }
            let work = core.prepare_restore(&generation_id, Arc::clone(&flag));
            #[cfg(test)]
            let work = self.restore_probe.preparing(work);
            let prepared = work.await?;
            #[cfg(test)]
            self.restore_probe.wait(probe::Phase::Prepared).await;
            prepared
        };

        // The completed preparation can be dropped here: it releases the
        // operation and keeps its evidence. No working future is abandoned.
        let mut slot = until_cancelled(&token, self.service.write()).await?;
        let live = slot.as_ref().ok_or_else(CoreError::unavailable_shut_down)?;
        if let Some(receipt) = live.restore_receipt(&idempotency_key).await? {
            return Ok(RestoreOutcome::Replayed {
                receipt: Receipt::from_service(&receipt),
            });
        }
        let core = slot
            .take()
            .expect("the exclusive guard still holds the core");
        #[cfg(test)]
        self.restore_probe.wait(probe::Phase::Taken).await;
        let result = core.restore(&idempotency_key, prepared, flag).await;
        Ok(install_result(&mut slot, result))
    }
}

/// The consuming call always reaches this transition, even after cancellation.
/// In particular a receipt failure MUST put the restored core back first.
fn install_result(
    slot: &mut Option<sb_service::Core>,
    result: Result<sb_service::Restored, sb_service::RestoreFailure>,
) -> RestoreOutcome {
    assert!(
        slot.is_none(),
        "a consuming restore must take the old core first"
    );
    match result {
        Ok(restored) => {
            *slot = Some(restored.core);
            match restored.receipt {
                Ok(receipt) => RestoreOutcome::Restored {
                    receipt: Receipt::from_service(&receipt),
                },
                Err(cause) => RestoreOutcome::ReceiptUnrecorded {
                    cause: cause.into(),
                },
            }
        }
        Err(sb_service::RestoreFailure::Refused {
            cause,
            core,
            prepared,
        }) => {
            *slot = Some(*core);
            drop(prepared);
            RestoreOutcome::Refused {
                cause: cause.into(),
            }
        }
        Err(sb_service::RestoreFailure::Failed(cause)) => RestoreOutcome::Closed {
            cause: cause.into(),
        },
    }
}

#[cfg(test)]
pub(crate) mod probe;
#[cfg(test)]
mod tests;
