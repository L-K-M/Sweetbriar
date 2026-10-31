//! Facade tests over a real store: synthetic fixtures on the real
//! filesystem, plus a counting `FaultFs` proving blocked and cancelled
//! preparation performs no object I/O (spec/06 "Preparation boundary").

use std::sync::Arc;

use sb_store::index::rebuild::{
    SearchIoCode, UserPause, pause_for_io, rebuild_enqueue_batch, rebuild_reset_batch,
    set_user_pause, start_search_rebuild,
};
use sb_store::index::{enqueue, publish_batch};
use sb_store::{Fs, ObjectId, ObjectStore, Seams, StorePaths, Timestamp, WritePriority};
use sb_test_support::{
    FaultFs, FaultKind, FixedClock, FsOp, ScratchDir, SeededRandom, block_on, real_fs,
};

use super::*;

/// The all-columns fixture shared with the MIME extraction tests.
const RICH: &[u8] = concat!(
    "Subject: =?UTF-8?Q?Quarterly_caf=C3=A9?=\r\n",
    "From: =?UTF-8?Q?Andr=C3=A9?= <andre@example.invalid>\r\n",
    "From: Team: Other <other@example.invalid>;\r\n",
    "To: Readers: Alice <alice@example.invalid>, bob@example.invalid;\r\n",
    "Cc: Carol <carol@example.invalid>\r\n",
    "Bcc: private@example.invalid\r\n",
    "MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=outer\r\n\r\n",
    "--outer\r\nContent-Type: multipart/alternative; boundary=alt\r\n\r\n",
    "--alt\r\nContent-Type: text/plain; charset=utf-8\r\n",
    "Content-Transfer-Encoding: quoted-printable\r\n\r\n",
    "Hello caf=C3=A9\r\n\r\n> Previous quarter\r\n\r\n--=20\r\nAndr=C3=A9\r\n",
    "--alt\r\nContent-Type: text/html; charset=utf-8\r\n\r\n",
    "<p>Hello café</p><p>HTML details</p><blockquote>Earlier figures</blockquote>\r\n",
    "--alt--\r\n",
    "--outer\r\nContent-Type: application/pdf\r\n",
    "Content-Disposition: attachment; filename*=utf-8''caf%C3%A9.pdf\r\n\r\nPDF-HIDDEN\r\n",
    "--outer\r\nContent-Type: message/rfc822; name=forward.eml\r\n\r\n",
    "Subject: INNER-HIDDEN\r\n\r\nNESTED-HIDDEN\r\n",
    "--outer--\r\n"
)
.as_bytes();

fn seams(fs: Arc<dyn Fs>) -> Seams {
    Seams::new(
        fs,
        Arc::new(FixedClock::new(Timestamp::from_millis(1_735_689_600_000))),
        Arc::new(SeededRandom::new(9)),
    )
}

fn store_on(fs: Arc<dyn Fs>, label: &str) -> (ScratchDir, StorePaths, Store) {
    let scratch = ScratchDir::new(label);
    let paths = StorePaths::new(scratch.path().join("store")).unwrap();
    let store = sb_store::open(&paths, &seams(fs)).unwrap();
    // Facade fixtures begin after the engine's version reconciliation.
    // Actual legacy-to-current startup is covered by Engine acceptance.
    write(&store, |tx| {
        tx.execute(
            "UPDATE store_meta SET value=?1 WHERE key='search_extractor_version'",
            [SEARCH_EXTRACTOR_VERSION.to_string()],
        )
        .map(|_| ())
        .map_err(|error| StoreError::internal(error.to_string()))
    });
    (scratch, paths, store)
}

/// A store over a `FaultFs` that never fires, used only for its
/// per-operation counters, so a blocked prepare is provably I/O-free.
fn counting_store(label: &str) -> (ScratchDir, StorePaths, Arc<FaultFs>, Store) {
    let fs = Arc::new(FaultFs::new(
        real_fs(),
        FsOp::Read,
        u64::MAX,
        FaultKind::Eio,
    ));
    let (scratch, paths, store) = store_on(fs.clone(), label);
    (scratch, paths, fs, store)
}

