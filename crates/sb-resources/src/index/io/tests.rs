//! I/O report façade tests on real store transactions (spec/06
//! "Publication I/O reports"): every verdict, generation accounting,
//! rollback on injected SQL failure, and the native `SQLITE_FULL`
//! proof on an owned `Resources` writer bound.

use std::io::ErrorKind;
use std::sync::Arc;

use sb_store::index::rebuild::{
    SearchIndexStatus, UserPause, pause_for_io, rebuild_reset_batch, set_user_pause,
    start_search_rebuild,
};
use sb_store::index::{
    IndexBatch, IndexDocument, IndexPublication, IndexReason, IndexState, MAX_INDEX_BATCH, enqueue,
    pending_batch, publish_batch, search_generation,
};
use sb_store::{Seams, Store, StorePaths, WritePriority};
use sb_test_support::{FixedClock, ScratchDir, SeededRandom, block_on, real_fs};

use super::*;
use crate::Resources;

const NOW: i64 = 1_735_689_600_000;

fn now(millis: i64) -> Timestamp {
    Timestamp::from_millis(NOW + millis)
}

fn fixture(label: &str) -> (ScratchDir, Store) {
    let scratch = ScratchDir::new(label);
    let paths = StorePaths::new(scratch.path().join("store")).unwrap();
    let seams = Seams::new(
        real_fs(),
        Arc::new(FixedClock::new(Timestamp::from_millis(NOW))),
        Arc::new(SeededRandom::new(9)),
    );
    let store = sb_store::open(&paths, &seams).unwrap();
    write(&store, |tx| {
        tx.execute(
            "UPDATE store_meta SET value=?1 WHERE key='search_extractor_version'",
            [SEARCH_EXTRACTOR_VERSION.to_string()],
        )
        .map(|_| ())
        .map_err(|error| StoreError::internal(error.to_string()))
    })
    .unwrap();
    (scratch, store)
}

fn write<T: Send + 'static>(
    store: &Store,
    f: impl FnOnce(&Transaction) -> Result<T, StoreError> + Send + 'static,
) -> Result<T, StoreError> {
    block_on(store.write(WritePriority::User, f))
}

fn search_gen(store: &Store) -> i64 {
    store.read(|tx| search_generation(tx)).unwrap()
}

fn status(store: &Store) -> SearchIndexStatus {
    store.read(|tx| search_index_status(tx)).unwrap()
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

/// A synthetic published acquisition plus its `new` queue token: the
/// report paths never read the object, so no file is needed.
fn queue_doc(store: &Store, id: i64) {
    write(store, move |tx| {
        tx.execute_batch(&format!(
            "INSERT INTO acquisitions (acquisition_id, content_sha256, byte_length,
               object_state, first_acquired_at)
             VALUES ({id}, '{id:064x}', 1, 'published', 1)"
        ))
        .map_err(|error| StoreError::internal(error.to_string()))?;
        enqueue(tx, &[(id, IndexReason::New)], Timestamp::from_millis(10))
    })
    .unwrap();
}

fn full() -> StoreError {
    StoreError::Unavailable {
        reason: "synthetic storage-full report".into(),
        io_kind: Some(ErrorKind::StorageFull),
    }
}

fn report(
    store: &Store,
    context: IndexIoContext<'static>,
    error: StoreError,
    at: i64,
) -> Result<IndexIoUpdate, StoreError> {
    write(store, move |tx| {
        report_index_io_failure(tx, context, &error, Timestamp::from_millis(at))
    })
}

fn recover(
    store: &Store,
    context: IndexIoContext<'static>,
    error: StoreError,
    at: i64,
) -> Result<IndexIoUpdate, StoreError> {
    write(store, move |tx| {
        recover_index_io_failure(tx, context, &error, Timestamp::from_millis(at))
    })
}

fn report_doc(
    store: &Store,
    fence: IndexFence,
    pending: PendingDocument,
    at: Timestamp,
) -> Result<IndexIoUpdate, StoreError> {
    write(store, move |tx| {
        report_index_io_failure(
            tx,
            IndexIoContext::Document {
                fence,
                pending: &pending,
            },
            &full(),
            at,
        )
    })
}

