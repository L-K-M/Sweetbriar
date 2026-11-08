//! The `Fs` seam (spec/01 rule 5): every filesystem effect the core
//! needs, behind one trait so leaves take no platform dependency and
//! tests can inject faults. `sb-platform` implements the real one;
//! `sb-test-support` hands it out.

use std::error::Error;
use std::fmt;
use std::io;
use std::path::{Path, PathBuf};

/// The filesystem seam. Object-safe; shared as `Arc<dyn Fs>`.
pub trait Fs: Send + Sync {
    /// Open an absolute directory without following any symbolic link.
    /// The returned capability remains anchored to that directory if its
    /// pathname is renamed. Unsupported adapters fail closed.
    fn open_confined_dir(&self, _path: &Path) -> io::Result<Box<dyn ConfinedDir>> {
        Err(io::ErrorKind::Unsupported.into())
    }

    /// Create a new file for writing; fails if `path` already exists.
    fn create(&self, path: &Path) -> io::Result<Box<dyn WriteFile>>;
    /// Open an existing file for reading and seeking.
    fn open(&self, path: &Path) -> io::Result<Box<dyn ReadFile>>;
    /// Open a regular file without following its final symlink.
    fn open_no_follow(&self, _path: &Path) -> io::Result<Box<dyn ReadFile>> {
        Err(io::ErrorKind::Unsupported.into())
    }
    /// Open `path` for reading and seeking only when it names a
    /// regular file, and return the `Metadata` of the file actually
    /// opened with the handle. One operation: a `metadata` look
    /// followed by [`Fs::open`] can be raced into opening a fifo,
    /// device or socket swapped in between the two calls. Fails with
    /// `ErrorKind::InvalidInput` when `path` opens to anything but a
    /// regular file — promptly: a fifo without a writer must not
    /// park the caller — and with the filesystem's own error
    /// (`NotFound`, permissions, …) otherwise, including kinds that
    /// cannot be opened at all. The handle and its `Metadata` stay
    /// bound to the opened file: renaming or replacing `path`
    /// afterwards changes neither. A symlink to a regular file counts
    /// as regular.
    fn open_regular(&self, path: &Path) -> io::Result<(Box<dyn ReadFile>, Metadata)>;
    /// Describe `path`, following symlinks; `Ok(None)` when it does not
    /// exist.
    fn metadata(&self, path: &Path) -> io::Result<Option<Metadata>>;
    /// List a directory's entries as full paths, sorted.
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>>;
    /// Create one directory level; fails if it already exists.
    fn create_dir(&self, path: &Path) -> io::Result<()>;
    /// Create one owner-only directory (0700), exclusively.
    /// Legacy adapters fail closed until they implement this operation.
    fn create_private_dir(&self, _path: &Path) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }
    /// Restrict an existing directory to its owner (0700), refusing symlinks.
    fn restrict_dir(&self, _path: &Path) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }
    /// Remove a directory tree without following symlinks. Missing is success.
    fn remove_dir_all(&self, _path: &Path) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }
    /// Mark an untrusted file before exposure. macOS sets quarantine;
    /// Linux is explicitly a no-op and provides no Gatekeeper protection.
    fn quarantine(&self, _path: &Path) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }
    /// Durably flush a directory's entries; returns only after they are
    /// on stable storage.
    fn sync_dir(&self, path: &Path) -> io::Result<()>;
    /// Rename `from` to `to`, replacing `to` if it exists.
    fn rename(&self, from: &Path, to: &Path) -> io::Result<()>;
    /// Remove a file.
    fn remove(&self, path: &Path) -> io::Result<()>;
    /// The absolute path of an existing `path`, with every symlink
    /// resolved: two spellings of one directory compare equal.
    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf>;
    /// Take the exclusive, non-blocking lock on `path` (created when
    /// absent), or report it held.
    fn lock_exclusive(&self, path: &Path) -> Result<Box<dyn Lease>, LockError>;
    /// Classify the filesystem location of `path`, or of its nearest
    /// existing ancestor when `path` does not exist.
    fn location(&self, path: &Path) -> io::Result<Location>;
}

