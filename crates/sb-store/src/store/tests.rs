//! Acceptance tests for `open` (plan M0.3). They live in-crate so they
//! can read the private connection. The cross-process tests re-execute
//! this test binary; see `child_process`.

use std::env;
use std::fs;
use std::io::{self, BufRead, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use sb_test_support::{ScratchDir, block_on, real_fs};
use sb_types::fs::{Fs, Location};

use super::*;
use crate::testing::{FIXED_NOW_MS, crash_write, scratch_paths, test_seams};

/// A schema version past the app's: a store "newer than the app".
const NEWER_VERSION: u32 = crate::schema::LATEST_SCHEMA_VERSION + 1;

/// `open` on a fresh scratch store.
fn open_scratch(label: &str) -> (ScratchDir, StorePaths, Store) {
    let (scratch, paths) = scratch_paths(label);
    let store = open(&paths, &test_seams(real_fs())).unwrap();
    (scratch, paths, store)
}

/// The linked SQLite matches the pin and the release bounds (spec/03).
#[test]
fn linked_sqlite_matches_pin_and_bounds() {
    let number = rusqlite::version_number();
    assert!(
        number >= SQLITE_VERSION_FLOOR,
        "linked SQLite {number} is below the floor {SQLITE_VERSION_FLOOR}"
    );
    assert_ne!(
        number, SQLITE_VERSION_WITHDRAWN,
        "linked SQLite is the withdrawn 3.52.0"
    );

    let conn = Connection::open_in_memory().unwrap();
    let version: String = conn
        .query_row("SELECT sqlite_version()", [], |r| r.get(0))
        .unwrap();
    let source_id: String = conn
        .query_row("SELECT sqlite_source_id()", [], |r| r.get(0))
        .unwrap();
    assert_eq!(version, PINNED_SQLITE.version);
    assert_eq!(source_id, PINNED_SQLITE.source_id);
    assert_eq!(number, PINNED_SQLITE.version_number);
}

/// FTS5 is compiled in and works (spec/03).
#[test]
fn fts5_works() {
    let conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("CREATE VIRTUAL TABLE t USING fts5(body)")
        .unwrap();
    conn.execute("INSERT INTO t (body) VALUES ('hello world')", [])
        .unwrap();
    let hits: i64 = conn
        .query_row("SELECT count(*) FROM t WHERE t MATCH 'hello'", [], |r| {
            r.get(0)
        })
        .unwrap();
    assert_eq!(hits, 1);
}

/// `PRAGMA compile_options` shows the spec/03 build flags, asserted as
/// SQLite prints them: `DEFAULT_FOREIGN_KEYS` and `STRICT_SUBTYPE` are
/// bare (ctime.c prints no value for them), the others carry `=value`.
#[test]
fn compile_options_show_the_spec_03_flags() {
    let conn = Connection::open_in_memory().unwrap();
    let options: Vec<String> = conn
        .prepare("PRAGMA compile_options")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    for expected in [
        "DQS=0",
        "DEFAULT_FOREIGN_KEYS",
        "DEFAULT_WAL_SYNCHRONOUS=2",
        "STRICT_SUBTYPE",
        "ENABLE_FTS5",
    ] {
        assert!(
            options.iter().any(|option| option == expected),
            "compile option {expected} missing from {options:?}"
        );
    }
}

/// After open, every setting reads back its spec/03 value — on the
/// writer's own connection, so through a write closure (spec/01: only
/// the writer's thread touches that connection).
#[test]
fn settings_stick_after_open() {
    let (_scratch, _paths, store) = open_scratch("settings");
    let read_pragma = |name: &str| pragma_on_writer(&store, name);
    let journal: String = block_on(store.write(crate::WritePriority::User, |tx| {
        tx.query_row("PRAGMA journal_mode", [], |r| r.get(0))
            .map_err(crate::schema::bug)
    }))
    .unwrap();
    assert_eq!(journal, "wal");
    let mut expected_settings = vec![
        ("synchronous", 2),
        ("foreign_keys", 1),
        ("trusted_schema", 0),
        ("busy_timeout", 5000),
        ("wal_autocheckpoint", 1000),
    ];
    if cfg!(target_os = "macos") {
        expected_settings.extend([("fullfsync", 1), ("checkpoint_fullfsync", 1)]);
    }
    for (pragma, expected) in expected_settings {
        assert_eq!(read_pragma(pragma), expected, "PRAGMA {pragma}");
    }
}

/// One pragma's value on the writer's own connection, through a write
/// closure (spec/01: only the writer's thread touches that connection).
fn pragma_on_writer(store: &Store, name: &str) -> i64 {
    let sql = format!("PRAGMA {name}");
    block_on(store.write(crate::WritePriority::User, move |tx| {
        tx.query_row(&sql, [], |r| r.get(0))
            .map_err(crate::schema::bug)
    }))
    .unwrap()
}

/// DQS is off: a double-quoted string that isn't a column errors, a
/// real column name still works (spec/03).
#[test]
fn dqs_rejects_bare_double_quoted_strings() {
    let (_scratch, _paths, store) = open_scratch("dqs");
    let err = block_on(store.write(crate::WritePriority::User, |tx| {
        tx.query_row("SELECT \"x\"", [], |r| r.get::<_, String>(0))
            .map_err(crate::schema::bug)?;
        Ok(())
    }))
    .unwrap_err();
    assert!(err.to_string().contains("no such column"), "{err}");

    block_on(store.write(crate::WritePriority::User, |tx| {
        tx.execute_batch("CREATE TABLE t(a); INSERT INTO t VALUES ('v')")
            .map_err(crate::schema::bug)?;
        let value: String = tx
            .query_row("SELECT \"a\" FROM t", [], |r| r.get(0))
            .map_err(crate::schema::bug)?;
        Ok(value)
    }))
    .map(|value| assert_eq!(value, "v"))
    .unwrap();
}

// ---------------------------------------------------------------------------
// Cross-process ownership (spec/03 "One process owns the store").
//

const CHILD_ROLE_ENV: &str = "SB_STORE_TEST_CHILD_ROLE";
const CHILD_ROOT_ENV: &str = "SB_STORE_TEST_CHILD_ROOT";
const READY_LINE: &str = "SB_STORE_CHILD_READY";

/// The child half of the cross-process tests. Spawned by
/// `spawn_child` with `--exact`; prints the ready line, then blocks on
/// stdin until the parent kills it (releasing the lock by dying).
#[test]
#[ignore = "spawned by the cross-process tests"]
fn child_process() {
    let root = env::var(CHILD_ROOT_ENV).expect("data root env");
    let paths = StorePaths::new(&root).expect("absolute data root");
    // Held for the whole function, so the lock outlives the ready line.
    let held = match env::var(CHILD_ROLE_ENV).expect("role env").as_str() {
        // Open the store and hold it.
        "open-store" => ChildHold::Store(Box::new(
            open(&paths, &test_seams(real_fs())).expect("child open"),
        )),
        // Hold only the lock, before any store exists.
        "hold-lock" => ChildHold::Lease(
            real_fs()
                .lock_exclusive(paths.lock_file())
                .expect("child lock"),
        ),
        role => panic!("unknown role {role}"),
    };
    println!("{READY_LINE}");
    io::stdout().flush().unwrap();
    let mut line = String::new();
    let _ = io::stdin().read_line(&mut line);
    drop(held);
}

/// What the child process holds while blocking on stdin. The fields
/// exist to be dropped (releasing the lease), never to be read. The
/// store rides in a box: it is far larger than a lease.
#[allow(dead_code)]
enum ChildHold {
    Store(Box<Store>),
    Lease(Box<dyn sb_types::fs::Lease>),
}

/// A child holding an open store, from its ready line to its death.
struct ChildGuard(Child);

impl Drop for ChildGuard {
    fn drop(&mut self) {
        let _ = self.0.kill();
        let _ = self.0.wait();
    }
}

/// Re-execute this test binary as `child_process` with `role` and the
/// data root, waiting for its ready line.
fn spawn_child(role: &str, data_root: &Path) -> ChildGuard {
    let mut child = Command::new(env::current_exe().unwrap())
        .args([
            "--exact",
            "store::tests::child_process",
            "--ignored",
            "--nocapture",
        ])
        .env_clear()
        .env(CHILD_ROLE_ENV, role)
        .env(CHILD_ROOT_ENV, data_root)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::inherit())
        .spawn()
        .expect("spawn child");
    // The harness prints its own banner lines (starting with an empty
    // one); scan for the ready line instead of reading the first.
    let ready = {
        let stdout = child.stdout.take().unwrap();
        let mut ready = String::new();
        for line in BufReader::new(stdout).lines() {
            let line = line.unwrap();
            if line == READY_LINE {
                ready = line;
                break;
            }
        }
        ready
    };
    assert_eq!(ready, READY_LINE, "child did not report ready");
    ChildGuard(child)
}

