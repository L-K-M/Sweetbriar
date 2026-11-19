//! spec/07 `recovery(account_id?)`: the Recovery list's newest-first
//! order, the page boundary, the account filter, the three states, and
//! the query plan's index use.

use rusqlite::{Connection, params};
use sb_test_support::{block_on, real_fs};
use sb_types::time::Timestamp;

use super::*;
use crate::lists::PageRead;
use crate::proton::{
    ListedMessage, NewProtonAccount, ObservedLabel, ProtonPasswordMode, begin_listing,
    commit_listed_page, insert_proton_account, reconcile_proton_labels,
};
use crate::schema::MIGRATIONS;
use crate::testing::fixtures::{archive_account, mailbox, message, migrated};
use crate::testing::{reading, scratch_paths, test_seams};
use crate::{WritePriority, open};

const ACCOUNT: &str = "00000000-0000-4000-8000-0000000000a0";

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

/// One occurrence of `message` in `mailbox`, in `state`, last seen at
/// `last_seen`.
fn occurrence(
    conn: &Connection,
    account: &str,
    mailbox: i64,
    message: i64,
    state: &str,
    last_seen: i64,
) -> i64 {
    conn.execute(
        "INSERT INTO occurrences (account_id, mailbox_id, message_id, state, first_seen_at,
         last_seen_at) VALUES (?1, ?2, ?3, ?4, ?5, ?5)",
        params![account, mailbox, message, state, last_seen],
    )
    .unwrap();
    conn.last_insert_rowid()
}

fn read_page(
    conn: &Connection,
    account: Option<&str>,
    anchor: RecoveryAnchor,
    limit: usize,
) -> PageRead<RecoveryPage> {
    let tx = conn.unchecked_transaction().unwrap();
    recovery(&tx, account, anchor, limit).unwrap()
}

fn ready(read: PageRead<RecoveryPage>) -> RecoveryPage {
    match read {
        PageRead::Ready(page) => page,
        other => panic!("expected a page, got {other:?}"),
    }
}

fn ids(page: &RecoveryPage) -> Vec<i64> {
    page.rows.iter().map(|row| row.message_id).collect()
}

/// A message whose last occurrence vanished is listed with its mailbox,
/// state and time, newest first; a message with a present occurrence is
/// not, and neither is one whose last occurrence is only
/// `vanish_pending`.
#[test]
fn the_list_holds_messages_with_no_present_occurrence_newest_first() {
    let (conn, archive) = store();
    let inbox = mailbox(&conn, &archive, "Inbox", "folder", Some("inbox"));
    let sent = mailbox(&conn, &archive, "Sent", "folder", Some("sent"));

    let old = message(&conn, &archive, None);
    occurrence(&conn, &archive, inbox, old, "vanished", 100);
    let new = message(&conn, &archive, None);
    occurrence(&conn, &archive, sent, new, "vanished", 300);
    let middle = message(&conn, &archive, None);
    occurrence(&conn, &archive, inbox, middle, "epoch_stale", 200);

    // A present occurrence keeps the message out of the list.
    let present = message(&conn, &archive, None);
    occurrence(&conn, &archive, inbox, present, "vanished", 400);
    occurrence(&conn, &archive, inbox, present, "present", 401);

    // A `vanish_pending` last occurrence is the breaker's, not the
    // list's.
    let held = message(&conn, &archive, None);
    occurrence(&conn, &archive, inbox, held, "vanished", 500);
    occurrence(&conn, &archive, inbox, held, "vanish_pending", 501);

    let page = ready(read_page(&conn, None, RecoveryAnchor::Top, 10));
    assert_eq!(ids(&page), vec![new, middle, old]);
    assert!(!page.has_more);
    assert_eq!(
        page.rows[0],
        StoredRecovery {
            message_id: new,
            account_id: archive.clone(),
            mailbox_id: sent,
            mailbox_name: "Sent".to_owned(),
            state: RecoveryState::Vanished,
            last_seen_at: 300,
            occurrence_id: page.rows[0].occurrence_id,
        }
    );
    assert_eq!(page.rows[1].state, RecoveryState::EpochStale);
    assert_eq!(page.rows[1].mailbox_id, inbox);
}

