//! Native proof for the confined WAL2 staging writer (M1.8e3b2b4): a
//! real WAL2 snapshot copied into four retained capabilities, exclusive
//! heap wal-index mode, an explicit guarded checkpoint, close/reopen,
//! receipt invalidation, malformed-schema refusal, fired WAL I/O and
//! panic faults, unconfined-path refusal and retained-inode replacement.
//! Every test drives the actual retained VFS; no C callback is invoked
//! by hand in place of the main proof.

use std::ffi::CString;
use std::io::Read as _;

use super::super::{Context, METHODS, StageFile, close, default_vfs};
use super::*;

/// The synthetic WAL2 image is tiny; never read an unbounded fixture.
const FIXTURE_BOUND: u64 = 64 * 1024;

type Capabilities = (
    Arc<dyn ReadWriteFile>,
    Arc<dyn ReadWriteFile>,
    Arc<dyn ReadWriteFile>,
    Arc<dyn ReadWriteFile>,
);

fn caps4(fs: &SimFs) -> Capabilities {
    caps4_in(fs, "/stage")
}

fn caps4_in(fs: &SimFs, dir: &str) -> Capabilities {
    fs.create_dir_all(Path::new(dir)).unwrap();
    let root = fs.open_confined_dir(Path::new(dir)).unwrap();
    let main: Arc<dyn ReadWriteFile> = Arc::from(root.create_read_write(Path::new("db")).unwrap());
    let journal: Arc<dyn ReadWriteFile> =
        Arc::from(root.create_read_write(Path::new("journal")).unwrap());
    let statement: Arc<dyn ReadWriteFile> =
        Arc::from(root.create_read_write(Path::new("stmt")).unwrap());
    let wal: Arc<dyn ReadWriteFile> = Arc::from(root.create_read_write(Path::new("wal")).unwrap());
    (main, journal, statement, wal)
}

/// Read at most [`FIXTURE_BOUND`] bytes and prove nothing was truncated.
fn read_bounded(path: &Path) -> Vec<u8> {
    let len = std::fs::metadata(path).unwrap().len();
    assert!(len <= FIXTURE_BOUND, "fixture is not tiny");
    let file = std::fs::File::open(path).unwrap();
    let mut bytes = Vec::new();
    file.take(FIXTURE_BOUND).read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes.len() as u64, len, "fixture read was truncated");
    bytes
}

fn stage_main(cap: &dyn ReadWriteFile, bytes: &[u8]) {
    assert_eq!(cap.write_at(bytes, 0).unwrap(), bytes.len());
    cap.set_len(bytes.len() as u64).unwrap();
}

fn read_all(cap: &dyn ReadWriteFile) -> Vec<u8> {
    let len = cap.len().unwrap() as usize;
    let mut bytes = vec![0u8; len];
    let mut done = 0;
    while done < len {
        let read = cap.read_at(&mut bytes[done..], done as u64).unwrap();
        assert!(read > 0, "retained read made no progress");
        done += read;
    }
    bytes
}

fn assert_header2(cap: &dyn ReadWriteFile) {
    let bytes = read_all(cap);
    assert!(bytes.len() >= 20, "database header is truncated");
    assert_eq!(&bytes[18..20], &[2, 2], "database is not in WAL2 format");
}

fn assert_cleanup(context: &Arc<Context>, name: &std::ffi::CStr) {
    assert_eq!(
        context.opens.load(Ordering::SeqCst),
        context.closes.load(Ordering::SeqCst),
        "opened files were not all closed"
    );
    assert!(
        unsafe { ffi::sqlite3_vfs_find(name.as_ptr()) }.is_null(),
        "the private VFS is still registered"
    );
    assert_eq!(
        Arc::strong_count(context),
        1,
        "callbacks outlived the connection"
    );
}

fn assert_open_wal_error(
    main: &Arc<dyn ReadWriteFile>,
    journal: &Arc<dyn ReadWriteFile>,
    statement: &Arc<dyn ReadWriteFile>,
    wal: &Arc<dyn ReadWriteFile>,
) -> rusqlite::Error {
    match ConfinedSqlite::open_wal(
        Arc::clone(main),
        Arc::clone(journal),
        Arc::clone(statement),
        Arc::clone(wal),
    ) {
        Ok(_) => panic!("open_wal accepted an invalid WAL2 staging set"),
        Err(error) => error,
    }
}

fn assert_cannot_open(error: rusqlite::Error) {
    assert!(
        matches!(
            error,
            rusqlite::Error::SqliteFailure(
                ffi::Error {
                    code: ffi::ErrorCode::CannotOpen,
                    ..
                },
                Some(_)
            )
        ),
        "unexpected error: {error:?}"
    );
}

