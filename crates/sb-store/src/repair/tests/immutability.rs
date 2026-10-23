use super::*;
use rusqlite::types::Value;

const LEGACY_MIGRATIONS: usize = 9;
const TABLES: &[&str] = &[
    "store_meta",
    "acquisitions",
    "logical_messages",
    "d_message_summary",
    "d_repair_assemblies",
    "d_repair_pieces",
    "d_repair_diagnoses",
    "seen_addresses",
    "seen_address_messages",
];

fn snapshot(conn: &Connection) -> Vec<Vec<Vec<Value>>> {
    TABLES
        .iter()
        .map(|table| {
            let mut statement = conn
                .prepare(&format!("SELECT * FROM {table} ORDER BY 1,2"))
                .unwrap();
            let width = statement.column_count();
            statement
                .query_map([], |row| {
                    (0..width)
                        .map(|i| row.get(i))
                        .collect::<Result<Vec<_>, _>>()
                })
                .unwrap()
                .collect::<Result<Vec<_>, _>>()
                .unwrap()
        })
        .collect()
}

fn legacy() -> Connection {
    let conn = fixtures::index_connection_through(LEGACY_MIGRATIONS);
    fixtures::acquisitions(&conn, 3);
    let account = fixtures::archive_account(&conn);
    let message = fixtures::message(&conn, &account, Some(1));
    conn.execute_batch("INSERT INTO d_repair_assemblies SELECT acquisition_id,2,content_sha256,byte_length,NULL,0,'[null,3,null,null,null,null,null,null,null,null,null,null]',NULL FROM acquisitions;
        INSERT INTO d_repair_pieces VALUES(1,2,1,0,'old'),(2,2,1,0,'old');
        UPDATE d_repair_assemblies SET completed_at=0 WHERE acquisition_id=1;
        UPDATE d_repair_assemblies SET completed_at=0,field_lengths='[null,null,null,null,null,null,null,null,null,null,null,null]' WHERE acquisition_id=3;
        INSERT INTO d_repair_diagnoses SELECT 2,2,content_sha256,byte_length,'partial',17 FROM acquisitions WHERE acquisition_id=2;
        INSERT INTO d_message_summary(acquisition_id,parser_version,parse_state,subject,has_attachments) VALUES(1,1,'partial','base',0);
        INSERT INTO seen_addresses(canonical,email,last_seen_at,times_received,times_sent_to) VALUES('kept@x.test','kept@x.test',17,11,7)").unwrap();
    conn.execute(
        "INSERT INTO seen_address_messages VALUES(?1,'kept@x.test')",
        [message],
    )
    .unwrap();
    conn
}

fn migrated() -> Connection {
    let mut conn = legacy();
    run_migrations(&mut conn, &test_seams(real_fs()), MIGRATIONS).unwrap();
    conn
}

fn refused(conn: &Connection, sql: &str) {
    let before = snapshot(conn);
    let changes = conn.total_changes();
    let error = conn.execute(sql, []).unwrap_err();
    assert_eq!(
        error.sqlite_error().unwrap().extended_code,
        rusqlite::ffi::SQLITE_CONSTRAINT_TRIGGER,
        "{sql}: {error}"
    );
    assert!(matches!(sql_error(error), StoreError::Internal { .. }));
    assert_eq!(conn.total_changes(), changes, "{sql}");
    assert_eq!(snapshot(conn), before, "{sql}");
}

#[test]
fn migration_preserves_v9_data_and_adds_only_guards() {
    let mut conn = legacy();
    let before = snapshot(&conn);
    let catalog = |conn: &Connection| -> Vec<(String, String, i64)> {
        conn.prepare("SELECT type,name,rootpage FROM sqlite_schema WHERE type IN('table','index') ORDER BY type,name").unwrap()
            .query_map([], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?))).unwrap().collect::<Result<_,_>>().unwrap()
    };
    let objects = catalog(&conn);
    let changes = conn.total_changes();
    run_migrations(&mut conn, &test_seams(real_fs()), MIGRATIONS).unwrap();
    // Only the runner's last-migrated/open-version metadata writes, no artifact rewrites.
    assert_eq!(conn.total_changes() - changes, 2);
    assert_eq!(snapshot(&conn), before);
    assert_eq!(catalog(&conn), objects);
    let triggers: i64 = conn.query_row("SELECT count(*) FROM sqlite_schema WHERE type='trigger' AND name GLOB 'd_repair_*_immutable_*'", [], |r| r.get(0)).unwrap();
    assert_eq!(triggers, 6);
    let changes = conn.total_changes();
    assert!(matches!(
        run_migrations(
            &mut conn,
            &test_seams(real_fs()),
            &MIGRATIONS[..LEGACY_MIGRATIONS]
        ),
        Err(StoreError::NewerStore { .. })
    ));
    assert_eq!(conn.total_changes(), changes);
    assert_eq!(snapshot(&conn), before);
    for table in [
        "d_repair_assemblies",
        "d_repair_pieces",
        "d_repair_diagnoses",
    ] {
        assert_eq!(
            conn.query_row(
                "SELECT strict,wr FROM pragma_table_list WHERE name=?1",
                [table],
                |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))
            )
            .unwrap(),
            (1, 1)
        );
    }
    assert!(
        conn.execute("INSERT INTO d_repair_pieces VALUES(99,2,1,0,'orphan')", [])
            .is_err()
    );
}

