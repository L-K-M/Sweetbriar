//! M1.8e3b6a binding-input proof: retained identity leases for the
//! unqualified restore copy. Covers the four assigned groups:
//! (1) `prepare` into a fresh store, exact manifest length/digest and
//!     source/live identity leases against real identified opens, then
//!     source-generation and live-database replacement while the
//!     retained capabilities must keep reading the original bytes and
//!     identities and `recheck` must refuse the replacements;
//! (2) `stage_into` progress mutations at `LiveBound`, `LiveRead` and
//!     `HandlesRetained`, each failing the handoff before a usable
//!     `PreparedRestoreStage` exists (with RAII restoration guards so a
//!     failed assertion never leaves the live SQLite writer pointed at a
//!     replacement when the store destructs);
//! (3) the default `open_regular_identified` `Unsupported` failure
//!     propagating through the real `Fs` delegation seam instead of a
//!     legacy `open_regular` fallback;
//! (4) fired-open failures at the three capture operations and
//!     read-only lease cleanup on temporary rechecks, cancellation,
//!     error and `PreparedRestoreStage` drop.
//!
//! Everything here is test-only. The production binding fields are
//! exercised through the same private surface the parent fixtures see.
//! Capture failures use the real FaultFs adapter and preserve its native I/O kind.

use std::cell::RefCell;
use std::io::{self, Read, Seek};
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use sb_types::fs::{
    ConfinedDir, DirectoryIdentity, FileIdentity, Fs, Lease, Location, LockError, Metadata,
    ReadFile, WriteFile,
};

use super::super::super::stage_bindings::LiveBinding;
use super::super::super::staging::StagePhase;
use super::*;

// ---------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------

/// The canonical live root: `open_confined_dir` never follows a symlinked
/// ancestor (macOS `/var`), so every live-target path goes through the
/// canonical spelling the native helpers and `Backups` actually use.
fn canonical_paths(paths: &StorePaths) -> StorePaths {
    StorePaths::new(std::fs::canonicalize(paths.data_root()).expect("canonical live root"))
        .expect("canonical store paths")
}

/// Relative path of the live state database below the live root.
fn live_db_relative(paths: &StorePaths) -> PathBuf {
    paths
        .state_db()
        .strip_prefix(paths.data_root())
        .expect("state database below the live root")
        .to_path_buf()
}

/// Read a retained reader to the end after rewinding it.
fn read_all(reader: &mut dyn ReadFile) -> Vec<u8> {
    reader.rewind().expect("rewind retained reader");
    let mut bytes = Vec::new();
    reader
        .read_to_end(&mut bytes)
        .expect("read retained reader");
    bytes
}

/// Real identified identities of the live root and its state database.
fn real_live_identities(paths: &StorePaths) -> (DirectoryIdentity, FileIdentity) {
    let root = real_fs()
        .open_confined_dir(paths.data_root())
        .expect("open live root");
    let root_identity = root.identity().expect("live root identity");
    let (_, _, database_identity) = root
        .open_regular_identified(live_db_relative(paths))
        .expect("identified live database");
    (root_identity, database_identity)
}

/// Real identified identity of the selected source generation.
fn real_source_identity(location: &Path) -> DirectoryIdentity {
    let source = real_fs()
        .open_confined_dir(location)
        .expect("open source generation");
    source.identity().expect("source identity")
}

/// Whether the `.restoring-<incarnation>` stage directory exists. The
/// production `sibling_name` prefixes the data-root final component.
fn stage_dir_exists(paths: &StorePaths, incarnation: &str) -> bool {
    let mut name = paths
        .data_root()
        .file_name()
        .expect("live root final name")
        .to_os_string();
    name.push(".restoring-");
    name.push(incarnation);
    paths
        .data_root()
        .parent()
        .expect("live root parent")
        .join(name)
        .exists()
}

/// RAII rename/replacement guard. Restores the original name on drop so a
/// failed assertion never leaves a live SQLite writer pointed at a
/// replacement when the store destructs.
struct RestorePath {
    original: PathBuf,
    backup: PathBuf,
    is_dir: bool,
    active: bool,
}

impl RestorePath {
    fn rename(original: &Path, backup: &Path, is_dir: bool) -> io::Result<Self> {
        std::fs::rename(original, backup)?;
        Ok(Self {
            original: original.to_path_buf(),
            backup: backup.to_path_buf(),
            is_dir,
            active: true,
        })
    }

