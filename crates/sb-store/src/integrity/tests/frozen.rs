//! M1.8h5b1: a latched store's own file work refuses, a damaged object
//! stays where a read found it, and the latch's observer hears once.

use std::collections::BTreeMap;
use std::ops::ControlFlow;
use std::sync::atomic::AtomicUsize;

use super::*;
use crate::backups::BackupDestination;
use crate::{ObjectId, ObjectStore};

/// Every directory under the data root, and every file but the
/// volatile `-shm` with its digest.
fn tree(paths: &StorePaths) -> BTreeMap<PathBuf, Option<String>> {
    let mut found = BTreeMap::new();
    let mut pending = vec![paths.data_root().to_path_buf()];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            let relative = path.strip_prefix(paths.data_root()).unwrap().to_path_buf();
            if path.is_dir() {
                pending.push(path);
                found.insert(relative, None);
            } else if !relative.ends_with("state.sqlite-shm") {
                found.insert(relative, Some(sha256(&path)));
            }
        }
    }
    found
}

/// `op` is refused `Corrupt` naming the finding, and leaves the data
/// root as it was.
fn assert_frozen<T: std::fmt::Debug>(
    paths: &StorePaths,
    what: &str,
    op: impl FnOnce() -> Result<T, StoreError>,
) {
    let before = tree(paths);
    match op() {
        Err(StoreError::Corrupt { detail }) => {
            assert!(detail.contains("probe_pad"), "{what}: {detail}");
        }
        other => panic!("{what}: expected Corrupt, got {other:?}"),
    }
    assert_eq!(tree(paths), before, "{what} touched the data root");
}

/// Everything that would create, write, move or remove a file in the
/// data root outside the writer.
fn refuse_all_file_work(
    store: &Store,
    paths: &StorePaths,
    kept: &ObjectId,
    generation: &str,
    source: &Path,
) {
    assert_frozen(paths, "publish", || {
        store.objects().publish(&b"a new object"[..]).map(drop)
    });
    assert_frozen(paths, "remove", || store.objects().remove(kept));
    assert_frozen(paths, "backup", || {
        store.backup_snapshot(BackupDestination::DataRoot).map(drop)
    });
    assert_frozen(paths, "capture", || {
        store
            .capture_import_source(source, |_| ControlFlow::Continue(()))
            .map(drop)
    });
    assert_frozen(paths, "remove sources", || store.remove_import_sources(&[]));
    assert_frozen(paths, "report", || {
        store.write_import_report("job", b"report")
    });
    assert_frozen(paths, "scratch", || {
        store.import_validation_scratch().map(drop)
    });
    assert_frozen(paths, "verify", || {
        store
            .verify_recorded_backup(generation, &AtomicBool::new(false))
            .map(drop)
    });
}

/// From the finding on, and after a latched reopen, the store's own
/// file work is refused before it touches anything: the data root
/// stays as it was, file for file.
#[test]
fn a_latched_store_touches_no_file_in_its_root() {
    let (scratch, paths, store) = open_with_probe("integrity-frozen");
    let kept = *store.objects().publish(&b"a kept object"[..]).unwrap().id();
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let complete = store.complete_backup(snapshot).unwrap();
    let generation = complete.generation_id().to_owned();
    store.close().unwrap();
    damage_the_index(&paths, true);
    let source = scratch.path().join("source.mbox");
    std::fs::write(&source, b"From a@example.com\n\nbody\n").unwrap();

    let store = reopen(&paths);
    // A generation running when the store latches can't complete.
    let running = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let IntegrityCheck::Finding(finding) = check(&store) else {
        panic!("no finding");
    };
    finding.latch.unwrap();
    finding.copy.unwrap();
    assert_frozen(&paths, "complete", || {
        store.complete_backup(running).map(drop)
    });
    refuse_all_file_work(&store, &paths, &kept, &generation, &source);
    store.close().unwrap();

    let store = reopen(&paths);
    refuse_all_file_work(&store, &paths, &kept, &generation, &source);
    store.close().unwrap();
}

