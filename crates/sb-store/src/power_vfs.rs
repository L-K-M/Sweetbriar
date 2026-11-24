//! The test SQLite VFS that simulates power loss (spec/11 "Power loss").
//!
//! It wraps the default VFS. For a file under an armed root, writes and
//! truncations stay in memory until that file's next `xSync` applies them
//! to the real file, which stands for what the disk holds durably. The
//! pending operations are kept per path, so every handle on a file sees
//! them as it would see the OS's page cache. A cut fails every later
//! operation on the root's files, as a dead machine would; restoring the
//! power drops whatever no sync applied. Files elsewhere pass straight
//! through to the default VFS.
//!
//! The model is a disk that keeps nothing it wasn't asked to sync:
//! - SQLite maps no file through it (io methods version 2), so every
//!   read sees the pending writes;
//! - the device promises only power-safe overwrite, so SQLite never
//!   skips a sync on the strength of atomic or ordered writes;
//! - creating and deleting a file count as durable at once, and a
//!   write is never torn.
//!
//! Shared memory passes through: after a cut every connection closes,
//! so the next opener truncates the WAL index and rebuilds it from the
//! WAL that survived.
//!
//! Its kill mode cuts a crash-cut child inside SQLite's own I/O
//! instead: before the chosen operation it applies every pending write
//! and truncation in order, as the OS keeps a killed process's writes,
//! and the process kills itself.
//!
//! The store's connection constructor opens through this VFS only for
//! paths under an armed root, and only in test builds.

use rusqlite::ffi;
use std::collections::HashMap;
use std::ffi::{CStr, c_char, c_int, c_void};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard, OnceLock, PoisonError};

/// The VFS's registered name.
pub(crate) const NAME: &CStr = c"sweetbriar-power";

/// One pending change to a file, applied in order at its next sync.
enum Op {
    Write { offset: u64, data: Vec<u8> },
    Truncate(u64),
}

/// A simulated machine: one armed root's files and its power.
struct Machine {
    /// The root as armed and as the file system resolves it: SQLite
    /// hands xOpen a full pathname, which may have either form.
    roots: Vec<PathBuf>,
    state: Mutex<MachineState>,
}

#[derive(Default)]
struct MachineState {
    off: bool,
    /// Pending operations by full pathname.
    pending: HashMap<String, Vec<Op>>,
    /// Writes, truncations and syncs left before the power goes; none
    /// counts down when unset.
    operations_left: Option<u64>,
    /// Writes, truncations and syncs seen while the power was on.
    operations: u64,
    /// The operation the process dies before, in kill mode.
    kill_before: Option<u64>,
}

impl Machine {
    fn state(&self) -> MutexGuard<'_, MachineState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    fn owns(&self, path: &Path) -> bool {
        self.roots.iter().any(|root| path.starts_with(root))
    }
}

impl MachineState {
    /// Count one write, truncation or sync: false when the power is off
    /// or goes with this one, which then never happens.
    fn admit(&mut self) -> bool {
        if self.off {
            return false;
        }
        if self.kill_before == Some(self.operations + 1) {
            self.apply_pending();
            sb_test_support::kill_now();
        }
        if let Some(left) = self.operations_left.as_mut() {
            if *left == 0 {
                self.off = true;
                return false;
            }
            *left -= 1;
        }
        self.operations += 1;
        true
    }

    /// Write every pending operation to its real file, as the OS holds
    /// a killed process's writes. A failure aborts the process, which
    /// its crash-cut parent reports as an abnormal exit.
    fn apply_pending(&mut self) {
        use std::os::unix::fs::FileExt;
        for (path, ops) in self.pending.drain() {
            let file = std::fs::OpenOptions::new()
                .write(true)
                .open(&path)
                .unwrap_or_else(|e| panic!("open {path} to apply its writes: {e}"));
            for op in ops {
                match op {
                    Op::Write { offset, data } => file.write_all_at(&data, offset),
                    Op::Truncate(size) => file.set_len(size),
                }
                .unwrap_or_else(|e| panic!("apply a write to {path}: {e}"));
            }
        }
    }
}