/// Names and sizes of everything under `root`, recursively, sorted —
/// to prove a failed open creates and touches nothing.
fn recursive_listing(root: &Path) -> Vec<(PathBuf, u64)> {
    let mut listing = Vec::new();
    let mut stack = vec![root.to_path_buf()];
    while let Some(dir) = stack.pop() {
        for entry in fs::read_dir(&dir).unwrap() {
            let entry = entry.unwrap();
            let metadata = entry.metadata().unwrap();
            if metadata.is_dir() {
                stack.push(entry.path());
            }
            listing.push((entry.path(), metadata.len()));
        }
    }
    listing.sort();
    listing
}

/// While another process holds the store, other spellings of the same
/// root are `Locked` too: a trailing separator, a `.` component, a
/// symlink to it. None creates a lock file inside the root.
#[test]
fn other_spellings_of_a_held_store_are_locked() {
    let (scratch, paths) = scratch_paths("cross-proc-spellings");
    let root = paths.data_root().to_path_buf();
    let _child = spawn_child("open-store", &root);

    let alias = scratch.path().join("alias");
    std::os::unix::fs::symlink(&root, &alias).unwrap();
    let mut trailing = root.clone().into_os_string();
    trailing.push("/");
    for spelling in [
        PathBuf::from(trailing),
        scratch.path().join(".").join("store"),
        alias,
    ] {
        let spelled = StorePaths::new(&spelling).unwrap();
        assert!(
            matches!(
                open(&spelled, &test_seams(real_fs())),
                Err(StoreError::Locked)
            ),
            "{} was not Locked",
            spelling.display()
        );
    }
    assert!(!root.join(".lock").exists(), "a lock file inside the root");
}

/// A second process holding the store: `Locked`, nothing changed, and
/// after it dies the store opens.
#[test]
fn other_process_holding_the_store_yields_locked() {
    let (_scratch, paths) = scratch_paths("cross-proc-store");
    let root = paths.data_root().to_path_buf();
    let _child = spawn_child("open-store", &root);

    let before = recursive_listing(&root);
    assert!(
        before
            .iter()
            .any(|(path, _)| path.ends_with("state.sqlite")),
        "child should have created the store"
    );
    assert!(matches!(
        open(&paths, &test_seams(real_fs())),
        Err(StoreError::Locked)
    ));
    assert_eq!(
        recursive_listing(&root),
        before,
        "failed open changed files"
    );

    drop(_child);
    open(&paths, &test_seams(real_fs())).unwrap();
}

/// A second process holding only the lock, before any store exists:
/// `Locked`, and the data root is still not created.
#[test]
fn other_process_holding_only_the_lock_yields_locked() {
    let (_scratch, paths) = scratch_paths("cross-proc-lock");
    let root = paths.data_root().to_path_buf();
    let _child = spawn_child("hold-lock", &root);

    assert!(!root.exists());
    assert!(matches!(
        open(&paths, &test_seams(real_fs())),
        Err(StoreError::Locked)
    ));
    assert!(!root.exists(), "failed open created the data root");

    drop(_child);
    open(&paths, &test_seams(real_fs())).unwrap();
}

/// Same process, second open: `Locked` until `close`.
#[test]
fn same_process_second_open_is_locked_until_close() {
    let (_scratch, paths, store) = open_scratch("same-proc");
    assert!(matches!(
        open(&paths, &test_seams(real_fs())),
        Err(StoreError::Locked)
    ));
    store.close().unwrap();
    assert!(open(&paths, &test_seams(real_fs())).is_ok());
}

// ---------------------------------------------------------------------------
// Refused opens.
//

