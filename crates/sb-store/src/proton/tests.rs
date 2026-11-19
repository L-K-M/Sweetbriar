//! Proton account and body-failure row tests (spec/02 "Proton
//! accounts", plan M2.13a).

use rusqlite::{OptionalExtension, params};
use sb_test_support::{ScratchDir, block_on, real_fs};
use sb_types::time::Timestamp;

use crate::StoreError;
use crate::accounts::{CredentialPause, IdentitySendable, NewIdentity};
use crate::open;
use crate::proton::{
    HUMAN_VERIFICATION_REASON, NewProtonAccount, NewProtonSignIn, ProtonBodyFailureReason,
    ProtonListingState, ProtonPasswordMode, SESSION_REFUSED_REASON, begin_listing, body_failure,
    clear_body_failure, commit_proton_sign_in, complete_listing, disconnect_proton_account,
    insert_proton_account, pause_for_proton_credential, pause_for_reason, pause_for_session,
    proton_account, proton_account_by_user_id, reactivate_proton_account, record_body_failure,
    record_refusal, refresh_proton_identities, set_diagnostics, set_event_cursor,
    update_listing_checkpoint,
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

/// A completed sign-in's rows, as the resources layer would build them.
fn sign_in_for(account_id: &str, proton_user_id: &str) -> NewProtonSignIn {
    NewProtonSignIn {
        account_id: account_id.to_owned(),
        display_name: "Owner".to_owned(),
        api_base_url: "https://mail.proton.me/api".to_owned(),
        username: "owner".to_owned(),
        proton_user_id: proton_user_id.to_owned(),
        credential_ref: format!("account/{account_id}/proton-session"),
        password_mode: ProtonPasswordMode::Mailbox,
        identities: vec![NewIdentity {
            identity_id: "addr-1".to_owned(),
            email: "owner@example.test".to_owned(),
            display_name: "Owner".to_owned(),
            is_default: true,
            sendable: IdentitySendable::Validated,
        }],
    }
}

/// A sign-in again's `Complete` makes a restored account active, so it
/// reconciles in the same transaction (07): the listing resets and
/// `restore_pending` clears.
#[test]
fn reactivate_reconciles_a_restore_pending_account() {
    let (_scratch, store) = proton_store("proton-reactivate-restore");
    block_on(store.write(crate::WritePriority::User, |tx| {
        insert_account(tx, "a1", 1);
        insert_proton_account(tx, &new_account("a1", "user-1"))?;
        // A restore left the account needs_attention and restore-pending,
        // with the backed-up completed listing.
        tx.execute(
            "UPDATE accounts
             SET state = 'needs_attention', state_reason = 'Sign in again',
                 restore_pending = 1
             WHERE account_id = 'a1'",
            [],
        )
        .map_err(|e| crate::schema::classify(e, "pause the account"))?;
        tx.execute(
            "UPDATE proton_accounts
             SET listing_state = 'complete', event_cursor = 'e1'
             WHERE account_id = 'a1'",
            [],
        )
        .map_err(|e| crate::schema::classify(e, "complete the listing"))?;
        reactivate_proton_account(tx, "a1").map(|_| ())
    }))
    .unwrap();

    let account = reading(&store, |tx| proton_account(tx, "a1")).expect("the row exists");
    assert_eq!(account.listing_state, ProtonListingState::None);
    assert_eq!(account.event_cursor, None);
    let (state, pending): (String, i64) = reading(&store, |tx| {
        tx.query_row(
            "SELECT state, restore_pending FROM accounts WHERE account_id = 'a1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|e| crate::schema::classify(e, "read the account"))
    });
    assert_eq!(state, "active");
    assert_eq!(pending, 0);
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

/// Signing an existing account in again updates it in place: same
/// UUID, generation and revision bumped, username and password mode
/// replaced, identities upserted with the first as the one default
/// (spec/07). An address the new session no longer returns stays, since
/// removal never deletes (spec/02), but it can no longer send.
#[test]
fn signing_in_again_updates_the_account_in_place() {
    let (_scratch, store) = proton_store("proton-again");
    block_on(store.write(crate::WritePriority::User, |tx| {
        insert_account(tx, "a1", 1);
        insert_proton_account(tx, &new_account("a1", "user-1")).map(|_| ())?;
        tx.execute(
            "INSERT INTO identities
             (identity_id, account_id, email, display_name, is_default, sendable)
             VALUES ('addr-gone', 'a1', 'gone@example.test', 'Gone', 1, 'validated')",
            [],
        )
        .map(|_| ())
        .map_err(|e| crate::schema::classify(e, "seed a removed address"))
    }))
    .unwrap();

    let sign_in = NewProtonSignIn {
        account_id: "a1".to_owned(),
        display_name: "Renamed".to_owned(),
        api_base_url: "https://mail.proton.me/api".to_owned(),
        username: "renamed".to_owned(),
        proton_user_id: "user-1".to_owned(),
        credential_ref: "account/a1/proton-session".to_owned(),
        password_mode: ProtonPasswordMode::Login,
        identities: vec![
            NewIdentity {
                identity_id: "addr-1".to_owned(),
                email: "one@example.test".to_owned(),
                display_name: "One".to_owned(),
                is_default: false,
                sendable: IdentitySendable::Validated,
            },
            NewIdentity {
                identity_id: "addr-2".to_owned(),
                email: "two@example.test".to_owned(),
                display_name: "Two".to_owned(),
                is_default: false,
                sendable: IdentitySendable::Unavailable,
            },
        ],
    };
    block_on(store.write(crate::WritePriority::User, move |tx| {
        commit_proton_sign_in(tx, &sign_in, now())
    }))
    .unwrap();

    let account = reading(&store, |tx| proton_account(tx, "a1")).expect("the row");
    assert_eq!(account.username, "renamed");
    assert_eq!(account.password_mode, ProtonPasswordMode::Login);
    assert_eq!(account.revision, 2);
    let row: (i64, i64, String, String) = reading(&store, |tx| {
        tx.query_row(
            "SELECT config_generation, revision, display_name, state
             FROM accounts WHERE account_id = 'a1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .map_err(|e| crate::schema::classify(e, "read the account row"))
    });
    assert_eq!(row, (2, 2, "Renamed".to_owned(), "active".to_owned()));
    let identities: Vec<(String, i64, String)> = reading(&store, |tx| {
        let mut statement = tx
            .prepare(
                "SELECT email, is_default, sendable FROM identities
                 WHERE account_id = 'a1' ORDER BY email",
            )
            .map_err(|e| crate::schema::classify(e, "read the identities"))?;
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .map_err(|e| crate::schema::classify(e, "read the identities"))?;
        rows.collect::<Result<Vec<_>, _>>()
            .map_err(|e| crate::schema::classify(e, "read the identities"))
    });
    let identity = |email: &str, default: i64, sendable: &str| {
        (email.to_owned(), default, sendable.to_owned())
    };
    assert_eq!(
        identities,
        vec![
            identity("gone@example.test", 0, "unavailable"),
            identity("one@example.test", 1, "validated"),
            identity("two@example.test", 0, "unavailable"),
        ]
    );
}

/// An identity id another account holds is `Conflict`, never moved to
/// the account signing in.
#[test]
fn signing_in_again_never_takes_another_accounts_identity() {
    let (_scratch, store) = proton_store("proton-again-identity");
    block_on(store.write(crate::WritePriority::User, |tx| {
        insert_account(tx, "a1", 1);
        insert_proton_account(tx, &new_account("a1", "user-1"))?;
        insert_account(tx, "a2", 2);
        insert_proton_account(tx, &new_account("a2", "user-2"))?;
        tx.execute(
            "INSERT INTO identities
             (identity_id, account_id, email, display_name, is_default, sendable)
             VALUES ('addr-1', 'a2', 'other@example.test', 'Other', 1, 'validated')",
            [],
        )
        .map(|_| ())
        .map_err(|e| crate::schema::classify(e, "seed another account's identity"))
    }))
    .unwrap();

    let sign_in = sign_in_for("a1", "user-1");
    let write = block_on(store.write(crate::WritePriority::User, move |tx| {
        commit_proton_sign_in(tx, &sign_in, now())
    }));
    expect_conflict(write);

    let owner: String = reading(&store, |tx| {
        tx.query_row(
            "SELECT account_id FROM identities WHERE identity_id = 'addr-1'",
            [],
            |row| row.get(0),
        )
        .map_err(|e| crate::schema::classify(e, "read the identity's account"))
    });
    assert_eq!(owner, "a2");
}

fn identity(id: &str, email: &str, sendable: IdentitySendable) -> NewIdentity {
    NewIdentity {
        identity_id: id.to_owned(),
        email: email.to_owned(),
        display_name: "Owner".to_owned(),
        is_default: false,
        sendable,
    }
}

/// An address, user or key event's refresh writes, and bumps the
/// account's generation, only when an identity changed; an address no
/// longer returned keeps its row but can't send (spec/02, spec/04).
#[test]
fn refreshing_identities_writes_only_a_change() {
    let (_scratch, store) = proton_store("proton-refresh-identities");
    let sign_in = sign_in_for("a1", "user-1");
    block_on(store.write(crate::WritePriority::User, move |tx| {
        commit_proton_sign_in(tx, &sign_in, now())
    }))
    .unwrap();
    let refresh = |identities: Vec<NewIdentity>| {
        block_on(store.write(crate::WritePriority::User, move |tx| {
            refresh_proton_identities(tx, "a1", &identities)
        }))
    };
    let state = || {
        reading(&store, |tx| {
            let generation: i64 = tx
                .query_row(
                    "SELECT config_generation FROM accounts WHERE account_id = 'a1'",
                    [],
                    |row| row.get(0),
                )
                .map_err(|e| crate::schema::classify(e, "read the generation"))?;
            let mut statement = tx
                .prepare(
                    "SELECT identity_id, is_default, sendable FROM identities
                     ORDER BY identity_id",
                )
                .map_err(|e| crate::schema::classify(e, "read the identities"))?;
            let rows: Vec<(String, bool, String)> = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .and_then(Iterator::collect)
                .map_err(|e| crate::schema::classify(e, "read the identities"))?;
            Ok((generation, rows))
        })
    };
    let owner = || identity("addr-1", "owner@example.test", IdentitySendable::Validated);
    let alias = || identity("addr-2", "alias@example.test", IdentitySendable::Validated);
    let row =
        |id: &str, default: bool, sendable: &str| (id.to_owned(), default, sendable.to_owned());
    let (generation, _) = state();

    assert!(!refresh(vec![owner()]).unwrap());
    assert_eq!(
        state(),
        (generation, vec![row("addr-1", true, "validated")])
    );

    // A new address listed first becomes the default.
    assert!(refresh(vec![alias(), owner()]).unwrap());
    assert_eq!(
        state(),
        (
            generation + 1,
            vec![
                row("addr-1", false, "validated"),
                row("addr-2", true, "validated")
            ]
        )
    );

    // One no longer returned keeps its row but can't send.
    assert!(refresh(vec![owner()]).unwrap());
    assert!(!refresh(vec![owner()]).unwrap());
    assert_eq!(
        state(),
        (
            generation + 2,
            vec![
                row("addr-1", true, "validated"),
                row("addr-2", false, "unavailable")
            ]
        )
    );

    // A kept address whose email changed writes too.
    let renamed = identity(
        "addr-1",
        "renamed@example.test",
        IdentitySendable::Validated,
    );
    assert!(refresh(vec![renamed]).unwrap());
    let email = reading(&store, |tx| {
        tx.query_row(
            "SELECT email FROM identities WHERE identity_id = 'addr-1'",
            [],
            |row| row.get::<_, String>(0),
        )
        .map_err(|e| crate::schema::classify(e, "read the email"))
    });
    assert_eq!(email, "renamed@example.test");
    assert_eq!(state().0, generation + 3);

    let unknown = block_on(store.write(crate::WritePriority::User, |tx| {
        refresh_proton_identities(tx, "nobody", &[])
    }));
    assert!(matches!(unknown, Err(StoreError::NotFound { .. })));
}

/// Another Proton user than the account's is `Conflict`, never merged
/// (spec/02); the stored row is unchanged.
#[test]
fn signing_in_again_as_another_proton_user_is_refused() {
    let (_scratch, store) = proton_store("proton-again-user");
    block_on(store.write(crate::WritePriority::User, |tx| {
        insert_account(tx, "a1", 1);
        insert_proton_account(tx, &new_account("a1", "user-1")).map(|_| ())
    }))
    .unwrap();

    let sign_in = NewProtonSignIn {
        proton_user_id: "user-2".to_owned(),
        ..sign_in_for("a1", "user-1")
    };
    let write = block_on(store.write(crate::WritePriority::User, move |tx| {
        commit_proton_sign_in(tx, &sign_in, now())
    }));
    expect_conflict(write);

    let account = reading(&store, |tx| proton_account(tx, "a1")).expect("the row");
    assert_eq!(account.proton_user_id, "user-1");
    assert_eq!(account.revision, 1);
    let generation: i64 = reading(&store, |tx| {
        tx.query_row(
            "SELECT config_generation FROM accounts WHERE account_id = 'a1'",
            [],
            |row| row.get(0),
        )
        .map_err(|e| crate::schema::classify(e, "read the generation"))
    });
    assert_eq!(generation, 1);
}

/// A refused session pause marks the account without touching its
/// rows; a user pause stays; a disconnect keeps every row (spec/04).
#[test]
fn a_session_refusal_pauses_and_a_disconnect_keeps_the_rows() {
    let (_scratch, store) = proton_store("proton-session-state");
    block_on(store.write(crate::WritePriority::User, |tx| {
        insert_account(tx, "a1", 1);
        insert_proton_account(tx, &new_account("a1", "user-1")).map(|_| ())
    }))
    .unwrap();

    block_on(store.write(crate::WritePriority::User, |tx| pause_for_session(tx, "a1"))).unwrap();
    let (state, reason): (String, Option<String>) = reading(&store, |tx| {
        tx.query_row(
            "SELECT state, state_reason FROM accounts WHERE account_id = 'a1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|e| crate::schema::classify(e, "read the state"))
    });
    assert_eq!(state, "needs_attention");
    assert_eq!(reason.as_deref(), Some(SESSION_REFUSED_REASON));

    // A user pause outranks the restore's refusal.
    block_on(store.write(crate::WritePriority::User, |tx| {
        tx.execute(
            "UPDATE accounts SET state = 'paused', state_reason = NULL WHERE account_id = 'a1'",
            [],
        )
        .map(|_| ())
        .map_err(|e| crate::schema::classify(e, "pause the account"))
    }))
    .unwrap();
    block_on(store.write(crate::WritePriority::User, |tx| pause_for_session(tx, "a1"))).unwrap();
    let state: String = reading(&store, |tx| {
        tx.query_row(
            "SELECT state FROM accounts WHERE account_id = 'a1'",
            [],
            |row| row.get(0),
        )
        .map_err(|e| crate::schema::classify(e, "read the state"))
    });
    assert_eq!(state, "paused");

    block_on(store.write(crate::WritePriority::User, |tx| {
        disconnect_proton_account(tx, "a1")
    }))
    .unwrap();
    let state: String = reading(&store, |tx| {
        tx.query_row(
            "SELECT state FROM accounts WHERE account_id = 'a1'",
            [],
            |row| row.get(0),
        )
        .map_err(|e| crate::schema::classify(e, "read the state"))
    });
    assert_eq!(state, "disconnected");
    assert!(reading(&store, |tx| proton_account(tx, "a1")).is_some());

    // The user's own sign-in outranks the disconnect: a credential
    // failure while signing in again still pauses the account.
    block_on(store.write(crate::WritePriority::User, |tx| {
        pause_for_proton_credential(tx, "a1", CredentialPause::StoreUnavailable)
    }))
    .unwrap();
    let (state, reason): (String, Option<String>) = reading(&store, |tx| {
        tx.query_row(
            "SELECT state, state_reason FROM accounts WHERE account_id = 'a1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|e| crate::schema::classify(e, "read the state"))
    });
    assert_eq!(state, "needs_attention");
    assert_eq!(
        reason.as_deref(),
        Some(CredentialPause::StoreUnavailable.reason())
    );

    // Another kind is refused, never disconnected here.
    block_on(store.write(crate::WritePriority::User, |tx| {
        tx.execute(
            "INSERT INTO accounts
             (account_id, kind, provider, display_name, state, sort_order, created_at)
             VALUES ('bridge', 'imap', 'proton_bridge', 'Bridge', 'active', 0, 0)",
            [],
        )
        .map(|_| ())
        .map_err(|e| crate::schema::classify(e, "insert the Bridge account"))
    }))
    .unwrap();
    let write = block_on(store.write(crate::WritePriority::User, |tx| {
        disconnect_proton_account(tx, "bridge")
    }));
    expect_conflict(write);
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

/// A refusal records its redacted diagnostics (spec/02, 10): the
/// numeric code when Proton sent one, the message with an address
/// masked, and the time. `pause_for_reason` puts the account
/// `needs_attention` with the spec/04 reason and keeps every row.
#[test]
fn a_refusal_records_redacted_diagnostics_and_pauses_with_the_reason() {
    let (_scratch, store) = proton_store("proton-refusal");
    block_on(store.write(crate::WritePriority::User, |tx| {
        insert_account(tx, "a1", 1);
        insert_proton_account(tx, &new_account("a1", "user-1")).map(|_| ())
    }))
    .unwrap();

    block_on(store.write(crate::WritePriority::User, |tx| {
        let account = record_refusal(
            tx,
            "a1",
            Some(2064),
            "Proton refused owner@example.test and other@example.test: unknown product",
            now(),
        )?;
        let json = account.diagnostics_json.expect("the diagnostics");
        assert!(json.contains("\"code\":2064"), "{json}");
        assert!(json.contains("\"v\":1"), "{json}");
        assert!(json.contains("<address>"), "{json}");
        assert!(!json.contains("owner@example.test"), "{json}");
        assert!(!json.contains("other@example.test"), "{json}");
        Ok(())
    }))
    .unwrap();

    // No code when Proton sent none (a 5xx body, an offline transport).
    block_on(store.write(crate::WritePriority::User, |tx| {
        let account = record_refusal(tx, "a1", None, "Proton isn't answering", now())?;
        let json = account.diagnostics_json.expect("the diagnostics");
        assert!(!json.contains("\"code\""), "{json}");
        assert!(json.contains("Proton isn't answering"), "{json}");
        Ok(())
    }))
    .unwrap();

    block_on(store.write(crate::WritePriority::User, |tx| {
        pause_for_reason(tx, "a1", HUMAN_VERIFICATION_REASON)
    }))
    .unwrap();
    let (state, reason): (String, Option<String>) = reading(&store, |tx| {
        tx.query_row(
            "SELECT state, state_reason FROM accounts WHERE account_id = 'a1'",
            [],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(|e| crate::schema::classify(e, "read the state"))
    });
    assert_eq!(state, "needs_attention");
    assert_eq!(reason.as_deref(), Some("Proton asks you to verify"));
    // The account's data stays: the Proton row and its diagnostics.
    assert!(proton_account_reads(&store, "a1"));
}

/// Whether the account's Proton row still reads.
fn proton_account_reads(store: &crate::Store, account_id: &str) -> bool {
    reading(store, |tx| {
        tx.query_row(
            "SELECT 1 FROM proton_accounts WHERE account_id = ?1",
            [account_id],
            |_| Ok(()),
        )
        .optional()
        .map_err(|e| crate::schema::classify(e, "read the Proton account"))
        .map(|row| row.is_some())
    })
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
