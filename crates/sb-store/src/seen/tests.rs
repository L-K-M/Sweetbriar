use super::*;
use crate::schema::{MIGRATIONS, run_migrations};
use crate::testing::{fixtures, test_seams};
use sb_test_support::real_fs;

#[test]
fn a_received_sweep_reuses_its_publication_statements() {
    use crate::testing::{scratch_paths, writing};

    const COUNT: i64 = 1_000;
    const SQL: [&str; 4] = [
        "SELECT length(CAST(email AS BLOB))+coalesce(length(CAST(display_name AS BLOB)),0)+coalesce(length(CAST(last_account_id AS BLOB)),0) FROM seen_addresses WHERE canonical=?1",
        "SELECT EXISTS(SELECT 1 FROM seen_address_messages WHERE message_id=?1 AND canonical=?2)",
        "SELECT times_received FROM seen_addresses WHERE canonical=?1",
        "INSERT INTO seen_address_messages(message_id,canonical) VALUES(?1,?2) ON CONFLICT(message_id,canonical) DO NOTHING",
    ];
    let (_scratch, paths) = scratch_paths("received-statement-reuse");
    let store = crate::store::open(&paths, &test_seams(real_fs())).unwrap();
    writing(&store, |tx| {
        fixtures::acquisitions(tx, 1);
        let account = fixtures::archive_account(tx);
        fixtures::mailbox(tx, &account, "received", "folder", None);
        tx.execute("WITH RECURSIVE n(id) AS (VALUES(1) UNION ALL SELECT id+1 FROM n WHERE id<?1) INSERT INTO logical_messages(message_id,account_id,acquisition_id,created_at) SELECT id,account_id,1,0 FROM n CROSS JOIN accounts WHERE provider='local_archive'", [COUNT]).unwrap();
        tx.execute_batch("INSERT INTO occurrences(occurrence_id,account_id,mailbox_id,message_id,state,first_seen_at,last_seen_at) SELECT message_id,account_id,1,message_id,'present',10,10 FROM logical_messages;
            INSERT INTO d_message_summary(acquisition_id,parser_version,parse_state,from_addr,has_attachments) VALUES(1,2,'ok','User@EXAMPLE.test',0);
            INSERT INTO seen_addresses(canonical,email,last_seen_at,times_received,times_sent_to) VALUES('User@example.test','User@example.test',0,11,9)").unwrap();
        note_inputs(tx)?;
        tx.flush_prepared_statement_cache();
        Ok(())
    });
    let started = std::time::Instant::now();
    let ledger = || {
        store
            .read(|tx| {
                Ok(tx
                    .query_row("SELECT count(*) FROM seen_address_messages", [], |row| {
                        row.get::<_, i64>(0)
                    })
                    .unwrap())
            })
            .unwrap()
    };
    let mut scan = store.read(begin).unwrap().unwrap();
    let mut commits = 0;
    loop {
        let before = ledger();
        let captured = store.read(|tx| capture(tx, scan)).unwrap();
        let prepared = prepare(captured).unwrap();
        let result = writing(&store, move |tx| publish(tx, prepared));
        assert!(ledger() - before <= MAX_RECEIVED_KEYS as i64);
        commits += 1;
        assert!(commits <= COUNT / MAX_RECEIVED_KEYS as i64 + 2);
        scan = match result {
            SeenCommit::Continue(next) => next,
            SeenCommit::Complete => break,
            SeenCommit::Recapture(_) => panic!("stable received sources recaptured"),
        };
    }
    assert_eq!(ledger(), COUNT);
    let history: (i64, i64) = store.read(|tx| {
        Ok(tx.query_row("SELECT times_received,times_sent_to FROM seen_addresses WHERE canonical='User@example.test'", [], |row| Ok((row.get(0)?, row.get(1)?))).unwrap())
    }).unwrap();
    assert_eq!(history, (COUNT + 11, 9));
    let status = store.read(revisions).unwrap();
    assert_eq!(status.inputs, status.swept);
    let runs = writing(&store, |tx| {
        Ok(SQL.map(|sql| {
            tx.prepare_cached(sql)
                .unwrap()
                .get_status(rusqlite::StatementStatus::Run)
        }))
    });
    store.close().unwrap();
    eprintln!(
        "received publication statements ran {runs:?}; sweep took {:?}",
        started.elapsed()
    );
    for (sql, runs) in SQL.into_iter().zip(runs) {
        assert!(
            runs >= COUNT as i32,
            "publication statement was repeatedly compiled: {sql}"
        );
    }
}

#[test]
fn migration_preserves_kept_history_identities_and_refuses_older_writers() {
    let mut connection = fixtures::index_connection_through(6);
    let account = fixtures::archive_account(&connection);
    fixtures::acquisitions(&connection, 1);
    let message = fixtures::message(&connection, &account, Some(1));
    connection.execute("INSERT INTO seen_addresses(canonical,email,display_name,last_seen_at,times_sent_to,times_received,last_account_id) VALUES('User@example.test','User@EXAMPLE.test','history',-7,9,11,?1)", [&account]).unwrap();
    let before: (String,String,i64,i64,i64,String) = connection.query_row(
        "SELECT email,display_name,last_seen_at,times_sent_to,times_received,last_account_id FROM seen_addresses", [],
        |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?))).unwrap();
    run_migrations(&mut connection, &test_seams(real_fs()), MIGRATIONS).unwrap();
    let after = connection.query_row(
        "SELECT email,display_name,last_seen_at,times_sent_to,times_received,last_account_id FROM seen_addresses", [],
        |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,i64>(2)?,row.get::<_,i64>(3)?,row.get::<_,i64>(4)?,row.get::<_,String>(5)?))).unwrap();
    assert_eq!(after, before);
    assert_eq!(fixtures::archive_account(&connection), account);
    assert_eq!(
        connection
            .query_row("SELECT message_id FROM logical_messages", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        message
    );
    let tx = connection.transaction().unwrap();
    assert_eq!(
        revisions(&tx).unwrap(),
        SeenRevisions {
            target: 1,
            // Migration 38 advanced the input revision for the account
            // senders' readiness watermark.
            inputs: 2,
            swept: 0
        }
    );
    assert_eq!(
        tx.query_row("SELECT count(*) FROM seen_address_messages", [], |row| row
            .get::<_, i64>(
            0
        ))
        .unwrap(),
        0
    );
    assert!(
        tx.execute(
            "INSERT INTO seen_address_messages VALUES(999,'User@example.test')",
            []
        )
        .is_err()
    );
    assert!(
        tx.execute(
            "INSERT INTO seen_address_messages VALUES(?1,'missing@example.test')",
            [message]
        )
        .is_err()
    );
    tx.execute(
        "INSERT INTO seen_address_messages VALUES(?1,'User@example.test')",
        [message],
    )
    .unwrap();
    assert!(
        tx.execute(
            "INSERT INTO seen_address_messages VALUES(?1,'User@example.test')",
            [message]
        )
        .is_err()
    );
    tx.commit().unwrap();
    assert!(matches!(
        run_migrations(&mut connection, &test_seams(real_fs()), &MIGRATIONS[..6]),
        Err(StoreError::NewerStore { .. })
    ));
    assert_eq!(
        connection
            .query_row("SELECT times_sent_to FROM seen_addresses", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        9
    );
}

#[test]
fn actual_ddl_copy_counts_and_unicode_bound_make_a_legal_key_fit() {
    let connection = fixtures::index_connection();
    let (strict, without_rowid): (i64, i64) = connection
        .query_row(
            "SELECT strict,wr FROM pragma_table_list WHERE name='seen_address_messages'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((strict, without_rowid), (1, 1));
    let canonical_index: i64 = connection
        .query_row(
            "SELECT count(*) FROM pragma_index_list('seen_addresses') WHERE origin='pk'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(canonical_index, 1);
    assert_eq!(CANONICAL_COPIES, 1 + canonical_index as usize + 1);
    // No original-domain DNS shortcut: verify the pinned Unicode lowercase byte-growth bound.
    for scalar in 0..=0x10ffff {
        let Some(character) = char::from_u32(scalar) else {
            continue;
        };
        let lower: usize = character.to_lowercase().map(char::len_utf8).sum();
        assert!(
            lower * 2 <= character.len_utf8() * 3,
            "Unicode lowercase expansion: {character:?}"
        );
    }
    // 3 canonical copies (<=1.5A each), email A, name <=3N; A+N<=headercap.
    // Max(5.5A+3N) <=5.5H, plus the UUID and one bounded framing allowance.
    let worst = crate::messages::MAX_READER_HEADER_BYTES * 11 / 2 + 36 + KEY_FRAMING;
    assert!(worst < MAX_RECEIVED_BYTES);
}

/// A non-local sending account for the sweep tests.
fn sweep_account(conn: &rusqlite::Connection, id: &str, sort: i64) {
    conn.execute(
        "INSERT INTO accounts (account_id, kind, provider, display_name, state, sort_order,
         created_at) VALUES (?1, 'imap', 'generic_imap', ?1, 'active', ?2, 0)",
        rusqlite::params![id, sort],
    )
    .unwrap();
}

/// An identity address, so a message from it is the account's own mail.
fn sweep_identity(conn: &rusqlite::Connection, account: &str, email: &str) {
    conn.execute(
        "INSERT INTO identities (identity_id, account_id, email, display_name, is_default,
         sendable) VALUES (?1, ?2, ?3, ?3, 1, 'validated')",
        rusqlite::params![format!("{account}-{email}"), account, email],
    )
    .unwrap();
}

/// One eligible source: a message with an occurrence and a complete
/// summary whose From is `from`. Arrival alone writes no sender row.
fn sweep_source(
    conn: &rusqlite::Connection,
    account: &str,
    mailbox: i64,
    acquisition: i64,
    from: &str,
    first_seen_at: i64,
) {
    let message = fixtures::message(conn, account, Some(acquisition));
    conn.execute(
        "INSERT INTO occurrences (account_id, mailbox_id, message_id, state, first_seen_at,
         last_seen_at) VALUES (?1, ?2, ?3, 'present', ?4, ?4)",
        rusqlite::params![account, mailbox, message, first_seen_at],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO d_message_summary (acquisition_id, parser_version, parse_state, from_addr,
         has_attachments) VALUES (?1, 2, 'ok', ?2, 0)",
        rusqlite::params![acquisition, from],
    )
    .unwrap();
}

