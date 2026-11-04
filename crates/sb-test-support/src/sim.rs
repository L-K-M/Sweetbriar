//! `SimFs`: an in-memory `Fs` that models durability (spec/11 "Power
//! loss"). Every directory keeps its live entries and the entries its
//! last `sync_dir` made durable; every file keeps its live bytes and
//! the bytes its last `sync_all` made durable. `create`, `create_dir`,
//! `rename` and `remove` change live state only, so [`SimFs::power_cut`]
//! — which replaces all live state with durable state — undoes exactly
//! what was never flushed: an unsynced directory entry vanishes, a
//! synced entry with unsynced data comes back empty, and a rename whose
//! directories weren't flushed is rolled back.

use std::collections::{HashMap, HashSet};
use std::io::{self, Read, Seek, SeekFrom};
use std::path::{Component, Path, PathBuf};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};

use sb_types::fs::{
    ConfinedDir, DirectoryIdentity, FileKind, Fs, Lease, Location, LockError, Metadata, ReadFile,
    WriteFile,
};

/// An in-memory filesystem whose only ground truth is what was
/// flushed. Tests drive it exactly like a real one and then call
/// [`SimFs::power_cut`] to see what a power loss would have kept.
#[derive(Default)]
pub struct SimFs {
    root: Arc<Mutex<SimDir>>,
    locks: Arc<Mutex<HashSet<PathBuf>>>,
    /// Held by every operation on the tree, so each is atomic as on a
    /// real filesystem: no thread sees a rename half done.
    ops: Arc<Mutex<()>>,
}

/// A directory: the entries it holds now, and the entries its last
/// `sync_dir` snapshot recorded. Children are shared by handle, so a
/// snapshot and the live tree can point at one node.
struct SimDir {
    identity: u64,
    owner: Option<Vec<u8>>,
    durable_owner: Option<Vec<u8>>,
    live: HashMap<Box<Path>, Child>,
    durable: HashMap<Box<Path>, Child>,
}

static NEXT_DIR_ID: AtomicU64 = AtomicU64::new(1);

impl Default for SimDir {
    fn default() -> Self {
        let identity = NEXT_DIR_ID
            .try_update(Ordering::Relaxed, Ordering::Relaxed, |n| n.checked_add(1))
            .expect("sim directory identities exhausted");
        Self {
            identity,
            owner: None,
            durable_owner: None,
            live: HashMap::new(),
            durable: HashMap::new(),
        }
    }
}

/// One directory entry. `Dir` nodes are shared (`Arc`), because a
/// durable snapshot must keep pointing at the directory it froze, not
/// at a copy of it.
#[derive(Clone)]
enum Child {
    File(Arc<Mutex<SimFile>>),
    Dir(Arc<Mutex<SimDir>>),
}

/// A file: its live bytes, and the bytes its last `sync_all` froze.
/// Open write handles hold the node, so writes after a rename still
/// land in it.
#[derive(Default)]
struct SimFile {
    live: Vec<u8>,
    durable: Vec<u8>,
}

impl SimFs {
    pub fn new() -> Self {
        Self::default()
    }

    /// Simulated power loss: drop every unflushed write and directory
    /// entry, keeping only what the last flush of each file and
    /// directory had made durable. Anything reachable only through
    /// non-durable entries disappears.
    pub fn power_cut(&self) {
        let _op = self.op();
        cut_dir(&self.root);
    }

