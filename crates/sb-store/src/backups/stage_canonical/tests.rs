use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::{Arc, atomic::AtomicU64};

use rusqlite::Connection;
use sb_test_support::{ScratchDir, real_fs};

use super::*;
use crate::testing::{scratch_paths, test_seams};

struct Image {
    _scratch: ScratchDir,
    path: PathBuf,
}

impl Image {
    fn new() -> Self {
        let (scratch, paths) = scratch_paths("canonical-catalog");
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
        Connection::open(&self.path)
            .unwrap()
            .execute_batch(sql)
            .unwrap();
    }
    fn inspect(&self) -> Result<Option<Inspection>, StoreError> {
        inspect(
            Box::new(std::fs::File::open(&self.path).unwrap()),
            &test_seams(real_fs()),
            &AtomicBool::new(false),
        )
    }
}

struct Counted {
    file: std::fs::File,
    bytes: Arc<AtomicU64>,
}
impl Read for Counted {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.file.read(buffer)?;
        self.bytes.fetch_add(read as u64, Ordering::SeqCst);
        Ok(read)
    }
}
impl Seek for Counted {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.file.seek(to)
    }
}

#[test]
fn schema_materialization_has_an_aggregate_input_bound_before_comparison() {
    let image = Image::new();
    let large = "x".repeat(800_000);
    for table in 0..3 {
        image.sql(&format!(
            "CREATE TABLE extra{table}(x TEXT CHECK(x <> '{large}')) STRICT"
        ));
    }
    let bytes = Arc::new(AtomicU64::new(0));
    let result = inspect(
        Box::new(Counted {
            file: std::fs::File::open(&image.path).unwrap(),
            bytes: bytes.clone(),
        }),
        &test_seams(real_fs()),
        &AtomicBool::new(false),
    );
    assert!(
        matches!(result, Err(StoreError::Corrupt { .. })),
        "{result:?}"
    );
    assert!(
        bytes.load(Ordering::SeqCst) <= MAX_SCHEMA_INPUT,
        "eager catalog materialization read {} bytes before comparison",
        bytes.load(Ordering::SeqCst)
    );
}

#[test]
fn many_small_catalog_rows_fire_the_physical_input_budget() {
    let image = Image::new();
    let conn = Connection::open(&image.path).unwrap();
    conn.execute_batch("BEGIN").unwrap();
    let padding = "x".repeat(1000);
    for table in 0..2500 {
        conn.execute_batch(&format!(
            "CREATE TABLE extra{table}(x TEXT CHECK(x <> '{padding}')) STRICT"
        ))
        .unwrap();
    }
    conn.execute_batch("COMMIT").unwrap();
    conn.close().unwrap();
    assert!(std::fs::metadata(&image.path).unwrap().len() > MAX_SCHEMA_INPUT);
    let bytes = Arc::new(AtomicU64::new(0));
    let result = inspect(
        Box::new(Counted {
            file: std::fs::File::open(&image.path).unwrap(),
            bytes: bytes.clone(),
        }),
        &test_seams(real_fs()),
        &AtomicBool::new(false),
    );
    assert!(
        matches!(result, Err(StoreError::Corrupt { ref detail }) if detail == "restore catalog input budget exceeded"),
        "{result:?}"
    );
    assert_eq!(
        bytes.load(Ordering::SeqCst),
        MAX_SCHEMA_INPUT,
        "budget did not actually fire"
    );
}

#[test]
fn current_catalog_is_accepted_without_writes() {
    let image = Image::new();
    let before = std::fs::read(&image.path).unwrap();
    assert_eq!(
        image.inspect().unwrap(),
        Some(Inspection {
            schema_version: i64::from(schema::LATEST_SCHEMA_VERSION)
        })
    );
    assert_eq!(std::fs::read(&image.path).unwrap(), before);
}

