//! Send store tests: the `held` queue insert's exact rows, the
//! recipient canonicalization, the conflict boundaries, `cancel_send`
//! and the route snapshot's schema refusal.

use std::io::{Cursor, Read};
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
    queue_as(store, "a1", "r1");
}

/// Queue one revision as an attempt with the given id.
fn queue_as(store: &Store, attempt_id: &str, revision_id: &str) {
    let object = publish(store);
    let mut queued = attempt(&object);
    queued.attempt_id = attempt_id.to_owned();
    queued.revision_id = revision_id.to_owned();
    let published = Arc::clone(&object);
    writing(store, move |tx| {
        send_draft(tx, &queued, &published, 7, now())
    });
}

/// Insert a Proton message and an occurrence of it, so a draft can
/// point at the source.
fn seed_source(store: &Store, provider_key: Option<&'static str>) {
    writing(store, move |tx| {
        tx.execute(
            "INSERT INTO mailboxes
             (mailbox_id, account_id, remote_name, kind, selectable, sync_state)
             VALUES (7, 'acct', 'Inbox', 'folder', 1, 'synced')",
            [],
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        tx.execute(
            "INSERT INTO logical_messages (message_id, account_id, provider_key, created_at)
             VALUES (9, 'acct', ?1, 0)",
            params![provider_key],
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        tx.execute(
            "INSERT INTO occurrences
             (occurrence_id, account_id, mailbox_id, message_id, first_seen_at, last_seen_at, state)
             VALUES (11, 'acct', 7, 9, 0, 0, 'present')",
            [],
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        Ok(())
    });
}

fn upload(index: usize) -> ProtonUpload {
    ProtonUpload {
        index,
        id: format!("att-{index}"),
        name: format!("file-{index}.bin"),
        mime_type: "application/octet-stream".to_owned(),
        size: 10,
    }
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

/// One directly inserted Out Box attempt: the fields `out_box()`
/// reads, with explicit times so ordering and the 30-day cut are
/// observable.
struct SeedAttempt {
    attempt_id: &'static str,
    draft_id: &'static str,
    revision_id: &'static str,
    state: &'static str,
    config_generation: i64,
    created_at: i64,
    updated_at: i64,
    resolution: Option<&'static str>,
    resolved_at: Option<i64>,
    final_reply: Option<&'static str>,
    hold_until: Option<i64>,
    send_requested_at: Option<i64>,
}

fn insert_draft(store: &Store, draft_id: &str) {
    let draft_id = draft_id.to_owned();
    writing(store, move |tx| {
        tx.execute(
            "INSERT INTO drafts (draft_id, state, current_revision_id, created_at, updated_at)
             VALUES (?1, 'editing', NULL, 0, 0)",
            params![draft_id],
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        Ok(())
    });
}

fn insert_revision(store: &Store, draft_id: &str, revision_id: &str, subject: &str) {
    let (draft_id, revision_id, subject) = (
        draft_id.to_owned(),
        revision_id.to_owned(),
        subject.to_owned(),
    );
    writing(store, move |tx| {
        tx.execute(
            "INSERT INTO draft_revisions
             (revision_id, draft_id, seq, account_id, identity_id, compose_format, plain_flowed,
              subject, to_json, cc_json, bcc_json, body_source, attachments_json, created_at)
             VALUES (?1, ?2,
                     (SELECT COALESCE(MAX(seq), 0) + 1 FROM draft_revisions WHERE draft_id = ?2),
                     'acct', 'id', 'plain', 1, ?3, '[]', '[]', '[]', 'b', '[]', 0)",
            params![revision_id, draft_id, subject],
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        Ok(())
    });
}

fn insert_attempt(store: &Store, attempt: SeedAttempt) {
    let SeedAttempt {
        attempt_id,
        draft_id,
        revision_id,
        state,
        config_generation,
        created_at,
        updated_at,
        resolution,
        resolved_at,
        final_reply,
        hold_until,
        send_requested_at,
    } = attempt;
    let proton_draft_id = send_requested_at.map(|_| "pd");
    writing(store, move |tx| {
        tx.execute(
            "INSERT INTO send_attempts
             (attempt_id, draft_id, revision_id, account_id, config_generation, identity_id,
              mime_sha256, message_id_header, envelope_from, state, hold_until, final_reply,
              sent_copy, resolution, resolved_at, created_at, updated_at,
              proton_draft_id, send_requested_at)
             VALUES (?1, ?2, ?3, 'acct', ?4, 'id', 'sha', 'm', 'me@example.test',
                     ?5, ?6, ?7, 'server_saves', ?8, ?9, ?10, ?11, ?12, ?13)",
            params![
                attempt_id,
                draft_id,
                revision_id,
                config_generation,
                state,
                hold_until,
                final_reply,
                resolution,
                resolved_at,
                created_at,
                updated_at,
                proton_draft_id,
                send_requested_at,
            ],
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        Ok(())
    });
}

fn insert_recipient(
    store: &Store,
    attempt_id: &str,
    canonical: &str,
    address: &str,
    kind: &str,
    state: &str,
) {
    let (attempt_id, canonical, address, kind, state) = (
        attempt_id.to_owned(),
        canonical.to_owned(),
        address.to_owned(),
        kind.to_owned(),
        state.to_owned(),
    );
    writing(store, move |tx| {
        tx.execute(
            "INSERT INTO send_recipients (attempt_id, canonical, address, kind, state)
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![attempt_id, canonical, address, kind, state],
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        Ok(())
    });
}

fn read_out_box(store: &Store, now_ms: i64) -> OutBox {
    reading(store, |tx| out_box(tx, Timestamp::from_millis(now_ms)))
}

fn attempt_ids(rows: &[OutBoxRow]) -> Vec<&str> {
    rows.iter().map(|row| row.attempt_id.as_str()).collect()
}

#[test]
fn out_box_splits_live_and_history_orders_each_and_cuts_at_30_days() {
    let (_scratch, _paths, store) = scratch();
    seed(&store);
    let now_ms = 10_000_000_000_i64;
    let cutoff = now_ms - 30 * 24 * 60 * 60 * 1000;
    for draft_id in ["d2", "d3", "d4", "d5"] {
        insert_draft(&store, draft_id);
    }
    // d1's seed revision is r1; the rest are inserted with subjects.
    for (draft_id, revision_id, subject) in [
        ("d1", "r2", "queued ok"),
        ("d1", "r3", "queued wait"),
        ("d1", "r4", "submitting"),
        ("d1", "r5", "unknown"),
        ("d1", "r7", "accepted"),
        ("d1", "r8", "cancelled"),
        ("d1", "r9", "resolved"),
        ("d1", "r12", "old accepted"),
        ("d2", "ra", "failed old"),
        ("d2", "rb", "failed new"),
        ("d3", "rc", "failed only"),
        ("d4", "rd1", "tie b"),
        ("d4", "rd2", "tie a"),
        ("d4", "rd3", "cutoff"),
        ("d5", "rf", "failed live"),
    ] {
        insert_revision(&store, draft_id, revision_id, subject);
    }
    let base = 8_000_000_000_i64;
    let seed_attempt = |attempt_id: &'static str,
                        draft_id: &'static str,
                        revision_id: &'static str,
                        state: &'static str,
                        updated_at: i64| {
        insert_attempt(
            &store,
            SeedAttempt {
                attempt_id,
                draft_id,
                revision_id,
                state,
                config_generation: 1,
                created_at: updated_at,
                updated_at,
                resolution: None,
                resolved_at: None,
                final_reply: None,
                hold_until: None,
                send_requested_at: None,
            },
        );
    };
    seed_attempt("a_held", "d1", "r1", "held", base + 1);
    seed_attempt("a_queued_ok", "d1", "r2", "queued", base + 11);
    seed_attempt("a_queued_wait", "d1", "r3", "queued", base + 21);
    seed_attempt("a_submitting", "d1", "r4", "submitting", base + 31);
    seed_attempt("a_unknown", "d1", "r5", "outcome_unknown", base + 41);
    seed_attempt("a_failed_live", "d5", "rf", "failed", base + 51);
    seed_attempt("a_accepted", "d1", "r7", "accepted", base + 61);
    seed_attempt("a_cancelled", "d1", "r8", "cancelled", base + 71);
    seed_attempt("a_resolved", "d1", "r9", "outcome_unknown", base + 81);
    seed_attempt("a_failed_old", "d2", "ra", "failed", base + 91);
    seed_attempt("a_failed_new", "d2", "rb", "failed", base + 101);
    seed_attempt("a_failed_only", "d3", "rc", "failed", base + 111);
    seed_attempt("a_tie_b", "d4", "rd1", "held", base + 200);
    seed_attempt("a_tie_a", "d4", "rd2", "held", base + 200);
    // A reconfiguration and a resolution, then the boundary row.
    writing(&store, move |tx| {
        tx.execute(
            "UPDATE send_attempts SET config_generation = 2 WHERE attempt_id = 'a_queued_wait'",
            [],
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        tx.execute(
            "UPDATE send_attempts SET resolution = 'marked_sent', resolved_at = 8000000081
             WHERE attempt_id = 'a_resolved'",
            [],
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        tx.execute(
            "UPDATE send_attempts SET final_reply = '1000 OK' WHERE attempt_id = 'a_accepted'",
            [],
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        tx.execute(
            "UPDATE send_attempts SET final_reply = '550 no' WHERE attempt_id = 'a_failed_live'",
            [],
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        tx.execute(
            "UPDATE send_attempts SET send_requested_at = 8000000030,
                proton_draft_id = 'pd' WHERE attempt_id = 'a_submitting'",
            [],
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        tx.execute(
            "UPDATE send_attempts SET hold_until = 8000000002 WHERE attempt_id = 'a_held'",
            [],
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        tx.execute(
            "INSERT INTO send_attempts
             (attempt_id, draft_id, revision_id, account_id, config_generation, identity_id,
              mime_sha256, message_id_header, envelope_from, state, sent_copy, created_at, updated_at)
             VALUES ('a_old_accepted', 'd1', 'r12', 'acct', 1, 'id', 'sha', 'm',
                     'me@example.test', 'accepted', 'server_saves', 7000000000, 7000000001)",
            [],
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        tx.execute(
            "INSERT INTO send_attempts
             (attempt_id, draft_id, revision_id, account_id, config_generation, identity_id,
              mime_sha256, message_id_header, envelope_from, state, sent_copy, created_at, updated_at)
             VALUES ('a_cutoff', 'd4', 'rd3', 'acct', 1, 'id', 'sha', 'm',
                     'me@example.test', 'accepted', 'server_saves', ?1, ?1)",
            params![cutoff],
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        Ok(())
    });
    insert_recipient(&store, "a_held", "a@x.net", "a@X.NET", "to", "pending");
    insert_recipient(&store, "a_held", "b@y.net", "b@Y.NET", "bcc", "pending");
    insert_recipient(&store, "a_accepted", "a@x.net", "a@X.NET", "to", "sent");
    insert_recipient(&store, "a_accepted", "b@y.net", "b@Y.NET", "bcc", "sent");
    insert_recipient(&store, "a_unknown", "a@x.net", "a@X.NET", "to", "unknown");
    insert_recipient(
        &store,
        "a_failed_live",
        "a@x.net",
        "a@X.NET",
        "to",
        "unsent",
    );

    let out_box = read_out_box(&store, now_ms);
    assert_eq!(
        attempt_ids(&out_box.live),
        vec![
            "a_tie_b",
            "a_tie_a",
            "a_failed_only",
            "a_failed_new",
            "a_failed_live",
            "a_unknown",
            "a_submitting",
            "a_queued_wait",
            "a_queued_ok",
            "a_held",
        ]
    );
    assert_eq!(
        attempt_ids(&out_box.history),
        vec![
            "a_failed_old",
            "a_resolved",
            "a_cancelled",
            "a_accepted",
            "a_cutoff",
        ]
    );
    let row = |id: &str| {
        out_box
            .live
            .iter()
            .chain(out_box.history.iter())
            .find(|row| row.attempt_id == id)
            .unwrap_or_else(|| panic!("{id} is in the out box"))
    };
    let held = row("a_held");
    assert_eq!(held.state, AttemptState::Held);
    assert_eq!(held.subject, "s");
    assert_eq!(held.hold_until, Some(Timestamp::from_millis(base + 2)));
    assert_eq!(held.sent_copy, SentCopy::ServerSaves);
    assert!(!held.waits_for_confirmation);
    assert_eq!(
        held.recipients,
        vec![
            OutBoxRecipient {
                address: "a@X.NET".to_owned(),
                kind: RecipientKind::To,
                state: RecipientState::Pending,
            },
            OutBoxRecipient {
                address: "b@Y.NET".to_owned(),
                kind: RecipientKind::Bcc,
                state: RecipientState::Pending,
            },
        ]
    );
    assert!(!row("a_queued_ok").waits_for_confirmation);
    assert!(row("a_queued_wait").waits_for_confirmation);
    let submitting = row("a_submitting");
    assert_eq!(
        submitting.send_requested_at,
        Some(Timestamp::from_millis(base + 30))
    );
    assert_eq!(submitting.proton_draft_id.as_deref(), Some("pd"));
    let unknown = row("a_unknown");
    assert_eq!(unknown.state, AttemptState::OutcomeUnknown);
    assert_eq!(unknown.resolution, None);
    assert_eq!(
        unknown.recipients,
        vec![OutBoxRecipient {
            address: "a@X.NET".to_owned(),
            kind: RecipientKind::To,
            state: RecipientState::Unknown,
        }]
    );
    let failed = row("a_failed_live");
    assert_eq!(failed.state, AttemptState::Failed);
    assert_eq!(failed.final_reply.as_deref(), Some("550 no"));
    assert_eq!(
        failed.recipients,
        vec![OutBoxRecipient {
            address: "a@X.NET".to_owned(),
            kind: RecipientKind::To,
            state: RecipientState::Unsent,
        }]
    );
    let accepted = row("a_accepted");
    assert_eq!(accepted.state, AttemptState::Accepted);
    assert_eq!(accepted.final_reply.as_deref(), Some("1000 OK"));
    assert_eq!(
        accepted.recipients,
        vec![
            OutBoxRecipient {
                address: "a@X.NET".to_owned(),
                kind: RecipientKind::To,
                state: RecipientState::Sent,
            },
            OutBoxRecipient {
                address: "b@Y.NET".to_owned(),
                kind: RecipientKind::Bcc,
                state: RecipientState::Sent,
            },
        ]
    );
    let resolved = row("a_resolved");
    assert_eq!(resolved.resolution, Some(UnknownResolution::MarkedSent));
    assert_eq!(
        resolved.resolved_at,
        Some(Timestamp::from_millis(base + 81))
    );
    assert_eq!(row("a_cutoff").updated_at, Timestamp::from_millis(cutoff));
    store.close().unwrap();
}

#[test]
fn out_box_reports_the_sent_copy_and_warns_24_hours_after_acceptance() {
    let (_scratch, _paths, store) = scratch();
    seed(&store);
    insert_revision(&store, "d1", "r2", "accepted with a draft");
    let accepted_at = 1_700_000_000_000_i64;
    writing(&store, move |tx| {
        tx.execute(
            "INSERT INTO send_attempts
             (attempt_id, draft_id, revision_id, account_id, config_generation, identity_id,
              mime_sha256, message_id_header, envelope_from, state, sent_copy, created_at,
              updated_at, proton_draft_id)
             VALUES ('a1', 'd1', 'r2', 'acct', 1, 'id', 'sha', 'm', 'me@example.test',
                     'accepted', 'server_saves', ?1, ?1, 'pd')",
            params![accepted_at],
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        Ok(())
    });
    let day = 24 * 60 * 60 * 1000;
    let accepted_row = |store: &Store, now_ms: i64| {
        let out_box = read_out_box(store, now_ms);
        out_box
            .history
            .iter()
            .find(|row| row.attempt_id == "a1")
            .expect("the accepted attempt is history")
            .clone()
    };

    // A Sent copy missing 23 h 59 min after acceptance is only absent.
    let row = accepted_row(&store, accepted_at + day - 1);
    assert!(!row.sent_copy_appeared);
    assert!(!row.sent_copy_warning);

    // At 24 h exactly it warns, still without any resend.
    let row = accepted_row(&store, accepted_at + day);
    assert!(!row.sent_copy_appeared);
    assert!(row.sent_copy_warning);

    // The draft synced into Drafts under the same Proton id isn't the
    // Sent copy: the message must be present in the Sent mailbox.
    writing(&store, move |tx| {
        tx.execute_batch(
            "INSERT INTO mailboxes
             (mailbox_id, account_id, remote_name, kind, role, selectable, sync_state)
             VALUES (20, 'acct', 'Drafts', 'folder', 'drafts', 1, 'synced'),
                    (21, 'acct', 'Sent', 'folder', 'sent', 1, 'synced');
             INSERT INTO logical_messages (message_id, account_id, provider_key, created_at)
             VALUES (30, 'acct', 'pd', 0);
             INSERT INTO occurrences
             (occurrence_id, account_id, mailbox_id, message_id, first_seen_at, last_seen_at, state)
             VALUES (40, 'acct', 20, 30, 0, 0, 'present');",
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        Ok(())
    });
    let row = accepted_row(&store, accepted_at + 2 * day);
    assert!(
        !row.sent_copy_appeared,
        "a Drafts occurrence is not the Sent copy"
    );
    assert!(row.sent_copy_warning);

    // The send's events move the message into Sent; then it has
    // appeared and the warning clears.
    writing(&store, move |tx| {
        tx.execute(
            "INSERT INTO occurrences
             (occurrence_id, account_id, mailbox_id, message_id, first_seen_at, last_seen_at, state)
             VALUES (41, 'acct', 21, 30, 0, 0, 'present')",
            [],
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        Ok(())
    });
    let row = accepted_row(&store, accepted_at + 2 * day);
    assert!(row.sent_copy_appeared);
    assert!(!row.sent_copy_warning);
    store.close().unwrap();
}

#[test]
fn a_failed_attempt_is_live_until_a_newer_attempt_replaces_it() {
    let (_scratch, _paths, store) = scratch();
    seed(&store);
    insert_attempt(
        &store,
        SeedAttempt {
            attempt_id: "a1",
            draft_id: "d1",
            revision_id: "r1",
            state: "failed",
            config_generation: 1,
            created_at: 1_000,
            updated_at: 1_000,
            resolution: None,
            resolved_at: None,
            final_reply: None,
            hold_until: None,
            send_requested_at: None,
        },
    );
    let out_box = read_out_box(&store, 2_000);
    assert_eq!(attempt_ids(&out_box.live), vec!["a1"]);
    assert!(out_box.history.is_empty());

    // A newer attempt for the same draft replaces it into history.
    insert_revision(&store, "d1", "r2", "second");
    insert_attempt(
        &store,
        SeedAttempt {
            attempt_id: "a2",
            draft_id: "d1",
            revision_id: "r2",
            state: "held",
            config_generation: 1,
            created_at: 2_000,
            updated_at: 2_000,
            resolution: None,
            resolved_at: None,
            final_reply: None,
            hold_until: None,
            send_requested_at: None,
        },
    );
    let out_box = read_out_box(&store, 3_000);
    assert_eq!(attempt_ids(&out_box.live), vec!["a2"]);
    assert_eq!(attempt_ids(&out_box.history), vec!["a1"]);
    store.close().unwrap();
}

/// Claim the seed attempt `a1`.
fn claim(store: &Store) -> ClaimedSend {
    writing(store, |tx| claim_send_attempt(tx, "acct", now())).unwrap()
}

fn accept(store: &Store) -> bool {
    writing(store, |tx| mark_send_accepted(tx, "a1", now()))
}

fn fail(store: &Store, failure: SendFailure) -> bool {
    writing(store, move |tx| mark_send_failed(tx, "a1", &failure, now()))
}

fn unknown(store: &Store) -> bool {
    writing(store, |tx| mark_outcome_unknown(tx, "a1", now()))
}

#[test]
fn earliest_hold_until_is_the_account_s_first_open_window() {
    let (_scratch, _paths, store) = scratch();
    seed(&store);
    let earliest = |account: &'static str| {
        reading(&store, move |tx| earliest_hold_until(tx, account)).map(|t| t.as_millis())
    };
    assert_eq!(earliest("acct"), None, "no attempt is held yet");

    queue(&store);
    let held = one_i64(
        &store,
        "SELECT hold_until FROM send_attempts WHERE attempt_id = 'a1'",
    );
    assert_eq!(earliest("acct"), Some(held));
    assert_eq!(
        earliest("other"),
        None,
        "another account's window is not this one's"
    );

    // A queued attempt's window is over.
    writing(&store, move |tx| {
        queue_due_held(tx, Timestamp::from_millis(held)).map(|_| ())
    });
    assert_eq!(earliest("acct"), None);
}

#[test]
fn queue_due_held_moves_only_attempts_past_their_window() {
    let (_scratch, _paths, store) = scratch();
    seed(&store);
    queue(&store);
    writing(&store, |tx| {
        tx.execute(
            "UPDATE send_attempts SET hold_until = ?1 WHERE attempt_id = 'a1'",
            [now().as_millis()],
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        assert_eq!(queue_due_held(tx, now())?, 1);
        Ok(())
    });
    assert_eq!(
        one(
            &store,
            "SELECT state FROM send_attempts WHERE attempt_id = 'a1'"
        ),
        "queued"
    );
    writing(&store, |tx| {
        tx.execute(
            "UPDATE send_attempts SET state = 'held', hold_until = ?1
             WHERE attempt_id = 'a1'",
            [now().as_millis() + 1_000],
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        assert_eq!(queue_due_held(tx, now())?, 0);
        Ok(())
    });
    assert_eq!(
        one(
            &store,
            "SELECT state FROM send_attempts WHERE attempt_id = 'a1'"
        ),
        "held"
    );
    // A cancel that committed first wins.
    writing(&store, |tx| cancel_send(tx, "a1", now()));
    writing(&store, |tx| {
        assert_eq!(queue_due_held(tx, now())?, 0);
        Ok(())
    });
    assert_eq!(
        one(
            &store,
            "SELECT state FROM send_attempts WHERE attempt_id = 'a1'"
        ),
        "cancelled"
    );
    store.close().unwrap();
}

#[test]
fn claim_carries_the_attempt_its_recipients_and_its_parent() {
    let (_scratch, _paths, store) = scratch();
    seed(&store);
    seed_source(&store, Some("parent-1"));
    writing(&store, |tx| {
        tx.execute(
            "UPDATE drafts SET reply_to_occurrence_id = 11 WHERE draft_id = 'd1'",
            [],
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        Ok(())
    });
    queue(&store);
    force(&store, "a1", "queued", None);
    let claimed = claim(&store);
    assert_eq!(claimed.attempt_id, "a1");
    assert_eq!(
        (
            claimed.draft_id.as_str(),
            claimed.revision_id.as_str(),
            claimed.account_id.as_str(),
            claimed.identity_id.as_str(),
            claimed.envelope_from.as_str(),
            claimed.message_id_header.as_str(),
        ),
        (
            "d1",
            "r1",
            "acct",
            "id",
            "me@example.test",
            "m@example.test"
        )
    );
    assert_eq!(
        claimed.recipients,
        vec![
            ClaimedRecipient {
                address: "a@X.NET".to_owned(),
                canonical: "a@x.net".to_owned(),
                kind: RecipientKind::To,
            },
            ClaimedRecipient {
                address: "b@Y.NET".to_owned(),
                canonical: "b@y.net".to_owned(),
                kind: RecipientKind::Bcc,
            },
        ]
    );
    assert_eq!(
        claimed.parent,
        Some(SendParent {
            provider_message_id: "parent-1".to_owned(),
            kind: SendParentKind::Reply,
        })
    );
    assert_eq!(
        one(
            &store,
            "SELECT state FROM send_attempts WHERE attempt_id = 'a1'"
        ),
        "submitting"
    );
    assert!(
        writing(&store, |tx| claim_send_attempt(tx, "other", now())).is_none(),
        "another account has no claim"
    );
    assert!(
        writing(&store, |tx| claim_send_attempt(tx, "acct", now())).is_none(),
        "a submitting attempt is not claimable twice"
    );

    // A forward source reads the same way.
    let requeued = writing(&store, |tx| requeue_before_request(tx, "a1", now()));
    assert!(requeued);
    writing(&store, |tx| {
        tx.execute(
            "UPDATE drafts SET reply_to_occurrence_id = NULL, forward_of_message_id = 9
             WHERE draft_id = 'd1'",
            [],
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        Ok(())
    });
    let claimed = claim(&store);
    assert_eq!(
        claimed.parent,
        Some(SendParent {
            provider_message_id: "parent-1".to_owned(),
            kind: SendParentKind::Forward,
        })
    );
    let requeued = writing(&store, |tx| requeue_before_request(tx, "a1", now()));
    assert!(requeued);
    // A source Proton never listed has no parent id to send.
    writing(&store, |tx| {
        tx.execute(
            "UPDATE logical_messages SET provider_key = NULL WHERE message_id = 9",
            [],
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        Ok(())
    });
    assert_eq!(claim(&store).parent, None);
    store.close().unwrap();
}

#[test]
fn claim_takes_the_oldest_and_skips_a_stale_generation() {
    let (_scratch, _paths, store) = scratch();
    seed(&store);
    queue_as(&store, "a1", "r1");
    save_second_revision(&store);
    queue_as(&store, "a2", "r2");
    force(&store, "a1", "queued", None);
    force(&store, "a2", "queued", None);
    assert_eq!(claim(&store).attempt_id, "a1");
    assert_eq!(claim(&store).attempt_id, "a2");

    let (_scratch2, _paths2, store2) = scratch();
    seed(&store2);
    queue(&store2);
    force(&store2, "a1", "queued", None);
    writing(&store2, |tx| {
        tx.execute(
            "UPDATE accounts SET config_generation = 2 WHERE account_id = 'acct'",
            [],
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        Ok(())
    });
    assert!(
        writing(&store2, |tx| claim_send_attempt(tx, "acct", now())).is_none(),
        "a stale generation is skipped"
    );
    assert_eq!(
        one(
            &store2,
            "SELECT state FROM send_attempts WHERE attempt_id = 'a1'"
        ),
        "queued"
    );
    writing(&store2, |tx| confirm_send_route(tx, "a1", now()));
    assert_eq!(claim(&store2).attempt_id, "a1");
    store.close().unwrap();
    store2.close().unwrap();
}

#[test]
fn records_apply_only_while_submitting_and_survive_a_requeue() {
    let (_scratch, _paths, store) = scratch();
    seed(&store);
    queue(&store);
    force(&store, "a1", "queued", None);
    assert!(!writing(&store, |tx| record_proton_draft(tx, "a1", "pd")));
    assert!(!writing(&store, |tx| record_proton_upload(
        tx,
        "a1",
        &upload(0)
    )));
    writing(&store, |tx| {
        assert!(claim_send_attempt(tx, "acct", now())?.is_some());
        assert!(!commit_send_requested(tx, "a1", now())?);
        assert!(record_proton_draft(tx, "a1", "pd")?);
        assert!(!record_proton_draft(tx, "a1", "other")?);
        assert!(record_proton_upload(tx, "a1", &upload(0))?);
        assert!(record_proton_upload(tx, "a1", &upload(1))?);
        assert!(commit_send_requested(tx, "a1", now())?);
        assert!(requeue_before_request(tx, "a1", now())?);
        Ok(())
    });
    assert_eq!(
        one(
            &store,
            "SELECT state FROM send_attempts WHERE attempt_id = 'a1'"
        ),
        "queued"
    );
    assert_eq!(
        one_i64(
            &store,
            "SELECT count(send_requested_at) FROM send_attempts WHERE attempt_id = 'a1'"
        ),
        0
    );
    assert_eq!(
        one(
            &store,
            "SELECT proton_draft_id FROM send_attempts WHERE attempt_id = 'a1'"
        ),
        "pd"
    );
    let claimed = claim(&store);
    assert_eq!(claimed.proton_draft_id.as_deref(), Some("pd"));
    assert_eq!(claimed.uploads, vec![upload(0), upload(1)]);
    store.close().unwrap();
}

#[test]
fn startup_recovery_marks_a_requested_send_unknown_and_requeues_the_unrequested() {
    let (_scratch, _paths, store) = scratch();
    seed(&store);
    queue(&store);
    force(&store, "a1", "submitting", None);
    writing(&store, |tx| {
        tx.execute(
            "UPDATE send_attempts SET proton_draft_id = 'pd', send_requested_at = 500
             WHERE attempt_id = 'a1'",
            [],
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        Ok(())
    });
    let recovered = writing(&store, |tx| recover_proton_submitting(tx, now()));
    assert_eq!(recovered, 1, "only the requested send became unknown");
    assert_eq!(
        one(
            &store,
            "SELECT state FROM send_attempts WHERE attempt_id = 'a1'"
        ),
        "outcome_unknown"
    );
    assert_eq!(
        one_i64(
            &store,
            "SELECT count(*) FROM send_recipients
             WHERE attempt_id = 'a1' AND state = 'unknown'"
        ),
        2,
        "the recipients became unknown"
    );
    assert_eq!(
        one(&store, "SELECT state FROM drafts WHERE draft_id = 'd1'"),
        "queued",
        "the draft reads queued under the current revision"
    );

    // A second attempt never issued its send request.
    save_second_revision(&store);
    queue_as(&store, "a2", "r2");
    force(&store, "a2", "submitting", None);
    writing(&store, |tx| {
        tx.execute(
            "UPDATE send_attempts SET proton_draft_id = 'pd2' WHERE attempt_id = 'a2'",
            [],
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        Ok(())
    });
    let recovered = writing(&store, |tx| recover_proton_submitting(tx, now()));
    assert_eq!(recovered, 0, "the unrequested attempt is only requeued");
    assert_eq!(
        one(
            &store,
            "SELECT state FROM send_attempts WHERE attempt_id = 'a2'"
        ),
        "queued",
        "the unrequested attempt returns to queued"
    );
    assert_eq!(
        one(
            &store,
            "SELECT proton_draft_id FROM send_attempts WHERE attempt_id = 'a2'"
        ),
        "pd2",
        "the recorded draft is kept"
    );
    assert_eq!(
        one_i64(
            &store,
            "SELECT count(send_requested_at) FROM send_attempts WHERE attempt_id = 'a2'"
        ),
        0
    );
    store.close().unwrap();
}

#[test]
fn unknown_proton_attempts_lists_only_unresolved_ones_with_a_draft() {
    let (_scratch, _paths, store) = scratch();
    seed(&store);
    for revision in ["r2", "r3", "r4"] {
        insert_revision(&store, "d1", revision, "subject");
    }
    let seed_attempt = |attempt_id: &'static str,
                        revision_id: &'static str,
                        state: &'static str,
                        resolution: Option<&'static str>,
                        send_requested_at: Option<i64>| {
        insert_attempt(
            &store,
            SeedAttempt {
                attempt_id,
                draft_id: "d1",
                revision_id,
                state,
                config_generation: 1,
                created_at: 1,
                updated_at: 1,
                resolution,
                resolved_at: resolution.map(|_| 1),
                final_reply: None,
                hold_until: None,
                send_requested_at,
            },
        );
    };
    seed_attempt("a1", "r1", "outcome_unknown", None, Some(1));
    seed_attempt("a2", "r2", "outcome_unknown", Some("marked_sent"), Some(1));
    seed_attempt("a3", "r3", "queued", None, Some(1));
    seed_attempt("a4", "r4", "outcome_unknown", None, None);

    let unknown = reading(&store, |tx| unknown_proton_attempts(tx, "acct"));
    assert_eq!(
        unknown,
        vec![UnknownSend {
            attempt_id: "a1".to_owned(),
            proton_draft_id: "pd".to_owned(),
        }],
        "only the unresolved attempt with a recorded draft"
    );
    store.close().unwrap();
}

#[test]
fn accept_moves_recipients_and_counts_each_once() {
    let (_scratch, _paths, store) = scratch();
    seed(&store);
    writing(&store, |tx| {
        tx.execute(
            "INSERT INTO seen_addresses
             (canonical, email, display_name, last_seen_at, times_sent_to, times_received,
              last_account_id)
             VALUES ('a@x.net', 'old@x.net', 'Old', 5, 4, 2, NULL)",
            [],
        )
        .map_err(|e| StoreError::internal(e.to_string()))?;
        Ok(())
    });
    queue(&store);
    force(&store, "a1", "queued", None);
    claim(&store);
    assert!(accept(&store));
    assert_eq!(
        one(
            &store,
            "SELECT state FROM send_attempts WHERE attempt_id = 'a1'"
        ),
        "accepted"
    );
    assert_eq!(
        one_i64(
            &store,
            "SELECT count(*) FROM send_recipients
             WHERE attempt_id = 'a1' AND state = 'sent'"
        ),
        2
    );
    assert_eq!(
        one(&store, "SELECT state FROM drafts WHERE draft_id = 'd1'"),
        "sent"
    );
    // `a@X.NET` and `a@x.net` are one recipient, counted once.
    assert_eq!(
        one(
            &store,
            "SELECT times_sent_to || '|' || email || '|' || last_account_id
             FROM seen_addresses WHERE canonical = 'a@x.net'"
        ),
        "5|old@x.net|acct"
    );
    assert!(
        !accept(&store),
        "a second acceptance doesn't apply or count again"
    );
    store.close().unwrap();
}

#[test]
fn accept_resolves_an_unknown_attempt() {
    let (_scratch, _paths, store) = scratch();
    seed(&store);
    queue(&store);
    force(&store, "a1", "outcome_unknown", Some("unknown"));
    assert!(accept(&store));
    assert_eq!(
        one_i64(
            &store,
            "SELECT count(*) FROM send_recipients
             WHERE attempt_id = 'a1' AND state = 'sent'"
        ),
        2
    );
    assert_eq!(
        one(&store, "SELECT state FROM drafts WHERE draft_id = 'd1'"),
        "sent"
    );
    store.close().unwrap();
}

#[test]
fn fail_records_the_reply_the_rejection_and_editing() {
    let (_scratch, _paths, store) = scratch();
    seed(&store);
    queue(&store);
    force(&store, "a1", "queued", None);
    claim(&store);
    assert!(fail(
        &store,
        SendFailure {
            code: Some(2064),
            message: "identity refused".to_owned(),
            rejected: None,
        }
    ));
    assert_eq!(
        one(
            &store,
            "SELECT final_reply FROM send_attempts WHERE attempt_id = 'a1'"
        ),
        "2064 identity refused"
    );
    assert_eq!(
        one_i64(
            &store,
            "SELECT count(*) FROM send_recipients
             WHERE attempt_id = 'a1' AND state = 'unsent'"
        ),
        2
    );
    assert_eq!(
        one(&store, "SELECT state FROM drafts WHERE draft_id = 'd1'"),
        "editing"
    );
    assert!(!fail(
        &store,
        SendFailure {
            code: None,
            message: "again".into(),
            rejected: None
        }
    ));

    // The external-with-keys shape: one recipient refused permanently,
    // the others unsent, the draft back to editing.
    let (_scratch, _paths, store) = scratch();
    seed(&store);
    queue(&store);
    force(&store, "a1", "queued", None);
    claim(&store);
    assert!(fail(
        &store,
        SendFailure {
            code: None,
            message: "external recipient has keys".to_owned(),
            rejected: Some(SendFailureRejection {
                canonical: "a@x.net".to_owned(),
                reason: "keys known; clear text refused".to_owned(),
            }),
        }
    ));
    assert_eq!(
        one(
            &store,
            "SELECT state || '|' || rcpt_reply FROM send_recipients
             WHERE attempt_id = 'a1' AND canonical = 'a@x.net'"
        ),
        "rejected_permanent|keys known; clear text refused"
    );
    assert_eq!(
        one(
            &store,
            "SELECT state FROM send_recipients
             WHERE attempt_id = 'a1' AND canonical = 'b@y.net'"
        ),
        "unsent"
    );
    assert_eq!(
        one(&store, "SELECT state FROM drafts WHERE draft_id = 'd1'"),
        "editing"
    );
    store.close().unwrap();
}

#[test]
fn fail_refuses_a_stranger_as_rejected_before_changing_anything() {
    let (_scratch, _paths, store) = scratch();
    seed(&store);
    queue(&store);
    force(&store, "a1", "queued", None);
    claim(&store);
    writing(&store, |tx| {
        let failure = SendFailure {
            code: None,
            message: "external recipient has keys".to_owned(),
            rejected: Some(SendFailureRejection {
                canonical: "stranger@z.net".to_owned(),
                reason: "keys known; clear text refused".to_owned(),
            }),
        };
        assert!(matches!(
            mark_send_failed(tx, "a1", &failure, now()),
            Err(StoreError::InvalidInput { .. })
        ));
        // Inside the same transaction: nothing moved, so a caller that
        // goes on to record the failure without the stranger can.
        let state: String = tx
            .query_row(
                "SELECT state FROM send_attempts WHERE attempt_id = 'a1'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| StoreError::internal(e.to_string()))?;
        assert_eq!(state, "submitting");
        let unsent: i64 = tx
            .query_row(
                "SELECT count(*) FROM send_recipients
                 WHERE attempt_id = 'a1' AND state = 'unsent'",
                [],
                |row| row.get(0),
            )
            .map_err(|e| StoreError::internal(e.to_string()))?;
        assert_eq!(unsent, 0);
        Ok(())
    });
    store.close().unwrap();
}

#[test]
fn unknown_marks_recipients_and_refuses_a_second_transition() {
    let (_scratch, _paths, store) = scratch();
    seed(&store);
    queue(&store);
    force(&store, "a1", "queued", None);
    claim(&store);
    assert!(unknown(&store));
    assert_eq!(
        one(
            &store,
            "SELECT state FROM send_attempts WHERE attempt_id = 'a1'"
        ),
        "outcome_unknown"
    );
    assert_eq!(
        one_i64(
            &store,
            "SELECT count(*) FROM send_recipients
             WHERE attempt_id = 'a1' AND state = 'unknown'"
        ),
        2
    );
    assert_eq!(
        one(&store, "SELECT state FROM drafts WHERE draft_id = 'd1'"),
        "queued"
    );
    assert!(
        !unknown(&store),
        "an unknown attempt doesn't go unknown twice"
    );
    store.close().unwrap();
}

#[test]
fn transitions_leave_a_newer_revision_alone() {
    for state in ["accepted", "failed", "outcome_unknown"] {
        for queue_r2 in [false, true] {
            let (_scratch, _paths, store) = scratch();
            seed(&store);
            queue(&store);
            force(&store, "a1", "queued", None);
            save_second_revision(&store);
            if queue_r2 {
                queue_as(&store, "a2", "r2");
                force(&store, "a2", "queued", None);
            }
            claim(&store);
            match state {
                "accepted" => assert!(accept(&store)),
                "failed" => assert!(fail(
                    &store,
                    SendFailure {
                        code: None,
                        message: "no".to_owned(),
                        rejected: None,
                    }
                )),
                _ => assert!(unknown(&store)),
            }
            let expected = if queue_r2 { "queued" } else { "editing" };
            assert_eq!(
                one(&store, "SELECT state FROM drafts WHERE draft_id = 'd1'"),
                expected
            );
            store.close().unwrap();
        }
    }
}

#[test]
fn frozen_bytes_read_by_sha_returns_the_published_bytes() {
    let (_scratch, _paths, store) = scratch();
    let object = publish(&store);
    let sha = object.id().sha256_hex();
    let mut reader = store.objects().open_by_sha256(&sha).unwrap();
    let mut bytes = Vec::new();
    reader.read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, b"frozen mime");

    // A file at another hash's path fails verification.
    let wrong = crate::objects::ObjectId::from_parts([7u8; 32], b"frozen mime".len() as u64);
    let path = store.objects().object_path(&wrong);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, b"frozen mime").unwrap();
    let mut reader = store.objects().open_by_sha256(&wrong.sha256_hex()).unwrap();
    let mut bytes = Vec::new();
    let error = reader.read_to_end(&mut bytes).unwrap_err();
    assert!(
        matches!(
            crate::objects::read_error_as_store_error(&error),
            Some(StoreError::Corrupt { .. })
        ),
        "{error:?}"
    );
    store.close().unwrap();
}
