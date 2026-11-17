//! Proton bootstrap writes (plan M2.15b): labels reconcile by id with
//! renames in place, pages commit with their checkpoint and resume
//! without duplicates, labels follow the metadata, and the listing
//! finishes with Proton's totals for coverage.

use rusqlite::params;
use sb_test_support::{ScratchDir, block_on, real_fs};
use sb_types::time::Timestamp;

use super::*;
use crate::proton::{
    NewProtonAccount, ProtonPasswordMode, begin_listing, insert_proton_account, proton_account,
};
use crate::testing::{reading, scratch_paths, test_seams};
use crate::{Store, WritePriority, open};

const ACCOUNT: &str = "00000000-0000-4000-8000-0000000000p1";

fn now() -> Timestamp {
    Timestamp::from_millis(1_735_689_600_000)
}

fn write<T: Send + 'static>(
    store: &Store,
    f: impl FnOnce(&Transaction) -> Result<T, StoreError> + Send + 'static,
) -> Result<T, StoreError> {
    block_on(store.write(WritePriority::Sync, f))
}

/// A Proton account that is listing under revision 2.
fn listing_store(label: &str) -> (ScratchDir, Store) {
    let (scratch, paths) = scratch_paths(label);
    let store = open(&paths, &test_seams(real_fs())).unwrap();
    write(&store, |tx| {
        tx.execute(
            "INSERT INTO accounts (account_id, kind, provider, display_name, state, sort_order, created_at)
             VALUES (?1, 'proton', 'proton_api', 'Proton', 'active', 1, 0)",
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
        begin_listing(tx, ACCOUNT, "event-0", 1).map(|_| ())
    })
    .unwrap();
    (scratch, store)
}

fn label(key: &str, name: &str, kind: MailboxKind, role: Option<MailboxRole>) -> ObservedLabel {
    ObservedLabel {
        provider_key: key.to_owned(),
        remote_name: name.to_owned(),
        delimiter: (kind == MailboxKind::Folder).then_some('/'),
        kind,
        role,
        selectable: true,
    }
}

fn system() -> Vec<ObservedLabel> {
    vec![
        label("0", "Inbox", MailboxKind::Folder, Some(MailboxRole::Inbox)),
        label(
            "5",
            "All Mail",
            MailboxKind::Virtual,
            Some(MailboxRole::All),
        ),
        label(
            "10",
            "Starred",
            MailboxKind::Virtual,
            Some(MailboxRole::Flagged),
        ),
    ]
}

fn message(id: &str, labels: &[&str], unread: bool) -> ListedMessage {
    ListedMessage {
        provider_key: id.to_owned(),
        label_ids: labels.iter().map(|label| (*label).to_owned()).collect(),
        raw_metadata: format!(r#"{{"v":1,"proton_metadata":{{"ID":"{id}"}}}}"#).into_bytes(),
        columns: HeaderColumns {
            subject: Some(format!("Subject {id}")),
            ..HeaderColumns::default()
        },
        internal_date: Some(Timestamp::from_millis(1_700_000_000_000)),
        size: Some(1234),
        flags: SyncedFlags {
            seen: !unread,
            flagged: labels.contains(&"10"),
            ..SyncedFlags::default()
        },
    }
}

fn commit(
    store: &Store,
    messages: Vec<ListedMessage>,
    checkpoint: &str,
    revision: i64,
) -> Result<ListedPageStats, StoreError> {
    let checkpoint = checkpoint.to_owned();
    write(store, move |tx| {
        commit_listed_page(
            tx,
            &ListedPage {
                account_id: ACCOUNT,
                messages: &messages,
                checkpoint: &checkpoint,
                expected_revision: revision,
            },
            now(),
        )
    })
}

fn revision(store: &Store) -> i64 {
    reading(store, |tx| {
        Ok(proton_account(tx, ACCOUNT)?.unwrap().revision)
    })
}

fn mailbox(store: &Store, key: &str) -> (i64, String, String) {
    let key = key.to_owned();
    reading(store, move |tx| {
        Ok(tx
            .query_row(
                "SELECT mailbox_id, remote_name, sync_state FROM mailboxes
                 WHERE account_id = ?1 AND provider_key = ?2",
                params![ACCOUNT, key],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap())
    })
}

/// (label id, state) of every occurrence of `id`, sorted.
fn occurrences(store: &Store, id: &str) -> Vec<(String, String)> {
    let id = id.to_owned();
    reading(store, move |tx| {
        let mut statement = tx
            .prepare(
                "SELECT m.provider_key, o.state FROM occurrences o
                 JOIN mailboxes m ON m.mailbox_id = o.mailbox_id
                 JOIN logical_messages l ON l.message_id = o.message_id
                 WHERE l.provider_key = ?1 ORDER BY m.provider_key, o.state",
            )
            .unwrap();
        let rows = statement
            .query_map(params![id], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap();
        Ok(rows.map(Result::unwrap).collect())
    })
}

fn count(store: &Store, sql: &'static str) -> i64 {
    reading(store, move |tx| {
        Ok(tx.query_row(sql, [], |row| row.get(0)).unwrap())
    })
}

#[test]
fn labels_reconcile_by_id_and_rename_in_place() {
    let (_scratch, store) = listing_store("proton-labels");
    let mut labels = system();
    labels.push(label("f1", "Work", MailboxKind::Folder, None));
    labels.push(label("l1", "Later", MailboxKind::Label, None));
    let first = write(&store, move |tx| {
        reconcile_proton_labels(tx, ACCOUNT, &labels)
    })
    .unwrap();
    assert_eq!(first.added.len(), 5);
    let (work, _, state) = mailbox(&store, "f1");
    let (later, _, _) = mailbox(&store, "l1");
    assert_eq!(state, "new");

    // The folder and the label swap names; both keep their mailbox.
    let mut swapped = system();
    swapped.push(label("f1", "Later", MailboxKind::Folder, None));
    swapped.push(label("l1", "Work", MailboxKind::Label, None));
    let second = write(&store, move |tx| {
        reconcile_proton_labels(tx, ACCOUNT, &swapped)
    })
    .unwrap();
    assert_eq!(second.changed, {
        let mut both = vec![work, later];
        both.sort_unstable();
        both
    });
    assert_eq!(
        mailbox(&store, "f1"),
        (work, "Later".to_owned(), "new".to_owned())
    );
    assert_eq!(
        mailbox(&store, "l1"),
        (later, "Work".to_owned(), "new".to_owned())
    );

    // A missing label is gone; a new label may take its name, and the
    // gone mailbox steps aside under its id.
    let mut replaced = system();
    replaced.push(label("f1", "Later", MailboxKind::Folder, None));
    replaced.push(label("l2", "Work", MailboxKind::Label, None));
    let third = write(&store, move |tx| {
        reconcile_proton_labels(tx, ACCOUNT, &replaced)
    })
    .unwrap();
    assert_eq!(third.gone, vec![later]);
    assert_eq!(
        mailbox(&store, "l1"),
        (later, "Work (l1)".to_owned(), "gone".to_owned())
    );
    assert_eq!(mailbox(&store, "l2").1, "Work");

    // Listed again, it returns under its own mailbox.
    let mut back = system();
    back.push(label("f1", "Later", MailboxKind::Folder, None));
    back.push(label("l1", "Someday", MailboxKind::Label, None));
    let fourth = write(&store, move |tx| {
        reconcile_proton_labels(tx, ACCOUNT, &back)
    })
    .unwrap();
    assert_eq!(fourth.returned, vec![later]);
    assert_eq!(
        mailbox(&store, "l1"),
        (later, "Someday".to_owned(), "new".to_owned())
    );

    // A name or id listed twice is refused before any write.
    let mut twice = system();
    twice.push(label("x", "Inbox", MailboxKind::Label, None));
    let refused = write(&store, move |tx| {
        reconcile_proton_labels(tx, ACCOUNT, &twice)
    });
    assert!(
        matches!(refused, Err(StoreError::InvalidInput { .. })),
        "{refused:?}"
    );
}

#[test]
fn a_page_commits_with_its_checkpoint_and_a_replay_adds_nothing() {
    let (_scratch, store) = listing_store("proton-page");
    let labels = system();
    write(&store, move |tx| {
        reconcile_proton_labels(tx, ACCOUNT, &labels)
    })
    .unwrap();
    let page = vec![
        message("m1", &["0", "5", "10"], true),
        message("m2", &["5", "2"], false),
    ];

    let stats = commit(&store, page.clone(), "anchor-1", 2).unwrap();
    assert_eq!(
        stats,
        ListedPageStats {
            new_messages: 2,
            inserted: 4,
            refreshed: 0,
            vanished: 0
        }
    );
    assert_eq!(
        occurrences(&store, "m1"),
        [("0", "present"), ("10", "present"), ("5", "present")]
            .map(|(a, b)| (a.to_owned(), b.to_owned()))
    );
    // All Sent (2) has no mailbox: ignored.
    assert_eq!(
        occurrences(&store, "m2"),
        vec![("5".to_owned(), "present".to_owned())]
    );
    let account = reading(&store, |tx| Ok(proton_account(tx, ACCOUNT)?.unwrap()));
    assert_eq!(account.listing_checkpoint.as_deref(), Some("anchor-1"));
    assert_eq!(account.revision, 3);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM d_list_rows"), 4);
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM message_headers WHERE subject LIKE 'Subject m%'"
        ),
        2
    );

    // The same page again, as after a crash before the next one: the
    // stale revision is refused, the current one adds nothing.
    let stale = commit(&store, page.clone(), "anchor-1", 2);
    assert!(
        matches!(stale, Err(StoreError::Conflict { .. })),
        "{stale:?}"
    );
    let again = commit(&store, page, "anchor-1", 3).unwrap();
    assert_eq!(
        again,
        ListedPageStats {
            new_messages: 0,
            inserted: 0,
            refreshed: 4,
            vanished: 0
        }
    );
    assert_eq!(count(&store, "SELECT COUNT(*) FROM logical_messages"), 2);
    assert_eq!(count(&store, "SELECT COUNT(*) FROM occurrences"), 4);
}

#[test]
fn labels_and_flags_follow_the_listed_metadata() {
    let (_scratch, store) = listing_store("proton-follow");
    let labels = system();
    write(&store, move |tx| {
        reconcile_proton_labels(tx, ACCOUNT, &labels)
    })
    .unwrap();
    commit(
        &store,
        vec![message("m1", &["0", "5", "10"], true)],
        "a1",
        2,
    )
    .unwrap();

    // Unstarred and read: the Starred occurrence vanishes, the others
    // take the new flags.
    let stats = commit(&store, vec![message("m1", &["0", "5"], false)], "a2", 3).unwrap();
    assert_eq!(
        stats,
        ListedPageStats {
            new_messages: 0,
            inserted: 0,
            refreshed: 2,
            vanished: 1
        }
    );
    assert_eq!(
        occurrences(&store, "m1"),
        [("0", "present"), ("10", "vanished"), ("5", "present")]
            .map(|(a, b)| (a.to_owned(), b.to_owned()))
    );
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM occurrences WHERE state = 'present' AND f_seen = 1"
        ),
        2
    );
    assert_eq!(count(&store, "SELECT COUNT(*) FROM d_list_rows"), 2);

    // Starred again: a new occurrence, never the vanished one revived.
    commit(
        &store,
        vec![message("m1", &["0", "5", "10"], false)],
        "a3",
        4,
    )
    .unwrap();
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM occurrences WHERE state = 'present'"
        ),
        3
    );
    assert_eq!(count(&store, "SELECT COUNT(*) FROM occurrences"), 4);
    // One current occurrence per (mailbox, message).
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM (SELECT mailbox_id, message_id FROM occurrences
             WHERE state IN ('present','vanish_pending') GROUP BY 1, 2 HAVING COUNT(*) > 1)"
        ),
        0
    );
}

