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

use std::io::Read as _;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sb_platform::{OsRandom, PlatformSecrets, RealFs, SystemClock};
pub use sb_store::ObjectId;
use sb_store::StorePaths;
use sb_store::{ObjectStore, read_error_as_store_error};

mod accounts;
pub use accounts::{
    ADD_BRIDGE_ACCOUNT, AddedAccount, BridgeAccountError, BridgeAccountParams, BridgeIdentity,
    certificate_pin_from_pem, connect_account,
};
// The transport items an account's sync worker drives (spec/01 rule 3):
// the engine applies the observations through the store.
pub use sb_imap::CertificatePin;
pub use sb_imap::{
    ConnectAttempt, Connection, Connector, HeaderBytes, HeaderFetchMode, HeaderRequest, HeaderRow,
    IdleEvent, ImapError, MailboxNotice, Password, ReconnectError, ReconnectWait, Reconnector,
};
pub use sb_platform::{Secret, SecretError, SecretStore};
pub use sb_store::accounts::{
    AddressMode, ConnectionPause, CredentialPause, EndpointSecurity, IdentitySendable,
    ImapEndpoint, ImapProvider, clear_connection_pause, clear_imap_cursors, imap_endpoint,
    pause_account, pause_for_connection, pause_for_credential, resume_account, syncable_accounts,
    update_account,
};
mod bodies;
pub use bodies::{AttachmentLease, BodyKind, BodyLease, PublishedAttachment, PublishedBody};
pub use sb_mime::{RenderWarning, RenderWarningCode};
mod acquire;
mod activation;
mod catchup;
mod drafts;
pub mod proton_bodies;
mod proton_send;
pub mod proton_sync;
pub mod sync;
pub use activation::{
    ActivationCommit, ActivationPreparation, ActivationPrepared, ActivationStatus, ActivationWork,
    RecordedActivationDeferral,
};
pub use catchup::{
    CatchupBatch, CatchupCommit, CatchupPreparation, CatchupReport, CatchupStatus, CatchupSweep,
};
pub use proton_send::{FrozenAttachment, FrozenBody, FrozenSend, frozen_send};
mod export_files;
mod import;
mod import_jobs;
mod index;
mod integrity;
pub use index::{
    IndexBlocked, IndexIoContext, IndexIoUpdate, IndexPreparation, IndexReadFailure,
    IndexReadStage, PreparedIndexBatch, index_retry_at, recover_index_io_failure,
    report_index_io_failure, search_io_code, search_io_message,
};
mod maintenance;
mod proton;
pub use proton::{
    APP_IDENTITY_REFUSED_REASON, BEGIN_PROTON_SIGN_IN, HUMAN_VERIFICATION_REASON,
    PROTON_API_BASE_URL, ProtonAccount, ProtonRefusal, ProtonSessionError, ProtonSessionSecret,
    ProtonSignInParams, ProtonTransport, RestoredProtonSession, SESSION_REFUSED_REASON,
    decode_proton_session, diagnostics_message, disconnect_proton_account, encode_proton_session,
    pause_for_reason, pause_for_session, proton_account, proton_identities,
    reset_proton_for_restore,
};
// The transport items an account's sign-in conversation drives
// (spec/01 rule 3): the engine keeps them in memory and maps errors
// without depending on a leaf.
pub use sb_proton::{
    ApiCode, AttachmentMetadata, AttachmentUpload, BackoffKind, CertificateDer, Challenge,
    Client as ProtonClient, DecryptError, Disposition, DraftAction, DraftAddress, DraftCreate,
    DraftTemplate, EventAction, EventBatch, EventId, FinishedSignIn, FullMessage, Identity,
    KeyStore, Label, LabelKind, ListingAnchor, MAX_PAGE_SIZE as PROTON_MAX_PAGE_SIZE,
    ModulusSignatureVerifier, ProtonDraftAttachment, ProtonError, RECIPIENT_TYPE_EXTERNAL,
    RECIPIENT_TYPE_INTERNAL, RecipientKeys, RefusalKind, SecretBuffer, SecretString,
    SendAttachment, SendBody, SendError, SendPackages, SendRecipient, SendRefusal,
    SendTransportError, Session, SignIn, SignInStep, Trust,
};
// The send lane's transport items (M4.3g): the lane drives the
// submission calls and their crypto the way sb-proton's fake-server
// tests do, so the resources layer re-exports what that needs beyond
// the worker's surface above, only under the test-only `test-seams`
// feature.
#[cfg(feature = "test-seams")]
pub use sb_proton::{
    DecryptedAddressKey, KeyFlag, KeyGenerator, KeyGeneratorSync, KeyId, PGPProviderSync,
    ProtonPGP, UnlockedAddressKey, build_packages, encrypt_attachment, encrypt_draft_body,
};
mod renditions;
mod repair;
mod restore;
mod search;
mod seen;
pub use renditions::{RenditionCache, RenditionError, RenditionFiles, RenditionProtection};
pub use repair::{
    RepairCapture, RepairCommit, RepairDiagnosis, RepairPersist, RepairPreparation, RepairPrepared,
    RepairSources, RepairStatus, RepairTerminal, RepairTerminalKind,
};
pub use restore::ResourcesSwapError;
pub use sb_store::{
    BACKUP_DESTINATION_SETTING, BackupDestination, BackupGeneration, BackupGenerationState,
    BackupReceipt, BackupSnapshot, FAILED_GENERATIONS_LISTED, GenerationVerification,
    PreparedRestoreStage, QualifiedStage, RestoreArtifact, RestoreArtifactKind, RestoreMetadata,
    StartedBackup, VerifiedExternalBackup, receipt_generation_id, valid_backup_destination,
};
pub use sb_store::{
    GC_GRACE_PERIOD, GcGate, GcReport, GenerationCheck, GenerationVerdict, MaintenanceFinish,
    MaintenanceJob, MaintenanceOutcome, MaintenanceRun, SampleReport,
};
pub use sb_store::{IntegrityCheck, IntegrityFinding, IntegrityLatch};
pub use search::{CoverageCounts, read_search_coverage, read_search_coverage_reusing};
pub use seen::{ReceivedCommit, ReceivedPreparation, ReceivedRevisions, ReceivedSweep};