static MACHINES: Mutex<Vec<Arc<Machine>>> = Mutex::new(Vec::new());
static REGISTERED: OnceLock<c_int> = OnceLock::new();

fn machines() -> MutexGuard<'static, Vec<Arc<Machine>>> {
    MACHINES.lock().unwrap_or_else(PoisonError::into_inner)
}

fn machine_for(path: &Path) -> Option<Arc<Machine>> {
    machines()
        .iter()
        .find(|machine| machine.owns(path))
        .cloned()
}

/// The VFS a store connection on `state_db` opens through: this one
/// under an armed root, else the default.
pub(crate) fn name_for(state_db: &Path) -> Option<&'static CStr> {
    let armed = machine_for(state_db).is_some()
        || std::path::absolute(state_db).is_ok_and(|path| machine_for(&path).is_some());
    armed.then_some(NAME)
}

/// The power to the files under one root, for a test. Dropping it
/// disarms the root; files opened before then keep their machine.
pub(crate) struct PowerSupply {
    machine: Arc<Machine>,
}

impl PowerSupply {
    /// Simulate the disk under `root`. Fails if the VFS can't register or
    /// another supply already owns an overlapping root.
    pub(crate) fn arm(root: &Path) -> rusqlite::Result<Self> {
        let rc = *REGISTERED.get_or_init(register);
        if rc != ffi::SQLITE_OK {
            return Err(rusqlite::Error::SqliteFailure(ffi::Error::new(rc), None));
        }
        let mut roots = vec![root.to_path_buf()];
        if let Ok(resolved) = std::fs::canonicalize(root)
            && resolved != root
        {
            roots.push(resolved);
        }
        let mut armed = machines();
        if armed.iter().any(|machine| {
            machine.roots.iter().any(|other| {
                roots
                    .iter()
                    .any(|root| root.starts_with(other) || other.starts_with(root))
            })
        }) {
            return Err(rusqlite::Error::InvalidPath(root.to_path_buf()));
        }
        let machine = Arc::new(Machine {
            roots,
            state: Mutex::new(MachineState::default()),
        });
        armed.push(machine.clone());
        Ok(Self { machine })
    }

    /// Cut the power once `operations` more writes, truncations or
    /// syncs have happened: the next one fails, and every one after it.
    pub(crate) fn cut_after(&self, operations: u64) {
        self.machine.state().operations_left = Some(operations);
    }

    /// Kill mode: the process dies of `SIGKILL` immediately before the
    /// `nth` write, truncation or sync (from 1), with every earlier one
    /// written to its file.
    pub(crate) fn kill_before(&self, nth: u64) {
        self.machine.state().kill_before = Some(nth);
    }

    /// Cut the power now.
    pub(crate) fn cut(&self) {
        self.machine.state().off = true;
    }

    /// Writes, truncations and syncs that happened with the power on.
    pub(crate) fn operations(&self) -> u64 {
        self.machine.state().operations
    }

    /// Writes and truncations no sync has applied yet.
    pub(crate) fn unsynced(&self) -> usize {
        self.machine.state().pending.values().map(Vec::len).sum()
    }

    /// Power back on over what the disk kept: everything no sync applied
    /// is gone. Close every connection on the root first, as the reboot
    /// would have.
    pub(crate) fn restore(&self) {
        let mut state = self.machine.state();
        state.pending.clear();
        state.off = false;
        state.operations_left = None;
    }
}

impl Drop for PowerSupply {
    fn drop(&mut self) {
        machines().retain(|machine| !Arc::ptr_eq(machine, &self.machine));
    }
}

/// The default VFS this one wraps. SQLite keeps it for the process's
/// life.
static ORIGINAL: OnceLock<usize> = OnceLock::new();

