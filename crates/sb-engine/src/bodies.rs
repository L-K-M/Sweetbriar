//! In-memory lease authority. Files are removed only on blocking workers.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::engine::Detached;
use crate::{Engine, EngineError};
use sb_resources::RenditionFiles;

pub use sb_resources::{AttachmentLease, BodyKind, BodyLease, RenderWarning, RenderWarningCode};

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
    fn insert(&self, lease_id: &str, files: RenditionFiles) {
        self.state
            .lock()
            .expect("lease lock poisoned")
            .live
            .insert(lease_id.to_owned(), files);
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
struct PendingLease<T> {
    lease: Option<T>,
    lease_id: String,
    registry: Arc<Leases>,
    cleanup: Detached,
}

impl<T> Drop for PendingLease<T> {
    fn drop(&mut self) {
        if self.lease.is_some() {
            self.registry.revoke(&self.lease_id);
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
                registry.insert(&body.lease.lease_id, body.files);
                Ok(PendingLease {
                    lease_id: body.lease.lease_id.clone(),
                    lease: Some(body.lease),
                    registry,
                    cleanup,
                })
            })
            .await?;
        // No await between transferring authority and returning to the caller.
        Ok(pending.lease.take().expect("pending body owns its lease"))
    }

    pub async fn open_attachment(
        &self,
        message_id: i64,
        part_id: String,
    ) -> Result<AttachmentLease, EngineError> {
        // IDs are canonical positive decimal tree components rooted at 1.
        if part_id.split('.').next() != Some("1")
            || part_id.split('.').any(|component| {
                component.is_empty()
                    || component.starts_with('0')
                    || !component.bytes().all(|byte| byte.is_ascii_digit())
            })
        {
            return Err(EngineError::invalid_input(
                "part_id",
                "invalid MIME part id",
            ));
        }
        let registry = self.leases().clone();
        let cleanup = self.detached();
        let mut pending = self
            .read_through(move |resources| {
                let attachment = resources.publish_attachment(message_id, part_id)?;
                registry.insert(&attachment.lease.lease_id, attachment.files);
                Ok(PendingLease {
                    lease_id: attachment.lease.lease_id.clone(),
                    lease: Some(attachment.lease),
                    registry,
                    cleanup,
                })
            })
            .await?;
        Ok(pending
            .lease
            .take()
            .expect("pending attachment owns its lease"))
    }

    pub fn release_lease(&self, lease_id: &str) {
        self.leases().revoke(lease_id);
        let registry = self.leases().clone();
        self.detached().spawn(move || registry.clean());
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod attachment_tests;