#[test]
fn retry_deadlines_double_to_the_cap_and_saturate() {
    for (attempts, delay) in [
        (0u32, 1_000i64),
        (1, 1_000),
        (2, 2_000),
        (3, 4_000),
        (6, 32_000),
        (7, 60_000),
        (8, 60_000),
        (u32::MAX, 60_000),
    ] {
        assert_eq!(index_retry_at(now(1), attempts), now(1 + delay));
    }
    assert_eq!(
        index_retry_at(Timestamp::from_millis(i64::MAX), 3),
        Timestamp::from_millis(i64::MAX)
    );
    assert_eq!(
        index_retry_at(Timestamp::from_millis(i64::MAX - 10), u32::MAX),
        Timestamp::from_millis(i64::MAX)
    );
}

/// Only `Unavailable` is a report; a negative clock is invalid. Both
/// reject before any store write, in either entry point.
#[test]
fn non_io_failures_and_negative_times_are_rejected() {
    let (_scratch, store) = fixture("io-reject");
    let fence = capture(&store).fence;
    let generation = search_gen(&store);
    for error in [
        StoreError::NotFound { what: "x".into() },
        StoreError::Corrupt { detail: "x".into() },
        StoreError::Internal { detail: "x".into() },
        StoreError::InvalidInput {
            message: "x".into(),
        },
        StoreError::Conflict { detail: "x".into() },
        StoreError::Locked,
        StoreError::NewerStore {
            app_version: "9".into(),
        },
    ] {
        for context in [IndexIoContext::Metadata, IndexIoContext::Batch { fence }] {
            assert!(
                matches!(
                    report(&store, context, error.clone(), 1),
                    Err(StoreError::InvalidInput { .. })
                ),
                "{error:?}"
            );
            assert!(matches!(
                recover(&store, context, error.clone(), 1),
                Err(StoreError::InvalidInput { .. })
            ));
        }
    }
    assert!(matches!(
        report(&store, IndexIoContext::Metadata, full(), -1),
        Err(StoreError::InvalidInput { .. })
    ));
    assert!(matches!(
        recover(&store, IndexIoContext::Metadata, full(), -1),
        Err(StoreError::InvalidInput { .. })
    ));
    assert_eq!(search_gen(&store), generation);
    assert!(status(&store).io_pause().is_none());
    store.close().unwrap();
}

/// A metadata report records the sanitized pause and advances
/// generation once; a newer report inside the deadline preserves the
/// recorded cause verbatim, and a due one begins the retry with its
/// attempt history intact.
#[test]
fn metadata_reports_pause_preserve_and_begin() {
    let (_scratch, store) = fixture("io-metadata");
    let generation = search_gen(&store);
    assert_eq!(
        report(&store, IndexIoContext::Metadata, full(), now(0).as_millis()).unwrap(),
        IndexIoUpdate::Paused {
            retry_at: now(1_000)
        }
    );
    let state = status(&store);
    let pause = state.io_pause().unwrap().clone();
    assert_eq!(pause.code(), SearchIoCode::StorageFull);
    // The durable text is the fixed actionable message, never the
    // reported error's raw content.
    assert_eq!(
        pause.message(),
        search_io_message(SearchIoCode::StorageFull)
    );
    assert!(!pause.message().contains("synthetic"));
    assert_eq!(pause.retry_at(), now(1_000));
    assert_eq!(state.io_attempts(), 1);
    assert_eq!(search_gen(&store), generation + 1);

    // A second report inside the deadline keeps the recorded cause:
    // same code/message/deadline, no write.
    assert_eq!(
        report(
            &store,
            IndexIoContext::Metadata,
            full(),
            now(999).as_millis()
        )
        .unwrap(),
        IndexIoUpdate::Paused {
            retry_at: now(1_000)
        }
    );
    assert_eq!(search_gen(&store), generation + 1);
    assert_eq!(status(&store).io_pause().unwrap().retry_at(), now(1_000));

    // At the deadline the metadata report begins the retry: the hold
    // clears, history survives, one generation.
    assert_eq!(
        report(
            &store,
            IndexIoContext::Metadata,
            full(),
            now(1_000).as_millis()
        )
        .unwrap(),
        IndexIoUpdate::RetryBegun
    );
    let state = status(&store);
    assert!(state.io_pause().is_none());
    assert_eq!(state.io_attempts(), 1);
    assert_eq!(search_gen(&store), generation + 2);

    // The next failure records attempts=2 with the doubled deadline.
    assert_eq!(
        report(
            &store,
            IndexIoContext::Metadata,
            full(),
            now(1_000).as_millis()
        )
        .unwrap(),
        IndexIoUpdate::Paused {
            retry_at: now(3_000)
        }
    );
    assert_eq!(status(&store).io_attempts(), 2);
    assert_eq!(search_gen(&store), generation + 3);
    store.close().unwrap();
}

