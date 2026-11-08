//! Retained identity leases for an unqualified restore copy. They pin the
//! selected source and live files, without certifying immutable bytes or
//! granting authority to quarantine, publish or swap a database.

use std::fmt;
use std::path::Path;

use sb_types::fs::{ConfinedDir, DirectoryIdentity, FileIdentity, Fs, ReadFile};

use super::stage_copy::io_error;
use crate::StoreError;

pub(super) struct LiveBinding {
    pub(super) root: Box<dyn ConfinedDir>,
    pub(super) root_identity: DirectoryIdentity,
    /// Holding this read-only descriptor prevents its inode from being reused.
    /// The later qualifier consumes this lease; no SQL connection is retained.
    #[allow(dead_code)]
    pub(super) database: Box<dyn ReadFile>,
    pub(super) database_identity: FileIdentity,
}

impl LiveBinding {
    pub(super) fn capture(fs: &dyn Fs, data_root: &Path) -> Result<Self, StoreError> {
        let root = fs
            .open_confined_dir(data_root)
            .map_err(|error| io_error("retaining the live root", error))?;
        let root_identity = root
            .identity()
            .map_err(|error| io_error("reading the live root identity", error))?;
        let (database, _, database_identity) = root
            .open_regular_identified(Path::new(super::SNAPSHOT_FILE))
            .map_err(|error| io_error("retaining the live database identity", error))?;
        Ok(Self {
            root,
            root_identity,
            database,
            database_identity,
        })
    }

    /// Compare current names with retained identities. The old descriptors
    /// remain held throughout every comparison, so replacement cannot recycle
    /// their identities. These are point-in-time checks, not a namespace lock.
    pub(super) fn recheck(&self, fs: &dyn Fs, data_root: &Path) -> Result<(), StoreError> {
        let named = fs
            .open_confined_dir(data_root)
            .map_err(|error| io_error("rechecking the live root", error))?;
        let root_identity = named
            .identity()
            .map_err(|error| io_error("rechecking the live root identity", error))?;
        if root_identity != self.root_identity {
            return Err(StoreError::conflict("the live restore root was replaced"));
        }
        let (_, _, database_identity) = named
            .open_regular_identified(Path::new(super::SNAPSHOT_FILE))
            .map_err(|error| io_error("rechecking the live database identity", error))?;
        if database_identity != self.database_identity {
            return Err(StoreError::conflict(
                "the live restore database was replaced",
            ));
        }
        Ok(())
    }
}

impl fmt::Debug for LiveBinding {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("LiveBinding")
            .field("root_identity", &self.root_identity)
            .field("database_identity", &self.database_identity)
            .finish_non_exhaustive()
    }
}

pub(super) struct StageBindings {
    pub(super) source: Box<dyn ConfinedDir>,
    pub(super) source_identity: DirectoryIdentity,
    pub(super) live: LiveBinding,
}

impl StageBindings {
    pub(super) fn recheck(
        &self,
        fs: &dyn Fs,
        source_location: &Path,
        data_root: &Path,
    ) -> Result<(), StoreError> {
        let named = fs
            .open_confined_dir(source_location)
            .map_err(|error| io_error("rechecking the selected source root", error))?;
        let identity = named
            .identity()
            .map_err(|error| io_error("rechecking the selected source identity", error))?;
        if identity != self.source_identity {
            return Err(StoreError::corrupt("the restore source root was replaced"));
        }
        self.live.recheck(fs, data_root)
    }
}

impl fmt::Debug for StageBindings {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StageBindings")
            .field("source_identity", &self.source_identity)
            .field("live", &self.live)
            .finish_non_exhaustive()
    }
}
