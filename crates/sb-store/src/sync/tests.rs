//! Mailbox discovery, the epoch check and the pass's reads (spec/04
//! steps 1 to 3) over a real store: new rows, `gone` with occurrences
//! kept, returning rows, changed roles, the generation advancing only
//! on change, the local archive refused, epoch adoption and changes,
//! the sync targets and the fetch cursor, and the schema's refusal of
//! an occurrence that mixes two accounts (plan M2.4 acceptance).

use rusqlite::params;

use super::*;
use crate::accounts::{
    EndpointSecurity, IdentitySendable, NewBridgeAccount, NewIdentity, NewImapEndpoint,
    insert_bridge_account,
};
use crate::testing::{reading, scratch_paths, test_seams, writing};
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
    let before = generation(&store);
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
    assert_eq!(
        generation(&store),
        before,
        "a refused listing advances no generation"
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
    // The server back on the stored epoch before the change was
    // resolved: no change after all, and sync resumes.
    assert_eq!(check(7), EpochCheck::Unchanged);
    assert_eq!(state(), "backfilling");

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

// Header batches (spec/04 step 3).

const EPOCH: u32 = 7;

/// Account `a` listed and its INBOX and All Mail on epoch 7.
fn syncing(store: &Store) -> (i64, i64) {
    reconcile(store, "a", bridge_listing());
    let inbox = mailbox_id(store, "a", "INBOX");
    let all = mailbox_id(store, "a", "All Mail");
    for mailbox in [inbox, all] {
        writing(store, move |tx| check_epoch(tx, "a", mailbox, EPOCH));
    }
    (inbox, all)
}

fn header(uid: u32, key: Option<&str>, subject: &str) -> SyncedHeader {
    let raw = format!(
        "Subject: {subject}\r\nDelivered-To: owner@example.org\r\nX-Pm-Internal-Id: {}\r\n\r\n",
        key.unwrap_or("-")
    );
    SyncedHeader {
        uid,
        flags: SyncedFlags::default(),
        internal_date: Some(Timestamp::from_millis(NOW_MS - i64::from(uid))),
        rfc822_size: Some(1000),
        provider_key: key.map(str::to_owned),
        raw_fields: raw.into_bytes(),
        columns: HeaderColumns {
            subject: Some(subject.to_owned()),
            from_json: Some(r#"[{"address":"sender@example.org","name":"Sender"}]"#.to_owned()),
            delivered_to_json: Some(r#"["owner@example.org"]"#.to_owned()),
            ..HeaderColumns::default()
        },
    }
}

fn commit(
    store: &Store,
    mailbox: i64,
    expected: Option<u32>,
    next: u32,
    headers: Vec<SyncedHeader>,
) -> Result<HeaderBatchStats, StoreError> {
    sb_test_support::block_on(store.write(crate::WritePriority::Sync, move |tx| {
        commit_header_batch(
            tx,
            &HeaderBatch {
                account_id: "a",
                mailbox_id: mailbox,
                uidvalidity: EPOCH,
                expected_uidnext: expected,
                next_uidnext: next,
                caught_up: true,
                headers: &headers,
            },
            Timestamp::from_millis(NOW_MS),
        )
    }))
}

/// `(uid, state, provider_key)` of a mailbox's occurrences by id.
fn occurrences(store: &Store, mailbox: i64) -> Vec<(i64, String, Option<String>)> {
    writing(store, move |tx| {
        let mut statement = tx
            .prepare(
                "SELECT o.uid, o.state, lm.provider_key FROM occurrences o
                 JOIN logical_messages lm ON lm.message_id = o.message_id
                 WHERE o.mailbox_id = ?1 ORDER BY o.occurrence_id",
            )
            .unwrap();
        let rows = statement
            .query_map(params![mailbox], |row| {
                Ok((row.get(0)?, row.get(1)?, row.get(2)?))
            })
            .unwrap()
            .map(Result::unwrap)
            .collect();
        Ok(rows)
    })
}

fn checkpoint(store: &Store, mailbox: i64) -> (Option<i64>, String) {
    writing(store, move |tx| {
        Ok(tx
            .query_row(
                "SELECT uidnext, sync_state FROM mailboxes WHERE mailbox_id = ?1",
                params![mailbox],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap())
    })
}

fn count(store: &Store, sql: &'static str) -> i64 {
    writing(store, move |tx| {
        Ok(tx.query_row(sql, [], |row| row.get(0)).unwrap())
    })
}

#[test]
fn a_batch_lands_headers_occurrences_and_its_checkpoint_together() {
    let (_scratch, store) = scratch();
    let (inbox, _) = syncing(&store);
    let before = generation(&store);

    let stats = commit(
        &store,
        inbox,
        None,
        3,
        vec![
            header(1, Some("pm-1"), "First"),
            header(2, Some("pm-2"), "Second"),
        ],
    )
    .unwrap();

    assert_eq!(
        stats,
        HeaderBatchStats {
            inserted: 2,
            refreshed: 0,
            superseded: 0,
            new_messages: 2
        }
    );
    assert_eq!(checkpoint(&store, inbox), (Some(3), "synced".to_owned()));
    let (raw, delivered): (Vec<u8>, String) = writing(&store, |tx| {
        Ok(tx
            .query_row(
                "SELECT h.raw_fields, h.delivered_to_json FROM message_headers h
                 JOIN logical_messages lm ON lm.message_id = h.message_id
                 WHERE lm.provider_key = 'pm-1'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap())
    });
    assert!(
        String::from_utf8(raw)
            .unwrap()
            .starts_with("Subject: First")
    );
    assert_eq!(delivered, r#"["owner@example.org"]"#);
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM d_list_rows WHERE subject IN ('First', 'Second')"
        ),
        2,
        "the lists show the header-only rows"
    );
    assert!(generation(&store) > before);
}

#[test]
fn one_provider_key_groups_occurrences_across_mailboxes() {
    let (_scratch, store) = scratch();
    let (inbox, all) = syncing(&store);

    commit(
        &store,
        inbox,
        None,
        2,
        vec![header(1, Some("pm-1"), "Shared")],
    )
    .unwrap();
    let stats = commit(
        &store,
        all,
        None,
        10,
        vec![header(9, Some("pm-1"), "Shared")],
    )
    .unwrap();

    assert_eq!(stats.new_messages, 0, "the same Proton message");
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM logical_messages WHERE provider_key = 'pm-1'"
        ),
        1
    );
    assert_eq!(count(&store, "SELECT COUNT(*) FROM message_headers"), 1);
    assert_eq!(occurrences(&store, all).len(), 1);
}

#[test]
fn a_reused_uid_supersedes_the_old_binding() {
    // plan M2.4: UID reuse marks the old occurrence superseded and
    // inserts the new one.
    let (_scratch, store) = scratch();
    let (inbox, _) = syncing(&store);
    commit(&store, inbox, None, 2, vec![header(1, Some("pm-1"), "Old")]).unwrap();

    let stats = commit(
        &store,
        inbox,
        Some(2),
        2,
        vec![header(1, Some("pm-9"), "New")],
    )
    .unwrap();

    assert_eq!((stats.superseded, stats.inserted), (1, 1));
    assert_eq!(
        occurrences(&store, inbox),
        vec![
            (1, "superseded".to_owned(), Some("pm-1".to_owned())),
            (1, "present".to_owned(), Some("pm-9".to_owned())),
        ]
    );
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM d_list_rows WHERE subject = 'Old'"
        ),
        0,
        "the superseded row leaves the lists"
    );
}

