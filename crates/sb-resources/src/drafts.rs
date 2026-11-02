//! The draft attachment's publication path (spec/02, spec/03
//! "Publication protocol"): bytes become a durable object plus its
//! one-shot reservation before the revision that references them is
//! written. The draft row work itself is `sb_store::drafts`, used
//! through [`Resources::write`]/[`Resources::read`].

use std::io::Read;

use sb_store::{ObjectStore, PublishedObject, StoreError};

use crate::Resources;

impl Resources {
    /// Publish one draft attachment's bytes: stream, hash, flush,
    /// reserve, install, flush (spec/03's order). The returned
    /// [`PublishedObject`] carries the reservation the revision's
    /// commit spends; an abandoned one pins the object until startup
    /// expiry. Blocking file I/O; run on a blocking thread, never the
    /// writer's.
    pub fn publish_draft_attachment(
        &self,
        bytes: impl Read,
    ) -> Result<PublishedObject, StoreError> {
        self.store.objects().publish(bytes)
    }
}
