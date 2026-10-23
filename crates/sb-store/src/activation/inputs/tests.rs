use super::*;
use crate::testing::fixtures;

fn fixture() -> (Connection, Owner) {
    let conn = fixtures::index_connection();
    fixtures::acquisitions(&conn, 2);
    let account = fixtures::archive_account(&conn);
    fixtures::message(&conn, &account, Some(1));
    fixtures::message(&conn, &account, Some(2));
    fixtures::mailbox(&conn, &account, "box", "folder", None);
    conn.execute_batch("INSERT INTO occurrences(occurrence_id,account_id,mailbox_id,message_id,state,first_seen_at,last_seen_at) SELECT 1,account_id,1,1,'present',0,0 FROM logical_messages WHERE message_id=1;
        INSERT INTO message_headers(message_id,raw_fields,fetched_at) VALUES(1,x'',0);
        INSERT INTO d_message_summary(acquisition_id,parser_version,parse_state,has_attachments) VALUES(1,1,'partial',0);
        INSERT INTO d_list_rows VALUES(1,1,NULL,(SELECT account_id FROM logical_messages WHERE message_id=1),0,1,NULL,NULL,NULL,1,0,0,1);
        INSERT INTO d_repair_assemblies SELECT 1,1,content_sha256,byte_length,NULL,0,'[null,3,null,null,null,null,null,null,null,null,null,null]',NULL FROM acquisitions WHERE acquisition_id=1;
        INSERT INTO d_repair_pieces VALUES(1,1,1,0,'old')").unwrap();
    let before = conn.total_changes();
    let pragmas = |conn: &Connection| {
        ["trusted_schema", "recursive_triggers", "foreign_keys"].map(|name| {
            conn.pragma_query_value(None, name, |row| row.get::<_, i64>(0))
                .unwrap()
        })
    };
    let settings = pragmas(&conn);
    let owner = Owner::install(&conn).unwrap();
    assert_eq!(conn.total_changes(), before);
    assert_eq!(pragmas(&conn), settings);
    (conn, owner)
}

#[test]
fn every_input_table_updates_scoped_stamps_without_persistent_notification_dml() {
    let (conn, owner) = fixture();
    for (table, column) in [
        ("occurrences", "message_id"),
        ("logical_messages", "message_id"),
        ("message_headers", "message_id"),
        ("d_list_rows", "message_id"),
        ("acquisitions", "acquisition_id"),
        ("d_message_summary", "acquisition_id"),
        ("d_repair_assemblies", "acquisition_id"),
        ("d_repair_pieces", "acquisition_id"),
    ] {
        let lease = Lease::new(owner.handle(), 1, 1).unwrap();
        let before = conn.total_changes();
        conn.execute(
            &format!("UPDATE {table} SET {column}={column} WHERE {column}=1"),
            [],
        )
        .unwrap();
        assert!(!lease.matches(&owner.handle()).unwrap(), "{table}");
        assert_eq!(
            conn.total_changes() - before,
            1,
            "observer added DML: {table}"
        );
        assert_eq!(owner.0.slots.lock().unwrap().len(), 4);
    }
    let lease = Lease::new(owner.handle(), 1, 1).unwrap();
    conn.execute_batch("UPDATE store_meta SET value=value WHERE key='list_generation';
        UPDATE mailboxes SET remote_name='renamed'; INSERT INTO seen_addresses(canonical,email,last_seen_at) VALUES('a@x.test','a@x.test',0)").unwrap();
    assert!(lease.matches(&owner.handle()).unwrap());
    conn.execute("UPDATE mailboxes SET kind='label'", [])
        .unwrap();
    assert!(!lease.matches(&owner.handle()).unwrap());
    drop(lease);
    assert!(owner.0.slots.lock().unwrap().is_empty());
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM sqlite_schema WHERE name LIKE 'sb_activation_%'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM sqlite_temp_schema WHERE name LIKE 'sb_activation_%'",
            [],
            |row| row.get::<_, i64>(0)
        )
        .unwrap(),
        31
    );
}

