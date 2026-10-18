use std::io::Read;

use rusqlite::params;
use sb_test_support::{ScratchDir, block_on, real_fs};

use super::*;
use crate::testing::{FIXED_NOW_MS, reading, scratch_paths, test_seams, writing};
use crate::{ObjectStore, Store, StorePaths, Timestamp, WritePriority};

const JOB: &str = "00000000-0000-4000-8000-000000000001";
const SOURCE_HASH: &str = "3a4e94e39a4c23bdaeb5df6092bc4ad83bec3e915c16562740edd4495013bb2f";
const ORIGINAL: &[u8] =
    b"From: sender@example.test\nSubject: Duplicate source message\n\nKeep both occurrences.\n";

fn now() -> Timestamp {
    Timestamp::from_millis(FIXED_NOW_MS)
}

fn setup() -> (ScratchDir, StorePaths, Store, i64) {
    let (scratch, paths) = scratch_paths("import");
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    let revision = writing(&store, |tx| {
        create_import_job(tx, JOB, "{\"paths\":[],\"v\":1}", now())?;
        record_import_source(tx, JOB, 0, "{\"path\":\"/synthetic/Inbox.mbox\",\"v\":1}")?;
        advance_import_job(
            tx,
            JOB,
            1,
            ImportJobState::AwaitingConfirmation,
            "{\"v\":1}",
            now(),
        )?;
        Ok(advance_import_job(tx, JOB, 2, ImportJobState::Running, "{\"v\":1}", now())?.revision())
    });
    (scratch, paths, store, revision)
}

fn message(store: &Store, path: &str, offset: u64, bytes: &[u8]) -> ImportMessage {
    ImportMessage {
        object: store.objects().publish(bytes).unwrap(),
        source_kind: ImportSourceKind::Mbox,
        source_path: path.to_owned(),
        source_sha256: SOURCE_HASH.to_owned(),
        offset,
        flags: ImportFlags::default(),
        summary: ImportSummary {
            parser_version: 1,
            parse_state: ImportParseState::Parsed,
            subject: Some("Duplicate source message".to_owned()),
            to_json: Some(
                "[ { \"name\": \"Reader\", \"address\": \"reader@example.test\" } ]".to_owned(),
            ),
            ..ImportSummary::default()
        },
    }
}

fn commit(
    store: &Store,
    revision: i64,
    mailbox: &str,
    messages: Vec<ImportMessage>,
) -> Result<ImportBatchStats, StoreError> {
    let mailbox = mailbox.to_owned();
    block_on(store.write(WritePriority::Sync, move |tx| {
        commit_import_batch(
            tx,
            &ImportBatch {
                job_id: JOB,
                expected_revision: revision,
                source_ordinal: 0,
                mailbox_name: &mailbox,
                messages: &messages,
            },
            now(),
            |stats| {
                let checkpoint = serde_json::json!({"v":1,"processed":stats.processed}).to_string();
                Ok((checkpoint.clone(), checkpoint))
            },
        )
    }))
}

fn count(store: &Store, from_clause: &str) -> i64 {
    reading(store, |tx| {
        Ok(tx
            .query_row(&format!("SELECT count(*) FROM {from_clause}"), [], |r| {
                r.get(0)
            })
            .unwrap())
    })
}

