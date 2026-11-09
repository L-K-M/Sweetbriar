//! Mailbox discovery and the epoch check (spec/04 steps 1 and 2) over
//! a real store: new rows, `gone` with occurrences kept, returning
//! rows, changed roles, the generation advancing only on change, the
//! local archive refused, epoch adoption and changes, and the
//! schema's refusal of an occurrence that mixes two accounts (plan
//! M2.4 acceptance).

use rusqlite::params;

use super::*;
use crate::accounts::{
    EndpointSecurity, IdentitySendable, NewBridgeAccount, NewIdentity, NewImapEndpoint,
    insert_bridge_account,
};
use crate::testing::{scratch_paths, test_seams, writing};
use crate::{Store, Timestamp, open};

const NOW_MS: i64 = 1_735_689_600_000;

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
            trusted_cert_sha256: "00".repeat(32),
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

/// A store with two Bridge accounts, `a` and `b`.
fn scratch() -> (sb_test_support::ScratchDir, Store) {
    let (scratch, paths) = scratch_paths("sync-mailboxes");
    let store = open(&paths, &test_seams(sb_test_support::real_fs())).unwrap();
    for id in ["a", "b"] {
        let account = bridge_account(id);
        writing(&store, move |tx| {
            insert_bridge_account(tx, &account, Timestamp::from_millis(NOW_MS))
        });
    }
    (scratch, store)
}

fn mailbox(name: &str, kind: MailboxKind, role: Option<MailboxRole>) -> ObservedMailbox {
    ObservedMailbox {
        remote_name: name.to_owned(),
        delimiter: Some('/'),
        kind,
        role,
        selectable: true,
    }
}

fn bridge_listing() -> Vec<ObservedMailbox> {
    vec![
        mailbox("INBOX", MailboxKind::Folder, Some(MailboxRole::Inbox)),
        mailbox("Sent", MailboxKind::Folder, Some(MailboxRole::Sent)),
        mailbox("All Mail", MailboxKind::Virtual, Some(MailboxRole::All)),
        mailbox("Labels/Work", MailboxKind::Label, None),
        ObservedMailbox {
            selectable: false,
            ..mailbox("Labels", MailboxKind::Label, None)
        },
    ]
}

fn reconcile(store: &Store, account: &str, listing: Vec<ObservedMailbox>) -> Reconciled {
    let account = account.to_owned();
    writing(store, move |tx| reconcile_mailboxes(tx, &account, &listing))
}

/// `(remote_name, kind, role, selectable, sync_state)` for `account`.
fn rows(store: &Store, account: &str) -> Vec<(String, String, Option<String>, bool, String)> {
    let account = account.to_owned();
    writing(store, move |tx| {
        let mut statement = tx
            .prepare(
                "SELECT remote_name, kind, role, selectable, sync_state FROM mailboxes
                 WHERE account_id = ?1 ORDER BY remote_name",
            )
            .unwrap();
        let rows = statement
            .query_map(params![account], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        Ok(rows)
    })
}

fn generation(store: &Store) -> u64 {
    writing(store, |tx| crate::list_rows::generation(tx))
}

fn mailbox_id(store: &Store, account: &str, name: &str) -> i64 {
    let (account, name) = (account.to_owned(), name.to_owned());
    writing(store, move |tx| {
        Ok(tx
            .query_row(
                "SELECT mailbox_id FROM mailboxes WHERE account_id = ?1 AND remote_name = ?2",
                params![account, name],
                |row| row.get(0),
            )
            .unwrap())
    })
}

/// A present occurrence of a new logical message of `message_account`
/// in `mailbox_id`, recorded as `occurrence_account`.
fn insert_occurrence(
    store: &Store,
    occurrence_account: &str,
    message_account: &str,
    mailbox_id: i64,
) -> Result<(), StoreError> {
    let (occurrence_account, message_account) =
        (occurrence_account.to_owned(), message_account.to_owned());
    sb_test_support::block_on(store.write(crate::WritePriority::User, move |tx| {
        tx.execute(
            "INSERT INTO logical_messages (account_id, created_at) VALUES (?1, ?2)",
            params![message_account, NOW_MS],
        )
        .map_err(|e| classify(e, "insert a test message"))?;
        let message_id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO occurrences
             (account_id, mailbox_id, uidvalidity, uid, message_id, state,
              first_seen_at, last_seen_at)
             VALUES (?1, ?2, 7, 1, ?3, 'present', ?4, ?4)",
            params![occurrence_account, mailbox_id, message_id, NOW_MS],
        )
        .map_err(|e| classify(e, "insert a test occurrence"))?;
        Ok(())
    }))
}

