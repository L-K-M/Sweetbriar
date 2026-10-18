//! `FaultFs`: a pass-through `Fs` wrapper that fails exactly one
//! chosen operation — the Nth call of it — with a chosen failure shape
//! (spec/11 "I/O faults": short writes, `ENOSPC`, `EIO`, rename and
//! directory-sync failures). Every other call goes to the wrapped `Fs`
//! unchanged, and every operation is counted, so a test can assert what
//! the code under test actually attempted.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use sb_types::fs::{Fs, Lease, Location, LockError, Metadata, WriteFile};

/// The filesystem operations `FaultFs` can count and fail, named after
/// the `Fs` (or write-handle) method they wrap.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FsOp {
    /// `write` on a file the wrapper handed out.
    Write,
    /// `sync_all` on a file the wrapper handed out.
    SyncFile,
    /// `Fs::sync_dir`.
    SyncDir,
    /// `Fs::rename`.
    Rename,
    /// `Fs::create_dir`.
    CreateDir,
    /// `Fs::create`.
    Create,
    /// `Fs::remove`.
    Remove,
}

/// The shape of the injected failure.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum FaultKind {
    /// Write about half the buffer, then fail: a torn write, as when
    /// space runs out mid-block. Only meaningful on [`FsOp::Write`];
    /// everywhere else it behaves like [`FaultKind::Eio`].
    ShortWrite,
    /// `ENOSPC`: the filesystem is full.
    NoSpace,
    /// `EIO`: the storage layer failed.
    Eio,
}

/// How many calls of each operation have gone through, per kind.
#[derive(Default)]
struct Counts {
    write: u64,
    sync_file: u64,
    sync_dir: u64,
    rename: u64,
    create_dir: u64,
    create: u64,
    remove: u64,
}

impl Counts {
    fn slot(&mut self, op: FsOp) -> &mut u64 {
        match op {
            FsOp::Write => &mut self.write,
            FsOp::SyncFile => &mut self.sync_file,
            FsOp::SyncDir => &mut self.sync_dir,
            FsOp::Rename => &mut self.rename,
            FsOp::CreateDir => &mut self.create_dir,
            FsOp::Create => &mut self.create,
            FsOp::Remove => &mut self.remove,
        }
    }
}

/// The one fault being injected, shared with the write handles.
struct FaultPlan {
    op: FsOp,
    /// 1-based: fail this call of `op`.
    nth: u64,
    kind: FaultKind,
    counts: Mutex<Counts>,
}

impl FaultPlan {
    /// Count one call of `op` and say how it fails, if it does.
    fn fire(&self, op: FsOp) -> Option<FaultKind> {
        let mut counts = self
            .counts
            .lock()
            .expect("a panicking thread held the fault counters");
        let count = {
            let slot = counts.slot(op);
            *slot += 1;
            *slot
        };
        (op == self.op && count == self.nth).then_some(self.kind)
    }

    fn calls(&self, op: FsOp) -> u64 {
        let mut counts = self
            .counts
            .lock()
            .expect("a panicking thread held the fault counters");
        *counts.slot(op)
    }
}

/// The failure the kind describes, as an `io::Error`.
fn fault_error(kind: FaultKind) -> io::Error {
    match kind {
        FaultKind::NoSpace => io::Error::from_raw_os_error(libc::ENOSPC),
        // `ShortWrite` already wrote its half; the error that ends the
        // write is the same one a failing device would give.
        FaultKind::Eio | FaultKind::ShortWrite => io::Error::from_raw_os_error(libc::EIO),
    }
}

/// An `Fs` that fails the Nth call of one operation (spec/11), passing
/// everything else through to the wrapped `Fs`.
pub struct FaultFs {
    inner: Arc<dyn Fs>,
    plan: Arc<FaultPlan>,
}

impl FaultFs {
    /// Fail the `nth` call (1-based) of `op` with `kind`.
    pub fn new(inner: Arc<dyn Fs>, op: FsOp, nth: u64, kind: FaultKind) -> Self {
        assert!(nth >= 1, "fault points are 1-based");
        Self {
            inner,
            plan: Arc::new(FaultPlan {
                op,
                nth,
                kind,
                counts: Mutex::new(Counts::default()),
            }),
        }
    }

    /// How many calls of `op` have gone through so far.
    pub fn calls(&self, op: FsOp) -> u64 {
        self.plan.calls(op)
    }
}

impl Fs for FaultFs {
    fn create(&self, path: &Path) -> io::Result<Box<dyn WriteFile>> {
        if let Some(kind) = self.plan.fire(FsOp::Create) {
            return Err(fault_error(kind));
        }
        let inner = self.inner.create(path)?;
        Ok(Box::new(FaultWrite {
            inner,
            plan: self.plan.clone(),
        }))
    }

    fn open(&self, path: &Path) -> io::Result<Box<dyn sb_types::fs::ReadFile>> {
        self.inner.open(path)
    }