/// User hold, a reset/enqueue phase and a newer stored target each
/// report `Blocked` and write nothing; the I/O hold alone preserves
/// its record and both holds together still block.
#[test]
fn holds_phases_and_newer_targets_block_without_writing() {
    let (_scratch, store) = fixture("io-blocked");
    queue_doc(&store, 1);

    write(&store, |tx| set_user_pause(tx, UserPause::Pause)).unwrap();
    let held = search_gen(&store);
    assert_eq!(
        report(&store, IndexIoContext::Metadata, full(), now(0).as_millis()).unwrap(),
        IndexIoUpdate::Blocked
    );
    assert_eq!(search_gen(&store), held);
    assert!(status(&store).io_pause().is_none());

    // Both holds: the user hold still blocks; the recorded I/O pause
    // is not released by the report.
    write(&store, |tx| {
        pause_for_io(
            tx,
            SearchIoCode::StorageFull,
            "earlier cause",
            now(9_000),
            3,
        )
    })
    .unwrap();
    let held = search_gen(&store);
    assert_eq!(
        report(&store, IndexIoContext::Metadata, full(), now(0).as_millis()).unwrap(),
        IndexIoUpdate::Blocked
    );
    let state = status(&store);
    assert!(state.user_paused());
    assert_eq!(state.io_pause().unwrap().code(), SearchIoCode::StorageFull);
    assert_eq!(state.io_pause().unwrap().message(), "earlier cause");
    assert_eq!(state.io_attempts(), 3);
    assert_eq!(search_gen(&store), held);
    store.close().unwrap();

    // Reset and enqueue now block only a document-context report: the
    // same worker drives those phases, so metadata and captured-batch
    // reports retry the supported phase work there.
    let (_s1, store) = fixture("io-blocked-phases");
    queue_doc(&store, 1);
    write(&store, |tx| {
        start_search_rebuild(tx, "job-1", SEARCH_EXTRACTOR_VERSION).map(|_| ())
    })
    .unwrap();
    let held = search_gen(&store);
    let batch = capture(&store);
    let pending = batch.documents[0].clone();
    assert_eq!(
        report_doc(&store, batch.fence, pending, now(0)).unwrap(),
        IndexIoUpdate::Blocked
    );
    // The batch's own phase fence records a pause; a due metadata
    // report begins the retry with its history preserved.
    assert_eq!(
        report(
            &store,
            IndexIoContext::Batch { fence: batch.fence },
            full(),
            now(0).as_millis()
        )
        .unwrap(),
        IndexIoUpdate::Paused {
            retry_at: now(1_000)
        }
    );
    assert_eq!(
        report(
            &store,
            IndexIoContext::Metadata,
            full(),
            now(1_000).as_millis()
        )
        .unwrap(),
        IndexIoUpdate::RetryBegun
    );
    assert_eq!(status(&store).io_attempts(), 1);
    // Driving reset into enqueue keeps document reports blocked.
    let epoch = store
        .read(|tx| Ok(latest_rebuild(tx)?.unwrap().epoch()))
        .unwrap();
    write(&store, move |tx| {
        rebuild_reset_batch(tx, "job-1", epoch, MAX_INDEX_BATCH)
    })
    .unwrap();
    let batch = capture(&store);
    let pending = batch.documents[0].clone();
    assert_eq!(
        report_doc(&store, batch.fence, pending, now(2_000)).unwrap(),
        IndexIoUpdate::Blocked
    );
    assert_eq!(
        search_gen(&store),
        held + 3,
        "only the pause, begin and reset commits advanced"
    );
    store.close().unwrap();

    // A newer stored target outranks everything, captured or not.
    let (_s2, store2) = fixture("io-blocked-target");
    queue_doc(&store2, 1);
    let batch = capture(&store2);
    write(&store2, |tx| {
        tx.execute(
            "UPDATE store_meta SET value=?1 WHERE key='search_extractor_version'",
            [(SEARCH_EXTRACTOR_VERSION + 1).to_string()],
        )
        .map_err(|error| StoreError::internal(error.to_string()))
    })
    .unwrap();
    let generation2 = search_gen(&store2);
    for context in [
        IndexIoContext::Metadata,
        IndexIoContext::Batch { fence: batch.fence },
    ] {
        assert_eq!(
            report(&store2, context, full(), now(0).as_millis()).unwrap(),
            IndexIoUpdate::Blocked
        );
    }
    assert_eq!(search_gen(&store2), generation2);
    assert!(status(&store2).io_pause().is_none());
    store2.close().unwrap();
}

