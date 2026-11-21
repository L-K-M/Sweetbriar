//! spec/07 `intents(filter)`: oldest-first order, the 1,000-row page
//! boundary, the account and state filters, the stored effect, and a
//! cursor whose row left the filtered set.

use rusqlite::{Connection, params};

use super::*;
use crate::lists::PageRead;
use crate::schema::MIGRATIONS;
use crate::testing::fixtures::{archive_account, migrated};

fn store() -> (Connection, String) {
    let conn = migrated(MIGRATIONS);
    let archive = archive_account(&conn);
    (conn, archive)
}

fn account(conn: &Connection, id: &str) {
    conn.execute(
        "INSERT INTO accounts (account_id, kind, provider, display_name, state, sort_order,
         created_at) VALUES (?1, 'imap', 'proton_bridge', ?2, 'active', 1, 0)",
        params![id, id],
    )
    .unwrap();
}

/// One stored intent; `state` is `parked` or `quarantined`.
#[allow(clippy::too_many_arguments)]
fn intent(
    conn: &Connection,
    intent_id: &str,
    account: &str,
    op: &str,
    target: &str,
    args: &str,
    state: &str,
    reason: Option<&str>,
    last_error: Option<&str>,
    created_at: i64,
) {
    conn.execute(
        "INSERT INTO intents (intent_id, account_id, config_generation, op, target_json,
         args_json, preconditions_json, state, quarantine_reason, last_error,
         idempotency_key, origin, created_at, updated_at)
         VALUES (?1, ?2, 1, ?3, ?4, ?5, '{}', ?6, ?7, ?8, ?1, 'user', ?9, ?9)",
        params![
            intent_id, account, op, target, args, state, reason, last_error, created_at
        ],
    )
    .unwrap();
}

const TARGET: &str = r#"{"v":1,"occurrence_ids":[7,9]}"#;
const FLAGS: &str = r#"{"v":1,"add":["\\Seen"],"remove":[]}"#;
const MOVE: &str = r#"{"v":1,"to_mailbox_id":42}"#;

fn read_page(
    conn: &Connection,
    filter: &IntentFilter,
    anchor: IntentAnchor,
    limit: usize,
) -> PageRead<IntentPage> {
    let tx = conn.unchecked_transaction().unwrap();
    intents(&tx, filter, anchor, limit).unwrap()
}

fn ready(read: PageRead<IntentPage>) -> IntentPage {
    match read {
        PageRead::Ready(page) => page,
        other => panic!("expected a page, got {other:?}"),
    }
}

fn ids(page: &IntentPage) -> Vec<&str> {
    page.rows.iter().map(|row| row.intent_id.as_str()).collect()
}

/// Oldest first by `created_at`, ties by `intent_id`, across the two
/// states; a limit of 1,000 is accepted and a larger one refused.
#[test]
fn intents_page_oldest_first_with_the_id_tie_break() {
    let (conn, archive) = store();
    intent(
        &conn,
        "bbb",
        &archive,
        "move",
        TARGET,
        MOVE,
        "parked",
        None,
        Some("no"),
        20,
    );
    intent(
        &conn,
        "aaa",
        &archive,
        "move",
        TARGET,
        MOVE,
        "quarantined",
        Some("breaker"),
        None,
        20,
    );
    intent(
        &conn, "ccc", &archive, "move", TARGET, MOVE, "parked", None, None, 10,
    );

    let all = IntentFilter::default();
    let page = ready(read_page(&conn, &all, IntentAnchor::Top, 1_000));
    assert_eq!(ids(&page), vec!["ccc", "aaa", "bbb"]);
    assert!(!page.has_more);

    let tx = conn.unchecked_transaction().unwrap();
    let error = intents(&tx, &all, IntentAnchor::Top, 1_001).unwrap_err();
    assert!(
        matches!(error, StoreError::InvalidInput { .. }),
        "{error:?}"
    );
}

/// A page of exactly the row count has no more; the last row's key
/// pages forward to the next row, and past the end it is empty.
#[test]
fn a_cursor_pages_to_the_next_row_and_then_stops() {
    let (conn, archive) = store();
    for (id, created) in [("a", 1), ("b", 2), ("c", 3)] {
        intent(
            &conn, id, &archive, "move", TARGET, MOVE, "parked", None, None, created,
        );
    }
    let filter = IntentFilter::default();

    let first = ready(read_page(&conn, &filter, IntentAnchor::Top, 2));
    assert_eq!(ids(&first), vec!["a", "b"]);
    assert!(first.has_more, "a third row follows");
    let last = first.rows.last().unwrap();
    let key = IntentKey {
        created_at: last.created_at,
        intent_id: last.intent_id.clone(),
    };
    let second = ready(read_page(&conn, &filter, IntentAnchor::After(key), 2));
    assert_eq!(ids(&second), vec!["c"]);
    assert!(!second.has_more);

    // Exactly one page: the whole list fits, so nothing follows.
    let exact = ready(read_page(&conn, &filter, IntentAnchor::Top, 3));
    assert_eq!(ids(&exact), vec!["a", "b", "c"]);
    assert!(!exact.has_more);
}

