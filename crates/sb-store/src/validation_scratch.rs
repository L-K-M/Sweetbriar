//! Disposable, bounded workspace for import validation.
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::StoreError;
use sb_types::fs::{Fs, ReadFile, WriteFile};
use sb_types::random::Random;

pub(crate) struct ScratchOwner {
    fs: Arc<dyn Fs>,
    random: Arc<dyn Random>,
    root: PathBuf,
    setup: Mutex<()>,
}

/// One of the two reusable validation tapes.
#[derive(Clone, Copy)]
pub enum ValidationTape {
    A,
    B,
}

/// A private disposable workspace borrowed from an open store.
pub struct ValidationScratch<'a> {
    owner: &'a ScratchOwner,
    dir: PathBuf,
    finished: bool,
}

fn error(action: &str, path: &Path, e: io::Error) -> StoreError {
    StoreError::unavailable_io(
        format!(
            "cannot {action} import validation scratch {}: {e}",
            path.display()
        ),
        e.kind(),
    )
}

impl ScratchOwner {
    pub(crate) fn new(fs: Arc<dyn Fs>, random: Arc<dyn Random>, tmp: &Path) -> Self {
        Self {
            fs,
            random,
            root: tmp.join("import-validation"),
            setup: Mutex::new(()),
        }
    }

    pub(crate) fn cleanup(fs: &dyn Fs, tmp: &Path) -> Result<(), StoreError> {
        let root = tmp.join("import-validation");
        fs.remove_dir_all(&root)
            .map_err(|e| error("remove", &root, e))
    }

    pub(crate) fn lease(&self) -> Result<ValidationScratch<'_>, StoreError> {
        let _guard = self
            .setup
            .lock()
            .map_err(|_| StoreError::internal("scratch setup lock poisoned"))?;
        match self.fs.create_private_dir(&self.root) {
            Ok(()) => (),
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => self
                .fs
                .restrict_dir(&self.root)
                .map_err(|e| error("secure", &self.root, e))?,
            Err(e) => return Err(error("create", &self.root, e)),
        }
        for _ in 0..8 {
            let mut bytes = [0; 16];
            self.random
                .fill(&mut bytes)
                .map_err(|e| error("name", &self.root, e))?;
            let mut token = String::with_capacity(32);
            for byte in bytes {
                let _ = write!(token, "{byte:02x}");
            }
            let dir = self.root.join(token);
            match self.fs.create_private_dir(&dir) {
                Ok(()) => {
                    return Ok(ValidationScratch {
                        owner: self,
                        dir,
                        finished: false,
                    });
                }
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => return Err(error("create", &dir, e)),
            }
        }
        Err(StoreError::unavailable(
            "cannot allocate unique import validation scratch directory",
        ))
    }
}

