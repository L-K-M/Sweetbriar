//! Disposable, bounded workspace for import validation.
use std::fmt::Write as _;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

use crate::{ImportSourceSnapshot, StoreError};
use sb_types::fs::{Fs, ReadFile, WriteFile};
use sb_types::random::Random;

pub(crate) struct ScratchOwner {
    fs: Arc<dyn Fs>,
    random: Arc<dyn Random>,
    root: PathBuf,
    setup: Mutex<()>,
}

/// One of the two reusable validation sort tapes.
#[derive(Clone, Copy)]
pub enum ValidationTape {
    /// First reusable sorting tape.
    A,
    /// Second reusable sorting tape.
    B,
}

/// Fixed proof files which can coexist with both reusable sort tapes.
/// They are disposable workspace, not durable evidence.
#[derive(Clone, Copy)]
pub enum ValidationFile {
    /// Fixed-size descriptors in grouped order.
    Descriptors,
    /// Unsorted file-ordinal range rows.
    IndexSpool,
    /// Dense file-ordinal lookup slots.
    Lookup,
}

/// Fixed files in one validated snapshot's private registry namespace.
/// They are disposable frozen proof, not durable evidence.
#[derive(Clone, Copy)]
pub enum RegistryFile {
    /// Fixed-size descriptors in grouped order.
    Descriptors,
    /// Dense file-ordinal lookup slots.
    Lookup,
    /// The identity header, created last as the ready marker.
    Header,
}

/// A private disposable workspace owned by its caller.
pub struct ValidationScratch {
    fs: Arc<dyn Fs>,
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

