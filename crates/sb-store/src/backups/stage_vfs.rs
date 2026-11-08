//! Confined writable SQLite VFS over retained private file capabilities
//! (M1.8e3b2b3/b4). The rollback entry takes database and two journals;
//! the WAL2 entry adds an empty WAL file and exclusive heap wal-index.
//! File I/O resolves to retained inodes and grants no stage eligibility.
//! The qualifier owns SQL limits, cancellation,
//! schema and integrity checks, journal-mode policy and publication.
//!
//! One VFS is registered per connection context. Its `sqlite3_vfs` is
//! the first field of an extended `repr(C)` struct, so callbacks cast
//! the base pointer back to their own context. The default VFS's
//! non-file callbacks and `pAppData` are inherited unchanged; only the
//! file callbacks, pathname resolution and dynamic loading are
//! replaced. The wrapper keeps the connection and the boxed VFS alive
//! until SQLite close succeeds. A live connection is preserved only
//! when SQLite itself refuses to close, never for eligibility errors.

use std::cell::Cell;
use std::ffi::{CStr, CString, c_char, c_int, c_void};
use std::panic::{self, AssertUnwindSafe};
use std::sync::Arc;
#[cfg(test)]
use std::sync::atomic::AtomicUsize;
use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};

use rusqlite::{Connection, OpenFlags, ffi};
use sb_types::fs::{FileIdentity, ReadWriteFile};

use super::stage_io::{
    file_size, read as io_read, sync as io_sync, truncate as io_truncate, write as io_write,
};

/// The file-type bits SQLite passes to `xOpen`. Exactly one must be set.
const TYPE_MASK: c_int = 0x0008_7F00;
/// A conservative sector size: no atomic-write or powersafe claims.
const SECTOR: c_int = 4096;

static NEXT_ID: AtomicU64 = AtomicU64::new(0);

#[derive(Clone, Copy, PartialEq, Eq)]
enum Role {
    Main,
    RollbackJournal,
    StatementJournal,
    Wal,
}

struct Wal {
    file: Arc<dyn ReadWriteFile>,
    name: CString,
    exists: AtomicBool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Checkpoint {
    Required,
    Verified(u64),
}

struct Context {
    main_db: Arc<dyn ReadWriteFile>,
    rollback_journal: Arc<dyn ReadWriteFile>,
    statement_journal: Arc<dyn ReadWriteFile>,
    wal: Option<Wal>,
    main_name: CString,
    journal_name: CString,
    /// One main connection ever; a second `MAIN_DB` open is refused.
    claimed: AtomicBool,
    /// An ordinary I/O failure or contained panic: close must not succeed.
    io_error: AtomicBool,
    /// File mutations since open; a checkpoint covers one exact generation.
    mutations: AtomicU64,
    /// Logical rollback-journal existence, for `xAccess`.
    journal_exists: AtomicBool,
    #[cfg(test)]
    opens: AtomicUsize,
    #[cfg(test)]
    closes: AtomicUsize,
}

/// The registered VFS: the default callbacks first, then this context.
#[repr(C)]
struct Vfs {
    base: ffi::sqlite3_vfs,
    name: CString,
    context: Arc<Context>,
}

/// SQLite's `sqlite3_file` plus the retained capability it is bound to.
#[repr(C)]
struct StageFile {
    base: ffi::sqlite3_file,
    context: *const Context,
    role: Role,
    delete_on_close: bool,
}

static METHODS: ffi::sqlite3_io_methods = ffi::sqlite3_io_methods {
    iVersion: 1,
    xClose: Some(close),
    xRead: Some(read),
    xWrite: Some(write),
    xTruncate: Some(truncate),
    xSync: Some(sync),
    xFileSize: Some(size),
    xLock: Some(lock),
    xUnlock: Some(lock),
    xCheckReservedLock: Some(reserved),
    xFileControl: Some(control),
    xSectorSize: Some(sector),
    xDeviceCharacteristics: Some(characteristics),
    xShmMap: None,
    xShmLock: None,
    xShmBarrier: None,
    xShmUnmap: None,
    xFetch: None,
    xUnfetch: None,
};

/// A retained, unqualified writable connection. It is not a stage and
/// certifies no eligibility; the qualifier consumes the borrowed
/// connection and owns every SQL policy.
pub(crate) struct ConfinedSqlite {
    connection: Option<Connection>,
    vfs: Option<Box<Vfs>>,
    context: Arc<Context>,
    checkpoint: Cell<Checkpoint>,
}

/// Why the private output is ineligible after close. Only a SQLite
/// close failure retains a live connection and its callback memory.
#[derive(Debug)]
pub(crate) enum CloseError {
    UncommittedTransaction,
    IoError,
    WalNotCheckpointed,
    InvalidWalState,
    Sqlite(rusqlite::Error),
}

impl std::fmt::Debug for ConfinedSqlite {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("ConfinedSqlite")
            .field("claimed", &self.context.claimed.load(Ordering::SeqCst))
            .field("io_error", &self.context.io_error.load(Ordering::SeqCst))
            .finish_non_exhaustive()
    }
}