    /// Take the operation lock (see `ops`).
    fn op(&self) -> std::sync::MutexGuard<'_, ()> {
        self.ops
            .lock()
            .expect("a panicking thread held the sim operation lock")
    }

    /// Create `path` and every missing ancestor — a test convenience
    /// the `Fs` seam deliberately doesn't offer. The chain it walks is
    /// marked durable as it goes: it stands in for a directory tree
    /// that existed before the test started, which no power cut may
    /// take away.
    pub fn create_dir_all(&self, path: &Path) -> io::Result<()> {
        let _op = self.op();
        let path = normalise(path)?;
        let mut dir = self.root.clone();
        for name in relative_components(&path) {
            let existing = {
                let mut d = dir.lock().expect("a panicking thread held the sim tree");
                match d.live.get(&name) {
                    Some(Child::Dir(next)) => Arc::clone(next),
                    Some(Child::File(_)) => {
                        return Err(io::Error::from_raw_os_error(libc::ENOTDIR));
                    }
                    None => {
                        let next: Arc<Mutex<SimDir>> = Arc::default();
                        d.live.insert(name.clone(), Child::Dir(Arc::clone(&next)));
                        next
                    }
                }
            };
            {
                // Only this chain's entry: unrelated unflushed siblings
                // stay unflushed.
                let mut d = dir.lock().expect("a panicking thread held the sim tree");
                d.durable.insert(name, Child::Dir(Arc::clone(&existing)));
            }
            dir = existing;
        }
        Ok(())
    }

    /// The node at `path`, if it exists live.
    fn child(&self, path: &Path) -> Option<Child> {
        let path = normalise(path).ok()?;
        let mut dir = self.root.clone();
        let names = relative_components(&path).collect::<Vec<_>>();
        for (depth, name) in names.iter().enumerate() {
            let found = {
                let d = dir.lock().expect("a panicking thread held the sim tree");
                d.live.get(name).cloned()?
            };
            match found {
                Child::Dir(next) if depth + 1 < names.len() => dir = next,
                child if depth + 1 == names.len() => return Some(child),
                // A file in the middle of the path.
                _ => return None,
            }
        }
        None // the path is "/", which is not a child of anything
    }

    /// The live directory node at `path`.
    fn dir(&self, path: &Path) -> Option<Arc<Mutex<SimDir>>> {
        let path = normalise(path).ok()?;
        if relative_components(&path).next().is_none() {
            return Some(self.root.clone());
        }
        match self.child(&path)? {
            Child::Dir(dir) => Some(dir),
            Child::File(_) => None,
        }
    }

    /// Run `f` on the live entries of the directory `parent`.
    fn with_dir<R>(
        &self,
        parent: &Path,
        f: impl FnOnce(&mut SimDir) -> io::Result<R>,
    ) -> io::Result<R> {
        let dir = self
            .dir(parent)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        let mut d = dir.lock().expect("a panicking thread held the sim tree");
        f(&mut d)
    }
}

/// Roll one directory back to its durable entries, then recurse into
/// whatever survived. No two directory locks are held at once.
fn cut_dir(dir: &Mutex<SimDir>) {
    let children = {
        let mut d = dir.lock().expect("a panicking thread held the sim tree");
        d.live = d.durable.clone();
        d.owner = d.durable_owner.clone();
        d.live.values().cloned().collect::<Vec<_>>()
    };
    for child in children {
        match child {
            Child::Dir(dir) => cut_dir(&dir),
            Child::File(file) => {
                let mut f = file.lock().expect("a panicking thread held a sim file");
                f.live = f.durable.clone();
            }
        }
    }
}

/// The absolute, lexically normal form of `path`: `.` dropped, `..`
/// resolved. A relative path is an error, as on a real filesystem.
fn normalise(path: &Path) -> io::Result<PathBuf> {
    if !path.is_absolute() {
        return Err(io::Error::new(
            io::ErrorKind::InvalidInput,
            format!(
                "the sim filesystem needs absolute paths, not {}",
                path.display()
            ),
        ));
    }
    let mut out = PathBuf::from("/");
    for component in path.components() {
        match component {
            Component::Normal(name) => out.push(name),
            Component::CurDir => {}
            Component::ParentDir => {
                out.pop();
            }
            Component::RootDir | Component::Prefix(_) => {}
        }
    }
    Ok(out)
}

/// The components of an already-normalised absolute path.
fn relative_components(path: &Path) -> impl Iterator<Item = Box<Path>> {
    path.components()
        .filter_map(|c| match c {
            Component::Normal(name) => Some(Box::from(Path::new(name))),
            _ => None,
        })
        .collect::<Vec<_>>()
        .into_iter()
}

struct SimConfinedDir {
    dir: Arc<Mutex<SimDir>>,
    ops: Arc<Mutex<()>>,
}

fn confined_name(name: &Path) -> io::Result<Box<Path>> {
    let mut parts = name.components();
    match (parts.next(), parts.next()) {
        (Some(Component::Normal(name)), None) => Ok(Box::from(Path::new(name))),
        _ => Err(io::ErrorKind::InvalidInput.into()),
    }
}