fn original() -> *mut ffi::sqlite3_vfs {
    ORIGINAL.get().copied().unwrap_or(0) as *mut ffi::sqlite3_vfs
}

fn register() -> c_int {
    // SAFETY: the default VFS is process-lived. The copy inherits its
    // non-file callbacks and pAppData, which those callbacks read; xOpen
    // and xDelete forward to the original with the original's pointer.
    unsafe {
        let base = ffi::sqlite3_vfs_find(std::ptr::null());
        if base.is_null() {
            return ffi::SQLITE_ERROR;
        }
        let _ = ORIGINAL.set(base as usize);
        let mut vfs = Box::new(*base);
        vfs.pNext = std::ptr::null_mut();
        vfs.zName = NAME.as_ptr();
        vfs.szOsFile = (std::mem::size_of::<PowerFile>() as c_int) + (*base).szOsFile;
        vfs.xOpen = Some(vfs_open);
        vfs.xDelete = Some(vfs_delete);
        let raw = Box::into_raw(vfs);
        let rc = ffi::sqlite3_vfs_register(raw, 0);
        if rc != ffi::SQLITE_OK {
            drop(Box::from_raw(raw));
        }
        rc
    }
}

/// This VFS's file: SQLite's header, the tracked path and its machine,
/// then the default VFS's own file in the bytes after it.
#[repr(C)]
struct PowerFile {
    base: ffi::sqlite3_file,
    /// The machine and full pathname, for a tracked file; null otherwise.
    tracked: *mut Tracked,
}

struct Tracked {
    machine: Arc<Machine>,
    path: String,
    /// SQLite's delete-on-close files leave no pending writes behind.
    delete_on_close: bool,
}

// `PowerFile` is followed by the real file, so its size keeps that
// file's alignment.
const _: () = assert!(std::mem::size_of::<PowerFile>().is_multiple_of(8));

/// The default VFS's file inside `file`.
unsafe fn real(file: *mut ffi::sqlite3_file) -> *mut ffi::sqlite3_file {
    // SAFETY: xOpen allocated szOsFile bytes: the header, then the real file.
    unsafe { (file as *mut u8).add(std::mem::size_of::<PowerFile>()) as *mut ffi::sqlite3_file }
}

unsafe fn tracked<'a>(file: *mut ffi::sqlite3_file) -> Option<&'a Tracked> {
    // SAFETY: `tracked` is null or a Box xOpen leaked, freed only by xClose.
    unsafe { (*(file as *mut PowerFile)).tracked.as_ref() }
}

/// The real file's io methods, which xOpen checked are present.
unsafe fn methods<'a>(file: *mut ffi::sqlite3_file) -> &'a ffi::sqlite3_io_methods {
    // SAFETY: an opened real file has non-null pMethods until xClose.
    unsafe { &*(*real(file)).pMethods }
}

static METHODS: ffi::sqlite3_io_methods = ffi::sqlite3_io_methods {
    // Version 2: shared memory, but no xFetch, so SQLite maps no file
    // past the pending writes.
    iVersion: 2,
    xClose: Some(close),
    xRead: Some(read),
    xWrite: Some(write),
    xTruncate: Some(truncate),
    xSync: Some(sync),
    xFileSize: Some(file_size),
    xLock: Some(lock),
    xUnlock: Some(unlock),
    xCheckReservedLock: Some(check_reserved),
    xFileControl: Some(file_control),
    xSectorSize: Some(sector_size),
    xDeviceCharacteristics: Some(device_characteristics),
    xShmMap: Some(shm_map),
    xShmLock: Some(shm_lock),
    xShmBarrier: Some(shm_barrier),
    xShmUnmap: Some(shm_unmap),
    xFetch: None,
    xUnfetch: None,
};

