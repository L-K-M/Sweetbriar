use std::fs;

use sb_test_support::{ScratchDir, real_fs};

use super::*;
use crate::testing::{test_seams, writing};
use crate::{BackupDestination, StartedBackup, Store, StorePaths, Timestamp, open};

const KEY: &str = "00000000-0000-4000-8000-0000000000d1";
const OTHER: &str = "00000000-0000-4000-8000-0000000000d2";

fn setup(label: &str) -> (ScratchDir, PathBuf, Store) {
    let scratch = ScratchDir::new(label);
    let parent = fs::canonicalize(scratch.path()).unwrap();
    let paths = StorePaths::new(parent.join("store")).unwrap();
    let store = open(&paths, &test_seams(real_fs())).unwrap();
    (scratch, parent, store)
}

fn raw_setting(store: &Store, raw: String) {
    writing(store, move |tx| {
        crate::settings::upsert(
            tx,
            BACKUP_DESTINATION_SETTING,
            &raw,
            Timestamp::from_millis(1),
        )
    });
}

fn setting(store: &Store, path: &str) {
    raw_setting(store, serde_json::to_string(path).unwrap());
}

fn complete(store: &Store, key: &str) -> crate::Receipt {
    let StartedBackup::Started { receipt, snapshot } = store
        .start_backup(BackupDestination::Configured, key)
        .unwrap()
    else {
        panic!("a fresh key starts a generation");
    };
    store.complete_backup(snapshot).unwrap();
    receipt
}

#[test]
fn destination_syntax_is_absolute_or_the_empty_default_and_never_nul() {
    for value in ["", "/", "/missing/with \"quotes\" \\ and café"] {
        assert!(valid_backup_destination(value), "{value:?}");
    }
    for value in ["relative", "~/Backups", "../other", "/bad\0path"] {
        assert!(!valid_backup_destination(value), "{value:?}");
    }
}

#[test]
fn default_selected_and_cleared_destinations_leave_old_locations_unchanged() {
    let (_scratch, parent, store) = setup("backup-destination-default");
    let receipt = complete(&store, KEY);
    let id = crate::receipt_generation_id(&receipt).unwrap();
    let before = store.backup_generation(&id).unwrap().unwrap();
    assert_eq!(
        before.location.parent().unwrap(),
        parent.join("store/backups")
    );
    let selected = parent.join("backup copies \"quoted\" \\ café");
    fs::create_dir(&selected).unwrap();
    setting(&store, selected.to_str().unwrap());
    assert_eq!(store.backup_generation(&id).unwrap().unwrap(), before);
    let snapshot = store
        .backup_snapshot(BackupDestination::Configured)
        .unwrap();
    assert_eq!(snapshot.location().parent().unwrap(), selected);
    assert_eq!(store.backup_generation(&id).unwrap().unwrap(), before);
    store.complete_backup(snapshot).unwrap();
    setting(&store, "");
    let snapshot = store
        .backup_snapshot(BackupDestination::Configured)
        .unwrap();
    assert_eq!(
        snapshot.location().parent().unwrap(),
        parent.join("store/backups")
    );
    store.complete_backup(snapshot).unwrap();
    store.close().unwrap();
}

#[test]
fn a_missing_chosen_volume_never_gets_created_and_spent_keys_still_replay() {
    let (_scratch, parent, store) = setup("backup-destination-missing");
    let volume = parent.join("volume");
    let selected = volume.join("backups");
    fs::create_dir_all(&selected).unwrap();
    setting(&store, selected.to_str().unwrap());
    let receipt = complete(&store, KEY);
    fs::remove_dir_all(&volume).unwrap();
    assert!(
        matches!(store.start_backup(BackupDestination::Configured, KEY).unwrap(), StartedBackup::Replayed(replayed) if replayed == receipt)
    );
    assert!(matches!(
        store.start_backup(BackupDestination::Configured, OTHER),
        Err(StoreError::Unavailable {
            io_kind: Some(io::ErrorKind::NotFound),
            ..
        })
    ));
    assert!(!volume.exists(), "never recreate an absent mount");
    assert_eq!(
        fs::read_dir(parent.join("store/backups")).unwrap().count(),
        0,
        "never fall back"
    );
    assert_eq!(
        store.backup_generations().unwrap().len(),
        1,
        "no new generation row"
    );

    // Receipt lookup precedes even decoding the current setting.
    raw_setting(&store, "not json".into());
    assert!(
        matches!(store.start_backup(BackupDestination::Configured, KEY).unwrap(), StartedBackup::Replayed(replayed) if replayed == receipt)
    );
    assert!(matches!(
        store.start_backup(BackupDestination::Configured, OTHER),
        Err(StoreError::Corrupt { .. })
    ));
    setting(&store, selected.to_str().unwrap());
    fs::create_dir(&volume).unwrap();
    fs::write(&selected, b"not a directory").unwrap();
    assert!(matches!(
        store.start_backup(BackupDestination::Configured, OTHER),
        Err(StoreError::Unavailable {
            io_kind: Some(io::ErrorKind::NotADirectory),
            ..
        })
    ));
    assert_eq!(fs::read(&selected).unwrap(), b"not a directory");
    assert_eq!(
        fs::read_dir(parent.join("store/backups")).unwrap().count(),
        0
    );
    store.close().unwrap();
}

#[test]
fn malformed_stored_destinations_are_corrupt_not_defaults() {
    let (_scratch, parent, store) = setup("backup-destination-corrupt");
    for raw in [
        "null",
        "3",
        "[]",
        "not json",
        r#""relative""#,
        r#""/bad\u0000path""#,
        r#""\/noncanonical""#,
    ] {
        raw_setting(&store, raw.into());
        let error = store
            .backup_snapshot(BackupDestination::Configured)
            .unwrap_err();
        assert!(
            matches!(error, StoreError::Corrupt { .. }),
            "{raw:?}: {error:?}"
        );
        assert!(store.backup_generations().unwrap().is_empty());
        assert_eq!(
            fs::read_dir(parent.join("store/backups")).unwrap().count(),
            0
        );
    }
    store.close().unwrap();
}

#[cfg(unix)]
#[test]
fn a_chosen_alias_is_recorded_with_its_canonical_location() {
    let (_scratch, parent, store) = setup("backup-destination-alias");
    let selected = parent.join("actual");
    fs::create_dir(&selected).unwrap();
    let alias = parent.join("alias");
    std::os::unix::fs::symlink(&selected, &alias).unwrap();
    setting(&store, alias.to_str().unwrap());
    let snapshot = store
        .backup_snapshot(BackupDestination::Configured)
        .unwrap();
    assert_eq!(snapshot.location().parent().unwrap(), selected);
    store.complete_backup(snapshot).unwrap();
    store.close().unwrap();
}