#[test]
fn every_completed_column_and_piece_owner_is_frozen() {
    let conn = migrated();
    for change in [
        "acquisition_id=2",
        "parser_target=1",
        "source_sha256=printf('%064d',7)",
        "source_bytes=2",
        "date_header=17",
        "has_attachments=1",
        "field_lengths='[]'",
        "completed_at=NULL",
        "completed_at=17",
    ] {
        refused(
            &conn,
            &format!("UPDATE d_repair_assemblies SET {change} WHERE acquisition_id=1"),
        );
    }
    for change in [
        "acquisition_id=2",
        "parser_target=1",
        "field_index=2",
        "byte_offset=1",
        "text='new'",
    ] {
        refused(
            &conn,
            &format!("UPDATE d_repair_pieces SET {change} WHERE acquisition_id=1"),
        );
    }
    for sql in [
        "DELETE FROM d_repair_assemblies WHERE acquisition_id=1",
        "DELETE FROM d_repair_assemblies WHERE acquisition_id=3",
        "DELETE FROM d_repair_pieces WHERE acquisition_id=1",
        "INSERT INTO d_repair_pieces VALUES(1,2,2,0,'extra')",
        "UPDATE d_repair_pieces SET acquisition_id=3 WHERE acquisition_id=2",
        "UPDATE d_repair_assemblies SET completed_at=1,has_attachments=1 WHERE acquisition_id=2",
        "INSERT INTO d_repair_assemblies SELECT 2,1,source_sha256,source_bytes,date_header,has_attachments,field_lengths,0 FROM d_repair_assemblies WHERE acquisition_id=2",
    ] {
        refused(&conn, sql);
    }
}

#[test]
fn replacement_and_upsert_cannot_hide_a_completed_destination() {
    let conn = migrated();
    for sql in [
        "INSERT OR REPLACE INTO d_repair_assemblies SELECT acquisition_id,parser_target,source_sha256,source_bytes,date_header,has_attachments,field_lengths,NULL FROM d_repair_assemblies WHERE acquisition_id=3",
        "UPDATE OR REPLACE d_repair_assemblies SET acquisition_id=3 WHERE acquisition_id=2",
        "INSERT INTO d_repair_assemblies SELECT acquisition_id,parser_target,source_sha256,source_bytes,date_header,has_attachments,field_lengths,NULL FROM d_repair_assemblies WHERE acquisition_id=3 ON CONFLICT(acquisition_id,parser_target) DO UPDATE SET completed_at=NULL",
        "INSERT OR REPLACE INTO d_repair_pieces VALUES(1,2,1,0,'new')",
        "UPDATE OR REPLACE d_repair_pieces SET acquisition_id=1 WHERE acquisition_id=2",
        "INSERT INTO d_repair_pieces VALUES(1,2,1,0,'new') ON CONFLICT(acquisition_id,parser_target,field_index,byte_offset) DO UPDATE SET text=excluded.text",
    ] {
        refused(&conn, sql);
    }
    assert_eq!(
        read_version(&conn, 1, 2)
            .unwrap()
            .unwrap()
            .subject
            .as_deref(),
        Some("old")
    );
    assert!(
        read_version(&conn, 3, 2)
            .unwrap()
            .unwrap()
            .subject
            .is_none()
    );
}