#[test]
fn the_same_message_at_its_locator_only_refreshes_flags() {
    let (_scratch, store) = scratch();
    let (inbox, _) = syncing(&store);
    commit(
        &store,
        inbox,
        None,
        2,
        vec![header(1, Some("pm-1"), "Mail")],
    )
    .unwrap();

    let mut seen = header(1, Some("pm-1"), "Mail");
    seen.flags = SyncedFlags {
        seen: true,
        keywords: vec![
            "$Forwarded".to_owned(),
            "$Archived".to_owned(),
            "$Forwarded".to_owned(),
        ],
        ..SyncedFlags::default()
    };
    let stats = commit(&store, inbox, Some(2), 2, vec![seen]).unwrap();

    assert_eq!(
        (stats.refreshed, stats.inserted, stats.new_messages),
        (1, 0, 0)
    );
    let (f_seen, keywords): (bool, String) = writing(&store, move |tx| {
        Ok(tx
            .query_row(
                "SELECT f_seen, keywords FROM occurrences WHERE mailbox_id = ?1",
                params![inbox],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap())
    });
    assert!(f_seen);
    assert_eq!(
        keywords, r#"["$Archived","$Forwarded"]"#,
        "sorted and deduplicated"
    );
}

#[test]
fn headers_without_a_provider_key_group_nothing() {
    let (_scratch, store) = scratch();
    let (inbox, _) = syncing(&store);

    let stats = commit(
        &store,
        inbox,
        None,
        3,
        vec![header(1, None, "A"), header(2, None, "B")],
    )
    .unwrap();

    assert_eq!(stats.new_messages, 2);
}

#[test]
fn a_stale_or_misplaced_batch_is_refused_and_writes_nothing() {
    let (_scratch, store) = scratch();
    let (inbox, _) = syncing(&store);
    commit(
        &store,
        inbox,
        None,
        2,
        vec![header(1, Some("pm-1"), "Mail")],
    )
    .unwrap();

    // The checkpoint moved to 2 since this fetch was planned at NULL.
    let stale = commit(
        &store,
        inbox,
        None,
        3,
        vec![header(2, Some("pm-2"), "Late")],
    );
    assert!(
        matches!(stale, Err(StoreError::Conflict { .. })),
        "{stale:?}"
    );

    // A listed mailbox whose epoch was never adopted is not syncing.
    let sent = mailbox_id(&store, "a", "Sent");
    let unsynced = commit(&store, sent, None, 2, vec![header(1, Some("pm-3"), "Sent")]);
    assert!(
        matches!(unsynced, Err(StoreError::Conflict { .. })),
        "{unsynced:?}"
    );

    // Another epoch's headers.
    let epoch = sb_test_support::block_on(store.write(crate::WritePriority::Sync, move |tx| {
        commit_header_batch(
            tx,
            &HeaderBatch {
                account_id: "a",
                mailbox_id: inbox,
                uidvalidity: EPOCH + 1,
                expected_uidnext: Some(2),
                next_uidnext: 3,
                caught_up: true,
                headers: &[header(2, Some("pm-2"), "Other epoch")],
            },
            Timestamp::from_millis(NOW_MS),
        )
    }));
    assert!(
        matches!(epoch, Err(StoreError::Conflict { .. })),
        "{epoch:?}"
    );

    assert_eq!(checkpoint(&store, inbox), (Some(2), "synced".to_owned()));
    assert_eq!(occurrences(&store, inbox).len(), 1);
}

#[test]
fn a_failure_mid_batch_rolls_the_whole_batch_back() {
    // plan M2.4: a cut batch resumes without gaps or duplicates. The
    // second header fails after the first was written; the
    // transaction leaves neither it nor the checkpoint.
    let (_scratch, store) = scratch();
    let (inbox, _) = syncing(&store);
    let mut broken = header(2, Some("pm-2"), "Broken");
    broken.rfc822_size = Some(u64::MAX);

    let failed = commit(
        &store,
        inbox,
        None,
        3,
        vec![header(1, Some("pm-1"), "Fine"), broken],
    );

    assert!(
        matches!(failed, Err(StoreError::InvalidInput { .. })),
        "{failed:?}"
    );
    assert!(occurrences(&store, inbox).is_empty());
    assert_eq!(
        count(
            &store,
            "SELECT COUNT(*) FROM logical_messages WHERE provider_key IS NOT NULL"
        ),
        0
    );
    assert_eq!(checkpoint(&store, inbox), (None, "backfilling".to_owned()));

    // The retried batch then lands once.
    commit(
        &store,
        inbox,
        None,
        3,
        vec![
            header(1, Some("pm-1"), "Fine"),
            header(2, Some("pm-2"), "Fixed"),
        ],
    )
    .unwrap();
    assert_eq!(occurrences(&store, inbox).len(), 2);
}

#[test]
fn malformed_batches_are_refused_before_any_write() {
    let (_scratch, store) = scratch();
    let (inbox, _) = syncing(&store);
    let refused = |headers: Vec<SyncedHeader>, next: u32| {
        matches!(
            commit(&store, inbox, None, next, headers),
            Err(StoreError::InvalidInput { .. })
        )
    };

    let too_many: Vec<_> = (1..=251).map(|uid| header(uid, None, "Bulk")).collect();
    assert!(refused(too_many, 300), "more than 250 headers");
    assert!(refused(vec![header(0, None, "Zero")], 2), "UID 0");
    assert!(
        refused(vec![header(1, None, "A"), header(1, None, "B")], 2),
        "a repeated UID"
    );
    assert!(
        refused(vec![header(5, None, "High")], 5),
        "a UID at the checkpoint"
    );
    let mut huge = header(1, None, "Huge");
    huge.raw_fields = vec![b'x'; MAX_HEADER_BATCH_BYTES];
    assert!(refused(vec![huge], 2), "over the write budget");
    assert!(occurrences(&store, inbox).is_empty());
}

#[test]
fn a_backfill_below_the_checkpoint_keeps_it_and_it_never_moves_back() {
    // spec/04: the fetch also backfills UIDs below `uidnext` that have
    // no occurrence; the checkpoint stays where it is.
    let (_scratch, store) = scratch();
    let (inbox, _) = syncing(&store);
    commit(
        &store,
        inbox,
        None,
        5,
        vec![header(4, Some("pm-4"), "Four")],
    )
    .unwrap();

    commit(
        &store,
        inbox,
        Some(5),
        5,
        vec![header(2, Some("pm-2"), "Late")],
    )
    .unwrap();
    assert_eq!(checkpoint(&store, inbox).0, Some(5));
    assert_eq!(occurrences(&store, inbox).len(), 2);

    let back = commit(
        &store,
        inbox,
        Some(5),
        3,
        vec![header(1, Some("pm-1"), "Back")],
    );
    assert!(
        matches!(back, Err(StoreError::InvalidInput { .. })),
        "{back:?}"
    );
    assert_eq!(checkpoint(&store, inbox).0, Some(5));
}

// The pass's reads (spec/04 steps 2 and 3): which mailboxes a pass
// works through, and one mailbox's cursor for the header fetch.

/// One occurrence of `mailbox_id` on account `a`, of any epoch, UID
/// and state — the rows `insert_occurrence` cannot write.
fn occurrence(store: &Store, mailbox_id: i64, uidvalidity: u32, uid: u32, state: &str) {
    let state = state.to_owned();
    sb_test_support::block_on(store.write(crate::WritePriority::User, move |tx| {
        tx.execute(
            "INSERT INTO logical_messages (account_id, created_at) VALUES ('a', ?1)",
            params![NOW_MS],
        )
        .map_err(|e| classify(e, "insert a test message"))?;
        let message_id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO occurrences
             (account_id, mailbox_id, uidvalidity, uid, message_id, state,
              first_seen_at, last_seen_at)
             VALUES ('a', ?1, ?2, ?3, ?4, ?5, ?6, ?6)",
            params![
                mailbox_id,
                i64::from(uidvalidity),
                i64::from(uid),
                message_id,
                state,
                NOW_MS
            ],
        )
        .map_err(|e| classify(e, "insert a test occurrence"))?;
        Ok(())
    }))
    .unwrap();
}