/// A read that finds an object damaged reports it, but a latched store
/// leaves the file where it is instead of quarantining it.
#[test]
fn a_latched_store_leaves_a_damaged_object_where_it_is() {
    let (_scratch, paths, store) = open_with_probe("integrity-no-quarantine");
    let short = *store
        .objects()
        .publish(&b"an object cut short"[..])
        .unwrap()
        .id();
    let altered = *store
        .objects()
        .publish(&b"an object rewritten"[..])
        .unwrap()
        .id();
    let short_path = store.objects().object_path(&short);
    let altered_path = store.objects().object_path(&altered);
    store.close().unwrap();
    damage_the_index(&paths, true);
    std::fs::write(&short_path, b"cut").unwrap();
    std::fs::write(&altered_path, b"AN OBJECT REWRITTEN").unwrap();

    let store = reopen(&paths);
    let IntegrityCheck::Finding(_) = check(&store) else {
        panic!("no finding");
    };
    // A length the id contradicts, seen at open.
    assert!(matches!(
        store.objects().open(&short),
        Err(StoreError::Corrupt { .. })
    ));
    // Bytes the hash contradicts, seen at the end of the read.
    let mut reader = store.objects().open(&altered).unwrap();
    let error = io::copy(&mut reader, &mut io::sink()).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);

    assert_eq!(std::fs::read(&short_path).unwrap(), b"cut");
    assert_eq!(
        std::fs::read(&altered_path).unwrap(),
        b"AN OBJECT REWRITTEN"
    );
    let quarantined: Vec<_> = std::fs::read_dir(paths.recovery())
        .unwrap()
        .filter_map(|entry| entry.unwrap().file_name().into_string().ok())
        .filter(|name| name.starts_with("object-"))
        .collect();
    assert!(quarantined.is_empty(), "{quarantined:?}");
    store.close().unwrap();
}

/// A latch waits for a step of file work in flight, and no step starts
/// after it.
#[test]
fn a_latch_waits_for_the_file_step_in_flight() {
    let latch = Arc::new(RootLatch::default());
    let step = latch.step("work").unwrap();
    let latching = std::thread::spawn({
        let latch = Arc::clone(&latch);
        move || latch.latch_for_test("a test finding")
    });
    std::thread::sleep(std::time::Duration::from_millis(200));
    assert!(!latching.is_finished(), "the step is still in flight");
    drop(step);
    latching.join().unwrap();
    match latch.step("work") {
        Err(StoreError::Corrupt { detail }) => assert!(detail.contains("a test finding")),
        other => panic!("expected Corrupt, got {:?}", other.map(drop)),
    }
}

/// An observer installed once the store has latched, by a check or at
/// a latched open, is told at once, and only once.
#[test]
fn an_observer_installed_after_the_latch_is_told_at_once() {
    let (_scratch, paths, store) = open_with_probe("integrity-late-observer");
    store.close().unwrap();
    damage_the_index(&paths, true);
    let store = reopen(&paths);
    let IntegrityCheck::Finding(_) = check(&store) else {
        panic!("no finding");
    };
    let counting = |heard: &Arc<AtomicUsize>| -> Arc<dyn Fn() + Send + Sync> {
        let heard = Arc::clone(heard);
        Arc::new(move || {
            heard.fetch_add(1, Ordering::SeqCst);
        })
    };
    let heard = Arc::new(AtomicUsize::new(0));
    store.observe_integrity_latch(counting(&heard));
    assert_eq!(heard.load(Ordering::SeqCst), 1);
    assert!(store.check_integrity(&AtomicBool::new(false)).is_err());
    assert_eq!(heard.load(Ordering::SeqCst), 1);
    store.close().unwrap();

    let store = reopen(&paths);
    let heard = Arc::new(AtomicUsize::new(0));
    store.observe_integrity_latch(counting(&heard));
    assert_eq!(heard.load(Ordering::SeqCst), 1);
    store.close().unwrap();
}

/// The observer hears once, as the store latches and before the latch
/// file is written; one that panics changes nothing.
#[test]
fn the_latch_observer_hears_once_and_cannot_break_the_latch() {
    for panics in [false, true] {
        let (_scratch, paths, store) = open_with_probe("integrity-observer");
        store.close().unwrap();
        damage_the_index(&paths, true);
        let store = reopen(&paths);
        let heard = Arc::new(AtomicUsize::new(0));
        let seen_latch_file = Arc::new(AtomicBool::new(false));
        store.observe_integrity_latch(Arc::new({
            let heard = Arc::clone(&heard);
            let seen_latch_file = Arc::clone(&seen_latch_file);
            let latch_file = latch_path(&paths);
            move || {
                heard.fetch_add(1, Ordering::SeqCst);
                seen_latch_file.store(latch_file.exists(), Ordering::SeqCst);
                assert!(!panics, "an observer that panics");
            }
        }));

        let IntegrityCheck::Finding(finding) = check(&store) else {
            panic!("no finding");
        };
        finding.latch.unwrap();
        finding.copy.unwrap();
        assert_eq!(heard.load(Ordering::SeqCst), 1);
        assert!(!seen_latch_file.load(Ordering::SeqCst));
        assert!(store.integrity_latch().is_some());
        // A second check is refused and tells no one.
        assert!(matches!(
            store.check_integrity(&AtomicBool::new(false)),
            Err(StoreError::Corrupt { .. })
        ));
        assert_eq!(heard.load(Ordering::SeqCst), 1);
        store.close().unwrap();
    }
}
