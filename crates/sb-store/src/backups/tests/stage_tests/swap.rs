//! Native proof for the restore swap (M1.8f2): a qualified stage
//! replaces the live store and reopens under the lock that was never
//! released, with every account paused and the old root preserved;
//! every check before the store closes refuses with the store and the
//! stage handed back unchanged.
use super::*;
use crate::{QualifiedStage, SwapRestoreError, open};

fn qualified(source: &StagingSource) -> QualifiedStage {
    let prepared = prepare(
        &source.store,
        source.select_with(&source.store),
        source.receipt.location(),
        source.metadata,
    );
    source
        .store
        .qualify_restore(prepared, &Arc::new(AtomicBool::new(false)))
        .unwrap()
}

fn meta(store: &Store, key: &str) -> String {
    let key = key.to_owned();
    reading(store, move |tx| {
        tx.query_row(
            "SELECT value FROM store_meta WHERE key = ?1",
            [&key],
            |row| row.get(0),
        )
        .map_err(bug)
    })
}

fn file_meta(path: &Path, key: &str) -> String {
    let connection =
        Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let value = connection
        .query_row(
            "SELECT value FROM store_meta WHERE key = ?1",
            [key],
            |row| row.get(0),
        )
        .unwrap();
    connection.close().unwrap();
    value
}

#[test]
fn a_qualified_stage_swaps_in_and_reopens_under_the_kept_lock() {
    let source = staging_source("swap-happy");
    let paths = StorePaths::new(source.paths.data_root()).unwrap();
    let stage = qualified(&source);
    let (incarnation, stage_path, preserved) = (
        stage.incarnation().to_owned(),
        stage.path().to_path_buf(),
        stage.preserved_root().to_path_buf(),
    );
    let old_incarnation = source.live_incarnation();
    let store_uuid = meta(&source.store, "store_uuid");
    let StagingSource {
        _scratch, store, ..
    } = source;

    let restored = store.swap_restore(stage, &AtomicBool::new(false)).unwrap();

    // The restored store is the snapshot under a new incarnation, with
    // every account paused (quarantine).
    assert_eq!(meta(&restored, "incarnation"), incarnation);
    assert_eq!(meta(&restored, "store_uuid"), store_uuid);
    let unpaused: i64 = reading(&restored, |tx| {
        tx.query_row(
            "SELECT count(*) FROM accounts WHERE state <> 'paused'",
            [],
            |row| row.get(0),
        )
        .map_err(bug)
    });
    assert_eq!(unpaused, 0, "every account paused");
    // The old root is preserved by name, never deleted; the stage name
    // is gone and so is the marker.
    assert_eq!(
        file_meta(&preserved.join("state.sqlite"), "incarnation"),
        old_incarnation
    );
    assert!(!stage_path.exists());
    assert!(!crate::restore_pending::marker_path(&paths).exists());
    // The lock was held throughout and still is.
    let seams = crate::testing::test_seams(real_fs());
    assert!(matches!(open(&paths, &seams), Err(StoreError::Locked)));

    restored.close().unwrap();
    let reopened = open(&paths, &seams).unwrap();
    assert_eq!(meta(&reopened, "incarnation"), incarnation);
    reopened.close().unwrap();
}

/// Each refusal before the store closes hands back a usable store and
/// the stage, with nothing renamed.
fn assert_refused(
    result: Result<Store, SwapRestoreError>,
    case: &str,
    stage_path: &Path,
    preserved: &Path,
    old_incarnation: &str,
) -> (Store, QualifiedStage) {
    let Err(SwapRestoreError::Refused {
        cause,
        store,
        stage,
    }) = result
    else {
        panic!("{case}: expected a refusal, got {result:?}");
    };
    assert!(
        matches!(
            cause,
            StoreError::Conflict { .. }
                | StoreError::Corrupt { .. }
                | StoreError::Unavailable { .. }
        ),
        "{case}: {cause:?}"
    );
    assert_eq!(
        meta(&store, "incarnation"),
        old_incarnation,
        "{case}: still the old store"
    );
    assert!(
        stage_path.join("state.sqlite").is_file(),
        "{case}: the stage stays"
    );
    // Only an entry the case itself put at the preserved name is there.
    assert_eq!(
        std::fs::symlink_metadata(preserved).is_ok(),
        case.contains("preserved name"),
        "{case}: nothing renamed"
    );
    (*store, *stage)
}