fn targets(store: &Store, account: &str) -> Vec<SyncTarget> {
    let account = account.to_owned();
    reading(store, move |tx| sync_targets(tx, &account))
}

fn cursor(store: &Store, account: &str, mailbox: i64, uidvalidity: u32) -> MailboxCursor {
    let account = account.to_owned();
    reading(store, move |tx| {
        mailbox_cursor(tx, &account, mailbox, uidvalidity)
    })
}

#[test]
fn the_pass_targets_the_inbox_first_then_each_selectable_by_name() {
    // spec/04: "Sync every selectable mailbox", the pass starting at
    // the INBOX. "Labels" is unselectable and never a target; a `gone`
    // mailbox is none either.
    let (_scratch, store) = scratch();
    reconcile(&store, "a", bridge_listing());

    let names = |store: &Store| {
        targets(store, "a")
            .into_iter()
            .map(|target| target.remote_name)
            .collect::<Vec<_>>()
    };
    assert_eq!(names(&store), ["INBOX", "All Mail", "Labels/Work", "Sent"]);
    for target in targets(&store, "a") {
        assert_eq!(
            target.mailbox_id,
            mailbox_id(&store, "a", &target.remote_name)
        );
    }

    // A mailbox missing from the next LIST is `gone` and leaves the pass.
    let without_sent: Vec<_> = bridge_listing()
        .into_iter()
        .filter(|mailbox| mailbox.remote_name != "Sent")
        .collect();
    reconcile(&store, "a", without_sent);
    assert_eq!(names(&store), ["INBOX", "All Mail", "Labels/Work"]);
}

