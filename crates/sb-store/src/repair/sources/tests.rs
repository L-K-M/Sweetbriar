use super::*;
use crate::repair::begin;
use crate::testing::{fixtures, reading, scratch_paths, test_seams, writing};
use sb_test_support::real_fs;

const BUILD: u32 = 2;
const PAGE: usize = 200;

#[test]
fn source_ranges_and_global_state_are_checked_even_for_empty_pages() {
    let mut conn = fixtures::index_connection();
    let tx = conn.transaction().unwrap();
    for (after, cutoff) in [(-1, None), (0, Some(-1)), (2, Some(1)), (1, None)] {
        assert!(matches!(
            sources(&tx, after, cutoff, BUILD),
            Err(StoreError::InvalidInput { .. })
        ));
    }
    for cutoff in [None, Some(0), Some(900)] {
        let page = sources(&tx, 0, cutoff, BUILD).unwrap();
        assert!(page.acquisition_ids.is_empty());
        assert_eq!(page.through, 0);
        assert_eq!(page.cutoff, cutoff.unwrap_or(0));
    }
    tx.execute(
        "UPDATE store_meta SET value='3' WHERE key='mime_repair_target'",
        [],
    )
    .unwrap();
    assert!(matches!(
        sources(&tx, 0, Some(0), BUILD),
        Err(StoreError::Unavailable { .. })
    ));
    tx.execute(
        "UPDATE store_meta SET value='bad' WHERE key='mime_repair_target'",
        [],
    )
    .unwrap();
    assert!(matches!(
        sources(&tx, 0, None, BUILD),
        Err(StoreError::Corrupt { .. })
    ));
    tx.execute(
        "UPDATE store_meta SET value='2' WHERE key='mime_repair_target'",
        [],
    )
    .unwrap();
    for value in ["bad", "-1", "9223372036854775808"] {
        tx.execute(
            "UPDATE store_meta SET value=?1 WHERE key='repair_inputs'",
            [value],
        )
        .unwrap();
        assert!(matches!(
            sources(&tx, 0, None, BUILD),
            Err(StoreError::Corrupt { .. })
        ));
    }
    tx.execute_batch("UPDATE store_meta SET value=CAST(x'ff' AS TEXT) WHERE key='repair_inputs'")
        .unwrap();
    assert!(matches!(
        sources(&tx, 0, None, BUILD),
        Err(StoreError::Corrupt { .. })
    ));
    tx.execute("DELETE FROM store_meta WHERE key='repair_inputs'", [])
        .unwrap();
    assert!(matches!(
        sources(&tx, 0, None, BUILD),
        Err(StoreError::Corrupt { .. })
    ));
    drop(tx);
    let mut old = fixtures::index_connection_through(11);
    assert!(matches!(
        sources(&old.transaction().unwrap(), 0, None, BUILD),
        Err(StoreError::Unavailable { .. })
    ));
}

#[test]
fn raw_pages_progress_before_referenced_and_current_filtering_without_writes() {
    let mut conn = fixtures::index_connection();
    fixtures::acquisitions(&conn, PAGE * 2 + 1);
    let account = fixtures::archive_account(&conn);
    for acquisition in [201, 201, 401] {
        fixtures::message(&conn, &account, Some(acquisition));
    }
    conn.execute_batch("INSERT INTO d_message_summary(acquisition_id,parser_version,parse_state,has_attachments) VALUES(201,2,'ok',0)").unwrap();
    let changes = conn.total_changes();
    let tx = conn.transaction().unwrap();
    let first = sources(&tx, 0, None, BUILD).unwrap();
    assert_eq!((first.through, first.cutoff), (200, 401));
    assert!(first.acquisition_ids.is_empty());
    let second = sources(&tx, first.through, Some(first.cutoff), BUILD).unwrap();
    assert_eq!(second.through, 400);
    assert!(second.acquisition_ids.is_empty());
    let third = sources(&tx, second.through, Some(first.cutoff), BUILD).unwrap();
    assert_eq!(third.acquisition_ids, [401]);
    assert_eq!(third.through, 401);
    assert_eq!(third.inputs, first.inputs);
    assert!(
        sources(&tx, third.through, Some(third.cutoff), BUILD)
            .unwrap()
            .acquisition_ids
            .is_empty()
    );
    tx.commit().unwrap();
    assert_eq!(conn.total_changes(), changes);
}

