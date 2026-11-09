//! Connection inputs and states (spec/04 "Account states and health",
//! spec/10 credentials): the endpoint read, which accounts sync, and
//! connection pauses that never cross credential, user or disconnect
//! states.

use rusqlite::params;

use super::*;
use crate::accounts::{
    CredentialPause, IdentitySendable, NewBridgeAccount, NewIdentity, NewImapEndpoint,
    credential_pause, insert_bridge_account, reactivate_after_credential,
};
use crate::testing::{scratch_paths, test_seams, writing};
use crate::{Store, Timestamp, open};

fn bridge_account(account_id: &str) -> NewBridgeAccount {
    NewBridgeAccount {
        account_id: account_id.to_owned(),
        display_name: format!("Bridge {account_id}"),
        endpoint: NewImapEndpoint {
            imap_port: 1143,
            imap_security: EndpointSecurity::StartTls,
            smtp_port: 1025,
            smtp_security: EndpointSecurity::StartTls,
            username: "owner@example.org".to_owned(),
            credential_ref: format!("account/{account_id}/bridge-password"),
            trusted_cert_sha256: "ab".repeat(32),
            address_mode: None,
            capabilities_json: "{}".to_owned(),
        },
        identities: vec![NewIdentity {
            identity_id: format!("identity-{account_id}"),
            email: "owner@example.org".to_owned(),
            display_name: "Owner".to_owned(),
            is_default: true,
            sendable: IdentitySendable::Provisional,
        }],
    }
}

/// A store with Bridge accounts `a` and `b`, both with their
/// credential stored (active).
fn scratch() -> (sb_test_support::ScratchDir, Store) {
    let (scratch, paths) = scratch_paths("account-connection");
    let store = open(&paths, &test_seams(sb_test_support::real_fs())).unwrap();
    for id in ["a", "b"] {
        let account = bridge_account(id);
        writing(&store, move |tx| {
            insert_bridge_account(tx, &account, Timestamp::from_millis(1_735_689_600_000))?;
            reactivate_after_credential(tx, &account.account_id)
        });
    }
    (scratch, store)
}