#[test]
fn an_unsupported_target_blocks_every_context_and_recovery_only_republishes_status() {
    type Operation = fn(
        &Transaction,
        IndexIoContext<'_>,
        &StoreError,
        Timestamp,
    ) -> Result<IndexIoUpdate, StoreError>;
    enum Context {
        Metadata,
        Batch,
        Document,
    }
    let (_scratch, store) = fixture("io-older-target");
    queue_doc(&store, 1);
    write(&store, |tx| {
        tx.execute(
            "UPDATE store_meta SET value=?1 WHERE key='search_extractor_version'",
            [(SEARCH_EXTRACTOR_VERSION + 1).to_string()],
        )
        .map_err(|error| StoreError::internal(error.to_string()))
    })
    .unwrap();
    let batch = capture(&store);
    for (operation, advance) in [
        (report_index_io_failure as Operation, 0),
        (recover_index_io_failure as Operation, 1),
    ] {
        for context in [Context::Metadata, Context::Batch, Context::Document] {
            let before = search_gen(&store);
            let pending = batch.documents[0].clone();
            let fence = batch.fence;
            let outcome = write(&store, move |tx| {
                let context = match context {
                    Context::Metadata => IndexIoContext::Metadata,
                    Context::Batch => IndexIoContext::Batch { fence },
                    Context::Document => IndexIoContext::Document {
                        fence,
                        pending: &pending,
                    },
                };
                operation(tx, context, &full(), now(0))
            })
            .unwrap();
            assert_eq!(outcome, IndexIoUpdate::Blocked);
            assert_eq!(search_gen(&store), before + advance);
            assert!(status(&store).io_pause().is_none());
            assert_eq!(status(&store).io_attempts(), 0);
            assert_eq!(pending_ids(&store), [1]);
        }
    }
    store.close().unwrap();
}

/// Stale epochs, targets and tokens are `Stale` for captured work
/// while uncaptured metadata re-fences itself; a due recorded pause
/// refuses captured work the same way without clearing the hold.
#[test]
fn stale_captures_recapture_and_due_pauses_keep_their_hold() {
    let (_scratch, store) = fixture("io-stale");
    queue_doc(&store, 1);
    let batch = capture(&store);
    let pending = batch.documents[0].clone();
    let generation = search_gen(&store);

    // An epoch the store no longer holds: Stale for captured work.
    let stale_epoch = IndexFence {
        epoch: batch.fence.epoch + 9,
        version: batch.fence.version,
    };
    assert_eq!(
        report_doc(&store, stale_epoch, pending.clone(), now(0)).unwrap(),
        IndexIoUpdate::Stale
    );
    assert_eq!(
        report(
            &store,
            IndexIoContext::Batch { fence: stale_epoch },
            full(),
            now(0).as_millis()
        )
        .unwrap(),
        IndexIoUpdate::Stale
    );
    assert_eq!(search_gen(&store), generation);
    assert!(status(&store).io_pause().is_none());

    // A captured-but-different target fences the same way.
    let stale_target = IndexFence {
        epoch: batch.fence.epoch,
        version: batch.fence.version + 1,
    };
    assert_eq!(
        report(
            &store,
            IndexIoContext::Batch {
                fence: stale_target
            },
            full(),
            now(0).as_millis()
        )
        .unwrap(),
        IndexIoUpdate::Stale
    );

    // A superseded token: requeue, then the old token's report is Stale.
    write(&store, move |tx| {
        enqueue(
            tx,
            &[(1, IndexReason::Reextract)],
            Timestamp::from_millis(20),
        )
    })
    .unwrap();
    let generation = search_gen(&store);
    assert_eq!(
        report_doc(&store, batch.fence, pending.clone(), now(0)).unwrap(),
        IndexIoUpdate::Stale
    );
    assert_eq!(search_gen(&store), generation);
    assert_eq!(pending_ids(&store), [1]);

    // A due recorded pause is Stale for captured work — the driver
    // recaptures — and the hold keeps its cause and deadline.
    let batch = capture(&store);
    let pending = batch.documents[0].clone();
    write(&store, |tx| {
        pause_for_io(
            tx,
            SearchIoCode::StorageUnavailable,
            "recorded earlier",
            now(500),
            2,
        )
    })
    .unwrap();
    let generation = search_gen(&store);
    assert_eq!(
        report_doc(&store, batch.fence, pending.clone(), now(600)).unwrap(),
        IndexIoUpdate::Stale
    );
    assert_eq!(
        report(
            &store,
            IndexIoContext::Batch { fence: batch.fence },
            full(),
            now(600).as_millis()
        )
        .unwrap(),
        IndexIoUpdate::Stale
    );
    let pause = status(&store).io_pause().unwrap().clone();
    assert_eq!(pause.code(), SearchIoCode::StorageUnavailable);
    assert_eq!(pause.retry_at(), now(500));
    assert_eq!(status(&store).io_attempts(), 2);
    assert_eq!(search_gen(&store), generation);
    store.close().unwrap();
}

