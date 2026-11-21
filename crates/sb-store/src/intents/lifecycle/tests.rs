//! Intent lifecycle tests (spec/04-sync-proton "Running an intent",
//! spec/02 "Intents"): planning, the claim's ordering, the transitions
//! and their backoff, and the reader.

use rusqlite::{Connection, Transaction, params};

use super::*;
use crate::intents::{
    IntentAnchor, IntentEffect, IntentFilter, IntentState, intent_counts, intents,
};
use crate::lists::PageRead;
use crate::schema::MIGRATIONS;
use crate::testing::fixtures::{mailbox, migrated};

const ACCOUNT: &str = "proton-1";
const OTHER_ACCOUNT: &str = "proton-2";
const PROVIDER_KEY: &str = "msg-1";
const NOW: i64 = 1_700_000_000_000;

struct Seed {
    message: i64,
    first: i64,
    second: i64,
}

fn store() -> Connection {
    migrated(MIGRATIONS)
}

fn account(conn: &Connection, id: &str) {
    conn.execute(
        "INSERT INTO accounts (account_id, kind, provider, display_name, state,
         config_generation, sort_order, created_at)
         VALUES (?1, 'proton', 'proton_api', ?1, 'active', 1, 1, 0)",
        params![id],
    )
    .unwrap();
}

fn seed(conn: &Connection) -> Seed {
    account(conn, ACCOUNT);
    let inbox = mailbox(conn, ACCOUNT, "Inbox", "folder", Some("inbox"));
    let work = mailbox(conn, ACCOUNT, "Work", "label", None);
    let message = message(conn, ACCOUNT, PROVIDER_KEY);
    let first = occurrence(conn, ACCOUNT, inbox, message, "present");
    let second = occurrence(conn, ACCOUNT, work, message, "present");
    Seed {
        message,
        first,
        second,
    }
}

fn message(conn: &Connection, account: &str, provider_key: &str) -> i64 {
    conn.execute(
        "INSERT INTO logical_messages (account_id, provider_key, created_at) VALUES (?1, ?2, 0)",
        params![account, provider_key],
    )
    .unwrap();
    conn.last_insert_rowid()
}

fn occurrence(conn: &Connection, account: &str, mailbox_id: i64, message: i64, state: &str) -> i64 {
    conn.execute(
        "INSERT INTO occurrences (account_id, mailbox_id, message_id, state,
         first_seen_at, last_seen_at) VALUES (?1, ?2, ?3, ?4, 0, 0)",
        params![account, mailbox_id, message, state],
    )
    .unwrap();
    conn.last_insert_rowid()
}

/// Plan with deterministic ids `intent-1`, `intent-2`, … per test.
fn plan(conn: &Connection, next: &mut i64, messages: &[i64], change: FlagChange) -> Vec<String> {
    let tx = conn.unchecked_transaction().unwrap();
    let planned = plan_flag_intents(
        &tx,
        ACCOUNT,
        messages,
        change,
        IntentOrigin::User,
        NOW,
        &mut || {
            *next += 1;
            format!("intent-{next}")
        },
    )
    .unwrap();
    tx.commit().unwrap();
    planned
}

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

fn seen() -> FlagChange {
    FlagChange {
        flag: IntentFlag::Seen,
        set: true,
    }
}

struct Stored {
    state: String,
    attempts: i64,
    next_attempt_at: Option<i64>,
    wait_reason: Option<String>,
    quarantine_reason: Option<String>,
    generation: i64,
    origin: String,
    target_json: String,
    args_json: String,
    idempotency_key: String,
}

fn stored(conn: &Connection, intent_id: &str) -> Stored {
    conn.query_row(
        "SELECT state, attempts, next_attempt_at, wait_reason, quarantine_reason,
                config_generation, origin, target_json, args_json, idempotency_key
         FROM intents WHERE intent_id = ?1",
        [intent_id],
        |row| {
            Ok(Stored {
                state: row.get(0)?,
                attempts: row.get(1)?,
                next_attempt_at: row.get(2)?,
                wait_reason: row.get(3)?,
                quarantine_reason: row.get(4)?,
                generation: row.get(5)?,
                origin: row.get(6)?,
                target_json: row.get(7)?,
                args_json: row.get(8)?,
                idempotency_key: row.get(9)?,
            })
        },
    )
    .unwrap()
}

