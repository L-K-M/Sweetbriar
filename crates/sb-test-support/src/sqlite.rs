//! A store's database seen from outside the store: tests above
//! `sb-store` (the engine, the service) observe what the store wrote,
//! or damage it as a crash would, without reaching into a leaf.

use std::path::Path;
use std::time::Duration;

use rusqlite::{Connection, OpenFlags, OptionalExtension, TransactionBehavior};

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

/// Hold a real writer lock for synchronous coordination; never await inside `body`.
/// No row changes; unwinding drops the transaction and releases the lock too.
pub fn with_sqlite_write_lock(state_db: &Path, body: impl FnOnce()) {
    let mut conn = Connection::open(state_db)
        .unwrap_or_else(|e| panic!("cannot open {}: {e}", state_db.display()));
    conn.busy_timeout(Duration::from_secs(30)).unwrap();
    let transaction = conn
        .transaction_with_behavior(TransactionBehavior::Immediate)
        .unwrap();
    body();
    transaction.rollback().unwrap();
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn native_writer_lock_excludes_writes_and_releases_on_return_and_panic() {
        let scratch = crate::ScratchDir::new("native-writer-gate");
        let db = scratch.path().join("gate.sqlite");
        let other = Connection::open(&db).unwrap();
        other
            .execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE proof(value INTEGER)")
            .unwrap();
        other.busy_timeout(Duration::ZERO).unwrap();
        with_sqlite_write_lock(&db, || {
            let error = other
                .execute("INSERT INTO proof VALUES(1)", [])
                .unwrap_err();
            assert_eq!(
                error.sqlite_error_code(),
                Some(rusqlite::ErrorCode::DatabaseBusy)
            );
        });
        other.execute("INSERT INTO proof VALUES(1)", []).unwrap();
        assert!(
            std::panic::catch_unwind(|| {
                with_sqlite_write_lock(&db, || panic!("coordinating caller stopped"));
            })
            .is_err()
        );
        other.execute("INSERT INTO proof VALUES(2)", []).unwrap();
        assert_eq!(
            other
                .query_row("SELECT count(*) FROM proof", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            2
        );
    }
}
