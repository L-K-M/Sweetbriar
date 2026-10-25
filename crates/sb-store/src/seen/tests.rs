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
            inputs: 1,
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