    /// Rename `original` aside and put a same-length replacement file at
    /// the original name (a fresh inode, so identity leases matter).
    fn replace_file(original: &Path, backup: &Path, replacement: &[u8]) -> io::Result<Self> {
        let guard = Self::rename(original, backup, false)?;
        std::fs::write(original, replacement)?;
        Ok(guard)
    }

    /// Rename the original and create its replacement under a held guard.
    fn replace_dir(original: &Path, backup: &Path) -> io::Result<Self> {
        let guard = Self::rename(original, backup, true)?;
        std::fs::create_dir(original)?;
        Ok(guard)
    }

    /// Restore the original name immediately (also run by `Drop`).
    fn restore(&mut self) {
        if !self.active {
            return;
        }
        match std::fs::symlink_metadata(&self.original) {
            Ok(metadata) => {
                if self.is_dir && !metadata.file_type().is_symlink() {
                    std::fs::remove_dir_all(&self.original).expect("remove replacement directory");
                } else {
                    std::fs::remove_file(&self.original)
                        .expect("remove replacement file or symlink");
                }
            }
            Err(error) if error.kind() == io::ErrorKind::NotFound => {}
            Err(error) => panic!("inspect replacement before restoring fixture: {error}"),
        }
        // Halt on a failed restoration before the live writer can destruct
        // through a replacement pathname, including during test unwinding.
        std::fs::rename(&self.backup, &self.original).expect("restore original fixture name");
        self.active = false;
    }
}

impl Drop for RestorePath {
    fn drop(&mut self) {
        self.restore();
    }
}

/// A unique, canonical, non-live incarnation string per case. Distinct
/// constants avoid any `.restoring-` collision between tests.
const INCARNATION_LIVE_BOUND: &str = "11111111-1111-4111-8111-111111111111";
const INCARNATION_LIVE_READ: &str = "22222222-2222-4222-8222-222222222222";
const INCARNATION_SOURCE_HANDOFF: &str = "33333333-3333-4333-8333-333333333333";
const INCARNATION_LIVE_HANDOFF: &str = "44444444-4444-4444-8444-444444444444";
const INCARNATION_LEASE_CANCEL: &str = "55555555-5555-4555-8555-555555555555";
const INCARNATION_LEASE_OK: &str = "66666666-6666-4666-8666-666666666666";

/// A target-live-root-only `Fs` wrapper. Non-target directories are raw
/// delegated so source verification and staging-parent work use the real
/// filesystem; only the live root is wrapped.
#[derive(Clone)]
enum TargetMode {
    /// Returns a confined directory that forwards `identity` and
    /// `open_regular` (counting the latter) but inherits the default
    /// `open_regular_identified`, which fails `Unsupported` without any
    /// pathname fallback.
    Legacy(Arc<AtomicUsize>),
    /// Wraps every new identified read in a `Read + Seek` handle that
    /// increments an active counter on creation and decrements it on
    /// drop, proving read-only lease cleanup.
    Tracking(Arc<AtomicUsize>),
}

struct TargetFs {
    inner: Arc<dyn Fs>,
    target: PathBuf,
    mode: TargetMode,
}

impl TargetFs {
    fn new(inner: Arc<dyn Fs>, target: PathBuf, mode: TargetMode) -> Self {
        Self {
            inner,
            target,
            mode,
        }
    }
}

impl Fs for TargetFs {
    fn open_confined_dir(&self, path: &Path) -> io::Result<Box<dyn ConfinedDir>> {
        if path != self.target {
            return self.inner.open_confined_dir(path);
        }
        match &self.mode {
            TargetMode::Legacy(counter) => {
                let inner = self.inner.open_confined_dir(path)?;
                Ok(Box::new(LegacyConfinedDir {
                    inner,
                    legacy_open: Arc::clone(counter),
                }))
            }
            TargetMode::Tracking(active) => {
                let inner = self.inner.open_confined_dir(path)?;
                Ok(Box::new(TrackingConfinedDir {
                    inner,
                    active: Arc::clone(active),
                }))
            }
        }
    }