/// A directory capability for race-safe, no-symlink descendant operations.
pub trait ConfinedDir: Send + Sync {
    /// Visit direct child names with bounded memory, in unspecified order.
    /// Names are untrusted and may change during or after the visit. Opening
    /// a child separately must still enforce confinement. Returning `Break`
    /// stops immediately; a visitor error or enumeration error is propagated.
    /// The visitor may use `Break` for cancellation and must discard partial
    /// results on error. Unsupported adapters fail closed.
    fn visit_children(
        &self,
        _visitor: &mut dyn FnMut(&Path) -> io::Result<std::ops::ControlFlow<()>>,
    ) -> io::Result<std::ops::ControlFlow<()>> {
        Err(io::ErrorKind::Unsupported.into())
    }
    /// Create this directory's private ownership marker, without replacing one.
    /// `sync` makes the marker durable along with the directory.
    fn set_ownership(&self, _bytes: &[u8]) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }
    /// Read this directory's private ownership marker, at most 36 bytes.
    fn ownership(&self) -> io::Result<Vec<u8>> {
        Err(io::ErrorKind::Unsupported.into())
    }
    /// Stable filesystem identity of this open directory.
    fn identity(&self) -> io::Result<DirectoryIdentity> {
        Err(io::ErrorKind::Unsupported.into())
    }
    /// Open a direct child directory without following links.
    fn open_dir(&self, _name: &Path) -> io::Result<Box<dyn ConfinedDir>> {
        Err(io::ErrorKind::Unsupported.into())
    }
    /// Create an owner-only direct child directory exclusively.
    fn create_private_dir(&self, _name: &Path) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }
    /// Create a direct child file exclusively.
    fn create(&self, _name: &Path) -> io::Result<Box<dyn WriteFile>> {
        Err(io::ErrorKind::Unsupported.into())
    }
    /// Exclusively create an owner-only regular file as a direct child and
    /// retain read/write authority to that file. Reject existing entries
    /// (including symlinks), absolute paths and parent components. The handle
    /// stays bound to the created file across pathname replacement or rename.
    /// No operation on the returned handle may resolve a pathname again.
    /// Unsupported adapters fail closed; there is no existing-file fallback.
    fn create_read_write(&self, _name: &Path) -> io::Result<Box<dyn ReadWriteFile>> {
        Err(io::ErrorKind::Unsupported.into())
    }
    /// Remove a direct child file without following links.
    fn remove_file(&self, _name: &Path) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }
    /// Remove an empty direct child directory, without following links.
    fn remove_empty_dir(&self, _name: &Path) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }
    /// Flush this directory.
    fn sync(&self) -> io::Result<()> {
        Err(io::ErrorKind::Unsupported.into())
    }
    /// Open a regular file below this directory. Reject absolute paths,
    /// parent components, symlinks and non-regular files. Resolution stays
    /// anchored to the opened directory even if its pathname is replaced.
    fn open_regular(&self, relative: &Path) -> io::Result<(Box<dyn ReadFile>, Metadata)>;
    /// Open a regular descendant for reading, returning metadata and identity
    /// from that same open file. Reject symlinks, traversal and non-files as
    /// for `open_regular`. Identity remains stable while the handle is held;
    /// it does not certify immutable bytes. No write authority is granted.
    /// Unsupported adapters fail closed, without a pathname-based fallback.
    fn open_regular_identified(
        &self,
        _relative: &Path,
    ) -> io::Result<(Box<dyn ReadFile>, Metadata, FileIdentity)> {
        Err(io::ErrorKind::Unsupported.into())
    }
}

/// Filesystem-local identity of an open directory.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DirectoryIdentity {
    /// Filesystem device identifier.
    pub device: u64,
    /// Directory inode identifier.
    pub inode: u64,
}

/// Filesystem-local identity of a retained regular file. It is stable while
/// the handle is alive, but does not certify the file's bytes are unchanged.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct FileIdentity {
    /// Filesystem device identifier.
    pub device: u64,
    /// File inode identifier.
    pub inode: u64,
}

