//! Appearance persistence through the native Resources facade.

use sb_resources::{
    AccountColor, AppearanceAssignment, Resources, StoreError, StoredAccountAppearance,
    Transaction, WritePriority, account_appearance, identity_appearance,
    initialize_account_appearance, set_account_appearance, set_identity_appearance,
};
use sb_test_support::{ScratchDir, block_on};

const COLORS: [&str; 7] = [
    "#0072b2", "#e69f00", "#009e73", "#cc79a7", "#56b4e9", "#d55e00", "#f0e442",
];
const IDENTITY: &str = "identity";

fn resources(label: &str) -> (ScratchDir, Resources) {
    let scratch = ScratchDir::new(label);
    let resources = Resources::open(&scratch.path().join("store")).unwrap();
    (scratch, resources)
}

fn write<T: Send + 'static>(
    res: &Resources,
    f: impl FnOnce(&Transaction) -> Result<T, StoreError> + Send + 'static,
) -> Result<T, StoreError> {
    block_on(res.write(WritePriority::User, f))
}

fn archive(res: &Resources) -> String {
    res.read(|tx| {
        Ok(tx
            .query_row(
                "SELECT account_id FROM accounts WHERE provider = 'local_archive'",
                [],
                |r| r.get(0),
            )
            .unwrap())
    })
    .unwrap()
}

