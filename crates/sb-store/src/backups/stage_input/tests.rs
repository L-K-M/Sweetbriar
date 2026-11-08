use super::*;
use std::path::Path;

use rusqlite::{Connection, limits::Limit};
use sb_test_support::{FaultFs, FaultKind, FsOp, ScratchDir, SimFs};
use sb_types::fs::Fs;

fn file(fs: &dyn Fs, bytes: &[u8]) -> Arc<dyn ReadWriteFile> {
    fs.create_dir(Path::new("/stage")).unwrap();
    let root = fs.open_confined_dir(Path::new("/stage")).unwrap();
    let file = root.create_read_write(Path::new("main")).unwrap();
    file.write_at(bytes, 0).unwrap();
    file.into()
}

fn wrap(bytes: &[u8]) -> (Arc<dyn ReadWriteFile>, InputBudget) {
    InputBudget::wrap(
        file(&SimFs::new(), bytes),
        bytes.len() as u64,
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap()
}

#[test]
fn exact_accounting_preserves_short_reads_eof_and_empty_reads() {
    let (file, mut budget) = wrap(b"0123456789");
    budget.state.counter.lock().unwrap().remaining = 20;
    let mut bytes = [0; 64];
    assert_eq!(file.read_at(&mut bytes, 0).unwrap(), 10);
    assert_eq!(&bytes[..10], b"0123456789");
    assert_eq!(budget.state.counter.lock().unwrap().remaining, 10);
    assert_eq!(file.read_at(&mut bytes, 10).unwrap(), 0);
    assert_eq!(budget.state.counter.lock().unwrap().remaining, 10);
    file.set_len(64).unwrap();
    assert_eq!(file.read_at(&mut bytes, 0).unwrap(), 10);
    assert!(file.read_at(&mut [], 0).is_ok());
    assert!(file.read_at(&mut bytes, 0).is_err());
    assert!(matches!(budget.error(), Some(StoreError::Corrupt { .. })));
    assert!(budget.begin_data_phase().is_err());
}

#[test]
fn interruption_does_not_spend_bytes_but_real_faults_remain_typed_and_sticky() {
    for kind in [io::ErrorKind::Interrupted, io::ErrorKind::PermissionDenied] {
        let fs = FaultFs::new(Arc::new(SimFs::new()), FsOp::Read, 2, FaultKind::Kind(kind));
        let (file, mut budget) =
            InputBudget::wrap(file(&fs, b"abc"), 3, Arc::new(AtomicBool::new(false))).unwrap();
        assert_eq!(
            fs.calls(FsOp::Read),
            1,
            "constructor measured the retained file"
        );
        let error = file.read_at(&mut [0; 3], 0).unwrap_err();
        assert_eq!(error.kind(), kind);
        assert_eq!(fs.calls(FsOp::Read), 2, "the selected fault fired");
        assert_eq!(
            budget.state.counter.lock().unwrap().remaining,
            CATALOG_BYTES
        );
        if kind == io::ErrorKind::Interrupted {
            assert!(budget.error().is_none());
            assert_eq!(file.read_at(&mut [0; 3], 0).unwrap(), 3);
            budget.begin_data_phase().unwrap();
            assert!(budget.begin_data_phase().is_err());
        } else {
            assert_eq!(budget.error().unwrap().io_kind(), Some(kind));
            assert!(budget.begin_data_phase().is_err());
        }
    }
}

#[test]
fn pre_cancelled_open_does_not_measure_or_open_sqlite() {
    let fs = FaultFs::new(
        Arc::new(SimFs::new()),
        FsOp::Read,
        1,
        FaultKind::Kind(io::ErrorKind::PermissionDenied),
    );
    let result = InputBudget::wrap(file(&fs, b"abc"), 3, Arc::new(AtomicBool::new(true)));
    assert!(matches!(
        result,
        Err(StoreError::Unavailable { io_kind: None, .. })
    ));
    assert_eq!(fs.calls(FsOp::Read), 0);
}

#[test]
fn an_observed_cancellation_cannot_be_cleared_to_promote_a_failed_phase() {
    let cancel = Arc::new(AtomicBool::new(false));
    let (file, mut budget) =
        InputBudget::wrap(file(&SimFs::new(), b"abc"), 3, Arc::clone(&cancel)).unwrap();
    cancel.store(true, Ordering::Release);
    assert!(file.read_at(&mut [0; 1], 0).is_err());
    cancel.store(false, Ordering::Release);
    assert!(
        budget.begin_data_phase().is_err(),
        "a failed catalog cannot be renewed"
    );
}

#[test]
fn concurrent_cursors_share_one_allowance_without_renewing_it() {
    let (file, budget) = wrap(&[7; 100]);
    budget.state.counter.lock().unwrap().remaining = 257;
    let total = std::thread::scope(|scope| {
        let mut threads = Vec::new();
        for _ in 0..8 {
            let file = Arc::clone(&file);
            threads.push(scope.spawn(move || {
                let mut read = 0;
                while let Ok(count) = file.read_at(&mut [0; 31], 0) {
                    assert!(count > 0);
                    read += count;
                }
                read
            }));
        }
        threads
            .into_iter()
            .map(|thread| thread.join().unwrap())
            .sum::<usize>()
    });
    assert_eq!(total, 257);
    assert!(matches!(budget.error(), Some(StoreError::Corrupt { .. })));
}

#[test]
fn forwarded_failures_keep_the_first_native_cause_and_block_promotion() {
    for (op, nth) in [
        (FsOp::Write, 2),
        (FsOp::Truncate, 1),
        (FsOp::SyncFile, 1),
        (FsOp::Open, 2),
        (FsOp::Read, 2),
    ] {
        for kind in [
            FaultKind::Eio,
            FaultKind::Kind(io::ErrorKind::PermissionDenied),
        ] {
            let fs = FaultFs::new(Arc::new(SimFs::new()), op, nth, kind);
            let (file, mut budget) =
                InputBudget::wrap(file(&fs, b"abc"), 3, Arc::new(AtomicBool::new(false))).unwrap();
            let error = match op {
                FsOp::Write => file.write_at(b"x", 0).err().unwrap(),
                FsOp::Truncate => file.set_len(1).err().unwrap(),
                FsOp::SyncFile => file.sync_all().err().unwrap(),
                FsOp::Open => file.identity().err().unwrap(),
                FsOp::Read => file.len().err().unwrap(),
                _ => unreachable!(),
            };
            assert_eq!(fs.calls(op), nth);
            assert_eq!(budget.error().unwrap().io_kind(), Some(error.kind()));
            assert!(budget.begin_data_phase().is_err());
        }
    }
}

#[derive(Clone, Copy)]
enum ReadFault {
    Panic,
    PermissionDenied,
}

struct ProbeFile {
    file: Arc<dyn ReadWriteFile>,
    short: usize,
    length: Option<u64>,
    fault: Arc<Mutex<Option<ReadFault>>>,
}

impl ReadWriteFile for ProbeFile {
    fn read_at(&self, bytes: &mut [u8], offset: u64) -> io::Result<usize> {
        let fault = self.fault.lock().unwrap().take();
        match fault {
            Some(ReadFault::Panic) => panic!("synthetic read panic"),
            Some(ReadFault::PermissionDenied) => return Err(io::ErrorKind::PermissionDenied.into()),
            None => {}
        }
        let count = bytes.len().min(self.short);
        self.file.read_at(&mut bytes[..count], offset)
    }
    fn write_at(&self, bytes: &[u8], offset: u64) -> io::Result<usize> {
        self.file.write_at(bytes, offset)
    }
    fn set_len(&self, len: u64) -> io::Result<()> {
        self.file.set_len(len)
    }
    fn len(&self) -> io::Result<u64> {
        self.length.map_or_else(|| self.file.len(), Ok)
    }
    fn sync_all(&self) -> io::Result<()> {
        self.file.sync_all()
    }
    fn identity(&self) -> io::Result<FileIdentity> {
        self.file.identity()
    }
}

#[test]
fn partial_reads_spend_only_returned_bytes_and_bad_extent_refuses_before_sql() {
    let inner = file(&SimFs::new(), b"0123456789");
    for length in [Some(11), Some(u64::MAX)] {
        let reported = length.unwrap();
        let probe = Arc::new(ProbeFile {
            file: Arc::clone(&inner),
            short: 2,
            length,
            fault: Arc::new(Mutex::new(None)),
        });
        let expected = if reported == 11 { 10 } else { reported };
        assert!(matches!(
            InputBudget::wrap(probe, expected, Arc::new(AtomicBool::new(false))),
            Err(StoreError::Corrupt { .. })
        ));
    }
    let probe = Arc::new(ProbeFile {
        file: inner,
        short: 2,
        length: None,
        fault: Arc::new(Mutex::new(None)),
    });
    let (file, budget) = InputBudget::wrap(probe, 10, Arc::new(AtomicBool::new(false))).unwrap();
    budget.state.counter.lock().unwrap().remaining = 5;
    let mut bytes = [99; 8];
    assert_eq!(
        super::super::stage_io::read(file.as_ref(), &mut bytes, 0),
        rusqlite::ffi::SQLITE_IOERR_READ
    );
    assert_eq!(&bytes, b"01234\0\0\0");
    assert_eq!(budget.state.counter.lock().unwrap().remaining, 0);
    assert!(matches!(budget.error(), Some(StoreError::Corrupt { .. })));
}

#[test]
fn retained_identity_and_writes_ignore_replaced_file_and_directory_names() {
    let scratch = ScratchDir::new("writable-input-identity");
    let path = scratch.path().join("stage");
    std::fs::create_dir(&path).unwrap();
    let fs = sb_test_support::real_fs();
    let root = fs.open_confined_dir(&path).unwrap();
    let main: Arc<dyn ReadWriteFile> = root.create_read_write(Path::new("main")).unwrap().into();
    main.write_at(b"original", 0).unwrap();
    let identity = main.identity().unwrap();
    let (wrapped, _) = InputBudget::wrap(main, 8, Arc::new(AtomicBool::new(false))).unwrap();
    std::fs::rename(path.join("main"), path.join("held")).unwrap();
    std::fs::write(path.join("main"), b"replaced").unwrap();
    let moved = scratch.path().join("moved");
    std::fs::rename(&path, &moved).unwrap();
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("main"), b"new-root").unwrap();
    assert_eq!(wrapped.identity().unwrap(), identity);
    wrapped.write_at(b"changed!", 0).unwrap();
    wrapped.sync_all().unwrap();
    assert_eq!(std::fs::read(moved.join("held")).unwrap(), b"changed!");
    assert_eq!(std::fs::read(moved.join("main")).unwrap(), b"replaced");
    assert_eq!(std::fs::read(path.join("main")).unwrap(), b"new-root");
}

