use super::*;
use crate::testing::{scratch_paths, test_seams};
use sb_test_support::{ScratchDir, real_fs};
use std::io::{self, Cursor, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::Arc;
use std::time::{Duration, Instant};

struct Image {
    _scratch: ScratchDir,
    path: PathBuf,
}
impl Image {
    fn new() -> Self {
        let (scratch, paths) = scratch_paths("restore-schema");
        crate::open(&paths, &test_seams(real_fs()))
            .unwrap()
            .close()
            .unwrap();
        Self {
            _scratch: scratch,
            path: paths.state_db().to_owned(),
        }
    }
    fn sql(&self, sql: &str) {
        let conn = Connection::open(&self.path).unwrap();
        conn.execute_batch(sql).unwrap();
    }
    fn inspect(&self) -> Result<Option<Inspection>, StoreError> {
        inspect(
            Box::new(std::fs::File::open(&self.path).unwrap()),
            &AtomicBool::new(false),
        )
    }
    fn replace_accounts(&self, declaration: &str) {
        self.sql(&format!(
            "ALTER TABLE accounts RENAME TO old_accounts; {declaration}"
        ));
    }
}

fn require_corrupt(result: Result<Option<Inspection>, StoreError>) {
    assert!(
        matches!(result, Err(StoreError::Corrupt { .. })),
        "{result:?}"
    );
}

#[test]
fn canonical_current_schema_is_accepted_without_modifying_the_image() {
    let image = Image::new();
    let before = std::fs::read(&image.path).unwrap();
    assert_eq!(
        image.inspect().unwrap(),
        Some(Inspection {
            schema_version: i64::from(schema::LATEST_SCHEMA_VERSION),
        })
    );
    assert_eq!(std::fs::read(&image.path).unwrap(), before);
    for table in REQUIRED {
        assert!(table.columns.len() < u32::BITS as usize);
    }
}

#[test]
fn historical_and_future_versions_are_not_migrated_by_inspection() {
    for version in [0, 22, 23, i64::from(schema::LATEST_SCHEMA_VERSION) + 1] {
        let image = Image::new();
        image.sql(&format!("PRAGMA user_version={version}"));
        require_corrupt(image.inspect());
        let conn = Connection::open(&image.path).unwrap();
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |r| r.get::<_, i64>(0))
                .unwrap(),
            version
        );
    }
}

#[test]
fn views_virtual_tables_and_non_strict_tables_are_refused() {
    for declaration in [
        "CREATE VIEW accounts AS SELECT 'id' AS account_id, 'active' AS state;",
        "CREATE VIEW accounts AS WITH RECURSIVE x(n) AS (VALUES(0) UNION ALL SELECT n+1 FROM x) SELECT CAST(n AS TEXT) AS account_id, 'active' AS state FROM x;",
        "CREATE VIRTUAL TABLE accounts USING fts5(account_id, state);",
        "CREATE TABLE accounts (account_id TEXT, state TEXT);",
        "CREATE TABLE accounts (account_id TEXT PRIMARY KEY, state TEXT) STRICT, WITHOUT ROWID;",
    ] {
        let image = Image::new();
        image.replace_accounts(declaration);
        let start = Instant::now();
        require_corrupt(image.inspect());
        assert!(start.elapsed() < Duration::from_secs(3), "{declaration}");
    }
}

#[test]
fn missing_tables_columns_wrong_types_and_generated_required_columns_are_refused() {
    for declaration in [
        "",
        "CREATE TABLE accounts (account_id TEXT) STRICT;",
        "CREATE TABLE accounts (account_id TEXT, renamed_state TEXT) STRICT;",
        "CREATE TABLE accounts (account_id TEXT, state INTEGER) STRICT;",
        "CREATE TABLE accounts (account_id TEXT, state TEXT GENERATED ALWAYS AS ('active') VIRTUAL) STRICT;",
        "CREATE TABLE accounts (account_id TEXT, state TEXT GENERATED ALWAYS AS ('active') STORED) STRICT;",
    ] {
        let image = Image::new();
        image.replace_accounts(declaration);
        require_corrupt(image.inspect());
    }
}

