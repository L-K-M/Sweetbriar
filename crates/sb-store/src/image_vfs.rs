//! Read-only SQLite VFS backed by an already-open file, never a pathname.
//!
//! SQLite owns each `sqlite3_file` until xClose. Its embedded pointer owns an
//! Arc to the retained reader; a thread-local slot transfers that Arc to
//! xOpen. The VFS stays registered for the life of the process.
use rusqlite::{Connection, OpenFlags, ffi};
use sb_types::fs::ReadFile;
use std::cell::RefCell;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::io::{Read, Seek, SeekFrom};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, OnceLock};

type Reader = Arc<Mutex<Box<dyn ReadFile>>>;

/// Process-global SQLite allocation diagnostics, for isolated child tests only.
#[cfg(test)]
pub(crate) fn largest_allocation(reset: bool) -> rusqlite::Result<i64> {
    let (mut current, mut largest) = (0, 0);
    // Both pointers refer to live i64 locals; SQLite synchronizes its counters.
    let result = unsafe {
        ffi::sqlite3_status64(
            ffi::SQLITE_STATUS_MALLOC_SIZE,
            &mut current,
            &mut largest,
            i32::from(reset),
        )
    };
    if result != ffi::SQLITE_OK {
        return Err(rusqlite::Error::SqliteFailure(
            ffi::Error::new(result),
            None,
        ));
    }
    Ok(largest)
}

thread_local! {
    static OPENING: RefCell<Option<(String, Reader)>> = const { RefCell::new(None) };
}
#[cfg(test)]
thread_local! {
    static CLOSES: std::cell::Cell<usize> = const { std::cell::Cell::new(0) };
}
#[cfg(test)]
pub(crate) fn closes_on_this_thread() -> usize {
    CLOSES.with(std::cell::Cell::get)
}
static NEXT_ID: AtomicU64 = AtomicU64::new(0);
static REGISTERED: OnceLock<c_int> = OnceLock::new();
const NAME: &CStr = c"sweetbriar-image";

#[repr(C)]
struct ImageFile {
    base: ffi::sqlite3_file,
    reader: *mut Reader,
}

// The table and callbacks have static lifetime as required by SQLite.
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

/// The expression and CTE-nesting depth an untrusted image is read under:
/// SQLite's own default, which the store's build raises for its generated
/// search SQL (spec/03 `LIBSQLITE3_FLAGS`).
pub(crate) const UNTRUSTED_EXPR_DEPTH: i32 = 1_000;

/// Open only the supplied handle. SQLite cannot open any other file through
/// this VFS, including a WAL, journal, temporary file or source pathname.
/// The image's schema is parsed at `UNTRUSTED_EXPR_DEPTH` from the first
/// statement on.
pub(crate) fn open(file: Box<dyn ReadFile>) -> rusqlite::Result<Connection> {
    let rc = *REGISTERED.get_or_init(register);
    if rc != ffi::SQLITE_OK {
        return Err(rusqlite::Error::SqliteFailure(ffi::Error::new(rc), None));
    }
    let id = NEXT_ID
        .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
        .map_err(|_| rusqlite::Error::InvalidQuery)?;
    let name = format!("sb-image-{id}");
    OPENING.with(|opening| {
        let mut slot = opening
            .try_borrow_mut()
            .map_err(|_| rusqlite::Error::InvalidQuery)?;
        if slot.is_some() {
            return Err(rusqlite::Error::InvalidQuery);
        }
        *slot = Some((name.clone(), Arc::new(Mutex::new(file))));
        drop(slot);
        let result = Connection::open_with_flags_and_vfs(
            format!("file:{name}?mode=ro&immutable=1"),
            OpenFlags::SQLITE_OPEN_READ_ONLY | OpenFlags::SQLITE_OPEN_URI,
            NAME,
        );
        // xOpen owns its own Arc; this slot only transfers it during open.
        *opening.borrow_mut() = None;
        let connection = result?;
        connection.set_limit(
            rusqlite::limits::Limit::SQLITE_LIMIT_EXPR_DEPTH,
            UNTRUSTED_EXPR_DEPTH,
        )?;
        Ok(connection)
    })
}