#[test]
fn input_budget_counts_actual_short_reads_and_cannot_be_renewed_by_seek() {
    struct Short {
        inner: io::Cursor<Vec<u8>>,
        interrupted: bool,
    }
    impl Read for Short {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            if self.interrupted {
                self.interrupted = false;
                return Err(io::ErrorKind::Interrupted.into());
            }
            let count = bytes.len().min(3);
            self.inner.read(&mut bytes[..count])
        }
    }
    impl Seek for Short {
        fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
            self.inner.seek(to)
        }
    }
    let exhausted = Arc::new(AtomicBool::new(false));
    let mut reader = BudgetReader {
        file: Box::new(Short {
            inner: io::Cursor::new(b"123456789".to_vec()),
            interrupted: true,
        }),
        remaining: Arc::new(AtomicU64::new(5)),
        exhausted: exhausted.clone(),
        fault: Arc::new(Mutex::new(None)),
    };
    let mut bytes = [0; 5];
    reader.read_exact(&mut bytes).unwrap();
    assert_eq!(bytes, *b"12345");
    assert!(!exhausted.load(Ordering::Acquire));
    assert_eq!(reader.read(&mut []).unwrap(), 0);
    assert_eq!(reader.seek(SeekFrom::Start(0)).unwrap(), 0);
    assert!(reader.read(&mut [0]).is_err());
    assert!(exhausted.load(Ordering::Acquire));
}

fn require_corrupt(result: Result<Option<Inspection>, StoreError>) {
    assert!(
        matches!(result, Err(StoreError::Corrupt { .. })),
        "{result:?}"
    );
}

#[test]
fn full_catalog_refuses_additions_removals_and_changes_outside_quarantine_tables() {
    for sql in [
        "CREATE TABLE foreign_table(x TEXT) STRICT",
        "CREATE INDEX foreign_index ON accounts(display_name)",
        "DROP INDEX identities_one_default",
        "DROP TRIGGER export_jobs_terminal",
        "CREATE VIEW foreign_view AS SELECT 1",
        "CREATE VIEW foreign_view AS WITH RECURSIVE x(n) AS (VALUES(0) UNION ALL SELECT n+1 FROM x) SELECT * FROM x",
        "CREATE VIRTUAL TABLE foreign_fts USING fts5(value)",
        "ALTER TABLE identities ADD COLUMN extra TEXT",
        "ALTER TABLE identities ADD COLUMN rowid TEXT GENERATED ALWAYS AS ('shadow') VIRTUAL",
        "ALTER TABLE identities RENAME COLUMN display_name TO renamed_name",
        "ALTER TABLE identities RENAME TO old_identities; CREATE VIEW identities AS SELECT * FROM old_identities",
        "CREATE TRIGGER hostile AFTER UPDATE ON accounts BEGIN UPDATE accounts SET state='active'; END",
    ] {
        let image = Image::new();
        image.sql(sql);
        let before = std::fs::read(&image.path).unwrap();
        require_corrupt(image.inspect());
        assert_eq!(std::fs::read(&image.path).unwrap(), before, "{sql}");
    }
}

#[test]
fn rewritten_constraints_types_and_canonical_programs_are_refused() {
    for (kind, name, from, to) in [
        ("table", "identities", "'validated'", "'forged'"),
        (
            "table",
            "identities",
            "display_name       TEXT",
            "display_name       INTEGER",
        ),
        (
            "trigger",
            "export_jobs_terminal",
            "BEFORE UPDATE",
            "AFTER UPDATE",
        ),
    ] {
        let image = Image::new();
        let conn = Connection::open(&image.path).unwrap();
        let original: String = conn
            .query_row(
                "SELECT sql FROM sqlite_schema WHERE type=?1 AND name=?2",
                [kind, name],
                |row| row.get(0),
            )
            .unwrap();
        let modified = original.replace(from, to);
        assert_ne!(modified, original, "fixture did not change {name}");
        conn.execute_batch("PRAGMA writable_schema=ON").unwrap();
        assert_eq!(
            conn.execute(
                "UPDATE sqlite_schema SET sql=?1 WHERE type=?2 AND name=?3",
                rusqlite::params![modified, kind, name]
            )
            .unwrap(),
            1
        );
        conn.execute_batch("PRAGMA writable_schema=OFF").unwrap();
        conn.close().unwrap();
        require_corrupt(image.inspect());
    }
}

