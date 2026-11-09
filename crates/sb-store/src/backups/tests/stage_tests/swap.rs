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
    assert_eq!(
        preserved.exists(),
        case == "the preserved name taken",
        "{case}: nothing renamed"
    );
    (*store, *stage)
}

#[test]
fn every_check_before_closing_refuses_with_the_store_handed_back() {
    type Interfere = fn(&StagingSource, &QualifiedStage);
    let cases: [(&str, Interfere); 4] = [
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
