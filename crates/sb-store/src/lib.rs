//! `sb-store` — L5. SQLite, migrations, object store, backups, FTS5,
//! query AST + SQL. Doesn't know about IMAP; reaches platform services
//! only through `sb-types` seam traits.
#![forbid(unsafe_code)]

mod error;
mod paths;
mod store;

pub use error::StoreError;
pub use paths::StorePaths;
pub use store::{Store, open};
