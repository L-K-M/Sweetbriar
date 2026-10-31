use sb_test_support::real_fs;

use super::*;
use crate::schema::{MIGRATIONS, run_migrations};
use crate::testing::fixtures::{archive_account, index_connection, index_connection_through};
use crate::testing::test_seams;

#[test]
fn migration_18_backfills_by_creation_and_id_without_rewriting_custom_records() {
    let prefix = MIGRATIONS
        .iter()
        .position(|migration| migration.version == 18)
        .unwrap();
    let mut conn = index_connection_through(prefix);
    let archive = archive_account(&conn);
    conn.execute(
        "INSERT INTO account_appearance (account_id, color_srgb, symbol, assignment, revision)
         VALUES (?1, '#123456', 'Archive', 'custom', 9)",
        [&archive],
    )
    .unwrap();
    for (id, created) in [("later", 2), ("z", 1), ("a", 1)] {
        conn.execute(
            "INSERT INTO accounts (account_id,kind,provider,display_name,state,sort_order,created_at)
             VALUES (?1,'imap','generic_imap',?1,'disconnected',0,?2)", (id, created),
        ).unwrap();
    }
    run_migrations(&mut conn, &test_seams(real_fs()), &MIGRATIONS[..=prefix]).unwrap();
    let tx = conn.transaction().unwrap();
    let kept = account_appearance(&tx, &archive).unwrap();
    assert_eq!(
        (kept.color_srgb(), kept.symbol(), kept.revision()),
        ("#123456", Some("Archive"), 9)
    );
    for (id, expected) in [("a", "#0072b2"), ("z", "#e69f00"), ("later", "#009e73")] {
        assert_eq!(account_appearance(&tx, id).unwrap().color_srgb(), expected);
    }
    assert_eq!(
        tx.query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        18
    );
}