    fn create(&self, path: &Path) -> io::Result<Box<dyn WriteFile>> {
        self.inner.create(path)
    }
    fn open(&self, path: &Path) -> io::Result<Box<dyn ReadFile>> {
        self.inner.open(path)
    }
    fn open_no_follow(&self, path: &Path) -> io::Result<Box<dyn ReadFile>> {
        self.inner.open_no_follow(path)
    }
    fn open_regular(&self, path: &Path) -> io::Result<(Box<dyn ReadFile>, Metadata)> {
        self.inner.open_regular(path)
    }
    fn metadata(&self, path: &Path) -> io::Result<Option<Metadata>> {
        self.inner.metadata(path)
    }
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        self.inner.read_dir(path)
    }
    fn create_dir(&self, path: &Path) -> io::Result<()> {
        self.inner.create_dir(path)
    }
    fn create_private_dir(&self, path: &Path) -> io::Result<()> {
        self.inner.create_private_dir(path)
    }
    fn restrict_dir(&self, path: &Path) -> io::Result<()> {
        self.inner.restrict_dir(path)
    }
    fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        self.inner.remove_dir_all(path)
    }
    fn quarantine(&self, path: &Path) -> io::Result<()> {
        self.inner.quarantine(path)
    }
    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        self.inner.sync_dir(path)
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
    fn lock_exclusive(&self, path: &Path) -> Result<Box<dyn Lease>, LockError> {
        self.inner.lock_exclusive(path)
    }
    fn location(&self, path: &Path) -> io::Result<Location> {
        self.inner.location(path)
    }
}

/// Forwards `identity` and `open_regular` to the real directory while
/// inheriting the default `Unsupported` `open_regular_identified`.
struct LegacyConfinedDir {
    inner: Box<dyn ConfinedDir>,
    legacy_open: Arc<AtomicUsize>,
}

impl ConfinedDir for LegacyConfinedDir {
    fn identity(&self) -> io::Result<DirectoryIdentity> {
        self.inner.identity()
    }
    fn open_regular(&self, relative: &Path) -> io::Result<(Box<dyn ReadFile>, Metadata)> {
        self.legacy_open.fetch_add(1, Ordering::SeqCst);
        self.inner.open_regular(relative)
    }
}

/// Wraps each new identified read so its lifetime is observable.
struct TrackingConfinedDir {
    inner: Box<dyn ConfinedDir>,
    active: Arc<AtomicUsize>,
}

struct TrackingReader {
    inner: Box<dyn ReadFile>,
    active: Arc<AtomicUsize>,
}

impl Read for TrackingReader {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.inner.read(buf)
    }
}

impl Seek for TrackingReader {
    fn seek(&mut self, position: io::SeekFrom) -> io::Result<u64> {
        self.inner.seek(position)
    }
}

impl Drop for TrackingReader {
    fn drop(&mut self) {
        self.active.fetch_sub(1, Ordering::SeqCst);
    }
}

impl ConfinedDir for TrackingConfinedDir {
    fn identity(&self) -> io::Result<DirectoryIdentity> {
        self.inner.identity()
    }
    fn open_regular(&self, relative: &Path) -> io::Result<(Box<dyn ReadFile>, Metadata)> {
        self.inner.open_regular(relative)
    }
    fn open_regular_identified(
        &self,
        relative: &Path,
    ) -> io::Result<(Box<dyn ReadFile>, Metadata, FileIdentity)> {
        let (file, metadata, identity) = self.inner.open_regular_identified(relative)?;
        self.active.fetch_add(1, Ordering::SeqCst);
        Ok((
            Box::new(TrackingReader {
                inner: file,
                active: Arc::clone(&self.active),
            }),
            metadata,
            identity,
        ))
    }
}

// ---------------------------------------------------------------------------
// Group 1: prepare into a fresh store and retained identity leases
// ---------------------------------------------------------------------------

