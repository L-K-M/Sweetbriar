use std::sync::Arc;

use rusqlite::{Connection, params};
use sb_test_support::{FaultFs, FaultKind, FsOp, ScratchDir, real_fs};
use sb_types::fs::Fs;

use super::*;
use crate::StorePaths;
use crate::schema::{MIGRATIONS, run_migrations};
use crate::testing::{scratch_paths, test_seams, writing};

const ORIGINAL: &[u8] = b"Subject: Synthetic\r\n\r\nExact body\x00\xff";

fn database() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys = ON").unwrap();
    run_migrations(&mut conn, &test_seams(real_fs()), MIGRATIONS).unwrap();
    seed(&conn);
    conn
}

fn seed(conn: &Connection) {
    conn.execute_batch(
        "INSERT INTO accounts (account_id, kind, provider, display_name, state, sort_order, created_at)
         VALUES ('home', 'imap', 'proton_bridge', 'Home', 'active', 1, 0),
                ('work', 'imap', 'proton_bridge', 'Work', 'active', 2, 0);
         INSERT INTO mailboxes (mailbox_id, account_id, remote_name, kind, selectable, sync_state)
         VALUES (10, 'home', 'Inbox', 'folder', 1, 'synced'),
                (11, 'home', 'Label', 'label', 1, 'synced'),
                (12, 'work', 'Inbox', 'folder', 1, 'synced');
         INSERT INTO logical_messages (message_id, account_id, created_at)
         VALUES (20, 'home', 0), (21, 'work', 0), (22, 'home', 0);
         INSERT INTO occurrences (occurrence_id, account_id, mailbox_id, message_id,
          state, first_seen_at, last_seen_at)
         VALUES (30, 'home', 10, 20, 'present', 0, 0),
                (31, 'home', 11, 20, 'present', 0, 0),
                (32, 'home', 10, 20, 'vanished', 0, 0),
                (33, 'work', 12, 21, 'present', 0, 0),
                (34, 'home', 10, 22, 'vanish_pending', 0, 0),
                (35, 'home', 10, 22, 'epoch_stale', 0, 0),
                (36, 'home', 10, 22, 'superseded', 0, 0);
         INSERT INTO message_headers (message_id, raw_fields, from_json, to_json, cc_json,
          subject, header_message_id, date_header, fetched_at)
         VALUES (20, X'', '[{\"name\":\"Sync\",\"address\":\"sync@example.test\"}]',
          '[{\"address\":\"to@example.test\"}]', '[{\"name\":\"Copy\"}]',
          'Sync subject', '<sync@example.test>', 123, 0);
         INSERT INTO d_threads (thread_id) VALUES (40);
         INSERT INTO d_thread_members (thread_id, message_id) VALUES (40, 20);
         INSERT INTO tags (tag_id, name, color_srgb) VALUES ('b', 'Beta', NULL), ('a', 'Alpha', '#123456');
         INSERT INTO message_tags (message_id, tag_id, added_at) VALUES (20, 'b', 0), (20, 'a', 0);
         INSERT INTO notes (note_id, message_id, body_source, created_at, updated_at)
         VALUES ('note', 20, 'Synthetic note', 0, 0);"
    ).unwrap();
}

fn attach(conn: &Connection, object: &ObjectId) {
    conn.execute(
        "INSERT INTO acquisitions (acquisition_id, content_sha256, byte_length, object_state,
          first_acquired_at) VALUES (50, ?1, ?2, 'published', 0)",
        params![
            object.sha256_hex(),
            i64::try_from(object.byte_length()).unwrap()
        ],
    )
    .unwrap();
    conn.execute(
        "UPDATE logical_messages SET acquisition_id = 50 WHERE message_id IN (20, 21)",
        [],
    )
    .unwrap();
}