// The engine's names, re-exported by item so the engine states bounds
// and maps errors without depending on a leaf (spec/01 rule 3). The
// default roots climb the same way, for the service's platform
// defaults.
pub use acquire::{AcquiredMessage, FlagExtraction, SourceAcquisition, SourcePosition};
pub(crate) use import::DescriptorCursor;
pub use import::{
    BundleFormat, ImportInventory, ImportInventoryOutcome, ImportProblem, ImportProblemKind,
    ImportProgress, ManifestDescriptor, ManifestProof, ManifestProofRegistry, SourceBundle,
    SourceInventory, SourceKind,
};
pub use import_jobs::{
    ImportJobBatch, ImportJobCheckpoint, ImportJobRecord, ImportTotals, LISTED_PROBLEMS,
    advance_import_job, commit_import_job_batch, create_import_job, finished_import_jobs,
    import_job, import_job_source, import_job_sources, import_report, record_import_sources,
    recorded_import_job_sources, recoverable_import_jobs,
};
pub use sb_mime::SEARCH_EXTRACTOR_VERSION;
pub use sb_mime::{
    Block, DOCUMENT_VERSION, DocumentSupport, HeadingLevel, ImageReference, Inline, MailDocument,
    MboxQuoting, MboxVariant,
};
// The outgoing-MIME items an account's send path composes with
// (spec/01 rule 3). `ComposeFormat` names the draft's stored format
// above, so the MIME one keeps its module's qualifier.
pub use sb_mime::ComposeFormat as MimeComposeFormat;
pub use sb_mime::{ComposeAddress, ComposeAttachment, ComposeError, ComposeInput, compose};
// Paste as Markdown's pure conversion (spec/07 "Paste conversion").
pub use sb_mime::{
    MAX_PASTE_HTML_BYTES, MarkdownFromHtml, PasteError, PasteLoss, markdown_from_html,
};
pub use sb_platform::{default_cache_root, default_data_root};
pub use sb_store::ACCOUNT_PALETTE_V1;
pub use sb_store::appearance::{
    AccountColor, AppearanceAssignment, StoredAccountAppearance, StoredIdentityAppearance,
    account_appearance, appearance_generation, identity_appearance, identity_appearances,
    initialize_account_appearance, set_account_appearance, set_identity_appearance,
};
pub use sb_store::compose::{
    ComposeCatalog, ComposeIdentity, MAX_RECIPIENT_INPUT_BYTES, RecipientInputError,
    RecipientQuery, compose_catalog, parse_draft_recipients, recipient_completions,
};
pub use sb_store::drafts::{
    AttachmentRef, AttemptState, ComposeFormat, DraftAttachment, DraftRecipient, DraftSource,
    DraftState, DraftSummary, LiveAttempt, ReplySource, RevisionInput, StoredDraft, StoredRevision,
    create_draft, discard_draft, draft, drafts, reply_source, save_draft,
};
pub use sb_store::exports::{
    ExportCaptureRequest, ExportChunkWriter, ExportFormat, ExportJob, ExportJobState, ExportScope,
    ExportSearchSort, ExportSelection, MAX_EXPORT_CHANGED_BYTES, MAX_EXPORT_CHUNK_BYTES,
    MAX_EXPORT_CHUNKS, MAX_EXPORT_JSON_BYTES, advance_export_job, append_export_chunks,
    create_export_job, export_chunks, export_job, recoverable_export_jobs,
};
pub use sb_store::identities::{
    IdentityError, IdentityResolutionInputs, IdentityRoute, IdentityWrite, SourceIdentityInputs,
    SourceMessageHeaders, identity_resolution_inputs, set_default_identity, upsert_identity,
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
pub use sb_store::intents::{
    ClaimedIntent, FailureOutcome, FlagChange, IntentAnchor, IntentCounts, IntentEffect,
    IntentFilter, IntentFlag, IntentKey, IntentKind, IntentOrigin, IntentPage, IntentState,
    MAX_INTENT_PAGE_ROWS, QuarantineReason, StoredIntent, UndoableIntent, WaitReason,
    cancel_intents, claim_next_intent, clear_account_intents_waiting, finish_intent_done,
    intent_counts, intent_for_undo, intents, park_intent, plan_flag_intents, quarantine_intent,
    record_intent_failure, recover_in_flight_intents, release_intents, return_intent_to_pending,
    set_account_intents_waiting,
};
pub use sb_store::lists::{
    ListAnchor, ListKey, ListPage, ListScope, MAX_PAGE_ROWS, MailboxRole, PageRead, StoredListRow,
    list_generation, list_ids, read_list_page,
};
pub use sb_store::messages::{
    BodyAvailability, MAX_READER_HEADER_BYTES, MAX_READER_MESSAGE_BYTES, ReaderAddress,
    ReaderHeaders, ReaderMessage, ReaderOccurrence, ReaderTag, StoredAcquisition, best_acquisition,
    get_message, message_for_occurrence, messages_for_occurrences,
};
pub use sb_store::query::{
    AddressOperand, Kept, MAX_QUERY_INPUT_BYTES, ParseCode, ParseContext, Query, QueryCounts,
    QueryError, SearchAnchor, SearchIndexState, SearchLimitation, SearchPage,
    SearchRebuildProgress, SearchScope, StoredSearchCoverage as Coverage, TextFormatError,
    UnknownCount, counts, read_search_date_page, read_search_relevance_page, search_date_ids,
    search_relevance_ids,
};
pub use sb_store::recovery::{
    MAX_RECOVERY_PAGE_ROWS, RecoveryAnchor, RecoveryKey, RecoveryPage, RecoveryState,
    StoredRecovery, recovery,
};
pub use sb_store::send::{
    ClaimedRecipient, ClaimedSend, OutBox, OutBoxRecipient, OutBoxRow, ProtonUpload, RecipientKind,
    RecipientState, SendAttempt, SendFailure, SendFailureRejection, SendParent, SendParentKind,
    SentCopy, UnknownResolution, UnknownSend, cancel_send, claim_send_attempt,
    commit_send_requested, confirm_send_route, earliest_hold_until, mark_outcome_unknown,
    mark_send_accepted, mark_send_failed, out_box, queue_due_held, record_proton_draft,
    record_proton_upload, recover_proton_submitting, reply_headers, reply_source_account,
    requeue_before_request, resolve_unknown_send, retry_rejected_recipients, send_draft,
    unknown_proton_attempts,
};
pub use sb_store::settings::{StoredSetting, read_all, upsert};
pub use sb_store::sidebar::{
    AccountKind, AccountState, MailboxKind, MailboxList, StoredAccount, StoredMailbox, accounts,
    mailboxes,
};
pub use sb_store::tags::{add_tag, remove_tag, set_note};
pub use sb_store::threads::{
    StoredThread, ThreadDescription, ThreadRevisions, ThreadSource, assign_thread, create_thread,
    describe_thread, mark_threads_built, note_membership_change, remove_empty_threads,
    remove_thread, stored_threads, thread_memberships, thread_revisions, thread_sources,
};
pub use sb_store::views::{
    SavedView, ViewSort, delete_saved_view, saved_view, saved_views, upsert_saved_view,
};
pub use sb_store::{
    AppearanceObserver, ExportMetadata, ImportSourceSnapshot, ListObserver, PublishedObject,
    ReadStop, Receipt, Scoped, SearchObserver, StoreError, Timestamp, Transaction, Uuid,
    WriteFuture, WritePriority, await_write, encode_job_receipt, export_occurrence_metadata,
    receipt_job_id, redact_addresses, replay, run_once, schema_version,
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
    secrets: Arc<dyn SecretStore>,
    proton: ProtonTransport,
}

/// The secret store when the platform has none (spec/10): every call is
/// a typed `Unavailable`, so setup reaches its pause path instead of
/// failing open or the whole core failing to open.
struct UnavailableSecrets;

impl SecretStore for UnavailableSecrets {
    fn get(&self, _credential_ref: &str) -> Result<Secret, SecretError> {
        Err(SecretError::Unavailable)
    }

    fn set(&self, _credential_ref: &str, _secret: &Secret) -> Result<(), SecretError> {
        Err(SecretError::Unavailable)
    }

    fn delete(&self, _credential_ref: &str) -> Result<(), SecretError> {
        Err(SecretError::Unavailable)
    }
}

/// The platform's protected secret store (spec/10), or
/// [`UnavailableSecrets`] where it cannot be built (Linux today).
fn platform_secrets() -> Arc<dyn SecretStore> {
    match PlatformSecrets::new() {
        Ok(store) => Arc::new(store),
        Err(_) => Arc::new(UnavailableSecrets),
    }
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

    /// [`Resources::open`] on an injected secret store: setup suites use
    /// the fake store and its fault injection (spec/11). The platform
    /// store is never built on this path.
    #[cfg(feature = "test-seams")]
    pub fn open_with_secrets(
        data_root: &Path,
        secrets: Arc<dyn SecretStore>,
    ) -> Result<Self, StoreError> {
        Self::open_on_with_secrets(
            data_root,
            Arc::new(RealFs),
            Arc::new(SystemClock),
            secrets,
            ProtonTransport::default(),
        )
    }

    /// [`Resources::open`] on injected secret store and Proton
    /// transport seams: setup suites use the fake store and the fake
    /// server's roots and verifier (spec/11).
    #[cfg(feature = "test-seams")]
    pub fn open_with_proton(
        data_root: &Path,
        clock: Arc<dyn Clock>,
        secrets: Arc<dyn SecretStore>,
        proton: ProtonTransport,
    ) -> Result<Self, StoreError> {
        Self::open_on_with_secrets(data_root, Arc::new(RealFs), clock, secrets, proton)
    }

    /// [`Resources::open_with_proton`] with the disposable rendition
    /// cache body leases need (test seam; spec/11). The cache root is
    /// validated like [`Resources::open_with_cache`]'s.
    #[cfg(feature = "test-seams")]
    pub fn open_with_proton_and_cache(
        data_root: &Path,
        cache_root: &Path,
        clock: Arc<dyn Clock>,
        secrets: Arc<dyn SecretStore>,
        proton: ProtonTransport,
    ) -> Result<Self, StoreError> {
        let data =
            bodies::resolved(&RealFs, data_root).map_err(|e| bodies::unavailable(e.to_string()))?;
        let cache = bodies::resolved(&RealFs, cache_root)
            .map_err(|e| bodies::unavailable(e.to_string()))?;
        if cache.starts_with(&data) || data.starts_with(cache.join("renditions")) {
            return Err(StoreError::InvalidInput {
                message: "cache root must be outside the data root, and renditions must not contain the data root".into(),
            });
        }
        let mut resources = Self::open_with_proton(data_root, clock, secrets, proton)?;
        let fs = Arc::clone(&resources.fs);
        let random = Arc::clone(&resources.random);
        resources.renditions = match RenditionCache::open_on(cache_root, fs, random) {
            Ok(cache) => Some(cache),
            Err(RenditionError::Locked) => None,
            Err(e) => return Err(bodies::cache_error(e)),
        };
        Ok(resources)
    }

    fn open_on(
        data_root: &Path,
        fs: Arc<dyn Fs>,
        clock: Arc<dyn Clock>,
    ) -> Result<Self, StoreError> {
        Self::open_on_with_secrets(
            data_root,
            fs,
            clock,
            platform_secrets(),
            ProtonTransport::default(),
        )
    }

    fn open_on_with_secrets(
        data_root: &Path,
        fs: Arc<dyn Fs>,
        clock: Arc<dyn Clock>,
        secrets: Arc<dyn SecretStore>,
        proton: ProtonTransport,
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
            secrets,
            proton,
        })
    }

    /// Store `secret` under `credential_ref` in the platform secret store
    /// (spec/10). Blocking — keychain IPC, or a Secret Service unlock
    /// prompt — so callers run it on a blocking thread, never on a
    /// runtime worker.
    pub fn set_secret(&self, credential_ref: &str, secret: &Secret) -> Result<(), SecretError> {
        self.secrets.set(credential_ref, secret)
    }

    /// Read the secret under `credential_ref` for a connection attempt
    /// (spec/10): a sync worker fetches it fresh for each attempt and
    /// never keeps it. `NotFound` when nothing is stored. Blocking, like
    /// [`Resources::set_secret`].
    pub fn get_secret(&self, credential_ref: &str) -> Result<Secret, SecretError> {
        self.secrets.get(credential_ref)
    }

    /// Delete the secret under `credential_ref` (spec/10): only an
    /// explicit, authorized disconnect calls this. Blocking, like
    /// [`Resources::set_secret`].
    pub fn delete_secret(&self, credential_ref: &str) -> Result<(), SecretError> {
        self.secrets.delete(credential_ref)
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

    /// Blocking capture sink. Replay the full stream and finish explicitly;
    /// it borrows this store and does not seal a scope.
    ///
    /// The ownership lease cannot close while the sink is live:
    /// ```compile_fail
    /// fn cannot_close(res: sb_resources::Resources, job: &sb_resources::ExportJob) {
    ///     let sink = res.export_chunk_writer(job).unwrap();
    ///     res.close().unwrap();
    ///     sink.finish().unwrap();
    /// }
    /// ```
    pub fn export_chunk_writer(
        &self,
        job: &ExportJob,
    ) -> Result<ExportChunkWriter<'_>, StoreError> {
        self.store.export_chunk_writer(job)
    }

    /// Blocking four-scope capture, with no file output. Break stops before
    /// acknowledgement; a completed capture is immutable and replayable.
    pub fn capture_export(
        &self,
        id: &str,
        request: &ExportCaptureRequest,
        progress: impl FnMut(u64) -> ControlFlow<()>,
    ) -> Result<Scoped<ExportJob>, StoreError> {
        self.store.capture_export(id, request, progress)
    }

    /// Blocking file writer for a sealed export capture (spec/09
    /// "Export file output"): claims the destination, streams verified
    /// acquisitions per message (EML) or per mailbox (mboxrd), then
    /// publishes the version-1 manifest; `Break` stops before the next
    /// commit.
    pub fn export_files(
        &self,
        id: &str,
        progress: impl FnMut(u64) -> ControlFlow<()>,
    ) -> Result<Scoped<ExportJob>, StoreError> {
        if self.store.is_writer_thread() {
            return Err(StoreError::internal(
                "the writer thread cannot export files through its own queue",
            ));
        }
        export_files::run(
            &self.store,
            self.fs.as_ref(),
            self.clock.as_ref(),
            id,
            progress,
        )
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

    /// Open one bundle EML source's manifest-verified acquisition session
    /// (spec/09 "Export bundles"). `registry` supplies the frozen
    /// manifest proof; the source's private snapshot is verified and
    /// reopened. `position` is `START` or the source's one finished
    /// message. `progress` cancels the first proof production before the
    /// session exists. EML and mboxrd sources are dispatched on the
    /// bundle's declared format; a bundle source
    /// never falls back to the plain path.
    pub fn open_bundle_acquisition(
        &self,
        registry: &ManifestProofRegistry,
        source: &SourceInventory,
        position: SourcePosition,
        progress: impl FnMut(u64) -> ControlFlow<()>,
    ) -> Result<Scoped<SourceAcquisition>, StoreError> {
        acquire::open_bundle(self, registry, source, position, progress)
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

    /// Read one published object's exact bytes after EOF verifies the
    /// hash (spec/03). Blocking I/O; run on a blocking thread.
    pub fn read_object(&self, id: &ObjectId) -> Result<Vec<u8>, StoreError> {
        let mut reader = self.store.objects().open(id)?;
        let mut bytes = Vec::with_capacity(usize::try_from(id.byte_length()).unwrap_or(0));
        reader.read_to_end(&mut bytes).map_err(|error| {
            read_error_as_store_error(&error)
                .cloned()
                .unwrap_or_else(|| StoreError::Unavailable {
                    reason: format!("cannot read the object: {error}"),
                    io_kind: None,
                })
        })?;
        Ok(bytes)
    }

    /// Read a published object's exact bytes by its SHA-256 alone,
    /// verifying the hash at EOF (spec/03). A send attempt records only
    /// the hash, so this is its frozen-bytes read (spec/05). Blocking
    /// I/O; run on a blocking thread.
    pub fn read_object_by_sha256(&self, sha: &str) -> Result<Vec<u8>, StoreError> {
        let mut reader = self.store.objects().open_by_sha256(sha)?;
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).map_err(|error| {
            read_error_as_store_error(&error)
                .cloned()
                .unwrap_or_else(|| StoreError::Unavailable {
                    reason: format!("cannot read the object: {error}"),
                    io_kind: None,
                })
        })?;
        Ok(bytes)
    }

    /// Publish one frozen message's bytes to the object store
    /// (spec/03). The returned [`PublishedObject`] carries the
    /// one-shot reservation the send transaction spends.
    pub fn publish_object(&self, bytes: Vec<u8>) -> Result<PublishedObject, StoreError> {
        self.store.objects().publish(bytes.as_slice())
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