#[test]
fn candidates_keep_native_decode_and_completed_diagnosis_validation_at_begin() {
    let mut conn = fixtures::index_connection();
    fixtures::acquisitions(&conn, 8);
    let account = fixtures::archive_account(&conn);
    for id in 1..=8 {
        fixtures::message(&conn, &account, Some(id));
    }
    conn.execute_batch("INSERT INTO d_message_summary(acquisition_id,parser_version,parse_state,has_attachments) VALUES
        (1,2,'ok',0),(2,3,'partial',0),(3,2,'partial',0),(4,1,'ok',0),
        (6,9223372036854775807,'ok',0),(7,1,'partial',0),(8,1,'partial',0);
        INSERT INTO d_repair_diagnoses SELECT 7,2,printf('%064x',0),1,'partial',0;
        INSERT INTO d_repair_assemblies SELECT 8,2,content_sha256,byte_length,NULL,0,'[null,null,null,null,null,null,null,null,null,null,null,null]',NULL FROM acquisitions WHERE acquisition_id=8;
        UPDATE d_repair_assemblies SET completed_at=0 WHERE acquisition_id=8;").unwrap();
    let tx = conn.transaction().unwrap();
    let page = sources(&tx, 0, None, BUILD).unwrap();
    assert_eq!(page.acquisition_ids, [3, 4, 5, 6, 7, 8]);
    assert!(begin(&tx, 6, BUILD).is_err());
    assert!(matches!(
        begin(&tx, 7, BUILD),
        Err(StoreError::Corrupt { .. })
    ));
    assert!(begin(&tx, 8, BUILD).unwrap().is_none());
    for id in [1, 2] {
        assert!(begin(&tx, id, BUILD).unwrap().is_none());
    }
}

#[test]
fn all_referenced_candidates_still_obey_the_raw_id_limit() {
    let mut conn = fixtures::index_connection();
    fixtures::acquisitions(&conn, PAGE * 2 + 1);
    let account = fixtures::archive_account(&conn);
    for id in 1..=PAGE * 2 + 1 {
        fixtures::message(&conn, &account, Some(id as i64));
    }
    let tx = conn.transaction().unwrap();
    let mut after = 0;
    let mut ids = Vec::new();
    loop {
        let page = sources(&tx, after, Some(401), BUILD).unwrap();
        assert!(page.acquisition_ids.len() <= PAGE);
        if page.through == after {
            break;
        }
        after = page.through;
        ids.extend(page.acquisition_ids);
    }
    assert_eq!(ids, (1..=401).collect::<Vec<_>>());
}

#[test]
fn page_revision_cutoff_and_eligibility_share_a_wal_snapshot_then_revisit_old_ids() {
    let (_scratch, paths) = scratch_paths("repair-source-snapshot");
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    writing(&store, |tx| {
        fixtures::acquisitions(tx, 2);
        Ok(())
    });
    let first = reading(&store, |tx| sources(tx, 0, None, BUILD));
    assert!(first.acquisition_ids.is_empty());
    reading(&store, |tx| {
        let held = sources(tx, 0, None, BUILD)?;
        writing(&store, |writer| {
            let account = fixtures::archive_account(writer);
            fixtures::message(writer, &account, Some(1));
            writer
                .execute(
                    "INSERT INTO acquisitions VALUES(3,printf('%064x',3),1,'published',0)",
                    [],
                )
                .unwrap();
            fixtures::message(writer, &account, Some(3));
            Ok(())
        });
        let still_held = sources(tx, 0, None, BUILD)?;
        assert_eq!(
            (still_held.inputs, still_held.cutoff),
            (held.inputs, held.cutoff)
        );
        assert!(still_held.acquisition_ids.is_empty());
        Ok(())
    });
    let old_range = reading(&store, |tx| {
        sources(tx, first.through, Some(first.cutoff), BUILD)
    });
    assert!(old_range.acquisition_ids.is_empty());
    assert!(old_range.inputs > first.inputs);
    let fresh = reading(&store, |tx| sources(tx, 0, None, BUILD));
    assert_eq!(fresh.acquisition_ids, [1, 3]);
    assert_eq!(fresh.cutoff, 3);
    store.close().unwrap();
}

#[test]
fn discovery_uses_pk_range_and_referenced_source_point_seeks() {
    let conn = fixtures::index_connection();
    let plan = conn
        .prepare(&format!("EXPLAIN QUERY PLAN {SOURCE_SQL}"))
        .unwrap()
        .query_map(params![0, 99, PAGE as i64], |row| row.get::<_, String>(3))
        .unwrap()
        .collect::<rusqlite::Result<Vec<_>>>()
        .unwrap()
        .join("\n");
    assert!(
        plan.contains("SEARCH a USING INTEGER PRIMARY KEY"),
        "{plan}"
    );
    assert!(plan.contains("logical_messages_by_acquisition"), "{plan}");
    assert!(!plan.contains("SCAN m"), "{plan}");
}