/// A reader that can't open fails the open before the writer starts,
/// so the writer's connection closes without checkpointing: WAL frames
/// a crash left stay in the WAL, `state.sqlite` keeps its bytes, and
/// the lease is released.
#[test]
fn a_reader_failing_to_open_leaves_crashed_frames_in_the_wal() {
    let (_scratch, paths, store) = open_scratch("reader-open-fails");
    store.close().unwrap();
    crash_write(&paths, "INSERT INTO store_meta VALUES ('crash', 'frame');");
    let before = fs::read(paths.state_db()).unwrap();
    let mut readers = READER_PRAGMAS.to_vec();
    readers[0].expect = Expect::Text("delete");
    let err = open_with(
        &paths,
        &test_seams(real_fs()),
        WRITER_PRAGMAS,
        &readers,
        &PINNED_SQLITE,
        MIGRATIONS,
    )
    .unwrap_err();
    assert!(matches!(err, StoreError::Unavailable { .. }), "{err:?}");
    assert_eq!(
        fs::read(paths.state_db()).unwrap(),
        before,
        "the failed open checkpointed crashed frames"
    );
    open(&paths, &test_seams(real_fs())).unwrap();
}

/// A settings row that cannot read back refuses the open with
/// `Unavailable` naming the pragma, and the lease is released.
#[test]
fn pragma_mismatch_is_unavailable_and_releases_the_lease() {
    let (_scratch, paths) = scratch_paths("pragma-mismatch");
    // journal_mode always reads back "wal" once set; expecting "delete"
    // can never match.
    let mut pragmas = WRITER_PRAGMAS.to_vec();
    pragmas[0].expect = Expect::Text("delete");
    match open_with(
        &paths,
        &test_seams(real_fs()),
        &pragmas,
        READER_PRAGMAS,
        &PINNED_SQLITE,
        MIGRATIONS,
    )
    .unwrap_err()
    {
        StoreError::Unavailable { reason, .. } => {
            assert!(reason.contains("journal_mode"), "{reason}");
        }
        err => panic!("expected Unavailable, got {err:?}"),
    }
    open(&paths, &test_seams(real_fs())).unwrap();
}

/// The verdict's connection can't write and doesn't trust the schema
/// of the file it judges.
#[test]
fn the_verdict_connection_is_read_only_and_distrusts_the_schema() {
    let (_scratch, paths, store) = open_scratch("read-only-verdict");
    store.close().unwrap();
    let conn = verdict_connection(paths.state_db()).unwrap();
    let trusted: i64 = conn
        .pragma_query_value(None, "trusted_schema", |row| row.get(0))
        .unwrap();
    assert_eq!(trusted, 0);
    assert!(conn.execute_batch("PRAGMA user_version = 9").is_err());
}

/// The build is checked before the schema verdict (spec/02): a wrong
/// build is `Internal` even for an existing store the verdict would
/// refuse as newer.
#[test]
fn version_mismatch_is_internal_before_the_schema_verdict() {
    let (_scratch, paths, store) = open_scratch("version-before-verdict");
    store.close().unwrap();
    let raw = rusqlite::Connection::open(paths.state_db()).unwrap();
    raw.execute_batch(&format!("PRAGMA user_version = {NEWER_VERSION};"))
        .unwrap();
    raw.close().unwrap();

    let wrong = PinnedSqlite {
        source_id: "another build of the same version",
        ..PINNED_SQLITE
    };
    let err = open_with(
        &paths,
        &test_seams(real_fs()),
        WRITER_PRAGMAS,
        crate::store::READER_PRAGMAS,
        &wrong,
        MIGRATIONS,
    )
    .unwrap_err();
    assert!(matches!(err, StoreError::Internal { .. }), "{err:?}");
}

/// A linked SQLite other than the pin refuses the open with
/// `Internal`.
#[test]
fn version_mismatch_is_internal() {
    let (_scratch, paths) = scratch_paths("version-mismatch");
    let wrong = PinnedSqlite {
        version: "9.9.9",
        version_number: 9_009_009,
        source_id: "not the pinned source id",
    };
    match open_with(
        &paths,
        &test_seams(real_fs()),
        WRITER_PRAGMAS,
        crate::store::READER_PRAGMAS,
        &wrong,
        MIGRATIONS,
    )
    .unwrap_err()
    {
        StoreError::Internal { detail } => {
            assert!(detail.contains("9.9.9"), "{detail}");
        }
        err => panic!("expected Internal, got {err:?}"),
    }

    // The source id alone differing (same version) is refused too.
    let other_source = PinnedSqlite {
        source_id: "another build of the same version",
        ..PINNED_SQLITE
    };
    match open_with(
        &paths,
        &test_seams(real_fs()),
        WRITER_PRAGMAS,
        READER_PRAGMAS,
        &other_source,
        MIGRATIONS,
    )
    .unwrap_err()
    {
        StoreError::Internal { detail } => {
            assert!(detail.contains("another build"), "{detail}");
        }
        err => panic!("expected Internal, got {err:?}"),
    }
    open(&paths, &test_seams(real_fs())).unwrap();
}

/// The real `Fs` with test hooks: a fixed `location`, a `sync_dir`
/// that fails for one path, and a log of each `sync_dir` with whether
/// `state.sqlite` existed at that moment.
struct HookedFs {
    inner: Arc<dyn Fs>,
    location: Option<Location>,
    failing_sync: Option<PathBuf>,
    state_db: PathBuf,
    syncs: Mutex<Vec<(PathBuf, bool)>>,
}

impl HookedFs {
    fn new(paths: &StorePaths) -> Self {
        Self {
            inner: real_fs(),
            location: None,
            failing_sync: None,
            state_db: paths.state_db().to_path_buf(),
            syncs: Mutex::new(Vec::new()),
        }
    }
}

impl Fs for HookedFs {
    fn location(&self, path: &Path) -> io::Result<Location> {
        match &self.location {
            Some(location) => Ok(location.clone()),
            None => self.inner.location(path),
        }
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        // Observed directly, not through the seam.
        let state_db_exists = self.state_db.exists();
        self.syncs
            .lock()
            .unwrap()
            .push((path.to_path_buf(), state_db_exists));
        if self.failing_sync.as_deref() == Some(path) {
            return Err(io::Error::other("injected flush failure"));
        }
        self.inner.sync_dir(path)
    }

    fn create(&self, path: &Path) -> io::Result<Box<dyn sb_types::fs::WriteFile>> {
        self.inner.create(path)
    }
    fn open(&self, path: &Path) -> io::Result<Box<dyn sb_types::fs::ReadFile>> {
        self.inner.open(path)
    }
    fn open_regular(
        &self,
        path: &Path,
    ) -> io::Result<(Box<dyn sb_types::fs::ReadFile>, sb_types::fs::Metadata)> {
        self.inner.open_regular(path)
    }
    fn metadata(&self, path: &Path) -> io::Result<Option<sb_types::fs::Metadata>> {
        self.inner.metadata(path)
    }
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        self.inner.read_dir(path)
    }
    fn create_dir(&self, path: &Path) -> io::Result<()> {
        self.inner.create_dir(path)
    }
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        self.inner.rename(from, to)
    }
    fn remove(&self, path: &Path) -> io::Result<()> {
        self.inner.remove(path)
    }
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        self.inner.canonicalize(path)
    }
    fn lock_exclusive(
        &self,
        path: &Path,
    ) -> Result<Box<dyn sb_types::fs::Lease>, sb_types::fs::LockError> {
        self.inner.lock_exclusive(path)
    }
}

