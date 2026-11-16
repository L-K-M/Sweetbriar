//! Proton account and body-failure row tests (spec/02 "Proton
//! accounts", plan M2.13a).

use rusqlite::params;
use sb_test_support::{ScratchDir, block_on, real_fs};
use sb_types::time::Timestamp;

use crate::StoreError;
use crate::open;
use crate::proton::{
    NewProtonAccount, ProtonBodyFailureReason, ProtonListingState, ProtonPasswordMode,
    begin_listing, body_failure, clear_body_failure, complete_listing, insert_proton_account,
    proton_account, proton_account_by_user_id, record_body_failure, set_diagnostics,
    set_event_cursor, update_listing_checkpoint,
};
use crate::testing::{reading, scratch_paths, test_seams};

fn now() -> Timestamp {
    Timestamp::from_millis(1_735_689_600_000)
}

fn proton_store(label: &str) -> (ScratchDir, crate::Store) {
    let (scratch, paths) = scratch_paths(label);
    let store = open(&paths, &test_seams(real_fs())).unwrap();
    (scratch, store)
}

/// Assert the typed refusal a stale revision or a second user gets.
fn expect_conflict(write: Result<(), StoreError>) {
    assert!(
        matches!(write, Err(StoreError::Conflict { .. })),
        "expected Conflict, got {write:?}"
    );
}

/// A Proton account row, as sign-in's account creation would write it.
fn insert_account(tx: &crate::Transaction, account_id: &str, sort_order: i64) {
    tx.execute(
        "INSERT INTO accounts
         (account_id, kind, provider, display_name, state, sort_order, created_at)
         VALUES (?1, 'proton', 'proton_api', 'Proton', 'active', ?2, 0)",
        params![account_id, sort_order],
    )
    .unwrap();
}

fn new_account(account_id: &str, proton_user_id: &str) -> NewProtonAccount {
    NewProtonAccount {
        account_id: account_id.to_owned(),
        api_base_url: "https://mail.proton.me/api".to_owned(),
        username: "user".to_owned(),
        proton_user_id: proton_user_id.to_owned(),
        credential_ref: format!("proton-{account_id}"),
        password_mode: ProtonPasswordMode::Mailbox,
    }
}

/// Insert, read by both keys, refuse a second account for one Proton
/// user, and refuse a stale revision (spec/02).
#[test]
fn proton_account_insert_read_and_refuse_another_user() {
    let (_scratch, store) = proton_store("proton-account");
    let write = block_on(store.write(crate::WritePriority::User, |tx| {
        insert_account(tx, "a1", 1);
        insert_proton_account(tx, &new_account("a1", "user-1")).map(|_| ())
    }));
    assert!(write.is_ok());

    let account = reading(&store, |tx| proton_account(tx, "a1")).expect("the row exists");
    assert_eq!(account.proton_user_id, "user-1");
    assert_eq!(account.password_mode, ProtonPasswordMode::Mailbox);
    assert_eq!(account.listing_state, ProtonListingState::None);
    assert_eq!(account.revision, 1);
    let by_user = reading(&store, |tx| proton_account_by_user_id(tx, "user-1"))
        .expect("the user id resolves");
    assert_eq!(by_user.account_id, "a1");
    assert!(reading(&store, |tx| proton_account(tx, "missing")).is_none());

    // A second account for the same Proton user is refused, never merged.
    let write = block_on(store.write(crate::WritePriority::User, |tx| {
        insert_account(tx, "a2", 2);
        insert_proton_account(tx, &new_account("a2", "user-1")).map(|_| ())
    }));
    expect_conflict(write);
    // So is a duplicate row for the same account.
    let write = block_on(store.write(crate::WritePriority::User, |tx| {
        insert_proton_account(tx, &new_account("a1", "user-2")).map(|_| ())
    }));
    expect_conflict(write);
    // A stale revision is Conflict even though the value is unchanged.
    let write = block_on(store.write(crate::WritePriority::User, |tx| {
        set_event_cursor(tx, "a1", "e1", 0).map(|_| ())
    }));
    expect_conflict(write);

    // The schema's UNIQUE is the backstop behind the accessor's check.
    let write = block_on(store.write(crate::WritePriority::User, |tx| {
        insert_account(tx, "a3", 3);
        tx.execute(
            "INSERT INTO proton_accounts
             (account_id, api_base_url, username, proton_user_id, credential_ref,
              password_mode, listing_state)
             VALUES ('a3', 'https://mail.proton.me/api', 'user', 'user-1', 'r', 1, 'none')",
            [],
        )
        .map(|_| ())
        .map_err(|e| crate::schema::classify(e, "insert a duplicate Proton user"))
    }));
    assert!(write.is_err(), "a duplicate proton_user_id was accepted");
}

