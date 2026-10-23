//! `open`: opens or creates a store exactly as spec/03 requires — the
//! location check, the ownership lease, the on-disk layout, a
//! read-only look at an existing database, the one connection
//! constructor with set-and-read-back settings, the pinned-version
//! check, then the migrations and the
//! `app_version_last_opened` record (spec/02) — and refuses anything
//! else.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use rusqlite::config::DbConfig;
use rusqlite::ffi::ErrorCode;
use rusqlite::types::Value;
use rusqlite::{Connection, OpenFlags};
use sb_types::fs::{FileKind, Fs, Lease, Location, LockError};
use sb_types::random::Random;
use sb_types::time::Clock;

use crate::StoreError;
use crate::StorePaths;
use crate::import_reports::ImportReports;
use crate::objects::FileObjectStore;
use crate::readers::Readers;
use crate::schema::{
    MIGRATIONS, Migration, check_existing, check_existing_read_only, read_user_version, require_ok,
    run_migrations,
};
use crate::source_snapshots::ImportSourceSnapshots;
use crate::writer::{ListObserver, SearchObserver, WriteFuture, WritePriority, Writer};

/// The SQLite build spec/01's crate table pins (rusqlite 0.40.2 →
/// libsqlite3-sys 0.38.2 → SQLite 3.53.2); `open` refuses any other.
pub(crate) const PINNED_SQLITE: PinnedSqlite = PinnedSqlite {
    version: "3.53.2",
    version_number: 3_053_002,
    source_id: "2026-06-03 19:12:13 d6e03d8c777cfa2d35e3b60d8ec3e0187f3e9f99d8e2ee9cac695fd6fcdf1a24",
};

/// Acceptable-release bounds (spec/03), as `version_number()`
/// integers: the floor CI enforces, and the withdrawn 3.52.0.
const SQLITE_VERSION_FLOOR: i32 = 3_051_003; // 3.51.3
const SQLITE_VERSION_WITHDRAWN: i32 = 3_052_000; // 3.52.0

/// The SQLite file header's length. SQLite takes a shorter non-empty
/// file for an empty database and overwrites it (its Unix VFS reports
/// a 1-byte file as empty), so `open` refuses one first.
const SQLITE_HEADER_LEN: u64 = 100;

/// The SQLite build this release pins; `open` verifies the linked
/// library against it (`version_number()`, `sqlite_version()` and
/// `sqlite_source_id()`).
pub(crate) struct PinnedSqlite {
    version: &'static str,
    version_number: i32,
    source_id: &'static str,
}

/// The `store_meta` key of the clean-shutdown marker (spec/03
/// "Integrity"): present only between a clean shutdown and the next
/// open.
pub(crate) const CLEAN_SHUTDOWN_KEY: &str = "clean_shutdown_at";

/// An open store: the reader pool, the writer, the object store and
/// the ownership lease (spec/01 "Store threads", spec/03).
///
/// Field order is load-bearing: fields drop in declaration order, so
/// the readers close first, the writer is the last connection to
/// close — SQLite then checkpoints and removes the WAL (M0.4 lifted
/// `NO_CKPT_ON_CLOSE` for the writer) — and the lease releases last,
/// after SQLite is done with the files.
pub struct Store {
    readers: Readers,
    writer: Writer,
    pub(crate) activation: crate::activation::inputs::Owner,
    objects: FileObjectStore,
    source_snapshots: ImportSourceSnapshots,
    import_reports: ImportReports,
    lease: Box<dyn Lease>,
    /// The clock [`Store::close`] stamps the clean-shutdown marker
    /// with, held from the seams the store opened with.
    clock: Arc<dyn Clock>,
}

impl Store {
    /// The store's object store (spec/03): publishing objects through
    /// it lands them under this store's `objects/`.
    pub fn objects(&self) -> &FileObjectStore {
        &self.objects
    }

    /// Capture stable import input before inventory or a referencing
    /// transaction. Blocking file I/O; progress may cancel the copy.
    /// Async callers must use a blocking thread for the full source copy.
    pub fn capture_import_source(
        &self,
        source: &Path,
        progress: impl FnMut(u64) -> std::ops::ControlFlow<()>,
    ) -> Result<crate::ImportSourceCapture, StoreError> {
        self.source_snapshots.capture(source, progress)
    }

    /// Verify the whole private copy before allowing offset-based reads.
    /// This never reopens the original source path.
    /// Async callers must use a blocking thread for the full hash check.
    pub fn open_import_source(
        &self,
        snapshot: &crate::ImportSourceSnapshot,
    ) -> Result<Box<dyn sb_types::fs::ReadFile>, StoreError> {
        self.source_snapshots.open(snapshot)
    }

    /// Terminal-job cleanup (spec/03): remove these private copies and
    /// flush `recovery/`. Missing copies count as removed. Blocking.
    pub fn remove_import_sources(
        &self,
        snapshots: &[crate::ImportSourceSnapshot],
    ) -> Result<(), StoreError> {
        self.source_snapshots.remove(snapshots)
    }

    /// Durably replace `job`'s report in `recovery/` (spec/03, 09).
    /// Blocking.
    pub fn write_import_report(&self, job: &str, report: &[u8]) -> Result<(), StoreError> {
        self.import_reports.write(job, report)
    }