#[test]
fn a_first_listing_inserts_every_mailbox_new() {
    let (_scratch, store) = scratch();
    let before = generation(&store);

    let reconciled = reconcile(&store, "a", bridge_listing());

    assert_eq!(reconciled.added.len(), 5);
    assert!(reconciled.changed.is_empty() && reconciled.gone.is_empty());
    assert_eq!(
        rows(&store, "a"),
        vec![
            (
                "All Mail".into(),
                "virtual".into(),
                Some("all".into()),
                true,
                "new".into()
            ),
            (
                "INBOX".into(),
                "folder".into(),
                Some("inbox".into()),
                true,
                "new".into()
            ),
            ("Labels".into(), "label".into(), None, false, "new".into()),
            (
                "Labels/Work".into(),
                "label".into(),
                None,
                true,
                "new".into()
            ),
            (
                "Sent".into(),
                "folder".into(),
                Some("sent".into()),
                true,
                "new".into()
            ),
        ]
    );
    assert!(rows(&store, "b").is_empty(), "another account is untouched");
    assert_eq!(generation(&store), before + 1, "the sidebar re-reads");
}

#[test]
fn an_unchanged_listing_writes_nothing() {
    let (_scratch, store) = scratch();
    reconcile(&store, "a", bridge_listing());
    let before = generation(&store);

    let reconciled = reconcile(&store, "a", bridge_listing());

    assert!(reconciled.is_empty());
    assert_eq!(generation(&store), before);
}

#[test]
fn a_missing_mailbox_goes_gone_with_its_occurrences_and_returns() {
    let (_scratch, store) = scratch();
    reconcile(&store, "a", bridge_listing());
    let work = mailbox_id(&store, "a", "Labels/Work");
    insert_occurrence(&store, "a", "a", work).unwrap();

    let without_work: Vec<_> = bridge_listing()
        .into_iter()
        .filter(|mailbox| mailbox.remote_name != "Labels/Work")
        .collect();
    let reconciled = reconcile(&store, "a", without_work);
    assert_eq!(reconciled.gone, vec![work]);
    let state = rows(&store, "a")
        .into_iter()
        .find(|row| row.0 == "Labels/Work")
        .unwrap()
        .4;
    assert_eq!(state, "gone");
    let kept: i64 = writing(&store, move |tx| {
        Ok(tx
            .query_row(
                "SELECT COUNT(*) FROM occurrences WHERE mailbox_id = ?1",
                params![work],
                |row| row.get(0),
            )
            .unwrap())
    });
    assert_eq!(kept, 1, "a gone mailbox keeps its occurrences");

    // Listed again: back to `new`, having never adopted an epoch.
    let reconciled = reconcile(&store, "a", bridge_listing());
    assert_eq!(reconciled.returned, vec![work]);
    assert!(reconciled.changed.is_empty());
    let state = rows(&store, "a")
        .into_iter()
        .find(|row| row.0 == "Labels/Work")
        .unwrap()
        .4;
    assert_eq!(state, "new");
}

#[test]
fn a_changed_role_or_selectability_is_reported() {
    let (_scratch, store) = scratch();
    reconcile(&store, "a", bridge_listing());
    let sent = mailbox_id(&store, "a", "Sent");

    let mut listing = bridge_listing();
    for mailbox in &mut listing {
        if mailbox.remote_name == "Sent" {
            mailbox.role = None;
        }
    }
    let reconciled = reconcile(&store, "a", listing);

    assert_eq!(reconciled.changed, vec![sent]);
    let role = rows(&store, "a")
        .into_iter()
        .find(|row| row.0 == "Sent")
        .unwrap()
        .2;
    assert_eq!(role, None);
}

