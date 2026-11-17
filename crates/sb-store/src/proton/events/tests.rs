//! Proton event application (plan M2.15c1): changes replace the
//! listed metadata, labels follow, a delete is withheld, the cursor
//! moves with a response's last chunk, and a replay changes nothing.

use rusqlite::params;
use sb_test_support::{ScratchDir, block_on, real_fs};
use sb_types::time::Timestamp;

use super::*;
use crate::lists::MailboxRole;
use crate::proton::{
    NewProtonAccount, ProtonPasswordMode, begin_listing, commit_listed_page, finish_listing,
    insert_proton_account, proton_account,
};
use crate::sidebar::MailboxKind;
use crate::sync::{HeaderColumns, SyncedFlags};
use crate::testing::{reading, scratch_paths, test_seams};
use crate::{ListedPage, Store, WritePriority, open};

const ACCOUNT: &str = "00000000-0000-4000-8000-0000000000e1";

fn now() -> Timestamp {
    Timestamp::from_millis(1_735_689_600_000)
}

fn write<T: Send + 'static>(
    store: &Store,
    f: impl FnOnce(&Transaction) -> Result<T, StoreError> + Send + 'static,
) -> Result<T, StoreError> {
    block_on(store.write(WritePriority::Sync, f))
}

fn label(key: &str, name: &str, kind: MailboxKind, role: Option<MailboxRole>) -> ObservedLabel {
    ObservedLabel {
        provider_key: key.to_owned(),
        remote_name: name.to_owned(),
        delimiter: None,
        kind,
        role,
        selectable: true,
    }
}

fn system() -> Vec<ObservedLabel> {
    vec![
        label("0", "Inbox", MailboxKind::Folder, Some(MailboxRole::Inbox)),
        label("3", "Trash", MailboxKind::Folder, Some(MailboxRole::Trash)),
        label(
            "5",
            "All Mail",
            MailboxKind::Virtual,
            Some(MailboxRole::All),
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
            ..SyncedFlags::default()
        },
    }
}

/// A Proton account whose listing completed with m1 in the Inbox, at
/// cursor `event-0` and revision 4.
fn complete_store(label_name: &str) -> (ScratchDir, Store) {
    let (scratch, paths) = scratch_paths(label_name);
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
                credential_ref: "proton-e1".to_owned(),
                password_mode: ProtonPasswordMode::Login,
            },
        )?;
        begin_listing(tx, ACCOUNT, "event-0", 1)?;
        reconcile_proton_labels(tx, ACCOUNT, &system())?;
        commit_listed_page(
            tx,
            &ListedPage {
                account_id: ACCOUNT,
                messages: &[message("m1", &["0", "5"], true)],
                checkpoint: "a1",
                expected_revision: 2,
            },
            now(),
        )?;
        finish_listing(tx, ACCOUNT, &[], 3).map(drop)
    })
    .unwrap();
    (scratch, store)
}