#[test]
fn identical_bytes_keep_distinct_occurrences_but_replay_keeps_counts() {
    let (_scratch, paths, store, revision) = setup();
    let first = message(&store, "/synthetic/Inbox.mbox", 0, ORIGINAL);
    let second = message(&store, "/synthetic/Inbox.mbox", 128, ORIGINAL);
    let object = first.object;
    let stats = commit(&store, revision, "Inbox", vec![first, second]).unwrap();
    assert_eq!(
        (
            stats.processed,
            stats.imported,
            stats.duplicates,
            stats.bytes_stored
        ),
        (2, 2, 1, ORIGINAL.len() as u64)
    );
    for (table, expected) in [
        ("acquisitions", 1),
        ("acquisition_sources", 2),
        ("logical_messages", 2),
        ("occurrences", 2),
        ("d_message_summary", 1),
        ("d_index_queue", 1),
    ] {
        assert_eq!(count(&store, table), expected, "{table}");
    }
    let mut bytes = Vec::new();
    store
        .objects()
        .open(&object)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(bytes, ORIGINAL);
    let stored_hash: String = reading(&store, |tx| {
        tx.query_row("SELECT content_sha256 FROM acquisitions", [], |row| {
            row.get(0)
        })
        .map_err(sql_error)
    });
    assert_eq!(stored_hash, object.sha256_hex());
    store.close().unwrap();
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    let job = reading(&store, |tx| import_job(tx, JOB));
    assert_eq!(job.revision(), revision + 1);
    assert_eq!(job.checkpoint_json(), "{\"processed\":2,\"v\":1}");
    let source = reading(&store, |tx| import_job_source(tx, JOB, 0));
    assert_eq!(source.checkpoint_json(), job.checkpoint_json());
    writing(&store, |tx| {
        tx.execute("UPDATE acquisitions SET object_state = 'missing'", [])
            .map_err(sql_error)?;
        Ok(())
    });
    let stats = commit(
        &store,
        job.revision(),
        "Inbox",
        vec![
            message(&store, "/synthetic/Inbox.mbox", 0, ORIGINAL),
            message(&store, "/synthetic/Inbox.mbox", 128, ORIGINAL),
        ],
    )
    .unwrap();
    assert_eq!(
        (stats.imported, stats.duplicates, stats.bytes_stored),
        (0, 2, 0)
    );
    assert_eq!(count(&store, "occurrences"), 2);
    assert_eq!(
        count(&store, "acquisitions WHERE object_state = 'published'"),
        1
    );
}

#[test]
fn distinct_paths_and_mailboxes_are_distinct_sources() {
    let (_scratch, _paths, store, revision) = setup();
    commit(
        &store,
        revision,
        "Inbox",
        vec![message(&store, "/synthetic/Inbox.mbox", 0, ORIGINAL)],
    )
    .unwrap();
    let stats = commit(
        &store,
        revision + 1,
        "Archive",
        vec![message(&store, "/synthetic/Archive.mbox", 0, ORIGINAL)],
    )
    .unwrap();
    assert_eq!((stats.imported, stats.duplicates), (1, 1));
    assert_eq!(count(&store, "acquisitions"), 1);
    assert_eq!(count(&store, "mailboxes"), 2);
    assert_eq!(count(&store, "occurrences"), 2);
    assert_eq!(count(&store, "logical_messages"), 2);
}

#[test]
fn flags_failed_parsing_and_canonical_summary_are_persisted() {
    let (_scratch, _paths, store, revision) = setup();
    let mut unread = message(&store, "/synthetic/Inbox.mbox", 0, ORIGINAL);
    unread.flags = ImportFlags {
        read: ImportReadState::FromHeaders { seen: false },
        flagged: true,
        answered: true,
        draft: true,
        deleted: true,
    };
    let mut failed = message(&store, "/synthetic/Inbox.mbox", 128, b"");
    failed.summary = ImportSummary {
        parser_version: 1,
        ..ImportSummary::default()
    };
    let stats = commit(&store, revision, "Inbox", vec![unread, failed]).unwrap();
    assert_eq!(
        (stats.rejected, stats.read_from_headers, stats.read_default),
        (1, 1, 1)
    );
    reading(&store, |tx| {
        let mut stmt = tx.prepare("SELECT f_seen, f_flagged, f_answered, f_draft, f_deleted FROM occurrences ORDER BY occurrence_id").unwrap();
        let flags: Vec<(bool, bool, bool, bool, bool)> = stmt
            .query_map([], |r| {
                Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?, r.get(4)?))
            })
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(
            flags,
            [
                (false, true, true, true, true),
                (true, false, false, false, false)
            ]
        );
        let (state, bytes): (String, i64) = tx.query_row("SELECT s.parse_state, a.byte_length FROM d_message_summary s JOIN acquisitions a USING(acquisition_id) WHERE a.byte_length = 0", [], |r| Ok((r.get(0)?, r.get(1)?))).unwrap();
        assert_eq!((state.as_str(), bytes), ("failed", 0));
        let json: String = tx
            .query_row(
                "SELECT to_json FROM d_message_summary WHERE parse_state = 'ok'",
                [],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(
            json,
            "[{\"address\":\"reader@example.test\",\"name\":\"Reader\"}]"
        );
        Ok(())
    });
}