#[test]
fn every_check_before_closing_refuses_with_the_store_handed_back() {
    type Interfere = fn(&StagingSource, &QualifiedStage);
    let cases: [(&str, Interfere); 6] = [
        ("a changed live incarnation", |source, _| {
            sql(
                &source.store,
                "UPDATE store_meta SET value='00000000-0000-4000-8000-0000000000f1' \
                 WHERE key='incarnation'"
                    .to_owned(),
            )
        }),
        ("a changed image byte", |_, stage| {
            let path = stage.path().join("state.sqlite");
            let mut bytes = std::fs::read(&path).unwrap();
            let last = bytes.len() - 1;
            bytes[last] ^= 1;
            std::fs::write(&path, bytes).unwrap();
        }),
        ("a damaged staged object", |source, stage| {
            let (sha, _) = &source.payloads[0];
            let object = stage.path().join(format!(
                "objects/sha256/{}/{}/{}",
                &sha[..2],
                &sha[2..4],
                sha
            ));
            let mut bytes = std::fs::read(&object).unwrap();
            bytes[0] ^= 1;
            std::fs::write(&object, bytes).unwrap();
        }),
        ("the preserved name taken", |_, stage| {
            std::fs::create_dir(stage.preserved_root()).unwrap();
        }),
        ("a dangling link at the preserved name", |_, stage| {
            std::os::unix::fs::symlink("/nonexistent-target", stage.preserved_root()).unwrap();
        }),
        ("a file at the preserved name", |_, stage| {
            std::fs::write(stage.preserved_root(), b"not a root").unwrap();
        }),
    ];
    for (case, interfere) in cases {
        let source = staging_source("swap-refused");
        let stage = qualified(&source);
        let (stage_path, preserved) = (
            stage.path().to_path_buf(),
            stage.preserved_root().to_path_buf(),
        );
        let old_incarnation = source.live_incarnation();
        interfere(&source, &stage);
        let old_incarnation = if case == "a changed live incarnation" {
            source.live_incarnation()
        } else {
            old_incarnation
        };
        let StagingSource {
            _scratch, store, ..
        } = source;

        let result = store.swap_restore(stage, &AtomicBool::new(false));

        let (store, _stage) =
            assert_refused(result, case, &stage_path, &preserved, &old_incarnation);
        store.close().unwrap();
    }

    // Cancelled before it starts: refused the same way.
    let source = staging_source("swap-cancelled");
    let stage = qualified(&source);
    let (stage_path, preserved) = (
        stage.path().to_path_buf(),
        stage.preserved_root().to_path_buf(),
    );
    let old_incarnation = source.live_incarnation();
    let StagingSource {
        _scratch, store, ..
    } = source;
    let result = store.swap_restore(stage, &AtomicBool::new(true));
    let (store, _stage) = assert_refused(
        result,
        "cancelled",
        &stage_path,
        &preserved,
        &old_incarnation,
    );
    store.close().unwrap();

    // Another store's stage: refused by the coordinator check.
    let source = staging_source("swap-foreign");
    let other = staging_source("swap-foreign-other");
    let stage = qualified(&source);
    let (stage_path, preserved) = (
        stage.path().to_path_buf(),
        stage.preserved_root().to_path_buf(),
    );
    let other_incarnation = other.live_incarnation();
    let StagingSource {
        _scratch: _other_scratch,
        store: other_store,
        ..
    } = other;
    let result = other_store.swap_restore(stage, &AtomicBool::new(false));
    let (other_store, _stage) = assert_refused(
        result,
        "another store's stage",
        &stage_path,
        &preserved,
        &other_incarnation,
    );
    other_store.close().unwrap();
    drop(source);
}