    /// Whether `job`'s report exists: a finished job's cleanup is done.
    pub fn import_report_exists(&self, job: &str) -> Result<bool, StoreError> {
        self.import_reports.exists(job)
    }

    /// `job`'s report as last written, or `None` while there is none;
    /// one over `max_bytes` is `Corrupt`.
    pub fn read_import_report(
        &self,
        job: &str,
        max_bytes: u64,
    ) -> Result<Option<String>, StoreError> {
        self.import_reports.read(job, max_bytes)
    }

    /// Where `job`'s report lives, for the user to open.
    pub fn import_report_path(&self, job: &str) -> Result<PathBuf, StoreError> {
        self.import_reports.path(job)
    }

    /// Submit one write to the writer (spec/03 "Write discipline"):
    /// see [`Writer::submit`]. The returned future resolves when the
    /// writer's transaction commits or rolls back.
    pub fn write<T, F>(&self, priority: WritePriority, closure: F) -> WriteFuture<T>
    where
        T: Send + 'static,
        F: FnOnce(&rusqlite::Transaction) -> Result<T, StoreError> + Send + 'static,
    {
        self.writer.submit(priority, closure)
    }

    // Own activation changes do not schedule another corpus pass.
    pub(crate) fn write_activation<T, F>(&self, closure: F) -> WriteFuture<T>
    where
        T: Send + 'static,
        F: FnOnce(&rusqlite::Transaction) -> Result<T, StoreError> + Send + 'static,
    {
        self.writer.submit_activation(closure)
    }

    /// Hear of each list generation after the transaction that made it
    /// commits (spec/07 "Subscriptions"). The observer runs on the
    /// writer thread and must return at once.
    pub fn observe_list_generation(&self, observer: ListObserver) {
        self.writer.observe_list_generation(observer);
    }

    /// Hear committed Search generations before their write future resolves.
    /// Runs on the writer thread; return immediately and never panic.
    pub fn observe_search_generation(&self, observer: SearchObserver) {
        self.writer.observe_search_generation(observer);
    }

    /// Run one read through the reader pool (spec/03): a short
    /// transaction on a free read-only connection, waiting for one
    /// when all are busy.
    pub fn read<T>(
        &self,
        f: impl FnOnce(&rusqlite::Transaction) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.readers.read(f)
    }

    /// Close the store (spec/03 "Integrity"): record the
    /// clean-shutdown marker as the writer's last write, then close
    /// the readers, join the writer (closing the last connection,
    /// which checkpoints and removes the WAL), then release the lease.
    /// The lock file itself is kept, never deleted (spec/03).
    ///
    /// Dropping a `Store` without `close` records nothing — an unclean
    /// end, which the next open answers with `PRAGMA quick_check`.
    pub fn close(self) -> Result<(), StoreError> {
        let Self {
            readers,
            writer,
            activation,
            objects,
            source_snapshots,
            import_reports,
            lease,
            clock,
        } = self;
        // The marker goes in at the lowest priority, so anything
        // already queued commits first and the marker truly is the
        // last write; callers stop submitting before closing.
        let marker = record_clean_shutdown(&writer, clock.as_ref());
        let readers_closed = readers.close();
        let writer_closed = writer.close();
        drop(activation);
        drop(objects);
        drop(source_snapshots);
        drop(import_reports);
        drop(lease);
        marker.and(readers_closed).and(writer_closed)
    }
}

/// The store is shared as `Arc<Store>` by many tasks (spec/01); prove
/// it carries across threads. Never called — the body only needs to
/// typecheck.
#[allow(dead_code)]
fn assert_store_is_send_and_sync() {
    fn check<T: Send + Sync>() {}
    check::<Store>();
    check::<ReadOnlyStore>();
}

/// The connection and lease aren't printable; the type name is all a
/// test needs.
impl std::fmt::Debug for Store {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Store")
    }
}

/// The seams `open` runs against (spec/01 rule 5): the filesystem,
/// the clock and the randomness, injected by the caller (`sb-resources`
/// builds the real ones from `sb-platform`; tests inject fixed ones).
pub struct Seams {
    fs: Arc<dyn Fs>,
    clock: Arc<dyn Clock>,
    random: Arc<dyn Random>,
}

impl Seams {
    pub fn new(fs: Arc<dyn Fs>, clock: Arc<dyn Clock>, random: Arc<dyn Random>) -> Self {
        Self { fs, clock, random }
    }

    pub(crate) fn fs(&self) -> &dyn Fs {
        self.fs.as_ref()
    }

    /// The filesystem seam by handle, for owners that outlive this
    /// borrow (the object store holds one).
    pub(crate) fn fs_arc(&self) -> &Arc<dyn Fs> {
        &self.fs
    }

    pub(crate) fn clock(&self) -> &dyn Clock {
        self.clock.as_ref()
    }

    pub(crate) fn random(&self) -> &dyn Random {
        self.random.as_ref()
    }

    /// The randomness seam by handle, as `fs_arc`.
    pub(crate) fn random_arc(&self) -> &Arc<dyn Random> {
        &self.random
    }

