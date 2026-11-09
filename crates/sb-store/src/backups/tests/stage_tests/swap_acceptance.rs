//! M1.8f's acceptance over the real swap: a restored store holds its
//! generation's rows and objects; a generation inside the old data root
//! and one outside it both resolve afterwards; back up G, restore G,
//! restart and back up again keeps G complete, intact and restorable;
//! no second open gets in at any step; and a process killed anywhere in
//! the swap, or in the startup that finishes it, leaves the old store
//! or the quarantined restored one, never an unquarantined or empty one.
use std::collections::{BTreeMap, BTreeSet};

use super::swap::{file_meta, meta, qualified};
use super::*;
use crate::restore_pending::marker_path;
use crate::store::SwapPhase;
use crate::{QualifiedStage, open};
use sb_test_support::{CutFs, run_crash_cuts};

/// Stage and qualify `generation`, found at `location`, into `store`.
fn qualify_from(store: &Store, generation: &str, location: &Path) -> QualifiedStage {
    let selection = store
        .verify_external_backup(
            location.parent().unwrap(),
            generation,
            &AtomicBool::new(false),
        )
        .unwrap()
        .unwrap();
    let metadata = restore_metadata(store, generation);
    let prepared = prepare(store, selection, location, metadata);
    store
        .qualify_restore(prepared, &Arc::new(AtomicBool::new(false)))
        .unwrap()
}

/// Every table's row count, by name.
fn row_counts(connection: &Connection) -> BTreeMap<String, i64> {
    let names: Vec<String> = connection
        .prepare("SELECT name FROM sqlite_schema WHERE type='table' ORDER BY name")
        .unwrap()
        .query_map([], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    names
        .into_iter()
        .map(|name| {
            let count = connection
                .query_row(&format!("SELECT count(*) FROM \"{name}\""), [], |row| {
                    row.get(0)
                })
                .unwrap();
            (name, count)
        })
        .collect()
}

/// Every object file under `root`'s object tree, each re-hashed against
/// its name.
fn object_hashes(root: &Path) -> BTreeSet<String> {
    let children = |dir: &Path| -> Vec<PathBuf> {
        std::fs::read_dir(dir)
            .unwrap()
            .map(|entry| entry.unwrap().path())
            .collect()
    };
    let mut found = BTreeSet::new();
    for first in children(&root.join("objects/sha256")) {
        for second in children(&first) {
            for object in children(&second) {
                let name = object.file_name().unwrap().to_str().unwrap().to_owned();
                assert_eq!(sha256_hex(&std::fs::read(&object).unwrap()), name);
                found.insert(name);
            }
        }
    }
    found
}

/// The object hashes a generation's manifest lists.
fn manifest_objects(location: &Path) -> BTreeSet<String> {
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(location.join("manifest.json")).unwrap()).unwrap();
    manifest["objects"]
        .as_array()
        .unwrap()
        .iter()
        .map(|object| object["sha256"].as_str().unwrap().to_owned())
        .collect()
}