#[test]
fn metadata_and_occurrence_mapping_are_logical_message_scoped() {
    let mut conn = database();
    let tx = conn.transaction().unwrap();
    let message = get_message(&tx, 20).unwrap();
    assert_eq!(message.message_id, 20);
    assert_eq!(message.account_id, "home");
    assert_eq!(message.account_display_name, "Home");
    assert_eq!(message.thread_id, Some(40));
    assert_eq!(message.note.as_deref(), Some("Synthetic note"));
    assert_eq!(
        message.tags,
        vec![
            ReaderTag {
                tag_id: "a".into(),
                name: "Alpha".into(),
                color_srgb: Some("#123456".into())
            },
            ReaderTag {
                tag_id: "b".into(),
                name: "Beta".into(),
                color_srgb: None
            },
        ]
    );
    assert_eq!(
        message.occurrences,
        vec![
            ReaderOccurrence {
                occurrence_id: 30,
                mailbox_id: 10,
                mailbox_name: "Inbox".into()
            },
            ReaderOccurrence {
                occurrence_id: 31,
                mailbox_id: 11,
                mailbox_name: "Label".into()
            },
        ]
    );
    assert_eq!(message.body_availability, BodyAvailability::NotAcquired);
    assert_eq!(best_acquisition(&tx, 20).unwrap(), None);
    for occurrence in [30, 31, 32] {
        assert_eq!(message_for_occurrence(&tx, occurrence).unwrap(), 20);
    }
    assert_eq!(message_for_occurrence(&tx, 33).unwrap(), 21);
    let empty = get_message(&tx, 22).unwrap();
    assert!(empty.occurrences.is_empty());
    assert!(empty.tags.is_empty());
    assert_eq!(empty.note, None);
    assert_eq!(empty.thread_id, None);
    assert_eq!(empty.headers, ReaderHeaders::default());
    assert_eq!(empty.body_availability, BodyAvailability::NotAcquired);
}

#[test]
fn unknown_ids_are_not_found() {
    let mut conn = database();
    let tx = conn.transaction().unwrap();
    for message in [-1, 0, 999] {
        assert!(matches!(
            get_message(&tx, message),
            Err(StoreError::NotFound { .. })
        ));
        assert!(matches!(
            best_acquisition(&tx, message),
            Err(StoreError::NotFound { .. })
        ));
        assert!(matches!(
            message_for_occurrence(&tx, message),
            Err(StoreError::NotFound { .. })
        ));
    }
}

#[test]
fn summary_row_takes_precedence_even_for_unknown_fields() {
    let mut conn = database();
    let tx = conn.transaction().unwrap();
    assert_eq!(
        get_message(&tx, 20).unwrap().headers,
        ReaderHeaders {
            from: vec![ReaderAddress {
                name: Some("Sync".into()),
                address: Some("sync@example.test".into())
            }],
            to: vec![ReaderAddress {
                name: None,
                address: Some("to@example.test".into())
            }],
            cc: vec![ReaderAddress {
                name: Some("Copy".into()),
                address: None
            }],
            subject: Some("Sync subject".into()),
            message_id: Some("<sync@example.test>".into()),
            date: Some(123),
        }
    );
    attach(&tx, &ObjectId::from_parts([0; 32], 0));
    // No summary yet: sync headers still supply the reader.
    assert_eq!(
        get_message(&tx, 20).unwrap().headers.subject.as_deref(),
        Some("Sync subject")
    );
    tx.execute_batch(
        "INSERT INTO d_message_summary (acquisition_id, parser_version, parse_state,
         from_addr, to_json, cc_json, subject, header_message_id, date_header, has_attachments, snippet)
         VALUES (50, 1, 'partial', 'parsed@example.test', '[{\"name\":\"Parsed to\"}]',
         '[{\"address\":\"parsed-copy@example.test\"}]', 'Parsed subject', '<parsed@example.test>', 456, 0,
         'Never use this body preview');"
    ).unwrap();
    assert_eq!(
        get_message(&tx, 20).unwrap().headers,
        ReaderHeaders {
            from: vec![ReaderAddress {
                name: None,
                address: Some("parsed@example.test".into())
            }],
            to: vec![ReaderAddress {
                name: Some("Parsed to".into()),
                address: None
            }],
            cc: vec![ReaderAddress {
                name: None,
                address: Some("parsed-copy@example.test".into())
            }],
            subject: Some("Parsed subject".into()),
            message_id: Some("<parsed@example.test>".into()),
            date: Some(456),
        }
    );
    tx.execute_batch(
        "UPDATE d_message_summary SET from_addr = NULL, to_json = NULL, cc_json = NULL,
        subject = NULL, header_message_id = NULL, date_header = NULL",
    )
    .unwrap();
    assert_eq!(
        get_message(&tx, 20).unwrap().headers,
        ReaderHeaders::default()
    );
}