fn write<T: Send + 'static>(
    store: &Store,
    f: impl FnOnce(&Transaction) -> Result<T, StoreError> + Send + 'static,
) -> T {
    block_on(store.write(WritePriority::User, f)).unwrap()
}

/// Publish `bytes` as an object and record it as published acquisition
/// `id`; returns the object so tests can damage its file.
fn acquire(store: &Store, id: i64, bytes: &[u8]) -> ObjectId {
    let object = *store.objects().publish(bytes).unwrap().id();
    write(store, move |tx| {
        tx.execute_batch(&format!(
            "INSERT INTO acquisitions (acquisition_id, content_sha256, byte_length,
             object_state, first_acquired_at) VALUES ({id}, '{}', {}, 'published', 0)",
            object.sha256_hex(),
            object.byte_length()
        ))
        .unwrap();
        Ok(())
    });
    object
}

fn enqueue_docs(store: &Store, docs: &[(i64, IndexReason)], at: i64) {
    let docs = docs.to_vec();
    write(store, move |tx| {
        enqueue(tx, &docs, Timestamp::from_millis(at))
    })
}

/// Queue `doc_id` directly: the shape only an unfenced v1 store or a
/// metadata invariant violation can produce; producers refuse it.
fn queue_raw(store: &Store, doc_id: i64, reason: &'static str, at: i64) {
    write(store, move |tx| {
        tx.execute_batch(&format!(
            "INSERT INTO d_index_queue (doc_id, reason, enqueued_at, revision)
             VALUES ({doc_id}, '{reason}', {at}, 1000 + {at})"
        ))
        .unwrap();
        Ok(())
    });
}

fn capture(store: &Store) -> IndexBatch {
    store.read(|tx| pending_batch(tx, MAX_INDEX_BATCH)).unwrap()
}

fn pending_ids(store: &Store) -> Vec<i64> {
    capture(store)
        .documents
        .iter()
        .map(|pending| pending.doc_id)
        .collect()
}

/// The object file for `id`'s hash, per spec/03's layout.
fn object_path(paths: &StorePaths, object: &ObjectId) -> std::path::PathBuf {
    let hash = object.sha256_hex();
    paths
        .objects_sha256()
        .join(&hash[..2])
        .join(&hash[2..4])
        .join(&hash)
}

fn assert_no_object_io(fs: &FaultFs, opens: u64, reads: u64) {
    assert_eq!(fs.calls(FsOp::Open), opens, "an object file was opened");
    assert_eq!(fs.calls(FsOp::Read), reads, "an object body was read");
}

