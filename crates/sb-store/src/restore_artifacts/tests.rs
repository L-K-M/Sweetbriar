use std::fs;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use sb_test_support::{FaultFs, FaultKind, FsOp, ScratchDir, damage_an_index, real_fs};

use super::*;
use crate::testing::test_seams;
use crate::{IntegrityCheck, StorePaths, open};

const A: &str = "00000000-0000-4000-8000-000000000001";
const B: &str = "00000000-0000-4000-8000-000000000002";

fn scratch(label: &str) -> (ScratchDir, StorePaths, Store) {
    let scratch = ScratchDir::new(label);
    let root = fs::canonicalize(scratch.path()).unwrap();
    let paths = StorePaths::new(root.join("store")).unwrap();
    let store = open(&paths, &test_seams(real_fs())).unwrap();
    (scratch, paths, store)
}

#[test]
fn exact_restore_names_are_listed_sorted_without_inspecting_their_contents() {
    let (_scratch, paths, mut store) = scratch("restore-artifacts-names");
    let parent = paths.data_root().parent().unwrap();
    let staged = parent.join(format!("store.restoring-{B}"));
    let preserved = parent.join(format!("store.preserved-{A}"));
    fs::create_dir(&staged).unwrap();
    fs::write(staged.join("unqualified"), b"keep").unwrap();
    // A matching regular file is still just a name, not a claim that it is
    // a valid directory. Inspecting or following it is not this query's job.
    fs::write(&preserved, b"not a directory").unwrap();
    for name in [
        format!("another.preserved-{A}"),
        format!("store.restoring-{A}.tmp"),
        "store.preserved-not-a-uuid".into(),
        format!("store.preserved-{}", A.replace('-', "")),
        "store.restoring-ABCDEF00-0000-4000-8000-000000000001".into(),
        "store.restore-pending".into(),
        "store.preserved-".into(),
    ] {
        fs::write(parent.join(name), b"unrelated").unwrap();
    }
    let nested = paths.data_root().join(format!("store.preserved-{B}"));
    fs::create_dir(&nested).unwrap();

    let fs = Arc::new(FaultFs::new(real_fs(), FsOp::Create, 1, FaultKind::Eio));
    store.seams = test_seams(fs.clone());
    let expected = vec![
        RestoreArtifact {
            path: preserved.clone(),
            kind: RestoreArtifactKind::Preserved,
        },
        RestoreArtifact {
            path: staged.clone(),
            kind: RestoreArtifactKind::Staged,
        },
    ];
    assert_eq!(store.restore_artifacts().unwrap(), expected);
    assert_eq!(store.restore_artifacts().unwrap(), expected);
    assert_eq!(
        fs.calls(FsOp::Open),
        2,
        "only the parent per query was opened"
    );
    for op in [
        FsOp::Write,
        FsOp::Remove,
        FsOp::RemoveDirAll,
        FsOp::Rename,
        FsOp::Create,
    ] {
        assert_eq!(fs.calls(op), 0, "no contents read or mutation: {op:?}");
    }
    assert_eq!(std::fs::read(preserved).unwrap(), b"not a directory");
    assert_eq!(std::fs::read(staged.join("unqualified")).unwrap(), b"keep");
    store.close().unwrap();
}

#[test]
fn a_failed_enumeration_never_returns_a_partial_list() {
    for op in [FsOp::ReadDir, FsOp::Read] {
        let (_scratch, paths, mut store) = scratch("restore-artifacts-io");
        let parent = paths.data_root().parent().unwrap();
        for id in [A, B] {
            fs::create_dir(parent.join(format!("store.preserved-{id}"))).unwrap();
        }
        assert_eq!(fs::read_dir(parent).unwrap().count(), 4);
        // Fail before enumeration, or at the fourth child after at least
        // one matching name was visited (the others are the root and lock).
        let nth = if op == FsOp::Read { 4 } else { 1 };
        let fs = Arc::new(FaultFs::new(real_fs(), op, nth, FaultKind::Eio));
        store.seams = test_seams(fs.clone());
        let error = store.restore_artifacts().unwrap_err();
        assert!(
            matches!(
                error,
                StoreError::Unavailable {
                    io_kind: Some(_),
                    ..
                }
            ),
            "{error:?}"
        );
        assert_eq!(fs.calls(op), nth);
        assert_eq!(
            store.restore_artifacts().unwrap().len(),
            2,
            "a later refresh retries"
        );
        store.close().unwrap();
    }
}

#[test]
fn retained_names_remain_available_while_the_store_is_latched() {
    let (_scratch, paths, store) = scratch("restore-artifacts-latched");
    let path = paths
        .data_root()
        .parent()
        .unwrap()
        .join(format!("store.restoring-{A}"));
    fs::create_dir(&path).unwrap();
    store.close().unwrap();
    damage_an_index(paths.state_db());
    let store = open(&paths, &test_seams(real_fs())).unwrap();
    assert!(matches!(
        store.check_integrity(&AtomicBool::new(false)).unwrap(),
        IntegrityCheck::Finding(_)
    ));
    assert!(store.integrity_latch().is_some());
    assert_eq!(
        store.restore_artifacts().unwrap(),
        vec![RestoreArtifact {
            path,
            kind: RestoreArtifactKind::Staged
        }]
    );
    store.close().unwrap();
}

#[cfg(unix)]
#[test]
fn enumeration_uses_the_resolved_root_not_the_callers_alias() {
    let scratch = ScratchDir::new("restore-artifacts-root-alias");
    let parent = fs::canonicalize(scratch.path()).unwrap();
    let real = parent.join("real");
    fs::create_dir(&real).unwrap();
    std::os::unix::fs::symlink(&real, parent.join("alias")).unwrap();
    let paths = StorePaths::new(parent.join("alias/store")).unwrap();
    let store = open(&paths, &test_seams(real_fs())).unwrap();
    let path = real.join(format!("store.preserved-{A}"));
    fs::create_dir(&path).unwrap();
    fs::create_dir(parent.join(format!("store.preserved-{B}"))).unwrap();
    assert_eq!(
        store.restore_artifacts().unwrap(),
        vec![RestoreArtifact {
            path,
            kind: RestoreArtifactKind::Preserved
        }]
    );
    store.close().unwrap();
}

#[cfg(unix)]
#[test]
fn matching_symlinks_are_names_only_and_are_not_followed() {
    let (_scratch, paths, store) = scratch("restore-artifacts-links");
    let parent = paths.data_root().parent().unwrap();
    let outside = parent.join("outside");
    fs::create_dir(&outside).unwrap();
    fs::write(outside.join("keep"), b"untouched").unwrap();
    let path = parent.join(format!("store.preserved-{A}"));
    std::os::unix::fs::symlink(&outside, &path).unwrap();
    assert_eq!(
        store.restore_artifacts().unwrap(),
        vec![RestoreArtifact {
            path: path.clone(),
            kind: RestoreArtifactKind::Preserved
        }]
    );
    fs::remove_file(&path).unwrap();
    std::os::unix::fs::symlink(parent.join("absent"), &path).unwrap();
    assert_eq!(
        store.restore_artifacts().unwrap().len(),
        1,
        "even a dangling link is a name, not readiness"
    );
    fs::remove_file(path).unwrap();
    assert!(
        store.restore_artifacts().unwrap().is_empty(),
        "manual removal is reflected"
    );
    assert_eq!(fs::read(outside.join("keep")).unwrap(), b"untouched");
    store.close().unwrap();
}