fn register() -> c_int {
    // SAFETY: SQLite's default VFS is process-lived. A copied VFS inherits its
    // non-file callbacks and pAppData; our replacements never use pathname IO.
    unsafe {
        let original = ffi::sqlite3_vfs_find(std::ptr::null());
        if original.is_null() {
            return ffi::SQLITE_ERROR;
        }
        let mut vfs = Box::new(*original);
        vfs.pNext = std::ptr::null_mut();
        vfs.zName = NAME.as_ptr();
        vfs.szOsFile = std::mem::size_of::<ImageFile>() as c_int;
        vfs.xOpen = Some(vfs_open);
        vfs.xDelete = Some(vfs_delete);
        vfs.xAccess = Some(vfs_access);
        vfs.xFullPathname = Some(full_path);
        vfs.xDlOpen = Some(dl_open);
        vfs.xDlError = Some(dl_error);
        vfs.xDlSym = Some(dl_sym);
        vfs.xDlClose = Some(dl_close);
        let raw = Box::into_raw(vfs);
        let rc = ffi::sqlite3_vfs_register(raw, 0);
        if rc != ffi::SQLITE_OK {
            drop(Box::from_raw(raw));
        }
        rc
    }
}

// Callbacks invoking a reader catch panics before they cross SQLite's C ABI.
unsafe extern "C" fn vfs_open(
    _: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    out: *mut ffi::sqlite3_file,
    flags: c_int,
    out_flags: *mut c_int,
) -> c_int {
    std::panic::catch_unwind(|| {
        if name.is_null()
            || flags & ffi::SQLITE_OPEN_MAIN_DB == 0
            || flags & ffi::SQLITE_OPEN_READWRITE != 0
        {
            return ffi::SQLITE_CANTOPEN;
        }
        // SAFETY: SQLite supplies the NUL-terminated name and allocated file.
        let Ok(name) = (unsafe { CStr::from_ptr(name) }).to_str() else {
            return ffi::SQLITE_CANTOPEN;
        };
        OPENING.with(|opening| {
            let Ok(slot) = opening.try_borrow() else {
                return ffi::SQLITE_CANTOPEN;
            };
            let Some((expected, reader)) = slot.as_ref() else {
                return ffi::SQLITE_CANTOPEN;
            };
            if name != expected {
                return ffi::SQLITE_CANTOPEN;
            }
            let file = out.cast::<ImageFile>();
            // SAFETY: SQLite allocated szOsFile bytes, and calls xClose once on success.
            unsafe {
                (*file).reader = Box::into_raw(Box::new(Arc::clone(reader)));
                (*file).base.pMethods = &METHODS;
                if !out_flags.is_null() {
                    *out_flags = ffi::SQLITE_OPEN_READONLY;
                }
            }
            ffi::SQLITE_OK
        })
    })
    .unwrap_or(ffi::SQLITE_CANTOPEN)
}
unsafe extern "C" fn close(file: *mut ffi::sqlite3_file) -> c_int {
    // SAFETY: xOpen initialized this pointer and SQLite calls xClose once.
    std::panic::catch_unwind(|| unsafe {
        drop(Box::from_raw((*file.cast::<ImageFile>()).reader));
        #[cfg(test)]
        CLOSES.with(|count| count.set(count.get() + 1));
    })
    .map_or(ffi::SQLITE_IOERR_CLOSE, |()| ffi::SQLITE_OK)
}
unsafe extern "C" fn read(
    file: *mut ffi::sqlite3_file,
    out: *mut c_void,
    amount: c_int,
    offset: i64,
) -> c_int {
    std::panic::catch_unwind(|| {
        if amount < 0 || offset < 0 || (amount > 0 && out.is_null()) {
            return ffi::SQLITE_IOERR_READ;
        }
        if amount == 0 {
            return ffi::SQLITE_OK;
        }
        // SAFETY: SQLite supplies a live file and buffer of `amount` bytes.
        let reader = unsafe { &*(*file.cast::<ImageFile>()).reader };
        let Ok(mut reader) = reader.lock() else {
            return ffi::SQLITE_IOERR_READ;
        };
        let bytes = unsafe { std::slice::from_raw_parts_mut(out.cast::<u8>(), amount as usize) };
        if reader.seek(SeekFrom::Start(offset as u64)).is_err() {
            return ffi::SQLITE_IOERR_READ;
        }
        let mut done = 0;
        while done < bytes.len() {
            match reader.read(&mut bytes[done..]) {
                Ok(0) => {
                    bytes[done..].fill(0);
                    return ffi::SQLITE_IOERR_SHORT_READ;
                }
                Ok(n) => done += n,
                Err(e) if e.kind() == std::io::ErrorKind::Interrupted => continue,
                Err(_) => return ffi::SQLITE_IOERR_READ,
            }
        }
        ffi::SQLITE_OK
    })
    .unwrap_or(ffi::SQLITE_IOERR_READ)
}
unsafe extern "C" fn size(file: *mut ffi::sqlite3_file, out: *mut i64) -> c_int {
    std::panic::catch_unwind(|| {
        // SAFETY: SQLite supplies a live file and output pointer.
        let reader = unsafe { &*(*file.cast::<ImageFile>()).reader };
        let Ok(mut reader) = reader.lock() else {
            return ffi::SQLITE_IOERR_FSTAT;
        };
        let Ok(n) = reader.seek(SeekFrom::End(0)) else {
            return ffi::SQLITE_IOERR_FSTAT;
        };
        let Ok(n) = i64::try_from(n) else {
            return ffi::SQLITE_IOERR_FSTAT;
        };
        unsafe {
            *out = n;
        }
        ffi::SQLITE_OK
    })
    .unwrap_or(ffi::SQLITE_IOERR_FSTAT)
}
unsafe extern "C" fn write(_: *mut ffi::sqlite3_file, _: *const c_void, _: c_int, _: i64) -> c_int {
    ffi::SQLITE_READONLY
}
unsafe extern "C" fn truncate(_: *mut ffi::sqlite3_file, _: i64) -> c_int {
    ffi::SQLITE_READONLY
}
unsafe extern "C" fn sync(_: *mut ffi::sqlite3_file, _: c_int) -> c_int {
    ffi::SQLITE_OK
}
unsafe extern "C" fn lock(_: *mut ffi::sqlite3_file, _: c_int) -> c_int {
    ffi::SQLITE_OK
}
unsafe extern "C" fn reserved(_: *mut ffi::sqlite3_file, out: *mut c_int) -> c_int {
    unsafe {
        *out = 0;
    }
    ffi::SQLITE_OK
}
unsafe extern "C" fn control(_: *mut ffi::sqlite3_file, _: c_int, _: *mut c_void) -> c_int {
    ffi::SQLITE_NOTFOUND
}
unsafe extern "C" fn sector(_: *mut ffi::sqlite3_file) -> c_int {
    4096
}
unsafe extern "C" fn characteristics(_: *mut ffi::sqlite3_file) -> c_int {
    ffi::SQLITE_IOCAP_IMMUTABLE
}
unsafe extern "C" fn vfs_delete(_: *mut ffi::sqlite3_vfs, _: *const c_char, _: c_int) -> c_int {
    ffi::SQLITE_READONLY
}
unsafe extern "C" fn vfs_access(
    _: *mut ffi::sqlite3_vfs,
    _: *const c_char,
    _: c_int,
    out: *mut c_int,
) -> c_int {
    unsafe {
        *out = 0;
    }
    ffi::SQLITE_OK
}
unsafe extern "C" fn dl_open(_: *mut ffi::sqlite3_vfs, _: *const c_char) -> *mut c_void {
    std::ptr::null_mut()
}
unsafe extern "C" fn dl_error(_: *mut ffi::sqlite3_vfs, len: c_int, out: *mut c_char) {
    if len > 0 {
        unsafe {
            *out = 0;
        }
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
unsafe extern "C" fn full_path(
    _: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    len: c_int,
    out: *mut c_char,
) -> c_int {
    // SAFETY: SQLite supplies a NUL-terminated name and writable output buffer.
    let bytes = unsafe { CStr::from_ptr(name) }.to_bytes_with_nul();
    if len < 0 || bytes.len() > len as usize {
        return ffi::SQLITE_CANTOPEN;
    }
    unsafe {
        std::ptr::copy_nonoverlapping(bytes.as_ptr(), out.cast::<u8>(), bytes.len());
    }
    ffi::SQLITE_OK
}

#[cfg(test)]
mod tests {
    use super::*;
    use sb_test_support::ScratchDir;

    #[test]
    fn an_untrusted_image_opens_at_sqlites_default_depth() {
        let scratch = ScratchDir::new("sqlite-image-depth");
        let path = scratch.path().join("state.sqlite");
        let store = Connection::open(&path).unwrap();
        store
            .execute_batch("CREATE TABLE proof (value TEXT);")
            .unwrap();
        let limit = rusqlite::limits::Limit::SQLITE_LIMIT_EXPR_DEPTH;
        assert_eq!(
            store.limit(limit).unwrap(),
            2000,
            "the store's raised depth"
        );
        store.close().unwrap();

        let image = open(Box::new(std::fs::File::open(&path).unwrap())).unwrap();

        assert_eq!(image.limit(limit).unwrap(), UNTRUSTED_EXPR_DEPTH);
    }

    #[test]
    fn reads_retained_file_after_replacement_without_sidecars() {
        let scratch = ScratchDir::new("sqlite-retained-file");
        let path = scratch.path().join("state.sqlite");
        let conn = Connection::open(&path).unwrap();
        conn.execute_batch(
            "CREATE TABLE proof (value TEXT); INSERT INTO proof VALUES ('retained');",
        )
        .unwrap();
        conn.close().unwrap();
        let retained = std::fs::File::open(&path).unwrap();
        std::fs::rename(&path, scratch.path().join("moved.sqlite")).unwrap();
        let replacement = Connection::open(&path).unwrap();
        replacement
            .execute_batch(
                "CREATE TABLE proof (value TEXT); INSERT INTO proof VALUES ('replacement');",
            )
            .unwrap();
        replacement.close().unwrap();
        std::fs::write(scratch.path().join("state.sqlite-wal"), b"untrusted WAL").unwrap();
        let conn = open(Box::new(retained)).unwrap();
        // SAFETY: This enables loading only to check that this VFS refuses it.
        unsafe {
            assert_eq!(
                ffi::sqlite3_enable_load_extension(conn.handle(), 1),
                ffi::SQLITE_OK
            );
        }
        assert!(conn.execute("SELECT load_extension('other')", []).is_err());
        // SAFETY: the connection remains open and owns the SQLite handle.
        unsafe {
            assert_eq!(
                ffi::sqlite3_enable_load_extension(conn.handle(), 0),
                ffi::SQLITE_OK
            );
        }
        let value: String = conn
            .query_row("SELECT value FROM proof", [], |r| r.get(0))
            .unwrap();
        assert_eq!(value, "retained");
        assert!(
            conn.execute("INSERT INTO proof VALUES ('bad')", [])
                .is_err()
        );
        assert!(
            conn.execute("ATTACH DATABASE 'other.sqlite' AS other", [])
                .is_err()
        );
        assert_eq!(
            std::fs::read(scratch.path().join("state.sqlite-wal")).unwrap(),
            b"untrusted WAL"
        );
        assert!(!scratch.path().join("state.sqlite-shm").exists());
    }

    #[test]
    fn ignores_a_live_wal() {
        let scratch = ScratchDir::new("sqlite-retained-wal");
        let path = scratch.path().join("state.sqlite");
        let writer = Connection::open(&path).unwrap();
        writer.execute_batch("PRAGMA journal_mode=WAL; CREATE TABLE proof (value TEXT); INSERT INTO proof VALUES ('base'); PRAGMA wal_checkpoint(TRUNCATE); INSERT INTO proof VALUES ('wal');").unwrap();
        assert!(scratch.path().join("state.sqlite-wal").exists());
        let reader = open(Box::new(std::fs::File::open(&path).unwrap())).unwrap();
        let rows: i64 = reader
            .query_row("SELECT count(*) FROM proof", [], |r| r.get(0))
            .unwrap();
        assert_eq!(rows, 1);
        let live: i64 = writer
            .query_row("SELECT count(*) FROM proof", [], |r| r.get(0))
            .unwrap();
        assert_eq!(live, 2);
    }

    #[test]
    fn reads_from_independent_handles_concurrently() {
        let scratch = ScratchDir::new("sqlite-retained-concurrent");
        let path = scratch.path().join("state.sqlite");
        let writer = Connection::open(&path).unwrap();
        writer
            .execute_batch("CREATE TABLE proof (value INTEGER); INSERT INTO proof VALUES (42);")
            .unwrap();
        writer.close().unwrap();
        let workers: Vec<_> = (0..8)
            .map(|_| {
                let file = std::fs::File::open(&path).unwrap();
                std::thread::spawn(move || {
                    let conn = open(Box::new(file)).unwrap();
                    conn.query_row("SELECT value FROM proof", [], |r| r.get::<_, i64>(0))
                        .unwrap()
                })
            })
            .collect();
        for worker in workers {
            assert_eq!(worker.join().unwrap(), 42);
        }
    }

    #[test]
    fn reader_errors_are_reported() {
        struct Broken;
        impl Read for Broken {
            fn read(&mut self, _: &mut [u8]) -> std::io::Result<usize> {
                Err(std::io::ErrorKind::Other.into())
            }
        }
        impl Seek for Broken {
            fn seek(&mut self, _: SeekFrom) -> std::io::Result<u64> {
                Err(std::io::ErrorKind::Other.into())
            }
        }
        assert!(open(Box::new(Broken)).is_err());
    }
}