#[test]
fn publications_map_all_eight_columns_and_states_through_the_facade() {
    let (scratch, _paths, store) = store_on(real_fs(), "index-map");
    acquire(&store, 1, RICH);
    acquire(
        &store,
        2,
        b"Content-Type: text/plain; charset=utf-8\r\n\r\na\xffb",
    );
    acquire(&store, 3, b"");
    enqueue_docs(
        &store,
        &[
            (1, IndexReason::New),
            (2, IndexReason::New),
            (3, IndexReason::New),
        ],
        10,
    );
    store.close().unwrap();

    let res = Resources::open(&scratch.path().join("store")).unwrap();
    let batch = res.pending_index_batch().unwrap();
    assert_eq!(batch.documents.len(), 3);
    assert_eq!(batch.fence.version, SEARCH_EXTRACTOR_VERSION);
    let prepared = match res.prepare_index_batch(batch, &|| false).unwrap() {
        IndexPreparation::Prepared(prepared) => prepared,
        other => panic!("expected Prepared, got {other:?}"),
    };
    let publications = prepared.publications();
    assert_eq!(publications.len(), 3);
    let doc = publications[0].document.as_ref().unwrap();
    assert_eq!(doc.state, IndexState::Indexed);
    assert_eq!(doc.extractor_version, SEARCH_EXTRACTOR_VERSION);
    assert_eq!(
        doc.columns,
        [
            "Quarterly café",
            "André\nandre@example.invalid\nOther\nother@example.invalid",
            "Alice\nalice@example.invalid\nbob@example.invalid",
            "Carol\ncarol@example.invalid",
            "Hello café\n\nHTML details",
            "> Previous quarter\n\nEarlier figures",
            "-- \nAndré",
            "café.pdf\nforward.eml",
        ]
    );
    assert!(doc.truncated_fields.is_empty());
    let partial = publications[1].document.as_ref().unwrap();
    assert_eq!(partial.state, IndexState::Partial);
    assert_eq!(partial.columns[4], "a\u{fffd}b");
    assert_eq!(
        partial.truncated_fields,
        ["body_authored", "body_quoted", "body_signature"]
    );
    let failed = publications[2].document.as_ref().unwrap();
    assert_eq!(failed.state, IndexState::Failed);
    assert!(failed.columns.iter().all(String::is_empty));
    assert!(failed.truncated_fields.is_empty());

    // One fenced write acknowledges all three and lands the docs.
    let (fence, publications, failure) = prepared.into_parts();
    assert!(failure.is_none());
    let accepted = block_on(res.write(WritePriority::Index, move |tx| {
        publish_batch(tx, fence, &publications)
    }))
    .unwrap();
    assert_eq!(accepted, 3);
    assert!(res.pending_index_batch().unwrap().documents.is_empty());
    res.read(|tx| {
        let states: Vec<String> = tx
            .prepare("SELECT state FROM d_search_docs ORDER BY doc_id")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap();
        assert_eq!(states, ["indexed", "partial", "failed"]);
        Ok(())
    })
    .unwrap();
    res.close().unwrap();
}

#[test]
fn an_oversized_acquisition_yields_an_empty_partial_document() {
    let (_scratch, _paths, fs, store) = counting_store("index-oversized");
    acquire(
        &store,
        1,
        b"Subject: big\r\nContent-Type: text/plain\r\n\r\noversized body",
    );
    enqueue_docs(&store, &[(1, IndexReason::New)], 10);
    let limits = Limits::default().with_max_message_bytes(8);
    let batch = capture(&store);
    let reads = fs.calls(FsOp::Read);
    let prepared = match prepare(&store, batch, &limits, &|| false).unwrap() {
        IndexPreparation::Prepared(prepared) => prepared,
        other => panic!("expected Prepared, got {other:?}"),
    };
    assert!(prepared.failure().is_none());
    let doc = prepared.publications()[0].document.as_ref().unwrap();
    assert_eq!(doc.state, IndexState::Partial);
    assert!(doc.columns.iter().all(String::is_empty));
    let mut all = [
        "attachment_names",
        "body_authored",
        "body_quoted",
        "body_signature",
        "cc_text",
        "from_text",
        "subject",
        "to_text",
    ];
    all.sort_unstable();
    assert_eq!(doc.truncated_fields, all);
    // The object verified to EOF: it is still published, never quarantined.
    assert_eq!(
        fs.calls(FsOp::Read) - reads,
        2,
        "the whole body streamed through the verifier"
    );
    let accepted = write(&store, {
        let fence = prepared.fence();
        let publications = prepared.publications().to_vec();
        move |tx| publish_batch(tx, fence, &publications)
    });
    assert_eq!(accepted, 1);
    store.close().unwrap();
}