impl ConfinedSqlite {
    /// Open the retained database through a fresh private VFS. The three
    /// capabilities must be distinct files, both journals must be empty,
    /// and a non-empty database must already be in rollback-journal
    /// format (header bytes 18/19 both 1). No SQL runs during open.
    pub(crate) fn open(
        main_db: Arc<dyn ReadWriteFile>,
        rollback_journal: Arc<dyn ReadWriteFile>,
        statement_journal: Arc<dyn ReadWriteFile>,
    ) -> rusqlite::Result<Self> {
        Self::open_files(main_db, rollback_journal, statement_journal, None)
    }

    /// Open a standalone WAL2 copy over four distinct retained files.
    /// Only the schema-free locking-mode pragma runs here. The qualifier
    /// must install SQL hardening, limits and cancellation before any other
    /// SQL, including journal-mode inspection and `checkpoint_wal`.
    pub(crate) fn open_wal(
        main_db: Arc<dyn ReadWriteFile>,
        rollback_journal: Arc<dyn ReadWriteFile>,
        statement_journal: Arc<dyn ReadWriteFile>,
        wal: Arc<dyn ReadWriteFile>,
    ) -> rusqlite::Result<Self> {
        let writer = Self::open_files(main_db, rollback_journal, statement_journal, Some(wal))?;
        // Unlike journal_mode, locking_mode has no PragFlg_NeedSchema in
        // the pinned SQLite. Establish heap WAL-index mode before schema.
        let mode: String =
            writer
                .connection()
                .query_row("PRAGMA locking_mode=EXCLUSIVE", [], |row| row.get(0))?;
        if mode != "exclusive" {
            return Err(unsupported("exclusive WAL locking was not established"));
        }
        Ok(writer)
    }

    fn open_files(
        main_db: Arc<dyn ReadWriteFile>,
        rollback_journal: Arc<dyn ReadWriteFile>,
        statement_journal: Arc<dyn ReadWriteFile>,
        wal_file: Option<Arc<dyn ReadWriteFile>>,
    ) -> rusqlite::Result<Self> {
        verify(
            main_db.as_ref(),
            rollback_journal.as_ref(),
            statement_journal.as_ref(),
            wal_file.as_deref(),
        )?;
        let id = NEXT_ID.fetch_add(1, Ordering::Relaxed);
        let vfs_name = CString::new(format!("sweetbriar-stage-{id}"))
            .map_err(|_| unsupported("VFS name is not representable"))?;
        let main_name = CString::new(format!("sb-stage-{id}.db"))
            .map_err(|_| unsupported("database name is not representable"))?;
        let journal_name = CString::new(format!("sb-stage-{id}.db-journal"))
            .map_err(|_| unsupported("journal name is not representable"))?;
        let wal = wal_file
            .map(|file| -> rusqlite::Result<Wal> {
                Ok(Wal {
                    file,
                    name: CString::new(format!("sb-stage-{id}.db-wal"))
                        .map_err(|_| unsupported("WAL name is not representable"))?,
                    exists: AtomicBool::new(true),
                })
            })
            .transpose()?;
        let context = Arc::new(Context {
            main_db,
            rollback_journal,
            statement_journal,
            wal,
            main_name,
            journal_name,
            claimed: AtomicBool::new(false),
            io_error: AtomicBool::new(false),
            mutations: AtomicU64::new(0),
            journal_exists: AtomicBool::new(false),
            #[cfg(test)]
            opens: AtomicUsize::new(0),
            #[cfg(test)]
            closes: AtomicUsize::new(0),
        });
        let mut vfs = Box::new(Vfs {
            base: default_vfs()?,
            name: vfs_name,
            context: Arc::clone(&context),
        });
        vfs.base.pNext = std::ptr::null_mut();
        let name_ptr = vfs.name.as_ptr();
        vfs.base.zName = name_ptr;
        vfs.base.szOsFile = std::mem::size_of::<StageFile>() as c_int;
        vfs.base.xOpen = Some(vfs_open);
        vfs.base.xDelete = Some(vfs_delete);
        vfs.base.xAccess = Some(vfs_access);
        vfs.base.xFullPathname = Some(full_path);
        vfs.base.xDlOpen = Some(dl_open);
        vfs.base.xDlError = Some(dl_error);
        vfs.base.xDlSym = Some(dl_sym);
        vfs.base.xDlClose = Some(dl_close);
        let registered = &mut vfs.base as *mut ffi::sqlite3_vfs;
        let rc = unsafe { ffi::sqlite3_vfs_register(registered, 0) };
        if rc != ffi::SQLITE_OK {
            return Err(rusqlite::Error::SqliteFailure(ffi::Error::new(rc), None));
        }
        let path = vfs
            .context
            .main_name
            .to_str()
            .map_err(|_| unsupported("database name"))?;
        let result = Connection::open_with_flags_and_vfs(
            path,
            OpenFlags::SQLITE_OPEN_READ_WRITE | OpenFlags::SQLITE_OPEN_CREATE,
            vfs.name.as_c_str(),
        );
        match result {
            Ok(connection) => {
                let writer = Self {
                    connection: Some(connection),
                    vfs: Some(vfs),
                    context,
                    checkpoint: Cell::new(Checkpoint::Required),
                };
                // URI filenames can select another registered VFS and
                // bypass ours. Refuse ATTACH (including VACUUM's internal
                // ATTACH) before running any SQL. No hook can replace this.
                writer
                    .connection()
                    .set_limit(rusqlite::limits::Limit::SQLITE_LIMIT_ATTACHED, 0)?;
                Ok(writer)
            }
            Err(error) => {
                // The failed connection was already closed by SQLite.
                unsafe { ffi::sqlite3_vfs_unregister(registered) };
                drop(vfs);
                Err(error)
            }
        }
    }