/// Persisted history saturates: `io_attempts` at or above `u32::MAX`
/// stays saturated and the deadline stays at the cap.
#[test]
fn attempt_counters_saturate_without_overflow() {
    let (_scratch, store) = fixture("io-saturate");
    for stored in [u32::MAX as i64, i64::MAX] {
        write(&store, move |tx| {
            tx.execute(
                "UPDATE search_index_state SET io_attempts = ?1 WHERE id = 0",
                [stored],
            )
            .map_err(|error| StoreError::internal(error.to_string()))
        })
        .unwrap();
        assert_eq!(
            report(&store, IndexIoContext::Metadata, full(), now(0).as_millis()).unwrap(),
            IndexIoUpdate::Paused {
                retry_at: now(60_000)
            }
        );
        assert_eq!(status(&store).io_attempts(), u32::MAX as i64);
        write(&store, clear_hold).unwrap();
    }
    store.close().unwrap();
}

fn clear_hold(tx: &Transaction) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE search_index_state
         SET io_code = NULL, io_message = NULL, io_retry_at = NULL WHERE id = 0",
        [],
    )
    .map_err(|error| StoreError::internal(error.to_string()))?;
    Ok(())
}

/// Recovery always commits exactly one validated generation: a
/// recorded pause or begun retry carries the barrier itself; stale,
/// held and already-held outcomes republish the protected status,
/// byte-identical.
#[test]
fn recovery_commits_one_validated_generation_for_every_outcome() {
    let (_scratch, store) = fixture("io-recovery");
    queue_doc(&store, 1);
    let batch = capture(&store);
    let generation = search_gen(&store);

    // Committing outcome: the pause's own generation is the barrier.
    assert_eq!(
        recover(&store, IndexIoContext::Metadata, full(), now(0).as_millis()).unwrap(),
        IndexIoUpdate::Paused {
            retry_at: now(1_000)
        }
    );
    assert_eq!(search_gen(&store), generation + 1);
    assert!(status(&store).io_pause().is_some());

    // Already-held outcome: the protected pause survives verbatim and
    // exactly one recovery generation commits.
    let before = status(&store).io_pause().unwrap().clone();
    assert_eq!(
        recover(
            &store,
            IndexIoContext::Metadata,
            full(),
            now(500).as_millis()
        )
        .unwrap(),
        IndexIoUpdate::Paused {
            retry_at: now(1_000)
        }
    );
    let held = status(&store);
    assert_eq!(held.io_pause().unwrap(), &before);
    assert_eq!(held.io_attempts(), 1);
    assert_eq!(search_gen(&store), generation + 2);

    // Due begin: the retry's own generation is the barrier — not two.
    assert_eq!(
        recover(
            &store,
            IndexIoContext::Metadata,
            full(),
            now(1_000).as_millis()
        )
        .unwrap(),
        IndexIoUpdate::RetryBegun
    );
    assert_eq!(search_gen(&store), generation + 3);
    assert!(status(&store).io_pause().is_none());
    assert_eq!(status(&store).io_attempts(), 1);

    // Stale captured failure: the republish barrier still commits once.
    let stale = IndexFence {
        epoch: batch.fence.epoch + 9,
        version: batch.fence.version,
    };
    assert_eq!(
        recover(
            &store,
            IndexIoContext::Batch { fence: stale },
            full(),
            now(1_500).as_millis()
        )
        .unwrap(),
        IndexIoUpdate::Stale
    );
    assert_eq!(search_gen(&store), generation + 4);
    assert_eq!(pending_ids(&store), [1]);

    // User-held: the blocked report still validates and republishes
    // protected status once; the hold itself is untouched.
    write(&store, |tx| set_user_pause(tx, UserPause::Pause)).unwrap();
    let held = search_gen(&store);
    assert_eq!(
        recover(
            &store,
            IndexIoContext::Metadata,
            full(),
            now(1_500).as_millis()
        )
        .unwrap(),
        IndexIoUpdate::Blocked
    );
    assert_eq!(search_gen(&store), held + 1);
    assert!(status(&store).user_paused());
    assert!(status(&store).io_pause().is_none());
    store.close().unwrap();
}