#[test]
fn the_column_byte_budget_stops_before_exceeding_and_leaves_work_queued() {
    let (_scratch, _paths, store) = store_on(real_fs(), "index-budget");
    for id in 1..=3 {
        // Distinct objects (content_sha256 is unique); a header leaves
        // the extracted columns at exactly one megabyte each.
        let big = format!(
            "X-Doc: {id}\r\nContent-Type: text/plain\r\n\r\n{}",
            "x".repeat(1_000_000)
        );
        acquire(&store, id, big.as_bytes());
    }
    enqueue_docs(
        &store,
        &[
            (1, IndexReason::New),
            (2, IndexReason::New),
            (3, IndexReason::New),
        ],
        10,
    );
    let prepared = match prepare(&store, capture(&store), &Limits::default(), &|| false).unwrap() {
        IndexPreparation::Prepared(prepared) => prepared,
        other => panic!("expected Prepared, got {other:?}"),
    };
    assert_eq!(prepared.publications().len(), 2);
    assert_eq!(
        prepared.publications()[0]
            .document
            .as_ref()
            .unwrap()
            .column_bytes(),
        1_000_000
    );
    let fence = prepared.fence();
    let publications = prepared.publications().to_vec();
    assert_eq!(
        write(&store, move |tx| publish_batch(tx, fence, &publications)),
        2
    );
    assert_eq!(pending_ids(&store), [3]);
    // The remainder prepares and publishes on the next pass.
    let prepared = match prepare(&store, capture(&store), &Limits::default(), &|| false).unwrap() {
        IndexPreparation::Prepared(prepared) => prepared,
        other => panic!("expected Prepared, got {other:?}"),
    };
    let fence = prepared.fence();
    let publications = prepared.publications().to_vec();
    assert_eq!(
        write(&store, move |tx| publish_batch(tx, fence, &publications)),
        1
    );
    assert!(pending_ids(&store).is_empty());
    store.close().unwrap();
}

/// The legacy v1 shape (spec/06): a `delete` for an acquisition that
/// was never there needs no lookup or body I/O; normal fenced
/// publication acknowledges it.
#[test]
fn a_delete_for_an_absent_acquisition_needs_no_lookup_or_io() {
    let (_scratch, _paths, fs, store) = counting_store("index-delete-99");
    queue_raw(&store, 99, "delete", -20);
    let batch = capture(&store);
    assert_eq!(batch.documents.len(), 1);
    assert_eq!(batch.documents[0].reason, IndexReason::Delete);
    let opens = fs.calls(FsOp::Open);
    let reads = fs.calls(FsOp::Read);
    let prepared = match prepare(&store, batch, &Limits::default(), &|| false).unwrap() {
        IndexPreparation::Prepared(prepared) => prepared,
        other => panic!("expected Prepared, got {other:?}"),
    };
    assert_eq!(prepared.publications().len(), 1);
    assert!(prepared.publications()[0].document.is_none());
    assert_eq!(prepared.publications()[0].pending.doc_id, 99);
    assert!(prepared.failure().is_none());
    assert_no_object_io(&fs, opens, reads);
    let fence = prepared.fence();
    let publications = prepared.publications().to_vec();
    assert_eq!(
        write(&store, move |tx| publish_batch(tx, fence, &publications)),
        1
    );
    assert!(pending_ids(&store).is_empty());
    store.close().unwrap();
}