fn add_account(res: &Resources, id: &str) -> StoredAccountAppearance {
    let id = id.to_owned();
    write(res, move |tx| {
        tx.execute("INSERT INTO accounts (account_id,kind,provider,display_name,state,sort_order,created_at)
                    VALUES (?1,'imap','generic_imap',?1,'disconnected',0,1)", [&id]).unwrap();
        initialize_account_appearance(tx, &id)
    }).unwrap()
}

fn add_identity(res: &Resources, account: &str) {
    let account = account.to_owned();
    write(res, move |tx| {
        tx.execute(
            "INSERT INTO identities (identity_id,account_id,email,display_name,is_default,sendable)
                    VALUES (?1,?2,'synthetic@example.test','Synthetic',0,'validated')",
            (IDENTITY, account),
        )
        .unwrap();
        Ok(())
    })
    .unwrap();
}

#[test]
fn initialization_uses_fixed_order_then_least_used_without_recoloring() {
    let (_scratch, res) = resources("appearance-assign");
    let archive = archive(&res);
    let first = res.read(|tx| account_appearance(tx, &archive)).unwrap();
    assert_eq!(first.color_srgb(), COLORS[0]);
    assert_eq!(first.assignment(), AppearanceAssignment::PaletteV1(0));
    for (slot, expected) in COLORS.iter().enumerate().skip(1) {
        let row = add_account(&res, &format!("account-{slot}"));
        assert_eq!(row.color_srgb(), *expected);
        assert_eq!(
            row.assignment(),
            AppearanceAssignment::PaletteV1(slot as u8)
        );
    }
    let repeated = add_account(&res, "eighth");
    assert_eq!(repeated.color_srgb(), COLORS[0]);
    let archive_copy = archive.clone();
    write(&res, move |tx| {
        tx.execute(
            "UPDATE accounts SET sort_order = -1, state = 'paused' WHERE account_id = ?1",
            [&archive_copy],
        )
        .unwrap();
        let changes = tx.total_changes();
        assert_eq!(initialize_account_appearance(tx, "eighth")?, repeated);
        assert_eq!(tx.total_changes(), changes);
        Ok(())
    })
    .unwrap();
    assert_eq!(
        res.read(|tx| account_appearance(tx, &archive)).unwrap(),
        first
    );
    assert_eq!(add_account(&res, "ninth").color_srgb(), COLORS[1]);
}

#[test]
fn matching_custom_and_disconnected_colors_count_as_uses() {
    let (_scratch, res) = resources("appearance-custom-use");
    let archive = archive(&res);
    write(&res, move |tx| {
        tx.execute(
            "UPDATE accounts SET state = 'disconnected' WHERE account_id = ?1",
            [&archive],
        )
        .unwrap();
        set_account_appearance(
            tx,
            &archive,
            1,
            &AccountColor::Custom("#E69F00".into()),
            None,
        )
    })
    .unwrap();
    assert_eq!(add_account(&res, "blue").color_srgb(), COLORS[0]);
    // Ignoring the custom/disconnected orange would incorrectly reuse orange.
    assert_eq!(add_account(&res, "green").color_srgb(), COLORS[2]);
}

#[test]
fn setters_canonicalize_fence_revisions_and_do_not_rewrite_unchanged_values() {
    let (_scratch, res) = resources("appearance-set");
    let archive = archive(&res);
    let id = archive.clone();
    let changed = write(&res, move |tx| {
        set_account_appearance(
            tx,
            &id,
            1,
            &AccountColor::Custom("#AABBCC".into()),
            Some("Work"),
        )
    })
    .unwrap();
    assert_eq!(
        (changed.color_srgb(), changed.symbol(), changed.revision()),
        ("#aabbcc", Some("Work"), 2)
    );
    assert_eq!(changed.assignment(), AppearanceAssignment::Custom);
    let id = archive.clone();
    write(&res, move |tx| {
        let changes = tx.total_changes();
        assert_eq!(
            set_account_appearance(
                tx,
                &id,
                2,
                &AccountColor::Custom("#AABBCC".into()),
                Some("Work")
            )?,
            changed
        );
        assert_eq!(tx.total_changes(), changes);
        Ok(())
    })
    .unwrap();
    for color in ["", "#abc", "#12345678", "#gggggg", " #aabbcc"] {
        let id = archive.clone();
        let color = color.to_owned();
        assert!(matches!(
            write(&res, move |tx| set_account_appearance(
                tx,
                &id,
                2,
                &AccountColor::Custom(color),
                None
            )),
            Err(StoreError::InvalidInput { .. })
        ));
    }
    let id = archive.clone();
    assert!(matches!(
        write(&res, move |tx| set_account_appearance(
            tx,
            &id,
            1,
            &AccountColor::Custom("#aabbcc".into()),
            Some("Work")
        )),
        Err(StoreError::Conflict { .. })
    ));
    for (revision, choice, symbol) in [
        (0, AccountColor::PaletteV1(0), None),
        (2, AccountColor::PaletteV1(7), None),
        (2, AccountColor::PaletteV1(0), Some(String::new())),
        (2, AccountColor::PaletteV1(0), Some("é".repeat(65))),
    ] {
        let id = archive.clone();
        assert!(matches!(
            write(&res, move |tx| set_account_appearance(
                tx,
                &id,
                revision,
                &choice,
                symbol.as_deref()
            )),
            Err(StoreError::InvalidInput { .. })
        ));
    }
    assert!(matches!(
        write(&res, |tx| set_account_appearance(
            tx,
            "missing",
            1,
            &AccountColor::PaletteV1(0),
            None
        )),
        Err(StoreError::NotFound { .. })
    ));
    let id = archive.clone();
    let edge = write(&res, move |tx| {
        set_account_appearance(
            tx,
            &id,
            2,
            &AccountColor::PaletteV1(6),
            Some(&"é".repeat(64)),
        )
    })
    .unwrap();
    assert_eq!(
        (
            edge.color_srgb(),
            edge.symbol().unwrap().len(),
            edge.revision()
        ),
        (COLORS[6], 128, 3)
    );
}

#[test]
fn absent_identity_and_explicit_inheritance_have_distinct_revisions() {
    let (_scratch, res) = resources("appearance-identity");
    let account = archive(&res);
    add_identity(&res, &account);
    let absent = res.read(|tx| identity_appearance(tx, IDENTITY)).unwrap();
    assert_eq!(absent.account_id(), account);
    assert_eq!(
        (
            absent.revision(),
            absent.color_override_srgb(),
            absent.symbol_override()
        ),
        (None, None, None)
    );
    assert!(matches!(
        write(&res, |tx| set_identity_appearance(
            tx,
            IDENTITY,
            Some(1),
            None,
            None
        )),
        Err(StoreError::Conflict { .. })
    ));
    let custom = write(&res, |tx| {
        set_identity_appearance(tx, IDENTITY, None, Some("#ABCDEF"), Some("Alias"))
    })
    .unwrap();
    assert_eq!(
        (custom.revision(), custom.color_override_srgb()),
        (Some(1), Some("#abcdef"))
    );
    assert!(matches!(
        write(&res, |tx| set_identity_appearance(
            tx, IDENTITY, None, None, None
        )),
        Err(StoreError::Conflict { .. })
    ));
    let inherit = write(&res, |tx| {
        set_identity_appearance(tx, IDENTITY, Some(1), None, None)
    })
    .unwrap();
    assert_eq!(
        (
            inherit.revision(),
            inherit.color_override_srgb(),
            inherit.symbol_override()
        ),
        (Some(2), None, None)
    );
    write(&res, move |tx| {
        let changes = tx.total_changes();
        assert_eq!(
            set_identity_appearance(tx, IDENTITY, Some(2), None, None)?,
            inherit
        );
        assert_eq!(tx.total_changes(), changes);
        assert_eq!(
            tx.query_row("SELECT count(*) FROM identity_appearance", [], |r| r
                .get::<_, i64>(0))
                .unwrap(),
            1
        );
        Ok(())
    })
    .unwrap();
    assert!(matches!(
        res.read(|tx| identity_appearance(tx, "missing")),
        Err(StoreError::NotFound { .. })
    ));
}

#[test]
fn creation_and_setting_failures_roll_back_without_assignment_leaks() {
    let (_scratch, res) = resources("appearance-rollback");
    let account = archive(&res);
    add_identity(&res, &account);
    let id = account.clone();
    let result = write(&res, move |tx| {
        set_account_appearance(tx, &id, 1, &AccountColor::Custom("#010203".into()), None)?;
        set_identity_appearance(tx, IDENTITY, None, Some("#040506"), None)?;
        Err::<(), _>(StoreError::internal("injected failure"))
    });
    assert!(matches!(result, Err(StoreError::Internal { .. })));
    assert_eq!(
        res.read(|tx| account_appearance(tx, &account))
            .unwrap()
            .revision(),
        1
    );
    assert_eq!(
        res.read(|tx| identity_appearance(tx, IDENTITY))
            .unwrap()
            .revision(),
        None
    );
    let result = write(&res, |tx| {
        tx.execute("INSERT INTO accounts (account_id,kind,provider,display_name,state,sort_order,created_at)
                    VALUES ('rolled-back','imap','generic_imap','Synthetic','active',0,1)", []).unwrap();
        initialize_account_appearance(tx, "rolled-back")?;
        Err::<(), _>(StoreError::internal("injected failure"))
    });
    assert!(result.is_err());
    assert!(matches!(
        res.read(|tx| account_appearance(tx, "rolled-back")),
        Err(StoreError::NotFound { .. })
    ));
    assert_eq!(add_account(&res, "after-rollback").color_srgb(), COLORS[1]);
    write(&res, |tx| {
        tx.execute_batch(
            "CREATE TRIGGER reject_color BEFORE UPDATE ON account_appearance
                          BEGIN SELECT RAISE(ABORT, 'injected SQL failure'); END;",
        )
        .unwrap();
        Ok(())
    })
    .unwrap();
    let id = account.clone();
    assert!(matches!(
        write(&res, move |tx| set_account_appearance(
            tx,
            &id,
            1,
            &AccountColor::PaletteV1(2),
            None
        )),
        Err(StoreError::Internal { .. })
    ));
    assert_eq!(
        res.read(|tx| account_appearance(tx, &account))
            .unwrap()
            .revision(),
        1
    );
}

#[test]
fn records_survive_reopen_and_corrupt_assignments_never_become_defaults() {
    let (scratch, res) = resources("appearance-reopen");
    let account = archive(&res);
    add_identity(&res, &account);
    let id = account.clone();
    let saved = write(&res, move |tx| {
        set_account_appearance(
            tx,
            &id,
            1,
            &AccountColor::Custom("#654321".into()),
            Some("Personal"),
        )
    })
    .unwrap();
    let identity = write(&res, |tx| {
        set_identity_appearance(tx, IDENTITY, None, Some("#abcdef"), None)
    })
    .unwrap();
    res.close().unwrap();
    let res = Resources::open(&scratch.path().join("store")).unwrap();
    assert_eq!(
        res.read(|tx| account_appearance(tx, &account)).unwrap(),
        saved
    );
    assert_eq!(
        res.read(|tx| identity_appearance(tx, IDENTITY)).unwrap(),
        identity
    );
    let id = account.clone();
    write(&res, move |tx| {
        tx.execute(
            "UPDATE account_appearance SET assignment = 'palette:v1:9' WHERE account_id = ?1",
            [&id],
        )
        .unwrap();
        Ok(())
    })
    .unwrap();
    assert!(matches!(
        res.read(|tx| account_appearance(tx, &account)),
        Err(StoreError::Corrupt { .. })
    ));
    let id = account.clone();
    assert!(matches!(
        write(&res, move |tx| initialize_account_appearance(tx, &id)),
        Err(StoreError::Corrupt { .. })
    ));
}
