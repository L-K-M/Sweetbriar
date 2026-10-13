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
    /// Create a new file for writing; fails if `path` already exists.
    fn create(&self, path: &Path) -> io::Result<Box<dyn WriteFile>>;
    /// Open an existing file for reading and seeking.
    fn open(&self, path: &Path) -> io::Result<Box<dyn ReadFile>>;
    /// Describe `path`, following symlinks; `Ok(None)` when it does not
    /// exist.
    fn metadata(&self, path: &Path) -> io::Result<Option<Metadata>>;
    /// List a directory's entries as full paths, sorted.
    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>>;
    /// Create one directory level; fails if it already exists.
    fn create_dir(&self, path: &Path) -> io::Result<()>;
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
/// Dropping it releases the lock; anything `Send` can stand in for one.
pub trait Lease: Send {}
impl<T: Send> Lease for T {}

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
