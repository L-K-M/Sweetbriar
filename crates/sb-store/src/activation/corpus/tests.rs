use super::*;
use crate::schema::{MIGRATIONS, run_migrations};
use crate::testing::{fixtures, test_seams};
use sb_test_support::real_fs;

#[test]
fn a_corpus_batch_reuses_its_hot_inspection_statement() {
    use crate::testing::{scratch_paths, writing};
    use sb_test_support::block_on;

    const COUNT: i64 = 100;
    let (_scratch, paths) = scratch_paths("activation-statement-reuse");
    let store = crate::store::open(&paths, &test_seams(real_fs())).unwrap();
    writing(&store, |tx| {
        fixtures::acquisitions(tx, 1);
        let account = fixtures::archive_account(tx);
        fixtures::mailbox(tx, &account, "corpus", "folder", None);
        tx.execute("WITH RECURSIVE n(id) AS (VALUES(1) UNION ALL SELECT id+1 FROM n WHERE id<?1) INSERT INTO logical_messages(message_id,account_id,acquisition_id,created_at) SELECT id,account_id,1,0 FROM n CROSS JOIN accounts WHERE provider='local_archive'", [COUNT]).unwrap();
        tx.execute_batch("INSERT INTO occurrences(occurrence_id,account_id,mailbox_id,message_id,state,first_seen_at,last_seen_at) SELECT message_id,account_id,1,message_id,'present',10,10 FROM logical_messages;
            INSERT INTO d_message_summary(acquisition_id,parser_version,parse_state,subject,has_attachments) VALUES(1,1,'ok','old',0);
            INSERT INTO d_repair_assemblies SELECT acquisition_id,2,content_sha256,byte_length,NULL,0,'[null,3,null,null,null,null,null,null,null,null,null,null]',NULL FROM acquisitions;
            INSERT INTO d_repair_pieces VALUES(1,2,1,0,'new');
            UPDATE d_repair_assemblies SET completed_at=0").unwrap();
        crate::list_rows::refresh(tx, &(1..=COUNT).collect()).unwrap();
        tx.flush_prepared_statement_cache();
        Ok(())
    });
    let started = std::time::Instant::now();
    let mut scan = block_on(store.begin_catchup(2)).unwrap().unwrap();
    let mut preparations = 0;
    loop {
        preparations += 1;
        scan = match store.prepare_catchup_step(scan, &|| false).unwrap() {
            CatchupPreparation::Continue(next) => next,
            CatchupPreparation::Prepared(batch) => {
                match block_on(store.commit_catchup_batch(batch, 2, Timestamp::from_millis(10)))
                    .unwrap()
                {
                    CatchupCommit::Continue(next, _) => next,
                    CatchupCommit::Complete(_) => break,
                    other => panic!("stable corpus recaptured: {other:?}"),
                }
            }
            other => panic!("unexpected corpus preparation: {other:?}"),
        };
    }
    let executions = |tx: &Transaction<'_>| {
        let sql = format!(
            "SELECT o.message_id,o.state,{} FROM occurrences o LEFT JOIN mailboxes mb ON mb.mailbox_id=o.mailbox_id WHERE o.occurrence_id=?1",
            list_rows::KIND_RANK
        );
        let statement = tx.prepare_cached(&sql).unwrap();
        let inspection = statement.get_status(rusqlite::StatementStatus::Run);
        let acquisition = tx
            .prepare_cached(
                "SELECT acquisition_id, content_sha256, byte_length, object_state
         FROM acquisitions WHERE acquisition_id = ?1",
            )
            .unwrap()
            .get_status(rusqlite::StatementStatus::Run);
        Ok((inspection, acquisition))
    };
    let writer_runs = writing(&store, executions);
    let reader_runs = store.read(executions).unwrap();
    let activated: i64 = store.read(|tx| {
        Ok(tx.query_row("SELECT count(*) FROM d_message_bindings b JOIN d_list_rows r USING(message_id) WHERE r.subject='new'", [], |row| row.get(0)).unwrap())
    }).unwrap();
    assert_eq!(activated, COUNT);
    let status = store.catchup_status(2).unwrap();
    assert_eq!(status.inputs, status.swept);
    store.close().unwrap();
    eprintln!(
        "inspection/acquisition statements ran {writer_runs:?} writer/{reader_runs:?} reader times; batch took {:?}",
        started.elapsed()
    );
    for (runs, acquisitions) in [writer_runs, reader_runs] {
        assert!(
            runs >= COUNT as i32,
            "the hot inspection statement was repeatedly compiled"
        );
        assert!(
            acquisitions >= COUNT as i32,
            "the acquisition lookup was repeatedly compiled"
        );
    }
    assert!(
        preparations <= COUNT * 3,
        "short pages took {preparations} preparations despite observed EOF"
    );
}

#[test]
fn migration_preserves_v11_rows_and_old_schema_operations_are_unavailable() {
    let mut conn = fixtures::index_connection_through(11);
    assert!(matches!(
        status(&conn, 2),
        Err(StoreError::Unavailable { .. })
    ));
    fixtures::acquisitions(&conn, 1);
    let account = fixtures::archive_account(&conn);
    fixtures::message(&conn, &account, Some(1));
    conn.execute_batch("INSERT INTO seen_addresses(canonical,email,last_seen_at,times_received,times_sent_to) VALUES('kept@x.test','kept@x.test',0,11,7)").unwrap();
    let snapshot = |conn: &Connection| -> String {
        conn.query_row("SELECT json_array((SELECT json_group_array(json_array(key,value)) FROM store_meta WHERE key NOT LIKE 'repair_%' AND key NOT IN('last_migrated_by','app_version_last_opened')),(SELECT json_group_array(json_array(message_id,account_id,acquisition_id)) FROM logical_messages),(SELECT json_group_array(json_array(canonical,email,times_received,times_sent_to)) FROM seen_addresses))", [], |row| row.get(0)).unwrap()
    };
    let before = snapshot(&conn);
    run_migrations(&mut conn, &test_seams(real_fs()), MIGRATIONS).unwrap();
    assert_eq!(snapshot(&conn), before);
    let value = status(&conn, 2).unwrap();
    assert_eq!(
        (value.inputs, value.swept, value.deferred, value.unready),
        (1, 0, 0, 0)
    );
    assert!(matches!(
        run_migrations(&mut conn, &test_seams(real_fs()), &MIGRATIONS[..11]),
        Err(StoreError::NewerStore { .. })
    ));
    assert_eq!(snapshot(&conn), before);
    assert_eq!(
        conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        0
    );
}
