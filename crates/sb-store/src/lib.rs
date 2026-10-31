//! `sb-store` — L5. SQLite, migrations, object store, backups, FTS5,
//! query AST + SQL. Doesn't know about IMAP; reaches platform services
//! only through `sb-types` seam traits.
#![forbid(unsafe_code)]

pub mod activation;
mod address_keys;
mod corpus_tracking;
mod error;
mod export_metadata;
mod import_reports;
pub mod imports;
pub mod index;
mod list_rows;
pub mod lists;
mod message_source;
pub mod messages;
mod objects;
mod paths;
pub mod query;
mod readers;
mod receipts;
pub mod repair;
mod schema;
pub mod seen;
pub mod settings;
pub mod sidebar;
mod source_snapshots;
mod store;
#[cfg(test)]
mod testing;
pub mod threads;
mod writer;

// The seam vocabulary sb-types owns (spec/01 rules 2 and 3), re-exported
// by item so the layer above — which cannot name sb-types directly —
// reaches it through this crate, one layer at a time.
pub use sb_types::fs::{FileKind, Fs, ReadFile};
pub use sb_types::id::Uuid;
pub use sb_types::random::Random;
pub use sb_types::time::{Clock, Timestamp};

// The write API's closure parameter, re-exported for the same reason:
// the layer above states the bound without taking a rusqlite
// dependency of its own.
pub use rusqlite::Transaction;

pub use error::StoreError;
pub use export_metadata::{ExportMetadata, export_occurrence_metadata};
pub use objects::{
    FileObjectStore, ObjectId, ObjectIter, ObjectReader, ObjectStore, PublishedObject,
    read_error_as_store_error,
};
pub use paths::StorePaths;
pub use readers::{ReadStop, Scoped};
pub use receipts::{Receipt, encode_job_receipt, receipt_job_id, run_once};
pub use settings::{StoredSetting, read_all, upsert};
pub use source_snapshots::{ImportSourceCapture, ImportSourceSnapshot};
pub use store::{ReadOnlyStore, Seams, Store, open, open_read_only};

#[cfg(feature = "test-seams")]
pub use list_rows::refresh_list_rows;
pub use writer::{ListObserver, SearchObserver, WriteFuture, WritePriority, Writer};
