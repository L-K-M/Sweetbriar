use super::*;
use crate::schema::{MIGRATIONS, run_migrations};
use crate::testing::{fixtures, test_seams};
use sb_test_support::real_fs;

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