impl ConfinedDir for SimConfinedDir {
    fn set_ownership(&self, bytes: &[u8]) -> io::Result<()> {
        if bytes.is_empty() || bytes.len() > 36 {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let _op = self.ops.lock().expect("sim operation lock");
        let mut dir = self.dir.lock().expect("sim tree lock");
        if dir.owner.is_some() {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        dir.owner = Some(bytes.to_vec());
        Ok(())
    }
    fn ownership(&self) -> io::Result<Vec<u8>> {
        let _op = self.ops.lock().expect("sim operation lock");
        self.dir
            .lock()
            .expect("sim tree lock")
            .owner
            .clone()
            .ok_or(io::ErrorKind::NotFound.into())
    }
    fn identity(&self) -> io::Result<DirectoryIdentity> {
        Ok(DirectoryIdentity {
            device: 0,
            inode: self.dir.lock().expect("sim tree lock").identity,
        })
    }
    fn open_dir(&self, name: &Path) -> io::Result<Box<dyn ConfinedDir>> {
        let name = confined_name(name)?;
        let _op = self.ops.lock().expect("sim operation lock");
        let child = self
            .dir
            .lock()
            .expect("sim tree lock")
            .live
            .get(&name)
            .cloned();
        match child {
            Some(Child::Dir(dir)) => Ok(Box::new(Self {
                dir,
                ops: self.ops.clone(),
            })),
            _ => Err(io::ErrorKind::NotFound.into()),
        }
    }
    fn create_private_dir(&self, name: &Path) -> io::Result<()> {
        let name = confined_name(name)?;
        let _op = self.ops.lock().expect("sim operation lock");
        let mut dir = self.dir.lock().expect("sim tree lock");
        if dir.live.contains_key(&name) {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        dir.live
            .insert(name, Child::Dir(Arc::new(Mutex::new(SimDir::default()))));
        Ok(())
    }
    fn create(&self, name: &Path) -> io::Result<Box<dyn WriteFile>> {
        let name = confined_name(name)?;
        let _op = self.ops.lock().expect("sim operation lock");
        let mut dir = self.dir.lock().expect("sim tree lock");
        if dir.live.contains_key(&name) {
            return Err(io::ErrorKind::AlreadyExists.into());
        }
        let node = Arc::new(Mutex::new(SimFile::default()));
        dir.live.insert(name, Child::File(node.clone()));
        Ok(Box::new(SimWriteFile { node }))
    }
    fn remove_file(&self, name: &Path) -> io::Result<()> {
        let name = confined_name(name)?;
        let _op = self.ops.lock().expect("sim operation lock");
        let mut dir = self.dir.lock().expect("sim tree lock");
        match dir.live.get(&name) {
            Some(Child::File(_)) => {
                dir.live.remove(&name);
                Ok(())
            }
            Some(Child::Dir(_)) => Err(io::ErrorKind::IsADirectory.into()),
            None => Err(io::ErrorKind::NotFound.into()),
        }
    }
    fn remove_empty_dir(&self, name: &Path) -> io::Result<()> {
        let name = confined_name(name)?;
        let _op = self.ops.lock().expect("sim operation lock");
        let mut dir = self.dir.lock().expect("sim tree lock");
        match dir.live.get(&name) {
            Some(Child::Dir(child)) if child.lock().expect("sim tree lock").live.is_empty() => {
                dir.live.remove(&name);
                Ok(())
            }
            Some(Child::Dir(_)) => Err(io::Error::other("directory not empty")),
            Some(Child::File(_)) => Err(io::ErrorKind::NotADirectory.into()),
            None => Err(io::ErrorKind::NotFound.into()),
        }
    }
    fn sync(&self) -> io::Result<()> {
        let _op = self.ops.lock().expect("sim operation lock");
        let mut dir = self.dir.lock().expect("sim tree lock");
        dir.durable = dir.live.clone();
        dir.durable_owner = dir.owner.clone();
        Ok(())
    }
    fn open_regular(&self, relative: &Path) -> io::Result<(Box<dyn ReadFile>, Metadata)> {
        let _op = self
            .ops
            .lock()
            .expect("a panicking thread held the sim operation lock");
        let parts: Vec<_> = relative
            .components()
            .map(|part| match part {
                Component::Normal(name) => Ok(Box::<Path>::from(Path::new(name))),
                _ => Err(io::Error::from(io::ErrorKind::InvalidInput)),
            })
            .collect::<io::Result<_>>()?;
        let (last, parents) = parts.split_last().ok_or(io::ErrorKind::InvalidInput)?;
        let mut dir = self.dir.clone();
        for name in parents {
            let next = dir
                .lock()
                .expect("a panicking thread held the sim tree")
                .live
                .get(name)
                .cloned();
            match next {
                Some(Child::Dir(child)) => dir = child,
                _ => return Err(io::ErrorKind::NotFound.into()),
            }
        }
        let child = dir
            .lock()
            .expect("a panicking thread held the sim tree")
            .live
            .get(last)
            .cloned();
        match child {
            Some(Child::File(node)) => {
                let len = node
                    .lock()
                    .expect("a panicking thread held a sim file")
                    .live
                    .len() as u64;
                Ok((
                    Box::new(SimReadFile { node, pos: 0 }),
                    Metadata::new(FileKind::File, len),
                ))
            }
            Some(Child::Dir(_)) => Err(not_regular(relative)),
            None => Err(io::ErrorKind::NotFound.into()),
        }
    }
}

impl Fs for SimFs {
    fn open_confined_dir(&self, path: &Path) -> io::Result<Box<dyn ConfinedDir>> {
        if !path.is_absolute()
            || path
                .components()
                .any(|part| matches!(part, Component::ParentDir | Component::Prefix(_)))
        {
            return Err(io::ErrorKind::InvalidInput.into());
        }
        let _op = self.op();
        let dir = self.dir(path).ok_or(io::ErrorKind::NotFound)?;
        Ok(Box::new(SimConfinedDir {
            dir,
            ops: self.ops.clone(),
        }))
    }

    fn create(&self, path: &Path) -> io::Result<Box<dyn WriteFile>> {
        let _op = self.op();
        let path = normalise(path)?;
        let name = file_name(&path)?;
        let node = Arc::new(Mutex::new(SimFile::default()));
        self.with_dir(path.parent().unwrap_or(Path::new("/")), |dir| {
            if dir.live.contains_key(&name) {
                return Err(io::Error::from(io::ErrorKind::AlreadyExists));
            }
            dir.live.insert(name, Child::File(node.clone()));
            Ok(())
        })?;
        Ok(Box::new(SimWriteFile { node }))
    }

    fn open(&self, path: &Path) -> io::Result<Box<dyn sb_types::fs::ReadFile>> {
        let _op = self.op();
        let path = normalise(path)?;
        match self.child(&path) {
            Some(Child::File(node)) => Ok(Box::new(SimReadFile { node, pos: 0 })),
            _ => Err(io::Error::from(io::ErrorKind::NotFound)),
        }
    }

    fn open_regular(&self, path: &Path) -> io::Result<(Box<dyn sb_types::fs::ReadFile>, Metadata)> {
        // One op-lock hold resolves the node and reads its length, so
        // the handle and the metadata it reports are one atomic look,
        // as `RealFs`'s open-then-fstat is.
        let _op = self.op();
        let path = normalise(path)?;
        if relative_components(&path).next().is_none() {
            return Err(not_regular(&path)); // "/" is a directory
        }
        match self.child(&path) {
            Some(Child::File(node)) => {
                let len = node
                    .lock()
                    .expect("a panicking thread held a sim file")
                    .live
                    .len() as u64;
                Ok((
                    Box::new(SimReadFile { node, pos: 0 }),
                    Metadata::new(FileKind::File, len),
                ))
            }
            Some(Child::Dir(_)) => Err(not_regular(&path)),
            None => Err(io::Error::from(io::ErrorKind::NotFound)),
        }
    }

    fn metadata(&self, path: &Path) -> io::Result<Option<Metadata>> {
        let _op = self.op();
        let path = normalise(path)?;
        if relative_components(&path).next().is_none() {
            return Ok(Some(Metadata::new(FileKind::Directory, self.root_len())));
        }
        Ok(match self.child(&path) {
            Some(Child::File(node)) => {
                let file = node.lock().expect("a panicking thread held a sim file");
                Some(Metadata::new(FileKind::File, file.live.len() as u64))
            }
            Some(Child::Dir(dir)) => {
                let d = dir.lock().expect("a panicking thread held the sim tree");
                Some(Metadata::new(FileKind::Directory, d.live.len() as u64))
            }
            None => None,
        })
    }

    fn read_dir(&self, path: &Path) -> io::Result<Vec<PathBuf>> {
        let _op = self.op();
        let path = normalise(path)?;
        let dir = self
            .dir(&path)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        let d = dir.lock().expect("a panicking thread held the sim tree");
        let mut entries: Vec<PathBuf> = d.live.keys().map(|name| path.join(name)).collect();
        entries.sort();
        Ok(entries)
    }

    fn create_dir(&self, path: &Path) -> io::Result<()> {
        let _op = self.op();
        let path = normalise(path)?;
        let name = file_name(&path)?;
        self.with_dir(path.parent().unwrap_or(Path::new("/")), |dir| {
            if dir.live.contains_key(&name) {
                return Err(io::Error::from(io::ErrorKind::AlreadyExists));
            }
            dir.live.insert(name, Child::Dir(Arc::default()));
            Ok(())
        })
    }

    fn create_private_dir(&self, path: &Path) -> io::Result<()> {
        self.create_dir(path)
    }

    fn restrict_dir(&self, path: &Path) -> io::Result<()> {
        let _op = self.op();
        self.dir(path)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        Ok(())
    }

    fn remove_dir_all(&self, path: &Path) -> io::Result<()> {
        let _op = self.op();
        let path = normalise(path)?;
        match self.child(&path) {
            None => return Ok(()),
            Some(Child::File(_)) => return Err(io::ErrorKind::NotADirectory.into()),
            Some(Child::Dir(_)) => {}
        }
        let name = file_name(&path)?;
        self.with_dir(path.parent().unwrap_or(Path::new("/")), |dir| {
            dir.live.remove(&name);
            Ok(())
        })
    }

    fn quarantine(&self, path: &Path) -> io::Result<()> {
        self.open_regular(path)?;
        Ok(())
    }

    fn sync_dir(&self, path: &Path) -> io::Result<()> {
        let _op = self.op();
        let path = normalise(path)?;
        let dir = self
            .dir(&path)
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        let mut d = dir.lock().expect("a panicking thread held the sim tree");
        // The snapshot shares child handles: later changes under a
        // synced subdirectory are that directory's business, not this
        // one's.
        d.durable = d.live.clone();
        Ok(())
    }

    fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
        let _op = self.op();
        let from = normalise(from)?;
        let to = normalise(to)?;
        let from_name = file_name(&from)?;
        let to_name = file_name(&to)?;
        if from == to {
            return Ok(());
        }
        let from_dir = self
            .dir(from.parent().unwrap_or(Path::new("/")))
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;
        let to_dir = self
            .dir(to.parent().unwrap_or(Path::new("/")))
            .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?;

        // Validate everything before changing anything, so a failed
        // rename leaves both entries as they were, as a real one does.
        // Directory locks are taken one at a time.
        let child = {
            let d = from_dir
                .lock()
                .expect("a panicking thread held the sim tree");
            d.live
                .get(&from_name)
                .cloned()
                .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))?
        };
        {
            let d = to_dir.lock().expect("a panicking thread held the sim tree");
            match d.live.get(&to_name) {
                Some(Child::Dir(existing)) if matches!(child, Child::Dir(_)) => {
                    let existing = existing
                        .lock()
                        .expect("a panicking thread held the sim tree");
                    if !existing.live.is_empty() {
                        return Err(io::Error::other("rename target directory not empty"));
                    }
                }
                Some(Child::Dir(_)) => {
                    return Err(io::Error::from(io::ErrorKind::IsADirectory));
                }
                Some(Child::File(_)) if matches!(child, Child::Dir(_)) => {
                    return Err(io::Error::from(io::ErrorKind::NotADirectory));
                }
                _ => {}
            }
        }

        from_dir
            .lock()
            .expect("a panicking thread held the sim tree")
            .live
            .remove(&from_name);
        to_dir
            .lock()
            .expect("a panicking thread held the sim tree")
            .live
            .insert(to_name, child);
        Ok(())
    }

