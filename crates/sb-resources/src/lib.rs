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

mod bodies;
pub use bodies::{AttachmentLease, BodyKind, BodyLease, PublishedAttachment, PublishedBody};
pub use sb_mime::{RenderWarning, RenderWarningCode};
mod acquire;
mod activation;
mod catchup;
pub use activation::{
    ActivationCommit, ActivationPreparation, ActivationPrepared, ActivationStatus, ActivationWork,
    RecordedActivationDeferral,
};
pub use catchup::{
    CatchupBatch, CatchupCommit, CatchupPreparation, CatchupReport, CatchupStatus, CatchupSweep,
};
mod import;
mod import_jobs;
mod index;
pub use index::{
    IndexBlocked, IndexIoContext, IndexIoUpdate, IndexPreparation, IndexReadFailure,
    IndexReadStage, PreparedIndexBatch, index_retry_at, recover_index_io_failure,
    report_index_io_failure, search_io_code, search_io_message,
};
mod renditions;
mod repair;
mod search;
mod seen;
pub use renditions::{RenditionCache, RenditionError, RenditionFiles, RenditionProtection};
pub use repair::{
    RepairCapture, RepairCommit, RepairDiagnosis, RepairPersist, RepairPreparation, RepairPrepared,
    RepairSources, RepairStatus, RepairTerminal, RepairTerminalKind,
};
pub use search::{CoverageCounts, read_search_coverage, read_search_coverage_reusing};
pub use seen::{ReceivedCommit, ReceivedPreparation, ReceivedRevisions, ReceivedSweep};

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
pub use sb_mime::SEARCH_EXTRACTOR_VERSION;
pub use sb_mime::{MboxQuoting, MboxVariant};
pub use sb_platform::{default_cache_root, default_data_root};
pub use sb_store::ACCOUNT_PALETTE_V1;
pub use sb_store::appearance::{
    AccountColor, AppearanceAssignment, StoredAccountAppearance, StoredIdentityAppearance,
    account_appearance, appearance_generation, identity_appearance, initialize_account_appearance,
    set_account_appearance, set_identity_appearance,
};
pub use sb_store::exports::{
    ExportJob, ExportJobState, MAX_EXPORT_CHANGED_BYTES, MAX_EXPORT_CHUNK_BYTES, MAX_EXPORT_CHUNKS,
    MAX_EXPORT_JSON_BYTES, advance_export_job, append_export_chunks, create_export_job,
    export_chunks, export_job,
};
pub use sb_store::imports::{
    ImportJobState, ImportMessage, ImportParseState, ImportSummary, MAX_IMPORT_BATCH,
    MAX_IMPORT_CHANGES_BYTES,
};
pub use sb_store::index::rebuild::{
    JobKind, RebuildJob, RebuildPhase, SearchIndexStatus, SearchIoCode, UserPause,
    begin_index_retry, commit_index_recovery, complete_rebuild, job_kind, latest_rebuild,
    pause_for_io_if_current, rebuild_enqueue_batch, rebuild_job, rebuild_reset_batch,
    reconcile_search_target, record_index_attempt_success, request_index_retry,
    search_index_status, set_user_pause, start_search_rebuild,
};
pub use sb_store::index::{
    IndexBatch, IndexDocument, IndexFence, IndexPublication, IndexReason, IndexState,
    MAX_INDEX_BATCH, MAX_INDEX_BATCH_BYTES, ObservedAvailability, PendingDocument,
    note_observed_availability, publish_batch, search_generation, target_version,
};
pub use sb_store::lists::{
    ListAnchor, ListKey, ListPage, ListScope, MAX_PAGE_ROWS, MailboxRole, PageRead, StoredListRow,
    list_generation, list_ids, read_list_page,
};
pub use sb_store::messages::{
    BodyAvailability, MAX_READER_HEADER_BYTES, MAX_READER_MESSAGE_BYTES, ReaderAddress,
    ReaderHeaders, ReaderMessage, ReaderOccurrence, ReaderTag, StoredAcquisition, best_acquisition,
    get_message, message_for_occurrence,
};
pub use sb_store::query::{
    Kept, MAX_QUERY_INPUT_BYTES, ParseCode, ParseContext, Query, QueryCounts, QueryError,
    SearchAnchor, SearchIndexState, SearchLimitation, SearchPage, SearchRebuildProgress,
    SearchScope, StoredSearchCoverage as Coverage, TextFormatError, UnknownCount, counts,
    read_search_date_page, read_search_relevance_page, search_date_ids, search_relevance_ids,
};
pub use sb_store::settings::{StoredSetting, read_all, upsert};
pub use sb_store::sidebar::{
    AccountKind, AccountState, MailboxKind, MailboxList, StoredAccount, StoredMailbox, accounts,
    mailboxes,
};
pub use sb_store::threads::{
    StoredThread, ThreadDescription, ThreadRevisions, ThreadSource, assign_thread, create_thread,
    describe_thread, mark_threads_built, note_membership_change, remove_empty_threads,
    remove_thread, stored_threads, thread_memberships, thread_revisions, thread_sources,
};
pub use sb_store::{
    AppearanceObserver, ImportSourceSnapshot, ListObserver, ReadStop, Receipt, Scoped,
    SearchObserver, StoreError, Timestamp, Transaction, Uuid, WriteFuture, WritePriority,
    encode_job_receipt, receipt_job_id, run_once,
};

