use super::*;
use crate::schema::{MIGRATIONS, run_migrations};
use crate::testing::{fixtures, test_seams};
use sb_test_support::real_fs;

mod immutability;

#[test]
fn migration_is_additive_strict_and_older_writers_refuse_it() {
    let mut conn = fixtures::index_connection_through(7);
    let account = fixtures::archive_account(&conn);
    fixtures::acquisitions(&conn, 1);
    let message = fixtures::message(&conn, &account, Some(1));
    conn.execute_batch("INSERT INTO seen_addresses(canonical,email,last_seen_at,times_received,times_sent_to) VALUES('kept@x.test','kept@x.test',1,11,7)").unwrap();
    conn.execute(
        "INSERT INTO seen_address_messages VALUES(?1,'kept@x.test')",
        [message],
    )
    .unwrap();
    run_migrations(&mut conn, &test_seams(real_fs()), MIGRATIONS).unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT times_received+times_sent_to FROM seen_addresses",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        18
    );
    assert_eq!(target(&conn).unwrap(), 2);
    assert_eq!(
        conn.query_row("SELECT message_id FROM seen_address_messages", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        message
    );
    assert_eq!(fixtures::archive_account(&conn), account);
    for table in ["d_repair_assemblies", "d_repair_pieces"] {
        let flags: (i64, i64) = conn
            .query_row(
                "SELECT strict,wr FROM pragma_table_list WHERE name=?1",
                [table],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(flags, (1, 1));
    }
    assert!(
        conn.execute("INSERT INTO d_repair_pieces VALUES(99,2,0,0,'orphan')", [])
            .is_err()
    );
    assert!(matches!(
        run_migrations(&mut conn, &test_seams(real_fs()), &MIGRATIONS[..7]),
        Err(StoreError::NewerStore { .. })
    ));
}

#[test]
fn preparation_bounds_and_storage_codec_reject_invalid_text() {
    let mut conn = fixtures::index_connection();
    fixtures::acquisitions(&conn, 1);
    let summary = ImportSummary {
        parser_version: 2,
        parse_state: ImportParseState::Parsed,
        subject: Some(String::new()),
        from_name: None,
        ..ImportSummary::default()
    };
    let tx = conn.transaction().unwrap();
    let oversized = ImportSummary {
        subject: Some("x".repeat(MAX_TEXT_BYTES)),
        snippet: Some("x".to_owned()),
        ..summary.clone()
    };
    assert!(matches!(
        prepare(begin(&tx, 1, 2).unwrap().unwrap(), oversized),
        Err(StoreError::Corrupt { .. })
    ));
    assert_eq!(status(&tx, 1).unwrap().pieces, 0);
    assert!(assembly(&tx, 1, 2).unwrap().is_none());

    let plan = prepare(begin(&tx, 1, 2).unwrap().unwrap(), summary.clone()).unwrap();
    assert!(matches!(
        publish(&tx, plan, Timestamp::from_millis(0)).unwrap(),
        RepairCommit::Published
    ));
    tx.commit().unwrap();
    assert_eq!(read_version(&conn, 1, 2).unwrap(), Some(summary));
    assert!(
        conn.execute("INSERT INTO d_repair_pieces VALUES(1,2,12,0,'x')", [])
            .is_err()
    );
    // Inject a damaged completed store only after proving the normal guard refuses it.
    assert!(
        conn.execute("INSERT INTO d_repair_pieces VALUES(1,2,1,0,'x')", [])
            .is_err()
    );
    conn.execute_batch("DROP TRIGGER d_repair_pieces_immutable_insert")
        .unwrap();
    conn.execute(
        "INSERT INTO d_repair_pieces VALUES(1,2,1,0,CAST(x'80' AS TEXT))",
        [],
    )
    .unwrap();
    assert!(matches!(
        read_version(&conn, 1, 2),
        Err(StoreError::Corrupt { .. })
    ));
}

#[test]
fn real_catalog_and_manifest_bounds_price_the_maximum_stage() {
    let conn = fixtures::index_connection();
    for table in ["d_repair_assemblies", "d_repair_pieces"] {
        let indexes: i64 = conn
            .query_row("SELECT count(*) FROM pragma_index_list(?1)", [table], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(indexes, 1, "payload must not acquire an extra index copy");
    }
    let manifest = serde_json::to_string(&[Some(usize::MAX); TEXT_FIELDS]).unwrap();
    assert!(manifest.len() <= 256);
    let charged = MAX_STAGE_PIECES * (MAX_PIECE_BYTES + PIECE_FRAME) + METADATA_FRAME;
    assert!(charged <= MAX_REPAIR_BYTES);
}

#[test]
fn diagnosis_migration_keeps_v8_versions_metadata_and_both_address_tables() {
    let mut conn = fixtures::index_connection_through(8);
    fixtures::acquisitions(&conn, 1);
    let account = fixtures::archive_account(&conn);
    let message = fixtures::message(&conn, &account, Some(1));
    conn.execute_batch("INSERT INTO d_message_summary(acquisition_id,parser_version,parse_state,subject,has_attachments) VALUES(1,1,'partial','base',0);
        INSERT INTO seen_addresses(canonical,email,last_seen_at,times_received,times_sent_to) VALUES('kept@x.test','kept@x.test',17,11,7);
        INSERT INTO d_repair_assemblies SELECT 1,2,content_sha256,byte_length,NULL,0,'[null,4,null,null,null,null,null,null,null,null,null,null]',99 FROM acquisitions;
        INSERT INTO d_repair_pieces VALUES(1,2,1,0,'kept')").unwrap();
    conn.execute(
        "INSERT INTO seen_address_messages VALUES(?1,'kept@x.test')",
        [message],
    )
    .unwrap();
    let before = read_version(&conn, 1, 2).unwrap();
    let metadata = |conn: &Connection| -> String {
        conn.query_row("SELECT json_group_array(json_array(key,value)) FROM (SELECT key,value FROM store_meta ORDER BY key)", [], |r| r.get(0)).unwrap()
    };
    let meta = metadata(&conn);
    run_migrations(&mut conn, &test_seams(real_fs()), MIGRATIONS).unwrap();
    assert_eq!(read_version(&conn, 1, 2).unwrap(), before);
    assert_eq!(metadata(&conn), meta);
    let kept: (String, i64, i64, i64, i64) = conn.query_row("SELECT s.subject,a.last_seen_at,a.times_received,a.times_sent_to,l.message_id FROM d_message_summary s CROSS JOIN seen_addresses a CROSS JOIN seen_address_messages l", [], |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))).unwrap();
    assert_eq!(kept, ("base".into(), 17, 11, 7, message));
    assert_eq!(
        conn.query_row(
            "SELECT strict,wr FROM pragma_table_list WHERE name='d_repair_diagnoses'",
            [],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
        )
        .unwrap(),
        (1, 1)
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM pragma_index_list('d_repair_diagnoses')",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        1
    );
    assert!(
        conn.execute(
            "INSERT INTO d_repair_diagnoses VALUES(99,2,printf('%064d',0),0,'failed',1)",
            []
        )
        .is_err()
    );
    assert!(
        conn.execute(
            "INSERT INTO d_repair_diagnoses VALUES(1,2,printf('%064d',1),1,'unknown',1)",
            []
        )
        .is_err()
    );
    assert!(DiagnosisKind::decode("unknown").is_err());
    assert!(matches!(
        run_migrations(&mut conn, &test_seams(real_fs()), &MIGRATIONS[..8]),
        Err(StoreError::NewerStore { .. })
    ));
}

#[test]
fn completion_supersedes_attempts_without_bypassing_immutable_content_validation() {
    let mut conn = fixtures::index_connection();
    fixtures::acquisitions(&conn, 1);
    let tx = conn.transaction().unwrap();
    let diagnosed = begin(&tx, 1, 2).unwrap().unwrap();
    let attempt = begin(&tx, 1, 2).unwrap().unwrap();
    let eligible = begin(&tx, 1, 2).unwrap().unwrap();
    let summary = ImportSummary {
        parser_version: 2,
        parse_state: ImportParseState::Parsed,
        subject: Some("valid".into()),
        ..ImportSummary::default()
    };
    let prepared = prepare(begin(&tx, 1, 2).unwrap().unwrap(), summary.clone()).unwrap();
    let replay = prepare(begin(&tx, 1, 2).unwrap().unwrap(), summary).unwrap();
    assert_eq!(
        persist(
            &tx,
            diagnosed,
            DiagnosisKind::Failed,
            Timestamp::from_millis(0)
        )
        .unwrap(),
        RepairPersist::Diagnosed
    );
    assert!(matches!(
        publish(&tx, prepared, Timestamp::from_millis(1)).unwrap(),
        RepairCommit::Published
    ));
    assert_eq!(prepare_gate(&tx, &eligible).unwrap(), RepairGate::Current);
    assert_eq!(gate(&tx, &eligible).unwrap(), RepairGate::Open);
    assert_eq!(
        persist(
            &tx,
            attempt,
            DiagnosisKind::Failed,
            Timestamp::from_millis(2)
        )
        .unwrap(),
        RepairPersist::Current
    );
    assert_eq!(status(&tx, 1).unwrap().diagnosis.unwrap().diagnosed_at, 0);
    assert_eq!(
        tx.query_row("SELECT count(*) FROM d_repair_diagnoses", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert!(
        tx.execute("UPDATE d_repair_pieces SET text='other'", [])
            .is_err()
    );
    tx.execute_batch("DROP TRIGGER d_repair_pieces_immutable_update")
        .unwrap();
    tx.execute("UPDATE d_repair_pieces SET text='other'", [])
        .unwrap();
    assert!(matches!(
        publish(&tx, replay, Timestamp::from_millis(3)),
        Err(StoreError::Corrupt { .. })
    ));
}

#[test]
fn diagnosis_is_one_bounded_immutable_row_and_new_target_is_independent() {
    let mut conn = fixtures::index_connection();
    fixtures::acquisitions(&conn, 1);
    let tx = conn.transaction().unwrap();
    let first = begin(&tx, 1, 2).unwrap().unwrap();
    let retry = begin(&tx, 1, 2).unwrap().unwrap();
    assert_eq!(
        persist(&tx, first, DiagnosisKind::Failed, Timestamp::from_millis(1)).unwrap(),
        RepairPersist::Diagnosed
    );
    let changes = tx.total_changes();
    assert_eq!(
        persist(
            &tx,
            retry,
            DiagnosisKind::Failed,
            Timestamp::from_millis(999)
        )
        .unwrap(),
        RepairPersist::Current
    );
    assert_eq!(tx.total_changes(), changes);
    assert_eq!(status(&tx, 1).unwrap().diagnosis.unwrap().diagnosed_at, 1);
    assert!(
        tx.execute("DELETE FROM acquisitions WHERE acquisition_id=1", [])
            .is_err()
    );
    let charged: i64 = tx.query_row("SELECT length(CAST(source_sha256 AS BLOB))+length(CAST(kind AS BLOB))+48 FROM d_repair_diagnoses", [], |r| r.get(0)).unwrap();
    assert!(charged + 512 < MAX_REPAIR_BYTES as i64);
    assert_eq!(status(&tx, 1).unwrap().pieces, 0);
    let future = begin(&tx, 1, 3).unwrap().unwrap();
    assert_eq!(target(&tx).unwrap(), 3);
    assert_eq!(status(&tx, 1).unwrap().diagnosis, None);
    assert_eq!(
        persist(
            &tx,
            future,
            DiagnosisKind::Partial,
            Timestamp::from_millis(2)
        )
        .unwrap(),
        RepairPersist::Diagnosed
    );
    assert_eq!(
        tx.query_row("SELECT count(*) FROM d_repair_diagnoses", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        2
    );
}