    fn remove(&self, path: &Path) -> io::Result<()> {
        let _op = self.op();
        let path = normalise(path)?;
        let name = file_name(&path)?;
        self.with_dir(path.parent().unwrap_or(Path::new("/")), |dir| {
            match dir.live.get(&name) {
                Some(Child::File(_)) => {
                    dir.live.remove(&name);
                    Ok(())
                }
                Some(Child::Dir(_)) => Err(io::Error::from(io::ErrorKind::IsADirectory)),
                None => Err(io::Error::from(io::ErrorKind::NotFound)),
            }
        })
    }

    fn canonicalize(&self, path: &Path) -> io::Result<PathBuf> {
        let _op = self.op();
        let path = normalise(path)?;
        if self.child(&path).is_some() || relative_components(&path).next().is_none() {
            Ok(path)
        } else {
            Err(io::Error::from(io::ErrorKind::NotFound))
        }
    }

    fn lock_exclusive(&self, path: &Path) -> Result<Box<dyn Lease>, LockError> {
        let path = normalise(path).map_err(LockError::Io)?;
        let mut locks = self
            .locks
            .lock()
            .expect("a panicking thread held the sim locks");
        if locks.contains(&path) {
            return Err(LockError::Held);
        }
        locks.insert(path.clone());
        Ok(Box::new(SimLease {
            path,
            locks: self.locks.clone(),
        }))
    }

