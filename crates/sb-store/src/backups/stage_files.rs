//! Retained, unqualified staging capabilities. Identity is not immutability or
//! swap eligibility; qualification must still verify bytes and path bindings.
use std::fmt;
use std::io::{self, Read, Seek, SeekFrom};
use std::sync::Arc;

use sb_types::fs::{ConfinedDir, DirectoryIdentity, FileIdentity, ReadFile, ReadWriteFile};

pub(super) struct StageFiles {
    pub(super) root: Box<dyn ConfinedDir>,
    pub(super) root_identity: DirectoryIdentity,
    pub(super) database: Arc<dyn ReadWriteFile>,
    pub(super) database_identity: FileIdentity,
    pub(super) database_sha256: [u8; 32],
    pub(super) database_bytes: u64,
}

impl StageFiles {
    pub(super) fn new(
        root: Box<dyn ConfinedDir>,
        database: Arc<dyn ReadWriteFile>,
        database_sha256: [u8; 32],
        database_bytes: u64,
    ) -> io::Result<Self> {
        Ok(Self {
            root_identity: root.identity()?,
            database_identity: database.identity()?,
            root,
            database,
            database_sha256,
            database_bytes,
        })
    }

    /// Independent read cursor over the same file that received the copy. This
    /// cannot grant write authority or certify that its bytes remain verified.
    pub(super) fn database_reader(&self) -> Box<dyn ReadFile> {
        Box::new(DatabaseReader {
            file: Arc::clone(&self.database),
            position: 0,
        })
    }
}

impl fmt::Debug for StageFiles {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("StageFiles")
            .field("root_identity", &self.root_identity)
            .field("database_identity", &self.database_identity)
            .finish_non_exhaustive()
    }
}

struct DatabaseReader {
    file: Arc<dyn ReadWriteFile>,
    position: u64,
}

impl Read for DatabaseReader {
    fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
        let read = self.file.read_at(bytes, self.position)?;
        self.position = self
            .position
            .checked_add(read as u64)
            .ok_or(io::ErrorKind::InvalidData)?;
        Ok(read)
    }
}

impl Seek for DatabaseReader {
    fn seek(&mut self, from: SeekFrom) -> io::Result<u64> {
        let position = match from {
            SeekFrom::Start(position) => Some(position),
            SeekFrom::Current(delta) => self.position.checked_add_signed(delta),
            SeekFrom::End(delta) => self.file.len()?.checked_add_signed(delta),
        }
        .ok_or(io::ErrorKind::InvalidInput)?;
        self.position = position;
        Ok(position)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sb_test_support::{FaultFs, FaultKind, FsOp, SimFs};
    use sb_types::fs::Fs;
    use std::path::Path;

    fn files(fs: &dyn Fs) -> StageFiles {
        let root = fs.open_confined_dir(Path::new("/stage")).unwrap();
        let database = root.create_read_write(Path::new("db")).unwrap();
        database.write_at(b"01234567", 0).unwrap();
        StageFiles::new(root, database.into(), [0; 32], 8).unwrap()
    }

    #[test]
    fn cursors_are_independent_and_reject_invalid_seeks_without_moving() {
        let fs = SimFs::new();
        fs.create_dir_all(Path::new("/stage")).unwrap();
        let retained = files(&fs);
        let mut first = retained.database_reader();
        let mut second = retained.database_reader();
        let mut bytes = [0; 2];
        first.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"01");
        assert_eq!(
            first.seek(SeekFrom::Current(-3)).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
        first.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"23");
        second.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"01");
        assert_eq!(first.seek(SeekFrom::End(-2)).unwrap(), 6);
        first.read_exact(&mut bytes).unwrap();
        assert_eq!(&bytes, b"67");
        assert_eq!(first.seek(SeekFrom::End(1)).unwrap(), 9);
        assert_eq!(first.read(&mut bytes).unwrap(), 0);
        first.seek(SeekFrom::Start(u64::MAX)).unwrap();
        assert_eq!(
            first.seek(SeekFrom::Current(1)).unwrap_err().kind(),
            io::ErrorKind::InvalidInput
        );
    }

    #[test]
    fn reader_preserves_read_and_length_errors_without_advancing() {
        for seek in [false, true] {
            let inner = Arc::new(SimFs::new());
            inner.create_dir_all(Path::new("/stage")).unwrap();
            let fs = FaultFs::new(inner, FsOp::Read, 1, FaultKind::Eio);
            let retained = files(&fs);
            let mut reader = retained.database_reader();
            let error = if seek {
                reader.seek(SeekFrom::End(0)).unwrap_err()
            } else {
                reader.read(&mut [0; 1]).unwrap_err()
            };
            assert!(
                error.raw_os_error().is_some(),
                "native error evidence is retained"
            );
            assert_eq!(fs.calls(FsOp::Read), 1);
            let mut bytes = [0; 2];
            reader.read_exact(&mut bytes).unwrap();
            assert_eq!(&bytes, b"01");
        }
    }
    #[test]
    fn identity_errors_never_return_retained_capabilities() {
        for nth in [2, 3] {
            let inner = Arc::new(SimFs::new());
            inner.create_dir_all(Path::new("/stage")).unwrap();
            let fs = FaultFs::new(inner, FsOp::Open, nth, FaultKind::Eio);
            let root = fs.open_confined_dir(Path::new("/stage")).unwrap();
            let database = root.create_read_write(Path::new("db")).unwrap();
            let error = StageFiles::new(root, database.into(), [0; 32], 0).unwrap_err();
            assert!(error.raw_os_error().is_some());
            assert_eq!(fs.calls(FsOp::Open), nth);
        }
    }
}
