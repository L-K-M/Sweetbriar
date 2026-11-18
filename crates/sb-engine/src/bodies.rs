//! In-memory lease authority. Files are removed only on blocking workers.
use std::collections::HashMap;
use std::sync::{Arc, Mutex};

use crate::engine::Detached;
use crate::{Engine, EngineError};
use sb_resources::RenditionFiles;
use sb_resources::proton_bodies::proton_open_state;

/// The reason `publish_body` gives a message with no acquisition
/// (spec/04 "Sync loop" step 3).
const NOT_ACQUIRED: &str = "body is not acquired";

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

/// In-memory lease revocation, independent of the core's async lifecycle lock.
pub struct LeaseReleaser {
    registry: Arc<Leases>,
    cleanup: Detached,
}

impl LeaseReleaser {
    pub fn release_lease(&self, lease_id: &str) {
        self.registry.revoke(lease_id);
        let registry = self.registry.clone();
        self.cleanup.spawn(move || registry.clean());
    }
}

impl Engine {
    pub fn lease_releaser(&self) -> LeaseReleaser {
        LeaseReleaser {
            registry: self.leases().clone(),
            cleanup: self.detached(),
        }
    }

    pub async fn open_body(
        &self,
        message_id: i64,
        preferred: BodyKind,
    ) -> Result<BodyLease, EngineError> {
        let registry = self.leases().clone();
        let cleanup = self.detached();
        let published = self
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
            .await;
        let mut pending = match published {
            Ok(pending) => pending,
            // An unacquired Proton message the user opened joins the
            // queue and wakes the worker; one whose body couldn't be
            // decrypted is distinguished from one not fetched yet.
            Err(EngineError::Unavailable { reason }) if reason == NOT_ACQUIRED => {
                return self.proton_open(message_id).await;
            }
            Err(error) => return Err(error),
        };
        // No await between transferring authority and returning to the caller.
        Ok(pending.lease.take().expect("pending body owns its lease"))
    }

    /// Note an opened Proton message and answer its open (spec/04
    /// "Acquire bodies"). A current body failure is the readable
    /// "couldn't decrypt" condition; otherwise the message is queued
    /// and the not-acquired answer is unchanged.
    async fn proton_open(&self, message_id: i64) -> Result<BodyLease, EngineError> {
        let state = self
            .read_through(move |resources| resources.read(|tx| proton_open_state(tx, message_id)))
            .await?;
        let Some(state) = state else {
            return Err(EngineError::unavailable(NOT_ACQUIRED));
        };
        if state.current_failure {
            return Err(EngineError::BodyUndecryptable);
        }
        if let Some(provider_key) = state.provider_key {
            self.proton_opened().note(&state.account_id, provider_key);
            self.account_jobs().wake();
        }
        Err(EngineError::unavailable(NOT_ACQUIRED))
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
        self.lease_releaser().release_lease(lease_id);
    }

    /// Hold the lease table for `body` (the `test-seams` feature): an
    /// open's publication ends on the registry insert, which waits
    /// while the table is held, so a downstream test's first
    /// `open_body`/`open_attachment` poll stays `Pending` however fast
    /// the blocking pool runs.
    #[cfg(feature = "test-seams")]
    pub fn with_lease_table_held<R>(&self, body: impl FnOnce() -> R) -> R {
        // Held for the closure: the insert a publication ends on waits.
        let _table = self.leases().state.lock().expect("lease lock poisoned");
        body()
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod attachment_tests;
