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
/// Queue the seed revision as attempt `a1`.
fn queue(store: &Store) {
    let object = publish(store);
    let queued = attempt(&object);
    let published = Arc::clone(&object);
    writing(store, move |tx| {
        send_draft(tx, &queued, &published, 7, now())
    });
}

/// Move an attempt to `state`, optionally moving its recipients too.
fn force(store: &Store, attempt: &str, state: &str, recipient_state: Option<&str>) {
    let statement = match recipient_state {
        Some(recipient_state) => format!(
            "UPDATE send_attempts SET state = '{state}' WHERE attempt_id = '{attempt}';
             UPDATE send_recipients SET state = '{recipient_state}' WHERE attempt_id = '{attempt}';"
        ),
        None => {
            format!("UPDATE send_attempts SET state = '{state}' WHERE attempt_id = '{attempt}';")
        }
    };
    writing(store, move |tx| {
        tx.execute_batch(&statement)
            .map_err(|e| StoreError::internal(e.to_string()))?;
        Ok(())
    });
}

fn one(store: &Store, statement: &str) -> String {
    let statement = statement.to_owned();
    reading(store, move |tx| {
        tx.query_row(&statement, [], |row| row.get(0))
            .map_err(|e| StoreError::internal(e.to_string()))
    })
}

fn one_i64(store: &Store, statement: &str) -> i64 {
    let statement = statement.to_owned();
    reading(store, move |tx| {
        tx.query_row(&statement, [], |row| row.get(0))
            .map_err(|e| StoreError::internal(e.to_string()))
    })
}

#[test]
fn marked_sent_moves_unknown_recipients_and_counts_them_once() {
    let (_scratch, _paths, store) = scratch();
    seed(&store);
    writing(&store, |tx| {
        tx.execute(
            "INSERT INTO seen_addresses (canonical, email, display_name, last_seen_at,
                times_sent_to, times_received, last_account_id)
             VALUES ('a@x.net', 'old@x.net', 'Old', 5, 4, 2, NULL)",
            [],
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        Ok(())
    });
    queue(&store);
    force(&store, "a1", "outcome_unknown", Some("unknown"));
    writing(&store, |tx| {
        resolve_unknown_send(tx, "a1", UnknownResolution::MarkedSent, None, 0, now())
    });
    assert_eq!(
        one(
            &store,
            "SELECT resolution FROM send_attempts WHERE attempt_id = 'a1'"
        ),
        "marked_sent"
    );
    assert_eq!(
        one(&store, "SELECT state FROM drafts WHERE draft_id = 'd1'"),
        "sent"
    );
    assert_eq!(
        one_i64(
            &store,
            "SELECT count(*) FROM send_recipients WHERE attempt_id = 'a1' AND state = 'sent'"
        ),
        2
    );
    assert_eq!(
        one_i64(
            &store,
            "SELECT count(*) FROM send_recipients WHERE attempt_id = 'a1' AND state = 'unknown'"
        ),
        0
    );
    assert_eq!(
        one(
            &store,
            "SELECT email || '|' || coalesce(display_name, '') || '|' || last_seen_at || '|' || times_sent_to || '|' || last_account_id FROM seen_addresses WHERE canonical = 'a@x.net'"
        ),
        "old@x.net|Old|5|5|acct"
    );
    assert_eq!(
        one(
            &store,
            "SELECT email || '|' || coalesce(display_name, '') || '|' || last_seen_at || '|' || times_sent_to || '|' || last_account_id FROM seen_addresses WHERE canonical = 'b@y.net'"
        ),
        format!("b@Y.NET||{}|1|acct", now().as_millis())
    );
    let error = block_on(store.write(WritePriority::User, |tx| {
        resolve_unknown_send(tx, "a1", UnknownResolution::KeptAsDraft, None, 0, now())
    }))
    .unwrap_err();
    assert!(matches!(error, StoreError::Conflict { .. }), "{error:?}");
    store.close().unwrap();
}

