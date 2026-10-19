//! Disposable lease storage (spec/03, 07). Blocking I/O; the engine owns
//! scheduling, public lease IDs, cancellation and its registry (M1.5g).

use std::collections::HashSet;
use std::fmt;
use std::io::{self, Write};
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use sb_platform::{Fs, Lease, LockError, OsRandom, Random, RealFs};

// The lease and live budgets of spec/07. The 50,000,000-byte cap on a
// sanitized HTML or plain text file is its producer's (M1.5g): a source
// copy may reach the 100,000,000-byte message limit.
const LEASE_BYTES: u64 = 100_000_000;
const LIVE_BYTES: u64 = 512_000_000;
const LIVE_LEASES: usize = 64;
const TOKEN_ATTEMPTS: usize = 32;

/// Cache admission or filesystem failure. The service maps quotas to
/// Unavailable with this reason; no live lease is ever evicted.
#[derive(Debug)]
pub enum RenditionError {
    Quota(&'static str),
    Locked,
    Io(io::Error),
}

impl fmt::Display for RenditionError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Quota(reason) => write!(f, "rendition quota: {reason}"),
            Self::Locked => f.write_str("rendition cache is in use"),
            Self::Io(e) => write!(f, "rendition cache: {e}"),
        }
    }
}

impl std::error::Error for RenditionError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io(e) => Some(e),
            _ => None,
        }
    }
}

impl From<io::Error> for RenditionError {
    fn from(e: io::Error) -> Self {
        Self::Io(e)
    }
}

/// Whether this generated file will be offered as an attachment.
#[derive(Clone, Copy)]
pub enum RenditionProtection {
    Internal,
    Quarantine,
}

#[derive(Default)]
struct Reservations {
    bytes: u64,
    leases: usize,
    // Prevent reuse even after release, including with a faulty RNG.
    issued: HashSet<String>,
}

struct CacheInner {
    fs: Arc<dyn Fs>,
    random: Arc<dyn Random>,
    namespace: PathBuf,
    reservations: Mutex<Reservations>,
    // Leases retain this ownership lock even after the cache is dropped.
    _ownership: Box<dyn Lease>,
}

/// One exclusively owned `cache_root/renditions` namespace. Open only
/// after checking the cache root is separate from the data root (M1.5g).
pub struct RenditionCache(Arc<CacheInner>);

impl RenditionCache {
    /// Create missing cache ancestors, take ownership, then sweep stale
    /// entries only within renditions. Any cleanup error refuses startup.
    pub fn open(cache_root: &Path) -> Result<Self, RenditionError> {
        Self::open_on(cache_root, Arc::new(RealFs), Arc::new(OsRandom))
    }

    pub(crate) fn open_on(
        cache_root: &Path,
        fs: Arc<dyn Fs>,
        random: Arc<dyn Random>,
    ) -> Result<Self, RenditionError> {
        ensure_directory(fs.as_ref(), cache_root)?;
        let root = fs.canonicalize(cache_root)?;
        let ownership = fs
            .lock_exclusive(&root.join("renditions.lock"))
            .map_err(|e| match e {
                LockError::Held => RenditionError::Locked,
                LockError::Io(e) => RenditionError::Io(e),
            })?;
        let namespace = root.join("renditions");
        match fs.create_private_dir(&namespace) {
            Ok(()) => {}
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
                fs.restrict_dir(&namespace)?;
            }
            Err(e) => return Err(e.into()),
        }
        fs.sync_dir(&root)?;
        for entry in fs.read_dir(&namespace)? {
            match fs.remove_dir_all(&entry) {
                // Finder and other tools can leave files alongside lease dirs.
                Err(e) if e.kind() == io::ErrorKind::NotADirectory => fs.remove(&entry)?,
                result => result?,
            }
        }
        fs.sync_dir(&namespace)?;
        Ok(Self(Arc::new(CacheInner {
            fs,
            random,
            namespace,
            reservations: Mutex::new(Reservations::default()),
            _ownership: ownership,
        })))
    }

    /// Reserve exact sizes of all files before any bytes are written.
    /// The returned owner removes its directory on drop; explicit `remove`
    /// reports cleanup errors and can be retried. A failed cleanup stays
    /// charged until successful removal or the next startup sweep.
    pub fn reserve(&self, sizes: &[u64]) -> Result<RenditionFiles, RenditionError> {
        let bytes = sizes
            .iter()
            .try_fold(0u64, |sum, size| sum.checked_add(*size));
        let Some(bytes) = bytes.filter(|&bytes| bytes <= LEASE_BYTES) else {
            return Err(RenditionError::Quota("100,000,000 bytes per lease"));
        };
        let mut state = self
            .0
            .reservations
            .lock()
            .expect("cache reservation lock poisoned");
        if state.leases >= LIVE_LEASES {
            return Err(RenditionError::Quota("64 live leases"));
        }
        if bytes > LIVE_BYTES - state.bytes {
            return Err(RenditionError::Quota(
                "512,000,000 bytes across live leases",
            ));
        }
        for _ in 0..TOKEN_ATTEMPTS {
            let mut entropy = [0u8; 32];
            self.0.random.fill(&mut entropy)?;
            let token: String = entropy.iter().map(|b| format!("{b:02x}")).collect();
            if !state.issued.insert(token.clone()) {
                continue;
            }
            let directory = self.0.namespace.join(&token);
            match self.0.fs.create_private_dir(&directory) {
                Ok(()) => {}
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(e.into()),
            }
            state.bytes += bytes;
            state.leases += 1;
            drop(state);
            let files = RenditionFiles {
                cache: self.0.clone(),
                token,
                directory,
                sizes: sizes.to_vec(),
                published: HashSet::new(),
                status: Status::Active,
                bytes,
            };
            self.0.fs.sync_dir(&self.0.namespace)?;
            return Ok(files);
        }
        Err(io::Error::new(io::ErrorKind::AlreadyExists, "lease token collisions").into())
    }
}