#[test]
fn prepare_into_fresh_store_retains_source_and_live_identities() {
    let source = staging_source("m18-bindings-g1-source");
    let (_scratch, fresh_paths, fresh_store) = open("m18-bindings-g1-target");
    let live = canonical_paths(&fresh_paths);
    let selection = source.select_with(&fresh_store);
    let mut stage = prepare(
        &fresh_store,
        selection,
        source.receipt.location(),
        source.metadata,
    );

    // Exact manifest length and digest: source original and staged copy.
    let source_manifest = std::fs::read(source.receipt.location().join("manifest.json")).unwrap();
    let staged_manifest = std::fs::read(stage.path().join("manifest.json")).unwrap();
    assert_eq!(stage.manifest_bytes, source_manifest.len() as u64);
    assert_eq!(stage.manifest_bytes, staged_manifest.len() as u64);
    assert_eq!(stage.manifest_sha256(), sha256_hex(&source_manifest));
    assert_eq!(stage.manifest_sha256(), sha256_hex(&staged_manifest));

    // Identities match real identified opens of the same known files.
    let (live_root_id, live_db_id) = real_live_identities(&live);
    let source_id = real_source_identity(source.receipt.location());
    assert_eq!(stage.bindings.source_identity, source_id);
    assert_eq!(stage.bindings.live.root_identity, live_root_id);
    assert_eq!(stage.bindings.live.database_identity, live_db_id);

    // Retained source after a generation rename plus a same-name fake
    // root: the capability still opens the original manifest bytes and
    // keeps the original directory identity, while `recheck` refuses.
    let source_backup = source
        .receipt
        .location()
        .with_extension("m18-g1-source-backup");
    let mut source_guard =
        RestorePath::replace_dir(source.receipt.location(), &source_backup).unwrap();
    std::fs::write(
        source.receipt.location().join("manifest.json"),
        b"fake source manifest",
    )
    .unwrap();
    let (mut retained_manifest, _) = stage
        .bindings
        .source
        .open_regular(Path::new("manifest.json"))
        .expect("retained source manifest");
    assert_eq!(read_all(&mut retained_manifest), source_manifest);
    assert_eq!(
        stage.bindings.source.identity().unwrap(),
        stage.bindings.source_identity
    );
    assert!(
        stage
            .bindings
            .recheck(
                real_fs().as_ref(),
                source.receipt.location(),
                live.data_root()
            )
            .is_err()
    );
    source_guard.restore();
    drop(source_guard);
    assert_eq!(
        real_source_identity(source.receipt.location()),
        stage.bindings.source_identity
    );

    // Retained live database after a same-length replacement file.
    let live_db = live.state_db();
    let original_db = std::fs::read(live_db).unwrap();
    let replacement: Vec<u8> = original_db.iter().map(|byte| !byte).collect();
    let db_backup = live_db.with_extension("m18-g1-db-backup");
    let mut db_guard = RestorePath::replace_file(live_db, &db_backup, &replacement).unwrap();
    assert_eq!(read_all(stage.bindings.live.database.as_mut()), original_db);
    assert_eq!(stage.bindings.live.database_identity, live_db_id);
    let relative = live_db_relative(&live);
    let (mut old_file, _, old_id) = stage
        .bindings
        .live
        .root
        .open_regular_identified(&relative)
        .expect("retained directory opens its current child name");
    assert_ne!(old_id, live_db_id);
    assert_eq!(read_all(&mut old_file), replacement);
    let fresh_root = real_fs().open_confined_dir(live.data_root()).unwrap();
    let (_, _, fresh_id) = fresh_root
        .open_regular_identified(&relative)
        .expect("fresh root identified database");
    assert_ne!(fresh_id, live_db_id);
    assert!(
        stage
            .bindings
            .live
            .recheck(real_fs().as_ref(), live.data_root())
            .is_err()
    );
    db_guard.restore();
    drop(db_guard);

    // Retained live root after a whole-root rename and recreate.
    let root_backup = live.data_root().with_extension("m18-g1-root-backup");
    let mut root_guard = RestorePath::replace_dir(live.data_root(), &root_backup).unwrap();
    std::fs::write(live.state_db(), &replacement).unwrap();
    let (mut old_root_file, _, old_root_db_id) = stage
        .bindings
        .live
        .root
        .open_regular_identified(&relative)
        .expect("old root capability after whole-root rename");
    assert_eq!(old_root_db_id, live_db_id);
    assert_eq!(read_all(&mut old_root_file), original_db);
    assert_eq!(
        stage.bindings.live.root.identity().unwrap(),
        stage.bindings.live.root_identity
    );
    let new_root = real_fs().open_confined_dir(live.data_root()).unwrap();
    let new_root_id = new_root.identity().unwrap();
    assert_ne!(new_root_id, stage.bindings.live.root_identity);
    assert!(
        stage
            .bindings
            .live
            .recheck(real_fs().as_ref(), live.data_root())
            .is_err()
    );
    root_guard.restore();
    drop(root_guard);

    // All names restored; the source generation and live store are intact.
    assert_eq!(real_source_identity(source.receipt.location()), source_id);
    assert_eq!(real_live_identities(&live).0, live_root_id);
    source.assert_untouched();
}

// ---------------------------------------------------------------------------
// Group 2: progress mutations fail the handoff
// ---------------------------------------------------------------------------