#[test]
fn the_pass_refuses_a_non_imap_or_unknown_account() {
    let (_scratch, store) = scratch();
    let local: String = reading(&store, |tx| {
        Ok(tx
            .query_row(
                "SELECT account_id FROM accounts WHERE kind = 'local'",
                [],
                |row| row.get(0),
            )
            .unwrap())
    });
    let refused = |account: String| {
        sb_test_support::block_on(store.write(crate::WritePriority::User, move |tx| {
            sync_targets(tx, &account)
        }))
    };

    assert!(matches!(
        refused(local),
        Err(StoreError::InvalidInput { .. })
    ));
    assert!(matches!(
        refused("missing".into()),
        Err(StoreError::NotFound { .. })
    ));
}

#[test]
fn a_fresh_mailbox_has_an_empty_unsynced_cursor() {
    let (_scratch, store) = scratch();
    reconcile(&store, "a", bridge_listing());
    let inbox = mailbox_id(&store, "a", "INBOX");

    let cursor = cursor(&store, "a", inbox, EPOCH);

    assert_eq!(
        cursor,
        MailboxCursor {
            uidnext: None,
            synced: false,
            current_uids: vec![],
        }
    );
}

#[test]
fn a_committed_batch_moves_the_cursor() {
    let (_scratch, store) = scratch();
    let (inbox, _) = syncing(&store);
    commit(
        &store,
        inbox,
        None,
        3,
        vec![
            header(2, Some("pm-2"), "Second"),
            header(1, Some("pm-1"), "First"),
        ],
    )
    .unwrap();

    let cursor = cursor(&store, "a", inbox, EPOCH);

    assert_eq!(
        cursor,
        MailboxCursor {
            uidnext: Some(3),
            synced: true,
            current_uids: vec![1, 2],
        }
    );
}