#[test]
fn ordinary_and_generated_columns_cannot_shadow_any_physical_rowid_alias() {
    for alias in ["rowid", "_RoWiD_", "OID"] {
        for suffix in [
            "",
            "GENERATED ALWAYS AS ('masked') VIRTUAL",
            "GENERATED ALWAYS AS ('masked') STORED",
        ] {
            let image = Image::new();
            image.replace_accounts(&format!(
                "CREATE TABLE accounts (account_id TEXT, state TEXT, {alias} TEXT {suffix}) STRICT;"
            ));
            require_corrupt(image.inspect());
        }
    }
}

#[test]
fn verifier_objects_must_retain_its_without_rowid_shape() {
    let image = Image::new();
    image.sql(
        "ALTER TABLE verifier_objects RENAME TO old_verifier_objects;
        CREATE TABLE verifier_objects (scratch_id TEXT, sha256 BLOB, bytes BLOB,
            referenced INTEGER, PRIMARY KEY(scratch_id, sha256)) STRICT;",
    );
    require_corrupt(image.inspect());
}

#[test]
fn column_names_and_declared_types_follow_sqlites_case_insensitive_rules() {
    let image = Image::new();
    image.replace_accounts("CREATE TABLE ACCOUNTS (ACCOUNT_ID text, State text) STRICT;");
    assert!(image.inspect().unwrap().is_some());
}

#[test]
fn unrelated_large_archive_values_are_never_loaded_or_limited() {
    let image = Image::new();
    image.sql(
        "CREATE TABLE unrelated_archive (request BLOB, checkpoint TEXT) STRICT;
        INSERT INTO unrelated_archive VALUES (zeroblob(2000000), hex(zeroblob(1000000)));",
    );
    assert!(image.inspect().unwrap().is_some());
    let conn = Connection::open(&image.path).unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT length(request), length(checkpoint) FROM unrelated_archive",
            [],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        )
        .unwrap(),
        (2_000_000, 2_000_000)
    );
}

#[test]
fn oversized_schema_metadata_and_excessive_columns_fail_closed() {
    let image = Image::new();
    image.replace_accounts(&format!(
        "CREATE TABLE accounts (account_id TEXT, state TEXT, \"{}\" TEXT) STRICT;",
        "x".repeat(MAX_SQLITE_VALUE as usize + 1)
    ));
    require_corrupt(image.inspect());

    let image = Image::new();
    let columns = (0..MAX_COLUMNS)
        .map(|i| format!("c{i} TEXT"))
        .collect::<Vec<_>>()
        .join(",");
    image.replace_accounts(&format!(
        "CREATE TABLE accounts (account_id TEXT, state TEXT, {columns}) STRICT;"
    ));
    require_corrupt(image.inspect());
}

#[test]
fn inherited_trigger_programs_are_not_executed() {
    let image = Image::new();
    image.sql(
        "CREATE TABLE sentinel(value INTEGER); INSERT INTO sentinel VALUES(1);
        CREATE TRIGGER hostile_accounts AFTER UPDATE ON accounts BEGIN
            UPDATE sentinel SET value=2; SELECT nonexistent_function(); END;",
    );
    let bytes = std::fs::read(&image.path).unwrap();
    assert!(image.inspect().unwrap().is_some());
    assert_eq!(std::fs::read(&image.path).unwrap(), bytes);
    let conn = Connection::open(&image.path).unwrap();
    assert_eq!(
        conn.query_row("SELECT value FROM sentinel", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn cancellation_before_during_and_after_sqlite_work_never_returns_a_verdict() {
    let image = Image::new();
    assert_eq!(
        inspect(
            Box::new(std::fs::File::open(&image.path).unwrap()),
            &AtomicBool::new(true)
        )
        .unwrap(),
        None
    );

    // The reader raises cancellation while SQLite is obtaining schema pages.
    // Even if the work finishes before the monitor wakes, guarded rechecks it.
    struct CancelReader {
        input: std::fs::File,
        cancel: Arc<AtomicBool>,
        reads: usize,
    }
    impl Read for CancelReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.reads += 1;
            if self.reads >= 2 {
                self.cancel.store(true, Ordering::Release);
            }
            self.input.read(buf)
        }
    }
    impl Seek for CancelReader {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            self.input.seek(pos)
        }
    }
    let cancel = Arc::new(AtomicBool::new(false));
    let reader = CancelReader {
        input: std::fs::File::open(&image.path).unwrap(),
        cancel: Arc::clone(&cancel),
        reads: 0,
    };
    let outcome = inspect(Box::new(reader), &cancel).unwrap();
    assert!(cancel.load(Ordering::Acquire));
    assert_eq!(outcome, None);
}