/// Restored from an external generation whose snapshot also records
/// one inside the data root: the rows and objects are the snapshot's,
/// and both generations verify where the restored rows locate them.
#[test]
fn a_restored_store_keeps_its_rows_and_objects_and_both_generations_resolve() {
    let source = staging_source("swap-identical");
    let paths = StorePaths::new(source.paths.data_root()).unwrap();
    let inside = source.receipt.generation_id().to_owned();
    let external_parent = std::fs::canonicalize(source._scratch.path())
        .unwrap()
        .join("external");
    let snapshot = source
        .store
        .backup_snapshot(BackupDestination::Directory(external_parent))
        .unwrap();
    let external = source.store.complete_backup(snapshot).unwrap();
    let snapshot_counts = {
        let connection = Connection::open_with_flags(
            external.state_db(),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let counts = row_counts(&connection);
        connection.close().unwrap();
        counts
    };
    let expected_objects = manifest_objects(external.location());
    let payloads: BTreeSet<String> = source.payloads.iter().map(|(sha, _)| sha.clone()).collect();
    assert_eq!(expected_objects, payloads);
    let stage = qualify_from(&source.store, external.generation_id(), external.location());
    let preserved = stage.preserved_root().to_path_buf();
    let StagingSource {
        _scratch, store, ..
    } = source;

    let restored = store.swap_restore(stage, &AtomicBool::new(false)).unwrap();

    // Row for row the snapshot, but for the one row the quarantine
    // removes: the ownership token of the generation the snapshot still
    // held in progress, its own, which only interrupted-backup cleanup
    // reads.
    let mut expected_counts = snapshot_counts;
    *expected_counts.get_mut("backup_ownership").unwrap() -= 1;
    assert_eq!(reading(&restored, |tx| Ok(row_counts(tx))), expected_counts);
    let own_token: i64 = reading(&restored, |tx| {
        tx.query_row(
            "SELECT count(*) FROM backup_ownership WHERE generation_id=?1",
            [external.generation_id()],
            |row| row.get(0),
        )
        .map_err(bug)
    });
    assert_eq!(own_token, 0);
    assert_eq!(object_hashes(paths.data_root()), expected_objects);
    // The generation inside the old data root moved with it; the
    // external one stayed where it was. Both verify there.
    let (state, location, _) = generation_row(&restored, &inside).unwrap();
    assert_eq!(state, "complete");
    assert!(Path::new(&location).starts_with(&preserved), "{location}");
    let (state, location, _) = generation_row(&restored, external.generation_id()).unwrap();
    assert_eq!(state, "complete");
    assert_eq!(Path::new(&location), external.location());
    for generation in [inside.as_str(), external.generation_id()] {
        restored
            .verify_recorded_backup(generation, &AtomicBool::new(false))
            .unwrap();
    }
    restored.close().unwrap();
}

/// Back up G, restore G, restart and back up again (plan M1.8f): G
/// stays complete, intact and restorable, nothing it inherited blocks
/// GC, and a second open is refused at every step of the swap.
#[test]
fn back_up_restore_restart_and_back_up_again_keeps_the_generation() {
    let source = staging_source("swap-round-trip");
    let paths = StorePaths::new(source.paths.data_root()).unwrap();
    let generation = source.receipt.generation_id().to_owned();
    let manifest_sha256 = source.metadata.manifest_sha256;
    let stage = qualified(&source);
    let (incarnation, preserved) = (
        stage.incarnation().to_owned(),
        stage.preserved_root().to_path_buf(),
    );
    let StagingSource {
        _scratch, store, ..
    } = source;
    let seams = crate::testing::test_seams(real_fs());

    let mut phases = Vec::new();
    let restored = store
        .swap_restore_with(stage, &AtomicBool::new(false), |phase| {
            assert!(
                matches!(open(&paths, &seams), Err(StoreError::Locked)),
                "{phase:?}: the lock is held"
            );
            phases.push(phase);
        })
        .unwrap();
    assert_eq!(
        phases,
        [SwapPhase::Checked, SwapPhase::Closed, SwapPhase::Committed]
    );
    restored.close().unwrap();

    let reopened = open(&paths, &seams).unwrap();
    assert_eq!(meta(&reopened, "incarnation"), incarnation);
    let (state, location, _) = generation_row(&reopened, &generation).unwrap();
    assert_eq!(state, "complete");
    let location = PathBuf::from(location);
    assert!(location.starts_with(&preserved), "{location:?}");

    let snapshot = reopened
        .backup_snapshot(BackupDestination::DataRoot)
        .unwrap();
    let again = reopened.complete_backup(snapshot).unwrap();
    assert_eq!(
        generation_row(&reopened, again.generation_id()).unwrap().0,
        "complete"
    );
    assert_eq!(
        generation_row(&reopened, &generation).unwrap().0,
        "complete"
    );
    assert_eq!(
        restore_metadata(&reopened, &generation).manifest_sha256,
        manifest_sha256
    );
    for id in [generation.as_str(), again.generation_id()] {
        reopened
            .verify_recorded_backup(id, &AtomicBool::new(false))
            .unwrap();
    }
    assert_eq!(
        reopened.collect_garbage(GC_GRACE_PERIOD).unwrap().blocked,
        None,
        "nothing inherited blocks GC"
    );
    // Still restorable: G stages and qualifies again from where it is.
    drop(qualify_from(&reopened, &generation, &location));
    reopened.close().unwrap();
}

// --- process cuts through the swap and through the startup after it ---

const SWAP_CUT_N: &str = "SB_SWAP_CUT_N";
const SWAP_CUT_ROOT: &str = "SB_SWAP_CUT_ROOT";
const SWAP_CUT_CHILD: &str = "backups::tests::stage_tests::swap_acceptance::swap_cut_child";
const STARTUP_CUT_CHILD: &str = "backups::tests::stage_tests::swap_acceptance::startup_cut_child";

/// Counts the whole run's filesystem operations from the store's
/// first, writes where the swap starts and the count after each of its
/// steps to the report directory, and swaps.
#[test]
#[ignore = "spawned by process_cuts_leave_the_old_store_or_the_quarantined_restored_one"]
fn swap_cut_child() {
    let nth = env::var(SWAP_CUT_N).unwrap().parse().unwrap();
    let report = PathBuf::from(env::var(SWAP_CUT_ROOT).unwrap());
    let cuts = Arc::new(CutFs::new(real_fs(), nth));
    let source = staging_source_on("swap-cut-child", Arc::clone(&cuts) as Arc<dyn Fs>);
    let stage = qualified(&source);
    let expected = serde_json::json!({
        "scratch": source._scratch.path(),
        "root": source.paths.data_root(),
        "stage": stage.path(),
        "preserved": stage.preserved_root(),
        "old": source.live_incarnation(),
        "new": stage.incarnation(),
        "start": cuts.operations(),
    });
    std::fs::write(
        report.join("expected.json"),
        serde_json::to_vec(&expected).unwrap(),
    )
    .unwrap();
    let StagingSource {
        _scratch, store, ..
    } = source;
    let mut counts = Vec::new();
    let restored = store
        .swap_restore_with(stage, &AtomicBool::new(false), |_| {
            counts.push(cuts.operations());
        })
        .unwrap();
    counts.push(cuts.operations());
    std::fs::write(
        report.join("counts.json"),
        serde_json::to_vec(&counts).unwrap(),
    )
    .unwrap();
    restored.close().unwrap();
}

/// Opens (and closes) the copied store whose swap stopped after its
/// first rename, through a cutting filesystem.
#[test]
#[ignore = "spawned by process_cuts_leave_the_old_store_or_the_quarantined_restored_one"]
fn startup_cut_child() {
    let nth = env::var(SWAP_CUT_N).unwrap().parse().unwrap();
    let root = PathBuf::from(env::var(SWAP_CUT_ROOT).unwrap());
    let paths = StorePaths::new(root.join("store")).unwrap();
    let store = open(
        &paths,
        &crate::testing::test_seams(Arc::new(CutFs::new(real_fs(), nth))),
    )
    .unwrap();
    store.close().unwrap();
}

/// What a killed run left, once the next open resolved it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Resolved {
    Old,
    Restored,
}