#[test]
fn listed_metadata_never_replaces_a_bodys_headers() {
    let (_scratch, store) = listing_store("proton-headers");
    let labels = system();
    write(&store, move |tx| {
        reconcile_proton_labels(tx, ACCOUNT, &labels)
    })
    .unwrap();
    commit(&store, vec![message("m1", &["0"], true)], "a1", 2).unwrap();
    write(&store, |tx| {
        tx.execute(
            "UPDATE message_headers SET raw_fields = CAST('Subject: Body' AS BLOB), subject = 'Body'",
            [],
        )
        .map(|_| ())
        .map_err(|e| classify(e, "test"))
    })
    .unwrap();
    commit(&store, vec![message("m1", &["0"], true)], "a2", 3).unwrap();
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM message_headers WHERE subject = 'Body'"
        ),
        1
    );

    let mut bad = message("m2", &["0"], true);
    bad.raw_metadata = b"{}".to_vec();
    let refused = commit(&store, vec![bad], "a2", 4);
    assert!(
        matches!(refused, Err(StoreError::InvalidInput { .. })),
        "{refused:?}"
    );
}

#[test]
fn a_page_outside_a_listing_is_refused() {
    let (_scratch, store) = listing_store("proton-not-listing");
    write(&store, |tx| finish_listing(tx, ACCOUNT, &[], 2).map(|_| ())).unwrap();
    let refused = commit(&store, vec![], "a1", 3);
    assert!(
        matches!(refused, Err(StoreError::Conflict { .. })),
        "{refused:?}"
    );
}

