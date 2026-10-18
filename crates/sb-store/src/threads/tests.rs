use std::collections::BTreeSet;

use rusqlite::{Connection, params};
use sb_test_support::real_fs;

use super::*;
use crate::schema::{MIGRATIONS, run_migrations};
use crate::testing::{FIXED_NOW_MS, test_seams};

fn store() -> (Connection, String) {
    let mut conn = Connection::open_in_memory().unwrap();
    run_migrations(&mut conn, &test_seams(real_fs()), MIGRATIONS).unwrap();
    let account = conn
        .query_row(
            "SELECT account_id FROM accounts WHERE provider = 'local_archive'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    (conn, account)
}

/// A logical message whose summary carries `subject` and `references`,
/// listed by one occurrence dated `date`.
fn message(
    conn: &mut Connection,
    account: &str,
    subject: &str,
    references: &str,
    date: i64,
) -> i64 {
    let tx = conn.transaction().unwrap();
    let mailbox: i64 = match tx
        .query_row("SELECT mailbox_id FROM mailboxes", [], |row| row.get(0))
        .optional()
        .unwrap()
    {
        Some(mailbox) => mailbox,
        None => {
            tx.execute(
                "INSERT INTO mailboxes (account_id, remote_name, kind, selectable, sync_state)
                 VALUES (?1, 'Inbox', 'folder', 1, 'synced')",
                [account],
            )
            .unwrap();
            tx.last_insert_rowid()
        }
    };
    tx.execute(
        "INSERT INTO acquisitions (content_sha256, byte_length, object_state, first_acquired_at)
         VALUES (?1, 1, 'published', ?2)",
        params![format!("{:064x}", date), FIXED_NOW_MS],
    )
    .unwrap();
    let acquisition = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO d_message_summary (acquisition_id, parser_version, parse_state,
         header_message_id, subject, references_json, has_attachments)
         VALUES (?1, 1, 'ok', ?2, ?3, ?4, 0)",
        params![acquisition, format!("{date}@x"), subject, references],
    )
    .unwrap();
    tx.execute(
        "INSERT INTO logical_messages (account_id, acquisition_id, created_at) VALUES (?1, ?2, ?3)",
        params![account, acquisition, FIXED_NOW_MS],
    )
    .unwrap();
    let id = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO occurrences (account_id, mailbox_id, message_id, internal_date, state,
         first_seen_at, last_seen_at) VALUES (?1, ?2, ?3, ?4, 'present', ?5, ?5)",
        params![account, mailbox, id, date, FIXED_NOW_MS],
    )
    .unwrap();
    let occurrence = tx.last_insert_rowid();
    list_rows::refresh(&tx, &BTreeSet::from([occurrence])).unwrap();
    tx.commit().unwrap();
    id
}

/// A migrated store owes one rebuild; imports add to the owed count;
/// a rebuild's mark never moves the watermark back.
#[test]
fn watermarks_track_owed_rebuilds() {
    let (mut conn, _account) = store();
    let tx = conn.transaction().unwrap();
    let first = thread_revisions(&tx).unwrap();
    assert_eq!(
        first,
        ThreadRevisions {
            inputs: FIRST_INPUTS,
            built: FIRST_BUILT
        }
    );
    assert!(first.stale());
    note_new_messages(&tx).unwrap();
    assert_eq!(thread_revisions(&tx).unwrap().inputs, FIRST_INPUTS + 1);
    mark_threads_built(&tx, FIRST_INPUTS + 1).unwrap();
    mark_threads_built(&tx, FIRST_INPUTS).unwrap();
    let built = thread_revisions(&tx).unwrap();
    assert_eq!(built.built, FIRST_INPUTS + 1);
    assert!(!built.stale());
}

/// Sources page by message id, with references decoded and header
/// fields winning over the summary's (spec/02).
#[test]
fn sources_page_by_message_with_headers_first() {
    let (mut conn, account) = store();
    let first = message(&mut conn, &account, "Plan", "[\"a@x\",\"b@x\"]", 1_000);
    let second = message(&mut conn, &account, "Notes", "[]", 2_000);
    conn.execute(
        "INSERT INTO message_headers (message_id, raw_fields, header_message_id, subject,
         references_json, fetched_at) VALUES (?1, x'00', 'header@x', 'From headers', NULL, ?2)",
        params![second, FIXED_NOW_MS],
    )
    .unwrap();
    let tx = conn.transaction().unwrap();
    let page = thread_sources(&tx, 0, 1).unwrap();
    assert_eq!(
        page,
        vec![ThreadSource {
            message_id: first,
            account_id: account.clone(),
            header_id: Some("1000@x".to_owned()),
            references: vec!["a@x".to_owned(), "b@x".to_owned()],
            in_reply_to: None,
            subject: Some("Plan".to_owned()),
        }]
    );
    let rest = thread_sources(&tx, first, 10).unwrap();
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].header_id.as_deref(), Some("header@x"));
    assert_eq!(rest[0].subject.as_deref(), Some("From headers"));
    // NULL header references fall back to the summary's empty list.
    assert!(rest[0].references.is_empty());
}

/// Assigning reports only real changes, a message holds one thread,
/// removal takes leftover members, and a thread's date is its newest
/// representative's.
#[test]
fn memberships_change_only_when_they_differ() {
    let (mut conn, account) = store();
    let older = message(&mut conn, &account, "Plan", "[]", 1_000);
    let newer = message(&mut conn, &account, "Plan", "[]", 3_000);
    let tx = conn.transaction().unwrap();
    let one = create_thread(&tx, Some(&account), Some("plan")).unwrap();
    let two = create_thread(&tx, None, None).unwrap();
    assert!(assign_thread(&tx, older, one).unwrap());
    assert!(!assign_thread(&tx, older, one).unwrap());
    assert!(assign_thread(&tx, newer, two).unwrap());
    assert!(assign_thread(&tx, newer, one).unwrap());
    assert_eq!(
        thread_memberships(&tx, 0, 10).unwrap(),
        vec![(older, one), (newer, one)]
    );
    assert!(!describe_thread(&tx, one, Some(&account), Some("plan")).unwrap());
    assert!(describe_thread(&tx, one, None, Some("plan")).unwrap());

    date_thread(&tx, one).unwrap();
    let last: Option<i64> = tx
        .query_row(
            "SELECT last_date FROM d_threads WHERE thread_id = ?1",
            [one],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(last, Some(3_000));

    assert!(!remove_thread(&tx, two).unwrap());
    assert!(remove_thread(&tx, one).unwrap());
    assert!(thread_memberships(&tx, 0, 10).unwrap().is_empty());
    // The index keeps a message in one thread, whatever writes it.
    let insert = |thread: i64| {
        tx.execute(
            "INSERT INTO d_thread_members (thread_id, message_id) VALUES (?1, ?2)",
            params![thread, older],
        )
    };
    insert(create_thread(&tx, None, None).unwrap()).unwrap();
    assert!(insert(create_thread(&tx, None, None).unwrap()).is_err());
}