#[test]
fn insert_delete_rekey_old_new_and_rollback_invalidate_only_relevant_scopes() {
    let (conn, owner) = fixture();
    let first = Lease::new(owner.handle(), 1, 1).unwrap();
    let second = Lease::new(owner.handle(), 2, 2).unwrap();
    conn.execute(
        "UPDATE occurrences SET message_id=2 WHERE occurrence_id=1",
        [],
    )
    .unwrap();
    assert!(!first.matches(&owner.handle()).unwrap());
    assert!(!second.matches(&owner.handle()).unwrap());
    drop(first);
    drop(second);
    for sql in [
        "INSERT INTO message_headers(message_id,raw_fields,fetched_at) VALUES(2,x'',0)",
        "DELETE FROM message_headers WHERE message_id=2",
        "INSERT INTO d_message_summary(acquisition_id,parser_version,parse_state,has_attachments) VALUES(2,1,'partial',0)",
        "DELETE FROM d_message_summary WHERE acquisition_id=2",
        "INSERT INTO d_repair_assemblies SELECT 2,1,content_sha256,byte_length,NULL,0,'[]',NULL FROM acquisitions WHERE acquisition_id=2",
        "INSERT INTO d_repair_pieces VALUES(2,1,1,0,'new')",
        "DELETE FROM d_repair_pieces WHERE acquisition_id=2",
        "DELETE FROM d_repair_assemblies WHERE acquisition_id=2",
    ] {
        let first = Lease::new(owner.handle(), 1, 1).unwrap();
        let second = Lease::new(owner.handle(), 2, 2).unwrap();
        let before = conn.total_changes();
        conn.execute(sql, []).unwrap();
        assert_eq!(conn.total_changes() - before, 1);
        assert!(first.matches(&owner.handle()).unwrap(), "{sql}");
        assert!(!second.matches(&owner.handle()).unwrap(), "{sql}");
    }
    let lease = Lease::new(owner.handle(), 1, 1).unwrap();
    conn.execute_batch(
        "BEGIN; UPDATE acquisitions SET object_state='missing' WHERE acquisition_id=1; ROLLBACK;",
    )
    .unwrap();
    assert!(
        !lease.matches(&owner.handle()).unwrap(),
        "rollback conservatively invalidates its touched inputs"
    );
    assert_eq!(
        conn.query_row(
            "SELECT object_state FROM acquisitions WHERE acquisition_id=1",
            [],
            |row| row.get::<_, String>(0)
        )
        .unwrap(),
        "published"
    );
}

#[test]
fn honest_callback_obeys_trust_and_latches_overflow_or_poison_without_rejecting_writes() {
    for poison in [false, true] {
        let (conn, owner) = fixture();
        let lease = Lease::new(owner.handle(), 1, 1).unwrap();
        conn.execute_batch("PRAGMA trusted_schema=OFF").unwrap();
        let flags: i64 = conn
            .query_row(
                "SELECT flags FROM pragma_function_list WHERE name='sb_activation_input'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(
            flags, 0,
            "UTF8 is the encoding, not an innocuous/deterministic claim"
        );
        if poison {
            let state = owner.handle();
            assert!(
                std::thread::spawn(move || {
                    let _guard = state.slots.lock().unwrap();
                    panic!("poison fixture");
                })
                .join()
                .is_err()
            );
        } else {
            owner
                .0
                .slots
                .lock()
                .unwrap()
                .get_mut(&(ACQUISITION, 1))
                .unwrap()
                .1 = u64::MAX;
        }
        conn.execute(
            "UPDATE acquisitions SET object_state='missing' WHERE acquisition_id=1",
            [],
        )
        .unwrap();
        assert!(matches!(
            lease.matches(&owner.handle()),
            Err(StoreError::Internal { .. })
        ));
        assert!(owner.0.failed.load(Ordering::SeqCst));
        assert_eq!(
            conn.query_row(
                "SELECT object_state FROM acquisitions WHERE acquisition_id=1",
                [],
                |row| row.get::<_, String>(0)
            )
            .unwrap(),
            "missing"
        );
    }
}

