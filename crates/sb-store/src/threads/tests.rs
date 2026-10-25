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

/// An empty store owes nothing; imports add to the owed count; a
/// rebuild's mark never moves the watermark back.
#[test]
fn watermarks_track_owed_rebuilds() {
    let (mut conn, _account) = store();
    let tx = conn.transaction().unwrap();
    let first = thread_revisions(&tx).unwrap();
    assert_eq!(
        first,
        ThreadRevisions {
            inputs: 0,
            built: 0
        }
    );
    assert!(!first.stale());
    note_new_messages(&tx).unwrap();
    note_new_messages(&tx).unwrap();
    assert!(thread_revisions(&tx).unwrap().stale());
    mark_threads_built(&tx, 2).unwrap();
    mark_threads_built(&tx, 1).unwrap();
    let built = thread_revisions(&tx).unwrap();
    assert_eq!(
        built,
        ThreadRevisions {
            inputs: 2,
            built: 2
        }
    );
    assert!(!built.stale());
}

/// A store holding messages before migration 4 owes one rebuild.
#[test]
fn migration_4_owes_a_rebuild_only_for_held_messages() {
    let mut conn = Connection::open_in_memory().unwrap();
    run_migrations(&mut conn, &test_seams(real_fs()), &MIGRATIONS[..3]).unwrap();
    conn.execute(
        "INSERT INTO logical_messages (account_id, created_at)
         SELECT account_id, ?1 FROM accounts WHERE provider = 'local_archive'",
        [FIXED_NOW_MS],
    )
    .unwrap();
    run_migrations(&mut conn, &test_seams(real_fs()), MIGRATIONS).unwrap();
    let tx = conn.transaction().unwrap();
    assert_eq!(
        thread_revisions(&tx).unwrap(),
        ThreadRevisions {
            inputs: OWED_AT_MIGRATION,
            built: FIRST_BUILT
        }
    );
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
            representative_date: Some(1_000),
        }]
    );
    let rest = thread_sources(&tx, first, 10).unwrap();
    assert_eq!(rest.len(), 1);
    assert_eq!(rest[0].header_id.as_deref(), Some("header@x"));
    assert_eq!(rest[0].subject.as_deref(), Some("From headers"));
    // NULL header references fall back to the summary's empty list.
    assert!(rest[0].references.is_empty());
}

#[test]
fn source_pages_reuse_their_query_plan() {
    const MESSAGES: i64 = 128;
    let (mut conn, account) = store();
    for id in 1..=MESSAGES {
        message(&mut conn, &account, "Plan", "[]", id);
    }
    let tx = conn.transaction().unwrap();
    let sql = source_sql();
    let after = MESSAGES / 2;
    let plan = tx
        .prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
        .unwrap()
        .query_map(params![after, 1], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    let result = thread_sources(&tx, after, 1).unwrap();
    assert_eq!(result.len(), 1);
    assert_eq!(result[0].message_id, after + 1);
    let cases = [
        (0, 0, vec![]),
        (MESSAGES, 1, vec![]),
        (MESSAGES - 2, usize::MAX, vec![MESSAGES - 1, MESSAGES]),
        (MESSAGES - 1, 2, vec![MESSAGES]),
    ];
    for (after, limit, expected) in &cases {
        let ids: Vec<_> = thread_sources(&tx, *after, *limit)
            .unwrap()
            .into_iter()
            .map(|source| source.message_id)
            .collect();
        assert_eq!(&ids, expected);
    }
    let statement = tx.prepare_cached(&sql).unwrap();
    assert_eq!(
        statement.get_status(rusqlite::StatementStatus::Run),
        1 + cases.len() as i32,
        "probe did not inspect the actual source statement"
    );
    let scans = statement.get_status(rusqlite::StatementStatus::FullscanStep);
    let sorts = statement.get_status(rusqlite::StatementStatus::Sort);
    assert_eq!(
        (scans, sorts),
        (0, 0),
        "source pages scanned or sorted: {plan:?}"
    );
    assert_eq!(
        statement.get_status(rusqlite::StatementStatus::RePrepare),
        0,
        "rebinding a source page should reuse the prepared query plan"
    );
}

/// The cleanup removes threads without members, as a crash between
/// creating a thread and filling it leaves, and keeps the rest.
#[test]
fn empty_threads_are_removed() {
    let (mut conn, account) = store();
    let message = message(&mut conn, &account, "Plan", "[]", 1_000);
    let tx = conn.transaction().unwrap();
    let blank = ThreadDescription {
        account_scope: None,
        subject_norm: None,
        last_date: None,
    };
    let filled = create_thread(&tx, blank).unwrap();
    let empty = create_thread(&tx, blank).unwrap();
    assign_thread(&tx, message, filled).unwrap();
    remove_empty_threads(&tx).unwrap();
    let left: Vec<i64> = stored_threads(&tx, 0, 10)
        .unwrap()
        .iter()
        .map(|thread| thread.thread_id)
        .collect();
    assert_eq!(left, vec![filled]);
    assert_ne!(filled, empty);
}

/// Assigning and describing report only real changes, a message holds
/// one thread, removal takes leftover members, sources carry their
/// representatives' dates, and stored threads read back as written.
#[test]
fn memberships_change_only_when_they_differ() {
    let (mut conn, account) = store();
    let older = message(&mut conn, &account, "Plan", "[]", 1_000);
    let newer = message(&mut conn, &account, "Plan", "[]", 3_000);
    let tx = conn.transaction().unwrap();
    let blank = ThreadDescription {
        account_scope: None,
        subject_norm: None,
        last_date: None,
    };
    let planned = ThreadDescription {
        account_scope: Some(&account),
        subject_norm: Some("plan"),
        last_date: Some(3_000),
    };
    let one = create_thread(&tx, planned).unwrap();
    let two = create_thread(&tx, blank).unwrap();
    assert!(assign_thread(&tx, older, one).unwrap());
    assert!(!assign_thread(&tx, older, one).unwrap());
    assert!(assign_thread(&tx, newer, two).unwrap());
    assert!(assign_thread(&tx, newer, one).unwrap());
    assert_eq!(
        thread_memberships(&tx, 0, 10).unwrap(),
        vec![(older, one), (newer, one)]
    );
    assert!(!describe_thread(&tx, one, planned).unwrap());
    let undated = ThreadDescription {
        last_date: None,
        ..planned
    };
    assert!(describe_thread(&tx, one, undated).unwrap());
    assert_eq!(
        stored_threads(&tx, 0, 10).unwrap(),
        vec![
            StoredThread {
                thread_id: one,
                account_scope: Some(account.clone()),
                subject_norm: Some("plan".to_owned()),
                last_date: None,
            },
            StoredThread {
                thread_id: two,
                account_scope: None,
                subject_norm: None,
                last_date: None,
            },
        ]
    );
    let dates: Vec<Option<i64>> = thread_sources(&tx, 0, 10)
        .unwrap()
        .iter()
        .map(|source| source.representative_date)
        .collect();
    assert_eq!(dates, vec![Some(1_000), Some(3_000)]);

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
    insert(create_thread(&tx, blank).unwrap()).unwrap();
    assert!(insert(create_thread(&tx, blank).unwrap()).is_err());
}
