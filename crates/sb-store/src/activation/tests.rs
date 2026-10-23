use super::*;
use crate::schema::{MIGRATIONS, run_migrations};
use crate::testing::{fixtures, test_seams};
use sb_test_support::real_fs;

#[test]
fn witness_codec_rejects_every_framing_order_and_capacity_class() {
    let ids = BTreeSet::from([1, 7, i64::MAX]);
    let encoded = encode(&ids, i64::MAX).unwrap();
    assert_eq!(
        decode(&encoded).unwrap(),
        (ids.into_iter().collect(), i64::MAX)
    );
    assert!(encode(&BTreeSet::new(), 1).is_err());
    let valid = encode(&BTreeSet::from([1, 2]), 2).unwrap();
    let mut cases = vec![
        vec![],
        vec![0; WITNESS_HEADER_BYTES - 1],
        vec![0; MAX_WITNESS_BYTES + 1],
        valid[..valid.len() - 1].to_vec(),
    ];
    let mut trailing = valid.clone();
    trailing.push(0);
    cases.push(trailing);
    for (offset, word) in [(0, 0u32), (0, CODEC_VERSION + 1), (4, 0), (4, u32::MAX)] {
        let mut bytes = valid.clone();
        bytes[offset..offset + 4].copy_from_slice(&word.to_le_bytes());
        cases.push(bytes);
    }
    for (offset, word) in [
        (8, 0i64),
        (8, -1),
        (8, 1),
        (16, 0),
        (16, -1),
        (16, 2),
        (16, 3),
        (24, 1),
        (24, 3),
    ] {
        let mut bytes = valid.clone();
        bytes[offset..offset + 8].copy_from_slice(&word.to_le_bytes());
        cases.push(bytes);
    }
    for bytes in cases {
        assert!(
            matches!(decode(&bytes), Err(StoreError::Corrupt { .. })),
            "{bytes:?}"
        );
    }
    let ids: BTreeSet<_> = (1..=MAX_IDS as i64).collect();
    assert!(encode(&ids, MAX_IDS as i64).unwrap().len() <= MAX_WITNESS_BYTES);
    let mut too_many = ids;
    too_many.insert(MAX_IDS as i64 + 1);
    assert!(encode(&too_many, MAX_IDS as i64 + 1).is_err());
    const {
        assert!(
            MAX_IDS * (list_rows::ROW_FRAME + list_rows::INTEGER_INDEX_FRAME) + METADATA_FRAME
                > MAX_BYTES
        );
    }
}

