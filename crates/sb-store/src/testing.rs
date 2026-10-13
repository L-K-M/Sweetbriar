//! Helpers the crate's tests share: deterministic seams and scratch
//! stores.

use std::sync::Arc;

use sb_test_support::{FixedClock, ScratchDir, SeededRandom};
use sb_types::fs::Fs;
use sb_types::time::Timestamp;

use crate::{Seams, Store, StoreError, StorePaths};

/// The moment tests open at (2025-01-01T00:00:00Z).
pub(crate) const FIXED_NOW_MS: i64 = 1_735_689_600_000;

/// Deterministic seams over `fs`: one clock value and one seed, so
/// `created_at` and the UUIDs are reproducible.
pub(crate) fn test_seams(fs: Arc<dyn Fs>) -> Seams {
    Seams::new(
        fs,
        Arc::new(FixedClock::new(Timestamp::from_millis(FIXED_NOW_MS))),
        Arc::new(SeededRandom::new(42)),
    )
}

/// A fresh data root under a unique scratch dir, as `StorePaths`.
pub(crate) fn scratch_paths(label: &str) -> (ScratchDir, StorePaths) {
    let scratch = ScratchDir::new(label);
    let paths = StorePaths::new(scratch.path().join("store")).unwrap();
    (scratch, paths)
}

/// One read through the store's pool, unwrapped: the shape the
/// crate's tests read with before M0.6 gave the store readers. A
/// failing read is a failed test, not a value to inspect.
pub(crate) fn reading<T>(
    store: &Store,
    f: impl FnOnce(&rusqlite::Transaction) -> Result<T, StoreError>,
) -> T {
    store.read(f).expect("the read through the pool failed")
}

/// One user-priority write through the store's writer, awaited and
/// unwrapped: the shape the crate's tests wrote with before M0.6 gave
/// the store a writer thread.
pub(crate) fn writing<T>(
    store: &Store,
    f: impl FnOnce(&rusqlite::Transaction) -> Result<T, StoreError> + Send + 'static,
) -> T
where
    T: Send + 'static,
{
    sb_test_support::block_on(store.write(crate::WritePriority::User, f))
        .expect("the write through the writer failed")
}

/// The env vars that turn this test binary into a crashing writer: the
/// database to write and the SQL to commit.
const CRASH_DB_ENV: &str = "SB_STORE_TEST_CRASH_DB";
const CRASH_SQL_ENV: &str = "SB_STORE_TEST_CRASH_SQL";

/// Not a test: [`crash_write`] re-executes the test binary to run it.
/// It commits `CRASH_SQL_ENV` into the WAL and exits without closing
/// the connection, leaving committed frames behind as a crash does.
#[test]
#[ignore = "spawned by crash_write"]
fn crashed_writer_child() {
    let db = std::env::var(CRASH_DB_ENV).expect("database env");
    let sql = std::env::var(CRASH_SQL_ENV).expect("SQL env");
    let raw = rusqlite::Connection::open(db).unwrap();
    raw.execute_batch("PRAGMA wal_autocheckpoint = 0;").unwrap();
    raw.execute_batch(&sql).unwrap();
    std::process::exit(0);
}

/// Commit `sql` from a child process that then dies, so the WAL holds
/// frames the main file doesn't.
pub(crate) fn crash_write(paths: &StorePaths, sql: &str) {
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "testing::crashed_writer_child", "--ignored"])
        .env(CRASH_DB_ENV, paths.state_db())
        .env(CRASH_SQL_ENV, sql)
        .status()
        .unwrap();
    assert!(status.success(), "the crashing writer failed: {status}");
    let wal = std::fs::metadata(paths.data_root().join("state.sqlite-wal")).unwrap();
    assert!(wal.len() > 0, "the crash left no WAL frames");
}
