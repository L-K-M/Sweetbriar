//! Native proof for the confined writable VFS (M1.8e3b2b3): real
//! rollback/commit and reopen, statement-journal spill with statement
//! rollback, refused opens and aliases, retained-inode replacement,
//! injected faults/panics and resource lifetimes.

use std::io;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use rusqlite::{Connection, OpenFlags, ffi};
use sb_test_support::{ScratchDir, SimFs, real_fs};
use sb_types::fs::{FileIdentity, Fs, ReadWriteFile};

use super::{CloseError, ConfinedSqlite};

fn caps(
    fs: &SimFs,
) -> (
    Arc<dyn ReadWriteFile>,
    Arc<dyn ReadWriteFile>,
    Arc<dyn ReadWriteFile>,
) {
    fs.create_dir_all(Path::new("/stage")).unwrap();
    let root = fs.open_confined_dir(Path::new("/stage")).unwrap();
    let main: Arc<dyn ReadWriteFile> = Arc::from(root.create_read_write(Path::new("db")).unwrap());
    let journal: Arc<dyn ReadWriteFile> =
        Arc::from(root.create_read_write(Path::new("journal")).unwrap());
    let statement: Arc<dyn ReadWriteFile> =
        Arc::from(root.create_read_write(Path::new("stmt")).unwrap());
    (main, journal, statement)
}

/// A retained capability with independent one-shot faults and observable
/// writes/flushes. All tests exercise the same forwarding implementation.
struct Probe {
    inner: Arc<dyn ReadWriteFile>,
    writes: AtomicUsize,
    max_len: AtomicUsize,
    syncs: AtomicUsize,
    fail_writes: AtomicBool,
    fail_truncate: AtomicBool,
    fail_reads: AtomicBool,
    fail_sync: AtomicBool,
    short_reads: AtomicBool,
    interrupt_read: AtomicBool,
}

impl Probe {
    fn new(inner: Arc<dyn ReadWriteFile>) -> Self {
        Self {
            inner,
            writes: AtomicUsize::new(0),
            max_len: AtomicUsize::new(0),
            syncs: AtomicUsize::new(0),
            fail_writes: AtomicBool::new(false),
            fail_truncate: AtomicBool::new(false),
            fail_reads: AtomicBool::new(false),
            fail_sync: AtomicBool::new(false),
            short_reads: AtomicBool::new(false),
            interrupt_read: AtomicBool::new(false),
        }
    }
}

impl ReadWriteFile for Probe {
    fn read_at(&self, bytes: &mut [u8], offset: u64) -> io::Result<usize> {
        assert!(
            !self.fail_reads.swap(false, Ordering::SeqCst),
            "injected read panic"
        );
        if self.interrupt_read.swap(false, Ordering::SeqCst) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let count = if self.short_reads.load(Ordering::SeqCst) {
            bytes.len().min(3)
        } else {
            bytes.len()
        };
        self.inner.read_at(&mut bytes[..count], offset)
    }
    fn write_at(&self, bytes: &[u8], offset: u64) -> io::Result<usize> {
        if self.fail_writes.swap(false, Ordering::SeqCst) {
            return Err(io::Error::other("injected write failure"));
        }
        let written = self.inner.write_at(bytes, offset)?;
        self.writes.fetch_add(1, Ordering::SeqCst);
        let end = usize::try_from(offset.saturating_add(written as u64)).unwrap_or(usize::MAX);
        self.max_len.fetch_max(end, Ordering::SeqCst);
        Ok(written)
    }
    fn set_len(&self, len: u64) -> io::Result<()> {
        if self.fail_truncate.swap(false, Ordering::SeqCst) {
            return Err(io::Error::other("injected truncate failure"));
        }
        self.inner.set_len(len)
    }
    fn len(&self) -> io::Result<u64> {
        self.inner.len()
    }
    fn sync_all(&self) -> io::Result<()> {
        assert!(
            !self.fail_sync.swap(false, Ordering::SeqCst),
            "injected sync panic"
        );
        self.syncs.fetch_add(1, Ordering::SeqCst);
        self.inner.sync_all()
    }
    fn identity(&self) -> io::Result<FileIdentity> {
        self.inner.identity()
    }
}

