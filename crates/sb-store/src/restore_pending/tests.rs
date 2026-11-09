//! The restore-pending marker (spec/03 "Restore" step 4): its codec,
//! its atomic and durable write, every startup state through `recover`
//! and through the real `open`, power cuts that drop unflushed renames,
//! and fired faults that leave a resolvable state.

use sb_test_support::{FaultFs, FaultKind, FsOp, ScratchDir, SimFs, real_fs};

use super::*;
use crate::testing::test_seams;
use crate::{open, open_read_only};

const NEW: &str = "00000000-0000-4000-8000-0000000000a1";
const OLD: &str = "00000000-0000-4000-8000-0000000000b2";
const GENERATION: &str = "00000000-0000-4000-8000-0000000000c3";

/// A data root `<scratch>/store`, resolved as `open` resolves it.
fn scratch(label: &str) -> (ScratchDir, StorePaths) {
    let scratch = ScratchDir::new(label);
    let parent = std::fs::canonicalize(scratch.path()).unwrap();
    let paths = StorePaths::new(parent.join("store")).unwrap();
    (scratch, paths)
}

fn pending(paths: &StorePaths) -> PendingRestore {
    PendingRestore::new(paths.data_root(), NEW, GENERATION, OLD).unwrap()
}

fn sibling_path(paths: &StorePaths, name: &OsStr) -> PathBuf {
    paths.data_root().parent().unwrap().join(name)
}

fn assert_corrupt<T: std::fmt::Debug>(result: Result<T, StoreError>, case: &str) {
    assert!(
        matches!(result, Err(StoreError::Corrupt { .. })),
        "{case}: {result:?}"
    );
}

#[test]
fn the_marker_names_the_derived_siblings_and_round_trips() {
    let (_scratch, paths) = scratch("pending-codec");
    let record = pending(&paths);
    assert_eq!(
        record.staging,
        OsString::from(format!("store.restoring-{NEW}"))
    );
    assert_eq!(
        record.preserved,
        OsString::from(format!("store.preserved-{OLD}"))
    );

    let bytes = record.encode();
    assert_eq!(
        PendingRestore::decode(paths.data_root(), &bytes).unwrap(),
        record
    );
    // Another data root's marker does not decode here.
    let other = StorePaths::new(paths.data_root().with_file_name("other")).unwrap();
    assert_corrupt(
        PendingRestore::decode(other.data_root(), &bytes),
        "another root",
    );
}

#[test]
fn a_malformed_marker_is_refused() {
    let (_scratch, paths) = scratch("pending-malformed");
    let good = String::from_utf8(pending(&paths).encode()).unwrap();
    let cases: Vec<(&str, String)> = vec![
        ("empty", String::new()),
        ("no final newline", good.trim_end().to_owned()),
        ("another version", good.replacen(" 1\n", " 2\n", 1)),
        ("an extra line", format!("{good}extra value\n")),
        (
            "a missing line",
            good.lines()
                .take(4)
                .map(|line| format!("{line}\n"))
                .collect(),
        ),
        (
            "an uppercase UUID",
            good.replacen(NEW, &NEW.to_uppercase(), 2),
        ),
        (
            "a staging name off its incarnation",
            good.replacen(
                &format!("staging store.restoring-{NEW}"),
                &format!("staging store.restoring-{OLD}"),
                1,
            ),
        ),
        (
            "a preserved name for another root",
            good.replacen(
                "preserved store.preserved-",
                "preserved other.preserved-",
                1,
            ),
        ),
        (
            "a path in a name",
            good.replacen(
                &format!("staging store.restoring-{NEW}"),
                &format!("staging ../store.restoring-{NEW}"),
                1,
            ),
        ),
        ("the same incarnation twice", good.replace(OLD, NEW)),
        (
            "not UTF-8",
            String::from_utf8_lossy(&[0xff, 0xfe]).into_owned(),
        ),
    ];
    for (case, text) in cases {
        assert_corrupt(
            PendingRestore::decode(paths.data_root(), text.as_bytes()),
            case,
        );
    }
    assert!(PendingRestore::new(paths.data_root(), NEW, GENERATION, NEW).is_err());
    assert!(PendingRestore::new(paths.data_root(), "not-a-uuid", GENERATION, OLD).is_err());
}