/// Open the leftover root as the next start would and require the old
/// store with its stage kept, or the quarantined restored store with
/// the old root preserved; the marker is gone either way.
fn resolve(paths: &StorePaths, stage: &Path, preserved: &Path, old: &str, new: &str) -> Resolved {
    let store = open(paths, &crate::testing::test_seams(real_fs())).unwrap();
    let incarnation = meta(&store, "incarnation");
    let resolved = if incarnation == old {
        assert!(stage.join("state.sqlite").is_file(), "the stage stays");
        assert!(!preserved.exists());
        Resolved::Old
    } else {
        assert_eq!(incarnation, new, "neither the old nor the restored store");
        let (accounts, paused): (i64, i64) = reading(&store, |tx| {
            tx.query_row(
                "SELECT count(*), count(*) FILTER (WHERE state = 'paused') FROM accounts",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(bug)
        });
        // The fixture's account and the seeded local archive.
        assert_eq!(
            (accounts, paused),
            (2, 2),
            "the restored store opens quarantined"
        );
        assert_eq!(
            file_meta(&preserved.join("state.sqlite"), "incarnation"),
            old
        );
        assert!(!stage.exists());
        Resolved::Restored
    };
    assert!(!marker_path(paths).exists());
    store.close().unwrap();
    resolved
}

/// Copy a directory tree (directories and regular files only).
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), target).unwrap();
        }
    }
}