/// `superseded` is listed too, and the account filter narrows the same
/// order.
#[test]
fn superseded_is_listed_and_the_account_filter_narrows() {
    let (conn, archive) = store();
    account(&conn, ACCOUNT);
    let inbox = mailbox(&conn, &archive, "Inbox", "folder", Some("inbox"));
    let work = mailbox(&conn, ACCOUNT, "Inbox", "folder", Some("inbox"));

    let home = message(&conn, &archive, None);
    occurrence(&conn, &archive, inbox, home, "superseded", 10);
    let away = message(&conn, ACCOUNT, None);
    occurrence(&conn, ACCOUNT, work, away, "vanished", 20);

    let page = ready(read_page(&conn, None, RecoveryAnchor::Top, 10));
    assert_eq!(ids(&page), vec![away, home]);

    let page = ready(read_page(&conn, Some(ACCOUNT), RecoveryAnchor::Top, 10));
    assert_eq!(ids(&page), vec![away]);
    assert_eq!(page.rows[0].account_id, ACCOUNT);
    assert_eq!(page.rows[0].mailbox_id, work);
}

/// Exactly one page has no more; the last row's key pages forward to
/// the next row, and past the end it is empty.
#[test]
fn a_cursor_pages_to_the_next_row_and_then_stops() {
    let (conn, archive) = store();
    let inbox = mailbox(&conn, &archive, "Inbox", "folder", Some("inbox"));
    for (message_id, last_seen) in [(1, 1), (2, 2), (3, 3)] {
        let message = message(&conn, &archive, None);
        assert_eq!(message, message_id);
        occurrence(&conn, &archive, inbox, message, "vanished", last_seen);
    }

    let first = ready(read_page(&conn, None, RecoveryAnchor::Top, 2));
    assert_eq!(ids(&first), vec![3, 2]);
    assert!(first.has_more);
    let last = first.rows.last().unwrap();
    let key = RecoveryKey {
        last_seen_at: last.last_seen_at,
        occurrence_id: last.occurrence_id,
    };
    let second = ready(read_page(&conn, None, RecoveryAnchor::After(key), 2));
    assert_eq!(ids(&second), vec![1]);
    assert!(!second.has_more);

    let exact = ready(read_page(&conn, None, RecoveryAnchor::Top, 3));
    assert_eq!(ids(&exact), vec![3, 2, 1]);
    assert!(!exact.has_more);
}

/// A cursor whose row left the list is `NotInWindow`; an unknown
/// account is `NotFound`.
#[test]
fn a_cursor_outside_the_list_and_an_unknown_account_are_refused() {
    let (conn, archive) = store();
    let inbox = mailbox(&conn, &archive, "Inbox", "folder", Some("inbox"));
    let message = message(&conn, &archive, None);
    let vanished = occurrence(&conn, &archive, inbox, message, "vanished", 1);
    let key = RecoveryKey {
        last_seen_at: 1,
        occurrence_id: vanished,
    };
    let tx = conn.unchecked_transaction().unwrap();
    let error = recovery(&tx, Some("no-such-account"), RecoveryAnchor::Top, 10).unwrap_err();
    assert!(matches!(error, StoreError::NotFound { .. }), "{error:?}");
    drop(tx);

    // The row is gone once the message has a present occurrence again.
    occurrence(&conn, &archive, inbox, message, "present", 2);
    assert!(matches!(
        read_page(&conn, None, RecoveryAnchor::After(key), 10),
        PageRead::NotInWindow
    ));
}

/// The page reads seek `occ_by_message` for the last-occurrence and
/// present checks, with no scan of the whole list projection.
#[test]
fn the_recovery_read_is_indexed() {
    let (conn, _archive) = store();
    let sql = format!(
        "SELECT o.occurrence_id {} ORDER BY o.last_seen_at DESC, o.occurrence_id DESC LIMIT 10",
        source("")
    );
    let mut statement = conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
    let plan = statement
        .query_map(
            rusqlite::named_params! { ":account": None::<String> },
            |row| row.get::<_, String>(3),
        )
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap()
        .join("\n");
    assert!(
        plan.contains("SEARCH o2 USING INDEX occ_by_message"),
        "{plan}"
    );
    assert!(
        plan.contains("SEARCH p USING INDEX occ_by_message_observation"),
        "{plan}"
    );
}