/// Synced and network locations are refused with `Unavailable` naming
/// the service or filesystem, and nothing is created.
#[test]
fn non_local_locations_are_unavailable_and_create_nothing() {
    for location in [
        Location::Synced {
            service: "iCloud Drive".to_owned(),
        },
        Location::Network {
            filesystem: "NFS".to_owned(),
        },
    ] {
        let (_scratch, paths) = scratch_paths("location");
        let named = match &location {
            Location::Synced { service } => service.clone(),
            Location::Network { filesystem } => filesystem.clone(),
            Location::Local => unreachable!(),
        };
        let fs = HookedFs {
            location: Some(location),
            ..HookedFs::new(&paths)
        };
        match open(&paths, &test_seams(Arc::new(fs))).unwrap_err() {
            StoreError::Unavailable { reason, .. } => {
                assert!(reason.contains(&named), "{reason}");
            }
            err => panic!("expected Unavailable, got {err:?}"),
        }
        assert!(!paths.data_root().exists(), "data root was created");
    }
}

/// A `state.sqlite` that isn't a database is `Corrupt` and its bytes
/// are untouched.
#[test]
fn non_database_state_sqlite_is_corrupt_and_untouched() {
    let (_scratch, paths, store) = open_scratch("corrupt");
    store.close().unwrap();

    let bytes = b"this is not a SQLite database, not even a little bit".repeat(4);
    fs::write(paths.state_db(), &bytes).unwrap();
    match open(&paths, &test_seams(real_fs())).unwrap_err() {
        StoreError::Corrupt { detail } => {
            assert!(detail.contains("state.sqlite"), "{detail}");
        }
        err => panic!("expected Corrupt, got {err:?}"),
    }
    assert_eq!(
        fs::read(paths.state_db()).unwrap(),
        bytes,
        "the corrupt file must stay byte-for-byte untouched"
    );
}

/// A non-empty `state.sqlite` shorter than a SQLite header is
/// `Corrupt` and untouched; SQLite alone would take a 1-byte file for
/// an empty database and overwrite it.
#[test]
fn short_state_sqlite_is_corrupt_and_untouched() {
    let (_scratch, paths, store) = open_scratch("corrupt-short");
    store.close().unwrap();

    for len in [1, 99] {
        let bytes = vec![b'!'; len];
        fs::write(paths.state_db(), &bytes).unwrap();
        match open(&paths, &test_seams(real_fs())).unwrap_err() {
            StoreError::Corrupt { .. } => {}
            err => panic!("expected Corrupt for {len} bytes, got {err:?}"),
        }
        assert_eq!(fs::read(paths.state_db()).unwrap(), bytes, "{len} bytes");
    }
}

// ---------------------------------------------------------------------------
// Layout (spec/03 "On-disk layout").
//

/// A fresh store has all five layout directories.
#[test]
fn new_store_has_the_five_layout_directories() {
    let (_scratch, paths, _store) = open_scratch("layout-new");
    for dir in paths.layout_dirs() {
        assert!(
            fs::metadata(dir).unwrap().is_dir(),
            "{} is missing",
            dir.display()
        );
    }
}

/// An existing store missing a layout directory is `Corrupt`, and the
/// directory is not recreated.
#[test]
fn existing_store_missing_a_layout_directory_is_corrupt() {
    let (_scratch, paths, store) = open_scratch("layout-missing");
    store.close().unwrap();
    fs::remove_dir(paths.objects_tmp()).unwrap();

    match open(&paths, &test_seams(real_fs())).unwrap_err() {
        StoreError::Corrupt { detail } => {
            assert!(detail.contains("tmp"), "{detail}");
        }
        err => panic!("expected Corrupt, got {err:?}"),
    }
    assert!(
        !paths.objects_tmp().exists(),
        "the missing directory must not be recreated"
    );
}

/// A non-empty unrelated data root is refused with `Unavailable` and
/// left untouched; so is a data root that is a file.
#[test]
fn unrelated_data_root_is_unavailable_and_untouched() {
    let (_scratch, paths) = scratch_paths("layout-stray");
    let root = paths.data_root();
    fs::create_dir(root).unwrap();
    fs::write(root.join("stray.txt"), b"mine").unwrap();
    match open(&paths, &test_seams(real_fs())).unwrap_err() {
        StoreError::Unavailable { reason, .. } => {
            assert!(reason.contains("stray.txt"), "{reason}");
        }
        err => panic!("expected Unavailable, got {err:?}"),
    }
    assert_eq!(fs::read(root.join("stray.txt")).unwrap(), b"mine");

    let (_scratch, paths) = scratch_paths("layout-file-root");
    fs::write(paths.data_root(), b"a file").unwrap();
    match open(&paths, &test_seams(real_fs())).unwrap_err() {
        StoreError::Unavailable { .. } => {}
        err => panic!("expected Unavailable, got {err:?}"),
    }
    assert_eq!(fs::read(paths.data_root()).unwrap(), b"a file");
}

/// An interrupted creation (only `objects/` under the data root) is
/// completed.
#[test]
fn interrupted_creation_is_completed() {
    let (_scratch, paths) = scratch_paths("layout-interrupted");
    fs::create_dir(paths.data_root()).unwrap();
    fs::create_dir(paths.objects()).unwrap();
    open(&paths, &test_seams(real_fs())).unwrap();
    for dir in paths.layout_dirs() {
        assert!(fs::metadata(dir).unwrap().is_dir(), "{}", dir.display());
    }
}

/// A writable open creates the data root's missing ancestors, each
/// flushed into its parent before the store is built (spec/03), so a
/// default root works on a fresh system.
#[test]
fn missing_ancestors_are_created_and_flushed() {
    let scratch = ScratchDir::new("no-parent");
    let paths = StorePaths::new(scratch.path().join("no/such/parent/store")).unwrap();
    let fs = Arc::new(HookedFs::new(&paths));
    open(&paths, &test_seams(fs.clone()))
        .unwrap()
        .close()
        .unwrap();

    let syncs: Vec<PathBuf> = fs
        .syncs
        .lock()
        .unwrap()
        .iter()
        .map(|(p, _)| p.clone())
        .collect();
    let expected_first = [
        scratch.path().to_path_buf(),
        scratch.path().join("no"),
        scratch.path().join("no/such"),
    ];
    assert_eq!(
        syncs[..3],
        expected_first,
        "ancestors not flushed in order: {syncs:?}"
    );
    assert!(paths.state_db().exists());
}