    fn location(&self, _path: &Path) -> io::Result<Location> {
        Ok(Location::Local)
    }
}

impl SimFs {
    fn root_len(&self) -> u64 {
        let root = self
            .root
            .lock()
            .expect("a panicking thread held the sim tree");
        root.live.len() as u64
    }
}

/// The `open_regular` refusal: `path` names a directory (the sim has
/// no other non-file kinds), not a regular file.
fn not_regular(path: &Path) -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidInput,
        format!("{} is not a regular file", path.display()),
    )
}

/// The final component of a normalised path; only the root has none.
fn file_name(path: &Path) -> io::Result<Box<Path>> {
    path.file_name()
        .map(Path::new)
        .map(Box::from)
        .ok_or_else(|| io::Error::from(io::ErrorKind::NotFound))
}

/// A held in-memory lock; dropping it forgets the path.
struct SimLease {
    path: PathBuf,
    locks: Arc<Mutex<HashSet<PathBuf>>>,
}

impl Drop for SimLease {
    fn drop(&mut self) {
        self.locks
            .lock()
            .expect("a panicking thread held the sim locks")
            .remove(&self.path);
    }
}

/// A write handle to a sim file; it holds the node, not the path, so
/// writes after a rename still land in the file.
struct SimWriteFile {
    node: Arc<Mutex<SimFile>>,
}

