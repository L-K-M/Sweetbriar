//! Shared setup for existing Store suites; occurrence mutations stay local
//! because their refresh and generation ownership differs.

use rusqlite::{Connection, params};
use sb_test_support::real_fs;

use super::{FIXED_NOW_MS, test_seams};
use crate::index::{IndexDocument, IndexState};
use crate::schema::{MIGRATIONS, Migration, run_migrations};

const INITIAL_EXTRACTOR_VERSION: u32 = 1;

/// List fixtures retain SQLite's default pragmas and the requested migrations.
pub(crate) fn migrated(migrations: &[Migration]) -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    run_migrations(&mut conn, &test_seams(real_fs()), migrations).unwrap();
    conn
}

/// Publication fixtures explicitly distrust schemas before migrating.
pub(crate) fn index_connection() -> Connection {
    index_connection_through(MIGRATIONS.len())
}

pub(crate) fn index_connection_through(count: usize) -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA trusted_schema=OFF;")
        .unwrap();
    run_migrations(&mut conn, &test_seams(real_fs()), &MIGRATIONS[..count]).unwrap();
    conn
}

pub(crate) fn acquisitions(conn: &Connection, count: usize) {
    for id in 1..=count {
        conn.execute(
            "INSERT INTO acquisitions (acquisition_id, content_sha256, byte_length,
             object_state, first_acquired_at) VALUES (?1, ?2, 1, 'published', ?1)",
            params![id as i64, format!("{id:064x}")],
        )
        .unwrap();
    }
}

/// The original publication fixture: v1 indexed subject, all other columns empty.
pub(crate) fn document(text: &str) -> IndexDocument {
    let mut columns = std::array::from_fn(|_| String::new());
    columns[0] = text.to_owned();
    IndexDocument {
        columns,
        extractor_version: INITIAL_EXTRACTOR_VERSION,
        state: IndexState::Indexed,
        truncated_fields: vec![],
    }
}

pub(crate) fn archive_account(conn: &Connection) -> String {
    conn.query_row(
        "SELECT account_id FROM accounts WHERE provider = 'local_archive'",
        [],
        |row| row.get(0),
    )
    .unwrap()
}

pub(crate) fn mailbox(
    conn: &Connection,
    account: &str,
    name: &str,
    kind: &str,
    role: Option<&str>,
) -> i64 {
    conn.execute(
        "INSERT INTO mailboxes (account_id, remote_name, role, kind, selectable, sync_state)
         VALUES (?1, ?2, ?3, ?4, 1, 'synced')",
        params![account, name, role, kind],
    )
    .unwrap();
    conn.last_insert_rowid()
}

pub(crate) fn message(conn: &Connection, account: &str, acquisition: Option<i64>) -> i64 {
    conn.execute(
        "INSERT INTO logical_messages (account_id, acquisition_id, created_at) VALUES (?1, ?2, ?3)",
        params![account, acquisition, FIXED_NOW_MS],
    )
    .unwrap();
    conn.last_insert_rowid()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pragma(conn: &Connection, name: &str) -> i64 {
        conn.pragma_query_value(None, name, |row| row.get(0))
            .unwrap()
    }

    #[test]
    fn connection_policies_keep_defaults_and_migration_depth() {
        let raw = Connection::open_in_memory().unwrap();
        let list = migrated(MIGRATIONS);
        for name in ["foreign_keys", "trusted_schema"] {
            assert_eq!(pragma(&list, name), pragma(&raw, name), "{name}");
        }
        let mut index = index_connection_through(2);
        assert_eq!(pragma(&index, "user_version"), 2);
        assert!(pragma(&index, "foreign_keys") != 0);
        assert_eq!(pragma(&index, "trusted_schema"), 0);
        let exists: bool = index
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM sqlite_schema WHERE name='search_index_state')",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert!(!exists);
        let account = archive_account(&index);
        run_migrations(&mut index, &test_seams(real_fs()), MIGRATIONS).unwrap();
        assert_eq!(archive_account(&index), account);
        assert_eq!(account, archive_account(&list));
        assert_eq!(pragma(&index, "user_version"), MIGRATIONS.len() as i64);
        assert_eq!(pragma(&index, "trusted_schema"), 0);
        assert_eq!(pragma(&index_connection(), "trusted_schema"), 0);
        let state_rows: i64 = index
            .query_row("SELECT count(*) FROM search_index_state", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(state_rows, 1);
    }

    #[test]
    fn acquisition_and_document_defaults_keep_exact_native_values() {
        let conn = index_connection();
        acquisitions(&conn, 2);
        let mut statement = conn
            .prepare(
                "SELECT acquisition_id,content_sha256,byte_length,object_state,first_acquired_at
                 FROM acquisitions ORDER BY acquisition_id",
            )
            .unwrap();
        let rows: Vec<(i64, String, i64, String, i64)> = statement
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            rows,
            vec![
                (1, format!("{:064x}", 1), 1, "published".into(), 1),
                (2, format!("{:064x}", 2), 1, "published".into(), 2),
            ]
        );
        let doc = document("café");
        assert_eq!(
            doc.extractor_version,
            crate::index::target_version(&conn).unwrap()
        );
        assert_eq!(doc.state, IndexState::Indexed);
        assert!(doc.truncated_fields.is_empty());
        assert_eq!(doc.columns[0], "café");
        assert!(doc.columns[1..].iter().all(String::is_empty));
    }

    #[test]
    fn mailbox_and_message_mechanics_preserve_fields_without_projection() {
        let conn = migrated(MIGRATIONS);
        let account = archive_account(&conn);
        acquisitions(&conn, 1);
        let mailbox = mailbox(&conn, &account, "Work's label", "label", Some("flagged"));
        let one = message(&conn, &account, None);
        let two = message(&conn, &account, Some(1));
        let stored: (String, String, String, String, bool, String) = conn
            .query_row(
                "SELECT account_id,remote_name,kind,role,selectable,sync_state
                 FROM mailboxes WHERE mailbox_id=?1",
                [mailbox],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(
            stored,
            (
                account.clone(),
                "Work's label".into(),
                "label".into(),
                "flagged".into(),
                true,
                "synced".into()
            )
        );
        for (id, acquisition) in [(one, None), (two, Some(1))] {
            let stored: (String, Option<i64>, i64) = conn
                .query_row(
                    "SELECT account_id,acquisition_id,created_at FROM logical_messages WHERE message_id=?1",
                    [id],
                    |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
                )
                .unwrap();
            assert_eq!(stored, (account.clone(), acquisition, FIXED_NOW_MS));
        }
        assert_eq!(
            conn.query_row("SELECT count(*) FROM d_list_rows", [], |row| row
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}