#[test]
fn header_budget_is_aggregate_and_utf8_bytes() {
    let mut conn = database();
    let tx = conn.transaction().unwrap();
    tx.execute_batch(
        "UPDATE message_headers SET from_json = NULL, to_json = NULL, cc_json = NULL,
        header_message_id = NULL",
    )
    .unwrap();
    let subject = "é".repeat(MAX_STORED_HEADER_BYTES / 2);
    tx.execute("UPDATE message_headers SET subject = ?1", [&subject])
        .unwrap();
    assert_eq!(get_message(&tx, 20).unwrap().headers.subject, Some(subject));
    tx.execute("UPDATE message_headers SET header_message_id = 'x'", [])
        .unwrap();
    assert!(matches!(
        get_message(&tx, 20),
        Err(StoreError::Corrupt { .. })
    ));
    attach(&tx, &ObjectId::from_parts([0; 32], 0));
    tx.execute(
        "INSERT INTO d_message_summary (acquisition_id, parser_version, parse_state,
        subject, from_name, has_attachments) VALUES (50, 1, 'ok', ?1, 'x', 0)",
        ["x".repeat(MAX_STORED_HEADER_BYTES)],
    )
    .unwrap();
    assert!(matches!(
        get_message(&tx, 20),
        Err(StoreError::Corrupt { .. })
    ));
}

/// The import writer stores an admitted header block as JSON, which is
/// larger: `a,` becomes `{"address":"a","name":null},`. A full block of
/// such recipients reads back whole.
#[test]
fn stored_json_of_an_admitted_header_block_reads_back() {
    let mut conn = database();
    let tx = conn.transaction().unwrap();
    let recipients = MAX_READER_HEADER_BYTES / "a,".len();
    let json = format!(
        "[{}]",
        vec![r#"{"address":"a","name":null}"#; recipients].join(",")
    );
    assert!(json.len() > MAX_READER_HEADER_BYTES);
    tx.execute("UPDATE message_headers SET to_json = ?1", [&json])
        .unwrap();
    assert_eq!(get_message(&tx, 20).unwrap().headers.to.len(), recipients);
}

#[test]
fn malformed_address_data_fails_explicitly() {
    let mut conn = database();
    let tx = conn.transaction().unwrap();
    for json in ["{", "{}", "[1]", "[{\"name\":3}]", "[{\"address\":false}]"] {
        tx.execute("UPDATE message_headers SET to_json = ?1", [json])
            .unwrap();
        assert!(matches!(
            get_message(&tx, 20),
            Err(StoreError::Corrupt { .. })
        ));
    }
}

fn stored_acquisition(store: &Store) -> StoredAcquisition {
    store.read(|tx| best_acquisition(tx, 20)).unwrap().unwrap()
}

#[test]
fn shared_acquisition_keeps_each_accounts_identity_and_exact_bytes() {
    let (_scratch, paths) = scratch_paths("reader-shared");
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    let object = store.objects().publish(ORIGINAL).unwrap();
    writing(&store, move |tx| {
        seed(tx);
        attach(tx, &object);
        Ok(())
    });
    store
        .read(|tx| {
            let home = get_message(tx, 20)?;
            let work = get_message(tx, 21)?;
            assert_eq!(
                (home.account_id.as_str(), home.account_display_name.as_str()),
                ("home", "Home")
            );
            assert_eq!(
                (work.account_id.as_str(), work.account_display_name.as_str()),
                ("work", "Work")
            );
            assert_eq!(work.occurrences[0].occurrence_id, 33);
            assert!(work.tags.is_empty());
            assert_eq!(work.note, None);
            assert_eq!(home.body_availability, BodyAvailability::Acquired);
            assert_eq!(best_acquisition(tx, 20)?, best_acquisition(tx, 21)?);
            Ok(())
        })
        .unwrap();
    let acquisition = stored_acquisition(&store);
    assert_eq!(acquisition.acquisition_id(), 50);
    assert_eq!(*acquisition.object(), object);
    assert_eq!(store.read_acquisition(&acquisition).unwrap(), ORIGINAL);
    store.close().unwrap();
}

/// Each message reads the acquisition it records, not the newest one.
#[test]
fn each_message_reads_its_own_acquisition() {
    let (_scratch, paths) = scratch_paths("reader-selected");
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    let older = store.objects().publish(ORIGINAL).unwrap();
    let newer = store
        .objects()
        .publish(&b"Subject: Newer\r\n\r\nOther body"[..])
        .unwrap();
    writing(&store, move |tx| {
        seed(tx);
        for (id, object, acquired_at) in [(50, &older, 1), (51, &newer, 2)] {
            tx.execute(
                "INSERT INTO acquisitions (acquisition_id, content_sha256, byte_length,
                  object_state, first_acquired_at) VALUES (?1, ?2, ?3, 'published', ?4)",
                params![
                    id,
                    object.sha256_hex(),
                    i64::try_from(object.byte_length()).unwrap(),
                    acquired_at
                ],
            )
            .unwrap();
        }
        tx.execute_batch(
            "INSERT INTO d_message_summary (acquisition_id, parser_version, parse_state,
              subject, has_attachments)
             VALUES (50, 1, 'ok', 'Older', 0), (51, 1, 'ok', 'Newer', 0);
             UPDATE logical_messages SET acquisition_id = 50 WHERE message_id = 20;
             UPDATE logical_messages SET acquisition_id = 51 WHERE message_id = 22;",
        )
        .unwrap();
        Ok(())
    });

    let (message, acquisition) = store
        .read(|tx| Ok((get_message(tx, 20)?, best_acquisition(tx, 20)?.unwrap())))
        .unwrap();
    assert_eq!(message.headers.subject.as_deref(), Some("Older"));
    assert_eq!(acquisition.acquisition_id(), 50);
    assert_eq!(store.read_acquisition(&acquisition).unwrap(), ORIGINAL);
    store.close().unwrap();
}