/// A missing acquisition for `new`/`reextract` is a `Lookup`
/// `NotFound`, a contextual invariant failure, not a transient file
/// read. An absent object is a `Read` `NotFound`. Preparation
/// stops at the first failure with the valid prefix, acknowledges
/// nothing, and succeeds once the cause is repaired.
#[test]
fn lookup_and_read_failures_carry_stage_and_token_and_retry_verifies() {
    let (_scratch, paths, store) = store_on(real_fs(), "index-failures");
    acquire(&store, 1, b"Subject: one\r\n\r\nfirst");
    enqueue_docs(&store, &[(1, IndexReason::New)], 10);
    queue_raw(&store, 2, "new", 20);
    let gone = acquire(&store, 3, b"Subject: three\r\n\r\nthird");
    enqueue_docs(&store, &[(3, IndexReason::New)], 30);
    std::fs::remove_file(object_path(&paths, &gone)).unwrap();
    acquire(&store, 4, b"Subject: four\r\n\r\nfourth");
    enqueue_docs(&store, &[(4, IndexReason::New)], 40);

    let prepared = match prepare(&store, capture(&store), &Limits::default(), &|| false).unwrap() {
        IndexPreparation::Prepared(prepared) => prepared,
        other => panic!("expected Prepared, got {other:?}"),
    };
    assert_eq!(prepared.publications().len(), 1);
    assert_eq!(prepared.publications()[0].pending.doc_id, 1);
    // Publish only the prefix: the failure and the unprocessed tail stay.
    let (fence, publications, failure) = prepared.into_parts();
    assert_eq!(
        write(&store, move |tx| publish_batch(tx, fence, &publications)),
        1
    );
    let (pending, stage, error) = failure.unwrap().into_parts();
    assert_eq!(stage, IndexReadStage::Lookup);
    assert_eq!(pending.doc_id, 2);
    assert!(matches!(error, StoreError::NotFound { .. }));
    assert_eq!(pending_ids(&store), [2, 3, 4]);

    // Repair the metadata; the next batch reaches the missing object.
    acquire(&store, 2, b"Subject: two\r\n\r\nsecond");
    let prepared = match prepare(&store, capture(&store), &Limits::default(), &|| false).unwrap() {
        IndexPreparation::Prepared(prepared) => prepared,
        other => panic!("expected Prepared, got {other:?}"),
    };
    assert_eq!(prepared.publications().len(), 1);
    assert_eq!(prepared.publications()[0].pending.doc_id, 2);
    let failure = prepared.failure().unwrap();
    assert_eq!(failure.stage(), IndexReadStage::Read);
    assert_eq!(failure.pending().doc_id, 3);
    assert!(matches!(failure.error(), StoreError::NotFound { .. }));
    assert_eq!(pending_ids(&store), [2, 3, 4], "preparation acks nothing");

    // Republish the missing bytes; the acquisition row still names
    // them, so the verified read succeeds and everything drains.
    store
        .objects()
        .publish(&b"Subject: three\r\n\r\nthird"[..])
        .unwrap();
    let prepared = match prepare(&store, capture(&store), &Limits::default(), &|| false).unwrap() {
        IndexPreparation::Prepared(prepared) => prepared,
        other => panic!("expected Prepared, got {other:?}"),
    };
    assert!(prepared.failure().is_none());
    assert_eq!(prepared.publications().len(), 3);
    let fence = prepared.fence();
    let publications = prepared.publications().to_vec();
    assert_eq!(
        write(&store, move |tx| publish_batch(tx, fence, &publications)),
        3
    );
    assert!(pending_ids(&store).is_empty());
    store.close().unwrap();
}

#[test]
fn both_pauses_block_preparation_without_any_object_io() {
    for user in [true, false] {
        let (_scratch, _paths, fs, store) = counting_store("index-paused");
        acquire(&store, 1, b"Subject: s\r\n\r\nbody");
        enqueue_docs(&store, &[(1, IndexReason::New)], 10);
        let batch = capture(&store);
        if user {
            write(&store, |tx| set_user_pause(tx, UserPause::Pause));
        } else {
            write(&store, |tx| {
                pause_for_io(
                    tx,
                    SearchIoCode::StorageUnavailable,
                    "the disk is full",
                    Timestamp::from_millis(9_999),
                    1,
                )
            });
        }
        let opens = fs.calls(FsOp::Open);
        let reads = fs.calls(FsOp::Read);
        let outcome = prepare(&store, batch, &Limits::default(), &|| false).unwrap();
        let expected = if user {
            IndexBlocked::UserPaused
        } else {
            IndexBlocked::IoPaused
        };
        assert!(
            matches!(outcome, IndexPreparation::Blocked(blocked) if blocked == expected),
            "{outcome:?}"
        );
        assert_no_object_io(&fs, opens, reads);
        // The raw pending view stays readable while blocked.
        assert_eq!(pending_ids(&store), [1]);
        store.close().unwrap();
    }
}