#[test]
fn commits_and_reopens_through_retained_files() {
    let fs = SimFs::new();
    let (main, journal, statement) = caps(&fs);
    {
        let conn = ConfinedSqlite::open(
            Arc::clone(&main),
            Arc::clone(&journal),
            Arc::clone(&statement),
        )
        .unwrap();
        // The staged image is untrusted: SQLite's default depth.
        assert_eq!(
            conn.connection()
                .limit(rusqlite::limits::Limit::SQLITE_LIMIT_EXPR_DEPTH)
                .unwrap(),
            crate::image_vfs::UNTRUSTED_EXPR_DEPTH
        );
        // Open runs no SQL: a fresh retained database is still empty.
        let tables: i64 = conn
            .connection()
            .query_row("SELECT count(*) FROM sqlite_master", [], |row| row.get(0))
            .unwrap();
        assert_eq!(tables, 0);
        conn.connection()
            .execute_batch("CREATE TABLE proof(value TEXT); INSERT INTO proof VALUES('kept');")
            .unwrap();
        conn.close().unwrap();
    }
    assert_eq!(journal.len().unwrap(), 0);
    assert_eq!(statement.len().unwrap(), 0);
    let mut header = [0u8; 20];
    assert_eq!(main.read_at(&mut header, 0).unwrap(), 20);
    assert_eq!(&header[18..20], &[1, 1]);
    {
        let conn = ConfinedSqlite::open(
            Arc::clone(&main),
            Arc::clone(&journal),
            Arc::clone(&statement),
        )
        .unwrap();
        let value: String = conn
            .connection()
            .query_row("SELECT value FROM proof", [], |row| row.get(0))
            .unwrap();
        assert_eq!(value, "kept");
        conn.close().unwrap();
    }
}

