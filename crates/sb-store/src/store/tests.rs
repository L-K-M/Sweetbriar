//! Acceptance tests for `open` (plan M0.3). They live in-crate so they
//! can read the private connection. The cross-process tests re-execute
//! this test binary; see `child_process`.

use std::env;
use std::fs;
use std::io::{self, BufRead, BufReader, Write as _};
use std::path::{Path, PathBuf};
use std::process::{Child, Command, Stdio};
use std::sync::{Arc, Mutex};

use sb_test_support::{ScratchDir, real_fs};
use sb_types::fs::{Fs, Location};

use super::*;

/// A fresh data root under a unique scratch dir, as `StorePaths`.
fn scratch_paths(label: &str) -> (ScratchDir, StorePaths) {
    let scratch = ScratchDir::new(label);
    let paths = StorePaths::new(scratch.path().join("store")).unwrap();
    (scratch, paths)
}

/// `open` on a fresh scratch store.
fn open_scratch(label: &str) -> (ScratchDir, StorePaths, Store) {
    let (scratch, paths) = scratch_paths(label);
    let store = open(&paths, real_fs()).unwrap();
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

/// After open, every setting reads back its spec/03 value.
#[test]
fn settings_stick_after_open() {
    let (_scratch, _paths, store) = open_scratch("settings");
    let conn = &store.conn;
    let journal: String = conn
        .query_row("PRAGMA journal_mode", [], |r| r.get(0))
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
        let got: i64 = conn
            .query_row(&format!("PRAGMA {pragma}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(got, expected, "PRAGMA {pragma}");
    }
}

/// DQS is off: a double-quoted string that isn't a column errors, a
/// real column name still works (spec/03).
#[test]
fn dqs_rejects_bare_double_quoted_strings() {
    let (_scratch, _paths, store) = open_scratch("dqs");
    let conn = &store.conn;
    let err = conn
        .query_row("SELECT \"x\"", [], |r| r.get::<_, String>(0))
        .unwrap_err();
    assert!(err.to_string().contains("no such column"), "{err}");

    conn.execute_batch("CREATE TABLE t(a); INSERT INTO t VALUES ('v')")
        .unwrap();
    let value: String = conn
        .query_row("SELECT \"a\" FROM t", [], |r| r.get(0))
        .unwrap();
    assert_eq!(value, "v");
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
        "open-store" => ChildHold::Store(open(&paths, real_fs()).expect("child open")),
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
/// exist to be dropped (releasing the lease), never to be read.
#[allow(dead_code)]
enum ChildHold {
    Store(Store),
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
            matches!(open(&spelled, real_fs()), Err(StoreError::Locked)),
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
    assert!(matches!(open(&paths, real_fs()), Err(StoreError::Locked)));
    assert_eq!(
        recursive_listing(&root),
        before,
        "failed open changed files"
    );

    drop(_child);
    open(&paths, real_fs()).unwrap();
}

/// A second process holding only the lock, before any store exists:
/// `Locked`, and the data root is still not created.
#[test]
fn other_process_holding_only_the_lock_yields_locked() {
    let (_scratch, paths) = scratch_paths("cross-proc-lock");
    let root = paths.data_root().to_path_buf();
    let _child = spawn_child("hold-lock", &root);

    assert!(!root.exists());
    assert!(matches!(open(&paths, real_fs()), Err(StoreError::Locked)));
    assert!(!root.exists(), "failed open created the data root");

    drop(_child);
    open(&paths, real_fs()).unwrap();
}

/// Same process, second open: `Locked` until `close`.
#[test]
fn same_process_second_open_is_locked_until_close() {
    let (_scratch, paths, store) = open_scratch("same-proc");
    assert!(matches!(open(&paths, real_fs()), Err(StoreError::Locked)));
    store.close().unwrap();
    assert!(open(&paths, real_fs()).is_ok());
}

// ---------------------------------------------------------------------------
// Refused opens.
//

/// A settings row that cannot read back refuses the open with
/// `Unavailable` naming the pragma, and the lease is released.
#[test]
fn pragma_mismatch_is_unavailable_and_releases_the_lease() {
    let (_scratch, paths) = scratch_paths("pragma-mismatch");
    // journal_mode always reads back "wal" once set; expecting "delete"
    // can never match.
    let mut pragmas = WRITER_PRAGMAS.to_vec();
    pragmas[0].expect = Expect::Text("delete");
    match open_with(&paths, real_fs().as_ref(), &pragmas, &PINNED_SQLITE).unwrap_err() {
        StoreError::Unavailable { reason } => {
            assert!(reason.contains("journal_mode"), "{reason}");
        }
        err => panic!("expected Unavailable, got {err:?}"),
    }
    open(&paths, real_fs()).unwrap();
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
    match open_with(&paths, real_fs().as_ref(), WRITER_PRAGMAS, &wrong).unwrap_err() {
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
    match open_with(&paths, real_fs().as_ref(), WRITER_PRAGMAS, &other_source).unwrap_err() {
        StoreError::Internal { detail } => {
            assert!(detail.contains("another build"), "{detail}");
        }
        err => panic!("expected Internal, got {err:?}"),
    }
    open(&paths, real_fs()).unwrap();
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
        match open(&paths, Arc::new(fs)).unwrap_err() {
            StoreError::Unavailable { reason } => {
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
    match open(&paths, real_fs()).unwrap_err() {
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
        match open(&paths, real_fs()).unwrap_err() {
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

    match open(&paths, real_fs()).unwrap_err() {
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
    match open(&paths, real_fs()).unwrap_err() {
        StoreError::Unavailable { reason } => {
            assert!(reason.contains("stray.txt"), "{reason}");
        }
        err => panic!("expected Unavailable, got {err:?}"),
    }
    assert_eq!(fs::read(root.join("stray.txt")).unwrap(), b"mine");

    let (_scratch, paths) = scratch_paths("layout-file-root");
    fs::write(paths.data_root(), b"a file").unwrap();
    match open(&paths, real_fs()).unwrap_err() {
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
    open(&paths, real_fs()).unwrap();
    for dir in paths.layout_dirs() {
        assert!(fs::metadata(dir).unwrap().is_dir(), "{}", dir.display());
    }
}

/// The data root's parent must exist.
#[test]
fn missing_parent_is_unavailable() {
    let scratch = ScratchDir::new("no-parent");
    let orphan = StorePaths::new(scratch.path().join("no/such/parent/store")).unwrap();
    match open(&orphan, real_fs()).unwrap_err() {
        StoreError::Unavailable { .. } => {}
        err => panic!("expected Unavailable, got {err:?}"),
    }
}

/// Creation flushes each layout directory, then the data root, then
/// its parent — all before `state.sqlite` exists.
#[test]
fn creation_flushes_in_spec_order_before_state_sqlite() {
    let (scratch, paths) = scratch_paths("layout-order");
    let fs = Arc::new(HookedFs::new(&paths));

    open(&paths, fs.clone()).unwrap();

    let mut expected: Vec<PathBuf> = paths
        .layout_dirs()
        .iter()
        .map(|d| d.to_path_buf())
        .collect();
    expected.push(paths.data_root().to_path_buf());
    expected.push(scratch.path().to_path_buf());
    let recorded = fs.syncs.lock().unwrap().clone();
    assert_eq!(
        recorded
            .iter()
            .map(|(path, _)| path.clone())
            .collect::<Vec<_>>(),
        expected,
        "unexpected sync_dir order"
    );
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
        match open(&paths, Arc::new(fs)).unwrap_err() {
            StoreError::Unavailable { reason } => {
                assert!(reason.contains("injected flush failure"), "{reason}");
            }
            err => panic!("expected Unavailable, got {err:?}"),
        }
        assert!(!paths.state_db().exists(), "state.sqlite created");
    }
    open(&paths, real_fs()).unwrap();
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