/// Reset and enqueue phases block preparation: an empty queue under
/// a rebuild is `Blocked`, never a drained-looking `Prepared`.
#[test]
fn reset_and_enqueue_phases_block_without_object_io() {
    let (_scratch, _paths, fs, store) = counting_store("index-phases");
    acquire(&store, 1, b"Subject: s\r\n\r\nbody");
    acquire(&store, 2, b"Subject: t\r\n\r\nother");
    let job = write(&store, |tx| {
        start_search_rebuild(tx, "job-1", SEARCH_EXTRACTOR_VERSION)
    });
    assert_eq!(job, "job-1");
    // Reset on an empty queue: still Blocked, not an empty Prepared.
    let opens = fs.calls(FsOp::Open);
    let reads = fs.calls(FsOp::Read);
    let outcome = prepare(&store, capture(&store), &Limits::default(), &|| false).unwrap();
    assert!(
        matches!(
            outcome,
            IndexPreparation::Blocked(IndexBlocked::Rebuilding(RebuildPhase::Reset))
        ),
        "{outcome:?}"
    );
    assert_no_object_io(&fs, opens, reads);
    // Drive to Enqueue; one acquisition per batch keeps it there.
    let epoch = store
        .read(|tx| Ok(latest_rebuild(tx)?.unwrap().epoch()))
        .unwrap();
    write(&store, move |tx| {
        rebuild_reset_batch(tx, "job-1", epoch, MAX_INDEX_BATCH)
    });
    write(&store, move |tx| {
        rebuild_enqueue_batch(tx, "job-1", epoch, 1, Timestamp::from_millis(1))
    });
    let outcome = prepare(&store, capture(&store), &Limits::default(), &|| false).unwrap();
    assert!(
        matches!(
            outcome,
            IndexPreparation::Blocked(IndexBlocked::Rebuilding(RebuildPhase::Enqueue))
        ),
        "{outcome:?}"
    );
    assert_no_object_io(&fs, opens, reads);
    store.close().unwrap();
}

/// A mismatched epoch/target capture is `Stale`; an unsupported newer
/// target outranks every other state, all without object I/O.
#[test]
fn stale_capture_and_newer_target_block_without_object_io() {
    let (_scratch, _paths, fs, store) = counting_store("index-stale");
    acquire(&store, 1, b"Subject: s\r\n\r\nbody");
    enqueue_docs(&store, &[(1, IndexReason::New)], 10);
    let batch = capture(&store);
    write(&store, |tx| {
        tx.execute("UPDATE search_index_state SET epoch = 7 WHERE id = 0", [])
            .unwrap();
        Ok(())
    });
    let opens = fs.calls(FsOp::Open);
    let reads = fs.calls(FsOp::Read);
    assert!(matches!(
        prepare(&store, batch.clone(), &Limits::default(), &|| false).unwrap(),
        IndexPreparation::Stale
    ));
    assert_no_object_io(&fs, opens, reads);

    // A newer stored target outranks both the pause and the stale epoch.
    write(&store, |tx| {
        tx.execute(
            "UPDATE store_meta SET value=?1 WHERE key='search_extractor_version'",
            [(SEARCH_EXTRACTOR_VERSION + 1).to_string()],
        )
        .unwrap();
        set_user_pause(tx, UserPause::Pause)
    });
    assert!(matches!(
        prepare(&store, batch, &Limits::default(), &|| false).unwrap(),
        IndexPreparation::BlockedNewerExtractor { target } if target == SEARCH_EXTRACTOR_VERSION + 1
    ));
    assert_no_object_io(&fs, opens, reads);
    store.close().unwrap();
}

#[test]
fn cancellation_discards_the_unsubmitted_prefix() {
    let (_scratch, _paths, fs, store) = counting_store("index-cancel");
    acquire(&store, 1, b"Subject: s\r\n\r\nbody");
    acquire(&store, 2, b"Subject: t\r\n\r\nother");
    enqueue_docs(&store, &[(1, IndexReason::New), (2, IndexReason::New)], 10);
    let batch = capture(&store);
    // Before any I/O at all.
    assert!(matches!(
        prepare(&store, batch.clone(), &Limits::default(), &|| true).unwrap(),
        IndexPreparation::Cancelled
    ));
    let reads = fs.calls(FsOp::Read);
    // Cancel once the first candidate's two reads (chunk + EOF) have
    // run: the second candidate's stream stops mid-verification and the
    // whole prepared prefix is discarded.
    let cancelled = || fs.calls(FsOp::Read) > reads + 2;
    assert!(matches!(
        prepare(&store, batch, &Limits::default(), &cancelled).unwrap(),
        IndexPreparation::Cancelled
    ));
    assert_eq!(pending_ids(&store), [1, 2], "nothing was acknowledged");
    store.close().unwrap();
}