/// Create a real WAL2 snapshot with one committed row, close the source
/// (which checkpoints and deletes its WAL) and return the bounded image.
/// The source file stays alive so tests can prove it was never written.
fn wal2_source(tag: &str) -> (ScratchDir, Vec<u8>) {
    let scratch = ScratchDir::new(tag);
    let path = scratch.path().join("source.db");
    {
        let source = Connection::open(&path).unwrap();
        source
            .execute_batch(
                "PRAGMA journal_mode=WAL; \
                 CREATE TABLE proof(value TEXT); \
                 INSERT INTO proof VALUES('preserved');",
            )
            .unwrap();
    }
    let bytes = read_bounded(&path);
    assert!(bytes.len() >= 20, "fixture header is truncated");
    assert_eq!(&bytes[18..20], &[2, 2], "fixture is not in WAL2 format");
    (scratch, bytes)
}

/// A WAL2 image whose schema is malformed on disk. `PRAGMA
/// locking_mode=EXCLUSIVE` is schema-free and still works, while
/// `PRAGMA journal_mode` and any checkpoint load the schema and fail.
fn malformed_wal2_source(tag: &str) -> (ScratchDir, Vec<u8>) {
    let scratch = ScratchDir::new(tag);
    let path = scratch.path().join("malformed.db");
    {
        let source = Connection::open(&path).unwrap();
        source
            .execute_batch(
                "PRAGMA journal_mode=WAL; \
                 CREATE TABLE proof(value TEXT); \
                 INSERT INTO proof VALUES('preserved');",
            )
            .unwrap();
        source
            .execute_batch(
                "PRAGMA writable_schema=ON; \
                 UPDATE sqlite_schema SET sql='THIS IS NOT SQL' WHERE name='proof'; \
                 PRAGMA writable_schema=OFF;",
            )
            .unwrap();
    }
    let bytes = read_bounded(&path);
    assert_eq!(&bytes[18..20], &[2, 2], "fixture is not in WAL2 format");
    (scratch, bytes)
}

/// A real rollback-journal image, used to prove the WAL entry refuses it.
fn rollback_source(tag: &str) -> Vec<u8> {
    let scratch = ScratchDir::new(tag);
    let path = scratch.path().join("rollback.db");
    {
        let source = Connection::open(&path).unwrap();
        source
            .execute_batch(
                "PRAGMA journal_mode=DELETE; \
                 CREATE TABLE proof(value TEXT); \
                 INSERT INTO proof VALUES('preserved');",
            )
            .unwrap();
    }
    let bytes = read_bounded(&path);
    assert_eq!(&bytes[18..20], &[1, 1], "fixture is not in rollback format");
    bytes
}