/// Build a `Backups` over the canonical live root and the supplied `Fs`.
fn bindings_backups(store: &Store, paths: &StorePaths, fs: Arc<dyn Fs>, seed: u64) -> Backups {
    faulty_backups_seeded(store, paths, fs, seed)
}

#[test]
fn live_bound_database_replacement_fails_before_stage_creation() {
    let source = staging_source("m18-bindings-g2a-source");
    let (_scratch, fresh_paths, fresh_store) = open("m18-bindings-g2a-target");
    let live = canonical_paths(&fresh_paths);
    let selection = source.select_with(&fresh_store);
    let backups = bindings_backups(&fresh_store, &live, real_fs(), 101);

    let live_db = live.state_db();
    let original_db = std::fs::read(live_db).unwrap();
    let replacement: Vec<u8> = original_db.iter().map(|byte| !byte).collect();
    let db_backup = live_db.with_extension("m18-g2a-db-backup");
    let slot: Rc<RefCell<Option<RestorePath>>> = Rc::new(RefCell::new(None));
    let hook_slot = Rc::clone(&slot);

    let result = backups.stage_into(
        &selection,
        source.receipt.location(),
        source.metadata,
        INCARNATION_LIVE_BOUND,
        &AtomicBool::new(false),
        |phase| {
            if phase == StagePhase::LiveBound {
                *hook_slot.borrow_mut() = Some(
                    RestorePath::replace_file(live_db, &db_backup, &replacement)
                        .expect("replace live database"),
                );
            }
        },
    );

    assert!(result.is_err(), "LiveBound mutation must fail the handoff");
    assert!(
        !stage_dir_exists(&live, INCARNATION_LIVE_BOUND),
        "no stage directory may be created"
    );
    assert_no_stage_dir(&live);
    if let Some(mut guard) = slot.borrow_mut().take() {
        guard.restore();
    }
    source.assert_untouched();
}

#[test]
fn live_read_root_replacement_fails_before_stage_creation() {
    let source = staging_source("m18-bindings-g2b-source");
    let (_scratch, fresh_paths, fresh_store) = open("m18-bindings-g2b-target");
    let live = canonical_paths(&fresh_paths);
    let selection = source.select_with(&fresh_store);
    let backups = bindings_backups(&fresh_store, &live, real_fs(), 102);

    let root_backup = live.data_root().with_extension("m18-g2b-root-backup");
    let slot: Rc<RefCell<Option<RestorePath>>> = Rc::new(RefCell::new(None));
    let hook_slot = Rc::clone(&slot);

    let result = backups.stage_into(
        &selection,
        source.receipt.location(),
        source.metadata,
        INCARNATION_LIVE_READ,
        &AtomicBool::new(false),
        |phase| {
            if phase == StagePhase::LiveRead {
                *hook_slot.borrow_mut() = Some(
                    RestorePath::replace_dir(live.data_root(), &root_backup)
                        .expect("replace live root"),
                );
            }
        },
    );

    assert!(result.is_err(), "LiveRead mutation must fail the handoff");
    assert!(
        !stage_dir_exists(&live, INCARNATION_LIVE_READ),
        "no stage directory may be created"
    );
    assert_no_stage_dir(&live);
    if let Some(mut guard) = slot.borrow_mut().take() {
        guard.restore();
    }
    source.assert_untouched();
}

#[test]
fn handles_retained_source_replacement_fails_final_handoff() {
    let source = staging_source("m18-bindings-g2c-source");
    let (_scratch, fresh_paths, fresh_store) = open("m18-bindings-g2c-target");
    let live = canonical_paths(&fresh_paths);
    let selection = source.select_with(&fresh_store);
    let backups = bindings_backups(&fresh_store, &live, real_fs(), 103);

    let source_backup = source
        .receipt
        .location()
        .with_extension("m18-g2c-source-backup");
    let slot: Rc<RefCell<Option<RestorePath>>> = Rc::new(RefCell::new(None));
    let hook_slot = Rc::clone(&slot);

    let result = backups.stage_into(
        &selection,
        source.receipt.location(),
        source.metadata,
        INCARNATION_SOURCE_HANDOFF,
        &AtomicBool::new(false),
        |phase| {
            if phase == StagePhase::HandlesRetained {
                *hook_slot.borrow_mut() = Some(
                    RestorePath::replace_dir(source.receipt.location(), &source_backup)
                        .expect("replace source generation"),
                );
            }
        },
    );

    assert!(result.is_err(), "HandlesRetained source mutation must fail");
    assert!(
        stage_dir_exists(&live, INCARNATION_SOURCE_HANDOFF),
        "unqualified stage evidence is expected at HandlesRetained"
    );
    if let Some(mut guard) = slot.borrow_mut().take() {
        guard.restore();
    }
    assert_eq!(
        real_source_identity(source.receipt.location()),
        selection.identity
    );
    source.assert_untouched();
}

