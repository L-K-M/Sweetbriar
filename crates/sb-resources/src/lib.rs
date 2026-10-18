//! `sb-resources` — L4. One narrow, typed façade over the L5 leaf
//! crates; maps leaf types and errors into engine-facing shapes.
//! Leaf types reach L3 only through item re-exports here (spec/01
//! rules 1 and 3).
//!
//! For the M0.7 pilot this is the store's front door: it opens a store
//! with the real seams (`RealFs`, `SystemClock`, `OsRandom`) built from
//! `sb-platform` (spec/01 rule 5 — no leaf ever constructs them), and
//! exposes exactly what the engine needs: writes, reads, close, the
//! settings helpers, receipts, a new idempotency-key UUID, import
//! source inventory and the platform's default roots.

use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sb_platform::{OsRandom, RealFs, SystemClock};
use sb_store::StorePaths;

mod acquire;
mod import;
mod import_jobs;

// The engine's names, re-exported by item so the engine states bounds
// and maps errors without depending on a leaf (spec/01 rule 3). The
// default roots climb the same way, for the service's platform
// defaults.
pub use acquire::{AcquiredMessage, FlagExtraction, SourceAcquisition, SourcePosition};
pub use import::{
    ImportInventory, ImportInventoryOutcome, ImportProblem, ImportProblemKind, ImportProgress,
    SourceInventory, SourceKind,
};
pub use import_jobs::{
    ImportJobBatch, ImportJobCheckpoint, ImportJobRecord, ImportTotals, LISTED_PROBLEMS,
    advance_import_job, commit_import_job_batch, create_import_job, finished_import_jobs,
    import_job, import_job_source, import_job_sources, import_report, record_import_sources,
    recorded_import_job_sources, recoverable_import_jobs,
};
pub use sb_mime::{MboxQuoting, MboxVariant};
pub use sb_platform::{default_cache_root, default_data_root};
pub use sb_store::imports::{
    ImportJobState, ImportMessage, MAX_IMPORT_BATCH, MAX_IMPORT_CHANGES_BYTES,
};
pub use sb_store::settings::{StoredSetting, read_all, upsert};
pub use sb_store::{
    ImportSourceSnapshot, Receipt, StoreError, Timestamp, Transaction, Uuid, WriteFuture,
    WritePriority, run_once,
};

#[cfg(feature = "test-seams")]
pub use sb_store::Fs;
#[cfg(not(feature = "test-seams"))]
use sb_store::Fs;
use sb_store::{Clock, Random, Seams, Store, open};

/// The store and the seams it opened with, held for the engine: one
/// process's whole reach into the leaves (spec/01).
pub struct Resources {
    store: Store,
    fs: Arc<dyn Fs>,
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
        Self::open_on(data_root, Arc::new(RealFs))
    }

    /// [`Resources::open`] on an injected filesystem: crash-cut suites
    /// kill the process at a chosen operation (spec/11).
    #[cfg(feature = "test-seams")]
    pub fn open_with_fs(data_root: &Path, fs: Arc<dyn Fs>) -> Result<Self, StoreError> {
        Self::open_on(data_root, fs)
    }

    fn open_on(data_root: &Path, fs: Arc<dyn Fs>) -> Result<Self, StoreError> {
        let paths = StorePaths::new(data_root)?;
        let clock: Arc<dyn Clock> = Arc::new(SystemClock);
        let random: Arc<dyn Random> = Arc::new(OsRandom);
        let seams = Seams::new(Arc::clone(&fs), Arc::clone(&clock), Arc::clone(&random));
        Ok(Self {
            store: open(&paths, &seams)?,
            fs,
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

    /// Discover sources in request order and count durable private copies
    /// (spec/09). Blocking file I/O: run on a blocking thread. `Break`
    /// requests an explicit `Cancelled` outcome, preserving earlier results.
    pub fn inventory_import_sources(
        &self,
        roots: &[PathBuf],
        progress: impl FnMut(ImportProgress<'_>) -> ControlFlow<()>,
    ) -> ImportInventoryOutcome {
        import::inventory(&self.store, self.fs.as_ref(), roots, progress)
    }

    /// Verify and reopen `source`'s snapshot to acquire messages from
    /// `position` (spec/09). Only an `Ambiguous` source needs `variant`.
    /// Blocking: the whole snapshot is hashed again first (spec/03).
    pub fn open_import_acquisition(
        &self,
        source: &SourceInventory,
        variant: Option<MboxVariant>,
        position: SourcePosition,
    ) -> Result<SourceAcquisition, StoreError> {
        acquire::open(&self.store, source, variant, position)
    }

    /// Publish the next message's exact bytes and summarize them under
    /// the spec/10 limits, or `None` once the source's count is consumed.
    /// Blocking. Nothing is committed: the job batches the results.
    pub fn acquire_import_message(
        &self,
        acquisition: &mut SourceAcquisition,
    ) -> Result<Option<AcquiredMessage>, StoreError> {
        acquire::next(&self.store, acquisition, &sb_mime::Limits::default())
    }

    /// Terminal-job cleanup (spec/03): remove these snapshots and flush
    /// `recovery/`; missing ones count as removed. Blocking.
    pub fn remove_import_sources(
        &self,
        snapshots: &[ImportSourceSnapshot],
    ) -> Result<(), StoreError> {
        self.store.remove_import_sources(snapshots)
    }

    /// Durably replace `job`'s report in `recovery/` (spec/03, 09).
    /// Blocking.
    pub fn write_import_report(&self, job: &str, report: &str) -> Result<(), StoreError> {
        self.store.write_import_report(job, report.as_bytes())
    }

    /// Whether `job`'s report exists, marking its cleanup done. Blocking.
    pub fn import_report_exists(&self, job: &str) -> Result<bool, StoreError> {
        self.store.import_report_exists(job)
    }

    /// Where `job`'s report lives, for the user to open.
    pub fn import_report_path(&self, job: &str) -> Result<PathBuf, StoreError> {
        self.store.import_report_path(job)
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