    /// The borrowed connection, for the qualifier's limits and SQL.
    /// Keep SQLITE_LIMIT_ATTACHED at zero to preserve file confinement.
    pub(crate) fn connection(&self) -> &Connection {
        self.checkpoint.set(Checkpoint::Required);
        self.connection.as_ref().expect("connection is open")
    }

    /// Run under the qualifier's SQL limits/cancellation guard: both
    /// journal_mode and wal_checkpoint can load schema. Call last, then
    /// close without borrowing the connection again. This state grants no
    /// qualification; subsequent file mutations invalidate it, including
    /// writes through a connection reference borrowed before checkpoint.
    pub(crate) fn checkpoint_wal(&self) -> Result<(), CloseError> {
        self.checkpoint.set(Checkpoint::Required);
        let Some(wal) = &self.context.wal else {
            return Err(CloseError::InvalidWalState);
        };
        let connection = self.connection.as_ref().expect("connection is open");
        if !connection.is_autocommit() {
            return Err(CloseError::UncommittedTransaction);
        }
        if self.context.io_error.load(Ordering::SeqCst) {
            return Err(CloseError::IoError);
        }
        let mode: String = connection
            .query_row("PRAGMA journal_mode", [], |row| row.get(0))
            .map_err(CloseError::Sqlite)?;
        if mode != "wal" {
            return Err(CloseError::InvalidWalState);
        }
        let result: (i64, i64, i64) = connection
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .map_err(CloseError::Sqlite)?;
        if result != (0, 0, 0) {
            return Err(CloseError::WalNotCheckpointed);
        }
        let empty = self.empty(&[wal.file.as_ref()]);
        if self.context.io_error.load(Ordering::SeqCst) {
            return Err(CloseError::IoError);
        }
        if !empty {
            return Err(CloseError::WalNotCheckpointed);
        }
        self.checkpoint.set(Checkpoint::Verified(
            self.context.mutations.load(Ordering::SeqCst),
        ));
        Ok(())
    }

    fn empty(&self, files: &[&dyn ReadWriteFile]) -> bool {
        let mut empty = true;
        for file in files {
            match file_size(*file) {
                Ok(0) => {}
                Ok(_) => empty = false,
                Err(_) => {
                    self.context.io_error.store(true, Ordering::SeqCst);
                    empty = false;
                }
            }
        }
        empty
    }

    /// Close and flush retained files. An unfinished transaction is
    /// rolled back by SQLite and reported as ineligible. Only SQLite's
    /// actual refusal to close may retain the connection and callbacks.
    pub(crate) fn close(mut self) -> Result<(), CloseError> {
        self.close_impl()
    }