#[test]
fn finishing_records_totals_moves_the_cursor_and_shows_coverage() {
    let (_scratch, store) = listing_store("proton-finish");
    let labels = system();
    write(&store, move |tx| {
        reconcile_proton_labels(tx, ACCOUNT, &labels)
    })
    .unwrap();
    commit(
        &store,
        vec![
            message("m1", &["0", "5"], true),
            message("m2", &["5"], true),
        ],
        "a1",
        2,
    )
    .unwrap();

    let totals = vec![
        ("0".to_owned(), 1),
        ("5".to_owned(), 3),
        ("2".to_owned(), 9),
    ];
    let account = write(&store, move |tx| finish_listing(tx, ACCOUNT, &totals, 3)).unwrap();
    assert_eq!(account.listing_state, ProtonListingState::Complete);
    assert_eq!(account.event_cursor.as_deref(), Some("event-0"));
    assert!(account.pending_cursor.is_none() && account.listing_checkpoint.is_none());

    let coverage = reading(&store, |tx| proton_coverage(tx, ACCOUNT));
    assert_eq!(coverage.listing, ProtonListingState::Complete);
    assert_eq!(
        (
            coverage.messages,
            coverage.with_body,
            coverage.body_failures
        ),
        (2, 0, 0)
    );
    let by_name: Vec<(&str, u64, Option<u64>)> = coverage
        .mailboxes
        .iter()
        .map(|m| (m.remote_name.as_str(), m.local, m.remote_total))
        .collect();
    assert_eq!(
        by_name,
        [
            ("All Mail", 2, Some(3)),
            ("Inbox", 1, Some(1)),
            ("Starred", 0, None)
        ]
    );
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM mailboxes WHERE sync_state = 'synced'"
        ),
        3
    );
    assert_eq!(revision(&store), 4);
}