#[test]
fn writing_the_marker_is_atomic_and_durable() {
    let fs = SimFs::new();
    let paths = StorePaths::new("/data/store").unwrap();
    fs.create_dir_all(Path::new("/data/store")).unwrap();
    // A stale temporary from an interrupted write is replaced.
    let temporary = PathBuf::from("/data/store.restore-pending.tmp");
    fs.create(&temporary).unwrap().write_all(b"torn").unwrap();

    write_marker(&fs, &paths, &pending(&paths)).unwrap();
    fs.power_cut();

    assert!(
        fs.metadata(&temporary).unwrap().is_none(),
        "no temporary left"
    );
    let bytes = read_marker(&fs, &marker_path(&paths)).unwrap().unwrap();
    assert_eq!(
        PendingRestore::decode(paths.data_root(), &bytes).unwrap(),
        pending(&paths)
    );
}

#[test]
fn a_marker_that_is_no_file_or_under_an_unusable_root_is_corrupt() {
    let (_scratch, paths) = scratch("pending-not-a-file");
    std::fs::create_dir(marker_path(&paths)).unwrap();
    assert_corrupt(recover(real_fs().as_ref(), &paths), "a directory marker");

    // No restore writes a marker under a root it cannot name.
    let (_scratch, paths) = scratch("pending-control-root");
    let odd = StorePaths::new(paths.data_root().with_file_name("st\nore")).unwrap();
    assert!(matches!(
        PendingRestore::new(odd.data_root(), NEW, GENERATION, OLD),
        Err(StoreError::InvalidInput { .. })
    ));
    let bytes = pending(&paths).encode();
    assert_corrupt(
        PendingRestore::decode(odd.data_root(), &bytes),
        "a control character root",
    );
}

#[test]
fn an_oversized_marker_is_refused_unread_past_its_bound() {
    let (_scratch, paths) = scratch("pending-oversized");
    std::fs::create_dir(paths.data_root()).unwrap();
    std::fs::write(marker_path(&paths), vec![b'x'; 4096]).unwrap();
    assert_corrupt(recover(real_fs().as_ref(), &paths), "oversized");
    assert!(marker_path(&paths).exists(), "a refused marker stays");
}

/// Directories standing in for the roots: each holds a file naming it
/// and a `state.sqlite` stand-in.
fn make_dir(path: &Path, label: &str) {
    std::fs::create_dir(path).unwrap();
    std::fs::write(path.join("which"), label).unwrap();
    std::fs::write(path.join("state.sqlite"), label).unwrap();
}

fn which(path: &Path) -> String {
    std::fs::read_to_string(path.join("which")).unwrap()
}

#[test]
fn startup_resolves_every_marker_state() {
    let fs = real_fs();

    // No marker: nothing to do.
    let (_s0, paths) = scratch("pending-none");
    assert_eq!(recover(fs.as_ref(), &paths).unwrap(), Recovered::Nothing);

    // The first rename landed: the stage is renamed into place.
    let (_s1, paths) = scratch("pending-half");
    let record = pending(&paths);
    make_dir(&sibling_path(&paths, &record.staging), "restored");
    make_dir(&sibling_path(&paths, &record.preserved), "old");
    write_marker(fs.as_ref(), &paths, &record).unwrap();
    assert_eq!(recover(fs.as_ref(), &paths).unwrap(), Recovered::Completed);
    assert_eq!(which(paths.data_root()), "restored");
    assert_eq!(which(&sibling_path(&paths, &record.preserved)), "old");
    assert!(!sibling_path(&paths, &record.staging).exists());
    assert!(!marker_path(&paths).exists());

    // Nothing was renamed yet: the stage stays as evidence.
    let (_s2, paths) = scratch("pending-not-started");
    let record = pending(&paths);
    make_dir(paths.data_root(), "old");
    make_dir(&sibling_path(&paths, &record.staging), "restored");
    write_marker(fs.as_ref(), &paths, &record).unwrap();
    assert_eq!(recover(fs.as_ref(), &paths).unwrap(), Recovered::NotStarted);
    assert_eq!(which(paths.data_root()), "old");
    assert_eq!(which(&sibling_path(&paths, &record.staging)), "restored");
    assert!(!marker_path(&paths).exists());

    // Both renames landed: only the marker remained.
    let (_s3, paths) = scratch("pending-finished");
    let record = pending(&paths);
    make_dir(paths.data_root(), "restored");
    make_dir(&sibling_path(&paths, &record.preserved), "old");
    write_marker(fs.as_ref(), &paths, &record).unwrap();
    assert_eq!(recover(fs.as_ref(), &paths).unwrap(), Recovered::Finished);
    assert_eq!(which(paths.data_root()), "restored");
    assert!(!marker_path(&paths).exists());

    // A stage without its database never replaces the root.
    let (_s5, paths) = scratch("pending-emptied");
    let record = pending(&paths);
    make_dir(&sibling_path(&paths, &record.staging), "emptied");
    std::fs::remove_file(sibling_path(&paths, &record.staging).join("state.sqlite")).unwrap();
    make_dir(&sibling_path(&paths, &record.preserved), "old");
    write_marker(fs.as_ref(), &paths, &record).unwrap();
    assert_corrupt(recover(fs.as_ref(), &paths), "an emptied stage");
    assert!(!paths.data_root().exists());
    assert!(sibling_path(&paths, &record.staging).is_dir());
    assert!(marker_path(&paths).exists());

    // Neither root: refused, nothing created, the marker kept.
    let (_s4, paths) = scratch("pending-neither");
    let record = pending(&paths);
    make_dir(&sibling_path(&paths, &record.preserved), "old");
    write_marker(fs.as_ref(), &paths, &record).unwrap();
    assert_corrupt(recover(fs.as_ref(), &paths), "neither root");
    assert!(!paths.data_root().exists());
    assert!(marker_path(&paths).exists());
}