/// Run one owed pass to completion on a short-lived transaction.
fn sweep_to_completion(conn: &mut rusqlite::Connection) {
    let tx = conn.transaction().unwrap();
    let mut scan = begin(&tx).unwrap().expect("the pass is owed");
    loop {
        let captured = capture(&tx, scan).unwrap();
        let prepared = prepare(captured).unwrap();
        scan = match publish(&tx, prepared).unwrap() {
            SeenCommit::Continue(next) => next,
            SeenCommit::Complete => break,
            SeenCommit::Recapture(_) => panic!("stable received sources recaptured"),
        };
    }
    tx.commit().unwrap();
}

#[test]
fn the_sweep_records_each_account_s_first_from_once() {
    let mut connection = fixtures::index_connection();
    let archive = fixtures::archive_account(&connection);
    sweep_account(&connection, "a-account", 1);
    sweep_account(&connection, "b-account", 2);
    sweep_identity(&connection, "a-account", "Me.A@Example.Test");
    let a_mailbox = fixtures::mailbox(&connection, "a-account", "Inbox", "folder", Some("inbox"));
    let b_mailbox = fixtures::mailbox(&connection, "b-account", "Inbox", "folder", Some("inbox"));
    let archive_mailbox = fixtures::mailbox(&connection, &archive, "Imported", "folder", None);
    fixtures::acquisitions(&connection, 4);
    sweep_source(
        &connection,
        "a-account",
        a_mailbox,
        1,
        "alice@external.test",
        10,
    );
    // The account's own identity address is its sent mail, not a sender.
    sweep_source(
        &connection,
        "a-account",
        a_mailbox,
        2,
        "Me.A@Example.Test",
        11,
    );
    sweep_source(
        &connection,
        "b-account",
        b_mailbox,
        3,
        "bob@external.test",
        12,
    );
    // Imported mail doesn't say which sending account received it.
    sweep_source(
        &connection,
        &archive,
        archive_mailbox,
        4,
        "carol@external.test",
        13,
    );

    let before: i64 = connection
        .query_row("SELECT count(*) FROM d_account_senders", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(before, 0, "arrival alone must not write a sender row");

    sweep_to_completion(&mut connection);

    let rows: Vec<(String, String)> = connection
        .prepare(
            "SELECT canonical, account_id FROM d_account_senders ORDER BY canonical, account_id",
        )
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        rows,
        vec![
            ("alice@external.test".to_owned(), "a-account".to_owned()),
            ("bob@external.test".to_owned(), "b-account".to_owned()),
        ]
    );

    // Another full pass over the same messages records nothing further.
    let tx = connection.transaction().unwrap();
    note_inputs(&tx).unwrap();
    tx.commit().unwrap();
    sweep_to_completion(&mut connection);
    assert_eq!(sender_rows(&connection).len(), 2);
}