impl ValidationScratch<'_> {
    fn path(&self, tape: ValidationTape) -> PathBuf {
        self.dir.join(match tape {
            ValidationTape::A => "a",
            ValidationTape::B => "b",
        })
    }
    /// Create an empty tape exclusively. The writer exposes checked writes and sync.
    pub fn create(&self, tape: ValidationTape) -> Result<Box<dyn WriteFile>, StoreError> {
        let path = self.path(tape);
        self.owner
            .fs
            .create(&path)
            .map_err(|e| error("create tape", &path, e))
    }
    /// Open an existing tape for reading and seeking.
    pub fn open(&self, tape: ValidationTape) -> Result<Box<dyn ReadFile>, StoreError> {
        let path = self.path(tape);
        self.owner
            .fs
            .open_no_follow(&path)
            .map_err(|e| error("open tape", &path, e))
    }
    /// Remove a tape, treating an absent tape as already removed.
    pub fn remove(&self, tape: ValidationTape) -> Result<(), StoreError> {
        let path = self.path(tape);
        match self.owner.fs.remove(&path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(error("remove tape", &path, e)),
        }
    }
    /// Remove the workspace explicitly, reporting cleanup failure.
    pub fn finish(mut self) -> Result<(), StoreError> {
        self.owner
            .fs
            .remove_dir_all(&self.dir)
            .map_err(|e| error("remove", &self.dir, e))?;
        self.finished = true;
        Ok(())
    }
}
impl Drop for ValidationScratch<'_> {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.owner.fs.remove_dir_all(&self.dir);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sb_test_support::{FaultFs, FaultKind, FsOp, ScratchDir, SeededRandom, real_fs};
    use std::io::{Read, Seek, SeekFrom, Write};

    fn owner(root: &Path, fs: Arc<dyn Fs>) -> ScratchOwner {
        ScratchOwner::new(fs, Arc::new(SeededRandom::new(42)), root)
    }

    #[test]
    fn tapes_are_isolated_reusable_and_cleaned() {
        let tmp = ScratchDir::new("validation-tapes");
        let owner = owner(tmp.path(), real_fs());
        let a = owner.lease().unwrap();
        let b = owner.lease().unwrap();
        let mut writer = a.create(ValidationTape::A).unwrap();
        writer.write_all(b"abc").unwrap();
        writer.sync_all().unwrap();
        drop(writer);
        assert!(b.open(ValidationTape::A).is_err());
        let mut reader = a.open(ValidationTape::A).unwrap();
        reader.seek(SeekFrom::Start(1)).unwrap();
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"bc");
        a.remove(ValidationTape::A).unwrap();
        a.remove(ValidationTape::A).unwrap();
        a.create(ValidationTape::A).unwrap();
        a.finish().unwrap();
        let dir = b.dir.clone();
        drop(b);
        assert!(!dir.exists());
    }

    #[test]
    fn errors_are_surfaced_and_cleanup_is_idempotent() {
        for op in [
            FsOp::Create,
            FsOp::Open,
            FsOp::Remove,
            FsOp::SyncFile,
            FsOp::RemoveDirAll,
        ] {
            let tmp = ScratchDir::new("validation-fault");
            let fs = Arc::new(FaultFs::new(real_fs(), op, 1, FaultKind::Eio));
            let owner = owner(tmp.path(), fs);
            let lease = owner.lease().unwrap();
            match op {
                FsOp::Create => assert!(lease.create(ValidationTape::A).is_err()),
                FsOp::Open => assert!(lease.open(ValidationTape::A).is_err()),
                FsOp::Remove => assert!(lease.remove(ValidationTape::A).is_err()),
                FsOp::SyncFile => {
                    assert!(lease.create(ValidationTape::A).unwrap().sync_all().is_err())
                }
                FsOp::RemoveDirAll => {
                    assert!(lease.finish().is_err());
                    continue;
                }
                _ => unreachable!(),
            }
            lease.finish().unwrap();
            ScratchOwner::cleanup(real_fs().as_ref(), tmp.path()).unwrap();
            ScratchOwner::cleanup(real_fs().as_ref(), tmp.path()).unwrap();
        }
    }

    #[test]
    fn collision_fails_closed() {
        let tmp = ScratchDir::new("validation-collision");
        let first = owner(tmp.path(), real_fs());
        let second = owner(tmp.path(), real_fs());
        let _held = first.lease().unwrap();
        // Both seeded streams first collide, then the second retries with a new token.
        let other = second.lease().unwrap();
        assert_ne!(_held.dir, other.dir);
    }

    #[test]
    fn exhausted_collisions_preserve_the_existing_workspace() {
        struct ConstantRandom(std::sync::atomic::AtomicUsize);
        impl Random for ConstantRandom {
            fn fill(&self, dest: &mut [u8]) -> io::Result<()> {
                self.0.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
                dest.fill(0);
                Ok(())
            }
        }
        let tmp = ScratchDir::new("validation-collision-exhaustion");
        let random = Arc::new(ConstantRandom(std::sync::atomic::AtomicUsize::new(0)));
        let owner = ScratchOwner::new(real_fs(), random.clone(), tmp.path());
        let held = owner.lease().unwrap();
        held.create(ValidationTape::A)
            .unwrap()
            .write_all(b"keep")
            .unwrap();
        assert!(matches!(owner.lease(), Err(StoreError::Unavailable { .. })));
        assert_eq!(random.0.load(std::sync::atomic::Ordering::Relaxed), 9);
        let mut bytes = Vec::new();
        held.open(ValidationTape::A)
            .unwrap()
            .read_to_end(&mut bytes)
            .unwrap();
        assert_eq!(bytes, b"keep");
        held.finish().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn tape_open_refuses_symlinks() {
        let tmp = ScratchDir::new("validation-symlink");
        let owner = owner(tmp.path(), real_fs());
        let lease = owner.lease().unwrap();
        let target = tmp.path().join("target");
        std::fs::write(&target, b"outside").unwrap();
        std::os::unix::fs::symlink(&target, lease.path(ValidationTape::A)).unwrap();
        assert!(lease.open(ValidationTape::A).is_err());
        lease.finish().unwrap();
        assert_eq!(std::fs::read(target).unwrap(), b"outside");
    }
}
