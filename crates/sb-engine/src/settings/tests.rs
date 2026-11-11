use std::fs;

use sb_resources::{BACKUP_DESTINATION_SETTING, WritePriority, read_all, upsert};
use sb_test_support::{ScratchDir, block_on};

use crate::{BackupEnd, Engine, EngineError};

fn configured(engine: &Engine) -> super::Setting {
    block_on(engine.settings())
        .unwrap()
        .into_iter()
        .find(|setting| setting.key() == BACKUP_DESTINATION_SETTING)
        .unwrap()
}

#[test]
fn backup_destination_strings_round_trip_canonically_survive_reopen_and_clear() {
    let scratch = ScratchDir::new("engine-destination-setting");
    let root = scratch.path().join("store");
    let engine = block_on(Engine::open(&root)).unwrap();
    let default = configured(&engine);
    assert_eq!(default.value(), "");
    assert!(default.is_default());
    // No filesystem availability check while saving a setting; the backup
    // checks it. Quotes, backslashes, Unicode and newline are JSON data.
    let value = "/not-mounted/with \"quotes\" \\ café\nfolder";
    block_on(engine.set_setting(
        &engine.new_idempotency_key(),
        BACKUP_DESTINATION_SETTING,
        value,
    ))
    .unwrap();
    assert_eq!(configured(&engine).value(), value);
    assert!(!configured(&engine).is_default());
    let rows = engine.resources().read(read_all).unwrap();
    let raw = rows
        .iter()
        .find(|row| row.key() == BACKUP_DESTINATION_SETTING)
        .unwrap()
        .value_json();
    assert_eq!(raw, serde_json::to_string(value).unwrap());
    block_on(engine.shutdown()).unwrap();

    let engine = block_on(Engine::open(&root)).unwrap();
    assert_eq!(configured(&engine).value(), value);
    block_on(engine.set_setting(
        &engine.new_idempotency_key(),
        BACKUP_DESTINATION_SETTING,
        "",
    ))
    .unwrap();
    assert!(configured(&engine).is_default());
    assert_eq!(configured(&engine).value(), "");
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn invalid_destination_syntax_spends_no_key() {
    let scratch = ScratchDir::new("engine-destination-invalid");
    let engine = block_on(Engine::open(scratch.path().join("store"))).unwrap();
    let key = engine.new_idempotency_key();
    for value in ["relative", "~/Backups", "/bad\0path"] {
        assert!(
            matches!(block_on(engine.set_setting(&key, BACKUP_DESTINATION_SETTING, value)), Err(EngineError::InvalidInput { field, .. }) if field == "value")
        );
        assert!(configured(&engine).is_default());
    }
    let receipt =
        block_on(engine.set_setting(&key, BACKUP_DESTINATION_SETTING, "/unmounted/valid")).unwrap();
    assert_eq!(receipt.command(), "set_setting");
    assert_eq!(configured(&engine).value(), "/unmounted/valid");
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn malformed_stored_destination_is_not_silently_defaulted() {
    let scratch = ScratchDir::new("engine-destination-corrupt");
    let engine = block_on(Engine::open(scratch.path().join("store"))).unwrap();
    for raw in [
        "null",
        "3",
        r#""relative""#,
        r#""/bad\u0000path""#,
        r#""\/noncanonical""#,
    ] {
        let raw = raw.to_owned();
        let now = engine.resources().now();
        block_on(engine.resources().write(WritePriority::User, move |tx| {
            upsert(tx, BACKUP_DESTINATION_SETTING, &raw, now)
        }))
        .unwrap();
        assert!(
            matches!(block_on(engine.settings()), Err(EngineError::Corrupt { detail }) if detail.contains(BACKUP_DESTINATION_SETTING))
        );
        assert!(matches!(
            block_on(engine.start_backup(&engine.new_idempotency_key())),
            Err(EngineError::Corrupt { .. })
        ));
        assert!(block_on(engine.list_backups()).unwrap().is_empty());
    }
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn requested_backups_use_the_setting_and_replay_after_the_volume_disappears() {
    let scratch = ScratchDir::new("engine-destination-backup");
    let parent = fs::canonicalize(scratch.path()).unwrap();
    let engine = block_on(Engine::open(parent.join("store"))).unwrap();
    let volume = parent.join("volume");
    let selected = volume.join("backups");
    fs::create_dir_all(&selected).unwrap();
    block_on(engine.set_setting(
        &engine.new_idempotency_key(),
        BACKUP_DESTINATION_SETTING,
        selected.to_str().unwrap(),
    ))
    .unwrap();
    let key = engine.new_idempotency_key();
    let started = block_on(engine.start_backup(&key)).unwrap();
    let (id, receipt) = (
        started.generation_id().to_owned(),
        started.receipt().clone(),
    );
    assert_eq!(
        block_on(started.into_copy().unwrap().finished()),
        BackupEnd::Complete { after: None }
    );
    let listed = block_on(engine.list_backups()).unwrap();
    assert_eq!(listed[0].location, selected.join(&id));
    fs::remove_dir_all(&volume).unwrap();
    let replayed = block_on(engine.start_backup(&key)).unwrap();
    assert_eq!(replayed.receipt(), &receipt);
    assert!(replayed.into_copy().is_none());
    assert!(matches!(
        block_on(engine.start_backup(&engine.new_idempotency_key())),
        Err(EngineError::Unavailable { .. })
    ));
    assert!(!volume.exists());
    assert_eq!(
        fs::read_dir(parent.join("store/backups")).unwrap().count(),
        0
    );
    assert_eq!(block_on(engine.list_backups()).unwrap().len(), 1);
    block_on(engine.shutdown()).unwrap();
}