/// The listing cursors follow the schema's CHECK invariants, and the
/// schema still rejects a state and cursor that disagree.
#[test]
fn proton_listing_cursors_follow_the_check_invariants() {
    let (_scratch, store) = proton_store("proton-listing");
    block_on(store.write(crate::WritePriority::User, |tx| {
        insert_account(tx, "a1", 1);
        insert_proton_account(tx, &new_account("a1", "user-1")).map(|_| ())
    }))
    .unwrap();

    let step = |f: fn(&crate::Transaction) -> Result<(), StoreError>| {
        block_on(store.write(crate::WritePriority::User, f)).unwrap()
    };
    step(|tx| {
        let account = begin_listing(tx, "a1", "e1", 1)?;
        assert_eq!(account.listing_state, ProtonListingState::Listing);
        assert_eq!(account.pending_cursor.as_deref(), Some("e1"));
        assert!(account.listing_checkpoint.is_none());
        Ok(())
    });
    step(|tx| {
        let account = update_listing_checkpoint(tx, "a1", "page-1", 2)?;
        assert_eq!(account.listing_checkpoint.as_deref(), Some("page-1"));
        Ok(())
    });
    // A checkpoint outside a listing is refused.
    step(|tx| complete_listing(tx, "a1", "e1", 3).map(|_| ()));
    let write = block_on(store.write(crate::WritePriority::User, |tx| {
        update_listing_checkpoint(tx, "a1", "page-2", 4).map(|_| ())
    }));
    expect_conflict(write);
    step(|tx| {
        let account = set_event_cursor(tx, "a1", "e2", 4)?;
        assert_eq!(account.event_cursor.as_deref(), Some("e2"));
        Ok(())
    });
    step(|tx| {
        let account = set_diagnostics(tx, "a1", Some("{\"code\":429}"), 5)?;
        assert_eq!(account.diagnostics_json.as_deref(), Some("{\"code\":429}"));
        Ok(())
    });
    step(|tx| set_diagnostics(tx, "a1", None, 6).map(|_| ()));

    // The schema's own CHECKs reject what the accessors never write.
    reading(&store, |tx| {
        for sql in [
            "UPDATE proton_accounts SET listing_state = 'complete',
             pending_cursor = 'e1' WHERE account_id = 'a1'",
            "UPDATE proton_accounts SET listing_state = 'none',
             listing_checkpoint = 'page-1' WHERE account_id = 'a1'",
        ] {
            assert!(tx.execute(sql, []).is_err(), "{sql}");
        }
        Ok(())
    });
}

/// A body failure records, increments, reads and clears (spec/02,
/// plan M2.16).
#[test]
fn proton_body_failures_record_read_clear() {
    let (_scratch, store) = proton_store("proton-failures");
    block_on(store.write(crate::WritePriority::User, |tx| {
        insert_account(tx, "a1", 1);
        tx.execute(
            "INSERT INTO logical_messages (account_id, created_at) VALUES ('a1', 0)",
            [],
        )
        .unwrap();
        record_body_failure(
            tx,
            1,
            ProtonBodyFailureReason::Decrypt,
            Some("redacted"),
            1,
            now(),
        )
    }))
    .unwrap();

    let failure = reading(&store, |tx| body_failure(tx, 1)).expect("the failure exists");
    assert_eq!(failure.reason, ProtonBodyFailureReason::Decrypt);
    assert_eq!(failure.attempts, 1);
    assert_eq!(failure.key_generation, 1);

    block_on(store.write(crate::WritePriority::User, |tx| {
        record_body_failure(tx, 1, ProtonBodyFailureReason::NoKey, None, 2, now())
    }))
    .unwrap();
    let failure = reading(&store, |tx| body_failure(tx, 1)).expect("the failure exists");
    assert_eq!(
        (failure.reason, failure.attempts, failure.key_generation),
        (ProtonBodyFailureReason::NoKey, 2, 2)
    );

    block_on(store.write(crate::WritePriority::User, |tx| clear_body_failure(tx, 1))).unwrap();
    assert!(reading(&store, |tx| body_failure(tx, 1)).is_none());
    let write = block_on(store.write(crate::WritePriority::User, |tx| clear_body_failure(tx, 1)));
    assert!(
        matches!(write, Err(StoreError::NotFound { .. })),
        "expected NotFound, got {write:?}"
    );
}
