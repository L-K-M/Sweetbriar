//! A store's database seen from outside the store: tests above
//! `sb-store` (the engine, the service) observe what the store wrote,
//! or damage it as a crash would, without reaching into a leaf.

use std::path::Path;

use rusqlite::{Connection, OpenFlags, OptionalExtension};

/// One `store_meta` value, read through a read-only connection so the
/// look itself changes nothing (it never checkpoints).
pub fn store_meta_value(state_db: &Path, key: &str) -> Option<String> {
    let conn = Connection::open_with_flags(state_db, OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap_or_else(|e| panic!("cannot open {} read-only: {e}", state_db.display()));
    conn.query_row(
        "SELECT value FROM store_meta WHERE key = ?1",
        [key],
        |row| row.get(0),
    )
    .optional()
    .unwrap_or_else(|e| panic!("cannot read store_meta.{key}: {e}"))
}

/// Run `sql` on a store's database outside the store, as damage, a
/// crash or another tool would.
pub fn execute_sql(state_db: &Path, sql: &str) {
    let conn = Connection::open(state_db)
        .unwrap_or_else(|e| panic!("cannot open {}: {e}", state_db.display()));
    conn.execute_batch(sql)
        .unwrap_or_else(|e| panic!("cannot run SQL on {}: {e}", state_db.display()));
}