fn flag(conn: &Connection, occurrence: i64, flag: IntentFlag) -> i64 {
    let column = match flag {
        IntentFlag::Seen => "f_seen",
        IntentFlag::Flagged => "f_flagged",
    };
    conn.query_row(
        &format!("SELECT {column} FROM occurrences WHERE occurrence_id = ?1"),
        [occurrence],
        |row| row.get(0),
    )
    .unwrap()
}

fn count(conn: &Connection, sql: &str) -> i64 {
    conn.query_row(sql, [], |row| row.get(0)).unwrap()
}

fn with_tx<T>(
    conn: &Connection,
    f: impl FnOnce(&Transaction) -> Result<T, StoreError>,
) -> Result<T, StoreError> {
    let tx = conn.unchecked_transaction().unwrap();
    let value = f(&tx)?;
    tx.commit().unwrap();
    Ok(value)
}

/// Planning writes every occurrence's flag and one pending intent, skips
/// a message that already holds the value, and refuses a foreign or
/// keyless message.
#[test]
fn planning_writes_flags_and_intent_together() {
    let conn = store();
    let seed = seed(&conn);
    let mut next = 0;

    let planned = plan(&conn, &mut next, &[seed.message], seen());
    assert_eq!(planned, ["intent-1".to_owned()]);
    assert!(
        flag(&conn, seed.first, IntentFlag::Seen) == 1
            && flag(&conn, seed.second, IntentFlag::Seen) == 1
    );
    let row = stored(&conn, &planned[0]);
    assert!(
        row.state == "pending" && row.attempts == 0 && row.generation == 1 && row.origin == "user"
    );
    assert_eq!(row.idempotency_key, planned[0]);
    assert_eq!(
        row.target_json,
        format!(
            r#"{{"occurrence_ids":[{},{}],"provider_key":"{PROVIDER_KEY}","v":1}}"#,
            seed.first, seed.second
        )
    );
    let args: serde_json::Value = serde_json::from_str(&row.args_json).unwrap();
    assert_eq!(args["add"], serde_json::json!(["\\Seen"]));
    assert_eq!(args["remove"], serde_json::json!([]));

    // The same change again is a no-op; removing it plans and clears.
    assert!(plan(&conn, &mut next, &[seed.message], seen()).is_empty());
    assert_eq!(count(&conn, "SELECT count(*) FROM intents"), 1);
    let removed = plan(
        &conn,
        &mut next,
        &[seed.message],
        FlagChange {
            flag: IntentFlag::Seen,
            set: false,
        },
    );
    assert_eq!(removed, ["intent-2".to_owned()]);
    assert!(
        flag(&conn, seed.first, IntentFlag::Seen) == 0
            && flag(&conn, seed.second, IntentFlag::Seen) == 0
    );
    let args: serde_json::Value =
        serde_json::from_str(&stored(&conn, &removed[0]).args_json).unwrap();
    assert_eq!(args["add"], serde_json::json!([]));
    assert_eq!(args["remove"], serde_json::json!(["\\Seen"]));

    // Another account's message, a keyless message and an unknown one.
    account(&conn, OTHER_ACCOUNT);
    let foreign = message(&conn, OTHER_ACCOUNT, "msg-2");
    conn.execute(
        "INSERT INTO logical_messages (account_id, created_at) VALUES (?1, 0)",
        params![ACCOUNT],
    )
    .unwrap();
    let keyless = conn.last_insert_rowid();
    let tx = conn.unchecked_transaction().unwrap();
    let refused = |tx: &Transaction, message: i64| {
        plan_flag_intents(
            tx,
            ACCOUNT,
            &[message],
            star(),
            IntentOrigin::User,
            NOW,
            &mut String::new,
        )
    };
    assert!(matches!(
        refused(&tx, foreign),
        Err(StoreError::InvalidInput { .. })
    ));
    assert!(matches!(
        refused(&tx, keyless),
        Err(StoreError::InvalidInput { .. })
    ));
    assert!(matches!(
        refused(&tx, seed.message + 100),
        Err(StoreError::NotFound { .. })
    ));
    drop(tx);
    assert_eq!(count(&conn, "SELECT count(*) FROM intents"), 2);
}