/// The account and state filters narrow the same oldest-first order.
#[test]
fn account_and_state_filters_narrow_the_page() {
    let (conn, archive) = store();
    account(&conn, "00000000-0000-4000-8000-0000000000a0");
    intent(
        &conn, "p1", &archive, "move", TARGET, MOVE, "parked", None, None, 1,
    );
    intent(
        &conn,
        "q1",
        &archive,
        "move",
        TARGET,
        MOVE,
        "quarantined",
        Some("restore"),
        None,
        2,
    );
    intent(
        &conn,
        "p2",
        "00000000-0000-4000-8000-0000000000a0",
        "move",
        TARGET,
        MOVE,
        "parked",
        None,
        None,
        3,
    );

    let by_account = IntentFilter {
        account_id: Some("00000000-0000-4000-8000-0000000000a0".to_owned()),
        state: None,
    };
    let page = ready(read_page(&conn, &by_account, IntentAnchor::Top, 10));
    assert_eq!(ids(&page), vec!["p2"]);

    let by_state = IntentFilter {
        account_id: None,
        state: Some(IntentState::Quarantined),
    };
    let page = ready(read_page(&conn, &by_state, IntentAnchor::Top, 10));
    assert_eq!(ids(&page), vec!["q1"]);

    let both = IntentFilter {
        account_id: Some(archive),
        state: Some(IntentState::Parked),
    };
    let page = ready(read_page(&conn, &both, IntentAnchor::Top, 10));
    assert_eq!(ids(&page), vec!["p1"]);
}

/// The stored effect and target cross as typed data, including a
/// reserved operation's raw args.
#[test]
fn the_stored_effect_and_target_cross_as_typed_data() {
    let (conn, archive) = store();
    intent(
        &conn,
        "flags",
        &archive,
        "set_flags",
        TARGET,
        FLAGS,
        "parked",
        None,
        None,
        1,
    );
    intent(
        &conn,
        "move",
        &archive,
        "move",
        TARGET,
        MOVE,
        "quarantined",
        Some("epoch_change"),
        None,
        2,
    );
    intent(
        &conn,
        "trash",
        &archive,
        "delete_to_trash",
        TARGET,
        r#"{"v":1}"#,
        "parked",
        None,
        None,
        3,
    );
    intent(
        &conn,
        "reserved",
        &archive,
        "expunge_scoped",
        TARGET,
        r#"{"v":1,"mailbox_id":4}"#,
        "quarantined",
        Some("ambiguous_match"),
        None,
        4,
    );

    let page = ready(read_page(
        &conn,
        &IntentFilter::default(),
        IntentAnchor::Top,
        10,
    ));
    let flags = page
        .rows
        .iter()
        .find(|row| row.intent_id == "flags")
        .unwrap();
    assert_eq!(
        flags.effect,
        IntentEffect::SetFlags {
            add: vec!["\\Seen".to_owned()],
            remove: vec![],
        }
    );
    assert_eq!(flags.target_occurrence_ids, vec![7, 9]);
    assert_eq!(flags.state, IntentState::Parked);
    assert_eq!(flags.quarantine_reason, None);

    let moved = page
        .rows
        .iter()
        .find(|row| row.intent_id == "move")
        .unwrap();
    assert_eq!(moved.effect, IntentEffect::ToMailbox { mailbox_id: 42 });
    assert_eq!(moved.quarantine_reason, Some(QuarantineReason::EpochChange));

    let trash = page
        .rows
        .iter()
        .find(|row| row.intent_id == "trash")
        .unwrap();
    assert_eq!(trash.effect, IntentEffect::DeleteToTrash);

    let reserved = page
        .rows
        .iter()
        .find(|row| row.intent_id == "reserved")
        .unwrap();
    assert_eq!(
        reserved.effect,
        IntentEffect::Reserved {
            args_json: r#"{"v":1,"mailbox_id":4}"#.to_owned()
        }
    );
}