#[test]
fn one_acquisition_stamp_has_no_message_fanout_and_leases_release_without_tombstones() {
    let (conn, owner) = fixture();
    let lease = Lease::new(owner.handle(), 1, 1).unwrap();
    let duplicate = Lease::new(owner.handle(), 1, 1).unwrap();
    let before = owner.0.slots.lock().unwrap().clone();
    conn.execute(
        "UPDATE acquisitions SET object_state=object_state WHERE acquisition_id=1",
        [],
    )
    .unwrap();
    let after = owner.0.slots.lock().unwrap().clone();
    assert_eq!(after.len(), 4);
    for (key, (refs, stamp)) in before {
        assert_eq!(
            after[&key],
            (refs, stamp + u64::from(key == (ACQUISITION, 1)))
        );
    }
    drop(duplicate);
    assert_eq!(owner.0.slots.lock().unwrap().len(), 4);
    drop(owner);
    assert!(lease.state.dead.load(Ordering::SeqCst));
    let state = lease.state.clone();
    drop(lease);
    assert!(state.slots.lock().unwrap().is_empty());
}

#[test]
fn permitted_insert_update_delete_events_cover_each_registered_table() {
    let (conn, owner) = fixture();
    for sql in [
        "DELETE FROM logical_messages WHERE message_id=2",
        "DELETE FROM acquisitions WHERE acquisition_id=2",
        "INSERT INTO acquisitions VALUES(2,printf('%064x',2),0,'published',0)",
        "INSERT INTO logical_messages(message_id,account_id,acquisition_id,created_at) SELECT 2,account_id,2,0 FROM logical_messages WHERE message_id=1",
        "INSERT INTO occurrences(occurrence_id,account_id,mailbox_id,message_id,state,first_seen_at,last_seen_at) SELECT 2,account_id,1,2,'present',0,0 FROM logical_messages WHERE message_id=2",
        "INSERT INTO d_list_rows SELECT 2,1,NULL,account_id,0,2,NULL,NULL,NULL,1,0,0,1 FROM logical_messages WHERE message_id=2",
        "DELETE FROM d_list_rows WHERE occurrence_id=2",
        "DELETE FROM occurrences WHERE occurrence_id=2",
        "INSERT INTO message_headers(message_id,raw_fields,fetched_at) VALUES(2,x'',0)",
        "DELETE FROM message_headers WHERE message_id=2",
        "INSERT INTO d_message_summary(acquisition_id,parser_version,parse_state,has_attachments) VALUES(2,1,'partial',0)",
        "DELETE FROM d_message_summary WHERE acquisition_id=2",
        "INSERT INTO d_repair_assemblies SELECT 2,1,content_sha256,byte_length,NULL,0,'[null,3,null,null,null,null,null,null,null,null,null,null]',NULL FROM acquisitions WHERE acquisition_id=2",
        "INSERT INTO d_repair_pieces VALUES(2,1,1,0,'old')",
        "DELETE FROM d_repair_pieces WHERE acquisition_id=2",
        "DELETE FROM d_repair_assemblies WHERE acquisition_id=2",
        "INSERT INTO d_repair_assemblies SELECT 2,2,content_sha256,byte_length,NULL,0,'[null,null,null,null,null,null,null,null,null,null,null,null]',NULL FROM acquisitions WHERE acquisition_id=2",
        "UPDATE d_repair_assemblies SET completed_at=0 WHERE acquisition_id=2",
        "INSERT INTO d_message_bindings VALUES(2,2,2,0)",
        "UPDATE d_message_bindings SET bound_at=bound_at WHERE message_id=2",
    ] {
        let lease = Lease::new(owner.handle(), 2, 2).unwrap();
        let other = Lease::new(owner.handle(), 1, 1).unwrap();
        let before = conn.total_changes();
        conn.execute(sql, []).unwrap();
        assert_eq!(conn.total_changes() - before, 1, "{sql}");
        assert!(!lease.matches(&owner.handle()).unwrap(), "{sql}");
        assert!(other.matches(&owner.handle()).unwrap(), "{sql}");
    }
    let lease = Lease::new(owner.handle(), 2, 2).unwrap();
    assert!(
        conn.execute("DELETE FROM d_message_bindings WHERE message_id=2", [])
            .is_err()
    );
    assert!(
        lease.matches(&owner.handle()).unwrap(),
        "a forbidden delete never reaches AFTER triggers"
    );
}