/// A message's intents run in planned order: a later one waits with
/// `dependency` while an earlier one is live; another message's intent
/// is not blocked. A non-due or non-flag intent is not claimed.
#[test]
fn claim_orders_a_message_and_marks_the_next_dependency() {
    let conn = store();
    let seed = seed(&conn);
    let sent = mailbox(&conn, ACCOUNT, "Sent", "folder", Some("sent"));
    let other = message(&conn, ACCOUNT, "msg-2");
    occurrence(&conn, ACCOUNT, sent, other, "present");
    let mut next = 0;

    let first = plan(&conn, &mut next, &[seed.message], star());
    let second = plan(&conn, &mut next, &[seed.message], unstar());
    assert_eq!(
        (first[0].as_str(), second[0].as_str()),
        ("intent-1", "intent-2")
    );
    let claimed = with_tx(&conn, |tx| claim_next_intent(tx, ACCOUNT, NOW))
        .unwrap()
        .expect("claimable");
    assert!(
        claimed.intent_id == "intent-1"
            && claimed.provider_key == PROVIDER_KEY
            && claimed.config_generation == 1
    );
    assert_eq!(claimed.occurrence_ids, [seed.first, seed.second]);
    assert_eq!(claimed.change, star());
    assert_eq!(claimed.attempts, 0);
    assert_eq!(
        stored(&conn, &second[0]).wait_reason.as_deref(),
        Some("dependency")
    );

    // Not due, then due; a wait reason other than `dependency` blocks.
    with_tx(&conn, |tx| {
        return_intent_to_pending(tx, &first[0], None, Some(NOW + 5), NOW)
    })
    .unwrap();
    assert!(
        with_tx(&conn, |tx| claim_next_intent(tx, ACCOUNT, NOW))
            .unwrap()
            .is_none()
    );
    assert!(
        with_tx(&conn, |tx| claim_next_intent(tx, ACCOUNT, NOW + 5))
            .unwrap()
            .is_some()
    );
    with_tx(&conn, |tx| {
        return_intent_to_pending(tx, &first[0], Some(WaitReason::Offline), Some(NOW + 5), NOW)
    })
    .unwrap();
    assert!(
        with_tx(&conn, |tx| claim_next_intent(tx, ACCOUNT, NOW + 5))
            .unwrap()
            .is_none()
    );

    // Only a claimed intent finishes: a pending one, even with its
    // successor waiting on it, is `Conflict`.
    assert!(matches!(
        with_tx(&conn, |tx| finish_intent_done(tx, &first[0], NOW)),
        Err(StoreError::Conflict { .. })
    ));
    with_tx(&conn, |tx| {
        clear_account_intents_waiting(tx, ACCOUNT, WaitReason::Offline, NOW)
    })
    .unwrap();
    let again = with_tx(&conn, |tx| claim_next_intent(tx, ACCOUNT, NOW + 5))
        .unwrap()
        .expect("claimable once online");
    assert_eq!(again.intent_id, first[0]);
    with_tx(&conn, |tx| finish_intent_done(tx, &first[0], NOW)).unwrap();
    let follows = with_tx(&conn, |tx| claim_next_intent(tx, ACCOUNT, NOW))
        .unwrap()
        .expect("follows");
    assert_eq!(follows.intent_id, second[0]);
    with_tx(&conn, |tx| finish_intent_done(tx, &second[0], NOW)).unwrap();

    // Another message's intent was never blocked; another operation's
    // pending intent is skipped.
    let third = plan(&conn, &mut next, &[other], star());
    assert_eq!(stored(&conn, &third[0]).wait_reason, None);
    let last = with_tx(&conn, |tx| claim_next_intent(tx, ACCOUNT, NOW))
        .unwrap()
        .expect("never blocked");
    assert_eq!(last.intent_id, third[0]);
    with_tx(&conn, |tx| finish_intent_done(tx, &third[0], NOW)).unwrap();
    conn.execute(
        "INSERT INTO intents (intent_id, account_id, config_generation, op, target_json,
         args_json, preconditions_json, state, idempotency_key, origin, created_at, updated_at)
         VALUES ('move-1', ?1, 1, 'move', '{\"v\":1,\"occurrence_ids\":[]}', '{}', '{}',
         'pending', 'move-1', 'user', 0, 0)",
        params![ACCOUNT],
    )
    .unwrap();
    assert!(
        with_tx(&conn, |tx| claim_next_intent(tx, ACCOUNT, NOW))
            .unwrap()
            .is_none()
    );
}