unsafe extern "C" fn vfs_open(
    _: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    file: *mut ffi::sqlite3_file,
    flags: c_int,
    out_flags: *mut c_int,
) -> c_int {
    // SAFETY: SQLite passes szOsFile writable bytes at `file` and a
    // NUL-terminated name or null; the original VFS is registered.
    unsafe {
        let header = file as *mut PowerFile;
        (*header).base.pMethods = std::ptr::null();
        (*header).tracked = std::ptr::null_mut();
        let path = (!name.is_null())
            .then(|| CStr::from_ptr(name).to_str().ok())
            .flatten();
        let machine = path.and_then(|path| machine_for(Path::new(path)));
        if machine.as_ref().is_some_and(|machine| machine.state().off) {
            return ffi::SQLITE_CANTOPEN;
        }
        let base = original();
        let Some(open) = (*base).xOpen else {
            return ffi::SQLITE_ERROR;
        };
        let rc = open(base, name, real(file), flags, out_flags);
        if rc != ffi::SQLITE_OK {
            return rc;
        }
        let real_methods = (*real(file)).pMethods;
        if real_methods.is_null() || (*real_methods).iVersion < 2 {
            if let Some(close) = real_methods.as_ref().and_then(|m| m.xClose) {
                close(real(file));
            }
            return ffi::SQLITE_CANTOPEN;
        }
        if let (Some(machine), Some(path)) = (machine, path) {
            (*header).tracked = Box::into_raw(Box::new(Tracked {
                machine,
                path: path.to_owned(),
                delete_on_close: flags & ffi::SQLITE_OPEN_DELETEONCLOSE != 0,
            }));
        }
        (*header).base.pMethods = &METHODS;
        ffi::SQLITE_OK
    }
}

unsafe extern "C" fn vfs_delete(
    _: *mut ffi::sqlite3_vfs,
    name: *const c_char,
    sync_dir: c_int,
) -> c_int {
    // SAFETY: as vfs_open; the name is NUL-terminated.
    unsafe {
        let path = (!name.is_null())
            .then(|| CStr::from_ptr(name).to_str().ok())
            .flatten();
        if let Some(path) = path
            && let Some(machine) = machine_for(Path::new(path))
        {
            let mut state = machine.state();
            if state.off {
                return ffi::SQLITE_IOERR_DELETE;
            }
            // A deleted file's pending writes go with it.
            state.pending.remove(path);
        }
        let base = original();
        match (*base).xDelete {
            Some(delete) => delete(base, name, sync_dir),
            None => ffi::SQLITE_IOERR_DELETE,
        }
    }
}

unsafe extern "C" fn close(file: *mut ffi::sqlite3_file) -> c_int {
    // SAFETY: SQLite calls xClose once, on a file xOpen opened.
    unsafe {
        let header = file as *mut PowerFile;
        if !(*header).tracked.is_null() {
            let tracked = Box::from_raw((*header).tracked);
            (*header).tracked = std::ptr::null_mut();
            if tracked.delete_on_close {
                tracked.machine.state().pending.remove(&tracked.path);
            }
        }
        match methods(file).xClose {
            Some(close) => close(real(file)),
            None => ffi::SQLITE_OK,
        }
    }
}

/// The real file's size, for replaying pending operations over it.
unsafe fn real_size(file: *mut ffi::sqlite3_file) -> Result<u64, c_int> {
    let mut size: ffi::sqlite3_int64 = 0;
    // SAFETY: an open real file; `size` is a live local.
    let rc = unsafe {
        match methods(file).xFileSize {
            Some(file_size) => file_size(real(file), &mut size),
            None => ffi::SQLITE_IOERR_FSTAT,
        }
    };
    if rc != ffi::SQLITE_OK {
        return Err(rc);
    }
    u64::try_from(size).map_err(|_| ffi::SQLITE_IOERR_FSTAT)
}