/// Missing or corrupt durable state fails explicitly: the singleton,
/// the stored target and the latest job's metadata all validate in
/// the reporting transaction.
#[test]
fn missing_and_corrupt_state_is_explicit() {
    let (_scratch, store) = fixture("io-corrupt");

    // Missing singleton.
    write(&store, |tx| {
        tx.execute("DELETE FROM search_index_state WHERE id = 0", [])
            .map_err(|error| StoreError::internal(error.to_string()))
    })
    .unwrap();
    for result in [
        report(&store, IndexIoContext::Metadata, full(), now(0).as_millis()),
        recover(&store, IndexIoContext::Metadata, full(), now(0).as_millis()),
    ] {
        assert!(
            matches!(result, Err(StoreError::Corrupt { .. })),
            "{result:?}"
        );
    }

    // Corrupt stored target.
    let (_s2, store) = fixture("io-corrupt-target");
    write(&store, |tx| {
        tx.execute(
            "UPDATE store_meta SET value = 'banana' WHERE key = 'search_extractor_version'",
            [],
        )
        .map_err(|error| StoreError::internal(error.to_string()))
    })
    .unwrap();
    assert!(matches!(
        report(&store, IndexIoContext::Metadata, full(), now(0).as_millis()),
        Err(StoreError::Corrupt { .. })
    ));
    assert!(matches!(
        recover(&store, IndexIoContext::Metadata, full(), now(0).as_millis()),
        Err(StoreError::Corrupt { .. })
    ));

    // Corrupt latest-job metadata: a temp view shadows the table on
    // the writer connection with an unparsable phase.
    let (_s3, store) = fixture("io-corrupt-job");
    write(&store, |tx| {
        tx.execute_batch(
            "CREATE TEMP VIEW search_rebuild_jobs AS
             SELECT 'job' AS job_id, 1 AS epoch, 'bogus' AS phase,
                    0 AS doc_cursor, 0 AS acq_cursor, 0 AS phase_count",
        )
        .map_err(|error| StoreError::internal(error.to_string()))
    })
    .unwrap();
    for result in [
        report(&store, IndexIoContext::Metadata, full(), now(0).as_millis()),
        recover(&store, IndexIoContext::Metadata, full(), now(0).as_millis()),
    ] {
        assert!(
            matches!(result, Err(StoreError::Corrupt { .. })),
            "{result:?}"
        );
    }
    store.close().unwrap();
}