#[test]
fn handles_retained_live_replacement_fails_final_handoff() {
    let source = staging_source("m18-bindings-g2d-source");
    let (_scratch, fresh_paths, fresh_store) = open("m18-bindings-g2d-target");
    let live = canonical_paths(&fresh_paths);
    let selection = source.select_with(&fresh_store);
    let backups = bindings_backups(&fresh_store, &live, real_fs(), 104);

    let root_backup = live.data_root().with_extension("m18-g2d-root-backup");
    let slot: Rc<RefCell<Option<RestorePath>>> = Rc::new(RefCell::new(None));
    let hook_slot = Rc::clone(&slot);

    let result = backups.stage_into(
        &selection,
        source.receipt.location(),
        source.metadata,
        INCARNATION_LIVE_HANDOFF,
        &AtomicBool::new(false),
        |phase| {
            if phase == StagePhase::HandlesRetained {
                *hook_slot.borrow_mut() = Some(
                    RestorePath::replace_dir(live.data_root(), &root_backup)
                        .expect("replace live root"),
                );
            }
        },
    );

    assert!(result.is_err(), "HandlesRetained live mutation must fail");
    assert!(
        stage_dir_exists(&live, INCARNATION_LIVE_HANDOFF),
        "unqualified stage evidence is expected at HandlesRetained"
    );
    if let Some(mut guard) = slot.borrow_mut().take() {
        guard.restore();
    }
    source.assert_untouched();
}

// ---------------------------------------------------------------------------
// Group 3: default Unsupported identified open propagates, no fallback
// ---------------------------------------------------------------------------

#[test]
fn default_unsupported_identified_open_propagates_without_legacy_fallback() {
    let source = staging_source("m18-bindings-g3-source");
    let (_scratch, fresh_paths, fresh_store) = open("m18-bindings-g3-target");
    let live = canonical_paths(&fresh_paths);
    // Selection happens through the fresh store's real filesystem before
    // the target-live-only wrapper is constructed.
    let selection = source.select_with(&fresh_store);

    let legacy_open = Arc::new(AtomicUsize::new(0));
    let wrapper = Arc::new(TargetFs::new(
        real_fs(),
        live.data_root().to_path_buf(),
        TargetMode::Legacy(Arc::clone(&legacy_open)),
    ));
    let backups = bindings_backups(&fresh_store, &live, wrapper, 105);

    let result = backups.stage_into(
        &selection,
        source.receipt.location(),
        source.metadata,
        INCARNATION_LEASE_OK,
        &AtomicBool::new(false),
        |_| {},
    );

    let error = result.expect_err("identified open must fail closed");
    assert_eq!(
        error.io_kind(),
        Some(io::ErrorKind::Unsupported),
        "the default Unsupported failure must propagate"
    );
    assert_eq!(
        legacy_open.load(Ordering::SeqCst),
        0,
        "legacy open_regular must never be used as a fallback"
    );
    assert!(
        !stage_dir_exists(&live, INCARNATION_LEASE_OK),
        "no stage directory may be created"
    );
    assert_no_stage_dir(&live);
    source.assert_untouched();
}

// ---------------------------------------------------------------------------
// Group 4: fired-open failures and read-only lease cleanup
// ---------------------------------------------------------------------------

#[test]
fn fired_open_failures_pin_each_capture_operation() {
    let (_scratch, paths, _store) = open("m18-bindings-g4-open");
    let live = canonical_paths(&paths);
    let probe = FaultFs::new(real_fs(), FsOp::Open, 1, FaultKind::Eio);
    let expected = probe
        .open_confined_dir(live.data_root())
        .err()
        .unwrap()
        .kind();
    for nth in 1..=3 {
        let faults = FaultFs::new(real_fs(), FsOp::Open, nth, FaultKind::Eio);
        let error =
            LiveBinding::capture(&faults, live.data_root()).expect_err("fired capture open");
        assert_eq!(error.io_kind(), Some(expected));
        assert_eq!(
            faults.calls(FsOp::Open),
            nth,
            "all capture operations are fault wrapped"
        );
    }
}