    /// The clock seam by handle, as `fs_arc`: the store stamps the
    /// clean-shutdown marker with the clock it opened with.
    pub(crate) fn clock_arc(&self) -> &Arc<dyn Clock> {
        &self.clock
    }
}

impl std::fmt::Debug for Seams {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Seams")
    }
}

/// Open or create the store at `paths` (spec/03, then spec/02
/// "Migrations"): location check, lease, layout, the read-only look
/// at an existing database, the one connection constructor, version
/// check, migrations, `app_version_last_opened` — in this order.
pub fn open(paths: &StorePaths, seams: &Seams) -> Result<Store, StoreError> {
    open_with(
        paths,
        seams,
        WRITER_PRAGMAS,
        READER_PRAGMAS,
        &PINNED_SQLITE,
        MIGRATIONS,
    )
}

/// `open` with the settings tables, pinned version and migration list
/// as parameters, so tests can simulate a pragma or version mismatch,
/// or add a failing migration.
#[allow(clippy::too_many_arguments)]
pub(crate) fn open_with(
    paths: &StorePaths,
    seams: &Seams,
    writer_pragmas: &[PragmaSpec],
    reader_pragmas: &[PragmaSpec],
    pinned: &PinnedSqlite,
    migrations: &[Migration],
) -> Result<Store, StoreError> {
    let fs = seams.fs();
    let paths = &resolve_root(fs, paths)?;
    check_location(fs, paths.data_root())?;
    create_missing_ancestors(fs, paths)?;
    let lease = take_lease(fs, paths.lock_file())?;
    // From here on an early return drops `lease`, releasing the lock.
    ensure_layout(fs, paths)?;
    // The layout exists: the object store can publish into it. Built
    // from the seams, so tests and the engine share one construction.
    let objects = FileObjectStore::new(
        Arc::clone(seams.fs_arc()),
        Arc::clone(seams.random_arc()),
        paths.objects().to_path_buf(),
        paths.recovery().to_path_buf(),
    );
    examine_existing(fs, paths, pinned, migrations)?;
    let mut conn = open_connection(paths.state_db(), Access::ReadWrite, writer_pragmas)?;
    check_sqlite_version(&conn, pinned)?;
    startup_quick_check(&conn)?;
    run_migrations(&mut conn, seams, migrations)?;
    // The readers first, from the same constructor (spec/03): if one
    // fails, the writer connection still doesn't checkpoint on close.
    let readers = Readers::open(paths.state_db(), reader_pragmas)?;
    let activation = crate::activation::inputs::Owner::install(&conn)?;
    // The writer thread owns the connection from here (spec/01), and
    // lets it checkpoint once it runs (spec/03).
    let writer = Writer::new(conn)?;
    Ok(Store {
        readers,
        writer,
        activation,
        objects,
        source_snapshots: ImportSourceSnapshots::new(
            Arc::clone(seams.fs_arc()),
            Arc::clone(seams.random_arc()),
            paths.recovery().to_path_buf(),
        ),
        import_reports: ImportReports::new(
            Arc::clone(seams.fs_arc()),
            Arc::clone(seams.random_arc()),
            paths.recovery().to_path_buf(),
        ),
        lease,
        clock: Arc::clone(seams.clock_arc()),
    })
}

/// The startup `quick_check` (spec/03 "Integrity"): an existing store
/// (`user_version` ≥ 1 before migrating) that lacks the clean-shutdown
/// marker didn't end cleanly, so check it before the migrations touch
/// anything; a finding is `Corrupt`. A new store has no previous
/// session and skips the check. Runs on the writer connection, after
/// the SQLite version check and before the migrations.
fn startup_quick_check(conn: &Connection) -> Result<(), StoreError> {
    if read_user_version(conn)? < 1 || has_clean_shutdown_marker(conn)? {
        return Ok(());
    }
    require_ok(conn, "PRAGMA quick_check", "the startup quick_check")
}

/// Whether `store_meta` holds the clean-shutdown marker. Open reaches
/// this only after [`examine_existing`] accepted the store, so
/// `store_meta` exists with the identity keys.
fn has_clean_shutdown_marker(conn: &Connection) -> Result<bool, StoreError> {
    let present: i64 = conn
        .query_row(
            "SELECT count(*) FROM store_meta WHERE key = ?1",
            [CLEAN_SHUTDOWN_KEY],
            |row| row.get(0),
        )
        .map_err(|e| crate::schema::classify(e, "read the clean-shutdown marker"))?;
    Ok(present > 0)
}

/// Record the clean-shutdown marker (spec/03 "Integrity") as the
/// writer's last write: the clock's milliseconds since the epoch in
/// decimal. A failure is reported by the caller after the rest of the
/// close ran — the next open's `quick_check` is the safety net.
fn record_clean_shutdown(writer: &Writer, clock: &dyn Clock) -> Result<(), StoreError> {
    let at = clock.now();
    let future = writer.submit(WritePriority::Index, move |tx| {
        tx.execute(
            "INSERT INTO store_meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            rusqlite::params![CLEAN_SHUTDOWN_KEY, at.as_millis().to_string()],
        )
        .map_err(|e| crate::schema::classify(e, "record the clean-shutdown marker"))?;
        Ok(())
    });
    await_write(future)
}

