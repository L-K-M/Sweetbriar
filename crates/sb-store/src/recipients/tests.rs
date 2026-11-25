use super::*;
use crate::testing::fixtures;
use rusqlite::{Connection, params};

/// A non-local sending account.
fn account(conn: &Connection, id: &str, sort: i64) {
    conn.execute(
        "INSERT INTO accounts (account_id, kind, provider, display_name, state, sort_order,
         created_at) VALUES (?1, 'imap', 'generic_imap', ?1, 'active', ?2, 0)",
        params![id, sort],
    )
    .unwrap();
}

#[test]
fn allow_recipient_canonicalizes_and_a_replay_writes_nothing() {
    let mut connection = fixtures::index_connection();
    let archive = fixtures::archive_account(&connection);
    account(&connection, "a-account", 1);
    let tx = connection.transaction().unwrap();

    assert!(matches!(
        allow_recipient(&tx, "a-account", "not an address", 7),
        Err(StoreError::InvalidInput { .. })
    ));
    assert!(matches!(
        allow_recipient(&tx, "missing-account", "new@example.test", 7),
        Err(StoreError::NotFound { .. })
    ));
    // The local archive can't send; it is refused before any allowance
    // is read or written.
    assert!(matches!(
        allow_recipient(&tx, &archive, "new@example.test", 7),
        Err(StoreError::InvalidInput { .. })
    ));

    assert!(allow_recipient(&tx, "a-account", "New@Example.Test", 7).unwrap());
    let stored: (String, String, i64) = tx
        .query_row(
            "SELECT canonical, email, created_at FROM recipient_allowances
             WHERE account_id = 'a-account'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(
        stored,
        (
            "New@example.test".to_owned(),
            "New@Example.Test".to_owned(),
            7
        )
    );

    // The same address with another domain case is already allowed: no
    // second row and the first spelling and timestamp survive.
    // Same local spelling, different domain case: the same canonical.
    assert!(!allow_recipient(&tx, "a-account", "New@EXAMPLE.test", 8).unwrap());
    let count: i64 = tx
        .query_row("SELECT count(*) FROM recipient_allowances", [], |row| {
            row.get(0)
        })
        .unwrap();
    assert_eq!(count, 1);
    let created_at: i64 = tx
        .query_row(
            "SELECT created_at FROM recipient_allowances WHERE account_id = 'a-account'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(created_at, 7);
    tx.commit().unwrap();
}

#[test]
fn remove_recipient_allowance_requires_an_existing_allowance() {
    let mut connection = fixtures::index_connection();
    let archive = fixtures::archive_account(&connection);
    account(&connection, "a-account", 1);
    let tx = connection.transaction().unwrap();

    assert!(matches!(
        remove_recipient_allowance(&tx, "a-account", "new@example.test"),
        Err(StoreError::NotFound { .. })
    ));
    assert!(matches!(
        remove_recipient_allowance(&tx, "missing-account", "new@example.test"),
        Err(StoreError::NotFound { .. })
    ));
    assert!(matches!(
        remove_recipient_allowance(&tx, &archive, "new@example.test"),
        Err(StoreError::InvalidInput { .. })
    ));
    assert!(matches!(
        remove_recipient_allowance(&tx, "a-account", "not an address"),
        Err(StoreError::InvalidInput { .. })
    ));

    allow_recipient(&tx, "a-account", "New@Example.Test", 7).unwrap();
    remove_recipient_allowance(&tx, "a-account", "New@EXAMPLE.test").unwrap();
    assert!(
        allowed(&tx, "a-account", &["new@example.test".to_owned()])
            .unwrap()
            .is_empty()
    );
    assert!(recipient_allowances(&tx).unwrap().is_empty());
    tx.commit().unwrap();
}

#[test]
fn sender_accounts_and_allowed_answer_for_many_canonicals() {
    let mut connection = fixtures::index_connection();
    account(&connection, "a-account", 1);
    account(&connection, "b-account", 2);
    let tx = connection.transaction().unwrap();
    tx.execute_batch(
        "INSERT INTO d_account_senders(canonical, account_id, first_seen_at) VALUES
           ('x@example.test', 'a-account', 0),
           ('shared@example.test', 'a-account', 0),
           ('shared@example.test', 'b-account', 0),
           ('y@example.test', 'b-account', 0);",
    )
    .unwrap();
    allow_recipient(&tx, "a-account", "Allowed@Example.Test", 7).unwrap();
    allow_recipient(&tx, "b-account", "Other@Example.Test", 8).unwrap();

    let canonicals = vec![
        "x@example.test".to_owned(),
        "shared@example.test".to_owned(),
        "y@example.test".to_owned(),
        "absent@example.test".to_owned(),
        "Allowed@example.test".to_owned(),
        "Other@example.test".to_owned(),
    ];
    let senders = sender_accounts(&tx, &canonicals).unwrap();
    assert_eq!(
        senders.get("x@example.test").unwrap(),
        &BTreeSet::from(["a-account".to_owned()])
    );
    assert_eq!(
        senders.get("shared@example.test").unwrap(),
        &BTreeSet::from(["a-account".to_owned(), "b-account".to_owned()])
    );
    assert_eq!(
        senders.get("y@example.test").unwrap(),
        &BTreeSet::from(["b-account".to_owned()])
    );
    assert!(senders.get("absent@example.test").unwrap().is_empty());

    let allowed_a = allowed(&tx, "a-account", &canonicals).unwrap();
    assert_eq!(
        allowed_a,
        // A recorded sender is not itself an allowance.
        BTreeSet::from(["Allowed@example.test".to_owned()])
    );
    let allowed_b = allowed(&tx, "b-account", &canonicals).unwrap();
    assert_eq!(allowed_b, BTreeSet::from(["Other@example.test".to_owned()]));
    tx.commit().unwrap();
}

#[test]
fn recipient_allowances_are_ordered_by_account_then_canonical() {
    let mut connection = fixtures::index_connection();
    account(&connection, "a-account", 1);
    account(&connection, "b-account", 2);
    let tx = connection.transaction().unwrap();
    allow_recipient(&tx, "b-account", "z@example.test", 9).unwrap();
    allow_recipient(&tx, "a-account", "m@example.test", 7).unwrap();
    allow_recipient(&tx, "b-account", "a@example.test", 8).unwrap();
    allow_recipient(&tx, "a-account", "a@example.test", 6).unwrap();
    assert_eq!(
        recipient_allowances(&tx).unwrap(),
        vec![
            StoredAllowance {
                account_id: "a-account".to_owned(),
                canonical: "a@example.test".to_owned(),
                email: "a@example.test".to_owned(),
                created_at: 6,
            },
            StoredAllowance {
                account_id: "a-account".to_owned(),
                canonical: "m@example.test".to_owned(),
                email: "m@example.test".to_owned(),
                created_at: 7,
            },
            StoredAllowance {
                account_id: "b-account".to_owned(),
                canonical: "a@example.test".to_owned(),
                email: "a@example.test".to_owned(),
                created_at: 8,
            },
            StoredAllowance {
                account_id: "b-account".to_owned(),
                canonical: "z@example.test".to_owned(),
                email: "z@example.test".to_owned(),
                created_at: 9,
            },
        ]
    );
    tx.commit().unwrap();
}

#[test]
fn readiness_is_corrupt_on_a_missing_or_malformed_key() {
    let mut connection = fixtures::index_connection();
    let tx = connection.transaction().unwrap();
    // Migration 38 stored revision 2 while the sweep has published none.
    assert!(!account_senders_ready(&tx).unwrap());

    tx.execute(
        "UPDATE store_meta SET value = 'not-a-number' WHERE key = 'account_senders_from'",
        [],
    )
    .unwrap();
    assert!(matches!(
        account_senders_ready(&tx),
        Err(StoreError::Corrupt { .. })
    ));

    tx.execute(
        "UPDATE store_meta SET value = '0' WHERE key = 'account_senders_from'",
        [],
    )
    .unwrap();
    assert!(account_senders_ready(&tx).unwrap());

    tx.execute(
        "DELETE FROM store_meta WHERE key = 'seen_address_swept'",
        [],
    )
    .unwrap();
    assert!(matches!(
        account_senders_ready(&tx),
        Err(StoreError::Corrupt { .. })
    ));
    tx.commit().unwrap();
}

#[test]
fn the_fence_blocks_addresses_known_only_to_other_accounts() {
    let mut connection = fixtures::index_connection();
    for (id, sort) in [("a-account", 1), ("b-account", 2), ("c-account", 3)] {
        account(&connection, id, sort);
    }
    let tx = connection.transaction().unwrap();
    tx.execute_batch(
        "INSERT INTO d_account_senders(canonical, account_id, first_seen_at) VALUES
           ('mine@example.test', 'a-account', 0),
           ('shared@example.test', 'a-account', 0),
           ('shared@example.test', 'b-account', 0),
           ('theirs@example.test', 'b-account', 0),
           ('both@example.test', 'b-account', 0),
           ('both@example.test', 'c-account', 0),
           ('kept@example.test', 'b-account', 0);",
    )
    .unwrap();
    allow_recipient(&tx, "a-account", "kept@example.test", 7).unwrap();
    let addresses = [
        "mine@example.test",
        "theirs@Example.Test",
        "shared@example.test",
        "nobody@example.test",
        "kept@EXAMPLE.test",
        "not an address",
        "both@example.test",
        "theirs@EXAMPLE.TEST",
    ];

    // Before the first pass after migration 38 the check blocks nothing,
    // recorded rows or not.
    let fence = recipient_fence(&tx, "a-account", &addresses).unwrap();
    assert_eq!(
        fence,
        StoredRecipientFence {
            ready: false,
            blocked: Vec::new(),
        }
    );

    tx.execute(
        "UPDATE store_meta SET value = '0' WHERE key = 'account_senders_from'",
        [],
    )
    .unwrap();
    let blocked = |account_id: &str| recipient_fence(&tx, account_id, &addresses).unwrap();
    let fence = blocked("a-account");
    assert!(fence.ready);
    assert_eq!(
        fence.blocked,
        vec![
            // Once per canonical address, in the spelling seen first.
            StoredBlockedRecipient {
                address: "theirs@Example.Test".to_owned(),
                accounts: vec!["b-account".to_owned()],
            },
            StoredBlockedRecipient {
                address: "both@example.test".to_owned(),
                accounts: vec!["b-account".to_owned(), "c-account".to_owned()],
            },
        ]
    );
    // From B, the address only A knows is blocked and the allowance,
    // which is A's, doesn't apply.
    let fence = blocked("b-account");
    assert_eq!(
        fence
            .blocked
            .iter()
            .map(|recipient| recipient.address.as_str())
            .collect::<Vec<_>>(),
        vec!["mine@example.test"]
    );
    tx.commit().unwrap();
}