/// The authority and name checks each refuse on their own: a stage
/// whose lease is not the running operation, a replaced stage
/// directory, and a replaced staged database.
#[test]
fn a_forged_lease_or_a_replaced_stage_name_is_refused() {
    let replace = |path: &Path| {
        let aside = path.with_extension("aside");
        std::fs::rename(path, &aside).unwrap();
        if aside.is_dir() {
            std::fs::create_dir(path).unwrap();
            for entry in std::fs::read_dir(&aside).unwrap() {
                let entry = entry.unwrap();
                if entry.path().is_file() {
                    std::fs::copy(entry.path(), path.join(entry.file_name())).unwrap();
                }
            }
        } else {
            std::fs::copy(&aside, path).unwrap();
        }
    };
    type Forge = fn(&mut QualifiedStage, &dyn Fn(&Path));
    let cases: [(&str, &str, Forge); 3] = [
        (
            "a forged lease",
            "operation is not the running one",
            |stage, _| {
                stage.operation = Arc::new(());
            },
        ),
        (
            "a replaced stage directory",
            "restore stage was replaced",
            |stage, replace| {
                replace(stage.path());
            },
        ),
        (
            "a replaced staged database",
            "staged database was replaced",
            |stage, replace| {
                replace(&stage.path().join("state.sqlite"));
            },
        ),
    ];
    for (case, detail, forge) in cases {
        let source = staging_source("swap-forged");
        let mut stage = qualified(&source);
        let (stage_path, preserved) = (
            stage.path().to_path_buf(),
            stage.preserved_root().to_path_buf(),
        );
        let old_incarnation = source.live_incarnation();
        forge(&mut stage, &replace);
        let StagingSource {
            _scratch, store, ..
        } = source;

        let result = store.swap_restore(stage, &AtomicBool::new(false));

        let Err(SwapRestoreError::Refused { cause, store, .. }) = result else {
            panic!("{case}: expected a refusal, got {result:?}");
        };
        assert!(
            matches!(&cause, StoreError::Conflict { detail: found } if found.contains(detail)),
            "{case}: {cause:?}"
        );
        assert_eq!(meta(&store, "incarnation"), old_incarnation, "{case}");
        assert!(
            stage_path.exists() && !preserved.exists(),
            "{case}: nothing renamed"
        );
        store.close().unwrap();
    }
}

/// A failure once the store has closed is `Failed`: here the marker
/// cannot be written, so nothing moved, and the next open is the old
/// store with the stage left as evidence.
#[test]
fn a_failure_after_closing_reports_failed_and_the_next_open_resolves_it() {
    let source = staging_source("swap-failed");
    let paths = StorePaths::new(source.paths.data_root()).unwrap();
    let stage = qualified(&source);
    let stage_path = stage.path().to_path_buf();
    let old_incarnation = source.live_incarnation();
    // A directory under the marker's temporary name: the marker write
    // fails after the store closed.
    let mut temporary = crate::restore_pending::marker_path(&paths).into_os_string();
    temporary.push(".tmp");
    std::fs::create_dir(PathBuf::from(&temporary)).unwrap();
    let StagingSource {
        _scratch, store, ..
    } = source;

    let result = store.swap_restore(stage, &AtomicBool::new(false));

    assert!(
        matches!(result, Err(SwapRestoreError::Failed(_))),
        "{result:?}"
    );
    assert!(!crate::restore_pending::marker_path(&paths).exists());
    assert!(stage_path.join("state.sqlite").is_file(), "the stage stays");
    // The lock went with the failed swap; the old store opens.
    std::fs::remove_dir(PathBuf::from(&temporary)).unwrap();
    let reopened = open(&paths, &crate::testing::test_seams(real_fs())).unwrap();
    assert_eq!(meta(&reopened, "incarnation"), old_incarnation);
    reopened.close().unwrap();
}