#[test]
fn tracking_leases_cover_rechecks_cancellation_and_prepared_drop() {
    let source = staging_source("m18-bindings-g4-lease-source");
    let (_scratch, fresh_paths, fresh_store) = open("m18-bindings-g4-lease-target");
    let live = canonical_paths(&fresh_paths);
    let selection = source.select_with(&fresh_store);
    let active = Arc::new(AtomicUsize::new(0));
    let wrapper = Arc::new(TargetFs::new(
        real_fs(),
        live.data_root().to_path_buf(),
        TargetMode::Tracking(Arc::clone(&active)),
    ));
    let backups = bindings_backups(&fresh_store, &live, wrapper, 106);

    // Cancellation at LiveBound: the captured lease is dropped on error.
    let cancel = Arc::new(AtomicBool::new(false));
    let hook_cancel = Arc::clone(&cancel);
    let cancelled = backups.stage_into(
        &selection,
        source.receipt.location(),
        source.metadata,
        INCARNATION_LEASE_CANCEL,
        &cancel,
        move |phase| {
            if phase == StagePhase::LiveBound {
                hook_cancel.store(true, Ordering::SeqCst);
            }
        },
    );
    assert!(cancelled.is_err(), "cancellation must fail the handoff");
    assert_eq!(
        active.load(Ordering::SeqCst),
        0,
        "cancellation must drop the captured read-only lease"
    );

    // Successful prepare: the PreparedRestoreStage holds one retained
    // identified read while temporary rechecks release their own handles.
    let prepared = backups
        .stage_into(
            &selection,
            source.receipt.location(),
            source.metadata,
            INCARNATION_LEASE_OK,
            &AtomicBool::new(false),
            |_| {},
        )
        .expect("successful stage");
    assert_eq!(
        active.load(Ordering::SeqCst),
        1,
        "one retained identified read while PreparedRestoreStage is held"
    );
    drop(prepared);
    assert_eq!(
        active.load(Ordering::SeqCst),
        0,
        "PreparedRestoreStage drop must release the retained lease"
    );
    // A late namespace refusal also drops the retained lease, not merely
    // cancellation or a successful capability's destructor.
    let original = std::fs::read(live.state_db()).unwrap();
    let replacement: Vec<u8> = original.iter().map(|byte| !byte).collect();
    let moved = live.state_db().with_extension("tracked-original");
    let mut guard = None;
    let failed = backups.stage_into(
        &selection,
        source.receipt.location(),
        source.metadata,
        "88888888-8888-4888-8888-888888888888",
        &AtomicBool::new(false),
        |phase| {
            if phase == StagePhase::HandlesRetained {
                assert_eq!(active.load(Ordering::SeqCst), 1);
                guard =
                    Some(RestorePath::replace_file(live.state_db(), &moved, &replacement).unwrap());
            }
        },
    );
    assert!(failed.is_err());
    assert_eq!(
        active.load(Ordering::SeqCst),
        0,
        "final refusal drops the live lease"
    );
    guard.as_mut().unwrap().restore();
    source.assert_untouched();
}

#[test]
fn a_symlink_to_the_original_source_still_refuses_final_handoff() {
    use std::os::unix::fs::symlink;
    let source = staging_source("m18-bindings-source-symlink");
    let (_scratch, paths, fresh) = open("m18-bindings-symlink-target");
    let selection = source.select_with(&fresh);
    let moved = source.receipt.location().with_extension("retained-source");
    let mut guard = None;
    let result = fresh.backups.stage_into(
        &selection,
        source.receipt.location(),
        source.metadata,
        "77777777-7777-4777-8777-777777777777",
        &AtomicBool::new(false),
        |phase| {
            if phase == StagePhase::HandlesRetained {
                guard = Some(RestorePath::rename(source.receipt.location(), &moved, true).unwrap());
                symlink(&moved, source.receipt.location()).unwrap();
            }
        },
    );
    assert!(
        result.is_err(),
        "same-identity symlink must not be followed"
    );
    guard.as_mut().unwrap().restore();
    assert!(
        !std::fs::symlink_metadata(source.receipt.location())
            .unwrap()
            .file_type()
            .is_symlink()
    );
    assert!(paths.state_db().is_file());
    source.assert_untouched();
}