fn source(large_catalog: bool) -> (ScratchDir, Vec<u8>) {
    let scratch = ScratchDir::new("writable-input-source");
    let path = scratch.path().join("source.db");
    let connection = Connection::open(&path).unwrap();
    connection
        .execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE proof(v BLOB) STRICT; BEGIN;")
        .unwrap();
    if large_catalog {
        let literal = "x".repeat(16_000);
        for table in 0..200 {
            connection
                .execute_batch(&format!(
                    "CREATE TABLE t{table}(v TEXT CHECK(v<>'{literal}')) STRICT;"
                ))
                .unwrap();
        }
    } else {
        connection
            .execute(
                "INSERT INTO proof VALUES (?1)",
                [vec![42u8; 3 * 1024 * 1024]],
            )
            .unwrap();
    }
    connection.execute_batch("COMMIT").unwrap();
    connection.close().unwrap();
    let bytes = std::fs::read(path).unwrap();
    assert_eq!(&bytes[18..20], &[2, 2]);
    (scratch, bytes)
}

fn writer(bytes: &[u8]) -> (ConfinedSqlite, InputBudget, Arc<dyn ReadWriteFile>) {
    let fs = SimFs::new();
    let main = file(&fs, bytes);
    let root = fs.open_confined_dir(Path::new("/stage")).unwrap();
    let journal = root.create_read_write(Path::new("journal")).unwrap().into();
    let statement = root
        .create_read_write(Path::new("statement"))
        .unwrap()
        .into();
    let wal = root.create_read_write(Path::new("wal")).unwrap().into();
    let (writer, budget) = open_bounded(
        Arc::clone(&main),
        journal,
        statement,
        wal,
        bytes.len() as u64,
        Arc::new(AtomicBool::new(false)),
    )
    .unwrap();
    (writer, budget, main)
}