#[test]
fn a_linked_swap_name_is_refused_unfollowed() {
    let fs = real_fs();
    let (_scratch, paths) = scratch("pending-linked");
    let record = pending(&paths);
    let elsewhere = paths.data_root().with_file_name("elsewhere");
    make_dir(&elsewhere, "elsewhere");
    std::os::unix::fs::symlink(&elsewhere, sibling_path(&paths, &record.staging)).unwrap();
    write_marker(fs.as_ref(), &paths, &record).unwrap();

    assert!(recover(fs.as_ref(), &paths).is_err());

    assert!(
        !paths.data_root().exists(),
        "the link was never renamed into place"
    );
    assert!(marker_path(&paths).exists());
    assert_eq!(which(&elsewhere), "elsewhere");
}

/// A durable `state.sqlite` stand-in inside a simulated stage.
fn sim_stage(fs: &SimFs, staging: &Path) {
    fs.create_dir_all(staging).unwrap();
    let database = staging.join("state.sqlite");
    let mut file = fs.create(&database).unwrap();
    file.write_all(b"stage").unwrap();
    file.sync_all().unwrap();
    fs.sync_dir(staging).unwrap();
}

#[test]
fn a_power_cut_after_an_unflushed_first_rename_still_finds_the_marker() {
    let fs = SimFs::new();
    let paths = StorePaths::new("/data/store").unwrap();
    let record = pending(&paths);
    let staging = sibling_path(&paths, &record.staging);
    let preserved = sibling_path(&paths, &record.preserved);
    fs.create_dir_all(paths.data_root()).unwrap();
    sim_stage(&fs, &staging);

    write_marker(&fs, &paths, &record).unwrap();
    fs.rename(paths.data_root(), &preserved).unwrap();
    // The rename's directory entry was never flushed.
    fs.power_cut();

    assert!(
        fs.metadata(paths.data_root()).unwrap().is_some(),
        "the rename was lost"
    );
    assert_eq!(recover(&fs, &paths).unwrap(), Recovered::NotStarted);
    assert!(fs.metadata(&marker_path(&paths)).unwrap().is_none());

    // Flushed, the same rename survives and startup finishes the swap.
    let fs = SimFs::new();
    fs.create_dir_all(paths.data_root()).unwrap();
    sim_stage(&fs, &staging);
    write_marker(&fs, &paths, &record).unwrap();
    fs.rename(paths.data_root(), &preserved).unwrap();
    fs.sync_dir(Path::new("/data")).unwrap();
    fs.power_cut();
    assert_eq!(recover(&fs, &paths).unwrap(), Recovered::Completed);
    fs.power_cut();
    assert!(
        fs.metadata(paths.data_root()).unwrap().is_some(),
        "durable once finished"
    );
    assert!(fs.metadata(&staging).unwrap().is_none());
    assert!(fs.metadata(&marker_path(&paths)).unwrap().is_none());
}

/// A finishing recovery's state: stage and preserved root present.
fn half_done(label: &str) -> (ScratchDir, StorePaths) {
    let (scratch, paths) = scratch(label);
    let record = pending(&paths);
    make_dir(&sibling_path(&paths, &record.staging), "restored");
    make_dir(&sibling_path(&paths, &record.preserved), "old");
    write_marker(real_fs().as_ref(), &paths, &record).unwrap();
    (scratch, paths)
}

