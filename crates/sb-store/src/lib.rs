//! `sb-store` — L5. SQLite, migrations, object store, backups, FTS5,
//! query AST + SQL. Doesn't know about IMAP; reaches platform services
//! only through `sb-types` seam traits.
#![deny(unsafe_code)]

pub mod accounts;
pub mod activation;
mod address_keys;
pub mod appearance;
mod backups;
mod changes;
pub mod compose;
mod corpus_tracking;
pub mod drafts;
mod error;
mod export_metadata;
pub mod exports;
mod gc;
#[allow(unsafe_code)]
mod image_vfs;
mod import_reports;
pub mod imports;
pub mod index;
mod integrity;
mod list_rows;
pub mod lists;
mod maintenance;
mod message_source;
pub mod messages;
mod objects;
mod paths;
pub mod proton;
pub mod query;
mod readers;
mod receipts;
pub mod repair;
mod restore_artifacts;
mod restore_pending;
mod schema;
pub mod seen;
pub mod settings;
pub mod sidebar;
mod source_snapshots;
mod store;
pub mod sync;
pub mod tags;
#[cfg(test)]
mod testing;
pub mod threads;
mod validation_scratch;
mod verification;
pub mod views;
mod writer;

// The seam vocabulary sb-types owns (spec/01 rules 2 and 3), re-exported
// by item so the layer above — which cannot name sb-types directly —
// reaches it through this crate, one layer at a time.
pub use sb_types::appearance::ACCOUNT_PALETTE_V1;
pub use sb_types::fs::{FileKind, Fs, ReadFile, WriteFile};
pub use sb_types::id::Uuid;
pub use sb_types::random::Random;
pub use sb_types::time::{Clock, Timestamp};

// The write API's closure parameter, re-exported for the same reason:
// the layer above states the bound without taking a rusqlite
// dependency of its own.
pub use rusqlite::Transaction;

pub use backups::{
    BACKUP_DESTINATION_SETTING, BackupDestination, BackupGeneration, BackupGenerationState,
    BackupImageInspection, BackupReceipt, BackupSnapshot, FAILED_GENERATIONS_LISTED,
    GenerationCheck, GenerationVerdict, GenerationVerification, PreparedRestoreStage,
    QualifiedStage, RestoreMetadata, START_BACKUP, START_RESTORE, StartedBackup,
    VerifiedExternalBackup, valid_backup_destination,
};
pub use error::StoreError;
pub use export_metadata::{ExportMetadata, export_occurrence_metadata};
pub use gc::{GC_GRACE_PERIOD, GcGate, GcReport};
pub use integrity::{IntegrityCheck, IntegrityFinding, IntegrityLatch};
pub use maintenance::{
    MAINTENANCE_DETAIL_MAX, MaintenanceFinish, MaintenanceJob, MaintenanceOutcome, MaintenanceRun,
};
pub use objects::{
    FileObjectStore, ObjectId, ObjectIter, ObjectReader, ObjectStore, PublishedObject,
    read_error_as_store_error,
};
pub use paths::StorePaths;
pub use proton::{
    AppliedEvents, BreakerResolution, Classified, EventChunk, ListedMessage, ListedPage,
    ListedPageStats, MailboxCoverage, NewProtonAccount, ObservedLabel, ProtonAccount,
    ProtonBodyFailure, ProtonBodyFailureReason, ProtonChange, ProtonCoverage, ProtonListingState,
    ProtonPasswordMode, apply_event_chunk, begin_listing, body_failure, clear_body_failure,
    commit_listed_page, complete_listing, finish_listing, insert_proton_account, proton_account,
    proton_account_by_user_id, proton_coverage, reconcile_proton_labels, record_body_failure,
    resolve_breaker_event, set_diagnostics, set_event_cursor, update_listing_checkpoint,
};
pub use readers::{ReadStop, Scoped};
pub use receipts::{
    Receipt, encode_job_receipt, receipt_generation_id, receipt_job_id, replay, run_once,
};
pub use restore_artifacts::{RestoreArtifact, RestoreArtifactKind};
pub use settings::{StoredSetting, read_all, upsert};
pub use source_snapshots::{ImportSourceCapture, ImportSourceSnapshot};
pub use store::SwapRestoreError;
pub use store::{ReadOnlyStore, Seams, Store, await_write, open, open_read_only};
pub use tags::{add_tag, remove_tag, set_note};
pub use validation_scratch::{RegistryFile, ValidationFile, ValidationScratch, ValidationTape};
pub use verification::{SAMPLE_BUCKETS, SampleReport};
pub use views::{
    SavedView, ViewSort, delete_saved_view, saved_view, saved_views, upsert_saved_view,
};

#[cfg(feature = "test-seams")]
pub use list_rows::refresh_list_rows;
pub use writer::{
    AppearanceObserver, ListObserver, SearchObserver, WriteFuture, WritePriority, Writer,
};