#[test]
fn migration_keeps_v10_state_and_rejects_older_writer_without_rewriting_inputs() {
    let mut conn = fixtures::index_connection_through(10);
    fixtures::acquisitions(&conn, 1);
    let account = fixtures::archive_account(&conn);
    fixtures::message(&conn, &account, Some(1));
    conn.execute_batch("INSERT INTO seen_addresses(canonical,email,last_seen_at,times_received,times_sent_to) VALUES('kept@x.test','kept@x.test',1,11,7)").unwrap();
    let snapshot = |conn: &Connection| -> String {
        conn.query_row("SELECT json_array((SELECT json_group_array(json_array(key,value)) FROM store_meta WHERE key NOT IN('last_migrated_by','app_version_last_opened')),(SELECT json_group_array(json_array(message_id,account_id,acquisition_id)) FROM logical_messages),(SELECT json_group_array(json_array(canonical,email,times_received,times_sent_to)) FROM seen_addresses))", [], |row| row.get(0)).unwrap()
    };
    let before = snapshot(&conn);
    run_migrations(&mut conn, &test_seams(real_fs()), MIGRATIONS).unwrap();
    assert_eq!(snapshot(&conn), before);
    for table in ["d_message_bindings", "d_activation_deferrals"] {
        assert_eq!(
            conn.query_row(
                "SELECT strict FROM pragma_table_list WHERE name=?1",
                [table],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }
    assert!(matches!(
        run_migrations(&mut conn, &test_seams(real_fs()), &MIGRATIONS[..10]),
        Err(StoreError::NewerStore { .. })
    ));
    assert_eq!(snapshot(&conn), before);
    assert!(
        conn.execute("INSERT INTO d_message_bindings VALUES(1,1,2,0)", [])
            .is_err()
    );
    assert!(
        conn.execute(
            "INSERT INTO d_activation_deferrals VALUES(99,1,2,NULL,NULL,x'00',2000001,0)",
            []
        )
        .is_err()
    );
    let index: Vec<(Option<String>, i64)> = conn
        .prepare("SELECT name,key FROM pragma_index_xinfo('d_list_rows_by_message') ORDER BY seqno")
        .unwrap()
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(
        index,
        [
            (Some("message_id".into()), 1),
            (Some("occurrence_id".into()), 1),
            (None, 0)
        ]
    );
    // Two signed varint keys, rowid and headers in old/new index images fit this reserve.
    const MAX_VARINT_BYTES: usize = 9;
    const INDEX_RECORD_FRAME: usize = 16;
    assert!(
        2 * (index.len() * MAX_VARINT_BYTES + INDEX_RECORD_FRAME) <= list_rows::INTEGER_INDEX_FRAME
    );
    assert_eq!(
        conn.query_row("SELECT count(*) FROM pragma_foreign_key_check", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap(),
        0
    );
}

#[test]
fn bindings_require_completed_exact_source_and_cannot_clear_rekey_or_downgrade() {
    let conn = fixtures::index_connection();
    fixtures::acquisitions(&conn, 2);
    let account = fixtures::archive_account(&conn);
    fixtures::message(&conn, &account, Some(1));
    fixtures::message(&conn, &account, Some(2));
    conn.execute_batch("INSERT INTO d_repair_assemblies SELECT acquisition_id,1,content_sha256,byte_length,NULL,0,'[null,null,null,null,null,null,null,null,null,null,null,null]',NULL FROM acquisitions;
        INSERT INTO d_repair_assemblies SELECT acquisition_id,2,content_sha256,byte_length,NULL,0,'[null,null,null,null,null,null,null,null,null,null,null,null]',NULL FROM acquisitions;
        UPDATE d_repair_assemblies SET completed_at=0 WHERE parser_target=2").unwrap();
    assert!(
        conn.execute("INSERT INTO d_message_bindings VALUES(1,1,1,0)", [])
            .is_err()
    );
    assert!(
        conn.execute("INSERT INTO d_message_bindings VALUES(1,2,2,0)", [])
            .is_err()
    );
    conn.execute("INSERT INTO d_message_bindings VALUES(1,1,2,0)", [])
        .unwrap();
    for sql in [
        "DELETE FROM d_message_bindings",
        "UPDATE d_message_bindings SET message_id=2",
        "UPDATE d_message_bindings SET acquisition_id=2",
        "UPDATE d_message_bindings SET parser_target=1",
        "UPDATE d_message_bindings SET bound_at=1",
        "INSERT OR REPLACE INTO d_message_bindings VALUES(1,1,1,0)",
        "INSERT OR REPLACE INTO d_message_bindings VALUES(1,2,2,0)",
        "UPDATE logical_messages SET acquisition_id=NULL WHERE message_id=1",
        "UPDATE logical_messages SET acquisition_id=2 WHERE message_id=1",
        "INSERT OR REPLACE INTO logical_messages SELECT message_id,account_id,provider_key,2,created_at FROM logical_messages WHERE message_id=1",
    ] {
        assert!(conn.execute(sql, []).is_err(), "{sql}");
    }
    conn.execute(
        "UPDATE d_message_bindings SET parser_target=parser_target,bound_at=bound_at",
        [],
    )
    .unwrap();
    assert_eq!(
        conn.query_row(
            "SELECT acquisition_id,parser_target,bound_at FROM d_message_bindings",
            [],
            |row| Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?
            ))
        )
        .unwrap(),
        (1, 2, 0)
    );
}

#[test]
fn effective_point_and_corpus_plans_seek_bindings_versions_and_pieces() {
    let conn = fixtures::index_connection();
    fixtures::acquisitions(&conn, 1);
    let account = fixtures::archive_account(&conn);
    fixtures::message(&conn, &account, Some(1));
    for source in [Source::Stored, Source::Candidate(2)] {
        for restriction in ["WHERE lm.message_id=?1", ""] {
            let sql = format!(
                "EXPLAIN QUERY PLAN SELECT {},{} FROM {} {} {restriction}",
                source.fields(["subject", "from_addr", "to_json"]).join(","),
                source.validity(),
                message_source::MESSAGES,
                source.joins()
            );
            let mut statement = conn.prepare(&sql).unwrap();
            let parameters: &[i64] = if restriction.is_empty() { &[] } else { &[1] };
            let plan: Vec<String> = statement
                .query_map(rusqlite::params_from_iter(parameters), |row| row.get(3))
                .unwrap()
                .collect::<Result<_, _>>()
                .unwrap();
            assert!(
                plan.iter().all(|line| !line.contains("MATERIALIZE")
                    && !line.starts_with("SCAN b")
                    && !line.starts_with("SCAN v")
                    && !line.starts_with("SCAN d_repair_pieces")),
                "{plan:?}"
            );
            if matches!(source, Source::Stored) {
                assert!(
                    plan.iter()
                        .any(|line| line.contains("SEARCH b USING INTEGER PRIMARY KEY")),
                    "{plan:?}"
                );
            }
            assert!(
                plan.iter()
                    .any(|line| line.contains("SEARCH v USING PRIMARY KEY")),
                "{plan:?}"
            );
            assert!(
                plan.iter()
                    .any(|line| line.contains("SEARCH d_repair_pieces USING PRIMARY KEY")),
                "{plan:?}"
            );
        }
    }
    let plan = |sql: &str| -> Vec<String> {
        conn.prepare(&format!("EXPLAIN QUERY PLAN {sql}"))
            .unwrap()
            .query_map([1], |row| row.get(3))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    };
    assert!(plan("SELECT occurrence_id FROM occurrences WHERE message_id=?1 AND occurrence_id>0 ORDER BY occurrence_id LIMIT 200").iter().any(|line| line.contains("USING COVERING INDEX occ_by_message")));
    assert!(plan("SELECT occurrence_id FROM d_list_rows WHERE message_id=?1 AND occurrence_id>0 ORDER BY occurrence_id LIMIT 200").iter().any(|line| line.contains("USING COVERING INDEX d_list_rows_by_message")));
    let first = plan(
        "SELECT first_seen_at FROM occurrences WHERE message_id=?1 ORDER BY first_seen_at LIMIT 1",
    );
    assert!(
        !first.iter().any(|line| line.contains("TEMP B-TREE")),
        "{first:?}"
    );
}