    fn close_impl(&mut self) -> Result<(), CloseError> {
        let Some(connection) = self.connection.take() else {
            return Ok(());
        };
        let uncommitted = !connection.is_autocommit();
        // C close can checkpoint implicitly. Check the explicit checkpoint
        // generation and zero WAL beforehand so it cannot conceal later
        // writes, even if a retained borrow ran its own raw checkpoint.
        let needs_checkpoint = self.context.wal.as_ref().is_some_and(|wal| {
            self.checkpoint.get()
                != Checkpoint::Verified(self.context.mutations.load(Ordering::SeqCst))
                || !self.empty(&[wal.file.as_ref()])
        });
        if let Err((connection, error)) = connection.close() {
            self.connection = Some(connection);
            return Err(CloseError::Sqlite(error));
        }
        // Close can roll back a transaction and invoke file callbacks.
        // Flush afterward and inspect the sticky fault even when SQLite
        // itself ignored a callback failure. The adapter contains panics.
        for file in [
            &self.context.main_db,
            &self.context.rollback_journal,
            &self.context.statement_journal,
        ]
        .into_iter()
        .chain(self.context.wal.iter().map(|wal| &wal.file))
        {
            if io_sync(file.as_ref()) != ffi::SQLITE_OK {
                self.context.io_error.store(true, Ordering::SeqCst);
            }
        }
        let mut wal_valid = true;
        if let Some(wal) = &self.context.wal {
            wal_valid = self.empty(&[
                wal.file.as_ref(),
                self.context.rollback_journal.as_ref(),
                self.context.statement_journal.as_ref(),
            ]);
            let mut header = [0; 20];
            if io_read(self.context.main_db.as_ref(), &mut header, 0) != ffi::SQLITE_OK {
                self.context.io_error.store(true, Ordering::SeqCst);
                wal_valid = false;
            } else {
                wal_valid &= header[18..20] == [2, 2];
            }
        }
        if let Some(mut vfs) = self.vfs.take() {
            // SQLite has released every file and callback before this.
            unsafe { ffi::sqlite3_vfs_unregister(&mut vfs.base) };
        }
        if self.context.io_error.load(Ordering::SeqCst) {
            return Err(CloseError::IoError);
        }
        if uncommitted {
            return Err(CloseError::UncommittedTransaction);
        }
        if needs_checkpoint {
            return Err(CloseError::WalNotCheckpointed);
        }
        if !wal_valid {
            return Err(CloseError::InvalidWalState);
        }
        Ok(())
    }

    #[cfg(test)]
    fn context(&self) -> &Arc<Context> {
        &self.context
    }

    #[cfg(test)]
    fn vfs_name(&self) -> &CStr {
        self.vfs.as_ref().expect("vfs").name.as_c_str()
    }

    #[cfg(test)]
    fn access(&self, name: &str) -> c_int {
        let vfs = self.vfs.as_ref().expect("vfs");
        let name = CString::new(name).expect("name");
        let registered = &vfs.base as *const ffi::sqlite3_vfs as *mut ffi::sqlite3_vfs;
        let mut out = 0;
        let x_access = vfs.base.xAccess.expect("xAccess");
        unsafe {
            x_access(
                registered,
                name.as_ptr(),
                ffi::SQLITE_ACCESS_EXISTS,
                &mut out,
            )
        };
        out
    }
}

impl Drop for ConfinedSqlite {
    fn drop(&mut self) {
        if self.close_impl().is_err() {
            // Safest documented leak: a failed close leaves SQLite in
            // possession of the connection and VFS.
            if let Some(connection) = self.connection.take() {
                std::mem::forget(connection);
            }
            if let Some(vfs) = self.vfs.take() {
                std::mem::forget(vfs);
            }
        }
    }
}

fn unsupported(message: &str) -> rusqlite::Error {
    rusqlite::Error::SqliteFailure(
        ffi::Error::new(ffi::SQLITE_CANTOPEN),
        Some(message.to_string()),
    )
}

fn default_vfs() -> rusqlite::Result<ffi::sqlite3_vfs> {
    // SAFETY: the default VFS is process-lived; a copy inherits its
    // non-file callbacks and pAppData.
    let original = unsafe { ffi::sqlite3_vfs_find(std::ptr::null()) };
    if original.is_null() {
        return Err(unsupported("no default VFS is registered"));
    }
    // SAFETY: `original` points at the live default VFS.
    Ok(unsafe { *original })
}