/// Drive one [`WriteFuture`] to completion on the calling thread: a
/// thread-park waker, as in tests, since `close` is synchronous and
/// the future works under any executor (a wake from the writer thread
/// unparks this one; a park before the wake keeps its permit).
fn await_write<T>(future: WriteFuture<T>) -> Result<T, StoreError> {
    use std::task::{Context, Poll, Wake, Waker};

    /// Wakes by unparking the blocked closing thread.
    struct ThreadParker(std::thread::Thread);

    impl Wake for ThreadParker {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }

    let waker = Waker::from(Arc::new(ThreadParker(std::thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut future = std::pin::pin!(future);
    loop {
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(output) => return output,
            Poll::Pending => std::thread::park(),
        }
    }
}

/// A store opened for reading only (spec/02 "Migrations", plan M0.6):
/// an existing store — newer or current — under the lease, served by
/// the reader pool. No writer, no migrations: `state.sqlite` and its
/// WAL keep their bytes. Like any WAL reader, SQLite may create its
/// shared-memory index (`-shm`) and an empty WAL.
pub struct ReadOnlyStore {
    readers: Readers,
    lease: Box<dyn Lease>,
}

impl ReadOnlyStore {
    /// Run one read through the reader pool, as [`Store::read`].
    pub fn read<T>(
        &self,
        f: impl FnOnce(&rusqlite::Transaction) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        self.readers.read(f)
    }

    /// Close the readers — none of them checkpoints, by construction —
    /// then release the lease. Nothing reaches `state.sqlite` or the
    /// WAL.
    pub fn close(self) -> Result<(), StoreError> {
        let Self { readers, lease } = self;
        let closed = readers.close();
        drop(lease);
        closed
    }
}

/// The connection and lease aren't printable; the type name is all a
/// test needs.
impl std::fmt::Debug for ReadOnlyStore {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("ReadOnlyStore")
    }
}

/// Open an existing store for reading only (spec/02 "Migrations"): a
/// store newer than this app opens here — the writable open refuses
/// it with `NewerStore` — and a current one passes the usual checks;
/// version 0 or foreign is `Corrupt`. The layout must already exist
/// (`NotFound` otherwise) and is never created; `state.sqlite` is
/// examined read-only and left byte-for-byte unchanged, WAL frames a
/// crash left included.
pub fn open_read_only(paths: &StorePaths, seams: &Seams) -> Result<ReadOnlyStore, StoreError> {
    open_read_only_with(paths, seams, READER_PRAGMAS, &PINNED_SQLITE, MIGRATIONS)
}

/// `open_read_only` with the settings table, pinned version and
/// migration list as parameters, so tests can vary them as for
/// [`open_with`].
pub(crate) fn open_read_only_with(
    paths: &StorePaths,
    seams: &Seams,
    reader_pragmas: &[PragmaSpec],
    pinned: &PinnedSqlite,
    migrations: &[Migration],
) -> Result<ReadOnlyStore, StoreError> {
    let fs = seams.fs();
    // No creation, not even the lock file: an absent store (or an
    // absent parent) is `NotFound` before anything is touched.
    require_existing(fs, paths)?;
    let paths = &resolve_root(fs, paths)?;
    check_location(fs, paths.data_root())?;
    let lease = take_lease(fs, paths.lock_file())?;
    // Again under the lease: it may have changed meanwhile.
    require_existing(fs, paths)?;
    examine_read_only(fs, paths, pinned, migrations)?;
    let readers = Readers::open(paths.state_db(), reader_pragmas)?;
    Ok(ReadOnlyStore { readers, lease })
}

/// The store's identity (spec/03): the data root with symlinks
/// resolved through the seam, so every spelling of one store derives
/// one lock (`alias -> store` locks `store.lock`). A root that doesn't
/// exist yet resolves through its parent, which must exist.
fn resolve_root(fs: &dyn Fs, paths: &StorePaths) -> Result<StorePaths, StoreError> {
    // Walk up to the nearest level that exists; the levels above it
    // that don't are appended as spelled.
    let mut existing = paths.data_root();
    let mut missing = Vec::new();
    while fs
        .metadata(existing)
        .map_err(fs_err("stat", paths))?
        .is_none()
    {
        let (Some(name), Some(parent)) = (existing.file_name(), existing.parent()) else {
            return Err(StoreError::unavailable(format!(
                "no level of {} exists",
                paths.data_root().display()
            )));
        };
        missing.push(name);
        existing = parent;
    }
    let mut resolved = fs
        .canonicalize(existing)
        .map_err(fs_err("resolve", paths))?;
    for name in missing.iter().rev() {
        resolved.push(name);
    }
    StorePaths::new(resolved)
}

/// A writable open's data root may sit under directories that don't
/// exist yet, such as a fresh system's `~/.local/share` (spec/03):
/// create each missing ancestor, flushing it into its parent, so the
/// lock file and the root have somewhere to live. Run after the
/// location check and before the lease; an absent ancestor means no
/// other process can hold the lock beside the root.
fn create_missing_ancestors(fs: &dyn Fs, paths: &StorePaths) -> Result<(), StoreError> {
    let mut missing = Vec::new();
    let mut dir = paths
        .data_root()
        .parent()
        .expect("StorePaths guarantees a final name");
    while fs.metadata(dir).map_err(fs_err("stat", paths))?.is_none() {
        missing.push(dir);
        dir = dir.parent().expect("the filesystem root exists");
    }
    for dir in missing.into_iter().rev() {
        match fs.create_dir(dir) {
            Ok(()) => {}
            // Another process got there first: fine, it's a directory.
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            Err(e) => return Err(fs_err("create", paths)(e)),
        }
        let parent = dir.parent().expect("a created level has a parent");
        fs.sync_dir(parent).map_err(fs_err("flush", paths))?;
    }
    Ok(())
}

/// Step 1 (spec/03): the data root lives on a local filesystem. Not
/// `Local` is refused with an explanation; nothing is created.
fn check_location(fs: &dyn Fs, data_root: &Path) -> Result<(), StoreError> {
    match fs.location(data_root).map_err(|e| {
        StoreError::unavailable(format!("cannot classify {}: {e}", data_root.display()))
    })? {
        Location::Local => Ok(()),
        Location::Synced { service } => Err(StoreError::unavailable(format!(
            "the data root is inside {service}, a synced folder; choose a directory on a local filesystem"
        ))),
        Location::Network { filesystem } => Err(StoreError::unavailable(format!(
            "the data root is on a network filesystem ({filesystem}); choose a local directory"
        ))),
    }
}

/// Step 2 (spec/03): one process owns the store, holding
/// `<data root>.lock` for its lifetime. `Held` is `Locked`; nothing
/// else is created.
fn take_lease(fs: &dyn Fs, lock_file: &Path) -> Result<Box<dyn Lease>, StoreError> {
    fs.lock_exclusive(lock_file).map_err(|e| match e {
        LockError::Held => StoreError::Locked,
        // e.g. the data root's parent is missing and the sibling lock
        // file cannot be created.
        LockError::Io(e) => {
            StoreError::unavailable(format!("cannot take {}: {e}", lock_file.display()))
        }
    })
}

/// Step 3 (spec/03 "On-disk layout").
fn ensure_layout(fs: &dyn Fs, paths: &StorePaths) -> Result<(), StoreError> {
    match fs.metadata(paths.state_db()).map_err(fs_err("stat", paths)) {
        Ok(None) => create_layout(fs, paths),
        // Empty is fine: SQLite creates the database in it (a creation
        // interrupted right after SQLite made the file).
        Ok(Some(md))
            if md.kind() == FileKind::File && !md.is_empty() && md.len() < SQLITE_HEADER_LEN =>
        {
            Err(StoreError::corrupt(format!(
                "{} is {} bytes, shorter than a SQLite header",
                paths.state_db().display(),
                md.len()
            )))
        }
        Ok(Some(md)) if md.kind() == FileKind::File => check_layout(fs, paths),
        // A state.sqlite that isn't a file can never open as a database.
        Ok(Some(_)) => Err(StoreError::corrupt(format!(
            "{} is not a file",
            paths.state_db().display()
        ))),
        Err(e) => Err(e),
    }
}

/// With `state.sqlite` present, every layout directory must exist as a
/// directory; a missing one is `Corrupt` and is not recreated — that
/// would hide data loss.
fn check_layout(fs: &dyn Fs, paths: &StorePaths) -> Result<(), StoreError> {
    for dir in paths.layout_dirs() {
        match fs.metadata(dir).map_err(fs_err("stat", paths))? {
            Some(md) if md.kind() == FileKind::Directory => {}
            _ => {
                return Err(StoreError::corrupt(format!(
                    "{} is missing or not a directory",
                    dir.display()
                )));
            }
        }
    }
    Ok(())
}

/// Without `state.sqlite`, the data root must be absent, empty, or
/// hold only layout directories and no files (an interrupted
/// creation); anything else is `Unavailable`. Then the missing
/// directories are made in spec order, each is flushed, then the data
/// root and its parent — all before `state.sqlite` exists.
pub(crate) fn create_layout(fs: &dyn Fs, paths: &StorePaths) -> Result<(), StoreError> {
    let root = paths.data_root();
    match fs.metadata(root).map_err(fs_err("stat", paths))? {
        None => fs.create_dir(root).map_err(fs_err("create", paths))?,
        Some(md) if md.kind() == FileKind::Directory => check_partial_layout(fs, paths)?,
        Some(_) => {
            return Err(StoreError::unavailable(format!(
                "{} exists and is not a directory",
                root.display()
            )));
        }
    }

    // The missing layout directories in spec order, then the flushes:
    // each layout directory, the data root, its parent.
    for dir in paths.layout_dirs() {
        if fs.metadata(dir).map_err(fs_err("stat", paths))?.is_none() {
            fs.create_dir(dir).map_err(fs_err("create", paths))?;
        }
    }
    for dir in paths.layout_dirs() {
        fs.sync_dir(dir).map_err(fs_err("flush", paths))?;
    }
    fs.sync_dir(root).map_err(fs_err("flush", paths))?;
    flush_root_chain(fs, paths)
}

/// Flush the data root's entry into its parent, and every ancestor's
/// into its own, up to the filesystem root. Any of them may have been
/// created for this store, by this open or an earlier one whose flush
/// failed (spec/03: each created ancestor flushed into its parent), and
/// existing alone doesn't prove a directory entry durable. A store is
/// created once, so the extra flushes cost nothing that matters.
fn flush_root_chain(fs: &dyn Fs, paths: &StorePaths) -> Result<(), StoreError> {
    let mut level = paths.data_root().parent();
    while let Some(dir) = level {
        fs.sync_dir(dir).map_err(fs_err("flush", paths))?;
        level = dir.parent();
    }
    Ok(())
}

/// The data root may hold only the layout directories themselves —
/// nothing else, and no files. Anything else is a root the store
/// didn't create: refuse rather than take it over.
fn check_partial_layout(fs: &dyn Fs, paths: &StorePaths) -> Result<(), StoreError> {
    let allowed: Vec<PathBuf> = paths
        .layout_dirs()
        .iter()
        .map(|d| d.to_path_buf())
        .collect();
    let mut pending = vec![paths.data_root().to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in fs.read_dir(&dir).map_err(fs_err("list", paths))? {
            let layout_dir = allowed.contains(&entry)
                && matches!(
                    fs.metadata(&entry).map_err(fs_err("stat", paths))?,
                    Some(md) if md.kind() == FileKind::Directory
                );
            if !layout_dir {
                return Err(StoreError::unavailable(format!(
                    "{} is not part of the store layout; refusing the data root",
                    entry.display()
                )));
            }
            pending.push(entry);
        }
    }
    Ok(())
}

/// How the one connection constructor opens `state.sqlite` (spec/03
/// "Connections"): the writer read-write, every reader and the
/// examination before it read-only, so nothing but the writer can
/// write or checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum Access {
    /// The store's one writer connection.
    ReadWrite,
    /// A reader: opened with `SQLITE_OPEN_READ_ONLY`, so SQLite
    /// itself refuses any write through it.
    ReadOnly,
}