#[test]
fn sqlite_work_budget_fails_closed_and_releases_the_handler() {
    let image = Image::new();
    let conn = crate::image_vfs::open(Box::new(std::fs::File::open(&image.path).unwrap())).unwrap();
    require_corrupt(inspect_connection(&conn, &AtomicBool::new(false), 1));
    assert_eq!(
        conn.query_row("SELECT 42", [], |r| r.get::<_, i64>(0))
            .unwrap(),
        42
    );
}

#[test]
fn retained_reader_cannot_be_redirected_to_a_replaced_path_or_source_wal() {
    let image = Image::new();
    let reader = std::fs::File::open(&image.path).unwrap();
    let moved = image.path.with_extension("retained");
    std::fs::rename(&image.path, &moved).unwrap();
    std::fs::write(&image.path, b"not the retained database").unwrap();
    assert!(
        inspect(Box::new(reader), &AtomicBool::new(false))
            .unwrap()
            .is_some()
    );
    std::fs::remove_file(&image.path).unwrap();
    std::fs::rename(&moved, &image.path).unwrap();

    let conn = Connection::open(&image.path).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA wal_autocheckpoint=0; PRAGMA user_version=0;",
    )
    .unwrap();
    assert!(image.path.with_extension("sqlite-wal").exists());
    assert!(
        image.inspect().unwrap().is_some(),
        "the image's sidecar WAL must not be consulted"
    );
}

#[test]
fn native_read_errors_propagate_and_retained_readers_close_on_all_outcomes() {
    struct Reader {
        input: Cursor<Vec<u8>>,
        dropped: Arc<AtomicBool>,
        fail: bool,
    }
    impl Read for Reader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            if self.fail {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
            self.input.read(buf)
        }
    }
    impl Seek for Reader {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            self.input.seek(pos)
        }
    }
    impl Drop for Reader {
        fn drop(&mut self) {
            self.dropped.store(true, Ordering::Release);
        }
    }
    let image = Image::new();
    for fail in [false, true] {
        let dropped = Arc::new(AtomicBool::new(false));
        let reader = Reader {
            input: Cursor::new(std::fs::read(&image.path).unwrap()),
            dropped: Arc::clone(&dropped),
            fail,
        };
        let result = inspect(Box::new(reader), &AtomicBool::new(false));
        assert!(dropped.load(Ordering::Acquire));
        if fail {
            assert!(
                matches!(result, Err(StoreError::Unavailable { .. })),
                "{result:?}"
            );
        } else {
            assert!(result.unwrap().is_some());
        }
    }
}

#[test]
fn schema_metadata_allocation_is_bounded_in_an_isolated_process() {
    // SQLite's allocation high-water mark is global, so other tests must
    // not be able to contaminate either the control or the bounded run.
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "backups::stage_schema::tests::allocation_child",
            "--nocapture",
        ])
        .env("SB_RESTORE_SCHEMA_ALLOC_CHILD", "1")
        .output()
        .unwrap();
    assert!(
        child.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
}

#[test]
#[ignore = "spawned by schema_metadata_allocation_is_bounded_in_an_isolated_process"]
fn allocation_child() {
    assert_eq!(
        std::env::var("SB_RESTORE_SCHEMA_ALLOC_CHILD").as_deref(),
        Ok("1")
    );
    let image = Image::new();
    let huge = "x".repeat(16 * 1024 * 1024);
    image.sql(&format!("ALTER TABLE accounts ADD COLUMN \"{huge}\" TEXT"));

    let legacy = Connection::open(&image.path).unwrap();
    crate::image_vfs::largest_allocation(true).unwrap();
    let mut statement = legacy.prepare("PRAGMA table_info(accounts)").unwrap();
    let columns = statement
        .query_map([], |r| r.get::<_, String>(1))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert!(columns.iter().any(|name| name.len() == huge.len()));
    let unbounded = crate::image_vfs::largest_allocation(false).unwrap();
    assert!(
        unbounded >= huge.len() as i64,
        "control allocation: {unbounded}"
    );
    drop(statement);
    drop(legacy);

    crate::image_vfs::largest_allocation(true).unwrap();
    let result = image.inspect();
    let bounded = crate::image_vfs::largest_allocation(false).unwrap();
    assert!(
        bounded < i64::from(MAX_SQLITE_VALUE),
        "bounded allocation: {bounded}"
    );
    require_corrupt(result);
}