/// A Proton `move`, `copy` or `delete_to_trash` may carry an optional
/// `from_mailbox_id` (an integer or null), which the reader accepts
/// without changing the effect; `remove_label` reads its mailbox from
/// that key (spec/02).
#[test]
fn from_mailbox_id_is_optional_and_remove_label_reads_its_mailbox() {
    let (conn, archive) = store();
    intent(
        &conn,
        "move-from",
        &archive,
        "move",
        TARGET,
        r#"{"v":1,"to_mailbox_id":5,"from_mailbox_id":3}"#,
        "parked",
        None,
        None,
        1,
    );
    intent(
        &conn,
        "copy-null",
        &archive,
        "copy",
        TARGET,
        r#"{"v":1,"to_mailbox_id":5,"from_mailbox_id":null}"#,
        "parked",
        None,
        None,
        2,
    );
    intent(
        &conn,
        "trash-from",
        &archive,
        "delete_to_trash",
        TARGET,
        r#"{"v":1,"from_mailbox_id":3}"#,
        "parked",
        None,
        None,
        3,
    );
    intent(
        &conn,
        "remove",
        &archive,
        "remove_label",
        TARGET,
        r#"{"v":1,"from_mailbox_id":7}"#,
        "parked",
        None,
        None,
        4,
    );

    let page = ready(read_page(
        &conn,
        &IntentFilter::default(),
        IntentAnchor::Top,
        10,
    ));
    let row = |id: &str| page.rows.iter().find(|row| row.intent_id == id).unwrap();
    assert_eq!(
        row("move-from").effect,
        IntentEffect::ToMailbox { mailbox_id: 5 }
    );
    assert_eq!(
        row("copy-null").effect,
        IntentEffect::ToMailbox { mailbox_id: 5 }
    );
    assert_eq!(row("trash-from").effect, IntentEffect::DeleteToTrash);
    assert_eq!(row("remove").kind, IntentKind::RemoveLabel);
    assert_eq!(
        row("remove").effect,
        IntentEffect::RemoveLabel { mailbox_id: 7 }
    );
}

/// A `remove_label` without `from_mailbox_id`, or a `move` whose
/// `from_mailbox_id` is neither an integer nor null, is `Corrupt`.
#[test]
fn malformed_from_mailbox_id_is_corrupt() {
    let (conn, archive) = store();
    intent(
        &conn,
        "remove",
        &archive,
        "remove_label",
        TARGET,
        r#"{"v":1}"#,
        "parked",
        None,
        None,
        1,
    );
    intent(
        &conn,
        "move",
        &archive,
        "move",
        TARGET,
        r#"{"v":1,"to_mailbox_id":5,"from_mailbox_id":"no"}"#,
        "parked",
        None,
        None,
        2,
    );
    let tx = conn.unchecked_transaction().unwrap();
    let error = intents(&tx, &IntentFilter::default(), IntentAnchor::Top, 10).unwrap_err();
    assert!(matches!(error, StoreError::Corrupt { .. }), "{error:?}");
}

/// A payload that isn't the stored shape is `Corrupt`, never a
/// defaulted effect.
#[test]
fn a_malformed_payload_is_corrupt() {
    let (conn, archive) = store();
    intent(
        &conn,
        "bad-target",
        &archive,
        "move",
        r#"{"v":2,"occurrence_ids":[]}"#,
        MOVE,
        "parked",
        None,
        None,
        1,
    );
    intent(
        &conn,
        "bad-args",
        &archive,
        "move",
        TARGET,
        r#"{"v":1}"#,
        "parked",
        None,
        None,
        2,
    );
    let tx = conn.unchecked_transaction().unwrap();
    let error = intents(&tx, &IntentFilter::default(), IntentAnchor::Top, 10).unwrap_err();
    assert!(matches!(error, StoreError::Corrupt { .. }), "{error:?}");
}

/// A cursor whose row left the filtered set is `NotInWindow`.
#[test]
fn a_cursor_outside_the_filter_is_not_in_window() {
    let (conn, archive) = store();
    account(&conn, "00000000-0000-4000-8000-0000000000a0");
    intent(
        &conn, "a", &archive, "move", TARGET, MOVE, "parked", None, None, 1,
    );
    let key = IntentKey {
        created_at: 1,
        intent_id: "a".to_owned(),
    };
    let other = IntentFilter {
        account_id: Some("00000000-0000-4000-8000-0000000000a0".to_owned()),
        state: None,
    };
    assert!(matches!(
        read_page(&conn, &other, IntentAnchor::After(key), 10),
        PageRead::NotInWindow
    ));
}

#[test]
fn stored_words_round_trip_and_a_non_string_provider_key_is_corrupt() {
    for state in [IntentState::Parked, IntentState::Quarantined] {
        assert_eq!(IntentState::parse(state.as_str()), Some(state));
    }
    assert_eq!(
        IntentKind::parse("remove_label"),
        Some(IntentKind::RemoveLabel)
    );
    for reason in [
        QuarantineReason::EpochChange,
        QuarantineReason::Restore,
        QuarantineReason::AmbiguousMatch,
        QuarantineReason::ConfigChange,
        QuarantineReason::Breaker,
    ] {
        assert_eq!(QuarantineReason::parse(reason.as_str()), Some(reason));
    }

    let target = r#"{"v":1,"occurrence_ids":[1],"provider_key":"p"}"#;
    assert_eq!(parse_provider_key(target).unwrap().as_deref(), Some("p"));
    assert_eq!(
        parse_provider_key(r#"{"v":1,"occurrence_ids":[1]}"#).unwrap(),
        None
    );
    assert!(matches!(
        parse_provider_key(r#"{"v":1,"occurrence_ids":[1],"provider_key":7}"#),
        Err(StoreError::Corrupt { .. })
    ));
}
