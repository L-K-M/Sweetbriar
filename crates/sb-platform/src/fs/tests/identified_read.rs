//! `ConfinedDir::open_regular_identified` proof: one open yields the
//! handle, metadata and identity of the same regular file, and the
//! triple stays bound to that file when its name is renamed, replaced
//! or unlinked. Each test owns a distinct `Scratch` label.

use super::*;

/// Open `path` as a confined root directory.
fn confined_root(path: &Path) -> Box<dyn ConfinedDir> {
    RealFs
        .open_confined_dir(&path.canonicalize().unwrap())
        .unwrap()
}

/// The identified open reports the creating write capability's inode,
/// exact metadata and original bytes; a second read-only open agrees.
#[test]
fn identified_read_reports_created_identity_metadata_and_bytes() {
    let scratch = Scratch::new("identified-read-created");
    let root = confined_root(&scratch.0);
    let writer = root.create_read_write(Path::new("state.sqlite")).unwrap();
    writer.write_at(b"original bytes", 0).unwrap();
    let created = writer.identity().unwrap();
    drop(writer);

    let (mut handle, metadata, identity) = root
        .open_regular_identified(Path::new("state.sqlite"))
        .unwrap();
    assert_eq!(identity, created);
    assert_eq!(metadata.kind(), FileKind::File);
    assert_eq!(metadata.len(), 14);
    let mut bytes = String::new();
    handle.read_to_string(&mut bytes).unwrap();
    assert_eq!(bytes, "original bytes");

    let (mut again, _, identity) = root
        .open_regular_identified(Path::new("state.sqlite"))
        .unwrap();
    assert_eq!(identity, created);
    let mut bytes = String::new();
    again.read_to_string(&mut bytes).unwrap();
    assert_eq!(bytes, "original bytes");
}

/// A same-length replacement at the old name leaves the retained
/// handle, its metadata snapshot and its identity on the original file.
#[test]
fn identified_read_stays_bound_across_same_length_replacement() {
    let scratch = Scratch::new("identified-read-replacement");
    let root = confined_root(&scratch.0);
    let writer = root.create_read_write(Path::new("state.sqlite")).unwrap();
    writer.write_at(b"original", 0).unwrap();
    let created = writer.identity().unwrap();
    drop(writer);

    let (mut handle, metadata, identity) = root
        .open_regular_identified(Path::new("state.sqlite"))
        .unwrap();
    assert_eq!(identity, created);
    assert_eq!(metadata.len(), 8);

    std::fs::rename(scratch.0.join("state.sqlite"), scratch.0.join("old.sqlite")).unwrap();
    std::fs::write(scratch.0.join("state.sqlite"), b"REPLACED").unwrap();

    let mut bytes = String::new();
    handle.read_to_string(&mut bytes).unwrap();
    assert_eq!(bytes, "original");
    assert_eq!(identity, created);
    assert_eq!(metadata.len(), 8);

    let (mut fresh, fresh_metadata, fresh_identity) = root
        .open_regular_identified(Path::new("state.sqlite"))
        .unwrap();
    assert_ne!(fresh_identity, identity);
    assert_eq!(fresh_metadata.len(), 8);
    let mut bytes = String::new();
    fresh.read_to_string(&mut bytes).unwrap();
    assert_eq!(bytes, "REPLACED");
}

/// Renaming the whole root directory leaves the retained capability
/// opening the original file; a fresh capability at the old pathname
/// sees the replacement.
#[test]
fn identified_read_follows_the_retained_directory_not_the_old_path() {
    let scratch = Scratch::new("identified-read-root-rename");
    let original_root = scratch.0.join("root");
    std::fs::create_dir(&original_root).unwrap();
    std::fs::write(original_root.join("state.sqlite"), b"original").unwrap();
    let root = confined_root(&original_root);

    let (mut handle, metadata, identity) = root
        .open_regular_identified(Path::new("state.sqlite"))
        .unwrap();
    assert_eq!(metadata.len(), 8);

    let moved_root = scratch.0.join("moved");
    std::fs::rename(&original_root, &moved_root).unwrap();
    std::fs::create_dir(&original_root).unwrap();
    std::fs::write(original_root.join("state.sqlite"), b"REPLACED").unwrap();

    let (mut retained, retained_metadata, retained_identity) = root
        .open_regular_identified(Path::new("state.sqlite"))
        .unwrap();
    assert_eq!(retained_identity, identity);
    assert_eq!(retained_metadata.len(), 8);
    let mut bytes = String::new();
    retained.read_to_string(&mut bytes).unwrap();
    assert_eq!(bytes, "original");

    let fresh_root = confined_root(&original_root);
    let (mut fresh, fresh_metadata, fresh_identity) = fresh_root
        .open_regular_identified(Path::new("state.sqlite"))
        .unwrap();
    assert_ne!(fresh_identity, identity);
    assert_eq!(fresh_metadata.len(), 8);
    let mut bytes = String::new();
    fresh.read_to_string(&mut bytes).unwrap();
    assert_eq!(bytes, "REPLACED");

    let mut bytes = String::new();
    handle.read_to_string(&mut bytes).unwrap();
    assert_eq!(bytes, "original");
}