/// The file's size once `ops` apply over `size`.
fn replayed_size(mut size: u64, ops: &[Op]) -> u64 {
    for op in ops {
        size = match op {
            Op::Write { offset, data } => size.max(offset + data.len() as u64),
            Op::Truncate(to) => *to,
        };
    }
    size
}

/// Lay `ops` over `buf`, which holds the real file's bytes at `offset`
/// (zeros past its end), and answer the file's size after them.
fn replay(buf: &mut [u8], offset: u64, mut size: u64, ops: &[Op]) -> u64 {
    let end = offset + buf.len() as u64;
    for op in ops {
        match op {
            Op::Write { offset: at, data } => {
                let from = offset.max(*at);
                let to = end.min(at + data.len() as u64);
                if from < to {
                    buf[(from - offset) as usize..(to - offset) as usize]
                        .copy_from_slice(&data[(from - at) as usize..(to - at) as usize]);
                }
                size = size.max(at + data.len() as u64);
            }
            Op::Truncate(to) => {
                if *to < end {
                    buf[(to.saturating_sub(offset)) as usize..].fill(0);
                }
                size = *to;
            }
        }
    }
    size
}

unsafe extern "C" fn read(
    file: *mut ffi::sqlite3_file,
    buf: *mut c_void,
    amount: c_int,
    offset: ffi::sqlite3_int64,
) -> c_int {
    // SAFETY: SQLite passes `amount` writable bytes at `buf`.
    unsafe {
        let Some(tracked) = tracked(file) else {
            return match methods(file).xRead {
                Some(read) => read(real(file), buf, amount, offset),
                None => ffi::SQLITE_IOERR_READ,
            };
        };
        let state = tracked.machine.state();
        if state.off {
            return ffi::SQLITE_IOERR_READ;
        }
        let rc = match methods(file).xRead {
            Some(read) => read(real(file), buf, amount, offset),
            None => ffi::SQLITE_IOERR_READ,
        };
        if rc != ffi::SQLITE_OK && rc != ffi::SQLITE_IOERR_SHORT_READ {
            return rc;
        }
        let (Ok(offset), Ok(amount)) = (u64::try_from(offset), usize::try_from(amount)) else {
            return ffi::SQLITE_IOERR_READ;
        };
        let size = match real_size(file) {
            Ok(size) => size,
            Err(rc) => return rc,
        };
        let bytes = std::slice::from_raw_parts_mut(buf as *mut u8, amount);
        let ops = state
            .pending
            .get(&tracked.path)
            .map_or(&[][..], Vec::as_slice);
        let size = replay(bytes, offset, size, ops);
        let end = offset + amount as u64;
        if end <= size {
            return ffi::SQLITE_OK;
        }
        // SQLite needs the unread tail zeroed on a short read.
        bytes[(size.saturating_sub(offset)) as usize..].fill(0);
        ffi::SQLITE_IOERR_SHORT_READ
    }
}

unsafe extern "C" fn write(
    file: *mut ffi::sqlite3_file,
    buf: *const c_void,
    amount: c_int,
    offset: ffi::sqlite3_int64,
) -> c_int {
    // SAFETY: SQLite passes `amount` readable bytes at `buf`.
    unsafe {
        let Some(tracked) = tracked(file) else {
            return match methods(file).xWrite {
                Some(write) => write(real(file), buf, amount, offset),
                None => ffi::SQLITE_IOERR_WRITE,
            };
        };
        let (Ok(offset), Ok(amount)) = (u64::try_from(offset), usize::try_from(amount)) else {
            return ffi::SQLITE_IOERR_WRITE;
        };
        let mut state = tracked.machine.state();
        if !state.admit() {
            return ffi::SQLITE_IOERR_WRITE;
        }
        let data = std::slice::from_raw_parts(buf as *const u8, amount).to_vec();
        state
            .pending
            .entry(tracked.path.clone())
            .or_default()
            .push(Op::Write { offset, data });
        ffi::SQLITE_OK
    }
}