#[test]
fn bad_listings_and_the_local_archive_are_refused() {
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
    let refused = |account: String, listing: Vec<ObservedMailbox>| {
        sb_test_support::block_on(store.write(crate::WritePriority::User, move |tx| {
            reconcile_mailboxes(tx, &account, &listing)
        }))
    };
    assert!(matches!(
        refused(local, bridge_listing()),
        Err(StoreError::InvalidInput { .. })
    ));
    assert!(matches!(
        refused("missing".into(), bridge_listing()),
        Err(StoreError::NotFound { .. })
    ));
    let mut twice = bridge_listing();
    twice.push(mailbox("INBOX", MailboxKind::Folder, None));
    assert!(matches!(
        refused("a".into(), twice),
        Err(StoreError::InvalidInput { .. })
    ));
    assert!(matches!(
        refused("a".into(), vec![mailbox("", MailboxKind::Folder, None)]),
        Err(StoreError::InvalidInput { .. })
    ));
    assert!(
        rows(&store, "a").is_empty(),
        "a refused listing writes nothing"
    );
}

#[test]
fn the_first_epoch_is_adopted_and_a_moved_one_is_reported() {
    let (_scratch, store) = scratch();
    reconcile(&store, "a", bridge_listing());
    let inbox = mailbox_id(&store, "a", "INBOX");
    let check =
        |uidvalidity: u32| writing(&store, move |tx| check_epoch(tx, "a", inbox, uidvalidity));
    let state = || {
        rows(&store, "a")
            .into_iter()
            .find(|row| row.0 == "INBOX")
            .unwrap()
            .4
    };

    assert_eq!(check(7), EpochCheck::Adopted);
    assert_eq!(state(), "backfilling");
    assert_eq!(check(7), EpochCheck::Unchanged);
    assert_eq!(check(8), EpochCheck::Changed);
    assert_eq!(state(), "epoch_changed");
    assert_eq!(
        check(8),
        EpochCheck::Changed,
        "stays changed until resolved"
    );

    let refused = sb_test_support::block_on(store.write(crate::WritePriority::User, move |tx| {
        check_epoch(tx, "b", inbox, 7)
    }));
    assert!(
        matches!(refused, Err(StoreError::NotFound { .. })),
        "another account's mailbox"
    );
    let zero = sb_test_support::block_on(store.write(crate::WritePriority::User, move |tx| {
        check_epoch(tx, "a", inbox, 0)
    }));
    assert!(matches!(zero, Err(StoreError::InvalidInput { .. })));
}

#[test]
fn a_null_epoch_with_occurrences_is_a_change() {
    // After a restore (spec/03), a mailbox may keep occurrences while
    // its epoch is NULL: spec/04 counts that as an epoch change.
    let (_scratch, store) = scratch();
    reconcile(&store, "a", bridge_listing());
    let inbox = mailbox_id(&store, "a", "INBOX");
    insert_occurrence(&store, "a", "a", inbox).unwrap();

    let check = writing(&store, move |tx| check_epoch(tx, "a", inbox, 7));

    assert_eq!(check, EpochCheck::Changed);
}

#[test]
fn an_occurrence_mixing_two_accounts_is_refused_by_the_schema() {
    // plan M2.4: schema-level cross-account rejection. The composite
    // keys tie an occurrence's account to its mailbox's and message's.
    let (_scratch, store) = scratch();
    reconcile(&store, "a", bridge_listing());
    reconcile(&store, "b", bridge_listing());
    let inbox_a = mailbox_id(&store, "a", "INBOX");

    assert!(
        insert_occurrence(&store, "a", "b", inbox_a).is_err(),
        "a message of another account"
    );
    assert!(
        insert_occurrence(&store, "b", "b", inbox_a).is_err(),
        "a mailbox of another account"
    );
    insert_occurrence(&store, "a", "a", inbox_a).expect("one account throughout");
}
