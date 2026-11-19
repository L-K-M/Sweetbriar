//! Identity store tests (spec/02 `identities`, spec/05): the writes'
//! exact rows and refusals.

use std::sync::{Arc, Mutex};

use sb_test_support::block_on;

use super::*;
use crate::testing::{reading, scratch_paths, test_seams, writing};
use crate::{Store, WritePriority, open};

fn scratch() -> (sb_test_support::ScratchDir, Store) {
    let (scratch, paths) = scratch_paths("identities");
    let store = open(&paths, &test_seams(sb_test_support::real_fs())).unwrap();
    (scratch, store)
}

/// One store write that preserves the command's typed refusal: the
/// writer's closure can only fail with `StoreError`, so the typed error
/// travels beside it and is recovered afterwards.
fn write_command<T>(
    store: &Store,
    f: impl FnOnce(&rusqlite::Transaction) -> Result<T, IdentityError> + Send + 'static,
) -> Result<T, IdentityError>
where
    T: Send + 'static,
{
    let slot = Arc::new(Mutex::new(None));
    let recorded = Arc::clone(&slot);
    let result = block_on(store.write(WritePriority::User, move |tx| match f(tx) {
        Ok(value) => Ok(value),
        Err(error) => {
            *recorded.lock().expect("identity test lock poisoned") = Some(error);
            Err(StoreError::internal("the identity command rolled back"))
        }
    }));
    match (
        result,
        slot.lock().expect("identity test lock poisoned").take(),
    ) {
        (Ok(value), None) => Ok(value),
        (Err(error), None) => Err(IdentityError::Store(error)),
        (_, Some(error)) => Err(error),
    }
}

