//! `StorePaths`: the on-disk layout (spec/03 "On-disk layout") derived
//! from the data root.

use std::path::{Path, PathBuf};

use crate::StoreError;

/// The paths a store owns: the sibling lock file, `state.sqlite` and
/// the layout directories under the data root.
#[derive(Debug)]
pub struct StorePaths {
    data_root: PathBuf,
    lock_file: PathBuf,
    state_db: PathBuf,
    objects: PathBuf,
    objects_tmp: PathBuf,
    objects_sha256: PathBuf,
    backups: PathBuf,
    recovery: PathBuf,
}

impl StorePaths {
    /// The store layout under `data_root`, which must be absolute and
    /// have a final name. Trailing separators and `.` components are
    /// dropped, so `/x/store/` locks `/x/store.lock`, not a file inside
    /// the root.
    pub fn new(data_root: impl AsRef<Path>) -> Result<Self, StoreError> {
        let data_root: PathBuf = data_root.as_ref().components().collect();
        let data_root = data_root.as_path();
        if !data_root.is_absolute() {
            return Err(StoreError::InvalidInput {
                message: format!(
                    "data root must be an absolute path: {}",
                    data_root.display()
                ),
            });
        }
        if data_root.file_name().is_none() {
            return Err(StoreError::InvalidInput {
                message: format!("data root must have a final name: {}", data_root.display()),
            });
        }
        // `<data root>.lock`: a sibling of the root, so it survives the
        // root being renamed during restore (spec/03).
        let mut lock_file = data_root.as_os_str().to_os_string();
        lock_file.push(".lock");
        let objects = data_root.join("objects");
        Ok(Self {
            state_db: data_root.join("state.sqlite"),
            objects_tmp: objects.join("tmp"),
            objects_sha256: objects.join("sha256"),
            backups: data_root.join("backups"),
            recovery: data_root.join("recovery"),
            lock_file: PathBuf::from(lock_file),
            objects,
            data_root: data_root.to_path_buf(),
        })
    }

    pub fn data_root(&self) -> &Path {
        &self.data_root
    }

    /// `<data root>.lock`, the ownership lease; it is never deleted.
    pub fn lock_file(&self) -> &Path {
        &self.lock_file
    }

    /// `<data root>/state.sqlite` (with its `-wal`/`-shm` companions).
    pub fn state_db(&self) -> &Path {
        &self.state_db
    }

    /// `objects/` — in-progress writes under `tmp/`, published
    /// immutable objects under `sha256/`.
    pub fn objects(&self) -> &Path {
        &self.objects
    }

    pub fn objects_tmp(&self) -> &Path {
        &self.objects_tmp
    }

    pub fn objects_sha256(&self) -> &Path {
        &self.objects_sha256
    }

    /// `backups/` — the backup generations.
    pub fn backups(&self) -> &Path {
        &self.backups
    }

    /// `recovery/` — preserved evidence: quarantined objects,
    /// pre-migration snapshots.
    pub fn recovery(&self) -> &Path {
        &self.recovery
    }

    /// The five layout directories in spec/03 creation order.
    pub fn layout_dirs(&self) -> [&Path; 5] {
        [
            &self.objects,
            &self.objects_tmp,
            &self.objects_sha256,
            &self.backups,
            &self.recovery,
        ]
    }
}