impl io::Write for SimWriteFile {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let mut file = self
            .node
            .lock()
            .expect("a panicking thread held a sim file");
        file.live.extend_from_slice(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

impl WriteFile for SimWriteFile {
    fn sync_all(&mut self) -> io::Result<()> {
        let mut file = self
            .node
            .lock()
            .expect("a panicking thread held a sim file");
        file.durable = file.live.clone();
        Ok(())
    }
}

/// A read handle over a sim file's live bytes.
struct SimReadFile {
    node: Arc<Mutex<SimFile>>,
    pos: u64,
}

impl Read for SimReadFile {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let file = self
            .node
            .lock()
            .expect("a panicking thread held a sim file");
        let pos = self.pos.min(file.live.len() as u64) as usize;
        let n = buf.len().min(file.live.len() - pos);
        buf[..n].copy_from_slice(&file.live[pos..pos + n]);
        self.pos = (pos + n) as u64;
        Ok(n)
    }
}

impl Seek for SimReadFile {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let len = {
            let file = self
                .node
                .lock()
                .expect("a panicking thread held a sim file");
            file.live.len() as i128
        };
        let target = match from {
            SeekFrom::Start(offset) => offset as i128,
            SeekFrom::End(offset) => len + offset as i128,
            SeekFrom::Current(offset) => self.pos as i128 + offset as i128,
        };
        if target < 0 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidInput,
                "seek before the start of the file",
            ));
        }
        self.pos = target as u64;
        Ok(self.pos)
    }
}

#[cfg(test)]
mod tests {
    #[test]
    fn confined_scratch_operations_survive_power_cut_and_refuse_nonempty_removal() {
        use super::*;
        use std::io::Write as _;
        let fs = SimFs::new();
        fs.create_dir_all(Path::new("/root")).unwrap();
        let root = fs.open_confined_dir(Path::new("/root")).unwrap();
        root.create_private_dir(Path::new("scratch")).unwrap();
        root.sync().unwrap();
        let scratch = root.open_dir(Path::new("scratch")).unwrap();
        let mut file = scratch.create(Path::new("state.sqlite")).unwrap();
        file.write_all(b"data").unwrap();
        file.sync_all().unwrap();
        scratch.sync().unwrap();
        fs.power_cut();
        assert!(root.remove_empty_dir(Path::new("scratch")).is_err());
        let mut data = String::new();
        scratch
            .open_regular(Path::new("state.sqlite"))
            .unwrap()
            .0
            .read_to_string(&mut data)
            .unwrap();
        assert_eq!(data, "data");
        scratch.remove_file(Path::new("state.sqlite")).unwrap();
        scratch.sync().unwrap();
        root.remove_empty_dir(Path::new("scratch")).unwrap();
        root.sync().unwrap();
        fs.power_cut();
        assert!(root.open_dir(Path::new("scratch")).is_err());
    }