    fn open_regular(&self, path: &Path) -> io::Result<(Box<dyn sb_types::fs::ReadFile>, Metadata)> {
        self.inner.open_regular(path)
    }

    fn metadata(&self, path: &Path) -> io::Result<Option<Metadata>> {
        self.inner.metadata(path)
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        self.inner.read_dir(path)
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        if let Some(kind) = self.plan.fire(FsOp::CreateDir) {
            return Err(fault_error(kind));
        }
        self.inner.create_dir(path)
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        if let Some(kind) = self.plan.fire(FsOp::SyncDir) {
            return Err(fault_error(kind));
        }
        self.inner.sync_dir(path)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        if let Some(kind) = self.plan.fire(FsOp::Rename) {
            return Err(fault_error(kind));
        }
        self.inner.rename(from, to)
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        if let Some(kind) = self.plan.fire(FsOp::Remove) {
            return Err(fault_error(kind));
        }
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

/// A write handle whose `write` and `sync_all` count (and can fail)
/// like any other operation.
struct FaultWrite {
    inner: Box<dyn WriteFile>,
    plan: Arc<FaultPlan>,
}

impl io::Write for FaultWrite {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        match self.plan.fire(FsOp::Write) {
            // A torn write: about half the buffer lands, then the
            // failure — the caller cannot know how much was written.
            Some(FaultKind::ShortWrite) => {
                let keep = buf.len().div_ceil(2);
                if keep > 0 {
                    self.inner.write_all(&buf[..keep])?;
                }
                Err(fault_error(FaultKind::ShortWrite))
            }
            Some(kind) => Err(fault_error(kind)),
            None => self.inner.write(buf),
        }
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl WriteFile for FaultWrite {
    fn sync_all(&mut self) -> io::Result<()> {
        if let Some(kind) = self.plan.fire(FsOp::SyncFile) {
            return Err(fault_error(kind));
        }
        self.inner.sync_all()
    }
}

#[cfg(test)]
mod tests {
    use std::io::Write as _;
    use std::path::Path;

    use super::*;
    use crate::SimFs;

    /// A scratch in-memory root: the sim needs no disk and counts as
    /// `Local` everywhere.
    fn scratch_fs() -> (Arc<SimFs>, PathBuf) {
        let fs = Arc::new(SimFs::new());
        fs.create_dir(Path::new("/x")).unwrap();
        (fs, PathBuf::from("/x"))
    }

    /// Only the Nth call of the chosen operation fails — exactly once —
    /// and the counts see every call, handle writes included.
    #[test]
    fn the_nth_call_of_one_operation_fails_once() {
        let (inner, dir) = scratch_fs();
        let fs = FaultFs::new(inner, FsOp::SyncFile, 2, FaultKind::Eio);
        let path = dir.join("only");
        let mut file = fs.create(&path).unwrap();
        file.write_all(b"bytes").unwrap();
        // The first sync passes, the second is the fault, and the
        // fault is spent: a third call passes again (a retry works).
        file.sync_all().unwrap();
        assert_eq!(file.sync_all().unwrap_err().raw_os_error(), Some(libc::EIO));
        file.sync_all().unwrap();
        drop(file);
        assert_eq!(fs.calls(FsOp::SyncFile), 3);
        assert_eq!(fs.calls(FsOp::Write), 1);
        assert_eq!(fs.calls(FsOp::Create), 1);
        assert_eq!(fs.calls(FsOp::SyncDir), 0);
    }

    /// A short write lands part of the buffer and then fails, as
    /// running out of space mid-block would.
    #[test]
    fn a_short_write_tears_the_buffer() {
        let (inner, dir) = scratch_fs();
        let fs = FaultFs::new(inner, FsOp::Write, 1, FaultKind::ShortWrite);
        let mut file = fs.create(&dir.join("torn")).unwrap();
        let err = file.write_all(b"0123456789").unwrap_err();
        assert_eq!(err.raw_os_error(), Some(libc::EIO));
        drop(file);
        let len = fs.metadata(&dir.join("torn")).unwrap().unwrap().len();
        assert_eq!(len, 5, "about half the buffer was written");
    }

    /// `NoSpace` is `ENOSPC`, and everything but the chosen call
    /// passes through untouched.
    #[test]
    fn no_space_is_enospc_and_the_rest_passes_through() {
        let (inner, dir) = scratch_fs();
        let fs = FaultFs::new(inner.clone(), FsOp::Rename, 1, FaultKind::NoSpace);
        let file = dir.join("f");
        let mut write = fs.create(&file).unwrap();
        write.write_all(b"kept").unwrap();
        write.sync_all().unwrap();
        drop(write);
        assert_eq!(
            fs.rename(&file, &dir.join("g")).unwrap_err().raw_os_error(),
            Some(libc::ENOSPC)
        );
        fs.sync_dir(&dir).unwrap();
        // The pass-through rename reaches the wrapped fs.
        fs.rename(&file, &dir.join("g")).unwrap();
        assert!(inner.metadata(&dir.join("g")).unwrap().is_some());
    }
}