#[test]
fn incomplete_staging_marker_only_completion_and_value_noops_remain_legal() {
    let conn = migrated();
    let before = snapshot(&conn);
    let changes = conn.total_changes();
    conn.execute_batch("UPDATE d_repair_assemblies SET acquisition_id=acquisition_id,parser_target=parser_target,source_sha256=source_sha256,source_bytes=source_bytes,date_header=date_header,has_attachments=has_attachments,field_lengths=field_lengths,completed_at=completed_at WHERE acquisition_id=1;
        UPDATE d_repair_pieces SET acquisition_id=acquisition_id,parser_target=parser_target,field_index=field_index,byte_offset=byte_offset,text=text WHERE acquisition_id=1").unwrap();
    assert_eq!(snapshot(&conn), before);
    assert_eq!(
        conn.total_changes() - changes,
        2,
        "issued no-op updates are still DML"
    );
    conn.execute_batch("INSERT INTO d_repair_assemblies SELECT acquisition_id,1,source_sha256,source_bytes,date_header,has_attachments,field_lengths,NULL FROM d_repair_assemblies WHERE acquisition_id=2;
        UPDATE d_repair_pieces SET parser_target=1,field_index=2,byte_offset=1,text='new' WHERE acquisition_id=2;
        DELETE FROM d_repair_pieces WHERE acquisition_id=2;
        DELETE FROM d_repair_assemblies WHERE acquisition_id=2 AND parser_target=1;
        UPDATE d_repair_assemblies SET field_lengths='[null,0,null,null,null,null,null,null,null,null,null,null]' WHERE acquisition_id=2;
        UPDATE d_repair_assemblies SET completed_at=0 WHERE acquisition_id=2").unwrap();
    let summary = read_version(&conn, 2, 2).unwrap().unwrap();
    assert_eq!(summary.subject.as_deref(), Some(""));
    assert_eq!(summary.from_name, None);
    refused(
        &conn,
        "DELETE FROM d_repair_assemblies WHERE acquisition_id=2",
    );
}

#[test]
fn trigger_programs_seek_owner_keys_and_add_no_payload_copies() {
    let conn = migrated();
    for sql in [
        "INSERT INTO d_repair_assemblies VALUES(2,1,printf('%064d',2),1,NULL,0,'[]',NULL)",
        "UPDATE d_repair_assemblies SET acquisition_id=3 WHERE acquisition_id=2",
        "DELETE FROM d_repair_assemblies WHERE acquisition_id=3",
        "INSERT INTO d_repair_pieces VALUES(2,2,2,0,'new')",
        "UPDATE d_repair_pieces SET acquisition_id=3 WHERE acquisition_id=2",
        "DELETE FROM d_repair_pieces WHERE acquisition_id=1",
    ] {
        let mut statement = conn.prepare(&format!("EXPLAIN {sql}")).unwrap();
        let program: Vec<(i64, String)> = statement
            .query_map([], |r| Ok((r.get(0)?, r.get(1)?)))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        let start = program
            .iter()
            .rposition(|(address, _)| *address == 0)
            .unwrap();
        let trigger = &program[start..];
        assert!(
            !trigger.iter().any(|(_, op)| matches!(
                op.as_str(),
                "Rewind" | "Next" | "Prev" | "Insert" | "Delete" | "IdxInsert" | "IdxDelete"
            )),
            "{sql}: {trigger:?}"
        );
        if sql.contains("pieces") || sql.starts_with("INSERT") || sql.starts_with("UPDATE") {
            assert!(
                trigger.iter().any(|(_, op)| op == "SeekGE"),
                "{sql}: {trigger:?}"
            );
        }
    }
    // Exercise actual row changes under those programs, not only bytecode inspection.
    let changes = conn.total_changes();
    conn.execute("INSERT INTO d_repair_pieces VALUES(2,2,2,0,'new')", [])
        .unwrap();
    assert_eq!(conn.total_changes() - changes, 1);
    for table in ["d_repair_assemblies", "d_repair_pieces"] {
        let copies: i64 = conn
            .query_row("SELECT count(*) FROM pragma_index_list(?1)", [table], |r| {
                r.get(0)
            })
            .unwrap();
        assert_eq!(copies, 1);
        let payload_keys: i64 = conn.query_row("SELECT count(*) FROM pragma_index_list(?1) i JOIN pragma_index_xinfo(i.name) x WHERE x.key=1 AND x.name IN('text','field_lengths')", [table], |r| r.get(0)).unwrap();
        assert_eq!(payload_keys, 0);
    }
    let charged = MAX_STAGE_PIECES * (MAX_PIECE_BYTES + PIECE_FRAME) + METADATA_FRAME;
    assert_eq!(charged, 1_060_864);
    assert!(charged < MAX_REPAIR_BYTES);
}
