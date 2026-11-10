//! The integrity check and the latch it sets (spec/03 "Integrity"):
//! the engine runs the check as a maintenance job (M1.8h5c) and turns
//! read-only on its latch (M1.8h5b2). Nothing here schedules (spec/01).

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use sb_store::{IntegrityCheck, IntegrityLatch, StoreError};

use crate::Resources;

impl Resources {
    /// One full integrity check, stopping between batches of steps once
    /// `stop` is raised; a finding latches the store read-only and
    /// copies its database files to `recovery/`. Blocking, and minutes
    /// on a large archive; `Corrupt` on a store already latched.
    pub fn check_integrity(&self, stop: &AtomicBool) -> Result<IntegrityCheck, StoreError> {
        self.store.check_integrity(stop)
    }

    /// The finding the store is read-only after, if any.
    pub fn integrity_latch(&self) -> Option<IntegrityLatch> {
        self.store.integrity_latch()
    }

    /// Install the one observer of the store latching. It is told
    /// once: at once, on this thread, when a finding is already
    /// recorded (a store that opened latched included), else on the
    /// check's thread as the store latches, before the latch file and
    /// the copy are written. Signal only: never block, join or write.
    pub fn observe_integrity_latch(&self, observer: Arc<dyn Fn() + Send + Sync>) {
        self.store.observe_integrity_latch(observer);
    }
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::AtomicBool;

    use sb_store::{IntegrityCheck, StoreError};
    use sb_test_support::ScratchDir;

    use crate::Resources;

    /// A clean store checks clean and stays unlatched; a data root
    /// holding a latch opens latched, and a draft attachment, which
    /// writes an object before any store write, is refused before it
    /// touches `objects/`.
    #[test]
    fn the_latch_reaches_the_resources_and_refuses_attachments() {
        let scratch = ScratchDir::new("resources-latch");
        let root = scratch.path().join("store");
        let res = Resources::open(&root).unwrap();
        assert!(matches!(
            res.check_integrity(&AtomicBool::new(false)),
            Ok(IntegrityCheck::Clean)
        ));
        assert_eq!(res.integrity_latch(), None);
        res.close().unwrap();

        std::fs::write(root.join("integrity-latch.json"), b"not json").unwrap();
        let res = Resources::open(&root).unwrap();
        let latch = res.integrity_latch().unwrap();
        assert!(latch.detail.contains("can't be read"), "{latch:?}");
        match res.publish_draft_attachment(b"attachment".as_slice()) {
            Err(StoreError::Corrupt { detail }) => assert!(detail.contains("can't be read")),
            other => panic!("expected Corrupt, got {other:?}"),
        }
        // Neither a temporary file nor a chain directory was made.
        for dir in ["objects/tmp", "objects/sha256"] {
            let leftovers: Vec<_> = std::fs::read_dir(root.join(dir)).unwrap().collect();
            assert!(leftovers.is_empty(), "{dir}: {leftovers:?}");
        }
        res.close().unwrap();
    }
}