#[test]
fn a_missing_implicit_index_catalog_row_is_not_ignored_because_its_sql_is_null() {
    let image = Image::new();
    let conn = Connection::open(&image.path).unwrap();
    conn.execute_batch("PRAGMA writable_schema=ON").unwrap();
    assert_eq!(
        conn.execute(
            "DELETE FROM sqlite_schema WHERE name='sqlite_autoindex_identities_1' AND sql IS NULL",
            []
        )
        .unwrap(),
        1
    );
    conn.execute_batch("PRAGMA writable_schema=OFF").unwrap();
    conn.close().unwrap();
    let before = std::fs::read(&image.path).unwrap();
    require_corrupt(image.inspect());
    assert_eq!(std::fs::read(&image.path).unwrap(), before);
}

#[test]
fn genuine_historical_catalogs_and_future_versions_are_never_migrated() {
    for version in [22, 23] {
        let scratch = ScratchDir::new("catalog-history");
        let path = scratch.path().join("old.db");
        let mut conn = Connection::open(&path).unwrap();
        schema::run_migrations(
            &mut conn,
            &test_seams(real_fs()),
            &schema::MIGRATIONS[..version],
        )
        .unwrap();
        conn.close().unwrap();
        let before = std::fs::read(&path).unwrap();
        require_corrupt(inspect(
            Box::new(std::fs::File::open(&path).unwrap()),
            &test_seams(real_fs()),
            &AtomicBool::new(false),
        ));
        assert_eq!(std::fs::read(&path).unwrap(), before);
        let conn = Connection::open(&path).unwrap();
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .unwrap(),
            version as i64
        );
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM sqlite_schema WHERE name='verifier_objects'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            0
        );
    }
    for version in [0, schema::LATEST_SCHEMA_VERSION + 1] {
        let image = Image::new();
        image.sql(&format!("PRAGMA user_version={version}"));
        let before = std::fs::read(&image.path).unwrap();
        require_corrupt(image.inspect());
        assert_eq!(std::fs::read(&image.path).unwrap(), before);
    }
}

