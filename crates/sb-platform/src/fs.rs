//! `RealFs`: the `Fs` seam over the actual filesystem (spec/01 rule
//! 5): durable flushes, the exclusive store lock and the location
//! check.

use std::fs::{File, OpenOptions, TryLockError};
use std::io;
use std::path::{Path, PathBuf};

use sb_types::fs::{FileKind, Fs, Lease, Location, LockError, Metadata, WriteFile};

use crate::location;

/// The real filesystem seam: every method maps to the obvious syscall
/// or std call; `location` and the durable flushes follow spec/03.
#[derive(Debug, Default, Clone, Copy)]
pub struct RealFs;

impl Fs for RealFs {
    fn create(&self, path: &Path) -> io::Result<Box<dyn WriteFile>> {
        // create_new: fails when the file already exists.
        let file = OpenOptions::new().write(true).create_new(true).open(path)?;
        Ok(Box::new(RealWriteFile(file)))
    }

    fn open(&self, path: &Path) -> io::Result<Box<dyn sb_types::fs::ReadFile>> {
        Ok(Box::new(File::open(path)?))
    }

    fn metadata(&self, path: &Path) -> io::Result<Option<Metadata>> {
        match std::fs::metadata(path) {
            Ok(md) => Ok(Some(Metadata::new(file_kind(&md), md.len()))),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        let mut entries = std::fs::read_dir(path)?
            .map(|entry| entry.map(|e| e.path()))
            .collect::<io::Result<Vec<_>>>()?;
        entries.sort();
        Ok(entries)
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        std::fs::create_dir(path)
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        flush_durably(&File::open(path)?)
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        std::fs::rename(from, to)
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        std::fs::remove_file(path)
    }

    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        std::fs::canonicalize(path)
    }

    fn lock_exclusive(&self, path: &Path) -> Result<Box<dyn Lease>, LockError> {
        // read+write, create when absent, never truncate (spec/03).
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .create(true)
            .truncate(false)
            .open(path)
            .map_err(LockError::Io)?;
        file.try_lock().map_err(|e| match e {
            TryLockError::WouldBlock => LockError::Held,
            TryLockError::Error(e) => LockError::Io(e),
        })?;
        Ok(Box::new(LockGuard {
            file,
            owner: std::process::id(),
        }))
    }

    fn location(&self, path: &Path) -> io::Result<Location> {
        location::classify(path)
    }
}

fn file_kind(md: &std::fs::Metadata) -> FileKind {
    let ty = md.file_type();
    if ty.is_file() {
        FileKind::File
    } else if ty.is_dir() {
        FileKind::Directory
    } else {
        FileKind::Other
    }
}

/// Durable flush per spec/03: `fsync` on Linux, an explicit
/// `fcntl(F_FULLFSYNC)` on macOS — not std's `File::sync_all`, and an
/// error is never weakened to a plain fsync.
#[cfg(target_os = "linux")]
fn flush_durably(file: &File) -> io::Result<()> {
    file.sync_all()
}

#[cfg(target_os = "macos")]
fn flush_durably(file: &File) -> io::Result<()> {
    use std::os::fd::AsRawFd;

    // SAFETY: `fd` is a valid open descriptor; F_FULLFSYNC takes no
    // third argument.
    if unsafe { libc::fcntl(file.as_raw_fd(), libc::F_FULLFSYNC) } == -1 {
        Err(io::Error::last_os_error())
    } else {
        Ok(())
    }
}

/// A held `flock`. A forked child shares the lock through its copy of
/// the descriptor, so closing ours alone wouldn't release it: drop
/// unlocks explicitly, but only in the process that took the lock, so
/// a child dropping its copy can't release the parent's lease.
struct LockGuard {
    file: File,
    owner: u32,
}

impl Drop for LockGuard {
    fn drop(&mut self) {
        if std::process::id() != self.owner {
            return;
        }
        // Closing the descriptor right after releases the lock anyway
        // when no child shares it, so an unlock error changes nothing.
        let _ = self.file.unlock();
    }
}

/// A file the seam opened for writing, flushing durably on `sync_all`.
/// A newtype: `WriteFile` and `File` are both foreign, so the impl
/// can't land on `File` itself.
struct RealWriteFile(File);

impl io::Write for RealWriteFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.0.flush()
    }
}

impl WriteFile for RealWriteFile {
    fn sync_all(&mut self) -> io::Result<()> {
        flush_durably(&self.0)
    }
}

#[cfg(test)]
mod tests {
    use std::io::{Read, Seek, SeekFrom, Write};

    use super::*;

    /// A scratch directory removed on drop (sb-test-support's
    /// ScratchDir can't be used: it depends on this crate).
    struct Scratch(PathBuf);

    impl Scratch {
        fn new(label: &str) -> Self {
            let path = std::env::temp_dir()
                .join(format!("sb-platform-test-{}-{label}", std::process::id()));
            let _ = std::fs::remove_dir_all(&path);
            std::fs::create_dir(&path).unwrap();
            Self(path)
        }
    }