    #[test]
    fn confined_directory_survives_rename_and_refuses_escape() {
        use super::*;
        let fs = SimFs::new();
        fs.create_dir_all(Path::new("/root/child")).unwrap();
        use std::io::Write as _;
        fs.create(Path::new("/root/child/file"))
            .unwrap()
            .write_all(b"old")
            .unwrap();
        let root = fs.open_confined_dir(Path::new("/root")).unwrap();
        assert!(root.open_regular(Path::new("../outside")).is_err());
        fs.rename(Path::new("/root"), Path::new("/moved")).unwrap();
        fs.create_dir(Path::new("/root")).unwrap();
        let mut content = String::new();
        root.open_regular(Path::new("child/file"))
            .unwrap()
            .0
            .read_to_string(&mut content)
            .unwrap();
        assert_eq!(content, "old");
    }

    use std::io::Write as _;

    use super::*;

    /// Build `/a` durably (each level synced) and hand back the fs.
    fn durable_dir() -> SimFs {
        let fs = SimFs::new();
        fs.create_dir(Path::new("/a")).unwrap();
        fs.sync_dir(Path::new("/")).unwrap();
        fs.sync_dir(Path::new("/a")).unwrap();
        fs
    }

    /// What `put` flushes after writing.
    #[derive(Clone, Copy)]
    enum Flush {
        Nothing,
        File,
        Dir,
        FileAndDir,
    }

    /// Write `bytes` to a new file at `path`, then flush as told.
    fn put(fs: &SimFs, path: &Path, bytes: &[u8], flush: Flush) {
        let mut file = fs.create(path).unwrap();
        file.write_all(bytes).unwrap();
        if matches!(flush, Flush::File | Flush::FileAndDir) {
            file.sync_all().unwrap();
        }
        drop(file);
        if matches!(flush, Flush::Dir | Flush::FileAndDir) {
            fs.sync_dir(path.parent().unwrap()).unwrap();
        }
    }

    /// A failed rename changes nothing: the source stays where it was,
    /// whether the destination's directory is missing or the
    /// destination is an incompatible directory.
    #[test]
    fn a_failed_rename_keeps_its_source() {
        let fs = durable_dir();
        put(&fs, Path::new("/a/f"), b"kept", Flush::FileAndDir);
        fs.create_dir(Path::new("/a/d")).unwrap();
        for to in ["/missing/f", "/a/d"] {
            assert!(fs.rename(Path::new("/a/f"), Path::new(to)).is_err(), "{to}");
            let mut read = String::new();
            fs.open(Path::new("/a/f"))
                .unwrap()
                .read_to_string(&mut read)
                .unwrap();
            assert_eq!(read, "kept", "a rename to {to} lost its source");
        }
    }

    /// `create_dir_all` makes only its own chain durable: an unflushed
    /// sibling it walks past still vanishes in a cut.
    #[test]
    fn create_dir_all_leaves_unflushed_siblings_unflushed() {
        let fs = durable_dir();
        put(&fs, Path::new("/a/sibling"), b"x", Flush::File);
        fs.create_dir_all(Path::new("/a/b/c")).unwrap();
        fs.power_cut();
        assert!(fs.metadata(Path::new("/a/b/c")).unwrap().is_some());
        assert!(fs.metadata(Path::new("/a/sibling")).unwrap().is_none());
    }

    /// A synced file survives a power cut.
    #[test]
    fn a_synced_file_survives_a_cut() {
        let fs = durable_dir();
        put(&fs, Path::new("/a/f"), b"synced", Flush::FileAndDir);
        fs.power_cut();
        let mut read = String::new();
        fs.open(Path::new("/a/f"))
            .unwrap()
            .read_to_string(&mut read)
            .unwrap();
        assert_eq!(read, "synced");
    }