#[cfg(feature = "test-seams")]
pub use sb_store::index::rebuild::pause_for_io;
#[cfg(not(feature = "test-seams"))]
use sb_store::{Clock, Fs};
#[cfg(feature = "test-seams")]
pub use sb_store::{Clock, Fs, refresh_list_rows};
use sb_store::{Random, Seams, Store, open};

/// The store and the seams it opened with, held for the engine: one
/// process's whole reach into the leaves (spec/01).
pub struct Resources {
    store: Store,
    renditions: Option<RenditionCache>,
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
        Self::open_on(data_root, Arc::new(RealFs), Arc::new(SystemClock))
    }

    /// [`Resources::open`] on an injected filesystem: crash-cut suites
    /// kill the process at a chosen operation (spec/11).
    #[cfg(feature = "test-seams")]
    pub fn open_with_fs(data_root: &Path, fs: Arc<dyn Fs>) -> Result<Self, StoreError> {
        Self::open_on(data_root, fs, Arc::new(SystemClock))
    }

    /// [`Resources::open`] on injected filesystem and clock seams: the
    /// same clock reaches the store's seams and [`Resources::now`], so
    /// writer tests observe deterministic timestamps (spec/11).
    #[cfg(feature = "test-seams")]
    pub fn open_with_clock(
        data_root: &Path,
        fs: Arc<dyn Fs>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, StoreError> {
        Self::open_on(data_root, fs, clock)
    }

    fn open_on(
        data_root: &Path,
        fs: Arc<dyn Fs>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, StoreError> {
        let paths = StorePaths::new(data_root)?;
        let random: Arc<dyn Random> = Arc::new(OsRandom);
        let seams = Seams::new(Arc::clone(&fs), Arc::clone(&clock), Arc::clone(&random));
        Ok(Self {
            store: open(&paths, &seams)?,
            renditions: None,
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

    /// Hear of each list generation once its transaction commits
    /// (spec/07 "Subscriptions"); the observer runs on the writer
    /// thread and must return at once.
    pub fn observe_list_generation(&self, observer: ListObserver) {
        self.store.observe_list_generation(observer);
    }

    /// Hear Search-only or shared source commits, independently of list changes.
    /// Runs on the writer thread; return immediately and never panic.
    pub fn observe_search_generation(&self, observer: SearchObserver) {
        self.store.observe_search_generation(observer);
    }

    /// Observe committed appearance revisions, including dropped callers.
    /// Runs on the writer thread; return immediately and never panic.
    pub fn observe_appearance_generation(&self, observer: AppearanceObserver) {
        self.store.observe_appearance_generation(observer);
    }

    /// Wake maintenance after source commits, independently of list and Search.
    /// Runs on the writer thread; return immediately and never panic.
    pub fn observe_maintenance_inputs(&self, observer: Arc<dyn Fn() + Send + Sync>) {
        self.store.observe_maintenance_inputs(observer);
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

    /// Run a read-scoped interruptible transaction (spec/03).
    /// Blocking; use a blocking thread. Other readers/writers are unaffected.
    pub fn read_scoped<T>(
        &self,
        stop: &ReadStop,
        f: impl FnOnce(&Transaction) -> Result<T, StoreError>,
    ) -> Result<Scoped<T>, StoreError> {
        self.store.read_scoped(stop, f)
    }

    /// Read exact acquisition bytes after EOF verifies integrity, under the
    /// 100 MB limit. Blocking I/O; call after the metadata transaction ends.
    pub fn read_acquisition(&self, acquisition: &StoredAcquisition) -> Result<Vec<u8>, StoreError> {
        self.store.read_acquisition(acquisition)
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

    /// `job`'s report as last written, or `None` while there is none;
    /// one longer than the report writer can write for `sources` sources
    /// is `Corrupt`. Blocking.
    pub fn read_import_report(
        &self,
        job: &str,
        sources: usize,
    ) -> Result<Option<String>, StoreError> {
        self.store
            .read_import_report(job, import_jobs::import_report_budget(sources))
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