#[test]
fn kept_as_draft_and_send_again_move_the_draft() {
    let (_scratch, _paths, store) = scratch();
    seed(&store);
    queue(&store);
    force(&store, "a1", "outcome_unknown", Some("unknown"));
    writing(&store, |tx| {
        resolve_unknown_send(tx, "a1", UnknownResolution::KeptAsDraft, None, 0, now())
    });
    assert_eq!(
        one(&store, "SELECT state FROM drafts WHERE draft_id = 'd1'"),
        "editing"
    );
    store.close().unwrap();

    let (_scratch, _paths, store) = scratch();
    seed(&store);
    queue(&store);
    force(&store, "a1", "outcome_unknown", Some("unknown"));
    writing(&store, |tx| {
        let created =
            resolve_unknown_send(tx, "a1", UnknownResolution::SendAgain, Some("a2"), 7, now());
        assert_eq!(created?.as_deref(), Some("a2"));
        Ok(())
    });
    assert_eq!(
        one(
            &store,
            "SELECT state FROM send_attempts WHERE attempt_id = 'a2'"
        ),
        "held"
    );
    assert_eq!(
        one_i64(
            &store,
            "SELECT hold_until FROM send_attempts WHERE attempt_id = 'a2'"
        ),
        now().as_millis() + 7_000
    );
    assert_eq!(
        one(
            &store,
            "SELECT resolution FROM send_attempts WHERE attempt_id = 'a1'"
        ),
        "send_again"
    );
    assert_eq!(
        one(&store, "SELECT state FROM drafts WHERE draft_id = 'd1'"),
        "queued"
    );
    assert_eq!(
        one_i64(
            &store,
            "SELECT count(*) FROM send_attempts o JOIN send_attempts n
            ON n.attempt_id = 'a2' WHERE o.attempt_id = 'a1'
            AND o.mime_sha256 = n.mime_sha256 AND o.message_id_header = n.message_id_header"
        ),
        1
    );
    assert_eq!(
        one(
            &store,
            "SELECT group_concat(canonical || '|' || address || '|' || state, ',') FROM (SELECT canonical, address, state FROM send_recipients WHERE attempt_id = 'a2' ORDER BY canonical)"
        ),
        "a@x.net|a@X.NET|pending,b@y.net|b@Y.NET|pending"
    );
    store.close().unwrap();
}

#[test]
fn a_resolution_changes_only_the_current_revision() {
    let (_scratch, _paths, store) = scratch();
    seed(&store);
    queue(&store);
    force(&store, "a1", "outcome_unknown", Some("unknown"));
    save_second_revision(&store);
    let object = publish(&store);
    let queued = SendAttempt {
        attempt_id: "a2".to_owned(),
        revision_id: "r2".to_owned(),
        ..attempt(&object)
    };
    let published = Arc::clone(&object);
    writing(&store, move |tx| {
        send_draft(tx, &queued, &published, 7, now())
    });
    writing(&store, |tx| {
        resolve_unknown_send(tx, "a1", UnknownResolution::MarkedSent, None, 0, now())
    });
    assert_eq!(
        one(
            &store,
            "SELECT state FROM send_attempts WHERE attempt_id = 'a2'"
        ),
        "held"
    );
    assert_eq!(
        one(&store, "SELECT state FROM drafts WHERE draft_id = 'd1'"),
        "queued"
    );
    assert_eq!(
        one(
            &store,
            "SELECT resolution FROM send_attempts WHERE attempt_id = 'a1'"
        ),
        "marked_sent"
    );
    store.close().unwrap();
}

#[test]
fn confirm_send_route_adopts_the_generation_and_refuses_unsendable() {
    let (_scratch, _paths, store) = scratch();
    seed(&store);
    queue(&store);
    let error = block_on(store.write(WritePriority::User, |tx| {
        confirm_send_route(tx, "a1", now())
    }))
    .unwrap_err();
    assert!(matches!(error, StoreError::Conflict { .. }), "{error:?}");
    force(&store, "a1", "queued", None);
    writing(&store, |tx| {
        tx.execute(
            "UPDATE accounts SET config_generation = 2 WHERE account_id = 'acct'",
            [],
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        Ok(())
    });
    writing(&store, |tx| confirm_send_route(tx, "a1", now()));
    assert_eq!(
        one_i64(
            &store,
            "SELECT config_generation FROM send_attempts WHERE attempt_id = 'a1'"
        ),
        2
    );
    writing(&store, |tx| {
        tx.execute(
            "UPDATE identities SET sendable = 'unavailable' WHERE identity_id = 'id'",
            [],
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        Ok(())
    });
    let error = block_on(store.write(WritePriority::User, |tx| {
        confirm_send_route(tx, "a1", now())
    }))
    .unwrap_err();
    assert!(matches!(error, StoreError::Conflict { .. }), "{error:?}");
    let error = block_on(store.write(WritePriority::User, |tx| {
        confirm_send_route(tx, "missing", now())
    }))
    .unwrap_err();
    assert!(matches!(error, StoreError::NotFound { .. }), "{error:?}");
    store.close().unwrap();
}

#[test]
fn retry_rejected_recipients_conflicts_for_proton() {
    let (_scratch, _paths, store) = scratch();
    seed(&store);
    queue(&store);
    let error = block_on(store.write(WritePriority::User, |tx| {
        retry_rejected_recipients(tx, "a1")
    }))
    .unwrap_err();
    assert!(matches!(error, StoreError::Conflict { .. }), "{error:?}");
    assert_eq!(one_i64(&store, "SELECT count(*) FROM send_attempts"), 1);
    store.close().unwrap();
}