unsafe extern "C" fn truncate(file: *mut ffi::sqlite3_file, size: ffi::sqlite3_int64) -> c_int {
    // SAFETY: an open file.
    unsafe {
        let Some(tracked) = tracked(file) else {
            return match methods(file).xTruncate {
                Some(truncate) => truncate(real(file), size),
                None => ffi::SQLITE_IOERR_TRUNCATE,
            };
        };
        let Ok(size) = u64::try_from(size) else {
            return ffi::SQLITE_IOERR_TRUNCATE;
        };
        let mut state = tracked.machine.state();
        if !state.admit() {
            return ffi::SQLITE_IOERR_TRUNCATE;
        }
        state
            .pending
            .entry(tracked.path.clone())
            .or_default()
            .push(Op::Truncate(size));
        ffi::SQLITE_OK
    }
}

unsafe extern "C" fn sync(file: *mut ffi::sqlite3_file, flags: c_int) -> c_int {
    // SAFETY: an open file; the pending writes' buffers are live.
    unsafe {
        let Some(tracked) = tracked(file) else {
            return match methods(file).xSync {
                Some(sync) => sync(real(file), flags),
                None => ffi::SQLITE_IOERR_FSYNC,
            };
        };
        let mut state = tracked.machine.state();
        if !state.admit() {
            return ffi::SQLITE_IOERR_FSYNC;
        }
        let Some(ops) = state.pending.remove(&tracked.path) else {
            return ffi::SQLITE_OK;
        };
        // The real file is the durable disk: applying is the sync. A real
        // fsync would only slow the suite down.
        let real_methods = methods(file);
        for op in &ops {
            let rc = match op {
                Op::Write { offset, data } => match (
                    real_methods.xWrite,
                    c_int::try_from(data.len()),
                    ffi::sqlite3_int64::try_from(*offset),
                ) {
                    (Some(write), Ok(amount), Ok(offset)) => {
                        write(real(file), data.as_ptr() as *const c_void, amount, offset)
                    }
                    _ => ffi::SQLITE_IOERR_WRITE,
                },
                Op::Truncate(to) => {
                    match (real_methods.xTruncate, ffi::sqlite3_int64::try_from(*to)) {
                        (Some(truncate), Ok(to)) => truncate(real(file), to),
                        _ => ffi::SQLITE_IOERR_TRUNCATE,
                    }
                }
            };
            if rc != ffi::SQLITE_OK {
                // The disk failed mid-sync: nothing after this is
                // durable, and the machine is no use any more.
                state.off = true;
                return ffi::SQLITE_IOERR_FSYNC;
            }
        }
        ffi::SQLITE_OK
    }
}

unsafe extern "C" fn file_size(
    file: *mut ffi::sqlite3_file,
    out: *mut ffi::sqlite3_int64,
) -> c_int {
    // SAFETY: SQLite passes a writable size.
    unsafe {
        let Some(tracked) = tracked(file) else {
            return match methods(file).xFileSize {
                Some(file_size) => file_size(real(file), out),
                None => ffi::SQLITE_IOERR_FSTAT,
            };
        };
        let state = tracked.machine.state();
        if state.off {
            return ffi::SQLITE_IOERR_FSTAT;
        }
        let size = match real_size(file) {
            Ok(size) => size,
            Err(rc) => return rc,
        };
        let ops = state
            .pending
            .get(&tracked.path)
            .map_or(&[][..], Vec::as_slice);
        match ffi::sqlite3_int64::try_from(replayed_size(size, ops)) {
            Ok(size) => {
                *out = size;
                ffi::SQLITE_OK
            }
            Err(_) => ffi::SQLITE_IOERR_FSTAT,
        }
    }
}

unsafe extern "C" fn lock(file: *mut ffi::sqlite3_file, level: c_int) -> c_int {
    // SAFETY: an open file.
    unsafe {
        match methods(file).xLock {
            Some(lock) => lock(real(file), level),
            None => ffi::SQLITE_IOERR_LOCK,
        }
    }
}