/// A real SQL failure on the `search_generation` update, after the
/// status write, rolls the whole operation back — report and recovery
/// alike. The fixture surfaces `Internal`, never a faked native Full.
#[test]
fn a_generation_sql_failure_rolls_back_report_and_recovery() {
    let (_scratch, store) = fixture("io-rollback");
    queue_doc(&store, 1);
    let batch = capture(&store);
    let pending = batch.documents[0].clone();
    write(&store, |tx| {
        tx.execute_batch(
            "CREATE TEMP TRIGGER reject_search_generation
             BEFORE UPDATE OF value ON store_meta
             WHEN OLD.key = 'search_generation'
             BEGIN SELECT RAISE(ABORT, 'injected generation SQL failure'); END;",
        )
        .map_err(|error| StoreError::internal(error.to_string()))
    })
    .unwrap();
    let generation = search_gen(&store);

    // The pause write happens, then generation fails: nothing sticks.
    let result = write(&store, {
        let pending = pending.clone();
        move |tx| {
            report_index_io_failure(
                tx,
                IndexIoContext::Document {
                    fence: batch.fence,
                    pending: &pending,
                },
                &full(),
                now(0),
            )
        }
    });
    assert!(
        matches!(result, Err(StoreError::Internal { .. })),
        "{result:?}"
    );
    assert!(status(&store).io_pause().is_none());
    assert_eq!(status(&store).io_attempts(), 0);
    assert_eq!(search_gen(&store), generation);
    assert_eq!(pending_ids(&store), [1]);

    // Recovery's republish barrier fails the same way: a stale
    // capture reaches commit_index_recovery and rolls back whole.
    let stale = IndexFence {
        epoch: batch.fence.epoch + 9,
        version: batch.fence.version,
    };
    let result = recover(
        &store,
        IndexIoContext::Batch { fence: stale },
        full(),
        now(0).as_millis(),
    );
    assert!(
        matches!(result, Err(StoreError::Internal { .. })),
        "{result:?}"
    );
    assert_eq!(search_gen(&store), generation);

    // Without the trigger the same calls commit exactly once.
    write(&store, |tx| {
        tx.execute_batch("DROP TRIGGER reject_search_generation")
            .map_err(|error| StoreError::internal(error.to_string()))
    })
    .unwrap();
    assert_eq!(
        recover(
            &store,
            IndexIoContext::Batch { fence: stale },
            full(),
            now(0).as_millis()
        )
        .unwrap(),
        IndexIoUpdate::Stale
    );
    assert_eq!(search_gen(&store), generation + 1);
    store.close().unwrap();
}

/// A valid <1MB indexed document for `pending`; its column bytes
/// force the page growth the owned bound refuses.
fn big_publication(pending: PendingDocument) -> IndexPublication {
    let mut columns: [String; 8] = std::array::from_fn(|_| String::new());
    columns[4] = "a ".repeat(450_000);
    IndexPublication {
        pending,
        document: Some(IndexDocument {
            extractor_version: SEARCH_EXTRACTOR_VERSION,
            state: IndexState::Indexed,
            truncated_fields: vec![],
            columns,
        }),
    }
}

