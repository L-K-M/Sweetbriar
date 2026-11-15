//! Local tag and note command tests (spec/02, spec/07 Messages):
//! logical-message attachment, replay, window selections, restore.

use std::collections::BTreeSet;
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use sb_resources::{BackupDestination, StoreError, WritePriority, refresh_list_rows};
use sb_test_support::block_on;

use crate::views::tests::{save_view, scratch_engine, seed_archive};
use crate::{Anchor, Engine, MessageSelection, ViewSort, WindowSpec};

/// Mark one occurrence vanished and refresh its list rows.
fn vanish(engine: &Engine, occurrence: i64) {
    block_on(engine.resources().write(WritePriority::User, move |tx| {
        tx.execute(
            "UPDATE occurrences SET state = 'vanished' WHERE occurrence_id = ?1",
            [occurrence],
        )
        .map_err(|error| StoreError::internal(error.to_string()))?;
        refresh_list_rows(tx, &BTreeSet::from([occurrence]))?;
        Ok(())
    }))
    .unwrap();
}

/// Stored row counts.
fn counts(engine: &Engine) -> (i64, i64, i64) {
    engine
        .resources()
        .read(|tx| {
            let count = |sql: &str| {
                tx.query_row(sql, [], |row| row.get(0))
                    .map_err(|error| StoreError::internal(error.to_string()))
            };
            Ok((
                count("SELECT count(*) FROM message_tags")?,
                count("SELECT count(*) FROM tags")?,
                count("SELECT count(*) FROM notes")?,
            ))
        })
        .unwrap()
}

/// Tags and notes attach to the logical message: they survive its only
/// occurrence vanishing, a repeated key replays without duplicating
/// anything, a window token resolves its membership (a stale
/// generation is `ResetRequired`), and removal takes them away.
#[test]
fn tags_and_notes_follow_the_logical_message() {
    let (_scratch, engine) = scratch_engine("tags-vanish");
    seed_archive(&engine);

    let first_key = engine.new_idempotency_key();
    let selection = MessageSelection::OccurrenceIds(vec![11, 12]);
    let added = block_on(engine.add_tag(&first_key, selection.clone(), "work")).unwrap();
    assert!(added.result_json().contains(r#""added":2"#));
    let note_key = engine.new_idempotency_key();
    let note = block_on(engine.set_note(&note_key, 1, "a note")).unwrap();
    assert_eq!(
        note,
        block_on(engine.set_note(&note_key, 1, "a note")).unwrap()
    );
    let message = block_on(engine.get_message(1)).unwrap();
    assert_eq!(
        (message.tags[0].name.as_str(), message.note.as_deref()),
        ("work", Some("a note"))
    );

    let replayed = block_on(engine.add_tag(&first_key, selection, "work")).unwrap();
    assert_eq!(replayed.result_json(), added.result_json());
    assert_eq!(counts(&engine).0, 2);

    vanish(&engine, 11);
    let vanished = block_on(engine.get_message(1)).unwrap();
    assert!(vanished.occurrences.is_empty());
    assert_eq!(
        (vanished.tags.len(), vanished.note.as_deref()),
        (1, Some("a note"))
    );

    // A window token names the mailbox's membership.
    let window = block_on(engine.open_window(WindowSpec::Mailbox(1), Anchor::Top, 10)).unwrap();
    let receipt = block_on(engine.add_tag(
        &engine.new_idempotency_key(),
        MessageSelection::Window {
            window_id: window.window_id().get(),
            generation: window.generation(),
        },
        "later",
    ))
    .unwrap();
    assert!(
        receipt.result_json().contains(r#""added":2"#),
        "{receipt:?}"
    );

    // Removing the tag keeps the definition; a repeated key replays.
    let remove_key = engine.new_idempotency_key();
    let target = MessageSelection::OccurrenceIds(vec![11]);
    let removed = block_on(engine.remove_tag(&remove_key, target.clone(), "work")).unwrap();
    assert!(removed.result_json().contains(r#""removed":1"#));
    assert_eq!(
        removed,
        block_on(engine.remove_tag(&remove_key, target, "work")).unwrap()
    );
    block_on(engine.set_note(&engine.new_idempotency_key(), 1, "")).unwrap();
    let cleaned = block_on(engine.get_message(1)).unwrap();
    assert!(cleaned.tags.is_empty() && cleaned.note.is_none());
    assert_eq!(counts(&engine), (3, 2, 0));
    block_on(engine.close_window(window.window_id()));
    block_on(engine.shutdown()).unwrap();
}

/// Private metadata rides the whole-store backup: after deleting it
/// live, restoring the generation brings it back.
#[test]
fn private_metadata_survives_a_backup_restore() {
    let (_scratch, engine) = scratch_engine("tags-backup-restore");
    seed_archive(&engine);
    block_on(engine.add_tag(
        &engine.new_idempotency_key(),
        MessageSelection::OccurrenceIds(vec![11]),
        "work",
    ))
    .unwrap();
    block_on(engine.set_note(&engine.new_idempotency_key(), 1, "kept note")).unwrap();
    let view_id = save_view(&engine, "Kept", ViewSort::DateDesc);

    let resources = engine.resources();
    let snapshot = resources
        .backup_snapshot(BackupDestination::DataRoot)
        .unwrap();
    let generation = resources
        .complete_backup(snapshot)
        .unwrap()
        .generation_id()
        .to_owned();

    block_on(engine.remove_tag(
        &engine.new_idempotency_key(),
        MessageSelection::OccurrenceIds(vec![11]),
        "work",
    ))
    .unwrap();
    block_on(engine.set_note(&engine.new_idempotency_key(), 1, "")).unwrap();
    let delete_key = engine.new_idempotency_key();
    let deleted = block_on(engine.delete_saved_view(&delete_key, &view_id)).unwrap();
    assert_eq!(
        deleted,
        block_on(engine.delete_saved_view(&delete_key, &view_id)).unwrap()
    );
    assert_eq!(counts(&engine), (0, 1, 0));
    assert!(block_on(engine.saved_views()).unwrap().is_empty());

    let prepared = block_on(engine.prepare_restore(&generation, Arc::new(AtomicBool::new(false))))
        .unwrap()
        .expect("the restore prepares");
    let restored =
        block_on(engine.restore(prepared.into_stage(), Arc::new(AtomicBool::new(false))))
            .unwrap_or_else(|error| panic!("the restore succeeds: {error:?}"));

    let message = block_on(restored.get_message(1)).unwrap();
    assert_eq!(message.tags.len(), 1);
    assert_eq!(message.tags[0].name, "work");
    assert_eq!(message.note.as_deref(), Some("kept note"));
    let views = block_on(restored.saved_views()).unwrap();
    assert_eq!((views.len(), views[0].view_id.clone()), (1, view_id));
    assert_eq!(counts(&restored), (1, 1, 1));
    block_on(restored.shutdown()).unwrap();
}