/// Creation flushes each layout directory, then the data root, then
/// its parent — all before `state.sqlite` exists.
#[test]
fn creation_flushes_in_spec_order_before_state_sqlite() {
    let (scratch, paths) = scratch_paths("layout-order");
    let fs = Arc::new(HookedFs::new(&paths));

    open(&paths, &test_seams(fs.clone())).unwrap();

    let mut expected: Vec<PathBuf> = paths
        .layout_dirs()
        .iter()
        .map(|d| d.to_path_buf())
        .collect();
    expected.push(paths.data_root().to_path_buf());
    expected.push(scratch.path().to_path_buf());
    let recorded = fs.syncs.lock().unwrap().clone();
    let synced: Vec<PathBuf> = recorded.iter().map(|(path, _)| path.clone()).collect();
    assert_eq!(
        synced[..expected.len()],
        expected,
        "unexpected sync_dir order"
    );
    // The scratch dir holds only the store and its lock, so the chain
    // goes on up: each further flush is the parent of the one before.
    for pair in synced[expected.len() - 1..].windows(2) {
        assert_eq!(Some(pair[1].as_path()), pair[0].parent(), "{synced:?}");
    }
    assert!(
        recorded.iter().all(|(_, state_db)| !state_db),
        "a sync_dir ran after state.sqlite existed: {recorded:?}"
    );
}

/// A failed flush during creation is `Unavailable`, `state.sqlite`
/// isn't created, the lease is released, and a retry completes.
#[test]
fn failed_creation_flush_is_unavailable_and_retryable() {
    let (scratch, paths) = scratch_paths("layout-flush-fail");
    for failing in [paths.objects_sha256(), paths.data_root(), scratch.path()] {
        let fs = HookedFs {
            failing_sync: Some(failing.to_path_buf()),
            ..HookedFs::new(&paths)
        };
        match open(&paths, &test_seams(Arc::new(fs))).unwrap_err() {
            StoreError::Unavailable { reason, .. } => {
                assert!(reason.contains("injected flush failure"), "{reason}");
            }
            err => panic!("expected Unavailable, got {err:?}"),
        }
        assert!(!paths.state_db().exists(), "state.sqlite created");
    }
    open(&paths, &test_seams(real_fs())).unwrap();
}

// ---------------------------------------------------------------------------
// StorePaths input checks.
//

/// The lock file is the data root's sibling however the root is
/// spelled (spec/03).
#[test]
fn lock_file_is_a_sibling_for_every_spelling() {
    for root in ["/a/store", "/a/store/", "/a/./store", "/a/store/."] {
        let paths = StorePaths::new(root).unwrap();
        assert_eq!(paths.data_root(), Path::new("/a/store"), "{root}");
        assert_eq!(paths.lock_file(), Path::new("/a/store.lock"), "{root}");
    }
}

/// A relative data root (or one without a final name) is
/// `InvalidInput`.
#[test]
fn relative_data_root_is_invalid_input() {
    for root in ["relative/path", "/"] {
        match StorePaths::new(root).unwrap_err() {
            StoreError::InvalidInput { .. } => {}
            err => panic!("expected InvalidInput for {root}, got {err:?}"),
        }
    }
}

// ---------------------------------------------------------------------------
// The read-only open (plan M0.6; spec/02 "Migrations").
//

/// `store_meta` through a read-only open, as a map.
fn read_only_meta(store: &ReadOnlyStore) -> std::collections::BTreeMap<String, String> {
    store
        .read(|tx| {
            let mut stmt = tx.prepare("SELECT key, value FROM store_meta").unwrap();
            let rows = stmt
                .query_map([], |row| {
                    Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
                })
                .unwrap()
                .collect::<Result<std::collections::BTreeMap<_, _>, _>>()
                .unwrap();
            Ok(rows)
        })
        .unwrap()
}

/// 8. A store with a `user_version` above the latest opens read-only:
///    `read` sees `store_meta`, and after `close` `state.sqlite`'s
///    bytes are unchanged — also when a crash left WAL frames (the
///    crashed-writer pattern from `schema/tests.rs`).
#[test]
fn a_newer_store_opens_read_only_and_stays_unchanged() {
    let (scratch, paths) = scratch_paths("read-only-newer");
    open(&paths, &test_seams(real_fs()))
        .unwrap()
        .close()
        .unwrap();
    let raw = rusqlite::Connection::open(paths.state_db()).unwrap();
    raw.execute_batch(&format!("PRAGMA user_version = {NEWER_VERSION};"))
        .unwrap();
    raw.close().unwrap();

    let before = fs::read(paths.state_db()).unwrap();
    let read_only = open_read_only(&paths, &test_seams(real_fs())).unwrap();
    let meta = read_only_meta(&read_only);
    assert!(
        meta.contains_key("store_uuid"),
        "the read-only open did not see store_meta: {meta:?}"
    );
    read_only.close().unwrap();
    assert_eq!(
        fs::read(paths.state_db()).unwrap(),
        before,
        "the read-only open changed state.sqlite"
    );
    // SQLite may leave an empty WAL and its shared-memory index, as
    // for any WAL reader; the WAL holds nothing written.
    let wal = paths.data_root().join("state.sqlite-wal");
    assert!(
        fs::metadata(&wal).map_or(true, |md| md.len() == 0),
        "the read-only open wrote to the WAL"
    );
    drop(scratch);
}

/// A newer app may change `store_meta`: a newer store whose
/// `store_meta` isn't migration 1's still opens read-only.
#[test]
fn a_newer_store_with_a_changed_store_meta_opens_read_only() {
    let (scratch, paths) = scratch_paths("read-only-newer-meta");
    open(&paths, &test_seams(real_fs()))
        .unwrap()
        .close()
        .unwrap();
    let raw = rusqlite::Connection::open(paths.state_db()).unwrap();
    raw.execute_batch(&format!(
        "ALTER TABLE store_meta ADD COLUMN note TEXT; PRAGMA user_version = {NEWER_VERSION};"
    ))
    .unwrap();
    raw.close().unwrap();

    let read_only = open_read_only(&paths, &test_seams(real_fs())).unwrap();
    assert!(read_only_meta(&read_only).contains_key("store_uuid"));
    read_only.close().unwrap();
    drop(scratch);
}