/// Step 4 (spec/03 "SQLite configuration"): the one connection
/// constructor. Open `state.sqlite`, apply each setting and read it
/// back; a `state.sqlite` SQLite can't read as a database is `Corrupt`
/// and stays byte-for-byte untouched. An existing database reaches it
/// only after [`examine_existing`] accepted it.
pub(crate) fn open_connection(
    state_db: &Path,
    access: Access,
    pragmas: &[PragmaSpec],
) -> Result<Connection, StoreError> {
    let flags = match access {
        Access::ReadWrite => {
            OpenFlags::SQLITE_OPEN_READ_WRITE
                | OpenFlags::SQLITE_OPEN_CREATE
                | OpenFlags::SQLITE_OPEN_NO_MUTEX
        }
        Access::ReadOnly => OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    };
    let conn = Connection::open_with_flags(state_db, flags).map_err(sqlite_err)?;
    // Closing doesn't checkpoint until the caller lifts this (`open_with`
    // on success): a failed open leaves the file as its steps left it,
    // with WAL frames a crash left behind still in the WAL (spec/02).
    conn.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, true)
        .map_err(sqlite_err)?;
    for spec in pragmas {
        // The table's `set` values are constants, so interpolating the
        // pragma line is safe.
        conn.execute_batch(&format!("PRAGMA {} = {}", spec.name, spec.set))
            .map_err(|e| setting_err(spec.name, e))?;
        let got: Value = conn
            .pragma_query_value(None, spec.name, |row| row.get(0))
            .map_err(|e| setting_err(spec.name, e))?;
        if !spec.expect.matches(&got) {
            return Err(StoreError::unavailable(format!(
                "PRAGMA {} reads back {got:?}; expected {:?}",
                spec.name, spec.expect
            )));
        }
    }
    // DQS off via sqlite3_db_config, both flags, read back with
    // db_config.
    for config in [
        DbConfig::SQLITE_DBCONFIG_DQS_DML,
        DbConfig::SQLITE_DBCONFIG_DQS_DDL,
    ] {
        conn.set_db_config(config, false).map_err(sqlite_err)?;
        if conn.db_config(config).map_err(sqlite_err)? {
            return Err(StoreError::unavailable(format!(
                "{config:?} reads back enabled; expected disabled"
            )));
        }
    }
    Ok(conn)
}