#[test]
fn valid_large_archive_requests_are_unread_and_not_subject_to_metadata_limits() {
    let image = Image::new();
    let conn = Connection::open(&image.path).unwrap();
    let request = format!("{{\"body\":\"{}\"}}", "x".repeat(2_000_000));
    conn.execute("INSERT INTO import_jobs(job_id,state,request_json,checkpoint_json,revision,created_at,updated_at)
        VALUES('large-request','failed',?1,?1,1,0,0)", [&request]).unwrap();
    conn.close().unwrap();
    let bytes = Arc::new(AtomicU64::new(0));
    let result = inspect(
        Box::new(Counted {
            file: std::fs::File::open(&image.path).unwrap(),
            bytes: bytes.clone(),
        }),
        &test_seams(real_fs()),
        &AtomicBool::new(false),
    )
    .unwrap();
    assert!(result.is_some());
    assert!(
        bytes.load(Ordering::SeqCst) < 1_000_000,
        "unrelated archive payload was read"
    );
}

#[test]
fn reference_catalog_is_independent_of_random_identity_data_and_vacuum_order() {
    let first = test_seams(real_fs());
    let second = Seams::new(
        real_fs(),
        Arc::new(sb_test_support::FixedClock::new(
            crate::Timestamp::from_millis(1),
        )),
        Arc::new(sb_test_support::SeededRandom::new(987)),
    );
    assert_eq!(
        catalog::reference(&first).unwrap(),
        catalog::reference(&second).unwrap()
    );
    let image = Image::new();
    image.sql("VACUUM");
    assert!(image.inspect().unwrap().is_some());
}

#[test]
fn oversized_catalog_names_and_excessive_columns_are_refused() {
    for declaration in [
        format!(
            "ALTER TABLE identities ADD COLUMN \"{}\" TEXT",
            "x".repeat(MAX_SQLITE_VALUE as usize + 1)
        ),
        format!(
            "CREATE TABLE oversized_columns ({}) STRICT",
            (0..MAX_COLUMNS + 1)
                .map(|index| format!("c{index} TEXT"))
                .collect::<Vec<_>>()
                .join(",")
        ),
    ] {
        let image = Image::new();
        image.sql(&declaration);
        require_corrupt(image.inspect());
    }
}

struct Watched {
    file: std::fs::File,
    read: u64,
    cancel: Option<Arc<AtomicBool>>,
    fault: bool,
    fired: Arc<AtomicBool>,
    drops: Arc<std::sync::atomic::AtomicUsize>,
}
impl Read for Watched {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.read >= 8192 {
            if let Some(cancel) = &self.cancel {
                cancel.store(true, Ordering::Release);
                self.fired.store(true, Ordering::Release);
            }
            if self.fault {
                self.fired.store(true, Ordering::Release);
                return Err(io::ErrorKind::PermissionDenied.into());
            }
        }
        let read = self.file.read(buffer)?;
        self.read += read as u64;
        Ok(read)
    }
}
impl Seek for Watched {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.file.seek(to)
    }
}
impl Drop for Watched {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn cancellation_faults_and_work_exhaustion_return_no_observation_and_release_reader() {
    let image = Image::new();
    assert_eq!(
        inspect(
            Box::new(std::fs::File::open(&image.path).unwrap()),
            &test_seams(real_fs()),
            &AtomicBool::new(true)
        )
        .unwrap(),
        None
    );
    for fault in [false, true] {
        let cancel = Arc::new(AtomicBool::new(false));
        let fired = Arc::new(AtomicBool::new(false));
        let drops = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let closed = crate::image_vfs::closes_on_this_thread();
        let result = inspect(
            Box::new(Watched {
                file: std::fs::File::open(&image.path).unwrap(),
                read: 0,
                cancel: (!fault).then(|| cancel.clone()),
                fault,
                fired: fired.clone(),
                drops: drops.clone(),
            }),
            &test_seams(real_fs()),
            &cancel,
        );
        assert!(
            fired.load(Ordering::Acquire),
            "the fault/cancel path never fired"
        );
        if fault {
            assert!(
                matches!(result, Err(StoreError::Unavailable { .. })),
                "{result:?}"
            );
        } else {
            assert_eq!(result.unwrap(), None);
        }
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert_eq!(crate::image_vfs::closes_on_this_thread(), closed + 1);
    }
    require_corrupt(inspect_with_budget(
        Box::new(std::fs::File::open(&image.path).unwrap()),
        &test_seams(real_fs()),
        &AtomicBool::new(false),
        1,
    ));
}

#[test]
fn retained_identity_survives_name_replacement_and_sidecars_are_ignored() {
    let image = Image::new();
    let file = std::fs::File::open(&image.path).unwrap();
    let moved = image.path.with_extension("retained");
    std::fs::rename(&image.path, &moved).unwrap();
    std::fs::write(&image.path, b"path replacement").unwrap();
    let wal = image.path.with_file_name("state.sqlite-wal");
    let journal = image.path.with_file_name("state.sqlite-journal");
    std::fs::write(&wal, b"untrusted WAL").unwrap();
    std::fs::write(&journal, b"untrusted journal").unwrap();
    let before = std::fs::read(&moved).unwrap();
    assert!(
        inspect(
            Box::new(file),
            &test_seams(real_fs()),
            &AtomicBool::new(false)
        )
        .unwrap()
        .is_some()
    );
    assert_eq!(std::fs::read(&moved).unwrap(), before);
    assert_eq!(std::fs::read(&image.path).unwrap(), b"path replacement");
    assert_eq!(std::fs::read(&wal).unwrap(), b"untrusted WAL");
    assert_eq!(std::fs::read(&journal).unwrap(), b"untrusted journal");
}