#[test]
fn batch_errors_rollback_all_rows_and_both_checkpoints() {
    let (_scratch, _paths, store, revision) = setup();
    let message = message(&store, "/synthetic/Inbox.mbox", 0, ORIGINAL);
    let error = block_on(store.write(WritePriority::Sync, move |tx| {
        commit_import_batch(
            tx,
            &ImportBatch {
                job_id: JOB,
                expected_revision: revision,
                source_ordinal: 0,
                mailbox_name: "Inbox",
                messages: &[message],
            },
            now(),
            |_| Ok(("{\"v\":1}".to_owned(), "{\"v\":2}".to_owned())),
        )
    }))
    .unwrap_err();
    assert!(matches!(error, StoreError::InvalidInput { .. }));
    assert_eq!(count(&store, "occurrences"), 0);
    assert_eq!(count(&store, "acquisitions"), 0);
    assert_eq!(count(&store, "mailboxes"), 0);
    assert_eq!(
        reading(&store, |tx| import_job(tx, JOB)).revision(),
        revision
    );
    assert_eq!(
        reading(&store, |tx| import_job_source(tx, JOB, 0)).checkpoint_json(),
        "{\"v\":1}"
    );
    // Publication preceded the rolled-back reference; only an orphan remains.
    assert_eq!(store.objects().list().unwrap().count(), 1);
}

#[test]
fn source_identity_cannot_silently_change_decoded_bytes() {
    let (_scratch, _paths, store, revision) = setup();
    commit(
        &store,
        revision,
        "Inbox",
        vec![message(&store, "/synthetic/Inbox.mbox", 0, ORIGINAL)],
    )
    .unwrap();
    let changed = message(&store, "/synthetic/Inbox.mbox", 0, b"changed quoting");
    assert!(matches!(
        commit(&store, revision + 1, "Inbox", vec![changed]),
        Err(StoreError::Conflict { .. })
    ));
    assert_eq!(count(&store, "acquisitions"), 1);
    assert_eq!(count(&store, "occurrences"), 1);
}

#[test]
fn stale_workers_and_terminal_jobs_cannot_ingest() {
    let (_scratch, _paths, store, revision) = setup();
    commit(
        &store,
        revision,
        "Inbox",
        vec![message(&store, "/synthetic/Inbox.mbox", 0, ORIGINAL)],
    )
    .unwrap();
    assert!(matches!(
        commit(
            &store,
            revision,
            "Inbox",
            vec![message(&store, "/synthetic/Inbox.mbox", 128, ORIGINAL)]
        ),
        Err(StoreError::Conflict { .. })
    ));
    writing(&store, move |tx| {
        advance_import_job(
            tx,
            JOB,
            revision + 1,
            ImportJobState::Completed,
            "{\"v\":1}",
            now(),
        )
    });
    assert!(matches!(
        commit(&store, revision + 2, "Inbox", Vec::new()),
        Err(StoreError::Conflict { .. })
    ));
    assert!(reading(&store, recoverable_import_jobs).is_empty());
    assert_eq!(count(&store, "import_jobs"), 1);
    assert_eq!(count(&store, "occurrences"), 1);
}

