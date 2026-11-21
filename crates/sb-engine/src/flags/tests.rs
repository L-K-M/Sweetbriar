//! Flag planning and undo through the engine's public API
//! (spec/04-sync-proton "Running an intent", spec/07 "Undo"): the local
//! effect and the intent commit together, a replayed key is inert, and
//! undo plans the inverse with `origin='undo'`.

use std::collections::BTreeSet;

use sb_resources::{StoreError, WritePriority, cancel_intents, refresh_list_rows};
use sb_test_support::block_on;

use crate::views::tests::scratch_engine;
use crate::{Engine, EngineError, FlagChange, IntentFlag, MessageSelection, UndoToken};

const ACCOUNT: &str = "acct-1";
const MESSAGE: i64 = 1;
const FIRST: i64 = 11;
const SECOND: i64 = 12;

fn star() -> FlagChange {
    FlagChange {
        flag: IntentFlag::Flagged,
        set: true,
    }
}

/// A Proton account with one message whose two occurrences sit in two
/// mailboxes.
fn seed(engine: &Engine) {
    block_on(engine.resources().write(WritePriority::User, |tx| {
        tx.execute(
            "INSERT INTO accounts (account_id, kind, provider, display_name, state,
             config_generation, sort_order, created_at)
             VALUES ('acct-1', 'proton', 'proton_api', 'Proton', 'active', 1, 1, 0)",
            [],
        )
        .map_err(internal)?;
        tx.execute(
            "INSERT INTO proton_accounts (account_id, api_base_url, username, proton_user_id,
             credential_ref, password_mode, listing_state)
             VALUES ('acct-1', 'https://mail.proton.me/api', 'owner', 'user-1',
             'cred-1', 1, 'none')",
            [],
        )
        .map_err(internal)?;
        tx.execute(
            "INSERT INTO mailboxes (mailbox_id, account_id, remote_name, role, kind,
             selectable, sync_state) VALUES
             (1, 'acct-1', 'Inbox', 'inbox', 'folder', 1, 'synced'),
             (2, 'acct-1', 'Starred', 'flagged', 'virtual', 1, 'synced')",
            [],
        )
        .map_err(internal)?;
        tx.execute(
            "INSERT INTO logical_messages (message_id, account_id, provider_key, created_at)
             VALUES (1, 'acct-1', 'msg-1', 0)",
            [],
        )
        .map_err(internal)?;
        tx.execute(
            "INSERT INTO occurrences (occurrence_id, account_id, mailbox_id, message_id,
             state, first_seen_at, last_seen_at) VALUES
             (11, 'acct-1', 1, 1, 'present', 0, 0),
             (12, 'acct-1', 2, 1, 'present', 0, 0)",
            [],
        )
        .map_err(internal)?;
        refresh_list_rows(tx, &BTreeSet::from([FIRST, SECOND]))?;
        Ok(())
    }))
    .unwrap();
}

fn internal(error: impl std::fmt::Display) -> StoreError {
    StoreError::internal(error.to_string())
}

fn flagged(engine: &Engine, occurrence: i64) -> i64 {
    engine
        .resources()
        .read(|tx| {
            tx.query_row(
                "SELECT f_flagged FROM occurrences WHERE occurrence_id = ?1",
                [occurrence],
                |row| row.get(0),
            )
            .map_err(internal)
        })
        .unwrap()
}

