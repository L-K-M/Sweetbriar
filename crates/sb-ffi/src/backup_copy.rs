//! Keyed backup start and its Rust-owned copy handle (spec/07; M1.8j3).

use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;

use tokio::sync::Mutex;

use crate::cancel::until_cancelled;
use crate::{CancelToken, Core, CoreError, Receipt};

/// A command's receipt and the copy started now; replay returns no copy.
#[derive(Clone, uniffi::Record)]
pub struct BackupStarted {
    pub receipt: Receipt,
    pub generation_id: String,
    pub copy: Option<Arc<BackupCopy>>,
}

/// The copy's eventual result, independent of the generation's later retention.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum BackupEnd {
    /// Published, even when the following retention has a warning.
    Complete {
        after: Option<String>,
    },
    Stopped,
    Failed {
        detail: String,
    },
}

impl BackupEnd {
    fn from_service(end: sb_service::BackupEnd) -> Self {
        match end {
            sb_service::BackupEnd::Complete { after } => Self::Complete { after },
            sb_service::BackupEnd::Stopped => Self::Stopped,
            sb_service::BackupEnd::Failed(detail) => Self::Failed { detail },
        }
    }
}

type CopyFuture = Pin<Box<dyn Future<Output = sb_service::BackupEnd> + Send>>;

enum CopyState {
    Waiting(CopyFuture),
    Ended(BackupEnd),
}

/// A shared completion answer, not ownership of the engine's running copy.
/// It holds no core lifecycle lock, so shutdown can stop a copy being awaited.
#[derive(uniffi::Object)]
pub struct BackupCopy {
    state: Mutex<CopyState>,
}

impl BackupCopy {
    fn new(copy: sb_service::BackupCopy) -> Arc<Self> {
        Arc::new(Self {
            state: Mutex::new(CopyState::Waiting(Box::pin(copy.finished()))),
        })
    }
}

#[uniffi::export]
impl BackupCopy {
    /// Cancel only this wait, retaining the future for the next waiter.
    /// Completed answers are cached, including retention warnings. Concurrent
    /// callers serialize on the handle, never on the core's lifecycle lock.
    pub async fn finished(&self, token: Arc<CancelToken>) -> Result<BackupEnd, CoreError> {
        let mut state = until_cancelled(&token, self.state.lock()).await?;
        match &mut *state {
            CopyState::Ended(end) => Ok(end.clone()),
            CopyState::Waiting(copy) => {
                // Borrow, do not take: cancellation must not drop the receiver
                // and lose the engine's eventual answer.
                let end = until_cancelled(&token, copy.as_mut()).await?;
                let end = BackupEnd::from_service(end);
                *state = CopyState::Ended(end.clone());
                Ok(end)
            }
        }
    }
}

#[uniffi::export]
impl Core {
    /// Start under a caller-made key; once begun, await the receipt despite
    /// cancellation. Dropping the returned handle never stops the engine copy.
    pub async fn start_backup(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
    ) -> Result<BackupStarted, CoreError> {
        let started = self
            .command(&token, async |core| {
                core.start_backup(&idempotency_key).await
            })
            .await?;
        Ok(BackupStarted {
            receipt: Receipt::from_service(&started.receipt),
            generation_id: started.generation_id,
            copy: started.copy.map(BackupCopy::new),
        })
    }
}

#[cfg(test)]
mod tests;