/// A read-only store refuses writes: its readers are read-only.
#[test]
fn a_read_only_store_refuses_writes() {
    let (scratch, paths) = scratch_paths("read-only-refuses");
    open(&paths, &test_seams(real_fs()))
        .unwrap()
        .close()
        .unwrap();
    let read_only = open_read_only(&paths, &test_seams(real_fs())).unwrap();
    let refused = read_only.read(|tx| {
        tx.execute_batch("CREATE TABLE must_not_exist (x INTEGER) STRICT;")
            .map_err(|e| StoreError::internal(e.to_string()))
    });
    assert!(refused.is_err(), "a read-only store accepted a write");
    read_only.close().unwrap();
    drop(scratch);
}

/// 8 with a crash: WAL frames a dead writer left behind survive the
/// read-only open and close, and `state.sqlite` keeps its bytes —
/// no reader checkpoints them into the file.
#[test]
fn a_newer_store_with_crashed_wal_frames_opens_read_only_untouched() {
    let (scratch, paths) = scratch_paths("read-only-newer-crash");
    open(&paths, &test_seams(real_fs()))
        .unwrap()
        .close()
        .unwrap();
    crash_write(
        &paths,
        &format!(
            "PRAGMA user_version = {NEWER_VERSION};
             UPDATE store_meta SET value = '9.9.9' WHERE key = 'last_migrated_by';"
        ),
    );

    let before = fs::read(paths.state_db()).unwrap();
    let wal_before = fs::read(paths.data_root().join("state.sqlite-wal")).unwrap();
    let read_only = open_read_only(&paths, &test_seams(real_fs())).unwrap();
    // The crashed frames are what made this store newer: `read` sees
    // the newer version they carry, through the read-only connections.
    let version = read_only
        .read(|tx| {
            tx.query_row("PRAGMA user_version", [], |row| row.get::<_, i64>(0))
                .map_err(|e| StoreError::internal(e.to_string()))
        })
        .unwrap();
    assert_eq!(version, i64::from(NEWER_VERSION));
    read_only.close().unwrap();
    assert_eq!(
        fs::read(paths.state_db()).unwrap(),
        before,
        "the read-only open changed state.sqlite"
    );
    assert_eq!(
        fs::read(paths.data_root().join("state.sqlite-wal")).unwrap(),
        wal_before,
        "a reader checkpointed the crashed WAL frames"
    );
    drop(scratch);
}

/// 8. A current store opens read-only too, and closes unchanged.
#[test]
fn a_current_store_opens_read_only() {
    let (scratch, paths) = scratch_paths("read-only-current");
    open(&paths, &test_seams(real_fs()))
        .unwrap()
        .close()
        .unwrap();

    let before = fs::read(paths.state_db()).unwrap();
    let read_only = open_read_only(&paths, &test_seams(real_fs())).unwrap();
    assert!(read_only_meta(&read_only).contains_key("incarnation"));
    read_only.close().unwrap();
    assert_eq!(fs::read(paths.state_db()).unwrap(), before);
    drop(scratch);
}

/// 8. An absent store is `NotFound` and nothing is created.
#[test]
fn an_absent_store_is_not_found_and_nothing_is_created() {
    let (scratch, paths) = scratch_paths("read-only-absent");
    match open_read_only(&paths, &test_seams(real_fs())).unwrap_err() {
        StoreError::NotFound { what } => assert!(what.contains("data root"), "{what}"),
        err => panic!("expected NotFound, got {err:?}"),
    }
    assert_eq!(
        recursive_listing(scratch.path()),
        Vec::new(),
        "the read-only open created something (the lock file?)"
    );
    drop(scratch);

    // An absent parent too: still `NotFound`, still nothing created.
    let scratch = ScratchDir::new("read-only-absent-parent");
    let paths = StorePaths::new(scratch.path().join("missing/parent/store")).unwrap();
    assert!(matches!(
        open_read_only(&paths, &test_seams(real_fs())),
        Err(StoreError::NotFound { .. })
    ));
    assert_eq!(recursive_listing(scratch.path()), Vec::new());
    drop(scratch);

    // A data root without `state.sqlite` is `NotFound` too, and no
    // lock appears beside it.
    let (scratch, paths) = scratch_paths("read-only-no-db");
    fs::create_dir_all(paths.objects_tmp()).unwrap();
    let listing = recursive_listing(scratch.path());
    match open_read_only(&paths, &test_seams(real_fs())).unwrap_err() {
        StoreError::NotFound { what } => assert!(what.contains("state.sqlite"), "{what}"),
        err => panic!("expected NotFound, got {err:?}"),
    }
    assert_eq!(recursive_listing(scratch.path()), listing);
    drop(scratch);
}

/// 8. A foreign database is `Corrupt`: version 0 with a schema, in or
///    out of WAL.
#[test]
fn a_foreign_database_is_corrupt_read_only() {
    for mode in ["WAL", "DELETE"] {
        let (scratch, paths) = scratch_paths("read-only-foreign");
        open(&paths, &test_seams(real_fs()))
            .unwrap()
            .close()
            .unwrap();
        for name in ["state.sqlite", "state.sqlite-wal", "state.sqlite-shm"] {
            let path = paths.data_root().join(name);
            if path.exists() {
                fs::remove_file(path).unwrap();
            }
        }
        let raw = rusqlite::Connection::open(paths.state_db()).unwrap();
        raw.execute_batch(&format!("PRAGMA journal_mode = {mode}; CREATE TABLE t(x);"))
            .unwrap();
        raw.close().unwrap();

        let before = fs::read(paths.state_db()).unwrap();
        match open_read_only(&paths, &test_seams(real_fs())).unwrap_err() {
            StoreError::Corrupt { .. } => {}
            err => panic!("expected Corrupt for journal mode {mode}, got {err:?}"),
        }
        assert_eq!(fs::read(paths.state_db()).unwrap(), before);
        drop(scratch);
    }
}

/// 8. A read-only open while the store is open is `Locked`: the
///    lease is exclusive (spec/03), even for readers.
#[test]
fn a_read_only_open_while_the_store_is_open_is_locked() {
    let (scratch, paths) = scratch_paths("read-only-locked");
    let store = open(&paths, &test_seams(real_fs())).unwrap();
    assert!(matches!(
        open_read_only(&paths, &test_seams(real_fs())),
        Err(StoreError::Locked)
    ));
    store.close().unwrap();
    open_read_only(&paths, &test_seams(real_fs())).unwrap();
    drop(scratch);
}