/// Hard links share one identity; unlinking both names keeps the
/// retained handles on the original inode while a recreated name is a
/// different file.
#[test]
fn identified_read_sees_hardlink_aliases_and_survives_unlink() {
    let scratch = Scratch::new("identified-read-hardlink");
    let root = confined_root(&scratch.0);
    let writer = root.create_read_write(Path::new("name")).unwrap();
    writer.write_at(b"shared bytes", 0).unwrap();
    let created = writer.identity().unwrap();
    drop(writer);
    std::fs::hard_link(scratch.0.join("name"), scratch.0.join("alias")).unwrap();

    let (mut first, first_metadata, first_identity) =
        root.open_regular_identified(Path::new("name")).unwrap();
    let (mut second, second_metadata, second_identity) =
        root.open_regular_identified(Path::new("alias")).unwrap();
    assert_eq!(first_identity, second_identity);
    assert_eq!(first_identity, created);
    assert_eq!(first_metadata.len(), 12);
    assert_eq!(second_metadata.len(), 12);
    let mut bytes = String::new();
    first.read_to_string(&mut bytes).unwrap();
    assert_eq!(bytes, "shared bytes");
    let mut bytes = String::new();
    second.read_to_string(&mut bytes).unwrap();
    assert_eq!(bytes, "shared bytes");

    root.remove_file(Path::new("name")).unwrap();
    root.remove_file(Path::new("alias")).unwrap();
    let replacement = root.create_read_write(Path::new("name")).unwrap();
    replacement.write_at(b"new content!", 0).unwrap();
    let replacement_identity = replacement.identity().unwrap();
    drop(replacement);
    assert_ne!(replacement_identity, first_identity);

    let (mut fresh, fresh_metadata, fresh_identity) =
        root.open_regular_identified(Path::new("name")).unwrap();
    assert_ne!(fresh_identity, first_identity);
    assert_eq!(fresh_metadata.len(), 12);
    let mut bytes = String::new();
    fresh.read_to_string(&mut bytes).unwrap();
    assert_eq!(bytes, "new content!");

    first.seek(SeekFrom::Start(0)).unwrap();
    second.seek(SeekFrom::Start(0)).unwrap();
    let mut bytes = String::new();
    first.read_to_string(&mut bytes).unwrap();
    assert_eq!(bytes, "shared bytes");
    let mut bytes = String::new();
    second.read_to_string(&mut bytes).unwrap();
    assert_eq!(bytes, "shared bytes");
}

/// The direct method refuses absolute paths, parent traversal,
/// directories, missing entries, symlinks in the final or an
/// intermediate component, and sockets.
#[test]
fn identified_read_rejects_traversal_symlinks_and_non_files() {
    use std::os::unix::fs::symlink;
    let scratch = Scratch::new("identified-read-refusal");
    let root = confined_root(&scratch.0);
    std::fs::write(scratch.0.join("file"), b"data").unwrap();
    std::fs::create_dir(scratch.0.join("dir")).unwrap();
    std::fs::write(scratch.0.join("dir/inner"), b"inner").unwrap();
    symlink("file", scratch.0.join("link")).unwrap();
    symlink("dir", scratch.0.join("dirlink")).unwrap();
    let _socket = std::os::unix::net::UnixListener::bind(scratch.0.join("sock")).unwrap();

    for (relative, expected) in [
        ("/absolute", io::ErrorKind::InvalidInput),
        ("../outside", io::ErrorKind::InvalidInput),
        ("dir", io::ErrorKind::InvalidInput),
        ("missing", io::ErrorKind::NotFound),
    ] {
        let error = root
            .open_regular_identified(Path::new(relative))
            .err()
            .unwrap();
        assert_eq!(error.kind(), expected, "{relative}");
    }

    for relative in ["link", "dirlink/inner", "sock"] {
        assert!(
            root.open_regular_identified(Path::new(relative)).is_err(),
            "{relative} opened"
        );
    }
}