#[test]
fn native_sqlite_read_faults_are_contained_typed_and_never_close_eligible() {
    for injected in [ReadFault::Panic, ReadFault::PermissionDenied] {
        let (scratch, bytes) = source(false);
        let fs = SimFs::new();
        let fault = Arc::new(Mutex::new(None));
        let main: Arc<dyn ReadWriteFile> = Arc::new(ProbeFile {
            file: file(&fs, &bytes),
            short: usize::MAX,
            length: None,
            fault: Arc::clone(&fault),
        });
        let root = fs.open_confined_dir(Path::new("/stage")).unwrap();
        let (writer, mut budget) = open_bounded(
            Arc::clone(&main),
            root.create_read_write(Path::new("journal")).unwrap().into(),
            root.create_read_write(Path::new("statement"))
                .unwrap()
                .into(),
            root.create_read_write(Path::new("wal")).unwrap().into(),
            bytes.len() as u64,
            Arc::new(AtomicBool::new(false)),
        )
        .unwrap();
        harden(writer.connection());
        *fault.lock().unwrap() = Some(injected);
        let result = writer
            .connection()
            .query_row("SELECT count(*) FROM proof", [], |row| row.get::<_, i64>(0));
        assert!(result.is_err());
        assert!(
            fault.lock().unwrap().is_none(),
            "the real SQLite callback fired the selected fault"
        );
        assert_eq!(
            budget.error().unwrap().io_kind(),
            Some(match injected {
                ReadFault::Panic => io::ErrorKind::Other,
                ReadFault::PermissionDenied => io::ErrorKind::PermissionDenied,
            })
        );
        assert!(budget.begin_data_phase().is_err());
        assert!(writer.close().is_err());
        assert_eq!(Arc::strong_count(&main), 1);
        assert_eq!(
            std::fs::read(scratch.path().join("source.db")).unwrap(),
            bytes
        );
    }
}