#[test]
fn batch_limit_is_enforced_at_the_writer_boundary() {
    let (_scratch, _paths, store, revision) = setup();
    let messages = (0..=MAX_IMPORT_BATCH)
        .map(|offset| message(&store, "/synthetic/Inbox.mbox", offset as u64, ORIGINAL))
        .collect();
    assert!(matches!(
        commit(&store, revision, "Inbox", messages),
        Err(StoreError::InvalidInput { .. })
    ));
    let messages = (0..MAX_IMPORT_BATCH)
        .map(|offset| message(&store, "/synthetic/Inbox.mbox", offset as u64, ORIGINAL))
        .collect();
    let stats = commit(&store, revision, "Inbox", messages).unwrap();
    assert_eq!(stats.imported, MAX_IMPORT_BATCH as u64);
    assert_eq!(count(&store, "acquisitions"), 1);
    assert_eq!(count(&store, "occurrences"), MAX_IMPORT_BATCH as i64);
}

#[test]
fn job_inventory_is_immutable_and_requires_confirmation() {
    let (_scratch, paths) = scratch_paths("import-inventory");
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    writing(&store, |tx| {
        let job = create_import_job(tx, JOB, "{ \"v\":1, \"paths\": [] }", now())?;
        assert_eq!(job.request_json(), "{\"paths\":[],\"v\":1}");
        record_import_source(tx, JOB, 0, "{\"v\":1,\"path\":\"a\"}")?;
        record_import_source(tx, JOB, 0, "{\"path\":\"a\",\"v\":1}")
    });
    let error = block_on(store.write(WritePriority::Sync, |tx| {
        record_import_source(tx, JOB, 0, "{\"path\":\"b\",\"v\":1}")
    }))
    .unwrap_err();
    assert!(matches!(error, StoreError::Conflict { .. }));
    assert!(matches!(
        commit(&store, 1, "Inbox", Vec::new()),
        Err(StoreError::Conflict { .. })
    ));
    let error = block_on(store.write(WritePriority::User, |tx| {
        advance_import_job(tx, JOB, 1, ImportJobState::Running, "{\"v\":1}", now())
    }))
    .unwrap_err();
    assert!(matches!(error, StoreError::Conflict { .. }));
    assert_eq!(reading(&store, recoverable_import_jobs).len(), 1);
}

