//! `sb-resources` — L4. One narrow, typed façade over the L5 leaf
//! crates; maps leaf types and errors into engine-facing shapes.
//! Leaf types reach L3 only through item re-exports here (spec/01
//! rules 1 and 3).
//!
//! For the M0.7 pilot this is the store's front door: it opens a store
//! with the real seams (`RealFs`, `SystemClock`, `OsRandom`) built from
//! `sb-platform` (spec/01 rule 5 — no leaf ever constructs them), and
//! exposes exactly what the engine needs: writes, reads, close, the
//! settings helpers, receipts, a new idempotency-key UUID and the
//! platform's default roots.

use std::path::Path;
use std::sync::Arc;

use sb_platform::{OsRandom, RealFs, SystemClock};
use sb_store::StorePaths;

// The engine's names, re-exported by item so the engine states bounds
// and maps errors without depending on a leaf (spec/01 rule 3). The
// default roots climb the same way, for the service's platform
// defaults.
pub use sb_platform::{default_cache_root, default_data_root};
pub use sb_store::settings::{StoredSetting, read_all, upsert};
pub use sb_store::{
    Receipt, StoreError, Timestamp, Transaction, Uuid, WriteFuture, WritePriority, run_once,
};

use sb_store::{Clock, Fs, Random, Seams, Store, open};

/// The store and the seams it opened with, held for the engine: one
/// process's whole reach into the leaves (spec/01).
pub struct Resources {
    store: Store,
    clock: Arc<dyn Clock>,
    random: Arc<dyn Random>,
}

impl Resources {
    /// Open or create the store at `data_root` with the real seams
    /// (spec/01 rule 5, spec/03): the location check, the lease, the
    /// layout, the migrations — everything `sb_store::open` does, with
    /// `sb-platform`'s filesystem, clock and randomness behind the
    /// seam traits.
    pub fn open(data_root: &Path) -> Result<Self, StoreError> {
        let paths = StorePaths::new(data_root)?;
        let fs: Arc<dyn Fs> = Arc::new(RealFs);
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let random: Arc<dyn Random> = Arc::new(OsRandom);
        let seams = Seams::new(Arc::clone(&fs), Arc::clone(&clock), Arc::clone(&random));
        Ok(Self {
            store: open(&paths, &seams)?,
            clock,
            random,
        })
    }

    /// Submit one write to the store's writer (spec/03 "Write
    /// discipline"): see [`Store::write`]. The returned future resolves
    /// when the writer's transaction commits, under any executor.
    pub fn write<T, F>(&self, priority: WritePriority, closure: F) -> WriteFuture<T>
    where
        T: Send + 'static,
        F: FnOnce(&Transaction) -> Result<T, StoreError> + Send + 'static,
    {
        self.store.write(priority, closure)
    }

    /// Run one read through the store's reader pool (spec/03): a short
    /// transaction on a read-only connection, waiting for a free
    /// reader when all are busy. Blocking, so callers off the hot path
    /// run it on a blocking thread.
    pub fn read<T>(
        &self,
        f: impl FnOnce(&Transaction) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.store.read(f)
    }

    /// Close the store: the clean-shutdown marker, the writer, the
    /// lease (spec/03). The caller holds the only handle: the engine
    /// waits for its blocking tasks before closing.
    pub fn close(self) -> Result<(), StoreError> {
        self.store.close()
    }

    /// The clock's "now", for command receipts and settings rows: the
    /// seams stay behind this layer (spec/01 rule 5).
    pub fn now(&self) -> Timestamp {
        self.clock.now()
    }

    /// A fresh UUID string (lowercase, hyphenated) for a command's
    /// idempotency key (spec/07 "Commands"), from OS randomness.
    /// Failing OS randomness cannot produce a key; the seam has no
    /// sane fallback, so it stops the process.
    pub fn new_uuid_string(&self) -> String {
        let mut bytes = [0u8; 16];
        self.random
            .fill(&mut bytes)
            .expect("the OS randomness source failed; cannot make an idempotency key");
        Uuid::from_random_bytes(bytes).to_string()
    }
}

impl std::fmt::Debug for Resources {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Resources")
    }
}