#[test]
fn statement_journal_spills_and_statement_rollback_keeps_previous_statement() {
    let fs = SimFs::new();
    let (main, journal, statement) = caps(&fs);
    let recording = Arc::new(Probe::new(Arc::clone(&statement)));
    let recorder: Arc<dyn ReadWriteFile> = recording.clone();
    let conn = ConfinedSqlite::open(Arc::clone(&main), Arc::clone(&journal), recorder).unwrap();
    let db = conn.connection();
    // Select FILE explicitly so a >64 KiB statement journal reaches
    // our VFS regardless of the bundled build's temp-storage default.
    db.execute_batch("PRAGMA temp_store=FILE;").unwrap();
    db.execute_batch(
        "CREATE TABLE t(id INTEGER PRIMARY KEY, v TEXT CHECK(length(v) < 200)); \
         WITH RECURSIVE seq(value) AS \
         (SELECT 1 UNION ALL SELECT value+1 FROM seq WHERE value < 20000) \
         INSERT INTO t(id, v) SELECT value, printf('x%09d', value) FROM seq;",
    )
    .unwrap();
    // Dirty the whole table in the preceding statement. Otherwise the
    // main journal already contains the savepoint's page images and
    // SQLite needs almost no statement journal for the failing update.
    db.execute_batch("BEGIN; UPDATE t SET v = printf('p%09d', id);")
        .unwrap();
    db.execute("INSERT INTO t(id, v) VALUES(0, 'keep');", [])
        .unwrap();
    // The statement journal is only consulted while a savepoint is open.
    db.execute_batch("SAVEPOINT s;").unwrap();
    let error = db
        .execute_batch(
            "UPDATE t SET v = CASE WHEN id = 20000 \
             THEN printf('y%0999d', id) ELSE printf('y%09d', id) END;",
        )
        .unwrap_err();
    assert!(format!("{error:?}").contains("CHECK"), "{error:?}");
    let changed: i64 = db
        .query_row("SELECT count(*) FROM t WHERE v LIKE 'y%'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(changed, 0);
    let preceding: i64 = db
        .query_row("SELECT count(*) FROM t WHERE v LIKE 'p%'", [], |r| r.get(0))
        .unwrap();
    assert_eq!(preceding, 20000);
    let kept: i64 = db
        .query_row("SELECT count(*) FROM t WHERE v = 'keep'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(kept, 1);
    db.execute_batch("ROLLBACK TO s;").unwrap();
    db.execute_batch("COMMIT;").unwrap();
    let changed: i64 = db
        .query_row("SELECT count(*) FROM t WHERE v LIKE 'y%'", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(changed, 0);
    conn.close().unwrap();
    assert!(recording.writes.load(Ordering::SeqCst) > 0);
    assert!(recording.max_len.load(Ordering::SeqCst) > 64 * 1024);
}

#[test]
fn rejects_wal_and_bad_preconditions_without_writes() {
    let fs = SimFs::new();
    let (main, journal, statement) = caps(&fs);
    let mut header = [0u8; 20];
    header[18] = 2;
    header[19] = 2;
    main.write_at(&header, 0).unwrap();
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
    assert_eq!(main.len().unwrap(), 20);
    assert_eq!(journal.len().unwrap(), 0);
    assert_eq!(statement.len().unwrap(), 0);
    let mut actual = [0u8; 20];
    assert_eq!(main.read_at(&mut actual, 0).unwrap(), 20);
    assert_eq!(actual, header);

    let fs = SimFs::new();
    let (main, journal, statement) = caps(&fs);
    journal.write_at(b"x", 0).unwrap();
    let error = ConfinedSqlite::open(
        Arc::clone(&main),
        Arc::clone(&journal),
        Arc::clone(&statement),
    )
    .unwrap_err();
    assert!(matches!(
        error,
        rusqlite::Error::SqliteFailure(
            ffi::Error {
                code: ffi::ErrorCode::CannotOpen,
                ..
            },
            Some(_)
        )
    ));
    assert_eq!(statement.len().unwrap(), 0);

    let fs = SimFs::new();
    let (main, journal, statement) = caps(&fs);
    let error = ConfinedSqlite::open(Arc::clone(&main), Arc::clone(&main), Arc::clone(&statement))
        .unwrap_err();
    assert!(matches!(
        error,
        rusqlite::Error::SqliteFailure(
            ffi::Error {
                code: ffi::ErrorCode::CannotOpen,
                ..
            },
            Some(_)
        )
    ));
    assert_eq!(journal.len().unwrap(), 0);
}

#[test]
fn refuses_attach_and_a_second_main_connection() {
    let fs = SimFs::new();
    let (main, journal, statement) = caps(&fs);
    let conn = ConfinedSqlite::open(
        Arc::clone(&main),
        Arc::clone(&journal),
        Arc::clone(&statement),
    )
    .unwrap();
    let scratch = ScratchDir::new("stage-vfs-attach-uri");
    let escaped = scratch.path().join("must-not-create.db");
    let native_vfs = super::default_vfs().unwrap();
    let native_name = unsafe { std::ffi::CStr::from_ptr(native_vfs.zName) }
        .to_str()
        .unwrap();
    let uri = format!("file:{}?vfs={native_name}", escaped.display());
    for name in ["other.db", "file:other.db?mode=rwc", &uri] {
        for sql in ["ATTACH DATABASE ?1 AS escaped", "VACUUM main INTO ?1"] {
            assert!(conn.connection().execute(sql, [name]).is_err(), "{sql}");
        }
    }
    assert!(!escaped.exists());
    let name = conn.context().main_name.to_str().unwrap().to_string();
    let second = Connection::open_with_flags_and_vfs(
        name,
        OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
        conn.vfs_name(),
    );
    assert!(second.is_err());
    conn.connection()
        .execute_batch("CREATE TABLE proof(value TEXT);")
        .unwrap();
    conn.close().unwrap();
}

#[test]
fn retained_inodes_survive_directory_and_file_replacement() {
    let scratch = ScratchDir::new("stage-vfs-retained");
    let fs = real_fs();
    let original = scratch.path().join("stage");
    std::fs::create_dir(&original).unwrap();
    let root = fs.open_confined_dir(&original).unwrap();
    let main: Arc<dyn ReadWriteFile> = Arc::from(root.create_read_write(Path::new("db")).unwrap());
    let journal: Arc<dyn ReadWriteFile> =
        Arc::from(root.create_read_write(Path::new("journal")).unwrap());
    let statement: Arc<dyn ReadWriteFile> =
        Arc::from(root.create_read_write(Path::new("stmt")).unwrap());
    let main_id = main.identity().unwrap();
    let journal_id = journal.identity().unwrap();
    let statement_id = statement.identity().unwrap();
    let conn = ConfinedSqlite::open(
        Arc::clone(&main),
        Arc::clone(&journal),
        Arc::clone(&statement),
    )
    .unwrap();
    conn.connection()
        .execute_batch("CREATE TABLE proof(value TEXT); INSERT INTO proof VALUES('retained');")
        .unwrap();

    let moved = scratch.path().join("moved");
    std::fs::rename(&original, &moved).unwrap();
    std::fs::create_dir(&original).unwrap();
    std::fs::write(original.join("db"), b"decoy db").unwrap();
    std::fs::write(original.join("journal"), b"decoy journal").unwrap();
    std::fs::write(original.join("stmt"), b"decoy stmt").unwrap();
    std::fs::rename(moved.join("db"), moved.join("retained-db")).unwrap();
    std::fs::rename(moved.join("journal"), moved.join("retained-journal")).unwrap();
    std::fs::rename(moved.join("stmt"), moved.join("retained-stmt")).unwrap();
    std::fs::write(moved.join("db"), b"decoy moved db").unwrap();
    std::fs::write(moved.join("journal"), b"decoy moved journal").unwrap();
    std::fs::write(moved.join("stmt"), b"decoy moved stmt").unwrap();

    conn.connection()
        .execute_batch("INSERT INTO proof VALUES('more');")
        .unwrap();
    conn.close().unwrap();

    assert_eq!(main.identity().unwrap(), main_id);
    assert_eq!(journal.identity().unwrap(), journal_id);
    assert_eq!(statement.identity().unwrap(), statement_id);
    assert_eq!(std::fs::read(original.join("db")).unwrap(), b"decoy db");
    assert_eq!(
        std::fs::read(original.join("journal")).unwrap(),
        b"decoy journal"
    );
    assert_eq!(std::fs::read(original.join("stmt")).unwrap(), b"decoy stmt");
    assert_eq!(std::fs::read(moved.join("db")).unwrap(), b"decoy moved db");
    assert_eq!(
        std::fs::read(moved.join("journal")).unwrap(),
        b"decoy moved journal"
    );
    assert_eq!(
        std::fs::read(moved.join("stmt")).unwrap(),
        b"decoy moved stmt"
    );
    let retained = std::fs::read(moved.join("retained-db")).unwrap();
    assert!(retained.len() > 512);

    let reopened = ConfinedSqlite::open(
        Arc::clone(&main),
        Arc::clone(&journal),
        Arc::clone(&statement),
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
    assert_eq!(values, vec!["more".to_string(), "retained".to_string()]);
    reopened.close().unwrap();
}

#[test]
fn logical_journal_delete_truncates_and_flushes() {
    let fs = SimFs::new();
    let (main, journal, statement) = caps(&fs);
    let recording = Arc::new(Probe::new(Arc::clone(&journal)));
    let recorder: Arc<dyn ReadWriteFile> = recording.clone();
    let conn = ConfinedSqlite::open(Arc::clone(&main), recorder, Arc::clone(&statement)).unwrap();
    conn.connection()
        .execute_batch("CREATE TABLE proof(value TEXT); INSERT INTO proof VALUES('kept');")
        .unwrap();
    assert_eq!(recording.len().unwrap(), 0);
    assert!(recording.syncs.load(Ordering::SeqCst) > 0);
    conn.close().unwrap();
}

#[test]
fn latched_io_error_rejects_close() {
    let fs = SimFs::new();
    let (main, journal, statement) = caps(&fs);
    let failing = Arc::new(Probe::new(Arc::clone(&main)));
    let failing_cap: Arc<dyn ReadWriteFile> = failing.clone();
    let conn =
        ConfinedSqlite::open(failing_cap, Arc::clone(&journal), Arc::clone(&statement)).unwrap();
    conn.connection()
        .execute_batch("CREATE TABLE proof(value TEXT); INSERT INTO proof VALUES('first');")
        .unwrap();
    failing.fail_writes.store(true, Ordering::SeqCst);
    assert!(
        conn.connection()
            .execute_batch("INSERT INTO proof VALUES('second');")
            .is_err()
    );
    match conn.close() {
        Err(CloseError::IoError) => {}
        other => panic!("expected IoError, got {other:?}"),
    }
}

#[test]
fn panics_are_contained_and_latched() {
    let fs = SimFs::new();
    let (main, journal, statement) = caps(&fs);
    {
        let conn = ConfinedSqlite::open(
            Arc::clone(&main),
            Arc::clone(&journal),
            Arc::clone(&statement),
        )
        .unwrap();
        conn.connection()
            .execute_batch("CREATE TABLE proof(value TEXT); INSERT INTO proof VALUES('kept');")
            .unwrap();
        conn.close().unwrap();
    }
    let panicking = Arc::new(Probe::new(Arc::clone(&main)));
    let panicking_cap: Arc<dyn ReadWriteFile> = panicking.clone();
    let conn =
        ConfinedSqlite::open(panicking_cap, Arc::clone(&journal), Arc::clone(&statement)).unwrap();
    panicking.fail_reads.store(true, Ordering::SeqCst);
    assert!(
        conn.connection()
            .query_row("SELECT value FROM proof", [], |row| row.get::<_, String>(0))
            .is_err()
    );
    match conn.close() {
        Err(CloseError::IoError) => {}
        other => panic!("expected IoError, got {other:?}"),
    }
}

#[test]
fn close_rejects_uncommitted_transaction() {
    let fs = SimFs::new();
    let (main, journal, statement) = caps(&fs);
    let conn = ConfinedSqlite::open(
        Arc::clone(&main),
        Arc::clone(&journal),
        Arc::clone(&statement),
    )
    .unwrap();
    conn.connection()
        .execute_batch(
            "CREATE TABLE proof(value TEXT); BEGIN; INSERT INTO proof VALUES('pending');",
        )
        .unwrap();
    match conn.close() {
        Err(CloseError::UncommittedTransaction) => {}
        other => panic!("expected UncommittedTransaction, got {other:?}"),
    }
}

#[test]
fn access_reports_known_and_unknown_names() {
    let fs = SimFs::new();
    let (main, journal, statement) = caps(&fs);
    let conn = ConfinedSqlite::open(
        Arc::clone(&main),
        Arc::clone(&journal),
        Arc::clone(&statement),
    )
    .unwrap();
    let context = conn.context();
    let main_name = context.main_name.to_str().unwrap().to_string();
    let journal_name = context.journal_name.to_str().unwrap().to_string();
    assert_eq!(conn.access("unknown.db"), 0);
    assert_eq!(conn.access(&main_name), 1);
    assert_eq!(conn.access(&journal_name), 0);
    conn.connection()
        .execute_batch("CREATE TABLE proof(value TEXT); BEGIN; INSERT INTO proof VALUES('x');")
        .unwrap();
    assert_eq!(conn.access(&journal_name), 1);
    conn.connection().execute_batch("ROLLBACK;").unwrap();
    assert_eq!(conn.access(&journal_name), 0);
    conn.close().unwrap();
}

#[test]
fn all_opened_files_are_closed_and_syncs_are_real() {
    let fs = SimFs::new();
    let (main, journal, statement) = caps(&fs);
    let syncing = Arc::new(Probe::new(Arc::clone(&main)));
    let syncing_cap: Arc<dyn ReadWriteFile> = syncing.clone();
    let conn =
        ConfinedSqlite::open(syncing_cap, Arc::clone(&journal), Arc::clone(&statement)).unwrap();
    let context = Arc::clone(conn.context());
    conn.connection()
        .execute_batch("CREATE TABLE proof(value TEXT); INSERT INTO proof VALUES('kept');")
        .unwrap();
    conn.close().unwrap();
    assert_eq!(
        context.opens.load(Ordering::SeqCst),
        context.closes.load(Ordering::SeqCst)
    );
    assert!(syncing.syncs.load(Ordering::SeqCst) > 0);
}

#[test]
fn failed_close_and_drop_release_connections_and_vfs() {
    for explicit in [true, false] {
        let fs = SimFs::new();
        let (main, journal, statement) = caps(&fs);
        let conn = ConfinedSqlite::open(main, journal, statement).unwrap();
        let context = Arc::clone(conn.context());
        let name = conn.vfs_name().to_owned();
        conn.connection()
            .execute_batch("CREATE TABLE t(v); BEGIN; INSERT INTO t VALUES(1)")
            .unwrap();
        if explicit {
            assert!(matches!(
                conn.close(),
                Err(CloseError::UncommittedTransaction)
            ));
        } else {
            drop(conn);
        }
        assert_eq!(
            context.opens.load(Ordering::SeqCst),
            context.closes.load(Ordering::SeqCst)
        );
        assert!(unsafe { ffi::sqlite3_vfs_find(name.as_ptr()) }.is_null());
    }
}

#[test]
fn header_reads_retry_and_final_sync_panics_do_not_escape() {
    let fs = SimFs::new();
    let (main, journal, statement) = caps(&fs);
    let conn = ConfinedSqlite::open(main.clone(), journal.clone(), statement.clone()).unwrap();
    conn.connection()
        .execute_batch("CREATE TABLE t(v)")
        .unwrap();
    conn.close().unwrap();
    let probe = Arc::new(Probe::new(main));
    probe.short_reads.store(true, Ordering::SeqCst);
    probe.interrupt_read.store(true, Ordering::SeqCst);
    let conn = ConfinedSqlite::open(probe.clone(), journal, statement).unwrap();
    let context = Arc::clone(conn.context());
    let name = conn.vfs_name().to_owned();
    probe.fail_sync.store(true, Ordering::SeqCst);
    let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| conn.close()));
    assert!(matches!(result, Ok(Err(CloseError::IoError))));
    assert_eq!(
        context.opens.load(Ordering::SeqCst),
        context.closes.load(Ordering::SeqCst)
    );
    assert!(unsafe { ffi::sqlite3_vfs_find(name.as_ptr()) }.is_null());
}

#[test]
fn fault_latched_during_sqlite_close_rejects_output() {
    unsafe extern "C" fn function(
        _: *mut ffi::sqlite3_context,
        _: i32,
        _: *mut *mut ffi::sqlite3_value,
    ) {
    }
    unsafe extern "C" fn on_close(pointer: *mut std::ffi::c_void) {
        let context = unsafe { Arc::from_raw(pointer.cast::<super::Context>()) };
        context.io_error.store(true, Ordering::SeqCst);
    }
    let fs = SimFs::new();
    let (main, journal, statement) = caps(&fs);
    let conn = ConfinedSqlite::open(main, journal, statement).unwrap();
    let name = conn.vfs_name().to_owned();
    let data = Arc::into_raw(Arc::clone(conn.context())).cast_mut().cast();
    // SQLite calls the destructor while closing, after the pre-close
    // eligibility check. This models a late file callback's sticky fault.
    assert_eq!(
        unsafe {
            ffi::sqlite3_create_function_v2(
                conn.connection().handle(),
                c"close_probe".as_ptr(),
                0,
                ffi::SQLITE_UTF8,
                data,
                Some(function),
                None,
                None,
                Some(on_close),
            )
        },
        ffi::SQLITE_OK
    );
    assert!(!conn.context().io_error.load(Ordering::SeqCst));
    assert!(matches!(conn.close(), Err(CloseError::IoError)));
    assert!(unsafe { ffi::sqlite3_vfs_find(name.as_ptr()) }.is_null());
}

#[test]
fn refuses_readonly_and_unconfined_file_types() {
    let fs = SimFs::new();
    let (main, journal, statement) = caps(&fs);
    let mut conn = ConfinedSqlite::open(main, journal, statement).unwrap();
    let journal_name = conn.context().journal_name.clone();
    let vfs = &mut conn.vfs.as_mut().unwrap().base;
    for flags in [
        ffi::SQLITE_OPEN_READONLY | ffi::SQLITE_OPEN_MAIN_JOURNAL,
        ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_WAL,
        ffi::SQLITE_OPEN_READWRITE | ffi::SQLITE_OPEN_TEMP_DB,
    ] {
        let mut file = std::mem::MaybeUninit::<super::StageFile>::zeroed();
        let pointer = file.as_mut_ptr().cast::<ffi::sqlite3_file>();
        let rc = unsafe {
            vfs.xOpen.unwrap()(
                vfs,
                journal_name.as_ptr(),
                pointer,
                flags,
                std::ptr::null_mut(),
            )
        };
        if rc == ffi::SQLITE_OK {
            unsafe {
                super::close(pointer);
            }
        }
        assert_eq!(rc, ffi::SQLITE_CANTOPEN);
    }
    conn.close().unwrap();
}

mod wal_regression;

mod wal;
