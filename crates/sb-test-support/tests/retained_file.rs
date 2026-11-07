use std::io;
use std::path::Path;
use std::sync::Arc;

use sb_test_support::{FaultFs, FaultKind, FsOp, SimFs};
use sb_types::fs::Fs;

#[test]
fn retained_file_sim_preserves_identity_and_only_flushed_state() {
    let fs = SimFs::new();
    fs.create_dir_all(Path::new("/stage")).unwrap();
    let dir = fs.open_confined_dir(Path::new("/stage")).unwrap();
    let file = dir.create_read_write(Path::new("db")).unwrap();
    let id = file.identity().unwrap();
    assert_eq!(file.write_at(b"saved", 2).unwrap(), 5);
    file.sync_all().unwrap();
    dir.sync().unwrap();
    file.write_at(b"lost", 0).unwrap();
    file.set_len(12).unwrap();
    fs.power_cut();
    let mut bytes = [0; 20];
    assert_eq!(file.read_at(&mut bytes, 0).unwrap(), 7);
    assert_eq!(&bytes[..7], b"\0\0saved");
    assert_eq!(file.identity().unwrap(), id);
    fs.rename(Path::new("/stage"), Path::new("/moved")).unwrap();
    fs.create_dir(Path::new("/stage")).unwrap();
    fs.rename(Path::new("/moved/db"), Path::new("/moved/retained"))
        .unwrap();
    let replacement = dir.create_read_write(Path::new("db")).unwrap();
    assert_ne!(replacement.identity().unwrap(), id);
    file.set_len(2).unwrap();
    file.write_at(b"ok", 0).unwrap();
    file.sync_all().unwrap();
    assert_eq!(file.read_at(&mut bytes, 0).unwrap(), 2);
    assert_eq!(&bytes[..2], b"ok");
    assert_eq!(replacement.len().unwrap(), 0);
    assert!(dir.create_read_write(Path::new("db")).is_err());
    assert!(dir.create_read_write(Path::new("../escape")).is_err());
}

#[test]
fn retained_file_sim_requires_both_entry_and_data_flushes() {
    for (flush_file, flush_dir, expected) in [
        (false, false, None),
        (true, false, None),
        (false, true, Some(0)),
        (true, true, Some(4)),
    ] {
        let fs = SimFs::new();
        fs.create_dir_all(Path::new("/stage")).unwrap();
        let dir = fs.open_confined_dir(Path::new("/stage")).unwrap();
        let file = dir.create_read_write(Path::new("db")).unwrap();
        file.write_at(b"data", 0).unwrap();
        if flush_file {
            file.sync_all().unwrap();
        }
        if flush_dir {
            dir.sync().unwrap();
        }
        drop(file);
        fs.power_cut();
        let len = fs
            .metadata(Path::new("/stage/db"))
            .unwrap()
            .map(|md| md.len());
        assert_eq!(len, expected);
    }
}

#[test]
fn retained_file_faults_are_reached_and_propagated() {
    for (op, nth) in [
        (FsOp::Create, 1),
        (FsOp::Read, 1),
        (FsOp::Write, 1),
        (FsOp::Truncate, 1),
        (FsOp::SyncFile, 1),
        (FsOp::Open, 2),
    ] {
        let inner = Arc::new(SimFs::new());
        inner.create_dir_all(Path::new("/stage")).unwrap();
        let fs = FaultFs::new(inner, op, nth, FaultKind::NoSpace);
        let dir = fs.open_confined_dir(Path::new("/stage")).unwrap();
        let result = dir.create_read_write(Path::new("db"));
        let error = if op == FsOp::Create {
            result.err().unwrap()
        } else {
            let file = result.unwrap();
            match op {
                FsOp::Read => file.read_at(&mut [0; 1], 0).err().unwrap(),
                FsOp::Write => file.write_at(b"abc", 0).err().unwrap(),
                FsOp::Truncate => file.set_len(5).err().unwrap(),
                FsOp::SyncFile => file.sync_all().err().unwrap(),
                FsOp::Open => file.identity().err().unwrap(),
                _ => unreachable!(),
            }
        };
        assert_eq!(error.raw_os_error(), Some(libc::ENOSPC));
        assert_eq!(fs.calls(op), nth);
    }
    let inner = Arc::new(SimFs::new());
    inner.create_dir_all(Path::new("/stage")).unwrap();
    let fs = FaultFs::new(inner, FsOp::Write, 1, FaultKind::ShortWrite);
    let dir = fs.open_confined_dir(Path::new("/stage")).unwrap();
    let file = dir.create_read_write(Path::new("db")).unwrap();
    assert!(file.write_at(b"12345", 3).is_err());
    let mut bytes = [0; 6];
    assert_eq!(file.read_at(&mut bytes, 0).unwrap(), 6);
    assert_eq!(&bytes, b"\0\0\x00123");
    assert_eq!(fs.calls(FsOp::Write), 1);
    assert!(
        matches!(dir.create_read_write(Path::new("db")), Err(err) if err.kind() == io::ErrorKind::AlreadyExists)
    );
}

#[test]
fn retained_file_unsupported_adapter_fails_closed() {
    use sb_types::fs::{ConfinedDir, Metadata, ReadFile};
    struct Legacy;
    impl ConfinedDir for Legacy {
        fn open_regular(&self, _: &Path) -> io::Result<(Box<dyn ReadFile>, Metadata)> {
            panic!("read-only open is not a read/write fallback")
        }
    }
    assert!(
        matches!(Legacy.create_read_write(Path::new("db")), Err(err) if err.kind() == io::ErrorKind::Unsupported)
    );
}