#[test]
fn migration_19_preserves_retained_generation_and_rejects_damaged_counters() {
    let prefix = MIGRATIONS
        .iter()
        .position(|migration| migration.version == 19)
        .unwrap();
    let oversized = format!("'{}'", i128::from(i64::MAX) + 1);
    for value in [
        "'7'",
        "'bad'",
        "CAST(x'ff' AS TEXT)",
        "'-1'",
        oversized.as_str(),
    ] {
        let mut conn = index_connection_through(prefix);
        conn.execute_batch(&format!(
            "INSERT INTO store_meta (key,value) VALUES ('{GENERATION_KEY}',{value})"
        ))
        .unwrap();
        let result = run_migrations(&mut conn, &test_seams(real_fs()), MIGRATIONS);
        if value == "'7'" {
            result.unwrap();
            let tx = conn.transaction().unwrap();
            assert_eq!(appearance_generation(&tx).unwrap(), 7);
            continue;
        }
        assert!(
            matches!(result, Err(StoreError::Corrupt { .. })),
            "{value}: {result:?}"
        );
        assert_eq!(
            conn.query_row("PRAGMA user_version", [], |row| row.get::<_, u32>(0))
                .unwrap(),
            18
        );
        assert_eq!(
            conn.query_row(
                "SELECT count(*) FROM store_meta WHERE key = ?1",
                [GENERATION_KEY],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
            1
        );
    }
}

#[test]
fn a_bad_legacy_assignment_refuses_migration_without_recoloring_or_partial_backfill() {
    let prefix = MIGRATIONS
        .iter()
        .position(|migration| migration.version == 18)
        .unwrap();
    let mut conn = index_connection_through(prefix);
    let archive = archive_account(&conn);
    conn.execute(
        "INSERT INTO account_appearance (account_id, color_srgb, assignment)
         VALUES (?1, '#0072b2', 'palette:v1:9')",
        [&archive],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO accounts (account_id,kind,provider,display_name,state,sort_order,created_at)
         VALUES ('missing','imap','generic_imap','Synthetic','active',0,1)",
        [],
    )
    .unwrap();
    let version: u32 = conn
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert!(matches!(
        run_migrations(&mut conn, &test_seams(real_fs()), MIGRATIONS),
        Err(StoreError::Corrupt { .. })
    ));
    assert_eq!(
        conn.query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
            .unwrap(),
        version
    );
    assert_eq!(
        conn.query_row("SELECT count(*) FROM account_appearance", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        conn.query_row("SELECT assignment FROM account_appearance", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "palette:v1:9"
    );
}

#[test]
fn backfill_classifies_stored_identity_key_damage_as_corrupt() {
    enum Damage {
        OversizedKey,
        MissingIdentity,
    }

    for damage in [Damage::OversizedKey, Damage::MissingIdentity] {
        let prefix = MIGRATIONS
            .iter()
            .position(|migration| migration.version == 18)
            .unwrap();
        let mut conn = index_connection_through(prefix);
        let archive = archive_account(&conn);
        conn.execute_batch("PRAGMA foreign_keys=OFF").unwrap();
        let id = match damage {
            Damage::OversizedKey => {
                let id = "x".repeat(MAX_KEY_BYTES + 1);
                conn.execute(
                    "INSERT INTO identities (identity_id,account_id,email,display_name,is_default,sendable)
                     VALUES (?1,?2,'synthetic@example.test','Synthetic',0,'validated')", (&id, &archive),
                ).unwrap();
                id
            }
            Damage::MissingIdentity => "dangling".to_owned(),
        };
        conn.execute(
            "INSERT INTO identity_appearance (identity_id) VALUES (?1)",
            [&id],
        )
        .unwrap();
        conn.execute_batch("PRAGMA foreign_keys=ON").unwrap();
        let result = run_migrations(&mut conn, &test_seams(real_fs()), MIGRATIONS);
        assert!(
            matches!(result, Err(StoreError::Corrupt { .. })),
            "{result:?}"
        );
        assert_eq!(
            conn.query_row("SELECT count(*) FROM account_appearance", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
}

#[test]
fn damaged_stored_colors_symbols_revisions_and_slots_are_corrupt_not_defaults() {
    for mutation in [
        "color_srgb = '#ABCDEF', assignment = 'custom'",
        "symbol = ''",
        "revision = 0",
        "assignment = 'palette:v1:00'",
        "assignment = 'palette:v1:1'",
    ] {
        let mut conn = index_connection();
        let archive = archive_account(&conn);
        conn.execute_batch("PRAGMA ignore_check_constraints=ON")
            .unwrap();
        conn.execute(
            &format!("UPDATE account_appearance SET {mutation} WHERE account_id = ?1"),
            [&archive],
        )
        .unwrap();
        conn.execute_batch("PRAGMA ignore_check_constraints=OFF")
            .unwrap();
        let tx = conn.transaction().unwrap();
        assert!(
            matches!(
                account_appearance(&tx, &archive),
                Err(StoreError::Corrupt { .. })
            ),
            "{mutation}"
        );
        assert!(
            matches!(
                initialize_account_appearance(&tx, &archive),
                Err(StoreError::Corrupt { .. })
            ),
            "{mutation}"
        );
        assert!(
            matches!(
                set_account_appearance(&tx, &archive, 1, &AccountColor::PaletteV1(0), None),
                Err(StoreError::Corrupt { .. })
            ),
            "{mutation}"
        );
    }
}

#[test]
fn identity_appearance_cannot_inherit_from_a_missing_account() {
    let mut conn = index_connection();
    conn.execute_batch(
        "PRAGMA foreign_keys=OFF;
        INSERT INTO identities (identity_id,account_id,email,display_name,is_default,sendable)
        VALUES ('orphan','missing','synthetic@example.test','Synthetic',0,'validated');
        PRAGMA foreign_keys=ON;",
    )
    .unwrap();
    let tx = conn.transaction().unwrap();
    assert!(matches!(
        identity_appearance(&tx, "orphan"),
        Err(StoreError::Corrupt { .. })
    ));
    assert!(matches!(
        set_identity_appearance(&tx, "orphan", None, Some("#112233"), None),
        Err(StoreError::Corrupt { .. })
    ));
    assert_eq!(appearance_generation(&tx).unwrap(), 0);
}

#[test]
fn sql_environment_failures_are_not_relabelled_as_corruption() {
    for code in [
        rusqlite::ffi::SQLITE_BUSY,
        rusqlite::ffi::SQLITE_IOERR,
        rusqlite::ffi::SQLITE_INTERRUPT,
    ] {
        let error = || rusqlite::Error::SqliteFailure(rusqlite::ffi::Error::new(code), None);
        let expected = crate::schema::classify(error(), "appearance storage");
        assert_eq!(
            std::mem::discriminant(&sql_error(error())),
            std::mem::discriminant(&expected)
        );
    }
}

#[test]
fn a_suppressed_sql_update_is_not_acknowledged_and_revision_overflow_is_explicit() {
    let mut conn = index_connection();
    let archive = archive_account(&conn);
    conn.execute_batch(
        "CREATE TRIGGER refuse_color BEFORE UPDATE ON account_appearance
                        BEGIN SELECT RAISE(IGNORE); END;",
    )
    .unwrap();
    let tx = conn.transaction().unwrap();
    assert!(matches!(
        set_account_appearance(&tx, &archive, 1, &AccountColor::PaletteV1(1), None),
        Err(StoreError::Corrupt { .. })
    ));
    assert_eq!(account_appearance(&tx, &archive).unwrap().revision(), 1);
    tx.commit().unwrap();
    conn.execute_batch("DROP TRIGGER refuse_color").unwrap();
    conn.execute(
        "UPDATE account_appearance SET revision = ?1 WHERE account_id = ?2",
        (i64::MAX, &archive),
    )
    .unwrap();
    let tx = conn.transaction().unwrap();
    assert!(matches!(
        set_account_appearance(&tx, &archive, i64::MAX, &AccountColor::PaletteV1(1), None),
        Err(StoreError::Corrupt { .. })
    ));
    assert_eq!(
        account_appearance(&tx, &archive).unwrap().revision(),
        i64::MAX
    );
}
