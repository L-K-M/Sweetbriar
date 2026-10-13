//! `sb-store` — L5. SQLite, migrations, object store, backups, FTS5,
//! query AST + SQL. Doesn't know about IMAP; reaches platform services
//! only through `sb-types` seam traits.
#![forbid(unsafe_code)]

mod error;
mod objects;
mod paths;
mod schema;
mod store;
#[cfg(test)]
mod testing;

pub use error::StoreError;
pub use objects::{
    FileObjectStore, ObjectId, ObjectIter, ObjectReader, ObjectStore, read_error_as_store_error,
};
pub use paths::StorePaths;
pub use store::{Seams, Store, open};