#[test]
fn source_identity_lookup_uses_its_partial_index() {
    let (_scratch, _paths, store, _revision) = setup();
    reading(&store, |tx| {
        let mut stmt = tx
            .prepare(&format!("EXPLAIN QUERY PLAN {FIND_IMPORT_SOURCE}"))
            .unwrap();
        let plan = stmt
            .query_map(params!["archive", "import_mbox", "source"], |row| {
                row.get::<_, String>(3)
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap()
            .join("\n");
        assert!(
            plan.contains("SEARCH s ") && plan.contains("INDEX acquisition_import_identity"),
            "{plan}"
        );
        Ok(())
    });
}

#[test]
fn oversized_metadata_cannot_hold_the_writer_for_an_unbounded_batch() {
    let (_scratch, _paths, store, revision) = setup();
    let mut oversized = message(&store, "/synthetic/Inbox.mbox", 0, ORIGINAL);
    oversized.summary.subject = Some("x".repeat(MAX_IMPORT_CHANGES_BYTES));
    assert!(matches!(
        commit(&store, revision, "Inbox", vec![oversized]),
        Err(StoreError::InvalidInput { .. })
    ));
    assert_eq!(count(&store, "occurrences"), 0);
    assert_eq!(
        reading(&store, |tx| import_job(tx, JOB)).revision(),
        revision
    );
}

#[test]
fn mailbox_names_share_the_metadata_and_checkpoint_budget() {
    let (_scratch, _paths, store, revision) = setup();
    for length in [
        MAX_IMPORT_CHANGES_BYTES / 2 + 1,
        MAX_IMPORT_CHANGES_BYTES / 2,
    ] {
        // The name enters the mailbox row and its unique index. The
        // second case leaves no room for even an empty batch's checkpoints.
        let error = commit(&store, revision, &"m".repeat(length), Vec::new());
        assert!(matches!(error, Err(StoreError::InvalidInput { .. })));
        assert_eq!(count(&store, "mailboxes"), 0);
        assert_eq!(
            reading(&store, |tx| import_job(tx, JOB)).revision(),
            revision
        );
        assert_eq!(
            reading(&store, |tx| import_job_source(tx, JOB, 0)).checkpoint_json(),
            "{\"v\":1}"
        );
    }

    let mailbox = "m".repeat(MAX_IMPORT_CHANGES_BYTES / 4);
    let mut entry = message(&store, "/synthetic/Inbox.mbox", 0, ORIGINAL);
    entry.summary.subject = Some("s".repeat(MAX_IMPORT_CHANGES_BYTES / 2));
    assert!(matches!(
        commit(&store, revision, &mailbox, vec![entry]),
        Err(StoreError::InvalidInput { .. })
    ));
    assert_eq!(count(&store, "mailboxes"), 0);
    assert_eq!(count(&store, "acquisitions"), 0);
    assert_eq!(count(&store, "occurrences"), 0);
    assert_eq!(
        reading(&store, |tx| import_job(tx, JOB)).revision(),
        revision
    );
}

#[test]
fn migration_preserves_a_v1_archive_and_its_originals() {
    let (_scratch, paths) = scratch_paths("import-upgrade");
    let store = crate::store::open_with(
        &paths,
        &test_seams(real_fs()),
        crate::store::WRITER_PRAGMAS,
        crate::store::READER_PRAGMAS,
        &crate::store::PINNED_SQLITE,
        &crate::schema::MIGRATIONS[..1],
    )
    .unwrap();
    let object = store.objects().publish(ORIGINAL).unwrap();
    writing(&store, move |tx| {
        tx.execute("INSERT INTO accounts (account_id,kind,provider,display_name,state,sort_order,created_at) VALUES ('kept','local','local_archive','Kept name','paused',7,123)", []).unwrap();
        tx.execute("INSERT INTO acquisitions (content_sha256,byte_length,object_state,first_acquired_at) VALUES (?1,?2,'published',123)", params![object.sha256_hex(), object.byte_length() as i64]).unwrap();
        Ok(())
    });
    store.close().unwrap();
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    reading(&store, |tx| {
        let archive: (String, String, String, i64) = tx
            .query_row(
                "SELECT account_id,display_name,state,sort_order FROM accounts",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?, r.get(3)?)),
            )
            .unwrap();
        assert_eq!(
            archive,
            (
                "kept".to_owned(),
                "Kept name".to_owned(),
                "paused".to_owned(),
                7
            )
        );
        assert_eq!(
            tx.query_row("PRAGMA user_version", [], |r| r.get::<_, u32>(0))
                .unwrap(),
            crate::schema::LATEST_SCHEMA_VERSION
        );
        Ok(())
    });
    assert_eq!(count(&store, "acquisitions"), 1);
    let mut bytes = Vec::new();
    store
        .objects()
        .open(&object)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(bytes, ORIGINAL);
    store.close().unwrap();
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    assert_eq!(count(&store, "accounts"), 1);
}

/// (occurrence, sort_date, sender, subject, snippet, unread, flagged,
/// has_attachments, representative) for every list row.
type ListRow = (
    i64,
    i64,
    Option<String>,
    Option<String>,
    Option<String>,
    bool,
    bool,
    bool,
    bool,
);

fn list_rows(store: &Store) -> Vec<ListRow> {
    reading(store, |tx| {
        let mut statement = tx
            .prepare(
                "SELECT occurrence_id, sort_date, sender, subject, snippet, unread, flagged,
                 has_attachments, representative FROM d_list_rows ORDER BY occurrence_id",
            )
            .map_err(sql_error)?;
        statement
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                    row.get(6)?,
                    row.get(7)?,
                    row.get(8)?,
                ))
            })
            .map_err(sql_error)?
            .collect::<Result<_, _>>()
            .map_err(sql_error)
    })
}