/// Every recorded sender row, ordered.
fn sender_rows(conn: &rusqlite::Connection) -> Vec<(String, String, i64)> {
    conn.prepare(
        "SELECT canonical, account_id, first_seen_at FROM d_account_senders
         ORDER BY canonical, account_id",
    )
    .unwrap()
    .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
    .unwrap()
    .collect::<Result<_, _>>()
    .unwrap()
}

/// The sources the backfill and incremental tests seed: two accounts,
/// one sender writing to both and an own-identity message.
const SEEDED: [(&str, i64, &str, i64); 4] = [
    ("a-account", 1, "alice@external.test", 10),
    ("b-account", 2, "alice@external.test", 12),
    ("b-account", 3, "Bob@External.Test", 13),
    ("a-account", 4, "me@a.test", 14),
];

/// Seed the accounts, their identities and mailboxes; returns the two
/// mailboxes by account.
fn seed_accounts(conn: &rusqlite::Connection) -> [(&'static str, i64); 2] {
    sweep_account(conn, "a-account", 1);
    sweep_account(conn, "b-account", 2);
    sweep_identity(conn, "a-account", "me@a.test");
    fixtures::acquisitions(conn, SEEDED.len());
    ["a-account", "b-account"].map(|account| {
        (
            account,
            fixtures::mailbox(conn, account, "Inbox", "folder", None),
        )
    })
}

fn seed_source(conn: &rusqlite::Connection, mailboxes: &[(&str, i64); 2], index: usize) {
    let (account, acquisition, from, seen) = SEEDED[index];
    let mailbox = mailboxes.iter().find(|(id, _)| *id == account).unwrap().1;
    sweep_source(conn, account, mailbox, acquisition, from, seen);
}

#[test]
fn migration_38_s_backfill_matches_the_incremental_rows() {
    // Backfill: mail stored before migration 38, recorded by the first
    // pass after it.
    let mut backfilled = fixtures::index_connection_through(37);
    let mailboxes = seed_accounts(&backfilled);
    for index in 0..SEEDED.len() {
        seed_source(&backfilled, &mailboxes, index);
    }
    run_migrations(&mut backfilled, &test_seams(real_fs()), MIGRATIONS).unwrap();
    assert!(sender_rows(&backfilled).is_empty());
    sweep_to_completion(&mut backfilled);

    // Incremental: the same mail arriving one message at a time, each
    // followed by its pass.
    let mut incremental = fixtures::index_connection();
    let mailboxes = seed_accounts(&incremental);
    sweep_to_completion(&mut incremental);
    for index in 0..SEEDED.len() {
        seed_source(&incremental, &mailboxes, index);
        let tx = incremental.transaction().unwrap();
        note_inputs(&tx).unwrap();
        tx.commit().unwrap();
        sweep_to_completion(&mut incremental);
    }

    let rows = sender_rows(&backfilled);
    assert_eq!(
        rows,
        vec![
            ("Bob@external.test".to_owned(), "b-account".to_owned(), 13),
            ("alice@external.test".to_owned(), "a-account".to_owned(), 10),
            ("alice@external.test".to_owned(), "b-account".to_owned(), 12),
        ]
    );
    assert_eq!(sender_rows(&incremental), rows);

    // A vanished occurrence keeps its row through later passes.
    let tx = incremental.transaction().unwrap();
    tx.execute("UPDATE occurrences SET state = 'vanished'", [])
        .unwrap();
    note_inputs(&tx).unwrap();
    tx.commit().unwrap();
    sweep_to_completion(&mut incremental);
    assert_eq!(sender_rows(&incremental), rows);
}

#[test]
fn a_sender_rebuild_keeps_allowances_and_waits_for_its_pass() {
    let mut connection = fixtures::index_connection();
    let mailboxes = seed_accounts(&connection);
    seed_source(&connection, &mailboxes, 1);
    sweep_to_completion(&mut connection);
    let rows = sender_rows(&connection);
    assert_eq!(rows.len(), 1);

    let tx = connection.transaction().unwrap();
    crate::recipients::allow_recipient(&tx, "a-account", "alice@external.test", 7).unwrap();
    assert!(crate::recipients::account_senders_ready(&tx).unwrap());
    crate::recipients::rebuild_account_senders(&tx).unwrap();
    assert!(!crate::recipients::account_senders_ready(&tx).unwrap());
    tx.commit().unwrap();
    assert!(sender_rows(&connection).is_empty());

    sweep_to_completion(&mut connection);
    let tx = connection.transaction().unwrap();
    assert!(crate::recipients::account_senders_ready(&tx).unwrap());
    assert_eq!(
        crate::recipients::recipient_allowances(&tx).unwrap().len(),
        1
    );
    tx.commit().unwrap();
    assert_eq!(sender_rows(&connection), rows);
}

#[test]
fn senders_are_ready_only_after_a_complete_pass() {
    let mut connection = fixtures::index_connection();
    sweep_account(&connection, "a-account", 1);
    let tx = connection.transaction().unwrap();
    assert!(!crate::recipients::account_senders_ready(&tx).unwrap());

    // Recorded history alone doesn't make the check ready; the sweep has
    // to reach the revision migration 38 stored.
    tx.execute(
        "INSERT INTO d_account_senders(canonical, account_id, first_seen_at)
         VALUES ('someone@example.test', 'a-account', 0)",
        [],
    )
    .unwrap();
    assert!(!crate::recipients::account_senders_ready(&tx).unwrap());

    let mut scan = begin(&tx).unwrap().expect("the pass is owed");
    loop {
        let captured = capture(&tx, scan).unwrap();
        let prepared = prepare(captured).unwrap();
        scan = match publish(&tx, prepared).unwrap() {
            SeenCommit::Continue(next) => next,
            SeenCommit::Complete => break,
            SeenCommit::Recapture(_) => panic!("an empty pass cannot recapture"),
        };
    }
    assert!(crate::recipients::account_senders_ready(&tx).unwrap());
    tx.commit().unwrap();
}
