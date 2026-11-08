use std::io::{Read, Seek, SeekFrom};
use std::path::Path;
use std::sync::Arc;

use sb_types::fs::{ConfinedDir, FileKind, Fs};

use crate::{CutFs, FaultFs, FaultKind, FsOp, SimFs};

fn seeded() -> (Arc<SimFs>, Box<dyn ConfinedDir>, sb_types::fs::FileIdentity) {
    let fs = Arc::new(SimFs::new());
    fs.create_dir_all(Path::new("/root/nested")).unwrap();
    let root = fs.open_confined_dir(Path::new("/root")).unwrap();
    let file = root.create_read_write(Path::new("file")).unwrap();
    file.write_at(b"original", 0).unwrap();
    file.sync_all().unwrap();
    root.sync().unwrap();
    (fs, root, file.identity().unwrap())
}

#[test]
fn simulated_identified_reads_retain_the_opened_node_across_file_and_root_replacement() {
    let (fs, root, expected) = seeded();
    let (mut original, metadata, identity) =
        root.open_regular_identified(Path::new("file")).unwrap();
    assert_eq!(identity, expected);
    assert_eq!(metadata.kind(), FileKind::File);
    assert_eq!(metadata.len(), 8);
    fs.rename(Path::new("/root/file"), Path::new("/root/old"))
        .unwrap();
    let replacement = root.create_read_write(Path::new("file")).unwrap();
    replacement.write_at(b"replaced", 0).unwrap();
    let (_, newer_metadata, newer) = root.open_regular_identified(Path::new("file")).unwrap();
    assert_eq!(newer_metadata, metadata);
    assert_ne!(newer, identity);
    let mut bytes = Vec::new();
    original.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"original");

    fs.rename(Path::new("/root"), Path::new("/moved")).unwrap();
    fs.create_dir_all(Path::new("/root")).unwrap();
    let fresh_root = fs.open_confined_dir(Path::new("/root")).unwrap();
    let fresh_file = fresh_root.create_read_write(Path::new("file")).unwrap();
    fresh_file.write_at(b"outsider", 0).unwrap();
    let (_, _, retained) = root.open_regular_identified(Path::new("old")).unwrap();
    assert_eq!(retained, identity);
    let (_, _, fresh) = fresh_root
        .open_regular_identified(Path::new("file"))
        .unwrap();
    assert_ne!(fresh, identity);
    original.seek(SeekFrom::Start(0)).unwrap();
    bytes.clear();
    original.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"original");
}

#[test]
fn simulated_read_open_does_not_make_unflushed_bytes_durable() {
    let (fs, root, identity) = seeded();
    // Keep a durable original file open while a sibling file has volatile
    // bytes; opening either for reading must not flush those bytes.
    let file = root.open_regular_identified(Path::new("file")).unwrap();
    let mut reader = file.0;
    let mut writer = fs.create(Path::new("/root/volatile")).unwrap();
    std::io::Write::write_all(&mut writer, b"not durable").unwrap();
    root.sync().unwrap();
    let (_, volatile_metadata, _) = root.open_regular_identified(Path::new("volatile")).unwrap();
    assert_eq!(volatile_metadata.len(), 11);
    fs.power_cut();
    let (_, _, after) = root.open_regular_identified(Path::new("file")).unwrap();
    assert_eq!(identity, after);
    let (mut volatile, metadata, _) = root.open_regular_identified(Path::new("volatile")).unwrap();
    assert_eq!(metadata.len(), 0);
    assert_eq!(volatile.read(&mut [0; 1]).unwrap(), 0);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"original");
}

#[test]
fn simulated_identified_reads_keep_the_confined_path_rules() {
    let (_, root, _) = seeded();
    for path in [
        "/root/file",
        "../file",
        "nested/../../file",
        "nested",
        "missing",
        ".",
        "",
    ] {
        assert!(
            root.open_regular_identified(Path::new(path)).is_err(),
            "{path}"
        );
    }
}

#[test]
fn identified_open_and_returned_reader_faults_fire_without_losing_the_identity() {
    let (inner, _, expected) = seeded();
    let fault = FaultFs::new(inner.clone(), FsOp::Open, 2, FaultKind::Eio);
    let root = fault.open_confined_dir(Path::new("/root")).unwrap();
    let error = root
        .open_regular_identified(Path::new("file"))
        .err()
        .unwrap();
    assert_eq!(error.raw_os_error(), Some(libc::EIO));
    assert_eq!(
        fault.calls(FsOp::Open),
        2,
        "new atomic open fault must fire"
    );
    for seek in [false, true] {
        let operation = if seek { FsOp::Seek } else { FsOp::Read };
        let fault = FaultFs::new(inner.clone(), operation, 1, FaultKind::Eio);
        let root = fault.open_confined_dir(Path::new("/root")).unwrap();
        let (mut reader, metadata, identity) =
            root.open_regular_identified(Path::new("file")).unwrap();
        assert_eq!(identity, expected);
        assert_eq!(metadata.len(), 8);
        let error = if seek {
            reader.seek(SeekFrom::Start(0)).unwrap_err()
        } else {
            reader.read(&mut [0; 1]).unwrap_err()
        };
        assert_eq!(error.raw_os_error(), Some(libc::EIO));
        assert_eq!(
            fault.calls(operation),
            1,
            "returned reader is still fault wrapped"
        );
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, b"original");
    }
}

#[test]
fn crash_adapter_counts_the_identified_open_as_one_operation() {
    let (inner, _, expected) = seeded();
    let cut = CutFs::new(inner, u64::MAX);
    let root = cut.open_confined_dir(Path::new("/root")).unwrap();
    assert_eq!(cut.operations(), 1);
    let (mut reader, _, identity) = root.open_regular_identified(Path::new("file")).unwrap();
    assert_eq!(cut.operations(), 2);
    assert_eq!(identity, expected);
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"original");
}