fn list_generation(store: &Store) -> u64 {
    reading(store, |tx| crate::list_rows::generation(tx))
}

/// An imported batch lists every new occurrence with its summary's
/// display fields — `Date` as the sort date, else the first-seen time —
/// in the same transaction, and advances the list generation once; a
/// replay of the same sources changes neither (spec/02 `d_list_rows`).
#[test]
fn imported_occurrences_are_listed_in_their_batch() {
    let (_scratch, _paths, store, revision) = setup();
    let mut dated = message(&store, "/synthetic/Inbox.mbox", 0, ORIGINAL);
    dated.summary.from_name = Some("Ada Sender".to_owned());
    dated.summary.from_addr = Some("ada@example.test".to_owned());
    dated.summary.date_header = Some(5_000);
    dated.summary.snippet = Some("Keep both occurrences.".to_owned());
    dated.summary.has_attachments = true;
    dated.flags.flagged = true;
    dated.flags.read = ImportReadState::FromHeaders { seen: false };
    let copy = message(&store, "/synthetic/Inbox.mbox", 128, ORIGINAL);
    let mut undated = message(
        &store,
        "/synthetic/Inbox.mbox",
        256,
        b"Subject: none\n\nx\n",
    );
    undated.summary.subject = None;
    let before = list_generation(&store);

    commit(&store, revision, "Inbox", vec![dated, copy, undated]).unwrap();

    let text = |value: &str| Some(value.to_owned());
    let subject = text("Duplicate source message");
    let shared = (
        5_000,
        text("Ada Sender"),
        subject.clone(),
        text("Keep both occurrences."),
    );
    let rows = list_rows(&store);
    let ids: Vec<i64> = rows.iter().map(|row| row.0).collect();
    assert_eq!(
        rows,
        vec![
            (
                ids[0],
                shared.0,
                shared.1.clone(),
                shared.2.clone(),
                shared.3.clone(),
                true,
                true,
                true,
                true
            ),
            // Identical bytes share the summary, but not the flags.
            (
                ids[1], shared.0, shared.1, shared.2, shared.3, false, false, true, true
            ),
            (
                ids[2],
                FIXED_NOW_MS,
                None,
                None,
                None,
                false,
                false,
                false,
                true
            ),
        ]
    );
    assert_eq!(list_generation(&store), before + 1);

    let replay = vec![
        message(&store, "/synthetic/Inbox.mbox", 0, ORIGINAL),
        message(&store, "/synthetic/Inbox.mbox", 128, ORIGINAL),
    ];
    commit(&store, revision + 1, "Inbox", replay).unwrap();
    assert_eq!(list_rows(&store), rows);
    assert_eq!(list_generation(&store), before + 1);
    store.close().unwrap();
}

/// A newer parser's summary for bytes already stored replaces nothing:
/// every row keeps showing the stored summary (spec/02).
#[test]
fn a_newer_parser_never_replaces_a_stored_summary() {
    let (_scratch, _paths, store, revision) = setup();
    let old = message(&store, "/synthetic/Inbox.mbox", 0, ORIGINAL);
    commit(&store, revision, "Inbox", vec![old]).unwrap();
    let before = list_generation(&store);

    let mut newer = message(&store, "/synthetic/Archive.mbox", 0, ORIGINAL);
    newer.summary.parser_version = 2;
    newer.summary.subject = Some("Reparsed subject".to_owned());
    commit(&store, revision + 1, "Archive", vec![newer]).unwrap();

    let subjects: Vec<Option<String>> = list_rows(&store).into_iter().map(|row| row.3).collect();
    assert_eq!(
        subjects,
        vec![Some("Duplicate source message".to_owned()); 2]
    );
    assert_eq!(
        count(&store, "d_message_summary WHERE parser_version = 1"),
        1
    );
    assert_eq!(count(&store, "d_index_queue WHERE reason = 'reextract'"), 0);
    // The new occurrence's row is the batch's only list change.
    assert_eq!(list_generation(&store), before + 1);
    store.close().unwrap();
}

