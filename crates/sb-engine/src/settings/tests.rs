use std::fs;

use sb_resources::{BACKUP_DESTINATION_SETTING, WritePriority, read_all, upsert};
use sb_test_support::{ScratchDir, block_on};

use crate::{BackupEnd, Engine, EngineError};

const READING_VIEW_SENDERS: &str = "reader.reading_view_senders";

fn configured(engine: &Engine) -> super::Setting {
    block_on(engine.settings())
        .unwrap()
        .into_iter()
        .find(|setting| setting.key() == BACKUP_DESTINATION_SETTING)
        .unwrap()
}

fn reading_senders(engine: &Engine) -> super::Setting {
    block_on(engine.settings())
        .unwrap()
        .into_iter()
        .find(|setting| setting.key() == READING_VIEW_SENDERS)
        .unwrap()
}

#[test]
fn reading_view_senders_accept_only_the_canonical_list() {
    let scratch = ScratchDir::new("engine-reading-senders");
    let engine = block_on(Engine::open(scratch.path().join("store"))).unwrap();
    let default = reading_senders(&engine);
    assert_eq!(default.value(), "");
    assert!(default.is_default());
    let list = |count: usize| {
        (0..count)
            .map(|index| format!("user{index:04}@example.test"))
            .collect::<Vec<_>>()
            .join("\n")
    };
    for value in [
        "",
        "a@b",
        "a@b\nc@d",
        &format!("{}@x", "a".repeat(252)),
        &list(1_000),
    ] {
        block_on(engine.set_setting(&engine.new_idempotency_key(), READING_VIEW_SENDERS, value))
            .unwrap();
        assert_eq!(reading_senders(&engine).value(), value);
    }
    let last = reading_senders(&engine).value().to_owned();
    for value in [
        "A@b",
        "a@b@c",
        &format!("{}@x", "a".repeat(253)),
        "b@x\na@x",
        "a@b\na@b",
        "a@b\n",
        &list(1_001),
    ] {
        assert!(matches!(block_on(engine.set_setting(
                &engine.new_idempotency_key(),
                READING_VIEW_SENDERS,
                value
            )), Err(EngineError::InvalidInput { field, .. }) if field == "value"));
        assert_eq!(reading_senders(&engine).value(), last);
    }
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn notifications_previews_default_off_and_take_only_off_and_on() {
    let scratch = ScratchDir::new("engine-notifications-previews");
    let engine = block_on(Engine::open(scratch.path().join("store"))).unwrap();
    let previews = |engine: &Engine| {
        block_on(engine.settings())
            .unwrap()
            .into_iter()
            .find(|setting| setting.key() == "notifications.previews")
            .unwrap()
    };
    let setting = previews(&engine);
    assert_eq!(setting.value(), "off");
    assert!(setting.is_default());

    for value in ["on", "off"] {
        block_on(engine.set_setting(
            &engine.new_idempotency_key(),
            "notifications.previews",
            value,
        ))
        .unwrap();
        assert_eq!(previews(&engine).value(), value);
    }
    assert!(previews(&engine).is_default(), "set back to the default");
    let error = block_on(engine.set_setting(
        &engine.new_idempotency_key(),
        "notifications.previews",
        "maybe",
    ))
    .unwrap_err();
    assert!(
        matches!(error, EngineError::InvalidInput { ref field, .. } if field == "value"),
        "{error:?}"
    );

    // A stored value no allowed choice matches is Corrupt, never the
    // default (spec/02).
    let now = engine.resources().now();
    block_on(engine.resources().write(WritePriority::User, move |tx| {
        upsert(tx, "notifications.previews", "null", now)
    }))
    .unwrap();
    assert!(matches!(
        block_on(engine.settings()),
        Err(EngineError::Corrupt { detail }) if detail.contains("notifications.previews")
    ));
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn malformed_stored_reading_senders_is_corrupt() {
    let scratch = ScratchDir::new("engine-reading-senders-corrupt");
    let engine = block_on(Engine::open(scratch.path().join("store"))).unwrap();
    for raw in [
        "null",
        r#""A@b""#,
        r#""a@b\na@b""#,
        r#""a@b\n""#,
        r#""\/noncanonical""#,
    ] {
        let now = engine.resources().now();
        block_on(engine.resources().write(WritePriority::User, {
            let raw = raw.to_owned();
            move |tx| upsert(tx, READING_VIEW_SENDERS, &raw, now)
        }))
        .unwrap();
        assert!(
            matches!(block_on(engine.settings()), Err(EngineError::Corrupt { detail }) if detail.contains(READING_VIEW_SENDERS))
        );
    }
    block_on(engine.shutdown()).unwrap();
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

/// `reader.mark_read` (spec/02, M3.2): `immediately` by default, one of
/// the registry's choices, anything else `InvalidInput`.
#[test]
fn reader_mark_read_defaults_to_immediately_and_takes_its_choices() {
    let scratch = ScratchDir::new("engine-reader-mark-read");
    let engine = block_on(Engine::open(scratch.path().join("store"))).unwrap();
    let mark_read = |engine: &Engine| {
        block_on(engine.settings())
            .unwrap()
            .into_iter()
            .find(|setting| setting.key() == "reader.mark_read")
            .unwrap()
    };
    assert_eq!(mark_read(&engine).value(), "immediately");
    assert!(mark_read(&engine).is_default());

    for value in ["2", "5", "10", "manual", "immediately"] {
        block_on(engine.set_setting(&engine.new_idempotency_key(), "reader.mark_read", value))
            .unwrap();
        assert_eq!(mark_read(&engine).value(), value);
    }
    for value in ["0", "3", "never", ""] {
        assert!(matches!(
            block_on(engine.set_setting(&engine.new_idempotency_key(), "reader.mark_read", value)),
            Err(EngineError::InvalidInput { field, .. }) if field == "value"
        ));
    }
    block_on(engine.shutdown()).unwrap();
}
