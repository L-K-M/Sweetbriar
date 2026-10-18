//! `sb-store` — L5. SQLite, migrations, object store, backups, FTS5,
//! query AST + SQL. Doesn't know about IMAP; reaches platform services
//! only through `sb-types` seam traits.
#![forbid(unsafe_code)]

mod error;
pub mod imports;
mod objects;
mod paths;
mod readers;
mod receipts;
mod schema;
pub mod settings;
mod source_snapshots;
mod store;
#[cfg(test)]
mod testing;
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
pub use objects::{
    FileObjectStore, ObjectId, ObjectIter, ObjectReader, ObjectStore, read_error_as_store_error,
};
pub use paths::StorePaths;
pub use receipts::{Receipt, run_once};
pub use settings::{StoredSetting, read_all, upsert};
pub use source_snapshots::{ImportSourceCapture, ImportSourceSnapshot};
pub use store::{ReadOnlyStore, Seams, Store, open, open_read_only};
pub use writer::{WriteFuture, WritePriority, Writer};