/// Every operation of a finishing recovery, failed in turn (each count
/// calibrated on a clean run): the error keeps its cause, and a later
/// recovery resolves what is left.
#[test]
fn a_failed_recovery_step_is_resolved_by_the_next_open() {
    let denied = FaultKind::Kind(io::ErrorKind::PermissionDenied);
    for operation in [
        FsOp::Open,
        FsOp::Read,
        FsOp::Rename,
        FsOp::SyncDir,
        FsOp::Remove,
    ] {
        let (_calibration, paths) = half_done("pending-fault-count");
        let counting = FaultFs::new(real_fs(), operation, u64::MAX, denied);
        assert_eq!(recover(&counting, &paths).unwrap(), Recovered::Completed);
        let total = counting.calls(operation);
        assert!(total > 0, "{operation:?} occurs in a finishing recovery");
        for nth in 1..=total {
            let (_scratch, paths) = half_done("pending-fault");
            let fault = FaultFs::new(real_fs(), operation, nth, denied);

            let first = recover(&fault, &paths);

            assert_eq!(
                first.unwrap_err().io_kind(),
                Some(io::ErrorKind::PermissionDenied),
                "{operation:?} #{nth} of {total}"
            );
            let again = recover(real_fs().as_ref(), &paths).unwrap();
            assert!(
                // A failed last flush comes after the marker went.
                matches!(
                    again,
                    Recovered::Completed | Recovered::Finished | Recovered::Nothing
                ),
                "{operation:?} #{nth}: {again:?}"
            );
            assert_eq!(which(paths.data_root()), "restored", "{operation:?} #{nth}");
            assert!(!marker_path(&paths).exists(), "{operation:?} #{nth}");
        }
    }

    // The marker's own open, refused by the file system.
    let (_scratch, paths) = half_done("pending-fault-marker");
    use std::os::unix::fs::PermissionsExt;
    std::fs::set_permissions(marker_path(&paths), std::fs::Permissions::from_mode(0o000)).unwrap();
    assert_eq!(
        recover(real_fs().as_ref(), &paths).unwrap_err().io_kind(),
        Some(io::ErrorKind::PermissionDenied)
    );
    std::fs::set_permissions(marker_path(&paths), std::fs::Permissions::from_mode(0o600)).unwrap();
    assert_eq!(
        recover(real_fs().as_ref(), &paths).unwrap(),
        Recovered::Completed
    );
}

/// The real open: a store staged beside the live one replaces it when
/// the first rename had landed; otherwise the old store opens; neither
/// root is refused without creating one.
#[test]
fn open_finishes_or_drops_an_interrupted_swap() {
    let seams = test_seams(real_fs());
    let store_uuid = |paths: &StorePaths| -> String {
        let store = open(paths, &seams).unwrap();
        let uuid = crate::testing::reading(&store, |tx| {
            tx.query_row(
                "SELECT value FROM store_meta WHERE key='store_uuid'",
                [],
                |row| row.get(0),
            )
            .map_err(crate::schema::bug)
        });
        store.close().unwrap();
        uuid
    };

    // The live store and a store at the staging name, both closed.
    let (_scratch, paths) = scratch("pending-open");
    let record = pending(&paths);
    let staging = sibling_path(&paths, &record.staging);
    let preserved = sibling_path(&paths, &record.preserved);
    let live_uuid = store_uuid(&paths);
    let staged_paths = StorePaths::new(&staging).unwrap();
    let staged_uuid = store_uuid(&staged_paths);
    assert_ne!(live_uuid, staged_uuid);

    // Not started: the old store opens, the stage stays.
    write_marker(real_fs().as_ref(), &paths, &record).unwrap();
    assert!(matches!(
        open_read_only(&paths, &seams),
        Err(StoreError::Unavailable { .. })
    ));
    assert!(
        marker_path(&paths).exists(),
        "the read-only open changes nothing"
    );
    assert_eq!(store_uuid(&paths), live_uuid);
    assert!(staging.is_dir());
    assert!(!marker_path(&paths).exists());

    // Half done: the stage replaces the root, the old root is preserved;
    // the read-only open refuses even with the data root gone.
    write_marker(real_fs().as_ref(), &paths, &record).unwrap();
    std::fs::rename(paths.data_root(), &preserved).unwrap();
    assert!(matches!(
        open_read_only(&paths, &seams),
        Err(StoreError::Unavailable { .. })
    ));
    assert_eq!(store_uuid(&paths), staged_uuid);
    assert!(preserved.join("state.sqlite").is_file(), "nothing deleted");
    assert!(!staging.exists());
    assert!(!marker_path(&paths).exists());

    // Finished: only the marker remained; the restored store opens.
    write_marker(real_fs().as_ref(), &paths, &record).unwrap();
    assert_eq!(store_uuid(&paths), staged_uuid);
    assert!(!marker_path(&paths).exists());

    // Neither root: refused, and no empty store appears.
    write_marker(real_fs().as_ref(), &paths, &record).unwrap();
    std::fs::rename(paths.data_root(), paths.data_root().with_file_name("moved")).unwrap();
    assert!(matches!(
        open(&paths, &seams),
        Err(StoreError::Corrupt { .. })
    ));
    assert!(!paths.data_root().exists(), "never creates an empty store");
}
