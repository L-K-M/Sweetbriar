use super::*;
use crate::Timestamp;
use crate::index::rebuild::{self, RebuildPhase, SearchIoCode, UserPause};

fn read(conn: &Connection, scope: SearchScope) -> Result<StoredSearchCoverage, StoreError> {
    let tx = conn.unchecked_transaction().unwrap();
    let result = read_search_coverage(&tx, scope);
    tx.commit().unwrap();
    result
}

fn committed<T>(conn: &Connection, body: impl FnOnce(&Transaction) -> Result<T, StoreError>) -> T {
    let tx = conn.unchecked_transaction().unwrap();
    let result = body(&tx).unwrap();
    tx.commit().unwrap();
    result
}

fn facts(value: &StoredSearchCoverage) -> [u64; 8] {
    [
        value.total_messages,
        value.headers_synced,
        value.bodies_acquired,
        value.indexed,
        value.partial,
        value.failed,
        value.pending,
        value.queue_length,
    ]
}

#[test]
fn scoped_facts_distinguish_messages_copies_shared_bytes_and_current_sources() {
    let conn = fixture(7);
    for (id, state, mask) in [
        (1, "indexed", "[]"),
        (2, "partial", "[\"subject\"]"),
        (3, "failed", "[]"),
        (4, "indexed", "[]"),
        (6, "indexed", "[]"),
    ] {
        doc(&conn, id, state, mask, &[("subject", "alpha")]);
    }
    for (id, state) in [(1, "ok"), (2, "partial"), (3, "failed"), (6, "ok")] {
        summary(&conn, id, state, 1, Summary::default());
    }
    edit(&conn, "UPDATE store_meta SET value='2' WHERE key='search_extractor_version';
        UPDATE d_search_docs SET extractor_version=2 WHERE doc_id IN (1,2,3,6);
        UPDATE acquisitions SET object_state='missing' WHERE acquisition_id=6;
        UPDATE logical_messages SET acquisition_id=NULL WHERE message_id=7;
        INSERT INTO message_headers(message_id,raw_fields,fetched_at) VALUES(5,x'01',1),(7,x'01',1)");
    let account = archive_account(&conn);
    let archive = mailbox(&conn, &account, "Archive", "folder", None);
    committed(&conn, |tx| {
        occurrence(tx, archive, 1);
        occurrence(tx, archive, 1);
        let shared = message(tx, &account, Some(1));
        occurrence(tx, archive, shared);
        index::enqueue(
            tx,
            &[
                (1, index::IndexReason::Reextract),
                (2, index::IndexReason::Reextract),
                (7, index::IndexReason::Reextract),
            ],
            Timestamp::from_millis(1),
        )
    });
    let all = read(&conn, SearchScope::All).unwrap();
    assert_eq!(facts(&all), [8, 6, 6, 2, 1, 1, 4, 2]);
    assert_eq!(all.extractor_version, 2);
    assert_eq!(
        all.generation,
        index::search_generation(&conn).unwrap() as u64
    );
    assert_eq!(all.index_state, SearchIndexState::Running);
    assert_eq!(
        all.limitations,
        [
            SearchLimitation::CjkUnsegmented,
            SearchLimitation::AttachmentTextUnavailable
        ]
    );
    assert_eq!(
        facts(&read(&conn, SearchScope::Account(account.parse().unwrap())).unwrap()),
        facts(&all)
    );
    assert_eq!(
        facts(&read(&conn, SearchScope::Mailbox(1)).unwrap()),
        [7, 5, 5, 1, 1, 1, 4, 2]
    );
    assert_eq!(
        facts(&read(&conn, SearchScope::Mailbox(archive)).unwrap()),
        [2, 2, 2, 2, 0, 0, 0, 1]
    );
    let empty = mailbox(&conn, &account, "Empty", "folder", None);
    let empty = read(&conn, SearchScope::Mailbox(empty)).unwrap();
    assert_eq!(facts(&empty), [0; 8]);
    assert_eq!(empty.index_state, SearchIndexState::Running);
    conn.execute(
        "INSERT INTO accounts(account_id,kind,provider,display_name,state,sort_order,created_at)
        VALUES(?1,'imap','generic_imap','Other','active',1,1)",
        [OTHER_ACCOUNT],
    )
    .unwrap();
    assert_eq!(
        facts(&read(&conn, SearchScope::Account(OTHER_ACCOUNT.parse().unwrap())).unwrap()),
        [0; 8]
    );
    edit(
        &conn,
        "UPDATE occurrences SET state='vanished' WHERE message_id=2",
    );
    assert_eq!(
        facts(&read(&conn, SearchScope::All).unwrap()),
        [7, 5, 5, 2, 0, 1, 4, 1]
    );
    edit(
        &conn,
        "UPDATE store_meta SET value='3' WHERE key='search_extractor_version'",
    );
    assert_eq!(
        facts(&read(&conn, SearchScope::All).unwrap()),
        [7, 5, 5, 0, 0, 0, 7, 1]
    );
}

#[test]
fn real_phase_and_pause_commits_keep_scoped_queue_separate_from_global_progress() {
    let conn = fixture(3);
    doc(&conn, 1, "indexed", "[]", &[("subject", "alpha")]);
    doc(&conn, 2, "partial", "[\"subject\"]", &[]);
    let empty = mailbox(&conn, &archive_account(&conn), "Empty", "folder", None);
    committed(&conn, |tx| {
        index::enqueue(
            tx,
            &[(1, index::IndexReason::Reextract)],
            Timestamp::from_millis(1),
        )
    });
    committed(&conn, |tx| {
        rebuild::start_search_rebuild(tx, "coverage-job", 1)
    });
    let epoch = rebuild::latest_rebuild(&conn).unwrap().unwrap().epoch();
    let reset = read(&conn, SearchScope::All).unwrap();
    assert_eq!(facts(&reset), [3, 0, 3, 0, 0, 0, 3, 1]);
    assert_eq!(
        reset.rebuild.as_ref().unwrap(),
        &SearchRebuildProgress {
            job_id: "coverage-job".into(),
            phase: RebuildPhase::Reset,
            processed: 0,
            pending_documents: 1,
        }
    );
    committed(&conn, |tx| {
        rebuild::rebuild_reset_batch(tx, "coverage-job", epoch, 1)
    });
    let reset = read(&conn, SearchScope::All).unwrap();
    assert_eq!(reset.rebuild.unwrap().processed, 1);
    assert_eq!(reset.indexed + reset.partial + reset.failed, 0);
    committed(&conn, |tx| {
        rebuild::rebuild_reset_batch(tx, "coverage-job", epoch, 200)
    });
    let enqueue = read(&conn, SearchScope::All).unwrap();
    assert_eq!(enqueue.rebuild.unwrap().phase, RebuildPhase::Enqueue);
    assert_eq!(enqueue.pending, 3);
    committed(&conn, |tx| {
        rebuild::rebuild_enqueue_batch(tx, "coverage-job", epoch, 1, Timestamp::from_millis(2))
    });
    assert_eq!(
        read(&conn, SearchScope::All)
            .unwrap()
            .rebuild
            .unwrap()
            .processed,
        1
    );
    committed(&conn, |tx| {
        rebuild::rebuild_enqueue_batch(tx, "coverage-job", epoch, 200, Timestamp::from_millis(2))
    });
    let active = read(&conn, SearchScope::Mailbox(empty)).unwrap();
    assert_eq!(facts(&active), [0; 8]);
    assert_eq!(active.index_state, SearchIndexState::Running);
    assert_eq!(active.rebuild.as_ref().unwrap().pending_documents, 3);
    assert_eq!(active.rebuild.as_ref().unwrap().phase, RebuildPhase::Index);
    committed(&conn, |tx| {
        rebuild::pause_for_io(
            tx,
            SearchIoCode::StorageFull,
            "Free disk space",
            Timestamp::from_millis(100),
            1,
        )
    });
    let io = read(&conn, SearchScope::Mailbox(empty)).unwrap();
    assert_eq!(
        io.index_state,
        SearchIndexState::PausedIo {
            code: SearchIoCode::StorageFull,
            message: "Free disk space".into(),
            retry_at: 100,
        }
    );
    committed(&conn, |tx| {
        rebuild::pause_for_io(
            tx,
            SearchIoCode::PermissionDenied,
            "Restore access",
            Timestamp::from_millis(101),
            2,
        )
    });
    let updated = read(&conn, SearchScope::Mailbox(empty)).unwrap();
    assert!(updated.generation > io.generation);
    assert_eq!(updated.queue_length, io.queue_length);
    assert_eq!(
        updated.index_state,
        SearchIndexState::PausedIo {
            code: SearchIoCode::PermissionDenied,
            message: "Restore access".into(),
            retry_at: 101,
        }
    );
    let io = updated;
    committed(&conn, |tx| rebuild::set_user_pause(tx, UserPause::Pause));
    assert_eq!(
        read(&conn, SearchScope::Mailbox(empty))
            .unwrap()
            .index_state,
        SearchIndexState::PausedByUser
    );
    committed(&conn, |tx| rebuild::set_user_pause(tx, UserPause::Resume));
    assert_eq!(
        read(&conn, SearchScope::Mailbox(empty))
            .unwrap()
            .index_state,
        io.index_state
    );
    committed(&conn, |tx| {
        rebuild::request_index_retry(tx, Timestamp::from_millis(50))
    });
    let requested = read(&conn, SearchScope::Mailbox(empty)).unwrap();
    assert_eq!(
        requested.index_state,
        SearchIndexState::PausedIo {
            code: SearchIoCode::PermissionDenied,
            message: "Restore access".into(),
            retry_at: 50,
        }
    );
    assert_eq!(requested.queue_length, 0);
    assert_eq!(requested.rebuild.as_ref().unwrap().pending_documents, 3);
    assert!(requested.generation > io.generation);
    let captured = index::pending_batch(&conn, 200).unwrap();
    committed(&conn, |tx| {
        rebuild::begin_index_retry(tx, captured.fence, Timestamp::from_millis(50))
    });
    let publications: Vec<_> = captured
        .documents
        .into_iter()
        .map(|pending| index::IndexPublication {
            pending,
            document: Some(crate::testing::fixtures::document("after")),
        })
        .collect();
    assert_eq!(
        committed(&conn, |tx| index::publish_batch(
            tx,
            captured.fence,
            &publications
        )),
        3
    );
    assert!(committed(&conn, |tx| rebuild::complete_rebuild(
        tx,
        "coverage-job",
        epoch
    )));
    let complete = read(&conn, SearchScope::All).unwrap();
    assert_eq!(facts(&complete), [3, 0, 3, 3, 0, 0, 0, 0]);
    assert_eq!(complete.index_state, SearchIndexState::Ready);
    assert_eq!(
        complete.rebuild.as_ref().unwrap().phase,
        RebuildPhase::Complete
    );
    assert_eq!(complete.rebuild.as_ref().unwrap().processed, 3);
    committed(&conn, |tx| {
        index::enqueue(
            tx,
            &[(1, index::IndexReason::Reextract)],
            Timestamp::from_millis(3),
        )
    });
    let later = read(&conn, SearchScope::All).unwrap();
    assert_eq!(later.queue_length, 1);
    assert_eq!(later.index_state, SearchIndexState::Running);
    assert_eq!(later.rebuild.as_ref().unwrap().pending_documents, 0);
    assert_eq!(later.rebuild.as_ref().unwrap().job_id, "coverage-job");
}

#[test]
fn scope_and_metadata_failures_are_explicit_even_without_hits() {
    let conn = fixture(0);
    for scope in [SearchScope::Mailbox(0), SearchScope::Mailbox(-1)] {
        assert!(matches!(
            read(&conn, scope),
            Err(StoreError::InvalidInput { .. })
        ));
    }
    for scope in [
        SearchScope::Mailbox(99999),
        SearchScope::Account(ABSENT.parse().unwrap()),
    ] {
        assert!(matches!(
            read(&conn, scope),
            Err(StoreError::NotFound { .. })
        ));
    }
    for (key, bad) in [
        ("search_generation", "-1"),
        ("search_extractor_version", "bad"),
    ] {
        let original: String = conn
            .query_row("SELECT value FROM store_meta WHERE key=?1", [key], |row| {
                row.get(0)
            })
            .unwrap();
        conn.execute(
            "UPDATE store_meta SET value=?2 WHERE key=?1",
            params![key, bad],
        )
        .unwrap();
        assert!(matches!(
            read(&conn, SearchScope::All),
            Err(StoreError::Corrupt { .. })
        ));
        conn.execute(
            "UPDATE store_meta SET value=?2 WHERE key=?1",
            params![key, original],
        )
        .unwrap();
    }
    conn.execute("DELETE FROM search_index_state", []).unwrap();
    assert!(matches!(
        read(&conn, SearchScope::All),
        Err(StoreError::Corrupt { .. })
    ));
}

#[test]
fn wal_reader_cannot_mix_counts_version_pause_or_generation() {
    use crate::schema::{MIGRATIONS, run_migrations};
    use crate::testing::test_seams;
    use sb_test_support::{ScratchDir, real_fs};
    let scratch = ScratchDir::new("coverage-snapshot");
    let path = scratch.path().join("state.sqlite");
    let mut writer = Connection::open(&path).unwrap();
    writer
        .execute_batch("PRAGMA journal_mode=WAL;PRAGMA foreign_keys=ON;PRAGMA trusted_schema=OFF")
        .unwrap();
    run_migrations(&mut writer, &test_seams(real_fs()), MIGRATIONS).unwrap();
    populate(&mut writer, 2);
    doc(&writer, 1, "indexed", "[]", &[("subject", "alpha")]);
    let reader = Connection::open(&path).unwrap();
    reader
        .execute_batch("PRAGMA foreign_keys=ON;PRAGMA trusted_schema=OFF")
        .unwrap();
    let held = reader.unchecked_transaction().unwrap();
    let before = read_search_coverage(&held, SearchScope::All).unwrap();
    assert_eq!(facts(&before), [2, 0, 2, 1, 0, 0, 1, 0]);
    assert_eq!(before.index_state, SearchIndexState::Ready);
    committed(&writer, |tx| {
        doc(tx, 2, "partial", "[\"subject\"]", &[]);
        tx.execute_batch(
            "UPDATE store_meta SET value='2' WHERE key='search_extractor_version';
            UPDATE d_search_docs SET extractor_version=2 WHERE doc_id=2;
            UPDATE acquisitions SET object_state='missing' WHERE acquisition_id=1;
            INSERT INTO message_headers(message_id,raw_fields,fetched_at) VALUES(1,x'01',1)",
        )
        .unwrap();
        rebuild::pause_for_io(
            tx,
            SearchIoCode::StorageUnavailable,
            "Reconnect storage",
            Timestamp::from_millis(100),
            1,
        )
    });
    assert_eq!(
        read_search_coverage(&held, SearchScope::All).unwrap(),
        before
    );
    held.commit().unwrap();
    let fresh = read(&reader, SearchScope::All).unwrap();
    assert_eq!(fresh.generation, before.generation + 1);
    assert_eq!(fresh.extractor_version, 2);
    assert_eq!(facts(&fresh), [2, 1, 1, 0, 1, 0, 1, 0]);
    assert_eq!(
        fresh.index_state,
        SearchIndexState::PausedIo {
            code: SearchIoCode::StorageUnavailable,
            message: "Reconnect storage".into(),
            retry_at: 100,
        }
    );
}

#[test]
fn kept_scope_counts_are_reused_only_for_their_scope_generation_and_target() {
    let conn = fixture(3);
    let reading = |scope, kept: Option<&CoverageCounts>| {
        let tx = conn.unchecked_transaction().unwrap();
        let read = read_search_coverage_reusing(&tx, scope, kept).unwrap();
        tx.commit().unwrap();
        read
    };
    let (first, kept) = reading(SearchScope::All, None);
    assert_eq!(first.indexed, 0);

    // A raw write moves no generation, so reuse shows the kept counts while
    // a scan, or another scope, sees the document.
    let generation = index::search_generation(&conn).unwrap();
    doc(&conn, 1, "indexed", "[]", &[("subject", "alpha")]);
    assert_eq!(index::search_generation(&conn).unwrap(), generation);
    let (reused, again) = reading(SearchScope::All, Some(&kept));
    assert_eq!((reused.indexed, &again), (0, &kept));
    assert_eq!(reading(SearchScope::All, None).0.indexed, 1);
    let (mailbox, counts) = reading(SearchScope::Mailbox(1), Some(&kept));
    assert_eq!(
        (mailbox.indexed, counts.scope()),
        (1, &SearchScope::Mailbox(1))
    );

    // A new generation or target makes the kept counts stale.
    edit(
        &conn,
        "UPDATE store_meta SET value=value+1 WHERE key='search_generation'",
    );
    let (fresh, kept) = reading(SearchScope::All, Some(&kept));
    assert_eq!(fresh.indexed, 1);
    edit(
        &conn,
        "UPDATE store_meta SET value='2' WHERE key='search_extractor_version'",
    );
    let (retargeted, _) = reading(SearchScope::All, Some(&kept));
    assert_eq!((retargeted.extractor_version, retargeted.indexed), (2, 0));
}