#[test]
fn missing_and_same_length_corrupt_objects_never_return_bytes() {
    for corrupt in [false, true] {
        let (_scratch, paths) = scratch_paths("reader-damage");
        let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
        let object = store.objects().publish(ORIGINAL).unwrap();
        writing(&store, move |tx| {
            seed(tx);
            attach(tx, &object);
            Ok(())
        });
        let acquisition = stored_acquisition(&store);
        let hash = object.sha256_hex();
        let path = paths
            .objects_sha256()
            .join(&hash[..2])
            .join(&hash[2..4])
            .join(&hash);
        if corrupt {
            // Same length passes open's length check; only the EOF hash catches it.
            std::fs::write(&path, vec![b'x'; ORIGINAL.len()]).unwrap();
            assert!(matches!(
                store.read_acquisition(&acquisition),
                Err(StoreError::Corrupt { .. })
            ));
            assert!(!path.exists());
            assert!(
                std::fs::read_dir(paths.recovery())
                    .unwrap()
                    .next()
                    .is_some()
            );
        } else {
            std::fs::remove_file(&path).unwrap();
            assert!(matches!(
                store.read_acquisition(&acquisition),
                Err(StoreError::NotFound { .. })
            ));
        }
        store.close().unwrap();
    }
}

#[test]
fn recorded_object_states_and_oversize_reads_fail_before_io() {
    let (_scratch, paths) = scratch_paths("reader-states");
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    writing(&store, |tx| {
        seed(tx);
        attach(
            tx,
            &ObjectId::from_parts([0; 32], MAX_READER_MESSAGE_BYTES + 1),
        );
        Ok(())
    });
    let Err(oversize) = store.read_acquisition(&stored_acquisition(&store)) else {
        panic!("an over-limit acquisition read clean")
    };
    assert!(
        matches!(oversize, StoreError::Unavailable { .. }),
        "{oversize:?}"
    );
    // A size refusal decided before any I/O carries no kind.
    assert_eq!(oversize.io_kind(), None);
    for (state, availability) in [
        ("missing", BodyAvailability::AcquisitionMissing),
        ("quarantined", BodyAvailability::AcquisitionCorrupt),
    ] {
        writing(&store, move |tx| {
            tx.execute("UPDATE acquisitions SET object_state = ?1", [state])
                .unwrap();
            Ok(())
        });
        let acquisition = stored_acquisition(&store);
        assert_eq!(acquisition.availability(), availability);
        assert_eq!(
            store
                .read(|tx| get_message(tx, 20))
                .unwrap()
                .body_availability,
            availability
        );
        match availability {
            BodyAvailability::AcquisitionMissing => assert!(matches!(
                store.read_acquisition(&acquisition),
                Err(StoreError::NotFound { .. })
            )),
            BodyAvailability::AcquisitionCorrupt => assert!(matches!(
                store.read_acquisition(&acquisition),
                Err(StoreError::Corrupt { .. })
            )),
            _ => unreachable!(),
        }
    }
    store.close().unwrap();
}