/// A requeued revision or a started rebuild between capture and
/// publication keeps stale work fenced: the writer accepts nothing.
#[test]
fn requeue_and_rebuild_between_capture_and_publication_stay_fenced() {
    let (_scratch, _paths, store) = store_on(real_fs(), "index-fence");
    acquire(&store, 1, b"Subject: s\r\n\r\nbody");
    enqueue_docs(&store, &[(1, IndexReason::New)], 10);

    // Requeue between capture and publication: the old token is skipped.
    let batch = capture(&store);
    enqueue_docs(&store, &[(1, IndexReason::Reextract)], 20);
    let prepared = match prepare(&store, batch, &Limits::default(), &|| false).unwrap() {
        IndexPreparation::Prepared(prepared) => prepared,
        other => panic!("expected Prepared, got {other:?}"),
    };
    let fence = prepared.fence();
    let publications = prepared.publications().to_vec();
    assert_eq!(
        write(&store, move |tx| publish_batch(tx, fence, &publications)),
        0
    );
    assert_eq!(pending_ids(&store), [1]);

    // A rebuild between preparation and publication: the epoch gate
    // refuses the whole submission.
    let prepared = match prepare(&store, capture(&store), &Limits::default(), &|| false).unwrap() {
        IndexPreparation::Prepared(prepared) => prepared,
        other => panic!("expected Prepared, got {other:?}"),
    };
    write(&store, |tx| {
        start_search_rebuild(tx, "job-2", SEARCH_EXTRACTOR_VERSION).map(|_| ())
    });
    let fence = prepared.fence();
    let publications = prepared.publications().to_vec();
    assert_eq!(
        write(&store, move |tx| publish_batch(tx, fence, &publications)),
        0
    );
    assert_eq!(pending_ids(&store), [1]);
    // And a fresh capture now reports the rebuild phase, not work.
    assert!(matches!(
        prepare(&store, capture(&store), &Limits::default(), &|| false).unwrap(),
        IndexPreparation::Blocked(IndexBlocked::Rebuilding(RebuildPhase::Reset))
    ));
    store.close().unwrap();
}

#[test]
fn an_oversized_caller_capture_is_rejected_before_object_io() {
    let (_scratch, _paths, fs, store) = counting_store("index-capture-bound");
    let batch = IndexBatch {
        fence: capture(&store).fence,
        documents: (1..=201)
            .map(|id| PendingDocument {
                doc_id: id,
                revision: id,
                reason: IndexReason::Delete,
                enqueued_at: Timestamp::from_millis(0),
            })
            .collect(),
    };
    let opens = fs.calls(FsOp::Open);
    let reads = fs.calls(FsOp::Read);
    assert!(matches!(
        prepare(&store, batch, &Limits::default(), &|| false),
        Err(StoreError::InvalidInput { .. })
    ));
    assert_no_object_io(&fs, opens, reads);
    store.close().unwrap();
}