#[test]
fn the_cursor_holds_only_the_epochs_current_uids() {
    // The current set is `present` and `vanish_pending` of the
    // mailbox's epoch (the occ_current_locator index's): another
    // epoch's and the non-current UIDs stay out.
    let (_scratch, store) = scratch();
    let (inbox, _) = syncing(&store);
    commit(&store, inbox, None, 2, vec![header(1, Some("pm-1"), "Old")]).unwrap();
    // UID reuse: the first binding is superseded, the second current.
    commit(
        &store,
        inbox,
        Some(2),
        2,
        vec![header(1, Some("pm-9"), "New")],
    )
    .unwrap();
    occurrence(&store, inbox, EPOCH, 9, "vanish_pending");
    occurrence(&store, inbox, EPOCH + 1, 5, "present");
    occurrence(&store, inbox, EPOCH, 6, "vanished");
    occurrence(&store, inbox, EPOCH, 7, "epoch_stale");

    let cursor = cursor(&store, "a", inbox, EPOCH);

    assert_eq!(
        cursor,
        MailboxCursor {
            uidnext: Some(2),
            synced: true,
            current_uids: vec![1, 9],
        }
    );
}

#[test]
fn the_cursor_is_scoped_to_the_account_and_a_real_epoch() {
    let (_scratch, store) = scratch();
    let (inbox, _) = syncing(&store);

    let other = sb_test_support::block_on(store.write(crate::WritePriority::User, move |tx| {
        mailbox_cursor(tx, "b", inbox, EPOCH)
    }));
    assert!(
        matches!(other, Err(StoreError::NotFound { .. })),
        "another account's mailbox: {other:?}"
    );
    let zero = sb_test_support::block_on(store.write(crate::WritePriority::User, move |tx| {
        mailbox_cursor(tx, "a", inbox, 0)
    }));
    assert!(
        matches!(zero, Err(StoreError::InvalidInput { .. })),
        "{zero:?}"
    );
    // Another epoch than the stored one: its UIDs would pair with this
    // epoch's checkpoint.
    let stale = sb_test_support::block_on(store.write(crate::WritePriority::User, move |tx| {
        mailbox_cursor(tx, "a", inbox, EPOCH + 1)
    }));
    assert!(
        matches!(stale, Err(StoreError::InvalidInput { .. })),
        "{stale:?}"
    );
}