/// The read-only open's look at the layout (plan M0.6): create
/// nothing. A missing data root or `state.sqlite` is `NotFound`; one
/// that exists answers to the writable open's rules — short or
/// non-file databases and missing layout directories are `Corrupt`.
fn require_existing(fs: &dyn Fs, paths: &StorePaths) -> Result<(), StoreError> {
    match fs
        .metadata(paths.data_root())
        .map_err(fs_err("stat", paths))?
    {
        Some(md) if md.kind() == FileKind::Directory => {}
        _ => {
            return Err(StoreError::not_found(format!(
                "the data root {} does not exist",
                paths.data_root().display()
            )));
        }
    }
    match fs
        .metadata(paths.state_db())
        .map_err(fs_err("stat", paths))?
    {
        None => Err(StoreError::not_found(format!(
            "{} does not exist",
            paths.state_db().display()
        ))),
        Some(md)
            if md.kind() == FileKind::File && !md.is_empty() && md.len() < SQLITE_HEADER_LEN =>
        {
            Err(StoreError::corrupt(format!(
                "{} is {} bytes, shorter than a SQLite header",
                paths.state_db().display(),
                md.len()
            )))
        }
        // An empty file is left to the examination: SQLite reads it as
        // an empty database, which no store lives in, and reports that.
        Some(md) if md.kind() == FileKind::File => check_layout(fs, paths),
        Some(_) => Err(StoreError::corrupt(format!(
            "{} is not a file",
            paths.state_db().display()
        ))),
    }
}