// The clean-shutdown marker (spec/03 "Integrity", plan M0.7).
//

/// One `store_meta` value read through a raw read-only connection, so
/// a closed store can be inspected without opening it (open would
/// remove the marker).
fn raw_meta_value(paths: &StorePaths, key: &str) -> Option<String> {
    let raw = Connection::open_with_flags(
        paths.state_db(),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY | rusqlite::OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .unwrap();
    let mut stmt = raw
        .prepare("SELECT value FROM store_meta WHERE key = ?1")
        .unwrap();
    stmt.query_map([key], |row| row.get::<_, String>(0))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .into_iter()
        .next()
}

/// `close` records `store_meta.clean_shutdown_at` with the clock's
/// milliseconds; the next open removes it; a `Store` dropped without
/// `close` records nothing (spec/03 "Integrity").
#[test]
fn close_records_the_marker_and_the_next_open_removes_it() {
    let (scratch, paths) = scratch_paths("marker-close");
    let store = open(&paths, &test_seams(real_fs())).unwrap();
    store.close().unwrap();
    // The fixed test clock's value, in decimal (spec/03).
    assert_eq!(
        raw_meta_value(&paths, "clean_shutdown_at"),
        Some(FIXED_NOW_MS.to_string()),
        "close must record the marker with the clock's milliseconds"
    );

    // The next open deletes it (in the same transaction as
    // app_version_last_opened), and closing again re-records it.
    let store = open(&paths, &test_seams(real_fs())).unwrap();
    assert_eq!(
        raw_meta_value(&paths, "clean_shutdown_at"),
        None,
        "open must remove the marker"
    );
    store.close().unwrap();
    assert_eq!(
        raw_meta_value(&paths, "clean_shutdown_at"),
        Some(FIXED_NOW_MS.to_string())
    );
    drop(scratch);
}

/// Dropping a `Store` without `close` is an unclean end: no marker,
/// exactly what a killed process leaves behind.
#[test]
fn a_dropped_store_records_no_marker() {
    let (scratch, paths) = scratch_paths("marker-drop");
    let store = open(&paths, &test_seams(real_fs())).unwrap();
    drop(store);
    assert_eq!(raw_meta_value(&paths, "clean_shutdown_at"), None);
    drop(scratch);
}

/// The startup `quick_check` runs only when the marker is missing
/// (spec/03 "Integrity"): a row violating a CHECK — inserted through a
/// raw connection with `ignore_check_constraints`, a finding none of
/// open's other steps sees — survives an open with the marker present,
/// and fails one without it as `Corrupt`.
#[test]
fn quick_check_runs_only_without_the_marker() {
    let (scratch, paths) = scratch_paths("marker-quick-check");
    let store = open(&paths, &test_seams(real_fs())).unwrap();
    store.close().unwrap();

    // A row no open step otherwise notices: CHECK enforcement was off.
    let raw = Connection::open(paths.state_db()).unwrap();
    raw.execute_batch("PRAGMA ignore_check_constraints = ON")
        .unwrap();
    raw.execute(
        "INSERT INTO accounts (account_id, kind, provider, display_name, state, sort_order, created_at)
         VALUES ('bad', 'not-a-kind', 'generic_imap', 'x', 'active', 0, 0)",
        [],
    )
    .unwrap();
    raw.close().unwrap();

    // Marker present: no quick_check, the open succeeds (and deletes
    // the marker); a clean close records it again.
    let store = open(&paths, &test_seams(real_fs())).unwrap();
    store.close().unwrap();

    // Marker removed by hand, as a killed process would leave it: the
    // next open runs quick_check and the CHECK violation is `Corrupt`.
    let raw = Connection::open(paths.state_db()).unwrap();
    raw.execute("DELETE FROM store_meta WHERE key = 'clean_shutdown_at'", [])
        .unwrap();
    raw.close().unwrap();
    match open(&paths, &test_seams(real_fs())).unwrap_err() {
        StoreError::Corrupt { detail } => {
            assert!(detail.contains("quick_check"), "{detail}");
        }
        err => panic!("expected Corrupt, got {err:?}"),
    }
    drop(scratch);
}

/// An unclean store holding a CHECK violation: closed, then the bad row
/// inserted and the marker removed through a raw connection, as a
/// killed process with a damaged file would leave it.
fn unclean_store_with_a_check_violation(label: &str) -> (ScratchDir, StorePaths) {
    let (scratch, paths) = scratch_paths(label);
    open(&paths, &test_seams(real_fs()))
        .unwrap()
        .close()
        .unwrap();
    let raw = Connection::open(paths.state_db()).unwrap();
    raw.execute_batch(
        "PRAGMA ignore_check_constraints = ON;
         INSERT INTO accounts (account_id, kind, provider, display_name, state, sort_order, created_at)
           VALUES ('bad', 'not-a-kind', 'generic_imap', 'x', 'active', 0, 0);
         DELETE FROM store_meta WHERE key = 'clean_shutdown_at';",
    )
    .unwrap();
    raw.close().unwrap();
    (scratch, paths)
}

/// A test-only migration 2 that would hide the damage by deleting the
/// bad row.
fn migration_2_repairs(tx: &rusqlite::Transaction, _seams: &Seams) -> Result<(), StoreError> {
    tx.execute_batch("DELETE FROM accounts WHERE account_id = 'bad';")
        .map_err(crate::schema::bug)
}

/// The startup check runs before the migrations (spec/03): a migration
/// that would delete the damaged row never runs, and the store keeps
/// its version and the row.
#[test]
fn the_startup_check_runs_before_the_migrations() {
    let (_scratch, paths) = unclean_store_with_a_check_violation("check-before-migrate");
    let migrations = crate::testing::extra_migration(migration_2_repairs);
    match open_with(
        &paths,
        &test_seams(real_fs()),
        WRITER_PRAGMAS,
        READER_PRAGMAS,
        &PINNED_SQLITE,
        &migrations,
    )
    .unwrap_err()
    {
        StoreError::Corrupt { detail } => assert!(detail.contains("quick_check"), "{detail}"),
        err => panic!("expected Corrupt, got {err:?}"),
    }
    let raw =
        Connection::open_with_flags(paths.state_db(), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let version: i64 = raw
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    let bad_rows: i64 = raw
        .query_row(
            "SELECT count(*) FROM accounts WHERE account_id = 'bad'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(
        (version, bad_rows),
        (i64::from(crate::schema::LATEST_SCHEMA_VERSION), 1),
        "a migration ran before the check"
    );
}

/// An unclean store whose damage even a read-only connection's
/// `quick_check` finds: a NULL in a column the schema (rewritten behind
/// SQLite's back) declares NOT NULL.
fn unclean_store_with_null_damage(label: &str) -> (ScratchDir, StorePaths) {
    let (scratch, paths) = unclean_store_with_a_check_violation(label);
    let raw = Connection::open(paths.state_db()).unwrap();
    raw.execute_batch(
        "CREATE TABLE null_damage (value TEXT);
         INSERT INTO null_damage VALUES (NULL);
         PRAGMA writable_schema = ON;
         UPDATE sqlite_schema SET sql = 'CREATE TABLE null_damage (value TEXT NOT NULL) STRICT'
           WHERE name = 'null_damage';
         PRAGMA writable_schema = OFF;",
    )
    .unwrap();
    raw.close().unwrap();
    (scratch, paths)
}

/// The read-only open checks nothing (spec/03): an unclean store with
/// damage a read-only `quick_check` would find still opens read-only,
/// its marker stays absent, and `state.sqlite` keeps its bytes (the
/// WAL side is covered by the crashed-frames read-only tests).
#[test]
fn the_read_only_open_neither_checks_nor_touches_the_marker() {
    let (_scratch, paths) = unclean_store_with_null_damage("check-read-only");
    // The damage is visible to a read-only check, so a check would fail.
    let checked = open_connection(paths.state_db(), Access::ReadOnly, READER_PRAGMAS).unwrap();
    assert!(matches!(
        startup_quick_check(&checked),
        Err(StoreError::Corrupt { .. })
    ));
    drop(checked);

    let before = fs::read(paths.state_db()).unwrap();
    let read_only = open_read_only(&paths, &test_seams(real_fs())).unwrap();
    read_only.close().unwrap();

    assert_eq!(fs::read(paths.state_db()).unwrap(), before);
    assert_eq!(raw_meta_value(&paths, "clean_shutdown_at"), None);
}

/// A failed ancestor flush, then a successful retry: the retry flushes
/// that ancestor's parent again, since an ancestor that exists isn't
/// proven durable (spec/03) — not even once something else (a cache
/// directory, another store) has appeared beside the store's way down.
#[test]
fn a_retry_reflushes_an_ancestor_whose_flush_failed() {
    let scratch = ScratchDir::new("ancestor-retry");
    let paths = StorePaths::new(scratch.path().join("a/b/store")).unwrap();
    let failing = Arc::new(HookedFs {
        failing_sync: Some(scratch.path().to_path_buf()),
        ..HookedFs::new(&paths)
    });
    assert!(matches!(
        open(&paths, &test_seams(failing)),
        Err(StoreError::Unavailable { .. })
    ));
    assert!(scratch.path().join("a").exists());
    fs::create_dir(scratch.path().join("a/cache")).unwrap();

    let retry = Arc::new(HookedFs::new(&paths));
    open(&paths, &test_seams(retry.clone()))
        .unwrap()
        .close()
        .unwrap();
    let synced = retry.syncs.lock().unwrap();
    assert!(
        synced.iter().any(|(path, _)| path == scratch.path()),
        "the retry never flushed the level whose flush had failed: {synced:?}"
    );
}

/// The same through a simulated power cut: after the failed flush, a
/// sibling appearing and a successful retry, a durably written file in
/// the store survives.
#[test]
fn a_retried_store_survives_a_power_cut() {
    use sb_test_support::{FaultFs, FaultKind, FsOp, SimFs};
    use std::io::Write as _;

    let sim = Arc::new(SimFs::new());
    sim.create_dir_all(Path::new("/existing")).unwrap();
    let paths = StorePaths::new("/existing/a/b/store").unwrap();
    let fs = FaultFs::new(sim.clone(), FsOp::SyncDir, 1, FaultKind::Eio);
    assert!(create_missing_ancestors(&fs, &paths).is_err());
    fs.create_dir(Path::new("/existing/a/cache")).unwrap();
    create_missing_ancestors(&fs, &paths).unwrap();
    create_layout(&fs, &paths).unwrap();

    // SQLite bypasses the seam; a synced file in the root stands in for
    // the database.
    let mut file = fs.create(paths.state_db()).unwrap();
    file.write_all(b"acknowledged store").unwrap();
    file.sync_all().unwrap();
    drop(file);
    fs.sync_dir(paths.data_root()).unwrap();
    sim.power_cut();
    assert!(
        sim.metadata(paths.state_db()).unwrap().is_some(),
        "the store vanished with an unflushed ancestor"
    );
}

/// A test-only migration that commits.
fn migration_succeeds(tx: &rusqlite::Transaction, _seams: &Seams) -> Result<(), StoreError> {
    tx.execute_batch("CREATE TABLE upgraded (x INTEGER) STRICT;")
        .map_err(crate::schema::bug)
}

/// A test-only migration that fails.
fn migration_fails(tx: &rusqlite::Transaction, _seams: &Seams) -> Result<(), StoreError> {
    tx.execute_batch("not_sql;").map_err(crate::schema::bug)
}

/// An upgrade cut short leaves no clean-shutdown marker: the last
/// session's marker goes before the first migration commits, so after
/// the first test migration commits and the second fails, the store
/// isn't claimed clean.
#[test]
fn an_interrupted_upgrade_leaves_no_marker() {
    let (_scratch, paths, store) = open_scratch("upgrade-marker");
    store.close().unwrap();
    assert!(raw_meta_value(&paths, "clean_shutdown_at").is_some());

    let mut migrations = crate::testing::extra_migration(migration_succeeds);
    let committed_version = migrations.last().unwrap().version;
    migrations.push(Migration {
        version: committed_version + 1,
        run: migration_fails,
    });
    assert!(
        open_with(
            &paths,
            &test_seams(real_fs()),
            WRITER_PRAGMAS,
            READER_PRAGMAS,
            &PINNED_SQLITE,
            &migrations,
        )
        .is_err()
    );
    let raw =
        Connection::open_with_flags(paths.state_db(), rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
            .unwrap();
    let version: i64 = raw
        .query_row("PRAGMA user_version", [], |row| row.get(0))
        .unwrap();
    drop(raw);
    assert_eq!(
        version,
        i64::from(committed_version),
        "the first test migration should have committed"
    );
    assert_eq!(raw_meta_value(&paths, "clean_shutdown_at"), None);
}