/// The acceptance: a message whose last occurrence vanished through a
/// real Proton listing is held in the Recovery list, and drops out when
/// the listing gives it back its label.
#[test]
fn a_listed_message_vanishes_and_reappears_through_the_sync_writes() {
    let (_scratch, paths) = scratch_paths("recovery-listing");
    let store = open(&paths, &test_seams(real_fs())).unwrap();
    block_on(store.write(WritePriority::Sync, |tx| {
        tx.execute(
            "INSERT INTO accounts (account_id, kind, provider, display_name, state, sort_order,
             created_at) VALUES (?1, 'proton', 'proton_api', 'Proton', 'active', 1, 0)",
            params![ACCOUNT],
        )
        .unwrap();
        insert_proton_account(
            tx,
            &NewProtonAccount {
                account_id: ACCOUNT.to_owned(),
                api_base_url: "https://mail.proton.me/api".to_owned(),
                username: "owner".to_owned(),
                proton_user_id: "user-1".to_owned(),
                credential_ref: "proton-p1".to_owned(),
                password_mode: ProtonPasswordMode::Login,
            },
        )?;
        reconcile_proton_labels(tx, ACCOUNT, &[inbox_label()])?;
        begin_listing(tx, ACCOUNT, "event-0", 1).map(|_| ())
    }))
    .unwrap();

    let listed = |labels: &[&str]| ListedMessage {
        provider_key: "message-1".to_owned(),
        label_ids: labels.iter().map(|label| (*label).to_owned()).collect(),
        raw_metadata: r#"{"v":1,"proton_metadata":{"ID":"message-1"}}"#.as_bytes().to_vec(),
        columns: crate::sync::HeaderColumns::default(),
        internal_date: Some(Timestamp::from_millis(1_700_000_000_000)),
        size: Some(10),
        flags: crate::sync::SyncedFlags::default(),
    };
    let commit = |messages: Vec<ListedMessage>| {
        // Each committed page records its checkpoint and bumps the
        // account revision, so a page is listed under the revision it
        // was read at.
        let revision = reading(&store, |tx| {
            Ok(crate::proton::proton_account(tx, ACCOUNT)?
                .unwrap()
                .revision)
        });
        block_on(store.write(WritePriority::Sync, move |tx| {
            commit_listed_page(
                tx,
                &crate::proton::ListedPage {
                    account_id: ACCOUNT,
                    messages: &messages,
                    checkpoint: "event-1",
                    expected_revision: revision,
                },
                Timestamp::from_millis(1_735_689_600_000),
            )
            .map(|_| ())
        }))
        .unwrap();
    };

    // Listed with Inbox: present, so not in the Recovery list.
    commit(vec![listed(&["0"])]);
    let page = reading(&store, |tx| {
        recovery(tx, None, RecoveryAnchor::Top, 10).map(|read| match read {
            PageRead::Ready(page) => page,
            PageRead::NotInWindow => panic!("not in window"),
            PageRead::Stale { generation } => panic!("stale at {generation}"),
        })
    });
    assert!(page.rows.is_empty(), "{:?}", page.rows);

    // The label is gone: the occurrence vanishes and the message is
    // held, with its mailbox and the vanished state.
    commit(vec![listed(&[])]);
    let page = reading(&store, |tx| {
        recovery(tx, None, RecoveryAnchor::Top, 10).map(|read| match read {
            PageRead::Ready(page) => page,
            PageRead::NotInWindow => panic!("not in window"),
            PageRead::Stale { generation } => panic!("stale at {generation}"),
        })
    });
    assert_eq!(page.rows.len(), 1, "{:?}", page.rows);
    assert_eq!(page.rows[0].state, RecoveryState::Vanished);
    assert_eq!(page.rows[0].mailbox_name, "Inbox");

    // Listed with Inbox again: a new present occurrence, so the
    // message drops out of the Recovery list.
    commit(vec![listed(&["0"])]);
    let page = reading(&store, |tx| {
        recovery(tx, None, RecoveryAnchor::Top, 10).map(|read| match read {
            PageRead::Ready(page) => page,
            PageRead::NotInWindow => panic!("not in window"),
            PageRead::Stale { generation } => panic!("stale at {generation}"),
        })
    });
    assert!(page.rows.is_empty(), "{:?}", page.rows);
}

/// The Inbox label as the policy maps it.
fn inbox_label() -> ObservedLabel {
    ObservedLabel {
        provider_key: "0".to_owned(),
        remote_name: "Inbox".to_owned(),
        delimiter: Some('/'),
        kind: crate::sidebar::MailboxKind::Folder,
        role: Some(crate::lists::MailboxRole::Inbox),
        selectable: true,
    }
}