fn identity(file: &dyn ReadWriteFile) -> rusqlite::Result<FileIdentity> {
    match panic::catch_unwind(AssertUnwindSafe(|| file.identity())) {
        Ok(Ok(identity)) => Ok(identity),
        _ => Err(unsupported("cannot identify a retained file")),
    }
}

fn len(file: &dyn ReadWriteFile) -> rusqlite::Result<u64> {
    match panic::catch_unwind(AssertUnwindSafe(|| file.len())) {
        Ok(Ok(len)) => Ok(len),
        _ => Err(unsupported("cannot measure a retained file")),
    }
}

fn verify(
    main_db: &dyn ReadWriteFile,
    rollback_journal: &dyn ReadWriteFile,
    statement_journal: &dyn ReadWriteFile,
    wal: Option<&dyn ReadWriteFile>,
) -> rusqlite::Result<()> {
    let mut identities = vec![
        identity(main_db)?,
        identity(rollback_journal)?,
        identity(statement_journal)?,
    ];
    if let Some(wal) = wal {
        identities.push(identity(wal)?);
    }
    if identities
        .iter()
        .enumerate()
        .any(|(at, id)| identities[..at].contains(id))
    {
        return Err(unsupported(
            "retained database and journal files must be distinct",
        ));
    }
    if len(rollback_journal)? != 0 {
        return Err(unsupported("rollback journal is not empty"));
    }
    if len(statement_journal)? != 0 {
        return Err(unsupported("statement journal is not empty"));
    }
    if let Some(wal) = wal
        && len(wal)? != 0
    {
        return Err(unsupported("WAL is not empty"));
    }
    let main_len = len(main_db)?;
    if main_len == 0 {
        return if wal.is_none() {
            Ok(())
        } else {
            Err(unsupported("WAL2 main image is empty"))
        };
    }
    if main_len < 20 {
        return Err(unsupported("database header is truncated"));
    }
    let mut header = [0u8; 20];
    if io_read(main_db, &mut header, 0) != ffi::SQLITE_OK {
        return Err(unsupported("cannot read the database header"));
    }
    if wal.is_some() {
        return if header[18..20] == [2, 2] {
            Ok(())
        } else {
            Err(unsupported("database is not in WAL2 format"))
        };
    }
    if header[18] == 2 || header[19] == 2 {
        return Err(unsupported(
            "WAL-format database requires the separate WAL writer",
        ));
    }
    if header[18] != 1 || header[19] != 1 {
        return Err(unsupported("database is not in rollback-journal format"));
    }
    Ok(())
}

fn capability(context: &Context, role: Role) -> &dyn ReadWriteFile {
    match role {
        Role::Main => context.main_db.as_ref(),
        Role::RollbackJournal => context.rollback_journal.as_ref(),
        Role::StatementJournal => context.statement_journal.as_ref(),
        Role::Wal => context
            .wal
            .as_ref()
            .expect("WAL role has a retained file")
            .file
            .as_ref(),
    }
}

fn record_mutation(context: &Context) {
    if context
        .mutations
        .try_update(Ordering::SeqCst, Ordering::SeqCst, |generation| {
            generation.checked_add(1)
        })
        .is_err()
    {
        context.io_error.store(true, Ordering::SeqCst);
    }
}

fn latch(context: *const Context) {
    // SAFETY: the VFS owns the context and outlives every open file.
    unsafe { (*context).io_error.store(true, Ordering::SeqCst) };
}