#[test]
fn wal2_commit_checkpoint_close_and_reopen() {
    let (scratch, source_bytes) = wal2_source("stage-wal-commit");
    let fs = SimFs::new();
    let (main, journal, statement, wal) = caps4(&fs);
    stage_main(main.as_ref(), &source_bytes);

    let conn = ConfinedSqlite::open_wal(
        Arc::clone(&main),
        Arc::clone(&journal),
        Arc::clone(&statement),
        Arc::clone(&wal),
    )
    .unwrap();
    {
        let db = conn.connection();
        let locking: String = db
            .query_row("PRAGMA locking_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(locking, "exclusive");
        let mode: String = db
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .unwrap();
        assert_eq!(mode, "wal");
    }
    conn.connection()
        .execute_batch("INSERT INTO proof VALUES('new-in-wal');")
        .unwrap();
    assert_eq!(
        read_all(main.as_ref()),
        source_bytes,
        "commit wrote the main image before checkpoint"
    );
    assert!(wal.len().unwrap() > 0, "commit did not write the WAL");
    conn.checkpoint_wal().unwrap();
    assert_eq!(wal.len().unwrap(), 0, "checkpoint did not truncate the WAL");
    assert_ne!(
        read_all(main.as_ref()),
        source_bytes,
        "checkpoint did not write the main image"
    );
    conn.close().unwrap();

    assert_eq!(wal.len().unwrap(), 0);
    assert_eq!(journal.len().unwrap(), 0);
    assert_eq!(statement.len().unwrap(), 0);
    assert_header2(main.as_ref());
    assert_eq!(
        read_bounded(&scratch.path().join("source.db")),
        source_bytes,
        "the source image changed"
    );

    let conn = ConfinedSqlite::open_wal(
        Arc::clone(&main),
        Arc::clone(&journal),
        Arc::clone(&statement),
        Arc::clone(&wal),
    )
    .unwrap();
    let values: Vec<String> = conn
        .connection()
        .prepare("SELECT value FROM proof ORDER BY value")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    assert_eq!(
        values,
        vec!["new-in-wal".to_string(), "preserved".to_string()]
    );
    conn.checkpoint_wal().unwrap();
    conn.close().unwrap();
    assert_eq!(wal.len().unwrap(), 0);
    assert_header2(main.as_ref());
}

#[test]
fn rejects_empty_rollback_and_nonempty_auxiliary_caps() {
    let (_, source_bytes) = wal2_source("stage-wal-preconditions");
    let rollback_bytes = rollback_source("stage-wal-rollback");

    let fs = SimFs::new();
    let (main, journal, statement, wal) = caps4(&fs);
    assert_cannot_open(assert_open_wal_error(&main, &journal, &statement, &wal));
    assert_eq!(main.len().unwrap(), 0);
    assert_eq!(wal.len().unwrap(), 0);

    let fs = SimFs::new();
    let (main, journal, statement, wal) = caps4(&fs);
    stage_main(main.as_ref(), &rollback_bytes);
    assert_cannot_open(assert_open_wal_error(&main, &journal, &statement, &wal));
    assert_eq!(read_all(main.as_ref()), rollback_bytes);
    assert_eq!(wal.len().unwrap(), 0);

    let fs = SimFs::new();
    let (main, journal, statement, wal) = caps4(&fs);
    stage_main(main.as_ref(), &source_bytes);
    wal.write_at(b"x", 0).unwrap();
    assert_cannot_open(assert_open_wal_error(&main, &journal, &statement, &wal));
    assert_eq!(wal.len().unwrap(), 1);

    let fs = SimFs::new();
    let (main, journal, statement, wal) = caps4(&fs);
    stage_main(main.as_ref(), &source_bytes);
    journal.write_at(b"x", 0).unwrap();
    assert_cannot_open(assert_open_wal_error(&main, &journal, &statement, &wal));
    assert_eq!(journal.len().unwrap(), 1);

    let fs = SimFs::new();
    let (main, journal, statement, wal) = caps4(&fs);
    stage_main(main.as_ref(), &source_bytes);
    statement.write_at(b"x", 0).unwrap();
    assert_cannot_open(assert_open_wal_error(&main, &journal, &statement, &wal));
    assert_eq!(statement.len().unwrap(), 1);
}

#[test]
fn rejects_aliased_wal_capabilities() {
    let (_, source_bytes) = wal2_source("stage-wal-aliases");
    let fs = SimFs::new();
    for (dir, alias) in [
        ("/stage-a", 0),
        ("/stage-b", 1),
        ("/stage-c", 2),
        ("/stage-d", 3),
        ("/stage-e", 4),
        ("/stage-f", 5),
    ] {
        let (main, journal, statement, wal) = caps4_in(&fs, dir);
        stage_main(main.as_ref(), &source_bytes);
        let error = match alias {
            0 => assert_open_wal_error(&main, &main, &statement, &wal),
            1 => assert_open_wal_error(&main, &journal, &main, &wal),
            2 => assert_open_wal_error(&main, &journal, &statement, &main),
            3 => assert_open_wal_error(&main, &journal, &journal, &wal),
            4 => assert_open_wal_error(&main, &journal, &statement, &journal),
            _ => assert_open_wal_error(&main, &journal, &statement, &statement),
        };
        assert_cannot_open(error);
        assert_eq!(read_all(main.as_ref()), source_bytes);
        assert_eq!(wal.len().unwrap(), 0);
    }
}

#[test]
fn wal_receipt_invalidated_by_borrow_and_restored_by_recheckpoint() {
    let (_, source_bytes) = wal2_source("stage-wal-receipt");
    let fs = SimFs::new();
    let (main, journal, statement, wal) = caps4(&fs);
    stage_main(main.as_ref(), &source_bytes);
    let conn = ConfinedSqlite::open_wal(
        Arc::clone(&main),
        Arc::clone(&journal),
        Arc::clone(&statement),
        Arc::clone(&wal),
    )
    .unwrap();
    conn.connection()
        .execute_batch("INSERT INTO proof VALUES('checkpointed');")
        .unwrap();
    conn.checkpoint_wal().unwrap();

    let context = Arc::clone(conn.context());
    let name = conn.vfs_name().to_owned();
    {
        let db = conn.connection();
        let value: String = db
            .query_row(
                "SELECT value FROM proof WHERE value='checkpointed'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(value, "checkpointed");
    }
    assert!(
        matches!(conn.close(), Err(CloseError::WalNotCheckpointed)),
        "close accepted a receipt invalidated by borrowing"
    );
    assert_cleanup(&context, &name);

    let conn = ConfinedSqlite::open_wal(
        Arc::clone(&main),
        Arc::clone(&journal),
        Arc::clone(&statement),
        Arc::clone(&wal),
    )
    .unwrap();
    conn.checkpoint_wal().unwrap();
    let _ = conn.connection();
    conn.checkpoint_wal().unwrap();
    conn.close().unwrap();
    assert_eq!(wal.len().unwrap(), 0);
    assert_header2(main.as_ref());
}

#[test]
fn wal_write_after_checkpoint_is_rejected_by_close() {
    let (_, source_bytes) = wal2_source("stage-wal-retained-borrow");
    let fs = SimFs::new();
    let (main, journal, statement, wal) = caps4(&fs);
    stage_main(main.as_ref(), &source_bytes);
    let conn = ConfinedSqlite::open_wal(
        Arc::clone(&main),
        Arc::clone(&journal),
        Arc::clone(&statement),
        Arc::clone(&wal),
    )
    .unwrap();
    let borrow = conn.connection();
    borrow
        .execute_batch("INSERT INTO proof VALUES('first');")
        .unwrap();
    conn.checkpoint_wal().unwrap();
    assert_eq!(wal.len().unwrap(), 0);
    borrow
        .execute_batch("INSERT INTO proof VALUES('second');")
        .unwrap();
    assert!(
        wal.len().unwrap() > 0,
        "the retained borrow did not write the WAL"
    );
    let context = Arc::clone(conn.context());
    let name = conn.vfs_name().to_owned();
    assert!(
        matches!(conn.close(), Err(CloseError::WalNotCheckpointed)),
        "close accepted a WAL written after the checkpoint"
    );
    assert_cleanup(&context, &name);
    assert_eq!(wal.len().unwrap(), 0);
    assert_header2(main.as_ref());

    let conn = ConfinedSqlite::open_wal(
        Arc::clone(&main),
        Arc::clone(&journal),
        Arc::clone(&statement),
        Arc::clone(&wal),
    )
    .unwrap();
    let values: Vec<String> = conn
        .connection()
        .prepare("SELECT value FROM proof ORDER BY value")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    assert_eq!(
        values,
        vec![
            "first".to_string(),
            "preserved".to_string(),
            "second".to_string()
        ]
    );
    conn.checkpoint_wal().unwrap();
    conn.close().unwrap();
}

#[test]
fn close_without_checkpoint_fails_even_though_sqlite_auto_checkpoints() {
    let (_, source_bytes) = wal2_source("stage-wal-no-checkpoint");
    let fs = SimFs::new();
    let (main, journal, statement, wal) = caps4(&fs);
    stage_main(main.as_ref(), &source_bytes);
    let conn = ConfinedSqlite::open_wal(
        Arc::clone(&main),
        Arc::clone(&journal),
        Arc::clone(&statement),
        Arc::clone(&wal),
    )
    .unwrap();
    conn.connection()
        .execute_batch("INSERT INTO proof VALUES('durable-but-ineligible');")
        .unwrap();
    assert!(wal.len().unwrap() > 0);
    let context = Arc::clone(conn.context());
    let name = conn.vfs_name().to_owned();
    assert!(
        matches!(conn.close(), Err(CloseError::WalNotCheckpointed)),
        "close without an explicit checkpoint receipt succeeded"
    );
    assert_cleanup(&context, &name);
    assert_eq!(wal.len().unwrap(), 0);
    assert_header2(main.as_ref());

    let conn = ConfinedSqlite::open_wal(
        Arc::clone(&main),
        Arc::clone(&journal),
        Arc::clone(&statement),
        Arc::clone(&wal),
    )
    .unwrap();
    let value: String = conn
        .connection()
        .query_row(
            "SELECT value FROM proof WHERE value='durable-but-ineligible'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(value, "durable-but-ineligible");
    conn.checkpoint_wal().unwrap();
    conn.close().unwrap();
}

#[test]
fn unfinished_transaction_and_cancelled_checkpoint_are_rejected() {
    let (_, source_bytes) = wal2_source("stage-wal-unfinished");

    let fs = SimFs::new();
    let (main, journal, statement, wal) = caps4(&fs);
    stage_main(main.as_ref(), &source_bytes);
    let conn = ConfinedSqlite::open_wal(
        Arc::clone(&main),
        Arc::clone(&journal),
        Arc::clone(&statement),
        Arc::clone(&wal),
    )
    .unwrap();
    conn.connection()
        .execute_batch("BEGIN; INSERT INTO proof VALUES('pending');")
        .unwrap();
    assert!(
        matches!(
            conn.checkpoint_wal(),
            Err(CloseError::UncommittedTransaction)
        ),
        "checkpoint accepted an open transaction"
    );
    let context = Arc::clone(conn.context());
    let name = conn.vfs_name().to_owned();
    assert!(
        matches!(conn.close(), Err(CloseError::UncommittedTransaction)),
        "close reported an ineligibility other than the open transaction"
    );
    assert_cleanup(&context, &name);
    assert_eq!(wal.len().unwrap(), 0);
    assert_header2(main.as_ref());
    let conn = ConfinedSqlite::open_wal(
        Arc::clone(&main),
        Arc::clone(&journal),
        Arc::clone(&statement),
        Arc::clone(&wal),
    )
    .unwrap();
    let pending: i64 = conn
        .connection()
        .query_row(
            "SELECT count(*) FROM proof WHERE value='pending'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(pending, 0, "the unfinished transaction was not rolled back");
    conn.checkpoint_wal().unwrap();
    conn.close().unwrap();

    let fs = SimFs::new();
    let (main, journal, statement, wal) = caps4(&fs);
    stage_main(main.as_ref(), &source_bytes);
    let conn = ConfinedSqlite::open_wal(
        Arc::clone(&main),
        Arc::clone(&journal),
        Arc::clone(&statement),
        Arc::clone(&wal),
    )
    .unwrap();
    conn.connection()
        .execute_batch("INSERT INTO proof VALUES('kept');")
        .unwrap();
    conn.connection()
        .progress_handler(1, Some(|| true))
        .unwrap();
    assert!(
        conn.checkpoint_wal().is_err(),
        "checkpoint ignored the caller's cancellation guard"
    );
    conn.connection()
        .progress_handler(0, None::<fn() -> bool>)
        .unwrap();
    let context = Arc::clone(conn.context());
    let name = conn.vfs_name().to_owned();
    assert!(
        matches!(conn.close(), Err(CloseError::WalNotCheckpointed)),
        "a cancelled checkpoint became qualified output"
    );
    assert_cleanup(&context, &name);
    assert_eq!(wal.len().unwrap(), 0);
    assert_header2(main.as_ref());
}

#[test]
fn malformed_schema_opens_without_loading_and_checkpoint_refuses() {
    let (_scratch, source_bytes) = malformed_wal2_source("stage-wal-malformed");
    let fs = SimFs::new();
    let (main, journal, statement, wal) = caps4(&fs);
    stage_main(main.as_ref(), &source_bytes);
    let conn = ConfinedSqlite::open_wal(
        Arc::clone(&main),
        Arc::clone(&journal),
        Arc::clone(&statement),
        Arc::clone(&wal),
    )
    .unwrap();
    let locking: String = conn
        .connection()
        .query_row("PRAGMA locking_mode", [], |row| row.get(0))
        .unwrap();
    assert_eq!(locking, "exclusive");
    assert!(
        matches!(conn.checkpoint_wal(), Err(CloseError::Sqlite(_))),
        "checkpoint did not verify journal_mode through the schema"
    );
    let context = Arc::clone(conn.context());
    let name = conn.vfs_name().to_owned();
    assert!(
        matches!(conn.close(), Err(CloseError::WalNotCheckpointed)),
        "a refused checkpoint became qualified output"
    );
    assert_cleanup(&context, &name);
    assert_eq!(wal.len().unwrap(), 0);
    assert_eq!(read_all(main.as_ref()), source_bytes);
}

#[test]
fn wal_write_fault_is_contained_and_rejects_close() {
    let (_, source_bytes) = wal2_source("stage-wal-write-fault");
    let fs = SimFs::new();
    let (main, journal, statement, wal) = caps4(&fs);
    stage_main(main.as_ref(), &source_bytes);
    let probe = Arc::new(Probe::new(Arc::clone(&wal)));
    let probe_cap: Arc<dyn ReadWriteFile> = probe.clone();
    let conn = ConfinedSqlite::open_wal(
        Arc::clone(&main),
        Arc::clone(&journal),
        Arc::clone(&statement),
        probe_cap,
    )
    .unwrap();
    probe.fail_writes.store(true, Ordering::SeqCst);
    let _ = conn
        .connection()
        .execute_batch("INSERT INTO proof VALUES('x');");
    assert!(
        !probe.fail_writes.load(Ordering::SeqCst),
        "the injected write fault never fired"
    );
    let context = Arc::clone(conn.context());
    let name = conn.vfs_name().to_owned();
    assert!(
        matches!(conn.close(), Err(CloseError::IoError)),
        "a latched WAL write fault became a successful close"
    );
    assert_cleanup(&context, &name);
}

#[test]
fn wal_read_panic_is_contained_and_rejects_close() {
    let (_, source_bytes) = wal2_source("stage-wal-read-panic");
    let fs = SimFs::new();
    let (main, journal, statement, wal) = caps4(&fs);
    stage_main(main.as_ref(), &source_bytes);
    let probe = Arc::new(Probe::new(Arc::clone(&wal)));
    let probe_cap: Arc<dyn ReadWriteFile> = probe.clone();
    let conn = ConfinedSqlite::open_wal(
        Arc::clone(&main),
        Arc::clone(&journal),
        Arc::clone(&statement),
        probe_cap,
    )
    .unwrap();
    conn.connection()
        .execute_batch("INSERT INTO proof VALUES('x');")
        .unwrap();
    probe.fail_reads.store(true, Ordering::SeqCst);
    // The just-written page remains cached for SELECT. A real checkpoint
    // must read its WAL frame, so the fault cannot pass vacuously.
    let result = conn.checkpoint_wal();
    assert!(result.is_err(), "the injected read panic never fired");
    assert!(
        !probe.fail_reads.load(Ordering::SeqCst),
        "the read fault was not consumed"
    );
    let context = Arc::clone(conn.context());
    let name = conn.vfs_name().to_owned();
    assert!(
        matches!(conn.close(), Err(CloseError::IoError)),
        "a latched WAL read panic became a successful close"
    );
    assert_cleanup(&context, &name);
}

#[test]
fn wal_sync_panic_rejects_commit_and_close() {
    let (_, source_bytes) = wal2_source("stage-wal-sync-panic");
    let fs = SimFs::new();
    let (main, journal, statement, wal) = caps4(&fs);
    stage_main(main.as_ref(), &source_bytes);
    let probe = Arc::new(Probe::new(Arc::clone(&wal)));
    let probe_cap: Arc<dyn ReadWriteFile> = probe.clone();
    let conn = ConfinedSqlite::open_wal(
        Arc::clone(&main),
        Arc::clone(&journal),
        Arc::clone(&statement),
        probe_cap,
    )
    .unwrap();
    probe.fail_sync.store(true, Ordering::SeqCst);
    let _ = conn
        .connection()
        .execute_batch("INSERT INTO proof VALUES('x');");
    assert!(
        !probe.fail_sync.load(Ordering::SeqCst),
        "the injected sync panic never fired"
    );
    let context = Arc::clone(conn.context());
    let name = conn.vfs_name().to_owned();
    assert!(
        matches!(conn.close(), Err(CloseError::IoError)),
        "a latched WAL sync panic became a successful close"
    );
    assert_cleanup(&context, &name);
}

#[test]
fn wal_truncate_fault_rejects_checkpoint_and_close() {
    let (_, source_bytes) = wal2_source("stage-wal-truncate-fault");
    let fs = SimFs::new();
    let (main, journal, statement, wal) = caps4(&fs);
    stage_main(main.as_ref(), &source_bytes);
    let probe = Arc::new(Probe::new(Arc::clone(&wal)));
    let probe_cap: Arc<dyn ReadWriteFile> = probe.clone();
    let conn = ConfinedSqlite::open_wal(
        Arc::clone(&main),
        Arc::clone(&journal),
        Arc::clone(&statement),
        probe_cap,
    )
    .unwrap();
    conn.connection()
        .execute_batch("INSERT INTO proof VALUES('x');")
        .unwrap();
    probe.fail_truncate.store(true, Ordering::SeqCst);
    let _ = conn.checkpoint_wal();
    assert!(
        !probe.fail_truncate.load(Ordering::SeqCst),
        "the injected truncate fault never fired"
    );
    let context = Arc::clone(conn.context());
    let name = conn.vfs_name().to_owned();
    assert!(
        matches!(conn.close(), Err(CloseError::IoError)),
        "a latched WAL truncate fault became a successful close"
    );
    assert_cleanup(&context, &name);
}

#[test]
fn wal_close_delete_truncate_fault_is_latched() {
    let (_, source_bytes) = wal2_source("stage-wal-close-truncate");
    let fs = SimFs::new();
    let (main, journal, statement, wal) = caps4(&fs);
    stage_main(main.as_ref(), &source_bytes);
    let probe = Arc::new(Probe::new(Arc::clone(&wal)));
    let probe_cap: Arc<dyn ReadWriteFile> = probe.clone();
    let conn = ConfinedSqlite::open_wal(
        Arc::clone(&main),
        Arc::clone(&journal),
        Arc::clone(&statement),
        probe_cap,
    )
    .unwrap();
    conn.connection()
        .execute_batch("INSERT INTO proof VALUES('x');")
        .unwrap();
    conn.checkpoint_wal().unwrap();
    probe.fail_truncate.store(true, Ordering::SeqCst);
    let context = Arc::clone(conn.context());
    let name = conn.vfs_name().to_owned();
    assert!(
        matches!(conn.close(), Err(CloseError::IoError)),
        "a close-time WAL delete failure became a successful close"
    );
    assert!(
        !probe.fail_truncate.load(Ordering::SeqCst),
        "the close-time truncate fault never fired"
    );
    assert_cleanup(&context, &name);
    assert_eq!(wal.len().unwrap(), 0);
}

#[test]
fn wal_short_and_interrupted_reads_retry_without_false_failure() {
    let (_, source_bytes) = wal2_source("stage-wal-short-reads");
    let fs = SimFs::new();
    let (main, journal, statement, wal) = caps4(&fs);
    stage_main(main.as_ref(), &source_bytes);
    let probe = Arc::new(Probe::new(Arc::clone(&wal)));
    let probe_cap: Arc<dyn ReadWriteFile> = probe.clone();
    let conn = ConfinedSqlite::open_wal(
        Arc::clone(&main),
        Arc::clone(&journal),
        Arc::clone(&statement),
        probe_cap,
    )
    .unwrap();
    probe.short_reads.store(true, Ordering::SeqCst);
    probe.interrupt_read.store(true, Ordering::SeqCst);
    conn.connection()
        .execute_batch("INSERT INTO proof VALUES('x');")
        .unwrap();
    conn.checkpoint_wal().unwrap();
    assert!(
        !probe.interrupt_read.load(Ordering::SeqCst),
        "the interrupted read was not consumed"
    );
    assert!(
        probe.short_reads.load(Ordering::SeqCst),
        "the short-read mode was not exercised"
    );
    conn.close().unwrap();
    assert_eq!(wal.len().unwrap(), 0);
    assert_header2(main.as_ref());
}

#[test]
fn refuses_second_main_attach_and_unconfined_paths() {
    let (_, source_bytes) = wal2_source("stage-wal-refusals");
    let scratch = ScratchDir::new("stage-wal-attach-escape");
    let fs = SimFs::new();
    let (main, journal, statement, wal) = caps4(&fs);
    stage_main(main.as_ref(), &source_bytes);
    let mut conn = ConfinedSqlite::open_wal(
        Arc::clone(&main),
        Arc::clone(&journal),
        Arc::clone(&statement),
        Arc::clone(&wal),
    )
    .unwrap();
    let context = conn.context();
    let main_name = context.main_name.to_str().unwrap().to_string();
    let journal_name = context.journal_name.to_str().unwrap().to_string();
    let wal_name = context
        .wal
        .as_ref()
        .expect("WAL role")
        .name
        .to_str()
        .unwrap()
        .to_string();
    assert_eq!(conn.access(&wal_name), 1);
    assert_eq!(conn.access(&journal_name), 0);
    assert_eq!(conn.access("unknown.db"), 0);

    let second = Connection::open_with_flags_and_vfs(
        main_name.as_str(),
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        conn.vfs_name(),
    );
    assert!(second.is_err(), "a second main connection was accepted");

    let escaped = scratch.path().join("must-not-create.db");
    let native_vfs = default_vfs().unwrap();
    let native_name = unsafe { std::ffi::CStr::from_ptr(native_vfs.zName) }
        .to_str()
        .unwrap();
    let uri = format!("file:{}?vfs={native_name}", escaped.display());
    for name in ["other.db", "file:other.db?mode=rwc", &uri] {
        for sql in ["ATTACH DATABASE ?1 AS escaped", "VACUUM main INTO ?1"] {
            assert!(
                conn.connection().execute(sql, [name]).is_err(),
                "{sql} escaped confinement"
            );
        }
    }
    assert!(!escaped.exists());

    let vfs = &mut conn.vfs.as_mut().unwrap().base;
    let wrong = CString::new("wrong").unwrap();
    for (name, flags) in [
        (
            wrong.as_ptr(),
            ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_MAIN_DB,
        ),
        (
            wrong.as_ptr(),
            ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_MAIN_JOURNAL,
        ),
        (
            wrong.as_ptr(),
            ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_WAL,
        ),
        (
            wrong.as_ptr(),
            ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_SUBJOURNAL,
        ),
        (
            wrong.as_ptr(),
            ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_TEMP_DB,
        ),
        (
            wrong.as_ptr(),
            ffi::SQLITE_OPEN_READONLY | ffi::SQLITE_OPEN_MAIN_DB,
        ),
    ] {
        let mut file = std::mem::MaybeUninit::<StageFile>::zeroed();
        let pointer = file.as_mut_ptr().cast::<ffi::sqlite3_file>();
        let rc = unsafe { vfs.xOpen.unwrap()(vfs, name, pointer, flags, std::ptr::null_mut()) };
        if rc == ffi::SQLITE_OK {
            unsafe { close(pointer) };
        }
        assert_eq!(rc, ffi::SQLITE_CANTOPEN);
    }
    // The statement-journal role is still wired for its NULL open.
    let mut file = std::mem::MaybeUninit::<StageFile>::zeroed();
    let pointer = file.as_mut_ptr().cast::<ffi::sqlite3_file>();
    let rc = unsafe {
        vfs.xOpen.unwrap()(
            vfs,
            std::ptr::null(),
            pointer,
            ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_SUBJOURNAL,
            std::ptr::null_mut(),
        )
    };
    assert_eq!(rc, ffi::SQLITE_OK);
    unsafe { close(pointer) };

    conn.checkpoint_wal().unwrap();
    conn.close().unwrap();
}

#[test]
fn retained_four_inodes_survive_directory_and_file_replacement() {
    let (_, source_bytes) = wal2_source("stage-wal-replacement");
    let scratch = ScratchDir::new("stage-wal-replacement-real");
    let fs = real_fs();
    let original = scratch.path().join("stage");
    std::fs::create_dir(&original).unwrap();
    let root = fs.open_confined_dir(&original).unwrap();
    let main: Arc<dyn ReadWriteFile> = Arc::from(root.create_read_write(Path::new("db")).unwrap());
    let journal: Arc<dyn ReadWriteFile> =
        Arc::from(root.create_read_write(Path::new("journal")).unwrap());
    let statement: Arc<dyn ReadWriteFile> =
        Arc::from(root.create_read_write(Path::new("stmt")).unwrap());
    let wal: Arc<dyn ReadWriteFile> = Arc::from(root.create_read_write(Path::new("wal")).unwrap());
    let main_id = main.identity().unwrap();
    let journal_id = journal.identity().unwrap();
    let statement_id = statement.identity().unwrap();
    let wal_id = wal.identity().unwrap();
    stage_main(main.as_ref(), &source_bytes);
    let conn = ConfinedSqlite::open_wal(
        Arc::clone(&main),
        Arc::clone(&journal),
        Arc::clone(&statement),
        Arc::clone(&wal),
    )
    .unwrap();
    conn.connection()
        .execute_batch("INSERT INTO proof VALUES('retained');")
        .unwrap();

    let moved = scratch.path().join("moved");
    std::fs::rename(&original, &moved).unwrap();
    std::fs::create_dir(&original).unwrap();
    std::fs::write(original.join("db"), b"decoy db").unwrap();
    std::fs::write(original.join("journal"), b"decoy journal").unwrap();
    std::fs::write(original.join("stmt"), b"decoy stmt").unwrap();
    std::fs::write(original.join("wal"), b"decoy wal").unwrap();
    std::fs::rename(moved.join("db"), moved.join("retained-db")).unwrap();
    std::fs::rename(moved.join("journal"), moved.join("retained-journal")).unwrap();
    std::fs::rename(moved.join("stmt"), moved.join("retained-stmt")).unwrap();
    std::fs::rename(moved.join("wal"), moved.join("retained-wal")).unwrap();
    std::fs::write(moved.join("db"), b"decoy moved db").unwrap();
    std::fs::write(moved.join("journal"), b"decoy moved journal").unwrap();
    std::fs::write(moved.join("stmt"), b"decoy moved stmt").unwrap();
    std::fs::write(moved.join("wal"), b"decoy moved wal").unwrap();

    conn.checkpoint_wal().unwrap();
    conn.close().unwrap();

    assert_eq!(main.identity().unwrap(), main_id);
    assert_eq!(journal.identity().unwrap(), journal_id);
    assert_eq!(statement.identity().unwrap(), statement_id);
    assert_eq!(wal.identity().unwrap(), wal_id);
    assert_eq!(std::fs::read(original.join("db")).unwrap(), b"decoy db");
    assert_eq!(
        std::fs::read(original.join("journal")).unwrap(),
        b"decoy journal"
    );
    assert_eq!(std::fs::read(original.join("stmt")).unwrap(), b"decoy stmt");
    assert_eq!(std::fs::read(original.join("wal")).unwrap(), b"decoy wal");
    assert_eq!(std::fs::read(moved.join("db")).unwrap(), b"decoy moved db");
    assert_eq!(
        std::fs::read(moved.join("journal")).unwrap(),
        b"decoy moved journal"
    );
    assert_eq!(
        std::fs::read(moved.join("stmt")).unwrap(),
        b"decoy moved stmt"
    );
    assert_eq!(
        std::fs::read(moved.join("wal")).unwrap(),
        b"decoy moved wal"
    );
    let retained = std::fs::read(moved.join("retained-db")).unwrap();
    assert!(retained.len() > 512);
    assert_eq!(
        std::fs::read(moved.join("retained-wal")).unwrap(),
        Vec::<u8>::new()
    );

    let reopened = ConfinedSqlite::open_wal(
        Arc::clone(&main),
        Arc::clone(&journal),
        Arc::clone(&statement),
        Arc::clone(&wal),
    )
    .unwrap();
    let values: Vec<String> = reopened
        .connection()
        .prepare("SELECT value FROM proof ORDER BY value")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .map(|row| row.unwrap())
        .collect();
    assert_eq!(
        values,
        vec!["preserved".to_string(), "retained".to_string()]
    );
    reopened.checkpoint_wal().unwrap();
    reopened.close().unwrap();
}

#[test]
fn registered_io_methods_stay_v1_without_shm() {
    assert_eq!(METHODS.iVersion, 1);
    assert!(METHODS.xShmMap.is_none());
    assert!(METHODS.xShmLock.is_none());
    assert!(METHODS.xShmBarrier.is_none());
    assert!(METHODS.xShmUnmap.is_none());
    assert!(METHODS.xFetch.is_none());
    assert!(METHODS.xUnfetch.is_none());
}

#[test]
fn original_open_still_refuses_wal2_without_sql() {
    let (_, source_bytes) = wal2_source("stage-wal-original-open");
    let fs = SimFs::new();
    let (main, journal, statement) = caps(&fs);
    stage_main(main.as_ref(), &source_bytes);
    let error = ConfinedSqlite::open(
        Arc::clone(&main),
        Arc::clone(&journal),
        Arc::clone(&statement),
    )
    .unwrap_err();
    match error {
        rusqlite::Error::SqliteFailure(
            ffi::Error {
                code: ffi::ErrorCode::CannotOpen,
                ..
            },
            Some(message),
        ) => assert!(message.contains("WAL"), "{message}"),
        other => panic!("unexpected error: {other:?}"),
    }
    assert_eq!(read_all(main.as_ref()), source_bytes);
    assert_eq!(journal.len().unwrap(), 0);
    assert_eq!(statement.len().unwrap(), 0);
}