    impl Drop for Scratch {
        fn drop(&mut self) {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn every_method_round_trips() {
        let scratch = Scratch::new("round-trip");
        let fs = RealFs;
        let dir = scratch.0.join("d");

        fs.create_dir(&dir).unwrap();
        fs.sync_dir(&dir).unwrap();

        let file = dir.join("a.txt");
        let mut w = fs.create(&file).unwrap();
        w.write_all(b"hello").unwrap();
        w.sync_all().unwrap();
        drop(w);

        let mut r = fs.open(&file).unwrap();
        let mut buf = String::new();
        r.seek(SeekFrom::Start(1)).unwrap();
        r.read_to_string(&mut buf).unwrap();
        assert_eq!(buf, "ello");

        let md = fs.metadata(&file).unwrap().unwrap();
        assert_eq!(md.kind(), FileKind::File);
        assert_eq!(md.len(), 5);
        assert_eq!(fs.metadata(&dir.join("missing")).unwrap(), None);
        assert_eq!(
            fs.metadata(&dir).unwrap().unwrap().kind(),
            FileKind::Directory
        );

        assert_eq!(fs.read_dir(&dir).unwrap(), vec![file.clone()]);

        let moved = dir.join("b.txt");
        fs.rename(&file, &moved).unwrap();
        assert_eq!(fs.metadata(&file).unwrap(), None);
        fs.remove(&moved).unwrap();
        assert_eq!(fs.read_dir(&dir).unwrap(), Vec::<PathBuf>::new());
    }

    #[test]
    fn create_fails_when_the_file_exists() {
        let scratch = Scratch::new("create-twice");
        let fs = RealFs;
        let file = scratch.0.join("a");
        fs.create(&file).unwrap();
        let err = fs
            .create(&file)
            .err()
            .expect("creating an existing file must fail");
        assert_eq!(err.kind(), io::ErrorKind::AlreadyExists);
        // create_dir is one level and also fails when it exists.
        assert!(fs.create_dir(&file.join("inner")).is_err());
        assert!(fs.create_dir(&scratch.0).is_err());
    }

    #[test]
    fn lock_exclusive_blocks_a_second_take_until_dropped() {
        let scratch = Scratch::new("lock");
        let fs = RealFs;
        let lock = scratch.0.join("x.lock");

        let lease = fs.lock_exclusive(&lock).unwrap();
        assert!(matches!(fs.lock_exclusive(&lock), Err(LockError::Held)));
        drop(lease);
        assert!(fs.lock_exclusive(&lock).is_ok());
        // The lock file is never deleted.
        assert_eq!(fs.metadata(&lock).unwrap().unwrap().kind(), FileKind::File);
    }

    /// A forked child shares the lock through its inherited descriptor;
    /// dropping the parent's lease must still release the lock.
    #[test]
    fn dropping_the_lease_releases_it_despite_a_forked_child() {
        let scratch = Scratch::new("lock-fork");
        let lock = scratch.0.join("x.lock");
        let lease = RealFs.lock_exclusive(&lock).unwrap();

        let mut pipe = [0; 2];
        // SAFETY: `pipe` has room for the two descriptors.
        assert_eq!(unsafe { libc::pipe(pipe.as_mut_ptr()) }, 0);

        // SAFETY: the child calls only async-signal-safe functions
        // (close, read, _exit), so forking a threaded test is sound.
        let pid = unsafe { libc::fork() };
        if pid == 0 {
            // Child: keep the inherited lock descriptor open until the
            // parent closes the pipe, then exit without destructors.
            let mut byte = 0u8;
            // SAFETY: valid descriptors and a one-byte buffer.
            unsafe {
                libc::close(pipe[1]);
                libc::read(pipe[0], (&raw mut byte).cast(), 1);
                libc::_exit(0);
            }
        }
        assert!(pid > 0, "fork failed");
        // SAFETY: closing our copy of the read end.
        unsafe { libc::close(pipe[0]) };

        drop(lease);
        let relocked = RealFs.lock_exclusive(&lock);

        let mut status = 0;
        // SAFETY: closing the write end lets the child exit; then reap it.
        unsafe {
            libc::close(pipe[1]);
            libc::waitpid(pid, &mut status, 0);
        }
        assert!(relocked.is_ok(), "the forked child kept the lock");
    }

    #[test]
    fn canonicalize_resolves_symlinks() {
        let scratch = Scratch::new("canonicalize");
        let target = scratch.0.join("target");
        let alias = scratch.0.join("alias");
        std::fs::create_dir(&target).unwrap();
        std::os::unix::fs::symlink(&target, &alias).unwrap();
        assert_eq!(
            RealFs.canonicalize(&alias).unwrap(),
            RealFs.canonicalize(&target).unwrap()
        );
    }

    #[test]
    fn location_of_a_scratch_dir_is_local() {
        let scratch = Scratch::new("location");
        assert_eq!(RealFs.location(&scratch.0).unwrap(), Location::Local);
        // A not-yet-existing path classifies by its nearest ancestor.
        let deep = scratch.0.join("a/b/c");
        assert_eq!(RealFs.location(&deep).unwrap(), Location::Local);
    }
}