unsafe extern "C" fn vfs_open(
    vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    out: *mut ffi::sqlite3_file,
    flags: c_int,
    out_flags: *mut c_int,
) -> c_int {
    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        if out.is_null() {
            return ffi::SQLITE_CANTOPEN;
        }
        unsafe { (*out).pMethods = std::ptr::null_mut() };
        // This writer never grants a writable handle for a read-only
        // request. Hot journals are excluded by the empty-journal entry
        // contract, so no read-only recovery handle is needed here.
        if flags & (ffi::SQLITE_OPEN_READONLY | ffi::SQLITE_OPEN_READWRITE)
            != ffi::SQLITE_OPEN_READWRITE
        {
            return ffi::SQLITE_CANTOPEN;
        }
        // SAFETY: the registered base is the first field of `Vfs`.
        let vfs = unsafe { &*vfs.cast::<Vfs>() };
        let context = &vfs.context;
        let role = match flags & TYPE_MASK {
            ffi::SQLITE_OPEN_MAIN_DB => {
                if name.is_null() {
                    return ffi::SQLITE_CANTOPEN;
                }
                // SAFETY: SQLite supplies a NUL-terminated name.
                let name = unsafe { CStr::from_ptr(name) };
                if name.to_bytes() != context.main_name.as_bytes() {
                    return ffi::SQLITE_CANTOPEN;
                }
                if context
                    .claimed
                    .compare_exchange(false, true, Ordering::AcqRel, Ordering::Acquire)
                    .is_err()
                {
                    return ffi::SQLITE_CANTOPEN;
                }
                Role::Main
            }
            ffi::SQLITE_OPEN_MAIN_JOURNAL => {
                if name.is_null() {
                    return ffi::SQLITE_CANTOPEN;
                }
                // SAFETY: SQLite supplies a NUL-terminated name.
                let name = unsafe { CStr::from_ptr(name) };
                if name.to_bytes() != context.journal_name.as_bytes() {
                    return ffi::SQLITE_CANTOPEN;
                }
                Role::RollbackJournal
            }
            ffi::SQLITE_OPEN_WAL => {
                let Some(wal) = &context.wal else {
                    return ffi::SQLITE_CANTOPEN;
                };
                if name.is_null()
                    || unsafe { CStr::from_ptr(name) }.to_bytes() != wal.name.as_bytes()
                {
                    return ffi::SQLITE_CANTOPEN;
                }
                Role::Wal
            }
            ffi::SQLITE_OPEN_SUBJOURNAL => {
                if !name.is_null() {
                    return ffi::SQLITE_CANTOPEN;
                }
                Role::StatementJournal
            }
            _ => return ffi::SQLITE_CANTOPEN,
        };
        // SAFETY: SQLite allocated `szOsFile` bytes and calls xClose once.
        let file = out.cast::<StageFile>();
        unsafe {
            (*file).context = Arc::as_ptr(context);
            (*file).role = role;
            (*file).delete_on_close =
                role == Role::StatementJournal && flags & ffi::SQLITE_OPEN_DELETEONCLOSE != 0;
            (*file).base.pMethods = &METHODS;
        }
        if !out_flags.is_null() {
            unsafe {
                *out_flags = flags
                    & (ffi::SQLITE_OPEN_READONLY
                        | ffi::SQLITE_OPEN_READWRITE
                        | ffi::SQLITE_OPEN_CREATE
                        | ffi::SQLITE_OPEN_DELETEONCLOSE);
            }
        }
        if role == Role::RollbackJournal && flags & ffi::SQLITE_OPEN_READWRITE != 0 {
            context.journal_exists.store(true, Ordering::Release);
        }
        if role == Role::Wal {
            context
                .wal
                .as_ref()
                .expect("WAL role")
                .exists
                .store(true, Ordering::Release);
        }
        #[cfg(test)]
        context.opens.fetch_add(1, Ordering::SeqCst);
        ffi::SQLITE_OK
    }));
    result.unwrap_or(ffi::SQLITE_CANTOPEN)
}

unsafe extern "C" fn vfs_delete(
    vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    _sync_dir: c_int,
) -> c_int {
    // SAFETY: the registered base is the first field of `Vfs`.
    let context = unsafe { Arc::as_ptr(&(*vfs.cast::<Vfs>()).context) };
    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        if name.is_null() {
            return ffi::SQLITE_IOERR_DELETE;
        }
        // SAFETY: SQLite supplies a NUL-terminated name.
        let name = unsafe { CStr::from_ptr(name) };
        // SAFETY: the VFS owns the context and outlives every open file.
        let context = unsafe { &*context };
        let (file, exists) = if name.to_bytes() == context.journal_name.as_bytes() {
            (context.rollback_journal.as_ref(), &context.journal_exists)
        } else if let Some(wal) = context
            .wal
            .as_ref()
            .filter(|wal| name.to_bytes() == wal.name.as_bytes())
        {
            (wal.file.as_ref(), &wal.exists)
        } else {
            return ffi::SQLITE_IOERR_DELETE;
        };
        record_mutation(context);
        // Logical delete: never unlink or reopen a host pathname.
        let rc = io_truncate(file, 0);
        if rc != ffi::SQLITE_OK {
            context.io_error.store(true, Ordering::SeqCst);
            return rc;
        }
        let rc = io_sync(file);
        if rc != ffi::SQLITE_OK {
            context.io_error.store(true, Ordering::SeqCst);
            return rc;
        }
        exists.store(false, Ordering::Release);
        ffi::SQLITE_OK
    }));
    match result {
        Ok(rc) => rc,
        Err(_) => {
            latch(context);
            ffi::SQLITE_IOERR_DELETE
        }
    }
}