/// A newly created file retained for positioned I/O. Operations have no shared
/// cursor. Reads and writes may be short and callers must handle interruptions;
/// unwritten gaps and extensions read as zero. Concurrent overlapping writes or
/// truncation need caller synchronization. Holding this capability preserves
/// identity, not immutability against another process with filesystem access.
pub trait ReadWriteFile: Send + Sync {
    /// Read at `offset`, returning zero at or beyond EOF.
    fn read_at(&self, bytes: &mut [u8], offset: u64) -> io::Result<usize>;
    /// Write at `offset`, extending the file when needed.
    fn write_at(&self, bytes: &[u8], offset: u64) -> io::Result<usize>;
    /// Change the file length, discarding truncated bytes or zero-filling growth.
    fn set_len(&self, len: u64) -> io::Result<()>;
    /// Current length of this retained file.
    fn len(&self) -> io::Result<u64>;
    /// Whether this retained file is empty.
    fn is_empty(&self) -> io::Result<bool> {
        self.len().map(|len| len == 0)
    }
    /// Flush this retained file to stable storage, without reopening its name.
    /// Directory entry durability remains the directory capability's job.
    fn sync_all(&self) -> io::Result<()>;
    /// Identity of the file actually created and still retained.
    fn identity(&self) -> io::Result<FileIdentity>;
}

/// A file open for writing through the `Fs` seam.
pub trait WriteFile: io::Write + Send {
    /// Returns only after the written data is on stable storage.
    fn sync_all(&mut self) -> io::Result<()>;
}

/// A file open for reading through the `Fs` seam. Any readable,
/// seekable, `Send` file qualifies.
pub trait ReadFile: io::Read + io::Seek + Send {}
impl<T: io::Read + io::Seek + Send> ReadFile for T {}

/// A held exclusive lock (spec/03 "One process owns the store").
/// Dropping it releases the lock; anything `Send + Sync` can stand in
/// for one. `Sync` because the store that holds it is shared as
/// `Arc<Store>` across tasks (spec/01).
pub trait Lease: Send + Sync {}
impl<T: Send + Sync> Lease for T {}

/// Why `Fs::lock_exclusive` did not hand out a lease.
#[derive(Debug)]
pub enum LockError {
    /// Another open file description already holds the lock.
    Held,
    /// The filesystem refused the lock or the lock file.
    Io(io::Error),
}

impl fmt::Display for LockError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Held => f.write_str("the lock is held"),
            Self::Io(e) => write!(f, "the lock could not be taken: {e}"),
        }
    }
}

impl Error for LockError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Held => None,
            Self::Io(e) => Some(e),
        }
    }
}

/// Where a path lives, for the spec/03 local-filesystem rule.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Location {
    /// On a local filesystem.
    Local,
    /// Inside a folder a sync client uploads, e.g. iCloud Drive or
    /// Dropbox; `service` names it.
    Synced { service: String },
    /// On a network filesystem; `filesystem` names it (e.g. "NFS").
    Network { filesystem: String },
}

/// What `Fs::metadata` knows about an entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Metadata {
    kind: FileKind,
    len: u64,
}

impl Metadata {
    pub fn new(kind: FileKind, len: u64) -> Self {
        Self { kind, len }
    }

    pub fn kind(&self) -> FileKind {
        self.kind
    }

    pub fn len(&self) -> u64 {
        self.len
    }

    /// Zero bytes long (a missing entry has no `Metadata` at all).
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
}

/// The kind of a directory entry, from `Fs::metadata`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FileKind {
    File,
    Directory,
    /// Neither file nor directory: fifo, socket, device, symlink target
    /// of another kind.
    Other,
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::{AtomicUsize, Ordering};

    #[test]
    fn identified_reads_default_to_unsupported_without_an_unidentified_fallback() {
        struct Legacy(AtomicUsize);
        impl ConfinedDir for Legacy {
            fn open_regular(&self, _: &Path) -> io::Result<(Box<dyn ReadFile>, Metadata)> {
                self.0.fetch_add(1, Ordering::SeqCst);
                Ok((
                    Box::new(io::Cursor::new(b"legacy".to_vec())),
                    Metadata::new(FileKind::File, 6),
                ))
            }
        }
        let legacy = Legacy(AtomicUsize::new(0));
        let error = legacy
            .open_regular_identified(Path::new("file"))
            .err()
            .unwrap();
        assert_eq!(error.kind(), io::ErrorKind::Unsupported);
        assert_eq!(legacy.0.load(Ordering::SeqCst), 0);
        assert_eq!(legacy.open_regular(Path::new("file")).unwrap().1.len(), 6);
    }
}
