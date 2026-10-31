use super::*;
use crate::testing::fixtures::{archive_account, index_connection, mailbox};

#[test]
fn malformed_or_missing_mailbox_parents_are_corrupt() {
    for value in ["'missing'", "CAST(x'ff' AS TEXT)"] {
        let mut conn = index_connection();
        let account = archive_account(&conn);
        let id = mailbox(&conn, &account, "Synthetic", "folder", None);
        // Represent external damage, not a valid application write.
        conn.pragma_update(None, "foreign_keys", false).unwrap();
        conn.execute(
            &format!("UPDATE mailboxes SET account_id={value} WHERE mailbox_id=?1"),
            [id],
        )
        .unwrap();
        let tx = conn.transaction().unwrap();
        let result = scoped_ids(
            &tx,
            &ExportScope::Mailbox {
                mailbox_id: id,
                expected_generation: list_generation(&tx).unwrap(),
            },
        );
        assert!(
            matches!(result, Err(StoreError::Corrupt { .. })),
            "{value}: {result:?}"
        );
    }
}

#[test]
fn scope_sql_errors_preserve_environmental_and_programming_failures() {
    let full = sql_error(rusqlite::Error::SqliteFailure(
        rusqlite::ffi::Error::new(rusqlite::ffi::SQLITE_FULL),
        None,
    ));
    assert!(matches!(full, StoreError::Unavailable { .. }));
    assert_eq!(full.io_kind(), Some(io::ErrorKind::StorageFull));
    assert!(matches!(
        sql_error(rusqlite::Error::InvalidColumnIndex(0)),
        StoreError::Internal { .. }
    ));
}