    pub(crate) fn lease(&self) -> Result<ValidationScratch, StoreError> {
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
                        fs: Arc::clone(&self.fs),
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

impl ValidationScratch {
    fn path(&self, tape: ValidationTape) -> PathBuf {
        self.dir.join(match tape {
            ValidationTape::A => "a",
            ValidationTape::B => "b",
        })
    }
    fn file_path(&self, file: ValidationFile) -> PathBuf {
        self.dir.join(match file {
            ValidationFile::Descriptors => "descriptors",
            ValidationFile::IndexSpool => "index-spool",
            ValidationFile::Lookup => "lookup",
        })
    }
    fn registry_dir(&self, snapshot: &ImportSourceSnapshot) -> PathBuf {
        self.dir.join(snapshot.token())
    }
    fn registry_file_path(&self, snapshot: &ImportSourceSnapshot, file: RegistryFile) -> PathBuf {
        self.registry_dir(snapshot).join(match file {
            RegistryFile::Descriptors => "descriptors",
            RegistryFile::Lookup => "lookup",
            RegistryFile::Header => "header",
        })
    }
    /// Create an empty tape exclusively. The writer exposes checked writes and sync.
    pub fn create(&self, tape: ValidationTape) -> Result<Box<dyn WriteFile>, StoreError> {
        let path = self.path(tape);
        self.fs
            .create(&path)
            .map_err(|e| error("create tape", &path, e))
    }
    /// Open an existing tape for reading and seeking.
    pub fn open(&self, tape: ValidationTape) -> Result<Box<dyn ReadFile>, StoreError> {
        let path = self.path(tape);
        self.fs
            .open_no_follow(&path)
            .map_err(|e| error("open tape", &path, e))
    }
    /// Remove a tape, treating an absent tape as already removed.
    pub fn remove(&self, tape: ValidationTape) -> Result<(), StoreError> {
        self.remove_path(&self.path(tape))
    }
    /// Create a fixed proof file exclusively within this lease.
    pub fn create_file(&self, file: ValidationFile) -> Result<Box<dyn WriteFile>, StoreError> {
        let path = self.file_path(file);
        self.fs
            .create(&path)
            .map_err(|e| error("create file", &path, e))
    }
    /// Open a fixed proof file for reading and seeking, without following links.
    pub fn open_file(&self, file: ValidationFile) -> Result<Box<dyn ReadFile>, StoreError> {
        let path = self.file_path(file);
        self.fs
            .open_no_follow(&path)
            .map_err(|e| error("open file", &path, e))
    }
    /// Remove a proof file, treating an absent file as already removed.
    pub fn remove_file(&self, file: ValidationFile) -> Result<(), StoreError> {
        self.remove_path(&self.file_path(file))
    }
    /// Create one snapshot's private registry namespace exclusively.
    pub fn create_registry_dir(&self, snapshot: &ImportSourceSnapshot) -> Result<(), StoreError> {
        let dir = self.registry_dir(snapshot);
        self.fs
            .create_private_dir(&dir)
            .map_err(|e| error("create registry dir", &dir, e))
    }
    /// Create a fixed registry file exclusively within the snapshot's namespace.
    pub fn create_registry_file(
        &self,
        snapshot: &ImportSourceSnapshot,
        file: RegistryFile,
    ) -> Result<Box<dyn WriteFile>, StoreError> {
        let path = self.registry_file_path(snapshot, file);
        self.fs
            .create(&path)
            .map_err(|e| error("create registry file", &path, e))
    }
    /// Open a fixed registry file for reading and seeking, without following links.
    pub fn open_registry_file(
        &self,
        snapshot: &ImportSourceSnapshot,
        file: RegistryFile,
    ) -> Result<Box<dyn ReadFile>, StoreError> {
        let path = self.registry_file_path(snapshot, file);
        self.fs
            .open_no_follow(&path)
            .map_err(|e| error("open registry file", &path, e))
    }
    /// Remove a snapshot's registry namespace, treating an absent one as removed.
    pub fn remove_registry_dir(&self, snapshot: &ImportSourceSnapshot) -> Result<(), StoreError> {
        let dir = self.registry_dir(snapshot);
        match self.fs.remove_dir_all(&dir) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(error("remove registry dir", &dir, e)),
        }
    }
    fn remove_path(&self, path: &Path) -> Result<(), StoreError> {
        match self.fs.remove(path) {
            Ok(()) => Ok(()),
            Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
            Err(e) => Err(error("remove file", path, e)),
        }
    }
    /// Remove the workspace explicitly, reporting cleanup failure.
    pub fn finish(mut self) -> Result<(), StoreError> {
        self.fs
            .remove_dir_all(&self.dir)
            .map_err(|e| error("remove", &self.dir, e))?;
        self.finished = true;
        Ok(())
    }
}
impl Drop for ValidationScratch {
    fn drop(&mut self) {
        if !self.finished {
            let _ = self.fs.remove_dir_all(&self.dir);
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

    fn registry_snapshot(token: char) -> ImportSourceSnapshot {
        ImportSourceSnapshot::from_metadata(&token.to_string().repeat(32), [7; 32], 64).unwrap()
    }

    #[test]
    fn registry_namespaces_are_exclusive_isolated_and_cleaned() {
        let tmp = ScratchDir::new("validation-registry-namespaces");
        let owner = owner(tmp.path(), real_fs());
        let lease = owner.lease().unwrap();
        let other = owner.lease().unwrap();
        let snapshot = registry_snapshot('a');
        lease.create_registry_dir(&snapshot).unwrap();
        assert!(matches!(
            lease.create_registry_dir(&snapshot),
            Err(StoreError::Unavailable { .. })
        ));
        let files = [
            RegistryFile::Descriptors,
            RegistryFile::Lookup,
            RegistryFile::Header,
        ];
        for (index, file) in files.iter().copied().enumerate() {
            let mut writer = lease.create_registry_file(&snapshot, file).unwrap();
            writer.write_all(&[index as u8]).unwrap();
            writer.sync_all().unwrap();
            assert!(lease.create_registry_file(&snapshot, file).is_err());
            assert!(other.open_registry_file(&snapshot, file).is_err());
        }
        for (index, file) in files.iter().copied().enumerate() {
            let mut bytes = [0];
            lease
                .open_registry_file(&snapshot, file)
                .unwrap()
                .read_exact(&mut bytes)
                .unwrap();
            assert_eq!(bytes, [index as u8]);
        }
        lease.remove_registry_dir(&snapshot).unwrap();
        lease.remove_registry_dir(&snapshot).unwrap();
        assert!(
            lease
                .open_registry_file(&snapshot, RegistryFile::Header)
                .is_err()
        );
        lease.create_registry_dir(&snapshot).unwrap();
        let dir = lease.dir.join(snapshot.token());
        drop(lease);
        assert!(!dir.exists());
        other.finish().unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn registry_file_open_refuses_symlinks() {
        let tmp = ScratchDir::new("validation-registry-symlink");
        let owner = owner(tmp.path(), real_fs());
        let lease = owner.lease().unwrap();
        let snapshot = registry_snapshot('b');
        lease.create_registry_dir(&snapshot).unwrap();
        let target = tmp.path().join("target");
        std::fs::write(&target, b"outside").unwrap();
        std::os::unix::fs::symlink(
            &target,
            lease.registry_file_path(&snapshot, RegistryFile::Header),
        )
        .unwrap();
        assert!(
            lease
                .open_registry_file(&snapshot, RegistryFile::Header)
                .is_err()
        );
        lease.finish().unwrap();
        assert_eq!(std::fs::read(target).unwrap(), b"outside");
    }

    #[test]
    fn registry_file_operations_propagate_faults() {
        for op in [FsOp::Create, FsOp::Open, FsOp::RemoveDirAll, FsOp::SyncFile] {
            let tmp = ScratchDir::new("validation-registry-fault");
            let fs = Arc::new(FaultFs::new(real_fs(), op, 1, FaultKind::Eio));
            let lease = owner(tmp.path(), fs).lease().unwrap();
            let snapshot = registry_snapshot('c');
            lease.create_registry_dir(&snapshot).unwrap();
            let file = RegistryFile::Descriptors;
            match op {
                FsOp::Create => assert!(lease.create_registry_file(&snapshot, file).is_err()),
                FsOp::Open => assert!(lease.open_registry_file(&snapshot, file).is_err()),
                FsOp::RemoveDirAll => {
                    assert!(lease.remove_registry_dir(&snapshot).is_err())
                }
                FsOp::SyncFile => {
                    assert!(
                        lease
                            .create_registry_file(&snapshot, file)
                            .unwrap()
                            .sync_all()
                            .is_err()
                    )
                }
                _ => unreachable!(),
            }
            lease.finish().unwrap();
        }
    }

    #[test]
    fn interrupted_registry_namespace_drop_removes_every_file() {
        let tmp = ScratchDir::new("validation-registry-drop");
        let owner = owner(tmp.path(), real_fs());
        let lease = owner.lease().unwrap();
        let snapshot = registry_snapshot('d');
        lease.create_registry_dir(&snapshot).unwrap();
        for file in [
            RegistryFile::Descriptors,
            RegistryFile::Lookup,
            RegistryFile::Header,
        ] {
            lease
                .create_registry_file(&snapshot, file)
                .unwrap()
                .write_all(b"partial")
                .unwrap();
        }
        let dir = lease.dir.join(snapshot.token());
        drop(lease);
        assert!(!dir.exists());
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
    fn proof_files_coexist_with_sort_tapes_and_are_lease_owned() {
        let tmp = ScratchDir::new("validation-proof-files");
        let owner = owner(tmp.path(), real_fs());
        let lease = owner.lease().unwrap();
        let other = owner.lease().unwrap();
        for tape in [ValidationTape::A, ValidationTape::B] {
            lease.create(tape).unwrap();
        }
        let files = [
            ValidationFile::Descriptors,
            ValidationFile::IndexSpool,
            ValidationFile::Lookup,
        ];
        for (index, file) in files.iter().copied().enumerate() {
            let mut writer = lease.create_file(file).unwrap();
            writer.write_all(&[index as u8]).unwrap();
            writer.sync_all().unwrap();
            assert!(lease.create_file(file).is_err());
            assert!(other.open_file(file).is_err());
        }
        for (index, file) in files.iter().copied().enumerate() {
            let mut bytes = [0];
            lease
                .open_file(file)
                .unwrap()
                .read_exact(&mut bytes)
                .unwrap();
            assert_eq!(bytes, [index as u8]);
        }
        lease.remove(ValidationTape::A).unwrap();
        lease.remove(ValidationTape::B).unwrap();
        // Reusing the sorting tapes must not disturb either proof output.
        lease.create(ValidationTape::A).unwrap();
        lease.create(ValidationTape::B).unwrap();
        for file in [ValidationFile::Descriptors, ValidationFile::Lookup] {
            assert!(lease.open_file(file).is_ok());
        }
        let mut lookup = lease.open_file(ValidationFile::Lookup).unwrap();
        lookup.seek(SeekFrom::Start(0)).unwrap();
        let mut slot = [0];
        lookup.read_exact(&mut slot).unwrap();
        assert_eq!(slot, [2]);
        drop(lookup);
        lease.remove_file(ValidationFile::IndexSpool).unwrap();
        lease.remove_file(ValidationFile::IndexSpool).unwrap();
        assert!(lease.open_file(ValidationFile::IndexSpool).is_err());
        let dir = lease.dir.clone();
        lease.finish().unwrap();
        assert!(!dir.exists());
        other.finish().unwrap();
    }

    #[test]
    fn interrupted_proof_workspace_drop_removes_every_file() {
        let tmp = ScratchDir::new("validation-proof-drop");
        let owner = owner(tmp.path(), real_fs());
        let lease = owner.lease().unwrap();
        for tape in [ValidationTape::A, ValidationTape::B] {
            lease.create(tape).unwrap().write_all(b"partial").unwrap();
        }
        for file in [
            ValidationFile::Descriptors,
            ValidationFile::IndexSpool,
            ValidationFile::Lookup,
        ] {
            lease
                .create_file(file)
                .unwrap()
                .write_all(b"partial")
                .unwrap();
        }
        let dir = lease.dir.clone();
        drop(lease);
        assert!(!dir.exists());
    }

    #[test]
    fn proof_file_operations_propagate_faults() {
        for op in [FsOp::Create, FsOp::Open, FsOp::Remove, FsOp::SyncFile] {
            let tmp = ScratchDir::new("validation-proof-fault");
            let fs = Arc::new(FaultFs::new(real_fs(), op, 1, FaultKind::Eio));
            let owner = owner(tmp.path(), fs);
            let lease = owner.lease().unwrap();
            let file = ValidationFile::Descriptors;
            match op {
                FsOp::Create => assert!(lease.create_file(file).is_err()),
                FsOp::Open => assert!(lease.open_file(file).is_err()),
                FsOp::Remove => assert!(lease.remove_file(file).is_err()),
                FsOp::SyncFile => assert!(lease.create_file(file).unwrap().sync_all().is_err()),
                _ => unreachable!(),
            }
            lease.finish().unwrap();
        }
    }

    #[cfg(unix)]
    #[test]
    fn proof_file_open_refuses_symlinks() {
        let tmp = ScratchDir::new("validation-proof-symlink");
        let owner = owner(tmp.path(), real_fs());
        let lease = owner.lease().unwrap();
        let target = tmp.path().join("target");
        std::fs::write(&target, b"outside").unwrap();
        std::os::unix::fs::symlink(&target, lease.file_path(ValidationFile::Lookup)).unwrap();
        assert!(lease.open_file(ValidationFile::Lookup).is_err());
        lease.finish().unwrap();
        assert_eq!(std::fs::read(target).unwrap(), b"outside");
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
