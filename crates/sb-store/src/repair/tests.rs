use super::*;
use crate::schema::{MIGRATIONS, run_migrations};
use crate::testing::{fixtures, test_seams};
use sb_test_support::real_fs;

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