#[test]
fn acquisition_metadata_is_validated() {
    for (hash, length, state) in [
        ("x".repeat(64), 0, "published"),
        ("0".repeat(63), 0, "published"),
        ("0".repeat(64), -1, "published"),
        ("0".repeat(64), 0, "unknown"),
    ] {
        assert!(matches!(
            acquisition(50, &hash, length, state),
            Err(StoreError::Corrupt { .. })
        ));
    }
}

#[test]
fn exact_message_limit_is_admitted_and_verified() {
    let (_scratch, paths) = scratch_paths("reader-limit");
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    let object = store
        .objects()
        .publish(std::io::repeat(b'x').take(MAX_READER_MESSAGE_BYTES))
        .unwrap();
    writing(&store, move |tx| {
        seed(tx);
        attach(tx, &object);
        Ok(())
    });
    let bytes = store.read_acquisition(&stored_acquisition(&store)).unwrap();
    assert_eq!(bytes.len() as u64, MAX_READER_MESSAGE_BYTES);
    assert!(bytes.iter().all(|byte| *byte == b'x'));
    store.close().unwrap();
}

/// A reader store over `fs`, seeded with one published acquisition
/// attached to message 20.
fn store_with_acquisition(
    fs: Arc<dyn Fs>,
    label: &str,
) -> (ScratchDir, StorePaths, Store, StoredAcquisition) {
    let (scratch, paths) = scratch_paths(label);
    let store = crate::open(&paths, &test_seams(fs)).unwrap();
    let object = store.objects().publish(ORIGINAL).unwrap();
    writing(&store, move |tx| {
        seed(tx);
        attach(tx, &object);
        Ok(())
    });
    let acquisition = stored_acquisition(&store);
    (scratch, paths, store, acquisition)
}

/// Injected object-store failures keep their native kinds through
/// `read_acquisition` (spec/03 "Search writes"): a refused `open` and
/// a mid-stream raw read each land in `Unavailable`'s `io_kind`.
#[test]
fn acquisition_io_failures_keep_their_native_kinds() {
    for (op, nth, kind) in [
        (FsOp::Open, 1, std::io::ErrorKind::PermissionDenied),
        (FsOp::Read, 2, std::io::ErrorKind::StorageFull),
    ] {
        let fs: Arc<dyn Fs> = Arc::new(FaultFs::new(real_fs(), op, nth, FaultKind::Kind(kind)));
        let (_scratch, _paths, store, acquisition) = store_with_acquisition(fs, "reader-io-kind");
        let Err(error) = store.read_acquisition(&acquisition) else {
            panic!("{op:?}: the injected fault did not fire")
        };
        assert!(
            matches!(error, StoreError::Unavailable { .. }),
            "{op:?}: {error:?}"
        );
        assert_eq!(error.io_kind(), Some(kind), "{op:?}");
        assert_eq!(store.read_acquisition(&acquisition).unwrap(), ORIGINAL);
        store.close().unwrap();
    }
}

/// A failed quarantine's kind survives being wrapped in the reader's
/// `io::Error` and recovered; the `Corrupt` a verified mismatch
/// reports carries none.
#[test]
fn acquisition_quarantine_failure_keeps_its_kind() {
    // The publish makes rename 1; the quarantine's is rename 2.
    let fs: Arc<dyn Fs> = Arc::new(FaultFs::new(
        real_fs(),
        FsOp::Rename,
        2,
        FaultKind::Kind(std::io::ErrorKind::ResourceBusy),
    ));
    let (_scratch, paths, store, acquisition) =
        store_with_acquisition(fs, "reader-quarantine-kind");
    // Damage in place at the same length: only the EOF hash catches it.
    let hex = acquisition.object().sha256_hex();
    let path = paths
        .objects_sha256()
        .join(&hex[..2])
        .join(&hex[2..4])
        .join(&hex);
    std::fs::write(&path, vec![b'x'; ORIGINAL.len()]).unwrap();
    let Err(error) = store.read_acquisition(&acquisition) else {
        panic!("the injected quarantine fault did not fire")
    };
    assert!(matches!(error, StoreError::Unavailable { .. }), "{error:?}");
    assert_eq!(error.io_kind(), Some(std::io::ErrorKind::ResourceBusy));
    // The next read retries the quarantine and reports `Corrupt`,
    // which carries no kind.
    let Err(error) = store.read_acquisition(&acquisition) else {
        panic!("the damaged object read clean")
    };
    assert!(matches!(error, StoreError::Corrupt { .. }), "{error:?}");
    assert_eq!(error.io_kind(), None);
    store.close().unwrap();
}