    /// An unsynced directory entry vanishes: the file was never
    /// reachable through durable entries.
    #[test]
    fn an_unsynced_entry_vanishes() {
        let fs = durable_dir();
        put(&fs, Path::new("/a/f"), b"lost", Flush::File);
        fs.power_cut();
        assert_eq!(fs.metadata(Path::new("/a/f")).unwrap(), None);
    }

    /// A rename whose directories were never flushed is undone by the
    /// cut: the file reappears under its old name.
    #[test]
    fn a_rename_without_the_directory_sync_is_undone() {
        let fs = durable_dir();
        put(&fs, Path::new("/a/f"), b"old", Flush::FileAndDir);
        fs.rename(Path::new("/a/f"), Path::new("/a/g")).unwrap();
        fs.power_cut();
        assert_eq!(fs.metadata(Path::new("/a/g")).unwrap(), None);
        let mut read = String::new();
        fs.open(Path::new("/a/f"))
            .unwrap()
            .read_to_string(&mut read)
            .unwrap();
        assert_eq!(read, "old");
    }

    /// A durable entry with unsynced data comes back as an empty file:
    /// the name was flushed, the bytes were not.
    #[test]
    fn a_synced_entry_with_unsynced_data_comes_back_empty() {
        let fs = durable_dir();
        put(&fs, Path::new("/a/f"), b"never flushed", Flush::Dir);
        fs.power_cut();
        assert_eq!(
            fs.metadata(Path::new("/a/f")).unwrap().unwrap().len(),
            0,
            "the unsynced bytes survived the cut"
        );
    }

    /// Locks work like the real seam: held is held until dropped.
    #[test]
    fn locks_are_exclusive_until_dropped() {
        let fs = SimFs::new();
        let lease = fs.lock_exclusive(Path::new("/x.lock")).unwrap();
        assert!(matches!(
            fs.lock_exclusive(Path::new("/x.lock")),
            Err(LockError::Held)
        ));
        drop(lease);
        assert!(fs.lock_exclusive(Path::new("/x.lock")).is_ok());
    }

    /// A read handle sees live (possibly unsynced) bytes, and seeking
    /// behaves like a real file.
    #[test]
    fn reads_see_live_bytes_and_seek_like_a_file() {
        let fs = durable_dir();
        put(&fs, Path::new("/a/f"), b"0123456789", Flush::Nothing);
        let mut file = fs.open(Path::new("/a/f")).unwrap();
        assert_eq!(file.seek(SeekFrom::End(-2)).unwrap(), 8);
        let mut tail = String::new();
        file.read_to_string(&mut tail).unwrap();
        assert_eq!(tail, "89");
        file.seek(SeekFrom::Start(0)).unwrap();
        let mut all = String::new();
        file.read_to_string(&mut all).unwrap();
        assert_eq!(all, "0123456789");
    }

    /// `open_regular` hands back the node and its length in one look:
    /// a rename afterwards rebinds neither the handle nor the length
    /// it reported.
    #[test]
    fn open_regular_binds_the_handle_and_len_to_the_node() {
        let fs = durable_dir();
        put(&fs, Path::new("/a/f"), b"kept", Flush::FileAndDir);
        let (mut handle, md) = fs.open_regular(Path::new("/a/f")).unwrap();
        assert_eq!(md.kind(), FileKind::File);
        assert_eq!(md.len(), 4);

        fs.rename(Path::new("/a/f"), Path::new("/a/g")).unwrap();
        put(&fs, Path::new("/a/f"), b"new!", Flush::FileAndDir);

        let mut read = String::new();
        handle.read_to_string(&mut read).unwrap();
        assert_eq!(read, "kept");
        assert_eq!(md.len(), 4);
    }

    /// Directories and missing paths are refused, not opened; the
    /// sim's only non-file kind is the directory.
    #[test]
    fn open_regular_refuses_non_files_and_missing_paths() {
        let fs = durable_dir();
        for path in ["/a", "/"] {
            let err = fs.open_regular(Path::new(path)).err().unwrap();
            assert_eq!(err.kind(), io::ErrorKind::InvalidInput, "{path}");
        }
        let missing = fs.open_regular(Path::new("/a/missing"));
        assert_eq!(missing.err().unwrap().kind(), io::ErrorKind::NotFound);
    }
}