/// The failure backoff is 30 s, 2 min, 8 min, 32 min, and the fifth
/// failure parks.
#[test]
fn failures_back_off_and_the_fifth_parks() {
    let conn = store();
    let seed = seed(&conn);
    let mut next = 0;
    let intent = plan(&conn, &mut next, &[seed.message], star()).remove(0);
    let mut now = NOW;
    let mut expected = 30_000;
    for attempt in 1..=4 {
        with_tx(&conn, |tx| claim_next_intent(tx, ACCOUNT, now))
            .unwrap()
            .expect("claimable");
        let outcome = with_tx(&conn, |tx| {
            record_intent_failure(tx, &intent, "refused", now)
        })
        .unwrap();
        assert_eq!(
            outcome,
            FailureOutcome::Retry {
                next_attempt_at: now + expected
            }
        );
        let row = stored(&conn, &intent);
        assert!(
            row.state == "pending"
                && row.attempts == attempt
                && row.next_attempt_at == Some(now + expected)
        );
        now += expected;
        expected *= 4;
    }
    with_tx(&conn, |tx| claim_next_intent(tx, ACCOUNT, now))
        .unwrap()
        .expect("claimable");
    assert_eq!(
        with_tx(&conn, |tx| record_intent_failure(
            tx, &intent, "refused", now
        ))
        .unwrap(),
        FailureOutcome::Parked
    );
    let row = stored(&conn, &intent);
    assert!(row.state == "parked" && row.attempts == 5);
    assert!(with_tx(&conn, |tx| record_intent_failure(tx, &intent, "again", now)).is_err());
}

/// Waiting doesn't count an attempt; startup recovers in-flight
/// intents; `resync` waiting blocks the claim until it is cleared.
#[test]
fn waits_recover_and_resync_waiting() {
    let conn = store();
    let seed = seed(&conn);
    let mut next = 0;
    let intent = plan(&conn, &mut next, &[seed.message], star()).remove(0);

    with_tx(&conn, |tx| claim_next_intent(tx, ACCOUNT, NOW))
        .unwrap()
        .expect("claimable");
    with_tx(&conn, |tx| {
        return_intent_to_pending(tx, &intent, Some(WaitReason::Throttled), Some(NOW + 7), NOW)
    })
    .unwrap();
    let row = stored(&conn, &intent);
    assert!(
        row.state == "pending"
            && row.attempts == 0
            && row.wait_reason.as_deref() == Some("throttled")
    );
    assert!(
        with_tx(&conn, |tx| claim_next_intent(tx, ACCOUNT, NOW))
            .unwrap()
            .is_none()
    );
    with_tx(&conn, |tx| {
        clear_account_intents_waiting(tx, ACCOUNT, WaitReason::Throttled, NOW)
    })
    .unwrap();
    with_tx(&conn, |tx| claim_next_intent(tx, ACCOUNT, NOW + 7))
        .unwrap()
        .expect("claimable");
    assert_eq!(
        with_tx(&conn, |tx| recover_in_flight_intents(tx, ACCOUNT, NOW)).unwrap(),
        1
    );
    let row = stored(&conn, &intent);
    assert!(row.state == "pending" && row.attempts == 0);

    with_tx(&conn, |tx| {
        set_account_intents_waiting(tx, ACCOUNT, WaitReason::Resync, NOW)
    })
    .unwrap();
    assert_eq!(
        stored(&conn, &intent).wait_reason.as_deref(),
        Some("resync")
    );
    assert!(
        with_tx(&conn, |tx| claim_next_intent(tx, ACCOUNT, NOW))
            .unwrap()
            .is_none()
    );
    with_tx(&conn, |tx| {
        clear_account_intents_waiting(tx, ACCOUNT, WaitReason::Resync, NOW)
    })
    .unwrap();
    assert!(
        with_tx(&conn, |tx| claim_next_intent(tx, ACCOUNT, NOW + 7))
            .unwrap()
            .is_some()
    );
}