/// The native proof (spec/06): an owned `Resources` writer bounded at
/// its current page count makes a <1MB publication fail with real
/// `SQLITE_FULL`, while the small recovery transaction still commits
/// the typed pause. Raising the owned bound lets the retry publish.
/// No filesystem, quota or host limit is touched.
#[test]
fn an_owned_page_bound_fails_bulk_but_recovery_commits_and_retry_publishes() {
    let scratch = ScratchDir::new("io-native-full");
    let root = scratch.path().join("store");
    let resources = Resources::open(&root).unwrap();
    block_on(resources.write(WritePriority::User, move |tx| {
        tx.execute("UPDATE store_meta SET value=?1 WHERE key='search_extractor_version'", [SEARCH_EXTRACTOR_VERSION.to_string()])
            .map_err(|error| StoreError::internal(error.to_string()))?;
        tx.execute_batch(
            "INSERT INTO acquisitions (acquisition_id, content_sha256, byte_length,
                   object_state, first_acquired_at)
                  VALUES (1, '0000000000000000000000000000000000000000000000000000000000000001', 1, 'published', 1);
                 INSERT INTO d_index_queue (doc_id, reason, enqueued_at, revision)
                 VALUES (1, 'new', 1, 1);
                 UPDATE store_meta SET value = '1' WHERE key = 'index_inputs';",
        )
        .map_err(|error| StoreError::internal(error.to_string()))
    }))
    .unwrap();
    let (pages, bound) = block_on(resources.write(WritePriority::User, move |tx| {
        let pages: i64 = tx
            .query_row("PRAGMA page_count", [], |row| row.get(0))
            .map_err(|error| StoreError::internal(error.to_string()))?;
        let bound: i64 = tx
            .query_row(&format!("PRAGMA max_page_count = {pages}"), [], |row| {
                row.get(0)
            })
            .map_err(|error| StoreError::internal(error.to_string()))?;
        Ok((pages, bound))
    }))
    .unwrap();
    assert_eq!(pages, bound);

    // A valid captured publication whose columns need page growth.
    let batch = resources.pending_index_batch().unwrap();
    let fence = batch.fence;
    let pending = batch.documents[0].clone();
    let publication = big_publication(pending.clone());
    let generation = resources.read(|tx| search_generation(tx)).unwrap();
    let result = block_on(resources.write(WritePriority::Index, move |tx| {
        publish_batch(tx, fence, &[publication])
    }));
    let error = result.unwrap_err();
    assert!(matches!(error, StoreError::Unavailable { .. }), "{error:?}");
    assert_eq!(search_io_code(&error), Some(SearchIoCode::StorageFull));

    // The failure acknowledged nothing: queue, docs, postings and the
    // acquisition's authority are byte-identical.
    let unchanged = resources
        .read(|tx| {
            tx.query_row(
                "SELECT (SELECT count(*) FROM d_search_docs),
                         (SELECT count(*) FROM d_search_fts WHERE d_search_fts MATCH 'a'),
                        (SELECT count(*) FROM d_index_queue
                          WHERE doc_id = 1 AND revision = 1),
                        (SELECT object_state FROM acquisitions WHERE acquisition_id = 1)",
                [],
                |row| {
                    Ok((
                        row.get::<_, i64>(0)?,
                        row.get::<_, i64>(1)?,
                        row.get::<_, i64>(2)?,
                        row.get::<_, String>(3)?,
                    ))
                },
            )
            .map_err(|error| StoreError::internal(error.to_string()))
        })
        .unwrap();
    assert_eq!(unchanged, (0, 0, 1, "published".to_owned()));
    assert_eq!(
        resources.read(|tx| search_generation(tx)).unwrap(),
        generation
    );

    // The small recovery commit fits: one generation records the
    // typed StorageFull pause with its deadline and history.
    let update = block_on(resources.write(WritePriority::Index, {
        let pending = pending.clone();
        let error = error.clone();
        move |tx| {
            recover_index_io_failure(
                tx,
                IndexIoContext::Document {
                    fence,
                    pending: &pending,
                },
                &error,
                now(0),
            )
        }
    }))
    .unwrap();
    assert_eq!(
        update,
        IndexIoUpdate::Paused {
            retry_at: now(1_000)
        }
    );
    let state = resources.read(|tx| search_index_status(tx)).unwrap();
    let pause = state.io_pause().unwrap();
    assert_eq!(pause.code(), SearchIoCode::StorageFull);
    assert_eq!(
        pause.message(),
        search_io_message(SearchIoCode::StorageFull)
    );
    assert_eq!(pause.retry_at(), now(1_000));
    assert_eq!(state.io_attempts(), 1);
    assert_eq!(
        resources.read(|tx| search_generation(tx)).unwrap(),
        generation + 1
    );

    // Raising the owned bound, the due retry begins and the same
    // publication commits.
    block_on(resources.write(WritePriority::User, move |tx| {
        tx.query_row("PRAGMA max_page_count = 1000000", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|error| StoreError::internal(error.to_string()))
    }))
    .unwrap();
    let error = full();
    let update = block_on(resources.write(WritePriority::Index, move |tx| {
        report_index_io_failure(tx, IndexIoContext::Metadata, &error, now(2_000))
    }))
    .unwrap();
    assert_eq!(update, IndexIoUpdate::RetryBegun);
    let publication = big_publication(pending.clone());
    let accepted = block_on(resources.write(WritePriority::Index, move |tx| {
        publish_batch(tx, fence, &[publication])
    }))
    .unwrap();
    assert_eq!(accepted, 1);
    assert!(
        resources
            .pending_index_batch()
            .unwrap()
            .documents
            .is_empty()
    );
    resources.close().unwrap();
}
