use super::*;
use crate::schema::{MIGRATIONS, run_migrations};
use crate::testing::{fixtures, test_seams};
use sb_test_support::real_fs;

#[test]
fn migration_preserves_kept_history_identities_and_refuses_older_writers() {
    let mut connection = fixtures::index_connection_through(6);
    let account = fixtures::archive_account(&connection);
    fixtures::acquisitions(&connection, 1);
    let message = fixtures::message(&connection, &account, Some(1));
    connection.execute("INSERT INTO seen_addresses(canonical,email,display_name,last_seen_at,times_sent_to,times_received,last_account_id) VALUES('User@example.test','User@EXAMPLE.test','history',-7,9,11,?1)", [&account]).unwrap();
    let before: (String,String,i64,i64,i64,String) = connection.query_row(
        "SELECT email,display_name,last_seen_at,times_sent_to,times_received,last_account_id FROM seen_addresses", [],
        |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?))).unwrap();
    run_migrations(&mut connection, &test_seams(real_fs()), MIGRATIONS).unwrap();
    let after = connection.query_row(
        "SELECT email,display_name,last_seen_at,times_sent_to,times_received,last_account_id FROM seen_addresses", [],
        |row| Ok((row.get::<_,String>(0)?,row.get::<_,String>(1)?,row.get::<_,i64>(2)?,row.get::<_,i64>(3)?,row.get::<_,i64>(4)?,row.get::<_,String>(5)?))).unwrap();
    assert_eq!(after, before);
    assert_eq!(fixtures::archive_account(&connection), account);
    assert_eq!(
        connection
            .query_row("SELECT message_id FROM logical_messages", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        message
    );
    let tx = connection.transaction().unwrap();
    assert_eq!(
        revisions(&tx).unwrap(),
        SeenRevisions {
            target: 1,
            inputs: 1,
            swept: 0
        }
    );
    assert_eq!(
        tx.query_row("SELECT count(*) FROM seen_address_messages", [], |row| row
            .get::<_, i64>(
            0
        ))
        .unwrap(),
        0
    );
    assert!(
        tx.execute(
            "INSERT INTO seen_address_messages VALUES(999,'User@example.test')",
            []
        )
        .is_err()
    );
    assert!(
        tx.execute(
            "INSERT INTO seen_address_messages VALUES(?1,'missing@example.test')",
            [message]
        )
        .is_err()
    );
    tx.execute(
        "INSERT INTO seen_address_messages VALUES(?1,'User@example.test')",
        [message],
    )
    .unwrap();
    assert!(
        tx.execute(
            "INSERT INTO seen_address_messages VALUES(?1,'User@example.test')",
            [message]
        )
        .is_err()
    );
    tx.commit().unwrap();
    assert!(matches!(
        run_migrations(&mut connection, &test_seams(real_fs()), &MIGRATIONS[..6]),
        Err(StoreError::NewerStore { .. })
    ));
    assert_eq!(
        connection
            .query_row("SELECT times_sent_to FROM seen_addresses", [], |row| row
                .get::<_, i64>(0))
            .unwrap(),
        9
    );
}

#[test]
fn actual_ddl_copy_counts_and_unicode_bound_make_a_legal_key_fit() {
    let connection = fixtures::index_connection();
    let (strict, without_rowid): (i64, i64) = connection
        .query_row(
            "SELECT strict,wr FROM pragma_table_list WHERE name='seen_address_messages'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .unwrap();
    assert_eq!((strict, without_rowid), (1, 1));
    let canonical_index: i64 = connection
        .query_row(
            "SELECT count(*) FROM pragma_index_list('seen_addresses') WHERE origin='pk'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(canonical_index, 1);
    assert_eq!(CANONICAL_COPIES, 1 + canonical_index as usize + 1);
    // No original-domain DNS shortcut: verify the pinned Unicode lowercase byte-growth bound.
    for scalar in 0..=0x10ffff {
        let Some(character) = char::from_u32(scalar) else {
            continue;
        };
        let lower: usize = character.to_lowercase().map(char::len_utf8).sum();
        assert!(
            lower * 2 <= character.len_utf8() * 3,
            "Unicode lowercase expansion: {character:?}"
        );
    }
    // 3 canonical copies (<=1.5A each), email A, name <=3N; A+N<=headercap.
    // Max(5.5A+3N) <=5.5H, plus the UUID and one bounded framing allowance.
    let worst = crate::messages::MAX_READER_HEADER_BYTES * 11 / 2 + 36 + KEY_FRAMING;
    assert!(worst < MAX_RECEIVED_BYTES);
}