/// Classification reads only the typed `io_kind`: every
/// non-`Unavailable` variant is `None`, and a misleading formatted
/// reason can never reclassify the native cause.
#[test]
fn only_typed_unavailable_causes_map_to_io_pause_codes() {
    use sb_store::index::rebuild::SearchIoCode;
    use std::io::ErrorKind;

    let unavailable = |io_kind: Option<ErrorKind>| StoreError::Unavailable {
        reason: "permission denied reading /private/path".to_owned(),
        io_kind,
    };
    for (kind, expected) in [
        (Some(ErrorKind::StorageFull), SearchIoCode::StorageFull),
        (
            Some(ErrorKind::PermissionDenied),
            SearchIoCode::PermissionDenied,
        ),
        (
            Some(ErrorKind::ReadOnlyFilesystem),
            SearchIoCode::PermissionDenied,
        ),
        (Some(ErrorKind::NotFound), SearchIoCode::StorageUnavailable),
        (
            Some(ErrorKind::NotConnected),
            SearchIoCode::StorageUnavailable,
        ),
        (
            Some(ErrorKind::BrokenPipe),
            SearchIoCode::StorageUnavailable,
        ),
        (
            Some(ErrorKind::ConnectionReset),
            SearchIoCode::StorageUnavailable,
        ),
        (
            Some(ErrorKind::ConnectionAborted),
            SearchIoCode::StorageUnavailable,
        ),
        (
            Some(ErrorKind::ConnectionRefused),
            SearchIoCode::StorageUnavailable,
        ),
        (Some(ErrorKind::TimedOut), SearchIoCode::StorageUnavailable),
        (Some(ErrorKind::ResourceBusy), SearchIoCode::OtherIo),
        (Some(ErrorKind::OutOfMemory), SearchIoCode::OtherIo),
        (Some(ErrorKind::Other), SearchIoCode::OtherIo),
        (Some(ErrorKind::UnexpectedEof), SearchIoCode::OtherIo),
        (Some(ErrorKind::InvalidInput), SearchIoCode::OtherIo),
        (None, SearchIoCode::OtherIo),
    ] {
        assert_eq!(
            search_io_code(&unavailable(kind)),
            Some(expected),
            "{kind:?}"
        );
    }
    for error in [
        StoreError::NotFound { what: "x".into() },
        StoreError::Corrupt { detail: "x".into() },
        StoreError::InvalidInput {
            message: "x".into(),
        },
        StoreError::Conflict { detail: "x".into() },
        StoreError::Internal { detail: "x".into() },
        StoreError::NewerStore {
            app_version: "9".into(),
        },
        StoreError::Locked,
    ] {
        assert_eq!(search_io_code(&error), None, "{error:?}");
    }
}

/// The persisted text is fixed per code, sanitized, actionable and
/// under the store's 512-byte `io_message` bound.
#[test]
fn io_pause_messages_are_fixed_bounded_and_actionable() {
    use sb_store::index::rebuild::{MAX_IO_MESSAGE_BYTES, SearchIoCode};

    let mut seen = std::collections::BTreeSet::new();
    for code in [
        SearchIoCode::StorageFull,
        SearchIoCode::PermissionDenied,
        SearchIoCode::StorageUnavailable,
        SearchIoCode::OtherIo,
    ] {
        let message = search_io_message(code);
        assert!(!message.is_empty() && message.len() < MAX_IO_MESSAGE_BYTES);
        assert!(seen.insert(message), "{code:?} shares a message");
    }
    // Full and denied get their specific actions; the others advise
    // restoring storage or letting the retry run.
    assert!(search_io_message(SearchIoCode::StorageFull).contains("disk space"));
    assert!(search_io_message(SearchIoCode::PermissionDenied).contains("permission"));
    assert!(search_io_message(SearchIoCode::StorageUnavailable).contains("local disk"));
}

/// The test-seams clock reaches both the store seams and
/// [`Resources::now`], while `open` and `open_with_fs` keep the system
/// clock (spec/11).
#[cfg(feature = "test-seams")]
#[test]
fn open_with_clock_drives_store_and_facade_time() {
    let fixed = Timestamp::from_millis(1_735_689_600_000);
    let scratch = ScratchDir::new("index-clock");
    let root = scratch.path().join("clocked");
    let res =
        Resources::open_with_clock(&root, real_fs(), Arc::new(FixedClock::new(fixed))).unwrap();
    assert_eq!(res.now(), fixed);
    let created_at: String = res
        .read(|tx| {
            tx.query_row(
                "SELECT value FROM store_meta WHERE key = 'created_at'",
                [],
                |row| row.get(0),
            )
            .map_err(|error| StoreError::internal(error.to_string()))
        })
        .unwrap();
    assert_eq!(created_at, fixed.as_millis().to_string());
    res.close().unwrap();

    // Uninjected opens still take the system clock.
    let res = Resources::open_with_fs(&scratch.path().join("system"), real_fs()).unwrap();
    let now = res.now().as_millis();
    assert!(
        now > fixed.as_millis(),
        "the system clock is real time: {now}"
    );
    res.close().unwrap();
}