#[derive(PartialEq, Eq)]
enum Status {
    Active,
    Failed,
    Removed,
}

/// An owned reservation and private files, independent of the engine's
/// registry. Keep this owner alive while consumers use returned paths.
pub struct RenditionFiles {
    cache: Arc<CacheInner>,
    token: String,
    directory: PathBuf,
    sizes: Vec<u64>,
    published: HashSet<usize>,
    status: Status,
    bytes: u64,
}

impl RenditionFiles {
    /// Unguessable token, also usable by a renderer when minting CID URLs.
    pub fn token(&self) -> &str {
        &self.token
    }

    /// Publish one reserved file. There is no filename argument: sender
    /// names and URL components never enter a path. A failed I/O attempt
    /// invalidates further publication; discard/remove this reservation.
    pub fn publish(
        &mut self,
        index: usize,
        bytes: &[u8],
        protection: RenditionProtection,
    ) -> Result<PathBuf, RenditionError> {
        self.publish_named(index, &format!("file-{index}"), bytes, protection)
    }

    /// Publish a generated, single-component name in this reservation.
    pub(crate) fn publish_named(
        &mut self,
        index: usize,
        name: &str,
        bytes: &[u8],
        protection: RenditionProtection,
    ) -> Result<PathBuf, RenditionError> {
        if name.is_empty()
            || name == "."
            || name == ".."
            || !name
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b".-_".contains(&b))
        {
            return Err(
                io::Error::new(io::ErrorKind::InvalidInput, "invalid generated filename").into(),
            );
        }
        if self.status != Status::Active
            || self.sizes.get(index).copied() != Some(bytes.len() as u64)
            || self.published.contains(&index)
        {
            return Err(
                io::Error::new(io::ErrorKind::InvalidInput, "invalid reserved file").into(),
            );
        }
        let result = self.write_file(index, name, bytes, protection);
        if result.is_err() {
            self.status = Status::Failed;
        } else {
            self.published.insert(index);
        }
        result
    }

    fn write_file(
        &self,
        index: usize,
        name: &str,
        bytes: &[u8],
        protection: RenditionProtection,
    ) -> Result<PathBuf, RenditionError> {
        let fs = &self.cache.fs;
        let temporary = self.directory.join(format!("file-{index}.tmp"));
        let final_path = self.directory.join(name);
        if fs.metadata(&final_path)?.is_some() {
            return Err(io::Error::new(
                io::ErrorKind::AlreadyExists,
                "duplicate generated filename",
            )
            .into());
        }
        let mut file = fs.create(&temporary)?;
        file.write_all(bytes)?;
        if matches!(protection, RenditionProtection::Quarantine) {
            fs.quarantine(&temporary)?;
        }
        file.sync_all()?;
        drop(file);
        fs.rename(&temporary, &final_path)?;
        fs.sync_dir(&self.directory)?;
        Ok(final_path)
    }

    /// Idempotent blocking cleanup. Budget is released only after removal
    /// and the parent flush both succeed. Errors leave ownership retryable.
    pub fn remove(&mut self) -> Result<(), RenditionError> {
        if self.status == Status::Removed {
            return Ok(());
        }
        self.status = Status::Failed;
        self.cache.fs.remove_dir_all(&self.directory)?;
        self.cache.fs.sync_dir(&self.cache.namespace)?;
        let mut state = self
            .cache
            .reservations
            .lock()
            .expect("cache reservation lock poisoned");
        state.bytes -= self.bytes;
        state.leases -= 1;
        self.status = Status::Removed;
        Ok(())
    }
}

impl Drop for RenditionFiles {
    fn drop(&mut self) {
        // Explicit removal reports errors; drop is a cancellation safety
        // net. Failed cleanup remains charged and is swept on next open.
        let _ = self.remove();
    }
}

fn ensure_directory(fs: &dyn Fs, path: &Path) -> io::Result<()> {
    if fs.metadata(path)?.is_some() {
        return Ok(());
    }
    let parent = path
        .parent()
        .filter(|p| !p.as_os_str().is_empty())
        .unwrap_or(Path::new("."));
    ensure_directory(fs, parent)?;
    match fs.create_dir(path) {
        Ok(()) => {}
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
        Err(e) => return Err(e),
    }
    fs.sync_dir(parent)
}

#[cfg(test)]
mod tests;