/// Before the writer touches an existing `state.sqlite`, judge it
/// through a read-only connection (spec/02 "Migrations", spec/03): the
/// SQLite build first, as spec/02 orders it, then not WAL, a newer
/// store, or not a Sweetbriar store is refused untouched. A read-write
/// connection would write even if it only read: closing it checkpoints
/// WAL frames a crash left behind into the file. A read-only one never
/// does.
fn examine_existing(
    fs: &dyn Fs,
    paths: &StorePaths,
    pinned: &PinnedSqlite,
    migrations: &[Migration],
) -> Result<(), StoreError> {
    let existing = fs
        .metadata(paths.state_db())
        .map_err(fs_err("stat", paths))?
        .is_some_and(|md| !md.is_empty());
    if !existing {
        return Ok(());
    }

    let conn = verdict_connection(paths.state_db())?;
    check_sqlite_version(&conn, pinned)?;
    refuse_other_journal_modes(&conn)?;
    check_existing(&conn, migrations)
}

/// The read-only open's examination of an existing `state.sqlite`
/// (spec/02 "Migrations", plan M0.6): as [`examine_existing`], except
/// a newer `user_version` is accepted (a newer app may have changed
/// `store_meta`'s shape). Still read-only, still WAL-only, still
/// version-checked against the pin — so nothing is written either way.
fn examine_read_only(
    fs: &dyn Fs,
    paths: &StorePaths,
    pinned: &PinnedSqlite,
    migrations: &[Migration],
) -> Result<(), StoreError> {
    let existing = fs
        .metadata(paths.state_db())
        .map_err(fs_err("stat", paths))?
        .is_some_and(|md| !md.is_empty());
    if !existing {
        // `require_existing` allowed an empty file through; SQLite
        // reads it as an empty database, which no store lives in.
        return Err(StoreError::corrupt(format!(
            "{} is empty; not an existing store",
            paths.state_db().display()
        )));
    }

    let conn = verdict_connection(paths.state_db())?;
    check_sqlite_version(&conn, pinned)?;
    refuse_other_journal_modes(&conn)?;
    check_existing_read_only(&conn, migrations)
}

/// The connection the examinations judge through: read-only, so it
/// never writes or checkpoints, and not trusting the schema of a file
/// that may not be ours, so no schema-defined function runs.
fn verdict_connection(state_db: &Path) -> Result<Connection, StoreError> {
    let conn = Connection::open_with_flags(
        state_db,
        OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_NO_MUTEX,
    )
    .map_err(sqlite_err)?;
    conn.execute_batch("PRAGMA trusted_schema = OFF")
        .map_err(sqlite_err)?;
    Ok(conn)
}

/// `journal_mode=WAL` persists in the file, so the constructor sets it
/// only on a new, empty database (spec/03). An existing database in
/// another mode isn't a Sweetbriar store, and switching it would
/// rewrite it: refuse it, as `Corrupt`.
fn refuse_other_journal_modes(conn: &Connection) -> Result<(), StoreError> {
    let mode: String = conn
        .query_row("PRAGMA journal_mode", [], |row| row.get(0))
        .map_err(sqlite_err)?;
    if mode.eq_ignore_ascii_case("wal") {
        return Ok(());
    }
    Err(StoreError::corrupt(format!(
        "state.sqlite uses journal mode {mode}; a Sweetbriar store is always WAL, so this isn't one"
    )))
}

/// Step 5 (spec/03): `sqlite_version()` and `sqlite_source_id()` equal
/// the pinned constants, else `Internal`. The release bounds (floor,
/// withdrawn) get their own message so a mis-linked build names the
/// right reason.
pub(crate) fn check_sqlite_version(
    conn: &Connection,
    pinned: &PinnedSqlite,
) -> Result<(), StoreError> {
    let version: String = conn
        .query_row("SELECT sqlite_version()", [], |r| r.get(0))
        .map_err(sqlite_err)?;
    let source_id: String = conn
        .query_row("SELECT sqlite_source_id()", [], |r| r.get(0))
        .map_err(sqlite_err)?;
    let number = rusqlite::version_number();
    if number < SQLITE_VERSION_FLOOR {
        return Err(StoreError::internal(format!(
            "linked SQLite {version} is below the supported floor 3.51.3"
        )));
    }
    if number == SQLITE_VERSION_WITHDRAWN {
        return Err(StoreError::internal(
            "linked SQLite 3.52.0 was withdrawn; rebuild with the pinned rusqlite".to_owned(),
        ));
    }
    if number == pinned.version_number && version == pinned.version && source_id == pinned.source_id
    {
        return Ok(());
    }
    Err(StoreError::internal(format!(
        "linked SQLite is {version} ({source_id}, #{number}); this build pins {} ({}, #{})",
        pinned.version, pinned.source_id, pinned.version_number
    )))
}