/// `(state, state_reason, revision)` of an account.
fn state(store: &Store, account: &str) -> (String, Option<String>, i64) {
    let account = account.to_owned();
    writing(store, move |tx| {
        Ok(tx
            .query_row(
                "SELECT state, state_reason, revision FROM accounts WHERE account_id = ?1",
                params![account],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap())
    })
}

fn pause(store: &Store, account: &str, pause: ConnectionPause) -> Result<(), StoreError> {
    let account = account.to_owned();
    sb_test_support::block_on(store.write(crate::WritePriority::User, move |tx| {
        pause_for_connection(tx, &account, pause)
    }))
}

fn clear(store: &Store, account: &str) -> bool {
    let account = account.to_owned();
    writing(store, move |tx| clear_connection_pause(tx, &account))
}

#[test]
fn connection_reasons_are_frozen_and_distinct_from_credential_ones() {
    let pinned = [
        (ConnectionPause::BridgeNotRunning, "Bridge isn't running"),
        (
            ConnectionPause::LoginRejected,
            "Bridge password changed or account signed out in Bridge",
        ),
        (
            ConnectionPause::CertificateChanged,
            "Bridge's certificate changed",
        ),
    ];
    assert_eq!(pinned.len(), ConnectionPause::ALL.len());
    for (pause, text) in pinned {
        assert_eq!(pause.reason(), text);
        assert_eq!(ConnectionPause::from_reason(text), Some(pause));
    }
    assert_eq!(
        ConnectionPause::from_reason(CredentialPause::NotStored.reason()),
        None
    );
}

#[test]
fn the_endpoint_reads_what_setup_stored() {
    let (_scratch, store) = scratch();
    let endpoint = writing(&store, |tx| imap_endpoint(tx, "a"));
    assert_eq!(
        endpoint,
        ImapEndpoint {
            provider: ImapProvider::ProtonBridge,
            port: 1143,
            security: EndpointSecurity::StartTls,
            username: "owner@example.org".to_owned(),
            credential_ref: "account/a/bridge-password".to_owned(),
            trusted_cert_sha256: Some("ab".repeat(32)),
        }
    );
    let local: String = writing(&store, |tx| {
        Ok(tx
            .query_row(
                "SELECT account_id FROM accounts WHERE kind = 'local'",
                [],
                |row| row.get(0),
            )
            .unwrap())
    });
    let missing = sb_test_support::block_on(store.write(crate::WritePriority::User, move |tx| {
        imap_endpoint(tx, &local)
    }));
    assert!(
        matches!(missing, Err(StoreError::NotFound { .. })),
        "{missing:?}"
    );
}

#[test]
fn only_active_or_connection_paused_imap_accounts_sync() {
    let (_scratch, store) = scratch();
    assert_eq!(writing(&store, syncable_accounts), vec!["a", "b"]);

    // A connection pause keeps its worker, which keeps retrying.
    pause(&store, "a", ConnectionPause::BridgeNotRunning).unwrap();
    assert_eq!(writing(&store, syncable_accounts), vec!["a", "b"]);

    // A credential pause, a user pause and a new account waiting for
    // its credential have none.
    writing(&store, |tx| {
        crate::accounts::pause_for_credential(tx, "b", CredentialPause::StoreUnavailable)
    });
    assert_eq!(writing(&store, syncable_accounts), vec!["a"]);
    writing(&store, |tx| {
        tx.execute(
            "UPDATE accounts SET state = 'paused', state_reason = NULL WHERE account_id = 'a'",
            [],
        )
        .unwrap();
        Ok(())
    });
    assert!(writing(&store, syncable_accounts).is_empty());
}

#[test]
fn a_connection_pause_is_recorded_once_and_lifted_by_a_connect() {
    let (_scratch, store) = scratch();
    let (_, _, before) = state(&store, "a");

    pause(&store, "a", ConnectionPause::BridgeNotRunning).unwrap();
    let paused = state(&store, "a");
    assert_eq!(
        (paused.0.as_str(), paused.1.as_deref()),
        ("needs_attention", Some("Bridge isn't running"))
    );
    assert_eq!(paused.2, before + 1);

    // Retries repeat the same pause: nothing is written.
    pause(&store, "a", ConnectionPause::BridgeNotRunning).unwrap();
    assert_eq!(state(&store, "a").2, before + 1);

    // Bridge answered but refused the login: the reason moves on.
    pause(&store, "a", ConnectionPause::LoginRejected).unwrap();
    assert_eq!(
        state(&store, "a").1.as_deref(),
        Some("Bridge password changed or account signed out in Bridge")
    );

    assert!(clear(&store, "a"), "a connect lifts it");
    let cleared = state(&store, "a");
    assert_eq!((cleared.0.as_str(), cleared.1), ("active", None));
    assert!(!clear(&store, "a"), "an active account is left as it is");
}

#[test]
fn connection_transitions_never_cross_other_states() {
    let (_scratch, store) = scratch();

    // A credential pause stays: neither a connection pause nor a
    // connect replaces it (spec/10's retry lifts it).
    writing(&store, |tx| {
        crate::accounts::pause_for_credential(tx, "a", CredentialPause::StoreRefused)
    });
    assert!(matches!(
        pause(&store, "a", ConnectionPause::BridgeNotRunning),
        Err(StoreError::Conflict { .. })
    ));
    assert!(!clear(&store, "a"));
    assert_eq!(
        writing(&store, |tx| credential_pause(tx, "a")),
        Some(CredentialPause::StoreRefused)
    );

    // A user pause or a disconnect stays too.
    for user_state in ["paused", "disconnected"] {
        writing(&store, move |tx| {
            tx.execute(
                "UPDATE accounts SET state = ?1, state_reason = NULL WHERE account_id = 'b'",
                params![user_state],
            )
            .unwrap();
            Ok(())
        });
        assert!(matches!(
            pause(&store, "b", ConnectionPause::CertificateChanged),
            Err(StoreError::Conflict { .. })
        ));
        assert!(!clear(&store, "b"));
        assert_eq!(state(&store, "b").0, user_state);
    }

    assert!(matches!(
        pause(&store, "missing", ConnectionPause::BridgeNotRunning),
        Err(StoreError::NotFound { .. })
    ));
}

#[test]
fn a_non_imap_account_takes_no_connection_transition() {
    let (_scratch, store) = scratch();
    let local: String = writing(&store, |tx| {
        Ok(tx
            .query_row(
                "SELECT account_id FROM accounts WHERE kind = 'local'",
                [],
                |row| row.get(0),
            )
            .unwrap())
    });
    let before = state(&store, &local);
    assert_eq!(before.0, "active");

    assert!(matches!(
        pause(&store, &local, ConnectionPause::BridgeNotRunning),
        Err(StoreError::Conflict { .. })
    ));
    assert_eq!(state(&store, &local), before);

    // Even a connection reason written onto it is not one to lift.
    let id = local.clone();
    writing(&store, move |tx| {
        tx.execute(
            "UPDATE accounts SET state = 'needs_attention', state_reason = ?2
             WHERE account_id = ?1",
            params![id, ConnectionPause::BridgeNotRunning.reason()],
        )
        .unwrap();
        Ok(())
    });
    assert!(!clear(&store, &local));
    assert_eq!(state(&store, &local).0, "needs_attention");
}

#[test]
fn a_credential_pause_never_replaces_a_user_pause_or_disconnect() {
    let (_scratch, store) = scratch();
    for user_state in ["paused", "disconnected"] {
        writing(&store, move |tx| {
            tx.execute(
                "UPDATE accounts SET state = ?1, state_reason = NULL WHERE account_id = 'b'",
                params![user_state],
            )
            .unwrap();
            Ok(())
        });
        let before = state(&store, "b");
        let refused = sb_test_support::block_on(store.write(crate::WritePriority::User, |tx| {
            crate::accounts::pause_for_credential(tx, "b", CredentialPause::StoreUnavailable)
        }));
        assert!(
            matches!(refused, Err(StoreError::Conflict { .. })),
            "{user_state}: {refused:?}"
        );
        assert_eq!(state(&store, "b"), before, "{user_state}");
    }

    // A credential failure is the more basic blocker: it replaces a
    // connection pause, and the credential retry later lifts it.
    pause(&store, "a", ConnectionPause::BridgeNotRunning).unwrap();
    writing(&store, |tx| {
        crate::accounts::pause_for_credential(tx, "a", CredentialPause::StoreRefused)
    });
    assert_eq!(
        writing(&store, |tx| credential_pause(tx, "a")),
        Some(CredentialPause::StoreRefused)
    );
}