unsafe extern "C" fn vfs_access(
    vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    _flags: c_int,
    out: *mut c_int,
) -> c_int {
    // SAFETY: the registered base is the first field of `Vfs`.
    let context = unsafe { Arc::as_ptr(&(*vfs.cast::<Vfs>()).context) };
    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        if out.is_null() {
            return ffi::SQLITE_CANTOPEN;
        }
        let exists = if name.is_null() {
            false
        } else {
            // SAFETY: SQLite supplies a NUL-terminated name.
            let name = unsafe { CStr::from_ptr(name) };
            // SAFETY: the VFS owns the context and outlives every open file.
            let context = unsafe { &*context };
            if name.to_bytes() == context.main_name.as_bytes() {
                true
            } else if name.to_bytes() == context.journal_name.as_bytes() {
                context.journal_exists.load(Ordering::Acquire)
            } else if let Some(wal) = context
                .wal
                .as_ref()
                .filter(|wal| name.to_bytes() == wal.name.as_bytes())
            {
                wal.exists.load(Ordering::Acquire)
            } else {
                false
            }
        };
        unsafe { *out = exists as c_int };
        ffi::SQLITE_OK
    }));
    match result {
        Ok(rc) => rc,
        Err(_) => {
            latch(context);
            ffi::SQLITE_CANTOPEN
        }
    }
}

unsafe extern "C" fn full_path(
    _vfs: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    len: c_int,
    out: *mut c_char,
) -> c_int {
    let result = panic::catch_unwind(AssertUnwindSafe(|| {
        if name.is_null() || out.is_null() || len < 0 {
            return ffi::SQLITE_CANTOPEN;
        }
        // SAFETY: SQLite supplies a NUL-terminated name.
        let bytes = unsafe { CStr::from_ptr(name) }.to_bytes_with_nul();
        if bytes.len() > len as usize {
            return ffi::SQLITE_CANTOPEN;
        }
        // SAFETY: `out` has at least `len` bytes and `bytes` fits.
        unsafe { std::ptr::copy_nonoverlapping(bytes.as_ptr(), out.cast::<u8>(), bytes.len()) };
        ffi::SQLITE_OK
    }));
    result.unwrap_or(ffi::SQLITE_CANTOPEN)
}

/// The shared C callback fence. SQLite supplies a live initialized
/// StageFile; the wrapper owns its context until every file is closed.
/// Normal EOF is allowed only for xRead. All other faults are sticky,
/// including a contained panic, so later close cannot certify output.
unsafe fn file_call(
    file: *mut ffi::sqlite3_file,
    panic_code: c_int,
    action: impl FnOnce(&StageFile, &Context) -> c_int,
) -> c_int {
    let file = unsafe { &*file.cast::<StageFile>() };
    let context = unsafe { &*file.context };
    if matches!(
        panic_code,
        ffi::SQLITE_IOERR_WRITE | ffi::SQLITE_IOERR_TRUNCATE
    ) {
        record_mutation(context);
    }
    let rc = panic::catch_unwind(AssertUnwindSafe(|| action(file, context))).unwrap_or(panic_code);
    if rc != ffi::SQLITE_OK
        && !(panic_code == ffi::SQLITE_IOERR_READ && rc == ffi::SQLITE_IOERR_SHORT_READ)
    {
        context.io_error.store(true, Ordering::SeqCst);
    }
    rc
}

unsafe extern "C" fn read(
    file: *mut ffi::sqlite3_file,
    out: *mut c_void,
    amount: c_int,
    offset: i64,
) -> c_int {
    unsafe {
        file_call(file, ffi::SQLITE_IOERR_READ, |file, context| {
            if amount < 0 || offset < 0 || (amount > 0 && out.is_null()) {
                return ffi::SQLITE_IOERR_READ;
            }
            if amount == 0 {
                return ffi::SQLITE_OK;
            }
            // SQLite supplies a buffer of `amount` bytes.
            let bytes = std::slice::from_raw_parts_mut(out.cast::<u8>(), amount as usize);
            io_read(capability(context, file.role), bytes, offset)
        })
    }
}