/// Quarantine sets the reason exactly when it quarantines; parking
/// keeps no reason; neither leaves a terminal or quarantined state.
#[test]
fn quarantine_and_park_keep_the_reason_invariant() {
    let conn = store();
    let seed = seed(&conn);
    let mut next = 0;
    let first = plan(&conn, &mut next, &[seed.message], star()).remove(0);
    with_tx(&conn, |tx| claim_next_intent(tx, ACCOUNT, NOW))
        .unwrap()
        .expect("claimable");
    with_tx(&conn, |tx| {
        quarantine_intent(tx, &first, QuarantineReason::ConfigChange, NOW)
    })
    .unwrap();
    let row = stored(&conn, &first);
    assert!(
        row.state == "quarantined" && row.quarantine_reason.as_deref() == Some("config_change")
    );
    assert!(matches!(
        with_tx(&conn, |tx| park_intent(tx, &first, "no", NOW)),
        Err(StoreError::Conflict { .. })
    ));
    assert!(matches!(
        with_tx(&conn, |tx| finish_intent_done(tx, &first, NOW)),
        Err(StoreError::Conflict { .. })
    ));

    let second = plan(&conn, &mut next, &[seed.message], unstar()).remove(0);
    with_tx(&conn, |tx| park_intent(tx, &second, "no", NOW)).unwrap();
    let row = stored(&conn, &second);
    assert!(row.state == "parked" && row.quarantine_reason.is_none());
    assert!(matches!(
        with_tx(&conn, |tx| quarantine_intent(
            tx,
            &second,
            QuarantineReason::Breaker,
            NOW
        )),
        Err(StoreError::Conflict { .. })
    ));
    assert!(matches!(
        with_tx(&conn, |tx| finish_intent_done(tx, &second, NOW)),
        Err(StoreError::Conflict { .. })
    ));
}

/// The health surface's reader reads the rows the writers produce.
#[test]
fn the_reader_reads_written_intents() {
    let conn = store();
    let seed = seed(&conn);
    let mut next = 0;
    let parked = plan(&conn, &mut next, &[seed.message], star()).remove(0);
    with_tx(&conn, |tx| park_intent(tx, &parked, "refused", NOW)).unwrap();
    let page = with_tx(&conn, |tx| {
        match intents(
            tx,
            &IntentFilter {
                account_id: Some(ACCOUNT.to_owned()),
                state: Some(IntentState::Parked),
            },
            IntentAnchor::Top,
            10,
        )? {
            PageRead::Ready(page) => Ok(page),
            other => panic!("expected a page, got {other:?}"),
        }
    })
    .unwrap();
    assert_eq!(page.rows.len(), 1);
    let row = &page.rows[0];
    assert!(
        row.intent_id == parked
            && row.target_occurrence_ids == [seed.first, seed.second]
            && row.last_error.as_deref() == Some("refused")
    );
    assert_eq!(
        row.effect,
        IntentEffect::SetFlags {
            add: vec!["\\Flagged".to_owned()],
            remove: vec![]
        }
    );
    let counts = with_tx(&conn, intent_counts).unwrap();
    assert!(counts.len() == 1 && counts[0].parked == 1 && counts[0].quarantined == 0);
}