fn harden(connection: &Connection) {
    for limit in [Limit::SQLITE_LIMIT_LENGTH, Limit::SQLITE_LIMIT_SQL_LENGTH] {
        connection.set_limit(limit, 1_048_576).unwrap();
    }
    connection
        .set_limit(Limit::SQLITE_LIMIT_COLUMN, 64)
        .unwrap();
    connection
        .execute_batch("PRAGMA trusted_schema=OFF; PRAGMA temp_store=MEMORY;")
        .unwrap();
    let mut calls = 0;
    connection
        .progress_handler(
            1_000,
            Some(move || {
                calls += 1;
                calls > 20_000
            }),
        )
        .unwrap();
}

#[test]
fn real_sqlite_eager_schema_loading_hits_the_fixed_catalog_input_limit() {
    let (scratch, bytes) = source(true);
    let (writer, mut budget, main) = writer(&bytes);
    let connection = writer.connection();
    harden(connection);
    // A header pragma alone need not load the catalog; preparing a table
    // read forces SQLite's eager schema load before it can inspect this row.
    let result = connection.query_row("SELECT count(*) FROM proof", [], |row| row.get::<_, i64>(0));
    assert!(result.is_err());
    assert!(
        matches!(budget.error(), Some(StoreError::Corrupt { .. })),
        "fixed physical input quota must fire"
    );
    assert!(budget.begin_data_phase().is_err());
    assert!(writer.close().is_err());
    assert_eq!(
        Arc::strong_count(&main),
        1,
        "SQLite callbacks released the retained main file"
    );
    assert_eq!(
        std::fs::read(scratch.path().join("source.db")).unwrap(),
        bytes
    );
}

#[test]
fn promoted_real_sqlite_reads_large_valid_data_and_closes_after_checkpoint() {
    let (scratch, bytes) = source(false);
    let (writer, mut budget, main) = writer(&bytes);
    let connection = writer.connection();
    harden(connection);
    let _: i64 = connection
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    assert!(budget.error().is_none());
    budget.begin_data_phase().unwrap();
    connection
        .set_limit(Limit::SQLITE_LIMIT_LENGTH, 1_000_000_000)
        .unwrap();
    let value: Vec<u8> = connection
        .query_row("SELECT v FROM proof", [], |row| row.get(0))
        .unwrap();
    assert_eq!(value, vec![42u8; 3 * 1024 * 1024]);
    connection
        .execute("INSERT INTO proof VALUES (?1)", [vec![9u8; 12]])
        .unwrap();
    writer.checkpoint_wal().unwrap();
    writer.close().unwrap();
    assert!(budget.error().is_none());
    assert_eq!(Arc::strong_count(&main), 1);
    assert_eq!(
        std::fs::read(scratch.path().join("source.db")).unwrap(),
        bytes
    );
    let mut copied = vec![0; main.len().unwrap() as usize];
    assert_eq!(main.read_at(&mut copied, 0).unwrap(), copied.len());
    let reopen_path = scratch.path().join("reopen.db");
    std::fs::write(&reopen_path, copied).unwrap();
    let reopened = Connection::open(reopen_path).unwrap();
    assert_eq!(
        reopened
            .query_row("SELECT count(*) FROM proof", [], |row| row.get::<_, i64>(0))
            .unwrap(),
        2
    );
    reopened.close().unwrap();
}