/// Two accounts with A/B, a Proton and Bridge account, and a received
/// message whose headers name A in `Delivered-To`.
fn seed(store: &Store) {
    writing(store, |tx| {
        tx.execute_batch(
            r#"INSERT INTO accounts (account_id, kind, provider, display_name, state, sort_order, created_at) VALUES
               ('acct', 'imap', 'generic_imap', 'Work', 'active', 1, 0),
               ('other', 'imap', 'generic_imap', 'Other', 'active', 2, 0),
               ('proton', 'proton', 'proton_api', 'Proton', 'active', 3, 0),
               ('bridge', 'imap', 'proton_bridge', 'Bridge', 'active', 4, 0),
               ('another', 'imap', 'generic_imap', 'Another', 'active', 5, 0);
               INSERT INTO identities (identity_id, account_id, email, display_name, is_default, sendable) VALUES
               ('a', 'acct', 'alias@example.test', 'Alias', 0, 'provisional'),
               ('b', 'acct', 'default@example.test', 'Default', 1, 'validated'),
               ('gone', 'acct', 'gone@example.test', 'Gone', 0, 'unavailable');
               INSERT INTO mailboxes (mailbox_id, account_id, remote_name, kind, selectable, sync_state) VALUES
               (7, 'acct', 'Inbox', 'folder', 1, 'synced'),
               (8, 'other', 'Inbox', 'folder', 1, 'synced');
               INSERT INTO logical_messages (message_id, account_id, created_at) VALUES (9, 'acct', 0);
               INSERT INTO occurrences (occurrence_id, account_id, mailbox_id, message_id, first_seen_at, last_seen_at, state)
               VALUES (11, 'acct', 7, 9, 0, 0, 'present');
               INSERT INTO message_headers (message_id, raw_fields, header_message_id, from_json, to_json, cc_json,
                   delivered_to_json, fetched_at)
               VALUES (9, x'', '<sent@example.test>',
                   '[{"address":"someone@else.test","name":null}]',
                   '[{"address":"other@example.test","name":null}]',
                   '[{"address":"third@example.test","name":null}]',
                   '["alias@example.test"]', 0);
               INSERT INTO drafts (draft_id, state, current_revision_id, created_at, updated_at)
               VALUES ('d', 'sent', 'r', 0, 0);
               INSERT INTO draft_revisions (revision_id, draft_id, seq, compose_format, subject, to_json, cc_json,
                   bcc_json, body_source, attachments_json, account_id, identity_id, created_at)
               VALUES ('r', 'd', 1, 'plain', '', '[]', '[]', '[]', '', '[]', 'acct', 'a', 0);
               INSERT INTO send_attempts (attempt_id, draft_id, revision_id, account_id, config_generation,
                   identity_id, mime_sha256, message_id_header, envelope_from, state, sent_copy, created_at, updated_at)
               VALUES ('s', 'd', 'r', 'acct', 1, 'a',
                   '401a4499cbe890fe7f85964ac62bb63527c3768036ea79bf32c5e43e7ba24bd2',
                   '<sent@example.test>', 'alias@example.test', 'accepted', 'server_saves', 0, 0);"#,
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        Ok(())
    });
}

fn write(identity_id: &str, email: &str) -> IdentityWrite {
    IdentityWrite {
        identity_id: identity_id.to_owned(),
        account_id: "acct".to_owned(),
        email: email.to_owned(),
        display_name: "Name".to_owned(),
        reply_to: None,
        signature_source: None,
        sendable: IdentitySendable::Provisional,
    }
}

#[test]
fn identity_commands_leave_one_default_and_refuse_backend_accounts() {
    let (_scratch, store) = scratch();
    seed(&store);

    // Create, then update in place: one row, revision 2, defaults intact.
    write_command(&store, move |tx| {
        upsert_identity(tx, &write("c", "new@example.test"))
    })
    .unwrap();
    let mut changed = write("c", "renamed@example.test");
    changed.display_name = "Renamed".to_owned();
    changed.reply_to = Some("reply@example.test".to_owned());
    changed.signature_source = Some("**Signature**".to_owned());
    write_command(&store, move |tx| upsert_identity(tx, &changed)).unwrap();
    let row: (String, String, Option<String>, Option<String>, i64, i64) = reading(&store, |tx| {
        tx.query_row(
            "SELECT email, display_name, reply_to, signature_source, is_default, revision
             FROM identities WHERE identity_id = 'c'",
            [],
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
        .map_err(|e| StoreError::internal(e.to_string()))
    });
    assert_eq!(
        row,
        (
            "renamed@example.test".to_owned(),
            "Renamed".to_owned(),
            Some("reply@example.test".to_owned()),
            Some("**Signature**".to_owned()),
            0,
            2
        )
    );

    // Refusals: a Proton account, another account's id, a taken address
    // and a malformed address. A Bridge account's identities are the
    // user's to enter (spec/05), so it is accepted.
    let mut params = write("x", "x@example.test");
    params.account_id = "proton".to_owned();
    let error = write_command(&store, move |tx| upsert_identity(tx, &params));
    assert!(
        matches!(&error, Err(IdentityError::ProtonAccount { account_id }) if account_id == "proton"),
        "{error:?}"
    );
    let mut bridged = write("y", "y@example.test");
    bridged.account_id = "bridge".to_owned();
    write_command(&store, move |tx| upsert_identity(tx, &bridged)).expect("a Bridge identity");
    let mut foreign = write("a", "elsewhere@example.test");
    foreign.account_id = "another".to_owned();
    let error = write_command(&store, move |tx| upsert_identity(tx, &foreign));
    assert!(
        matches!(
            error,
            Err(IdentityError::Store(StoreError::Conflict { .. }))
        ),
        "{error:?}"
    );
    let taken = write("c", "default@example.test");
    let error = write_command(&store, move |tx| upsert_identity(tx, &taken));
    assert!(
        matches!(
            error,
            Err(IdentityError::Store(StoreError::Conflict { .. }))
        ),
        "{error:?}"
    );
    let bad = write("d", "not an address");
    let error = write_command(&store, move |tx| upsert_identity(tx, &bad));
    assert!(
        matches!(error, Err(IdentityError::InvalidInput { field, .. }) if field == "email"),
        "{error:?}"
    );

    // Exactly one default after each flip, and an unavailable identity
    // or an unknown id is refused.
    write_command(&store, |tx| set_default_identity(tx, "a")).unwrap();
    write_command(&store, |tx| set_default_identity(tx, "b")).unwrap();
    let defaults: i64 = reading(&store, |tx| {
        tx.query_row(
            "SELECT COUNT(*) FROM identities WHERE account_id = 'acct' AND is_default = 1",
            [],
            |row| row.get(0),
        )
        .map_err(|e| StoreError::internal(e.to_string()))
    });
    assert_eq!(defaults, 1);
    let error = write_command(&store, |tx| set_default_identity(tx, "gone"));
    assert!(
        matches!(&error, Err(IdentityError::NotSendable { identity_id, sendable }) if identity_id == "gone" && *sendable == "unavailable"),
        "{error:?}"
    );
    let missing = write_command(&store, |tx| set_default_identity(tx, "nobody"));
    assert!(
        matches!(
            missing,
            Err(IdentityError::Store(StoreError::NotFound { .. }))
        ),
        "{missing:?}"
    );
}

/// spec/02: `config_generation` moves when an identity's route fields
/// change, not for a signature edit or a rewrite of the same values; the
/// local archive has no identities.
#[test]
fn route_edits_bump_the_generation_and_the_local_archive_is_refused() {
    let (_scratch, store) = scratch();
    seed(&store);
    let generation = |store: &Store| {
        reading(store, |tx| {
            tx.query_row(
                "SELECT config_generation FROM accounts WHERE account_id = 'acct'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .map_err(|e| StoreError::internal(e.to_string()))
        })
    };
    let start = generation(&store);
    let mut signature_only = write("a", "alias@example.test");
    signature_only.display_name = "Alias".to_owned();
    signature_only.signature_source = Some("-- \nAlias".to_owned());
    // A padded name stores trimmed, so it is the same name.
    let padded = IdentityWrite {
        display_name: "  Name  ".to_owned(),
        ..write("c", "moved@example.test")
    };
    let cases: [(&str, IdentityWrite, i64); 5] = [
        ("new identity", write("c", "new@example.test"), 1),
        ("same values again", write("c", "new@example.test"), 0),
        ("signature only", signature_only, 0),
        ("address change", write("c", "moved@example.test"), 1),
        ("padded name", padded, 0),
    ];
    let mut expected = start;
    for (label, identity, bump) in cases {
        write_command(&store, move |tx| upsert_identity(tx, &identity)).expect(label);
        expected += bump;
        assert_eq!(generation(&store), expected, "{label}");
    }

    // Only a server's acceptance validates (spec/05): `b` may keep its
    // validated address, but neither a new identity nor a moved address
    // can claim the state.
    let validated = |id: &str, email: &str| IdentityWrite {
        sendable: IdentitySendable::Validated,
        ..write(id, email)
    };
    let kept = validated("b", "default@example.test");
    write_command(&store, move |tx| upsert_identity(tx, &kept)).expect("kept validated");
    for (id, email) in [("v", "v@example.test"), ("b", "moved-b@example.test")] {
        let claimed = validated(id, email);
        let refused = write_command(&store, move |tx| upsert_identity(tx, &claimed));
        assert!(
            matches!(
                refused,
                Err(IdentityError::InvalidInput {
                    field: "sendable",
                    ..
                })
            ),
            "{id}: {refused:?}"
        );
    }

    // Every store has its one local archive account (spec/02).
    let local_id = reading(&store, |tx| {
        tx.query_row(
            "SELECT account_id FROM accounts WHERE provider = 'local_archive'",
            [],
            |row| row.get::<_, String>(0),
        )
        .map_err(|e| StoreError::internal(e.to_string()))
    });
    let mut local = write("l", "archive@example.test");
    local.account_id = local_id;
    let refused = write_command(&store, move |tx| upsert_identity(tx, &local));
    assert!(
        matches!(
            refused,
            Err(IdentityError::InvalidInput {
                field: "account_id",
                ..
            })
        ),
        "{refused:?}"
    );
}

/// Re-setting the current default changes no row, so no revision
/// moves; moving it changes exactly the two rows whose flag flips.
#[test]
fn set_default_identity_moves_only_the_flags_that_change() {
    let (_scratch, store) = scratch();
    seed(&store);
    let revisions = |store: &Store| {
        reading(store, |tx| {
            tx.query_row(
                "SELECT (SELECT revision FROM identities WHERE identity_id = 'a'),
                        (SELECT revision FROM identities WHERE identity_id = 'b')",
                [],
                |row| Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?)),
            )
            .map_err(|e| StoreError::internal(e.to_string()))
        })
    };
    let (a, b) = revisions(&store);
    write_command(&store, |tx| set_default_identity(tx, "b")).unwrap();
    assert_eq!(revisions(&store), (a, b), "re-setting the default");
    write_command(&store, |tx| set_default_identity(tx, "a")).unwrap();
    assert_eq!(revisions(&store), (a + 1, b + 1), "moving the default");
}
