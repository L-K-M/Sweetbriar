//! Filing through the engine's public API (spec/07 "Messages", "Local
//! archive" and "Undo"): Proton moves plan intents and undo moves them
//! back, the local archive moves at once, refusals name their argument,
//! and a replayed key is inert.

use std::collections::BTreeSet;

use sb_resources::{StoreError, WritePriority, refresh_list_rows};
use sb_test_support::block_on;

use crate::views::tests::scratch_engine;
use crate::{Engine, EngineError, MessageSelection};

const INBOX: i64 = 1;
const ARCHIVE: i64 = 2;
const SENT: i64 = 3;
const WORK: i64 = 4;
const LOCAL_FOLDER: i64 = 5;
const KEPT: i64 = 6;
const PROTON_OCCURRENCE: i64 = 11;
const LOCAL_OCCURRENCE: i64 = 21;
const PROTON_MESSAGE: i64 = 1;
const LOCAL_MESSAGE: i64 = 2;

/// A Proton account with one message in Inbox, and the local archive
/// with one message in an imported folder.
fn seed(engine: &Engine) {
    block_on(engine.resources().write(WritePriority::User, |tx| {
        let local: String = tx
            .query_row(
                "SELECT account_id FROM accounts WHERE provider = 'local_archive'",
                [],
                |row| row.get(0),
            )
            .map_err(internal)?;
        tx.execute_batch(&format!(
            "INSERT INTO accounts (account_id, kind, provider, display_name, state,
             config_generation, sort_order, created_at) VALUES
             ('acct-1', 'proton', 'proton_api', 'Proton', 'active', 1, 1, 0);
             INSERT INTO proton_accounts (account_id, api_base_url, username, proton_user_id,
             credential_ref, password_mode, listing_state)
             VALUES ('acct-1', 'https://mail.proton.me/api', 'owner', 'user-1',
             'cred-1', 1, 'none');
             INSERT INTO mailboxes (mailbox_id, account_id, remote_name, role, kind,
             selectable, sync_state) VALUES
             (1, 'acct-1', 'Inbox', 'inbox', 'folder', 1, 'synced'),
             (2, 'acct-1', 'Archive', 'archive', 'folder', 1, 'synced'),
             (3, 'acct-1', 'Sent', 'sent', 'folder', 1, 'synced'),
             (4, 'acct-1', 'Work', NULL, 'label', 1, 'synced'),
             (5, '{local}', 'Imported', NULL, 'folder', 1, 'synced'),
             (6, '{local}', 'Kept', NULL, 'folder', 1, 'synced');
             INSERT INTO logical_messages (message_id, account_id, provider_key, created_at)
             VALUES (1, 'acct-1', 'msg-1', 0), (2, '{local}', NULL, 0);
             INSERT INTO occurrences (occurrence_id, account_id, mailbox_id, message_id,
             state, first_seen_at, last_seen_at) VALUES
             (11, 'acct-1', 1, 1, 'present', 0, 0),
             (21, '{local}', 5, 2, 'present', 0, 0);",
        ))
        .map_err(internal)?;
        refresh_list_rows(tx, &BTreeSet::from([PROTON_OCCURRENCE, LOCAL_OCCURRENCE]))?;
        Ok(())
    }))
    .unwrap();
}

fn internal(error: impl std::fmt::Display) -> StoreError {
    StoreError::internal(error.to_string())
}

fn selection(occurrence: i64) -> MessageSelection {
    MessageSelection::OccurrenceIds(vec![occurrence])
}

/// The mailboxes `message` is currently in, ascending.
fn mailboxes_of(engine: &Engine, message: i64) -> Vec<i64> {
    engine
        .resources()
        .read(|tx| {
            let mut statement = tx
                .prepare(
                    "SELECT mailbox_id FROM occurrences WHERE message_id = ?1
                     AND state IN ('present', 'vanish_pending') ORDER BY mailbox_id",
                )
                .map_err(internal)?;
            statement
                .query_map([message], |row| row.get(0))
                .map_err(internal)?
                .collect::<Result<Vec<i64>, _>>()
                .map_err(internal)
        })
        .unwrap()
}

/// `(op, origin, args_json)` per intent, in planned order.
fn intents(engine: &Engine) -> Vec<(String, String, String)> {
    engine
        .resources()
        .read(|tx| {
            let mut statement = tx
                .prepare("SELECT op, origin, args_json FROM intents ORDER BY rowid")
                .map_err(internal)?;
            statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .map_err(internal)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(internal)
        })
        .unwrap()
}

fn field(error: EngineError) -> String {
    match error {
        EngineError::InvalidInput { field, .. } => field,
        other => panic!("expected InvalidInput, got {other:?}"),
    }
}

fn moved_elsewhere(result: Result<(), EngineError>) -> bool {
    matches!(result, Err(EngineError::Conflict { detail }) if detail == "Moved elsewhere since")
}

#[test]
fn a_proton_move_plans_an_intent_and_undo_moves_it_back() {
    let (_scratch, engine) = scratch_engine("filing-proton-move");
    seed(&engine);
    let key = engine.new_idempotency_key();

    let token = block_on(engine.move_messages(&key, selection(PROTON_OCCURRENCE), ARCHIVE))
        .unwrap()
        .expect("a move answers a token");
    assert_eq!(mailboxes_of(&engine, PROTON_MESSAGE), vec![ARCHIVE]);
    let replayed =
        block_on(engine.move_messages(&key, selection(PROTON_OCCURRENCE), ARCHIVE)).unwrap();
    assert_eq!(
        replayed,
        Some(token.clone()),
        "a replay answers the first token"
    );
    assert_eq!(intents(&engine).len(), 1, "and plans nothing");

    block_on(engine.undo(&engine.new_idempotency_key(), &token)).unwrap();

    assert_eq!(mailboxes_of(&engine, PROTON_MESSAGE), vec![INBOX]);
    let planned = intents(&engine);
    assert_eq!(planned.len(), 2);
    assert_eq!(
        (planned[1].0.as_str(), planned[1].1.as_str()),
        ("move", "undo")
    );
    let args: serde_json::Value = serde_json::from_str(&planned[1].2).unwrap();
    assert_eq!(
        args,
        serde_json::json!({"v": 1, "to_mailbox_id": INBOX, "from_mailbox_id": ARCHIVE})
    );
}