#[test]
fn process_cuts_leave_the_old_store_or_the_quarantined_restored_one() {
    let scratch = ScratchDir::new("swap-process-cuts");
    let baseline = scratch.path().join("baseline");
    let calibration = run_crash_cut_points(
        SWAP_CUT_N,
        SWAP_CUT_ROOT,
        SWAP_CUT_CHILD,
        &[u64::MAX],
        &mut |_| {
            std::fs::create_dir(&baseline).unwrap();
            baseline.clone()
        },
        &mut |_| panic!("calibration must survive"),
    );
    assert_eq!(calibration.survived, [u64::MAX]);
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(baseline.join("expected.json")).unwrap()).unwrap();
    let start = report["start"].as_u64().unwrap();
    // After the checks, after closing, after committing, after reopening.
    let counts: Vec<u64> =
        serde_json::from_slice(&std::fs::read(baseline.join("counts.json")).unwrap()).unwrap();
    let [checked, closed, committed, reopened] = counts[..] else {
        panic!("one count per step and one after reopening: {counts:?}");
    };
    assert!(checked > start, "the checks do real I/O");
    assert!(committed > closed, "the commit does real I/O");
    assert!(reopened > committed, "the reopen does real I/O");
    // The first, middle and last check; every operation from closing
    // through the commit (the marker, both renames, their flushes and
    // the marker's removal); the first, middle and last of the reopen.
    let mut points = vec![start + 1, start + (checked - start).div_ceil(2), checked];
    points.extend(checked + 1..=committed);
    points.extend([
        committed + 1,
        committed + (reopened - committed).div_ceil(2),
        reopened,
    ]);
    points.dedup();
    assert!(points.windows(2).all(|pair| pair[0] < pair[1]));

    let mut seen = Vec::new();
    let mut template: Option<PathBuf> = None;
    let result = run_crash_cut_points(
        SWAP_CUT_N,
        SWAP_CUT_ROOT,
        SWAP_CUT_CHILD,
        &points,
        &mut |n| {
            let path = scratch.path().join(format!("cut-{n}"));
            std::fs::create_dir(&path).unwrap();
            path
        },
        &mut |report_root| {
            let report: serde_json::Value =
                serde_json::from_slice(&std::fs::read(report_root.join("expected.json")).unwrap())
                    .unwrap();
            // Every run reaches the swap after the calibrated count of
            // operations, so each cut lands where it was aimed.
            assert_eq!(report["start"].as_u64(), Some(start), "the run drifted");
            let text = |key: &str| report[key].as_str().unwrap().to_owned();
            let paths = StorePaths::new(text("root")).unwrap();
            let child_scratch = PathBuf::from(text("scratch"));
            // The first run killed between the two renames leaves the
            // state the startup cuts below start from.
            if template.is_none() && !paths.data_root().exists() {
                let copy = scratch.path().join("between-renames");
                copy_tree(&child_scratch, &copy);
                template = Some(copy);
            }
            seen.push(resolve(
                &paths,
                Path::new(&text("stage")),
                Path::new(&text("preserved")),
                &text("old"),
                &text("new"),
            ));
            assert!(
                child_scratch
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .contains("swap-cut-child")
            );
            std::fs::remove_dir_all(child_scratch).unwrap();
        },
    );
    assert_eq!(result.killed, points, "every cut point killed the child");
    assert!(
        seen.contains(&Resolved::Old) && seen.contains(&Resolved::Restored),
        "cuts landed on both sides of the swap: {seen:?}"
    );

    // Startup after a kill between the renames: wherever this start is
    // killed in turn, the one after it finishes the swap.
    let template = template.expect("a cut landed between the two renames");
    let sibling = |prefix: &str| -> PathBuf {
        std::fs::read_dir(&template)
            .unwrap()
            .map(|entry| PathBuf::from(entry.unwrap().file_name()))
            .find(|name| name.to_str().unwrap().starts_with(prefix))
            .unwrap_or_else(|| panic!("no {prefix} sibling in {template:?}"))
    };
    let (stage_name, preserved_name) = (sibling("store.restoring-"), sibling("store.preserved-"));
    let old = file_meta(
        &template.join(&preserved_name).join("state.sqlite"),
        "incarnation",
    );
    let new = file_meta(
        &template.join(&stage_name).join("state.sqlite"),
        "incarnation",
    );
    let mut seen = Vec::new();
    let startup_cuts = run_crash_cuts(
        SWAP_CUT_N,
        SWAP_CUT_ROOT,
        STARTUP_CUT_CHILD,
        &mut |n| {
            let copy = scratch.path().join(format!("startup-{n}"));
            copy_tree(&template, &copy);
            copy
        },
        &mut |copy| {
            let paths = StorePaths::new(copy.join("store")).unwrap();
            seen.push(resolve(
                &paths,
                &copy.join(&stage_name),
                &copy.join(&preserved_name),
                &old,
                &new,
            ));
        },
    );
    assert!(startup_cuts > 0, "the startup was cut");
    assert!(
        seen.iter().all(|resolved| *resolved == Resolved::Restored),
        "a start after the first rename always finishes the swap: {seen:?}"
    );
}