fn apply(
    store: &Store,
    from: &str,
    labels: Option<Vec<ObservedLabel>>,
    changes: Vec<ProtonChange>,
    next: Option<&str>,
    revision: i64,
) -> Result<AppliedEvents, StoreError> {
    let from = from.to_owned();
    let next = next.map(str::to_owned);
    write(store, move |tx| {
        apply_event_chunk(
            tx,
            &EventChunk {
                account_id: ACCOUNT,
                from_cursor: &from,
                labels: labels.as_deref(),
                changes: &changes,
                next_cursor: next.as_deref(),
                expected_revision: revision,
            },
            now(),
        )
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

fn list_rows_of(store: &Store, id: &str) -> i64 {
    let id = id.to_owned();
    reading(store, move |tx| {
        Ok(tx
            .query_row(
                "SELECT COUNT(*) FROM d_list_rows r
                 JOIN logical_messages l ON l.message_id = r.message_id
                 WHERE l.provider_key = ?1",
                params![id],
                |row| row.get(0),
            )
            .unwrap())
    })
}

fn owned(rows: &[(&str, &str)]) -> Vec<(String, String)> {
    rows.iter()
        .map(|(a, b)| ((*a).to_owned(), (*b).to_owned()))
        .collect()
}

#[test]
fn changes_follow_the_metadata_and_labels_and_the_cursor_moves() {
    let (_scratch, store) = complete_store("proton-events-apply");
    let mut labels = system();
    labels.push(label("l1", "Travel", MailboxKind::Label, None));
    let mut moved = message("m1", &["3", "5", "l1"], false);
    moved.columns.subject = Some("Updated".to_owned());

    let applied = apply(
        &store,
        "event-0",
        Some(labels),
        vec![
            ProtonChange::Upsert(Box::new(moved)),
            ProtonChange::Upsert(Box::new(message("m2", &["0", "5"], true))),
        ],
        Some("event-1"),
        4,
    )
    .unwrap();
    assert_eq!(applied.messages.new_messages, 1);
    assert_eq!(applied.messages.vanished, 1);
    assert_eq!(applied.revision, 5);

    // The Inbox occurrence vanished; Trash and the new label are added.
    assert_eq!(
        occurrences(&store, "m1"),
        owned(&[
            ("0", "vanished"),
            ("3", "present"),
            ("5", "present"),
            ("l1", "present")
        ])
    );
    assert_eq!(
        occurrences(&store, "m2"),
        owned(&[("0", "present"), ("5", "present")])
    );
    let (subject, seen): (String, i64) = reading(&store, |tx| {
        Ok(tx
            .query_row(
                "SELECT h.subject, MIN(o.f_seen) FROM message_headers h
                 JOIN logical_messages l ON l.message_id = h.message_id
                 JOIN occurrences o ON o.message_id = l.message_id AND o.state = 'present'
                 WHERE l.provider_key = 'm1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap())
    });
    assert_eq!((subject.as_str(), seen), ("Updated", 1));
    let account = reading(&store, |tx| Ok(proton_account(tx, ACCOUNT)?.unwrap()));
    assert_eq!(account.event_cursor.as_deref(), Some("event-1"));
}

#[test]
fn a_delete_is_withheld_and_a_replayed_chunk_changes_nothing() {
    let (_scratch, store) = complete_store("proton-events-delete");
    let changes = vec![
        ProtonChange::Delete("m1".to_owned()),
        ProtonChange::Delete("unknown".to_owned()),
    ];
    // A first chunk of a longer response: the cursor stays.
    let first = apply(&store, "event-0", None, changes.clone(), None, 4).unwrap();
    assert_eq!((first.withheld, first.revision), (2, 4));
    assert_eq!(
        occurrences(&store, "m1"),
        owned(&[("0", "vanish_pending"), ("5", "vanish_pending")])
    );
    assert_eq!(list_rows_of(&store, "m1"), 0);

    // Replayed after a crash, with the response's last chunk this time.
    let replay = apply(&store, "event-0", None, changes, Some("event-1"), 4).unwrap();
    assert_eq!((replay.withheld, replay.revision), (0, 5));
    assert_eq!(
        occurrences(&store, "m1"),
        owned(&[("0", "vanish_pending"), ("5", "vanish_pending")])
    );
    let messages: i64 = reading(&store, |tx| {
        Ok(tx
            .query_row("SELECT COUNT(*) FROM logical_messages", [], |row| {
                row.get(0)
            })
            .unwrap())
    });
    assert_eq!(messages, 1);

    // A later change brings the message back.
    apply(
        &store,
        "event-1",
        None,
        vec![ProtonChange::Upsert(Box::new(message(
            "m1",
            &["0", "5"],
            true,
        )))],
        Some("event-2"),
        5,
    )
    .unwrap();
    assert_eq!(
        occurrences(&store, "m1"),
        owned(&[("0", "present"), ("5", "present")])
    );
    assert_eq!(list_rows_of(&store, "m1"), 2);
}

#[test]
fn a_chunk_off_the_cursor_or_before_a_complete_listing_is_refused() {
    let (_scratch, store) = complete_store("proton-events-refused");
    let change = || vec![ProtonChange::Upsert(Box::new(message("m2", &["0"], true)))];
    let refused = |result: Result<AppliedEvents, StoreError>| {
        assert!(
            matches!(result, Err(StoreError::Conflict { .. })),
            "expected Conflict, got {result:?}"
        );
    };
    refused(apply(
        &store,
        "event-9",
        None,
        change(),
        Some("event-10"),
        4,
    ));
    refused(apply(&store, "event-0", None, change(), Some("event-1"), 3));

    write(&store, |tx| {
        begin_listing(tx, ACCOUNT, "event-5", 4).map(drop)
    })
    .unwrap();
    refused(apply(&store, "event-0", None, change(), Some("event-1"), 5));
    assert!(occurrences(&store, "m2").is_empty());

    let too_many = (0..=MAX_HEADER_BATCH)
        .map(|n| ProtonChange::Delete(format!("d{n}")))
        .collect();
    assert!(matches!(
        apply(&store, "event-0", None, too_many, None, 5),
        Err(StoreError::InvalidInput { .. })
    ));
}