unsafe extern "C" fn write(
    file: *mut ffi::sqlite3_file,
    out: *const c_void,
    amount: c_int,
    offset: i64,
) -> c_int {
    unsafe {
        file_call(file, ffi::SQLITE_IOERR_WRITE, |file, context| {
            if amount < 0 || offset < 0 || (amount > 0 && out.is_null()) {
                return ffi::SQLITE_IOERR_WRITE;
            }
            if amount == 0 {
                return ffi::SQLITE_OK;
            }
            // SQLite supplies a buffer of `amount` bytes.
            let bytes = std::slice::from_raw_parts(out.cast::<u8>(), amount as usize);
            io_write(capability(context, file.role), bytes, offset)
        })
    }
}

unsafe extern "C" fn truncate(file: *mut ffi::sqlite3_file, size: i64) -> c_int {
    unsafe {
        file_call(file, ffi::SQLITE_IOERR_TRUNCATE, |file, context| {
            io_truncate(capability(context, file.role), size)
        })
    }
}

unsafe extern "C" fn sync(file: *mut ffi::sqlite3_file, _flags: c_int) -> c_int {
    unsafe {
        file_call(file, ffi::SQLITE_IOERR_FSYNC, |file, context| {
            io_sync(capability(context, file.role))
        })
    }
}

unsafe extern "C" fn size(file: *mut ffi::sqlite3_file, out: *mut i64) -> c_int {
    unsafe {
        file_call(file, ffi::SQLITE_IOERR_FSTAT, |file, context| {
            if out.is_null() {
                return ffi::SQLITE_IOERR_FSTAT;
            }
            match file_size(capability(context, file.role)) {
                Ok(size) => {
                    *out = size;
                    ffi::SQLITE_OK
                }
                Err(rc) => rc,
            }
        })
    }
}

unsafe extern "C" fn close(file: *mut ffi::sqlite3_file) -> c_int {
    unsafe {
        file_call(file, ffi::SQLITE_IOERR_CLOSE, |file, context| {
            #[cfg(test)]
            context.closes.fetch_add(1, Ordering::SeqCst);
            if !file.delete_on_close {
                return ffi::SQLITE_OK;
            }
            // Logical delete of the retained statement journal.
            let capability = capability(context, file.role);
            let rc = io_truncate(capability, 0);
            if rc == ffi::SQLITE_OK {
                io_sync(capability)
            } else {
                rc
            }
        })
    }
}

unsafe extern "C" fn lock(_file: *mut ffi::sqlite3_file, _state: c_int) -> c_int {
    // One enforced main connection: locking is a no-op.
    ffi::SQLITE_OK
}

unsafe extern "C" fn reserved(_file: *mut ffi::sqlite3_file, out: *mut c_int) -> c_int {
    if !out.is_null() {
        unsafe { *out = 0 };
    }
    ffi::SQLITE_OK
}

unsafe extern "C" fn control(
    _file: *mut ffi::sqlite3_file,
    _op: c_int,
    _arg: *mut c_void,
) -> c_int {
    ffi::SQLITE_NOTFOUND
}

unsafe extern "C" fn sector(_file: *mut ffi::sqlite3_file) -> c_int {
    SECTOR
}

unsafe extern "C" fn characteristics(file: *mut ffi::sqlite3_file) -> c_int {
    // SAFETY: xOpen initialized this pointer and SQLite calls xClose once.
    let role = unsafe { (*file.cast::<StageFile>()).role };
    match role {
        // The retained journal is never unlinked; the pinned pager
        // asserts this bit while a journal handle is open.
        Role::RollbackJournal => ffi::SQLITE_IOCAP_UNDELETABLE_WHEN_OPEN,
        Role::Main | Role::StatementJournal | Role::Wal => 0,
    }
}

unsafe extern "C" fn dl_open(_: *mut ffi::sqlite3_vfs, _: *const c_char) -> *mut c_void {
    std::ptr::null_mut()
}

unsafe extern "C" fn dl_error(_: *mut ffi::sqlite3_vfs, len: c_int, out: *mut c_char) {
    if len > 0 && !out.is_null() {
        unsafe { *out = 0 };
    }
}

unsafe extern "C" fn dl_sym(
    _: *mut ffi::sqlite3_vfs,
    _: *mut c_void,
    _: *const c_char,
) -> Option<unsafe extern "C" fn(*mut ffi::sqlite3_vfs, *mut c_void, *const c_char)> {
    None
}

unsafe extern "C" fn dl_close(_: *mut ffi::sqlite3_vfs, _: *mut c_void) {}

#[cfg(test)]
mod tests;