#[test]
fn undo_after_the_message_moved_on_is_a_conflict() {
    let (_scratch, engine) = scratch_engine("filing-moved-elsewhere");
    seed(&engine);
    let token = block_on(engine.move_messages(
        &engine.new_idempotency_key(),
        selection(PROTON_OCCURRENCE),
        ARCHIVE,
    ))
    .unwrap()
    .unwrap();
    let archived = engine
        .resources()
        .read(|tx| {
            tx.query_row(
                "SELECT occurrence_id FROM occurrences WHERE mailbox_id = ?1 AND state = 'present'",
                [ARCHIVE],
                |row| row.get(0),
            )
            .map_err(internal)
        })
        .unwrap();
    block_on(engine.move_messages(&engine.new_idempotency_key(), selection(archived), INBOX))
        .unwrap();

    let refused = block_on(engine.undo(&engine.new_idempotency_key(), &token));

    assert!(moved_elsewhere(refused), "a moved-on message refuses undo");
    assert_eq!(intents(&engine).len(), 2, "nothing was undone");
}

#[test]
fn the_local_archive_moves_at_once_and_undoes_locally() {
    let (_scratch, engine) = scratch_engine("filing-local-move");
    seed(&engine);

    let token = block_on(engine.move_messages(
        &engine.new_idempotency_key(),
        selection(LOCAL_OCCURRENCE),
        KEPT,
    ))
    .unwrap()
    .expect("a local move answers a token");
    assert_eq!(mailboxes_of(&engine, LOCAL_MESSAGE), vec![KEPT]);
    assert!(token.as_str().contains(r#""moves""#));

    block_on(engine.undo(&engine.new_idempotency_key(), &token)).unwrap();

    assert_eq!(mailboxes_of(&engine, LOCAL_MESSAGE), vec![LOCAL_FOLDER]);
    assert!(
        intents(&engine).is_empty(),
        "the local archive plans no intent"
    );
    let again = block_on(engine.undo(&engine.new_idempotency_key(), &token));
    assert!(moved_elsewhere(again), "the message is no longer in Kept");
}

#[test]
fn trash_and_labels_plan_their_intents_and_labels_answer_no_token() {
    let (_scratch, engine) = scratch_engine("filing-trash-labels");
    seed(&engine);
    block_on(engine.resources().write(WritePriority::User, |tx| {
        tx.execute(
            "INSERT INTO mailboxes (mailbox_id, account_id, remote_name, role, kind, selectable,
             sync_state) VALUES (7, 'acct-1', 'Trash', 'trash', 'folder', 1, 'synced')",
            [],
        )
        .map_err(internal)?;
        Ok(())
    }))
    .unwrap();

    block_on(engine.add_label(
        &engine.new_idempotency_key(),
        selection(PROTON_OCCURRENCE),
        WORK,
    ))
    .unwrap();
    block_on(engine.remove_label(
        &engine.new_idempotency_key(),
        selection(PROTON_OCCURRENCE),
        WORK,
    ))
    .unwrap();
    let token = block_on(
        engine.delete_to_trash(&engine.new_idempotency_key(), selection(PROTON_OCCURRENCE)),
    )
    .unwrap();

    assert!(token.is_some());
    let ops: Vec<String> = intents(&engine).into_iter().map(|(op, _, _)| op).collect();
    assert_eq!(ops, ["copy", "remove_label", "delete_to_trash"]);
    assert_eq!(mailboxes_of(&engine, PROTON_MESSAGE), vec![7]);
}

#[test]
fn refusals_name_the_argument_they_are_about() {
    let (_scratch, engine) = scratch_engine("filing-refusals");
    seed(&engine);

    let sent = block_on(engine.move_messages(
        &engine.new_idempotency_key(),
        selection(PROTON_OCCURRENCE),
        SENT,
    ));
    assert_eq!(field(sent.unwrap_err()), "to_mailbox");
    let label = block_on(engine.add_label(
        &engine.new_idempotency_key(),
        selection(PROTON_OCCURRENCE),
        INBOX,
    ));
    assert_eq!(field(label.unwrap_err()), "label_mailbox");
    let mixed = MessageSelection::OccurrenceIds(vec![PROTON_OCCURRENCE, LOCAL_OCCURRENCE]);
    let both = block_on(engine.move_messages(&engine.new_idempotency_key(), mixed, ARCHIVE));
    assert_eq!(field(both.unwrap_err()), "selection");
    let already = block_on(engine.move_messages(
        &engine.new_idempotency_key(),
        selection(PROTON_OCCURRENCE),
        INBOX,
    ));
    assert_eq!(field(already.unwrap_err()), "selection");
    let empty = block_on(engine.delete_to_trash(
        &engine.new_idempotency_key(),
        MessageSelection::OccurrenceIds(vec![]),
    ));
    assert_eq!(field(empty.unwrap_err()), "selection");
    assert!(intents(&engine).is_empty());
}