/// A `state.sqlite` SQLite can't read as a database is `Corrupt`
/// (untouched); every other SQLite failure is `Unavailable`.
fn sqlite_err(e: rusqlite::Error) -> StoreError {
    match e {
        rusqlite::Error::SqliteFailure(ref err, _)
            if matches!(
                err.code,
                ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt
            ) =>
        {
            StoreError::corrupt(format!("state.sqlite is not a readable database: {e}"))
        }
        _ => StoreError::unavailable(format!("cannot open state.sqlite: {e}")),
    }
}

/// `sqlite_err` with the setting's name attached; `Corrupt` stays
/// `Corrupt`.
fn setting_err(name: &'static str, e: rusqlite::Error) -> StoreError {
    if matches!(
        e,
        rusqlite::Error::SqliteFailure(ref err, _)
            if matches!(err.code, ErrorCode::NotADatabase | ErrorCode::DatabaseCorrupt)
    ) {
        sqlite_err(e)
    } else {
        StoreError::unavailable(format!("PRAGMA {name}: {e}"))
    }
}

/// A filesystem failure while preparing the store is environmental:
/// `Unavailable`, naming the operation and the path.
fn fs_err<'a>(
    op: &'static str,
    paths: &'a StorePaths,
) -> impl FnOnce(io::Error) -> StoreError + 'a {
    move |e| {
        StoreError::unavailable(format!(
            "cannot {op} under {}: {e}",
            paths.data_root().display()
        ))
    }
}

/// One row of the connection-settings table (spec/03): `PRAGMA <name>
/// = <set>`, and the value it must read back as.
#[derive(Debug, Clone)]
pub(crate) struct PragmaSpec {
    name: &'static str,
    set: &'static str,
    expect: Expect,
}

/// A pragma's expected read-back value; read-back types differ
/// (`wal` is text, `synchronous` an integer).
#[derive(Debug, Clone)]
pub(crate) enum Expect {
    Int(i64),
    Text(&'static str),
}

impl Expect {
    fn matches(&self, got: &Value) -> bool {
        match (self, got) {
            (Self::Int(want), Value::Integer(got)) => *want == *got,
            (Self::Text(want), Value::Text(got)) => *want == got,
            _ => false,
        }
    }
}

/// The writer connection's settings in spec order (spec/03). macOS
/// adds its two fullfsync rows — the only `cfg(target_os)` in this
/// crate.
pub(crate) const WRITER_PRAGMAS: &[PragmaSpec] = &[
    PragmaSpec {
        name: "journal_mode",
        set: "WAL",
        expect: Expect::Text("wal"),
    },
    PragmaSpec {
        name: "synchronous",
        set: "FULL",
        expect: Expect::Int(2),
    },
    PragmaSpec {
        name: "foreign_keys",
        set: "ON",
        expect: Expect::Int(1),
    },
    PragmaSpec {
        name: "busy_timeout",
        set: "5000",
        expect: Expect::Int(5000),
    },
    PragmaSpec {
        name: "trusted_schema",
        set: "OFF",
        expect: Expect::Int(0),
    },
    PragmaSpec {
        name: "wal_autocheckpoint",
        set: "1000",
        expect: Expect::Int(1000),
    },
    #[cfg(target_os = "macos")]
    PragmaSpec {
        name: "fullfsync",
        set: "ON",
        expect: Expect::Int(1),
    },
    #[cfg(target_os = "macos")]
    PragmaSpec {
        name: "checkpoint_fullfsync",
        set: "ON",
        expect: Expect::Int(1),
    },
];

/// The reader connections' settings (spec/03 "Connections"): the
/// writer's rows, except readers never checkpoint on their own
/// (`wal_autocheckpoint = 0`; only the writer checkpoints), plus
/// `query_only`, so a read can't write even by accident. The
/// constructor sets and reads back each one; `NO_CKPT_ON_CLOSE`
/// stays on for readers, so closing one never checkpoints either.
pub(crate) const READER_PRAGMAS: &[PragmaSpec] = &[
    PragmaSpec {
        name: "journal_mode",
        set: "WAL",
        expect: Expect::Text("wal"),
    },
    PragmaSpec {
        name: "synchronous",
        set: "FULL",
        expect: Expect::Int(2),
    },
    PragmaSpec {
        name: "foreign_keys",
        set: "ON",
        expect: Expect::Int(1),
    },
    PragmaSpec {
        name: "busy_timeout",
        set: "5000",
        expect: Expect::Int(5000),
    },
    PragmaSpec {
        name: "trusted_schema",
        set: "OFF",
        expect: Expect::Int(0),
    },
    PragmaSpec {
        name: "wal_autocheckpoint",
        set: "0",
        expect: Expect::Int(0),
    },
    PragmaSpec {
        name: "query_only",
        set: "ON",
        expect: Expect::Int(1),
    },
    #[cfg(target_os = "macos")]
    PragmaSpec {
        name: "fullfsync",
        set: "ON",
        expect: Expect::Int(1),
    },
    #[cfg(target_os = "macos")]
    PragmaSpec {
        name: "checkpoint_fullfsync",
        set: "ON",
        expect: Expect::Int(1),
    },
];

#[cfg(test)]
mod tests;