/// `held` messages sharing `bytes`' acquisition, which has no summary
/// yet, each with a listed occurrence in a local folder.
fn hold_without_summary(store: &Store, bytes: &'static [u8], held: usize) {
    let object = store.objects().publish(bytes).unwrap();
    writing(store, move |tx| {
        let account: String = tx
            .query_row(
                "SELECT account_id FROM accounts WHERE provider = 'local_archive'",
                [],
                |row| row.get(0),
            )
            .map_err(sql_error)?;
        tx.execute(
            "INSERT INTO mailboxes (account_id, remote_name, kind, selectable, sync_state)
             VALUES (?1, 'Held', 'folder', 1, 'synced')",
            [&account],
        )
        .map_err(sql_error)?;
        let mailbox = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO acquisitions (content_sha256, byte_length, object_state, first_acquired_at)
             VALUES (?1, ?2, 'published', ?3)",
            params![object.sha256_hex(), bytes.len() as i64, FIXED_NOW_MS],
        )
        .map_err(sql_error)?;
        let acquisition = tx.last_insert_rowid();
        let mut occurrences = std::collections::BTreeSet::new();
        for _ in 0..held {
            tx.execute(
                "INSERT INTO logical_messages (account_id, acquisition_id, created_at)
                 VALUES (?1, ?2, ?3)",
                params![account, acquisition, FIXED_NOW_MS],
            )
            .map_err(sql_error)?;
            let message = tx.last_insert_rowid();
            tx.execute(
                "INSERT INTO occurrences (account_id, mailbox_id, message_id, state,
                 first_seen_at, last_seen_at) VALUES (?1, ?2, ?3, 'present', ?4, ?4)",
                params![account, mailbox, message, FIXED_NOW_MS],
            )
            .map_err(sql_error)?;
            occurrences.insert(tx.last_insert_rowid());
        }
        crate::list_rows::refresh(tx, &occurrences)?;
        Ok(())
    });
}

/// An import that stores the first summary of bytes already held shows
/// it on every row sharing them; when those rows would exceed the
/// batch's write budget, the batch fails whole instead (spec/02, 03).
#[test]
fn a_first_summary_updates_every_sharing_row_within_the_budget() {
    let (_scratch, _paths, store, revision) = setup();
    hold_without_summary(&store, ORIGINAL, 3);
    let first = message(&store, "/synthetic/Inbox.mbox", 0, ORIGINAL);
    commit(&store, revision, "Inbox", vec![first]).unwrap();
    let subjects: Vec<Option<String>> = list_rows(&store).into_iter().map(|row| row.3).collect();
    assert_eq!(
        subjects,
        vec![Some("Duplicate source message".to_owned()); 4]
    );

    let (_scratch, _paths, store, revision) = setup();
    hold_without_summary(&store, ORIGINAL, 300);
    let before = (list_rows(&store), list_generation(&store));
    let mut wide = message(&store, "/synthetic/Inbox.mbox", 0, ORIGINAL);
    wide.summary.subject = Some("s".repeat(8 * 1024));
    assert!(wide.change_budget_bytes() < MAX_IMPORT_CHANGES_BYTES);
    assert!(matches!(
        commit(&store, revision, "Inbox", vec![wide]),
        Err(StoreError::InvalidInput { .. })
    ));
    assert_eq!((list_rows(&store), list_generation(&store)), before);
    assert_eq!(count(&store, "d_message_summary"), 0);
    assert_eq!(count(&store, "occurrences"), 300);
    store.close().unwrap();
}
