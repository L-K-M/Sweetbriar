//! The intent cursor's shape and the health actions' engine commands
//! (spec/04-sync-proton "Health actions", spec/07): a cursor round
//! trips, release resets a parked intent, cancel reverts a pending
//! star, a refused id changes nothing, and a replayed key is inert.

use std::collections::BTreeSet;

use sb_resources::{IntentKey, StoreError, WritePriority, refresh_list_rows};
use sb_test_support::block_on;

use crate::views::tests::scratch_engine;
use crate::{Engine, EngineError, FlagChange, IntentFlag};

use super::*;

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

fn unstar() -> FlagChange {
    FlagChange {
        flag: IntentFlag::Flagged,
        set: false,
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

/// Plan `change` and return the newest intent's id.
fn plan_change(engine: &Engine, change: FlagChange) -> String {
    block_on(engine.plan_flag_change(
        &engine.new_idempotency_key(),
        ACCOUNT,
        vec![MESSAGE],
        change,
    ))
    .unwrap()
    .expect("a token");
    engine
        .resources()
        .read(|tx| {
            tx.query_row(
                "SELECT intent_id FROM intents ORDER BY rowid DESC LIMIT 1",
                [],
                |row| row.get(0),
            )
            .map_err(internal)
        })
        .unwrap()
}

/// `(state, attempts, wait_reason, quarantine_reason, config_generation)`.
fn stored(engine: &Engine, intent_id: &str) -> (String, i64, Option<String>, Option<String>, i64) {
    engine
        .resources()
        .read(|tx| {
            tx.query_row(
                "SELECT state, attempts, wait_reason, quarantine_reason, config_generation
                 FROM intents WHERE intent_id = ?1",
                [intent_id],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .map_err(internal)
        })
        .unwrap()
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

fn count(engine: &Engine, statement: &str) -> i64 {
    engine
        .resources()
        .read(|tx| {
            tx.query_row(statement, [], |row| row.get(0))
                .map_err(internal)
        })
        .unwrap()
}

#[test]
fn a_cursor_round_trips_through_its_tag_and_key() {
    let cursor = mint_cursor(123, "intent-1");
    assert_eq!(cursor.as_str(), "sbi1|123|intent-1");
    assert_eq!(
        decode_cursor(&cursor).unwrap(),
        IntentKey {
            created_at: 123,
            intent_id: "intent-1".to_owned(),
        }
    );

    for bad in [
        "",
        "sbw1|1|a",
        "sbi1|not-a-number|a",
        "sbi1|1",
        "sbi1|1|a|b",
    ] {
        let cursor = Cursor::from(bad.to_owned());
        assert!(decode_cursor(&cursor).is_err(), "{bad}");
    }
}

/// `release_intents` resets a parked intent's attempts, reasons and
/// generation, keeps its local effect, and a replayed key changes
/// nothing.
#[test]
fn release_intents_resets_a_parked_intent_and_a_replay_changes_nothing() {
    let (_scratch, engine) = scratch_engine("intents-release");
    seed(&engine);
    let id = plan_change(&engine, star());
    block_on(engine.resources().write(WritePriority::User, {
        let id = id.clone();
        move |tx| {
            tx.execute(
                "UPDATE intents SET state = 'parked', attempts = 5,
                 quarantine_reason = 'config_change' WHERE intent_id = ?1",
                [&id],
            )
            .map_err(internal)?;
            tx.execute(
                "UPDATE accounts SET config_generation = 2 WHERE account_id = ?1",
                [ACCOUNT],
            )
            .map_err(internal)?;
            Ok(())
        }
    }))
    .unwrap();

    let key = engine.new_idempotency_key();
    let receipt = block_on(engine.release_intents(&key, vec![id.clone()])).unwrap();
    assert_eq!(receipt.command(), "release_intents");
    let row = stored(&engine, &id);
    assert!(row.0 == "pending" && row.1 == 0, "{row:?}");
    assert!(row.2.is_none() && row.3.is_none(), "{row:?}");
    assert_eq!(row.4, 2);
    assert_eq!(flagged(&engine, FIRST), 1, "release keeps the local effect");

    // A repeated key answers the stored receipt; a new key finds the
    // intent pending and is refused, so a release never re-arms twice.
    let replayed = block_on(engine.release_intents(&key, vec![id.clone()])).unwrap();
    assert_eq!(replayed, receipt);
    assert_eq!(
        count(
            &engine,
            "SELECT count(*) FROM command_receipts WHERE command = 'release_intents'"
        ),
        1
    );
    let error = block_on(engine.release_intents(&engine.new_idempotency_key(), vec![id.clone()]))
        .unwrap_err();
    assert!(matches!(error, EngineError::Conflict { .. }), "{error:?}");
    assert_eq!(stored(&engine, &id).0, "pending");

    block_on(engine.shutdown()).unwrap();
}

/// `cancel_intents` ends a pending star, reverts its local effect on
/// every occurrence, and a replayed key changes nothing.
#[test]
fn cancel_intents_reverts_a_pending_star_and_a_replay_changes_nothing() {
    let (_scratch, engine) = scratch_engine("intents-cancel-star");
    seed(&engine);
    let id = plan_change(&engine, star());
    assert_eq!(flagged(&engine, FIRST), 1);

    let key = engine.new_idempotency_key();
    let receipt = block_on(engine.cancel_intents(&key, vec![id.clone()])).unwrap();
    assert_eq!(receipt.command(), "cancel_intents");
    assert_eq!(stored(&engine, &id).0, "cancelled");
    assert_eq!(flagged(&engine, FIRST), 0);
    assert_eq!(flagged(&engine, SECOND), 0);

    let replayed = block_on(engine.cancel_intents(&key, vec![id.clone()])).unwrap();
    assert_eq!(replayed, receipt);
    assert_eq!(
        count(
            &engine,
            "SELECT count(*) FROM command_receipts WHERE command = 'cancel_intents'"
        ),
        1
    );
    assert_eq!(stored(&engine, &id).0, "cancelled");
    assert_eq!(flagged(&engine, FIRST), 0);

    block_on(engine.shutdown()).unwrap();
}

/// An `in_flight` or `done` id is `Conflict`, a missing one is
/// `NotFound`, and a batch with one bad id leaves every row unchanged.
#[test]
fn cancel_intents_refuses_a_bad_id_and_changes_nothing() {
    let (_scratch, engine) = scratch_engine("intents-cancel-conflict");
    seed(&engine);
    let done = plan_change(&engine, star());
    for state in ["in_flight", "done"] {
        let sql = match state {
            "in_flight" => "UPDATE intents SET state = 'in_flight' WHERE intent_id = ?1",
            _ => "UPDATE intents SET state = 'done' WHERE intent_id = ?1",
        };
        block_on(engine.resources().write(WritePriority::User, {
            let id = done.clone();
            move |tx| {
                tx.execute(sql, [&id]).map_err(internal)?;
                Ok(())
            }
        }))
        .unwrap();
        let error =
            block_on(engine.cancel_intents(&engine.new_idempotency_key(), vec![done.clone()]))
                .unwrap_err();
        assert!(matches!(error, EngineError::Conflict { .. }), "{error:?}");
        assert_eq!(stored(&engine, &done).0, state);
        assert_eq!(flagged(&engine, FIRST), 1);
    }

    // A pending second intent beside a bad id stays pending: the
    // action is all or nothing.
    let pending = plan_change(&engine, unstar());
    assert_eq!(flagged(&engine, FIRST), 0);
    let missing = block_on(engine.cancel_intents(
        &engine.new_idempotency_key(),
        vec![pending.clone(), "missing".to_owned()],
    ))
    .unwrap_err();
    assert!(
        matches!(missing, EngineError::NotFound { .. }),
        "{missing:?}"
    );
    let conflict = block_on(engine.cancel_intents(
        &engine.new_idempotency_key(),
        vec![pending.clone(), done.clone()],
    ))
    .unwrap_err();
    assert!(
        matches!(conflict, EngineError::Conflict { .. }),
        "{conflict:?}"
    );
    assert_eq!(stored(&engine, &pending).0, "pending");
    assert_eq!(flagged(&engine, FIRST), 0);

    block_on(engine.shutdown()).unwrap();
}

/// An empty id list is `InvalidInput` and stores no receipt.
#[test]
fn health_actions_refuse_an_empty_id_list() {
    let (_scratch, engine) = scratch_engine("intents-empty-ids");
    for error in [
        block_on(engine.release_intents(&engine.new_idempotency_key(), vec![])).unwrap_err(),
        block_on(engine.cancel_intents(&engine.new_idempotency_key(), vec![])).unwrap_err(),
    ] {
        assert!(
            matches!(error, EngineError::InvalidInput { ref field, .. } if field == "intent_ids"),
            "{error:?}"
        );
    }
    assert_eq!(count(&engine, "SELECT count(*) FROM command_receipts"), 0);

    block_on(engine.shutdown()).unwrap();
}
