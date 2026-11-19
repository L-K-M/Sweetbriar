//! Send store tests: the `held` queue insert's exact rows, the
//! recipient canonicalization, the conflict boundaries, `cancel_send`
//! and the route snapshot's schema refusal.

use std::io::Cursor;
use std::sync::Arc;

use rusqlite::Connection;
use sb_test_support::block_on;

use super::*;
use crate::drafts::DraftRecipient;
use crate::objects::ObjectStore;
use crate::testing::{reading, scratch_paths, test_seams, writing};
use crate::{Store, StoreError, StorePaths, Timestamp, WritePriority, open};

fn scratch() -> (sb_test_support::ScratchDir, StorePaths, Store) {
    let (scratch, paths) = scratch_paths("send");
    let store = open(&paths, &test_seams(sb_test_support::real_fs())).unwrap();
    (scratch, paths, store)
}

fn now() -> Timestamp {
    Timestamp::from_millis(crate::testing::FIXED_NOW_MS)
}

/// A Proton account, one identity and a routed revision whose To is
/// `a@X.NET`, Cc `a@x.net` and Bcc `b@Y.NET`.
fn seed(store: &Store) {
    writing(store, |tx| {
        tx.execute_batch(
            r#"INSERT INTO accounts
             (account_id, kind, provider, display_name, state, sort_order, created_at)
             VALUES ('acct', 'proton', 'proton_api', 'P', 'active', 1, 0);
             INSERT INTO identities
             (identity_id, account_id, email, display_name, is_default, sendable)
             VALUES ('id', 'acct', 'me@example.test', 'Me', 1, 'validated');
             INSERT INTO drafts (draft_id, state, current_revision_id, created_at, updated_at)
             VALUES ('d1', 'editing', 'r1', 0, 0);
             INSERT INTO draft_revisions
             (revision_id, draft_id, seq, account_id, identity_id, compose_format, plain_flowed,
              subject, to_json, cc_json, bcc_json, body_source, attachments_json, created_at)
             VALUES ('r1', 'd1', 1, 'acct', 'id', 'plain', 1, 's',
                     '[{"address":"a@X.NET","name":"A"}]',
                     '[{"address":"a@x.net","name":null}]',
                     '[{"address":"b@Y.NET","name":null}]', 'b', '[]', 0);"#,
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        Ok(())
    });
}

/// An attempt over the seed revision, with the seed recipients.
fn attempt(object: &PublishedObject) -> SendAttempt {
    SendAttempt {
        attempt_id: "a1".to_owned(),
        draft_id: "d1".to_owned(),
        revision_id: "r1".to_owned(),
        account_id: "acct".to_owned(),
        identity_id: "id".to_owned(),
        mime_sha256: object.id().sha256_hex(),
        message_id_header: "m@example.test".to_owned(),
        envelope_from: "me@example.test".to_owned(),
        to: vec![DraftRecipient {
            address: "a@X.NET".to_owned(),
            name: Some("A".to_owned()),
        }],
        cc: vec![DraftRecipient {
            address: "a@x.net".to_owned(),
            name: None,
        }],
        bcc: vec![DraftRecipient {
            address: "b@Y.NET".to_owned(),
            name: None,
        }],
    }
}

fn publish(store: &Store) -> Arc<PublishedObject> {
    Arc::new(
        store
            .objects()
            .publish(Cursor::new(b"frozen mime"))
            .unwrap(),
    )
}

/// Add a second revision over the seed one and make it current.
fn save_second_revision(store: &Store) {
    writing(store, |tx| {
        tx.execute_batch(
            "INSERT INTO draft_revisions
             (revision_id, draft_id, seq, account_id, identity_id, compose_format, plain_flowed,
              subject, to_json, cc_json, bcc_json, body_source, attachments_json, created_at)
             VALUES ('r2', 'd1', 2, 'acct', 'id', 'plain', 1, 's2', '[]', '[]', '[]', 'b', '[]', 0);
             UPDATE drafts SET current_revision_id = 'r2', state = 'editing' WHERE draft_id = 'd1';",
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        Ok(())
    });
}

#[test]
fn queue_inserts_a_held_attempt_with_one_row_per_canonical_recipient() {
    let (_scratch, _paths, store) = scratch();
    seed(&store);
    let object = publish(&store);
    let queued = attempt(&object);
    let published = Arc::clone(&object);
    writing(&store, move |tx| {
        send_draft(tx, &queued, &published, 7, now())
    });

    reading(&store, |tx| {
        let (state, hold_until, sha, sent_copy, config): (String, i64, String, String, i64) = tx
            .query_row(
                "SELECT state, hold_until, mime_sha256, sent_copy, config_generation
                 FROM send_attempts WHERE attempt_id = 'a1'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(state, "held");
        assert_eq!(hold_until, now().as_millis() + 7_000);
        assert_eq!(sha, object.id().sha256_hex());
        assert_eq!(sent_copy, "server_saves");
        assert_eq!(config, 1, "the attempt freezes the account's generation");
        let draft_state: String = tx
            .query_row(
                "SELECT state FROM drafts WHERE draft_id = 'd1'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(draft_state, "queued");
        // The publication's reservation is spent by the same transaction.
        assert_eq!(
            tx.query_row(
                "SELECT count(*) FROM object_pins
                 WHERE object_sha256 = ?1 AND pin_kind = 'reservation'",
                [&object.id().sha256_hex()],
                |row| row.get::<_, i64>(0),
            )
            .unwrap(),
            0
        );
        Ok(())
    });
    let rows: Vec<(String, String, String)> = reading(&store, |tx| {
        let mut statement = tx
            .prepare("SELECT canonical, address, kind FROM send_recipients ORDER BY canonical")
            .map_err(|e| StoreError::internal(e.to_string()))?;
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .map_err(|e| StoreError::internal(e.to_string()))?;
        rows.collect::<Result<_, _>>()
            .map_err(|e| StoreError::internal(e.to_string()))
    });
    assert_eq!(
        rows,
        vec![
            ("a@x.net".to_owned(), "a@X.NET".to_owned(), "to".to_owned()),
            ("b@y.net".to_owned(), "b@Y.NET".to_owned(), "bcc".to_owned()),
        ]
    );
    store.close().unwrap();
}

#[test]
fn queue_conflicts_on_a_non_current_live_or_terminal_revision() {
    let (_scratch, _paths, store) = scratch();
    seed(&store);
    let object = publish(&store);

    // Non-current: a second revision is current.
    save_second_revision(&store);
    let queued = attempt(&object);
    let error = block_on(store.write(WritePriority::User, move |tx| {
        send_draft(tx, &queued, &object, 7, now())
    }))
    .unwrap_err();
    assert!(matches!(error, StoreError::Conflict { .. }), "{error:?}");

    // Live: the seed revision queues once, then refuses a second key.
    let (_scratch2, _paths2, store2) = scratch();
    seed(&store2);
    let object2 = publish(&store2);
    let first = attempt(&object2);
    let published = Arc::clone(&object2);
    writing(&store2, move |tx| {
        send_draft(tx, &first, &published, 7, now())
    });
    let second = SendAttempt {
        attempt_id: "a2".to_owned(),
        mime_sha256: object2.id().sha256_hex(),
        ..attempt(&object2)
    };
    let error = block_on(store2.write(WritePriority::User, move |tx| {
        send_draft(tx, &second, &object2, 7, now())
    }))
    .unwrap_err();
    assert!(matches!(error, StoreError::Conflict { .. }), "{error:?}");

    // Terminal: a sent or discarded draft refuses.
    for state in ["sent", "discarded"] {
        writing(&store2, move |tx| {
            tx.execute(
                "UPDATE drafts SET state = ?1 WHERE draft_id = 'd1'",
                [state],
            )
            .map_err(|e| StoreError::internal(e.to_string()))?;
            Ok(())
        });
        let object3 = publish(&store2);
        let terminal = attempt(&object3);
        let error = block_on(store2.write(WritePriority::User, move |tx| {
            send_draft(tx, &terminal, &object3, 7, now())
        }))
        .unwrap_err();
        assert!(
            matches!(error, StoreError::Conflict { .. }),
            "{state}: {error:?}"
        );
        writing(&store2, |tx| {
            tx.execute(
                "UPDATE drafts SET state = 'editing' WHERE draft_id = 'd1'",
                [],
            )
            .map_err(|e| StoreError::internal(e.to_string()))?;
            Ok(())
        });
    }
    store.close().unwrap();
    store2.close().unwrap();
}

#[test]
fn cancel_moves_held_and_queued_to_cancelled_and_conflicts_elsewhere() {
    let (_scratch, _paths, store) = scratch();
    seed(&store);
    let object = publish(&store);
    let queued = attempt(&object);
    let published = Arc::clone(&object);
    writing(&store, move |tx| {
        send_draft(tx, &queued, &published, 7, now())
    });

    // Held: the draft returns to editing.
    writing(&store, |tx| cancel_send(tx, "a1", now()));
    let (held, draft): (String, String) = reading(&store, |tx| {
        Ok((
            tx.query_row(
                "SELECT state FROM send_attempts WHERE attempt_id = 'a1'",
                [],
                |row| row.get(0),
            )
            .unwrap(),
            tx.query_row(
                "SELECT state FROM drafts WHERE draft_id = 'd1'",
                [],
                |row| row.get(0),
            )
            .unwrap(),
        ))
    });
    assert_eq!((held.as_str(), draft.as_str()), ("cancelled", "editing"));

    // Queued: the same transition.
    writing(&store, |tx| {
        tx.execute_batch(
            "UPDATE send_attempts SET state = 'queued' WHERE attempt_id = 'a1';
             UPDATE drafts SET state = 'queued' WHERE draft_id = 'd1';",
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        Ok(())
    });
    writing(&store, |tx| cancel_send(tx, "a1", now()));
    let draft: String = reading(&store, |tx| {
        tx.query_row(
            "SELECT state FROM drafts WHERE draft_id = 'd1'",
            [],
            |row| row.get(0),
        )
        .map_err(|e| StoreError::internal(e.to_string()))
    });
    assert_eq!(draft, "editing");

    // Every other state is Conflict; an unknown attempt is NotFound.
    for state in [
        "submitting",
        "accepted",
        "failed",
        "outcome_unknown",
        "cancelled",
    ] {
        writing(&store, move |tx| {
            tx.execute(
                "UPDATE send_attempts SET state = ?1 WHERE attempt_id = 'a1'",
                [state],
            )
            .map_err(|e| StoreError::internal(e.to_string()))?;
            Ok(())
        });
        let error = block_on(store.write(WritePriority::User, |tx| cancel_send(tx, "a1", now())))
            .unwrap_err();
        assert!(
            matches!(error, StoreError::Conflict { .. }),
            "{state}: {error:?}"
        );
    }
    let error = block_on(store.write(WritePriority::User, |tx| cancel_send(tx, "missing", now())))
        .unwrap_err();
    assert!(matches!(error, StoreError::NotFound { .. }), "{error:?}");
    store.close().unwrap();
}

#[test]
fn cancelling_an_older_attempt_leaves_a_newer_revision_current() {
    let (_scratch, _paths, store) = scratch();
    seed(&store);
    let object = publish(&store);
    let queued = attempt(&object);
    let published = Arc::clone(&object);
    writing(&store, move |tx| {
        send_draft(tx, &queued, &published, 7, now())
    });
    save_second_revision(&store);
    writing(&store, |tx| cancel_send(tx, "a1", now()));
    let (state, current): (String, String) = reading(&store, |tx| {
        Ok((
            tx.query_row(
                "SELECT state FROM drafts WHERE draft_id = 'd1'",
                [],
                |row| row.get(0),
            )
            .unwrap(),
            tx.query_row(
                "SELECT current_revision_id FROM drafts WHERE draft_id = 'd1'",
                [],
                |row| row.get(0),
            )
            .unwrap(),
        ))
    });
    assert_eq!((state.as_str(), current.as_str()), ("editing", "r2"));
    store.close().unwrap();
}

#[test]
fn reply_headers_read_the_source_message() {
    let (_scratch, _paths, store) = scratch();
    seed(&store);
    writing(&store, |tx| {
        tx.execute_batch(
            r#"INSERT INTO mailboxes
             (mailbox_id, account_id, remote_name, kind, selectable, sync_state)
             VALUES (7, 'acct', 'Inbox', 'folder', 1, 'synced');
             INSERT INTO logical_messages (message_id, account_id, created_at)
             VALUES (9, 'acct', 0);
             INSERT INTO occurrences
             (occurrence_id, account_id, mailbox_id, message_id, first_seen_at, last_seen_at, state)
             VALUES (11, 'acct', 7, 9, 0, 0, 'present');
             INSERT INTO message_headers
             (message_id, raw_fields, header_message_id, in_reply_to, references_json, fetched_at)
             VALUES (9, x'', 'sent@example.test', 'parent@example.test',
                     '["ref@example.test"]', 0);"#,
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        Ok(())
    });
    let (in_reply_to, references) = reading(&store, |tx| reply_headers(tx, Some(11), None));
    assert_eq!(in_reply_to.as_deref(), Some("parent@example.test"));
    assert_eq!(references, vec!["ref@example.test".to_owned()]);
    store.close().unwrap();
}

/// The schema's foreign key refuses an attempt whose account or
/// identity is not its revision's route snapshot (spec/02), seen from
/// a raw connection as well as the store's own transaction.
#[test]
fn the_route_snapshot_refusal_is_the_schema() {
    let (_scratch, paths, store) = scratch();
    seed(&store);
    writing(&store, |tx| {
        let mismatched_account = tx.execute(
            "INSERT INTO send_attempts
             (attempt_id, draft_id, revision_id, account_id, config_generation, identity_id,
              mime_sha256, message_id_header, envelope_from, state, sent_copy, created_at, updated_at)
             VALUES ('a2', 'd1', 'r1', 'other', 1, 'id', 'sha', 'm', 'me@example.test',
                     'held', 'server_saves', 0, 0)",
            [],
        );
        assert!(
            mismatched_account.is_err(),
            "a foreign account was admitted"
        );
        let mismatched_identity = tx.execute(
            "INSERT INTO send_attempts
             (attempt_id, draft_id, revision_id, account_id, config_generation, identity_id,
              mime_sha256, message_id_header, envelope_from, state, sent_copy, created_at, updated_at)
             VALUES ('a3', 'd1', 'r1', 'acct', 1, 'missing', 'sha', 'm', 'me@example.test',
                     'held', 'server_saves', 0, 0)",
            [],
        );
        assert!(
            mismatched_identity.is_err(),
            "a foreign identity was admitted"
        );
        tx.execute(
            "INSERT INTO send_attempts
             (attempt_id, draft_id, revision_id, account_id, config_generation, identity_id,
              mime_sha256, message_id_header, envelope_from, state, sent_copy, created_at, updated_at)
             VALUES ('a4', 'd1', 'r1', 'acct', 1, 'id', 'sha', 'm', 'me@example.test',
                     'held', 'server_saves', 0, 0)",
            [],
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        Ok(())
    });
    assert_eq!(
        reading(&store, |tx| {
            tx.query_row("SELECT count(*) FROM send_attempts", [], |row| {
                row.get::<_, i64>(0)
            })
            .map_err(|e| StoreError::internal(e.to_string()))
        }),
        1
    );
    store.close().unwrap();

    // A raw connection with `foreign_keys = ON` sees the same refusal.
    let raw = Connection::open(paths.state_db()).unwrap();
    raw.execute_batch("PRAGMA foreign_keys = ON").unwrap();
    assert!(raw
        .execute(
            "INSERT INTO send_attempts
             (attempt_id, draft_id, revision_id, account_id, config_generation, identity_id,
              mime_sha256, message_id_header, envelope_from, state, sent_copy, created_at, updated_at)
             VALUES ('a5', 'd1', 'r1', 'acct', 1, 'missing', 'sha', 'm', 'me@example.test',
                     'held', 'server_saves', 0, 0)",
            [],
        )
        .is_err());
    raw.close().unwrap();
}