/// `(intent_id, state, origin, args_json)` per intent, planned order.
fn intents(engine: &Engine) -> Vec<(String, String, String, String)> {
    engine
        .resources()
        .read(|tx| {
            let mut statement = tx
                .prepare(
                    "SELECT intent_id, state, origin, args_json FROM intents
                     ORDER BY rowid",
                )
                .map_err(internal)?;
            statement
                .query_map([], |row| {
                    Ok((
                        row.get::<_, String>(0)?,
                        row.get::<_, String>(1)?,
                        row.get::<_, String>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                })
                .map_err(internal)?
                .collect::<Result<Vec<_>, _>>()
                .map_err(internal)
        })
        .unwrap()
}

fn count(engine: &Engine, statement: &str) -> i64 {
    engine
        .resources()
        .read(|tx| {
            tx.query_row(statement, [], |row| row.get(0))
                .map_err(internal)
        })
        .unwrap()
}

fn plan(engine: &Engine, key: &str, messages: Vec<i64>) -> Option<UndoToken> {
    block_on(engine.plan_flag_change(key, ACCOUNT, messages, star())).unwrap()
}

/// Planning a star sets the flag on both occurrences and writes one
/// pending intent.
#[test]
fn planning_a_star_sets_both_occurrences_and_writes_one_intent() {
    let (_scratch, engine) = scratch_engine("flags-plan-star");
    seed(&engine);

    let token = plan(&engine, &engine.new_idempotency_key(), vec![MESSAGE]).expect("a token");
    assert!(token.as_str().starts_with('{'));
    assert_eq!(flagged(&engine, FIRST), 1);
    assert_eq!(flagged(&engine, SECOND), 1);
    let rows = intents(&engine);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, "pending");
    assert_eq!(rows[0].2, "user");
    assert!(
        rows[0].3.contains(r#""add":["\\Flagged"]"#),
        "{}",
        rows[0].3
    );
    assert_eq!(count(&engine, "SELECT count(*) FROM command_receipts"), 1);
    assert_eq!(
        count(
            &engine,
            "SELECT flagged FROM d_list_rows WHERE occurrence_id = 11"
        ),
        1
    );

    block_on(engine.shutdown()).unwrap();
}

/// A replayed key returns the same token and writes nothing more.
#[test]
fn a_replayed_key_returns_the_same_token_and_writes_nothing() {
    let (_scratch, engine) = scratch_engine("flags-replay");
    seed(&engine);

    let key = engine.new_idempotency_key();
    let first = plan(&engine, &key, vec![MESSAGE]).expect("a token");
    let replayed = plan(&engine, &key, vec![MESSAGE]).expect("the same token");
    assert_eq!(first, replayed);
    assert_eq!(intents(&engine).len(), 1);
    assert_eq!(count(&engine, "SELECT count(*) FROM command_receipts"), 1);
    assert_eq!(flagged(&engine, FIRST), 1);

    block_on(engine.shutdown()).unwrap();
}

/// A message that already holds the requested value plans nothing.
#[test]
fn planning_a_no_op_returns_none() {
    let (_scratch, engine) = scratch_engine("flags-noop");
    seed(&engine);

    plan(&engine, &engine.new_idempotency_key(), vec![MESSAGE]);
    let again = block_on(engine.plan_flag_change(
        &engine.new_idempotency_key(),
        ACCOUNT,
        vec![MESSAGE],
        star(),
    ))
    .unwrap();
    assert!(again.is_none());
    assert_eq!(intents(&engine).len(), 1);

    block_on(engine.shutdown()).unwrap();
}

/// Undo plans the inverse change with `origin='undo'` and clears the
/// local flags.
#[test]
fn undo_plans_the_inverse_with_undo_origin() {
    let (_scratch, engine) = scratch_engine("flags-undo");
    seed(&engine);

    let token = plan(&engine, &engine.new_idempotency_key(), vec![MESSAGE]).expect("a token");
    let parsed = UndoToken::from_string(token.as_str()).unwrap();
    block_on(engine.undo(&engine.new_idempotency_key(), &parsed)).unwrap();

    assert_eq!(flagged(&engine, FIRST), 0);
    assert_eq!(flagged(&engine, SECOND), 0);
    let rows = intents(&engine);
    assert_eq!(rows.len(), 2);
    assert_eq!(rows[1].1, "pending");
    assert_eq!(rows[1].2, "undo");
    assert!(
        rows[1].3.contains(r#""remove":["\\Flagged"]"#),
        "{}",
        rows[1].3
    );

    block_on(engine.shutdown()).unwrap();
}

/// Undo of a cancelled intent plans nothing.
#[test]
fn undo_of_a_cancelled_intent_plans_nothing() {
    let (_scratch, engine) = scratch_engine("flags-undo-cancelled");
    seed(&engine);

    let token = plan(&engine, &engine.new_idempotency_key(), vec![MESSAGE]).expect("a token");
    let intent_id = intents(&engine)[0].0.clone();
    let now = engine.resources().now().as_millis();
    block_on(engine.resources().write(WritePriority::User, move |tx| {
        cancel_intents(tx, &[intent_id], now)
    }))
    .unwrap();
    assert_eq!(flagged(&engine, FIRST), 0);

    let parsed = UndoToken::from_string(token.as_str()).unwrap();
    block_on(engine.undo(&engine.new_idempotency_key(), &parsed)).unwrap();
    assert_eq!(intents(&engine).len(), 1);
    assert_eq!(flagged(&engine, FIRST), 0);

    block_on(engine.shutdown()).unwrap();
}

/// A token naming an unknown intent, or an intent of another account
/// than the one the token names, is `InvalidInput` and plans nothing.
#[test]
fn a_token_for_a_missing_or_foreign_intent_is_invalid_input() {
    let (_scratch, engine) = scratch_engine("flags-undo-foreign");
    seed(&engine);

    plan(&engine, &engine.new_idempotency_key(), vec![MESSAGE]).expect("a token");
    let intent_id = intents(&engine)[0].0.clone();
    for token in [
        format!(r#"{{"v":1,"account_id":"other","intents":["{intent_id}"]}}"#),
        r#"{"v":1,"account_id":"acct-1","intents":["missing"]}"#.to_owned(),
    ] {
        let parsed = UndoToken::from_string(&token).unwrap();
        let error = block_on(engine.undo(&engine.new_idempotency_key(), &parsed)).unwrap_err();
        assert!(
            matches!(error, EngineError::InvalidInput { ref field, .. } if field.as_str() == "token"),
            "{token}: {error:?}"
        );
    }
    assert_eq!(intents(&engine).len(), 1);
    assert_eq!(flagged(&engine, FIRST), 1);

    block_on(engine.shutdown()).unwrap();
}

/// A token that doesn't parse, or names no intent, is `InvalidInput`.
#[test]
fn a_malformed_token_is_invalid_input() {
    for token in [
        "not json",
        r#"{"v":1,"account_id":"acct-1","intents":[]}"#,
        r#"{"v":2,"account_id":"acct-1","intents":["i"]}"#,
        r#"{"v":1,"account_id":"","intents":["i"]}"#,
        r#"{"v":1,"account_id":"acct-1","intents":[1]}"#,
        r#"{"v":1,"account_id":"acct-1","intents":"i"}"#,
        r#"{"v":1,"intents":["i"]}"#,
    ] {
        let error = UndoToken::from_string(token).unwrap_err();
        assert!(
            matches!(error, EngineError::InvalidInput { ref field, .. } if field.as_str() == "token"),
            "{token}: {error:?}"
        );
    }
}

/// Only a Proton account plans flags in M3.
#[test]
fn a_local_archive_account_is_refused() {
    let (_scratch, engine) = scratch_engine("flags-local-refused");
    seed(&engine);
    let local: String = engine
        .resources()
        .read(|tx| {
            tx.query_row(
                "SELECT account_id FROM accounts WHERE provider = 'local_archive'",
                [],
                |row| row.get(0),
            )
            .map_err(internal)
        })
        .unwrap();
    let owner = local.clone();
    block_on(engine.resources().write(WritePriority::User, move |tx| {
        tx.execute(
            "INSERT INTO logical_messages (account_id, created_at) VALUES (?1, 0)",
            [owner],
        )
        .map_err(internal)?;
        Ok(())
    }))
    .unwrap();
    let message = engine
        .resources()
        .read(|tx| {
            tx.query_row("SELECT max(message_id) FROM logical_messages", [], |row| {
                row.get(0)
            })
            .map_err(internal)
        })
        .unwrap();

    let error = block_on(engine.plan_flag_change(
        &engine.new_idempotency_key(),
        &local,
        vec![message],
        star(),
    ))
    .unwrap_err();
    assert!(
        matches!(error, EngineError::InvalidInput { ref field, .. } if field.as_str() == "account_id"),
        "{error:?}"
    );

    block_on(engine.shutdown()).unwrap();
}

fn set_flags(
    engine: &Engine,
    key: &str,
    occurrences: Vec<i64>,
    add: &[&str],
    remove: &[&str],
) -> Result<Option<UndoToken>, EngineError> {
    let names = |names: &[&str]| names.iter().map(|name| (*name).to_owned()).collect();
    block_on(engine.set_flags(
        key,
        MessageSelection::OccurrenceIds(occurrences),
        names(add),
        names(remove),
    ))
}

fn seen(engine: &Engine, occurrence: i64) -> i64 {
    count(
        engine,
        &format!("SELECT f_seen FROM occurrences WHERE occurrence_id = {occurrence}"),
    )
}

/// An occurrence of the local archive account, in a fresh mailbox.
fn local_occurrence(engine: &Engine) -> i64 {
    block_on(engine.resources().write(WritePriority::User, |tx| {
        tx.execute(
            "INSERT INTO mailboxes (mailbox_id, account_id, remote_name, kind, selectable,
             sync_state)
             SELECT 9, account_id, 'Imported', 'folder', 1, 'synced' FROM accounts
             WHERE provider = 'local_archive'",
            [],
        )
        .map_err(internal)?;
        tx.execute(
            "INSERT INTO logical_messages (message_id, account_id, created_at)
             SELECT 9, account_id, 0 FROM accounts WHERE provider = 'local_archive'",
            [],
        )
        .map_err(internal)?;
        tx.execute(
            "INSERT INTO occurrences (occurrence_id, account_id, mailbox_id, message_id,
             state, first_seen_at, last_seen_at)
             SELECT 91, account_id, 9, 9, 'present', 0, 0 FROM accounts
             WHERE provider = 'local_archive'",
            [],
        )
        .map_err(internal)?;
        Ok(91)
    }))
    .unwrap()
}

/// `set_flags` plans each change once per logical message, however
/// many of its occurrences the selection names, under one token that
/// undo reverses; a replayed key is inert and a held flag plans nothing
/// (spec/07 "Flags").
#[test]
fn set_flags_plans_each_change_per_message_under_one_token() {
    let (_scratch, engine) = scratch_engine("flags-set-flags");
    seed(&engine);

    let key = engine.new_idempotency_key();
    let token = set_flags(
        &engine,
        &key,
        vec![FIRST, SECOND],
        &["\\Seen", "\\Flagged"],
        &[],
    )
    .unwrap()
    .expect("a token");
    let planned = intents(&engine);
    assert_eq!(planned.len(), 2, "one intent per change: {planned:?}");
    assert!(
        planned
            .iter()
            .all(|(_, state, origin, _)| state == "pending" && origin == "user")
    );
    for occurrence in [FIRST, SECOND] {
        assert_eq!(
            (seen(&engine, occurrence), flagged(&engine, occurrence)),
            (1, 1)
        );
    }
    let replayed = set_flags(&engine, &key, vec![FIRST], &["\\Seen"], &[]).unwrap();
    assert_eq!(replayed.as_ref(), Some(&token));
    assert_eq!(intents(&engine).len(), 2);
    assert_eq!(
        set_flags(
            &engine,
            &engine.new_idempotency_key(),
            vec![FIRST],
            &["\\Seen"],
            &[]
        )
        .unwrap(),
        None,
        "a held flag plans nothing"
    );

    block_on(engine.undo(&engine.new_idempotency_key(), &token)).unwrap();
    let all = intents(&engine);
    assert_eq!(all.len(), 4);
    assert!(all[2..].iter().all(|(_, _, origin, _)| origin == "undo"));
    assert_eq!((seen(&engine, FIRST), flagged(&engine, SECOND)), (0, 0));

    block_on(engine.shutdown()).unwrap();
}

/// Unknown or repeated flags, no flag at all, an empty selection, and
/// a selection of the local archive or of several accounts are
/// `InvalidInput` on their field and write nothing; an unknown
/// occurrence is `NotFound`.
#[test]
fn set_flags_refuses_bad_flags_and_selections() {
    let (_scratch, engine) = scratch_engine("flags-set-flags-refused");
    seed(&engine);
    let local = local_occurrence(&engine);
    let field_of = |result: Result<Option<UndoToken>, EngineError>| match result {
        Err(EngineError::InvalidInput { field, .. }) => field,
        other => panic!("expected InvalidInput, got {other:?}"),
    };
    let key = || engine.new_idempotency_key();

    assert_eq!(
        field_of(set_flags(
            &engine,
            &key(),
            vec![FIRST],
            &["\\Answered"],
            &[]
        )),
        "add"
    );
    assert_eq!(
        field_of(set_flags(
            &engine,
            &key(),
            vec![FIRST],
            &["\\Seen"],
            &["\\Seen"]
        )),
        "remove"
    );
    assert_eq!(
        field_of(set_flags(&engine, &key(), vec![FIRST], &[], &[])),
        "add"
    );
    assert_eq!(
        field_of(set_flags(&engine, &key(), vec![], &["\\Seen"], &[])),
        "selection"
    );
    assert_eq!(
        field_of(set_flags(&engine, &key(), vec![local], &["\\Seen"], &[])),
        "selection"
    );
    assert_eq!(
        field_of(set_flags(
            &engine,
            &key(),
            vec![FIRST, local],
            &["\\Seen"],
            &[]
        )),
        "selection"
    );
    assert!(matches!(
        set_flags(&engine, &key(), vec![404], &["\\Seen"], &[]),
        Err(EngineError::NotFound { .. })
    ));
    assert!(intents(&engine).is_empty());
    assert_eq!(seen(&engine, FIRST), 0);
    assert_eq!(count(&engine, "SELECT COUNT(*) FROM command_receipts"), 0);

    block_on(engine.shutdown()).unwrap();
}