unsafe extern "C" fn unlock(file: *mut ffi::sqlite3_file, level: c_int) -> c_int {
    // SAFETY: an open file.
    unsafe {
        match methods(file).xUnlock {
            Some(unlock) => unlock(real(file), level),
            None => ffi::SQLITE_IOERR_UNLOCK,
        }
    }
}

unsafe extern "C" fn check_reserved(file: *mut ffi::sqlite3_file, out: *mut c_int) -> c_int {
    // SAFETY: an open file and a writable result.
    unsafe {
        match methods(file).xCheckReservedLock {
            Some(check) => check(real(file), out),
            None => ffi::SQLITE_IOERR_CHECKRESERVEDLOCK,
        }
    }
}

unsafe extern "C" fn file_control(
    file: *mut ffi::sqlite3_file,
    op: c_int,
    arg: *mut c_void,
) -> c_int {
    // SAFETY: an open file; `arg` is the op's own argument.
    unsafe {
        // A size hint or chunk size would grow the real file outside the
        // pending writes; both are only hints.
        if tracked(file).is_some()
            && (op == ffi::SQLITE_FCNTL_SIZE_HINT || op == ffi::SQLITE_FCNTL_CHUNK_SIZE)
        {
            return ffi::SQLITE_OK;
        }
        match methods(file).xFileControl {
            Some(control) => control(real(file), op, arg),
            None => ffi::SQLITE_NOTFOUND,
        }
    }
}

unsafe extern "C" fn sector_size(file: *mut ffi::sqlite3_file) -> c_int {
    // SAFETY: an open file.
    unsafe {
        methods(file)
            .xSectorSize
            .map_or(4096, |size| size(real(file)))
    }
}

unsafe extern "C" fn device_characteristics(file: *mut ffi::sqlite3_file) -> c_int {
    // SAFETY: an open file.
    unsafe {
        let real_characteristics = methods(file)
            .xDeviceCharacteristics
            .map_or(0, |characteristics| characteristics(real(file)));
        if tracked(file).is_none() {
            return real_characteristics;
        }
        // Each pending write is applied whole or not at all, so overwrite
        // is power-safe; nothing is atomic or ordered without a sync.
        real_characteristics & ffi::SQLITE_IOCAP_POWERSAFE_OVERWRITE
    }
}

unsafe extern "C" fn shm_map(
    file: *mut ffi::sqlite3_file,
    region: c_int,
    size: c_int,
    extend: c_int,
    out: *mut *mut c_void,
) -> c_int {
    // SAFETY: an open file whose real methods are version 2 or later.
    unsafe {
        match methods(file).xShmMap {
            Some(map) => map(real(file), region, size, extend, out),
            None => ffi::SQLITE_IOERR_SHMMAP,
        }
    }
}

unsafe extern "C" fn shm_lock(
    file: *mut ffi::sqlite3_file,
    offset: c_int,
    n: c_int,
    flags: c_int,
) -> c_int {
    // SAFETY: as shm_map.
    unsafe {
        match methods(file).xShmLock {
            Some(lock) => lock(real(file), offset, n, flags),
            None => ffi::SQLITE_IOERR_SHMLOCK,
        }
    }
}

unsafe extern "C" fn shm_barrier(file: *mut ffi::sqlite3_file) {
    // SAFETY: as shm_map.
    unsafe {
        if let Some(barrier) = methods(file).xShmBarrier {
            barrier(real(file));
        }
    }
}

unsafe extern "C" fn shm_unmap(file: *mut ffi::sqlite3_file, delete: c_int) -> c_int {
    // SAFETY: as shm_map.
    unsafe {
        match methods(file).xShmUnmap {
            Some(unmap) => unmap(real(file), delete),
            None => ffi::SQLITE_OK,
        }
    }
}

#[cfg(test)]
mod tests;
