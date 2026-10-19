//! In-memory lease authority. Files are removed only on blocking workers.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::engine::Detached;
use crate::{Engine, EngineError};
use sb_resources::{PublishedBody, RenditionFiles};

pub use sb_resources::{BodyKind, BodyLease, RenderWarning, RenderWarningCode};

#[derive(Default)]
pub(crate) struct Leases {
    state: Mutex<LeaseState>,
}

#[derive(Default)]
struct LeaseState {
    live: HashMap<String, RenditionFiles>,
    retired: Vec<RenditionFiles>,
}

impl Leases {
    fn insert(&self, body: PublishedBody) -> BodyLease {
        self.state
            .lock()
            .expect("lease lock poisoned")
            .live
            .insert(body.lease.lease_id.clone(), body.files);
        body.lease
    }

    fn revoke(&self, id: &str) {
        let mut state = self.state.lock().expect("lease lock poisoned");
        if let Some(files) = state.live.remove(id) {
            state.retired.push(files);
        }
    }

    fn clean(&self) {
        let retired = std::mem::take(&mut self.state.lock().expect("lease lock poisoned").retired);
        drop(retired);
    }

    /// Called after blocking work has ended, on the close worker.
    pub(crate) fn close(&self) {
        let mut state = self.state.lock().expect("lease lock poisoned");
        let live = std::mem::take(&mut state.live);
        let retired = std::mem::take(&mut state.retired);
        drop(state);
        drop(live);
        drop(retired);
    }
}

/// Even an unpolled blocking-task result owns a cancellation guard. Its
/// drop revokes synchronously, while file destruction stays off the caller.
struct PendingBody {
    lease: Option<BodyLease>,
    registry: Arc<Leases>,
    cleanup: Detached,
}

impl Drop for PendingBody {
    fn drop(&mut self) {
        if let Some(lease) = &self.lease {
            self.registry.revoke(&lease.lease_id);
            let registry = self.registry.clone();
            self.cleanup.spawn(move || registry.clean());
        }
    }
}

impl Engine {
    pub async fn open_body(
        &self,
        message_id: i64,
        preferred: BodyKind,
    ) -> Result<BodyLease, EngineError> {
        let registry = self.leases().clone();
        let cleanup = self.detached();
        let mut pending = self
            .read_through(move |resources| {
                let body = resources.publish_body(message_id, preferred)?;
                let lease = registry.insert(body);
                Ok(PendingBody {
                    lease: Some(lease),
                    registry,
                    cleanup,
                })
            })
            .await?;
        // No await between transferring authority and returning to the caller.
        Ok(pending.lease.take().expect("pending body owns its lease"))
    }

    pub fn release_lease(&self, lease_id: &str) {
        self.leases().revoke(lease_id);
        let registry = self.leases().clone();
        self.detached().spawn(move || registry.clean());
    }
}

#[cfg(test)]
mod tests;
