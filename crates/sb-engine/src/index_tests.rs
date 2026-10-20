//! Real-worker publication, retry, shutdown and process-crash acceptance.

use std::path::Path;
use std::sync::atomic::{AtomicI64, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use sb_resources::{
    Clock, ImportJobState, Resources, StoreError, Timestamp, UserPause, WritePriority,
    request_index_retry, search_generation, set_user_pause,
};
use sb_test_support::{FaultFs, FaultKind, FsOp, ScratchDir, block_on, execute_sql, real_fs};

use crate::Engine;
use crate::index::Event;

const MESSAGE: &[u8] = b"Subject: searchable\n\nindexneedle\n";
const DOCS: &str = "SELECT count(*) FROM d_search_docs";
const QUEUE: &str = "SELECT count(*) FROM d_index_queue";
const STALE_CHANGES: [&str; 2] = [
    "UPDATE d_index_queue SET revision=revision+1; UPDATE store_meta SET value=CAST(value AS INTEGER)+1 WHERE key='index_inputs'",
    "UPDATE search_index_state SET epoch=epoch+1",
];

#[track_caller]
fn wait(mut ready: impl FnMut() -> bool) {
    // Same observation budget as the import suite on shared CI disks.
    let end = Instant::now() + Duration::from_secs(300);
    while !ready() {
        assert!(Instant::now() < end, "background work did not settle");
        // Observe completion without flooding the writer/read pool;
        // retry deadlines advance only through TestClock below.
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn count(engine: &Engine, sql: &str) -> i64 {
    engine
        .resources()
        .read(|tx| {
            tx.query_row(sql, [], |row| row.get(0))
                .map_err(|error| StoreError::internal(error.to_string()))
        })
        .unwrap()
}

fn write<T: Send + 'static>(
    engine: &Engine,
    body: impl FnOnce(&sb_resources::Transaction) -> Result<T, StoreError> + Send + 'static,
) -> T {
    block_on(engine.resources().write(WritePriority::User, body)).unwrap()
}

fn generation(engine: &Engine) -> i64 {
    engine.resources().read(|tx| search_generation(tx)).unwrap()
}

fn pause(engine: &Engine, hold: UserPause) {
    write(engine, move |tx| set_user_pause(tx, hold));
    engine.indexer().wake();
}

fn held(root: &Path) -> Engine {
    held_message(root, MESSAGE)
}

fn held_message(root: &Path, message: &[u8]) -> Engine {
    let engine = block_on(Engine::open(root)).unwrap();
    pause(&engine, UserPause::Pause);
    import(&engine, &root.with_extension("eml"), message);
    wait(|| {
        !engine
            .resources()
            .read(sb_resources::thread_revisions)
            .unwrap()
            .stale()
    });
    engine
}

fn status(engine: &Engine) -> sb_resources::SearchIndexStatus {
    engine.indexer().status(engine.resources()).unwrap()
}

fn hook(engine: &Engine, body: impl Fn(Event) + Send + Sync + 'static) {
    *engine.indexer().hook.lock().unwrap() = Some(Arc::new(body));
}

fn parked(engine: &Engine) -> mpsc::Receiver<()> {
    let (sent, received) = mpsc::channel();
    hook(engine, move |event| {
        if event == Event::Parked {
            let _ = sent.send(());
        }
    });
    engine.indexer().wake();
    received.recv_timeout(Duration::from_secs(30)).unwrap();
    received
}

#[derive(Default)]
struct TestClock(AtomicI64);

impl Clock for TestClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0.load(Ordering::SeqCst))
    }
}

fn clocked(root: &Path, fs: Arc<dyn sb_resources::Fs>, at: i64) -> (Engine, Arc<TestClock>) {
    let clock = Arc::new(TestClock(AtomicI64::new(at)));
    let engine = block_on(Engine::open_with_clock(root, fs, clock.clone())).unwrap();
    (engine, clock)
}

fn retry(engine: &Engine) {
    let now = engine.resources().now();
    write(engine, move |tx| request_index_retry(tx, now));
    engine.indexer().wake();
}

fn drained(engine: &Engine) {
    wait(|| count(engine, QUEUE) == 0);
    // Publication precedes the history-status write.
    wait(|| status(engine).io_attempts() == 0);
}

fn external(db: &Path, sql: &str) {
    execute_sql(db, &format!("PRAGMA busy_timeout=30000; {sql}"));
}

#[derive(Debug, PartialEq, Eq)]
struct Snapshot {
    docs: i64,
    queue: i64,
    generation: i64,
    hits: i64,
    acquisitions: i64,
}

fn snapshot(resources: &Resources) -> Snapshot {
    resources
        .read(|tx| {
            tx.query_row(
                "SELECT (SELECT count(*) FROM d_search_docs),
             (SELECT count(*) FROM d_index_queue),
             CAST((SELECT value FROM store_meta WHERE key='search_generation') AS INTEGER),
             (SELECT count(*) FROM d_search_fts WHERE d_search_fts MATCH 'indexneedle'),
             (SELECT count(*) FROM acquisitions)",
                [],
                |row| {
                    Ok(Snapshot {
                        docs: row.get(0)?,
                        queue: row.get(1)?,
                        generation: row.get(2)?,
                        hits: row.get(3)?,
                        acquisitions: row.get(4)?,
                    })
                },
            )
            .map_err(|error| StoreError::internal(error.to_string()))
        })
        .unwrap()
}

fn object(engine: &Engine, root: &Path) -> std::path::PathBuf {
    let hash: String = engine
        .resources()
        .read(|tx| {
            tx.query_row(
                "SELECT content_sha256 FROM acquisitions ORDER BY acquisition_id LIMIT 1",
                [],
                |row| row.get(0),
            )
            .map_err(|error| StoreError::internal(error.to_string()))
        })
        .unwrap();
    root.join("objects/sha256")
        .join(&hash[..2])
        .join(&hash[2..4])
        .join(hash)
}

fn import(engine: &Engine, path: &Path, bytes: &[u8]) {
    std::fs::write(path, bytes).unwrap();
    let started =
        block_on(engine.start_import(&engine.new_idempotency_key(), vec![path.into()])).unwrap();
    let id = started.job_id();
    wait(|| {
        block_on(engine.import_job(id)).unwrap().state() == ImportJobState::AwaitingConfirmation
    });
    block_on(engine.confirm_import(&engine.new_idempotency_key(), id, vec![])).unwrap();
    wait(|| {
        let job = block_on(engine.import_job(id)).unwrap();
        assert_ne!(
            job.state(),
            ImportJobState::Failed,
            "{:?}",
            job.checkpoint().failure()
        );
        job.state() == ImportJobState::Completed
    });
}

#[test]
fn a_real_import_is_indexed_without_a_manual_poke() {
    let scratch = ScratchDir::new("index-import");
    let engine = block_on(Engine::open(scratch.path().join("store"))).unwrap();
    import(&engine, &scratch.path().join("message.eml"), MESSAGE);
    wait(|| count(&engine, "SELECT count(*) FROM d_search_docs") == 1);
    assert_eq!(count(&engine, "SELECT count(*) FROM d_index_queue"), 0);
    assert_eq!(
        count(
            &engine,
            "SELECT count(*) FROM d_search_fts WHERE d_search_fts MATCH 'indexneedle'",
        ),
        1
    );
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn startup_and_both_publication_bounds_converge() {
    for (messages, body_size) in [(201, 1), (3, 999_970)] {
        let scratch = ScratchDir::new("index-bounds");
        let root = scratch.path().join("store");
        let engine = block_on(Engine::open(&root)).unwrap();
        pause(&engine, UserPause::Pause);
        let mut bytes = Vec::new();
        for n in 0..messages {
            bytes.extend_from_slice(
                format!("From sender Sat Oct 17 12:00:00 2026\nSubject: {n}\n\n").as_bytes(),
            );
            bytes.extend(std::iter::repeat_n(b'a', body_size));
            bytes.push(b'\n');
        }
        import(&engine, &scratch.path().join("many.mbox"), &bytes);
        assert_eq!(count(&engine, QUEUE), messages);
        let batches = Arc::new(Mutex::new(Vec::new()));
        let seen = batches.clone();
        hook(&engine, move |event| {
            if let Event::Prepared { documents, bytes } = event {
                seen.lock().unwrap().push((documents, bytes));
            }
        });
        pause(&engine, UserPause::Resume);
        drained(&engine);
        let batches = batches.lock().unwrap();
        assert!(batches.len() >= 2, "one publication bypassed a bound");
        assert!(
            batches
                .iter()
                .all(|(count, bytes)| *count <= 200 && *bytes <= 2_000_000)
        );
        assert_eq!(count(&engine, DOCS), messages);
        drop(batches);
        block_on(engine.shutdown()).unwrap();
        let engine = block_on(Engine::open(&root)).unwrap();
        assert_eq!(count(&engine, DOCS), messages);
        block_on(engine.shutdown()).unwrap();
    }
}

#[test]
fn a_wake_at_the_park_handoff_retains_new_input_and_one_worker() {
    let scratch = ScratchDir::new("index-wake");
    let engine = held(&scratch.path().join("store"));
    let once = Arc::new(AtomicUsize::new(0));
    let calls = once.clone();
    let weak = Arc::downgrade(engine.indexer());
    let root = scratch.path().join("store/state.sqlite");
    hook(&engine, move |event| {
        if event == Event::Parked && calls.fetch_add(1, Ordering::SeqCst) == 0 {
            external(&root, "UPDATE search_index_state SET user_paused=0");
            weak.upgrade().unwrap().wake();
        }
    });
    engine.schedule_index();
    engine.schedule_index();
    engine.indexer().wake();
    drained(&engine);
    assert_eq!(count(&engine, DOCS), 1);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn superseded_input_or_epoch_is_recaptured_without_acknowledging_old_work() {
    for update in STALE_CHANGES {
        let scratch = ScratchDir::new("index-stale");
        let engine = held(&scratch.path().join("store"));
        let changed = Arc::new(AtomicUsize::new(0));
        let calls = changed.clone();
        let db = scratch.path().join("store/state.sqlite");
        hook(&engine, move |event| {
            if matches!(event, Event::Prepared { .. }) && calls.fetch_add(1, Ordering::SeqCst) == 0
            {
                external(&db, update);
            }
        });
        pause(&engine, UserPause::Resume);
        drained(&engine);
        assert!(changed.load(Ordering::SeqCst) >= 2);
        assert_eq!(count(&engine, DOCS), 1);
        block_on(engine.shutdown()).unwrap();
    }
}

#[test]
fn typed_io_pause_honors_deadline_immediate_retry_and_the_independent_user_hold() {
    for (fault, code) in [
        (FaultKind::NoSpace, sb_resources::SearchIoCode::StorageFull),
        (
            FaultKind::Kind(std::io::ErrorKind::PermissionDenied),
            sb_resources::SearchIoCode::PermissionDenied,
        ),
        (FaultKind::Eio, sb_resources::SearchIoCode::OtherIo),
    ] {
        let scratch = ScratchDir::new("index-io");
        let root = scratch.path().join("store");
        let engine = held(&root);
        block_on(engine.shutdown()).unwrap();
        let fs = Arc::new(FaultFs::new(real_fs(), FsOp::Open, 1, fault));
        let (engine, clock) = clocked(&root, fs.clone(), 10_000);
        pause(&engine, UserPause::Resume);
        wait(|| status(&engine).io_pause().is_some());
        let state = status(&engine);
        let held = state.io_pause().unwrap();
        assert_eq!(held.code(), code);
        assert_eq!(held.retry_at().as_millis(), 11_000);
        assert_eq!(state.io_attempts(), 1);
        assert!(!held.message().contains(root.to_str().unwrap()));
        assert_eq!(count(&engine, QUEUE), 1);
        let opens = fs.calls(FsOp::Open);
        let notices = parked(&engine);
        engine.indexer().wake();
        notices.recv_timeout(Duration::from_secs(30)).unwrap();
        assert_eq!(fs.calls(FsOp::Open), opens);
        pause(&engine, UserPause::Pause);
        clock.0.store(20_000, Ordering::SeqCst);
        retry(&engine);
        notices.recv_timeout(Duration::from_secs(30)).unwrap();
        assert!(status(&engine).user_paused());
        assert!(status(&engine).io_pause().is_some());
        assert_eq!(fs.calls(FsOp::Open), opens);
        pause(&engine, UserPause::Resume);
        drained(&engine);
        assert!(status(&engine).io_pause().is_none());
        assert_eq!(count(&engine, DOCS), 1);
        block_on(engine.shutdown()).unwrap();
    }
}

#[test]
fn a_persisted_pause_survives_reopen_and_due_retries_keep_bounded_history() {
    let scratch = ScratchDir::new("index-retry-reopen");
    let root = scratch.path().join("store");
    let engine = held(&root);
    block_on(engine.shutdown()).unwrap();
    let fs = Arc::new(FaultFs::new(real_fs(), FsOp::Open, 1, FaultKind::NoSpace));
    let (engine, _) = clocked(&root, fs, 10_000);
    pause(&engine, UserPause::Resume);
    wait(|| status(&engine).io_pause().is_some());
    let expected = status(&engine).io_pause().unwrap().clone();
    block_on(engine.shutdown()).unwrap();
    let fs = Arc::new(FaultFs::new(real_fs(), FsOp::Open, 1, FaultKind::NoSpace));
    let (engine, clock) = clocked(&root, fs.clone(), 10_500);
    let parked = parked(&engine);
    assert_eq!(status(&engine).io_pause(), Some(&expected));
    assert_eq!(fs.calls(FsOp::Open), 0);
    clock.0.store(11_000, Ordering::SeqCst);
    engine.indexer().wake();
    wait(|| status(&engine).io_attempts() == 2);
    assert_eq!(
        status(&engine).io_pause().unwrap().retry_at().as_millis(),
        13_000
    );
    parked.recv_timeout(Duration::from_secs(30)).unwrap();
    retry(&engine);
    drained(&engine);
    assert_eq!(status(&engine).io_attempts(), 0);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn missing_and_quarantined_reads_preserve_authority_and_publish_only_the_delete() {
    for corrupt in [false, true] {
        let scratch = ScratchDir::new("index-availability");
        let root = scratch.path().join("store");
        let engine = held(&root);
        let path = object(&engine, &root);
        if corrupt {
            std::fs::write(&path, vec![b'x'; MESSAGE.len()]).unwrap();
        } else {
            std::fs::remove_file(&path).unwrap();
        }
        pause(&engine, UserPause::Resume);
        drained(&engine);
        let state: String = engine
            .resources()
            .read(|tx| {
                tx.query_row("SELECT object_state FROM acquisitions", [], |row| {
                    row.get(0)
                })
                .map_err(|error| StoreError::internal(error.to_string()))
            })
            .unwrap();
        assert_eq!(state, if corrupt { "quarantined" } else { "missing" });
        assert_eq!(count(&engine, DOCS), 0);
        assert_eq!(count(&engine, "SELECT count(*) FROM acquisitions"), 1);
        assert_eq!(
            count(&engine, "SELECT count(*) FROM acquisition_sources"),
            1
        );
        assert!(status(&engine).io_pause().is_none());
        block_on(engine.shutdown()).unwrap();
    }
}

#[test]
fn a_real_republication_supersedes_a_captured_missing_read() {
    let scratch = ScratchDir::new("index-republication");
    let root = scratch.path().join("store");
    let engine = Arc::new(held(&root));
    std::fs::remove_file(object(&engine, &root)).unwrap();
    let once = Arc::new(AtomicUsize::new(0));
    let calls = once.clone();
    let weak = Arc::downgrade(&engine);
    let source = scratch.path().join("republished.eml");
    hook(&engine, move |event| {
        if matches!(event, Event::Prepared { .. }) && calls.fetch_add(1, Ordering::SeqCst) == 0 {
            import(&weak.upgrade().unwrap(), &source, MESSAGE);
        }
    });
    pause(&engine, UserPause::Resume);
    drained(&engine);
    assert_eq!(count(&engine, DOCS), 1);
    assert_eq!(
        count(
            &engine,
            "SELECT count(*) FROM acquisitions WHERE object_state='published'"
        ),
        1
    );
    assert!(status(&engine).io_pause().is_none());
    block_on(Arc::try_unwrap(engine).unwrap().shutdown()).unwrap();
}

#[test]
fn an_old_io_failure_cannot_pause_a_new_revision_or_epoch() {
    for update in STALE_CHANGES {
        let scratch = ScratchDir::new("index-stale-io");
        let root = scratch.path().join("store");
        let engine = held(&root);
        block_on(engine.shutdown()).unwrap();
        let fs = Arc::new(FaultFs::new(real_fs(), FsOp::Open, 1, FaultKind::NoSpace));
        let (engine, _) = clocked(&root, fs, 10_000);
        parked(&engine);
        let db = root.join("state.sqlite");
        let changed = AtomicUsize::new(0);
        let (sent, idle) = mpsc::channel();
        hook(&engine, move |event| {
            if matches!(event, Event::Prepared { .. })
                && changed.fetch_add(1, Ordering::SeqCst) == 0
            {
                external(&db, update);
            }
            if event == Event::Parked && changed.load(Ordering::SeqCst) > 0 {
                let _ = sent.send(());
            }
        });
        pause(&engine, UserPause::Resume);
        idle.recv_timeout(Duration::from_secs(30)).unwrap();
        assert_eq!(
            count(&engine, QUEUE),
            0,
            "a stale failure parked pending input"
        );
        drained(&engine);
        assert_eq!(count(&engine, DOCS), 1);
        assert_eq!(status(&engine).io_attempts(), 0);
        assert!(status(&engine).io_pause().is_none());
        block_on(engine.shutdown()).unwrap();
    }
}

#[test]
fn a_missing_lookup_is_a_terminal_read_error_not_an_io_retry() {
    let scratch = ScratchDir::new("index-invariant");
    let engine = block_on(Engine::open(scratch.path().join("store"))).unwrap();
    pause(&engine, UserPause::Pause);
    write(&engine, |tx| {
        tx.execute_batch("INSERT INTO d_index_queue VALUES(999,'new',1,1); UPDATE store_meta SET value='1' WHERE key='index_inputs'")
            .map_err(|error| StoreError::internal(error.to_string()))
    });
    pause(&engine, UserPause::Resume);
    wait(|| {
        matches!(
            engine.indexer().status(engine.resources()),
            Err(StoreError::NotFound { .. })
        )
    });
    assert_eq!(count(&engine, QUEUE), 1);
    assert_eq!(count(&engine, DOCS), 0);
    assert!(
        engine
            .resources()
            .read(|tx| sb_resources::search_index_status(tx))
            .unwrap()
            .io_pause()
            .is_none()
    );
    parked(&engine);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn reset_enqueue_drive_the_due_hold_and_a_newer_target_never_extracts() {
    let scratch = ScratchDir::new("index-phase-gate");
    let root = scratch.path().join("store");
    let engine = held(&root);
    write(&engine, |tx| {
        sb_resources::start_search_rebuild(tx, "job", 1)
    });
    write(&engine, |tx| {
        sb_resources::pause_for_io(
            tx,
            sb_resources::SearchIoCode::OtherIo,
            "local fixture",
            Timestamp::from_millis(1),
            2,
        )
    });
    block_on(engine.shutdown()).unwrap();
    let fs = Arc::new(FaultFs::new(real_fs(), FsOp::Open, 1, FaultKind::NoSpace));
    let (engine, _) = clocked(&root, fs.clone(), 10_000);
    pause(&engine, UserPause::Resume);
    parked(&engine);
    // The due hold began its retry, reset and enqueue committed
    // without object I/O (successful work reset the history), then
    // the index phase's one failed object open recorded the native
    // cause as a fresh pause.
    let state = status(&engine);
    let held_io = state.io_pause().unwrap();
    assert_eq!(held_io.code(), sb_resources::SearchIoCode::StorageFull);
    assert_eq!(held_io.retry_at().as_millis(), 11_000);
    assert_eq!(state.io_attempts(), 1);
    assert_eq!(fs.calls(FsOp::Open), 1);
    assert_eq!((count(&engine, DOCS), count(&engine, QUEUE)), (0, 1));
    assert_eq!(
        write(&engine, |tx| {
            Ok(sb_resources::latest_rebuild(tx)?.unwrap().phase())
        }),
        sb_resources::RebuildPhase::Index
    );
    // A newer stored target is never extracted, driven or downgraded.
    external(
        &root.join("state.sqlite"),
        "UPDATE search_rebuild_jobs SET phase='complete'; UPDATE store_meta SET value='999' WHERE key='search_extractor_version'",
    );
    engine.indexer().wake();
    parked(&engine);
    assert_eq!(fs.calls(FsOp::Open), 1);
    assert!(status(&engine).io_pause().is_some());
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn failed_publication_and_failed_pause_persistence_gate_reads_until_recovery_commits() {
    for fail_read in [false, true] {
        let scratch = ScratchDir::new("index-writer");
        let root = scratch.path().join("store");
        let engine = held(&root);
        block_on(engine.shutdown()).unwrap();
        let fs = Arc::new(FaultFs::new(
            real_fs(),
            FsOp::Open,
            if fail_read { 1 } else { u64::MAX },
            FaultKind::NoSpace,
        ));
        let (engine, _) = clocked(&root, fs, 10_000);
        let engine = Arc::new(engine);
        let before = generation(&engine);
        let db = root.join("state.sqlite");
        let install = db.clone();
        let weak = Arc::downgrade(&engine);
        let once = AtomicUsize::new(0);
        let (notified, recovering) = mpsc::channel();
        let (sent, rejected) = mpsc::channel();
        hook(&engine, move |event| {
            if matches!(event, Event::Prepared { .. }) && once.fetch_add(1, Ordering::SeqCst) == 0 {
                external(
                    &install,
                    "CREATE TRIGGER reject_index_generation BEFORE UPDATE OF value ON store_meta WHEN OLD.key='search_generation' BEGIN SELECT RAISE(ABORT,'fixture transaction failure'); END",
                );
            }
            if event == Event::Recovered {
                let engine = weak.upgrade().unwrap();
                assert!(matches!(
                    engine.indexer().status(engine.resources()),
                    Err(StoreError::Unavailable { .. })
                ));
                assert!(generation(&engine) > before + 1);
                notified.send(()).unwrap();
            }
            if event == Event::RecoveryFailed {
                let _ = sent.send(());
            }
        });
        pause(&engine, UserPause::Resume);
        wait(|| {
            matches!(
                engine.indexer().status(engine.resources()),
                Err(StoreError::Unavailable { .. })
            )
        });
        let failed = snapshot(engine.resources());
        assert_eq!((failed.docs, failed.queue, failed.hits), (0, 1, 0));
        assert_eq!(failed.generation, before + 1, "only Resume committed");
        rejected.try_iter().for_each(drop);
        engine.indexer().wake();
        rejected.recv_timeout(Duration::from_secs(30)).unwrap();
        assert!(matches!(
            engine.indexer().status(engine.resources()),
            Err(StoreError::Unavailable { .. })
        ));
        assert_eq!(snapshot(engine.resources()), failed);
        external(&db, "DROP TRIGGER reject_index_generation");
        engine.indexer().wake();
        recovering.recv_timeout(Duration::from_secs(30)).unwrap();
        drained(&engine);
        assert_eq!(count(&engine, DOCS), 1);
        block_on(Arc::try_unwrap(engine).unwrap().shutdown()).unwrap();
    }
}

#[test]
fn a_valid_prefix_commits_before_its_failure_without_resetting_retry_history() {
    let scratch = ScratchDir::new("index-prefix");
    let root = scratch.path().join("store");
    let engine = held(&root);
    import(
        &engine,
        &scratch.path().join("second.eml"),
        b"Subject: second\n\nsecondneedle\n",
    );
    write(&engine, |tx| {
        sb_resources::pause_for_io(
            tx,
            sb_resources::SearchIoCode::OtherIo,
            "fixture",
            Timestamp::from_millis(10_000),
            5,
        )
    });
    block_on(engine.shutdown()).unwrap();
    let fs = Arc::new(FaultFs::new(real_fs(), FsOp::Open, 2, FaultKind::NoSpace));
    let (engine, _) = clocked(&root, fs, 10_000);
    pause(&engine, UserPause::Resume);
    wait(|| {
        status(&engine)
            .io_pause()
            .is_some_and(|pause| pause.code() == sb_resources::SearchIoCode::StorageFull)
    });
    assert_eq!(count(&engine, DOCS), 1);
    assert_eq!(count(&engine, QUEUE), 1);
    assert_eq!(status(&engine).io_attempts(), 6);
    assert_eq!(
        status(&engine).io_pause().unwrap().retry_at().as_millis(),
        42_000
    );
    retry(&engine);
    drained(&engine);
    assert_eq!(count(&engine, DOCS), 2);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn native_bulk_full_persists_pause_history_when_status_writes_still_fit() {
    let scratch = ScratchDir::new("index-native-full");
    let root = scratch.path().join("store");
    let message = format!("Subject: bulk\n\n{}", "word ".repeat(100_000));
    let engine = held_message(&root, message.as_bytes());
    block_on(engine.shutdown()).unwrap();
    let (engine, clock) = clocked(&root, real_fs(), 10_000);
    parked(&engine);
    let (pages, bound) = write(&engine, |tx| {
        let pages: i64 = tx
            .query_row("PRAGMA page_count", [], |row| row.get(0))
            .map_err(|error| StoreError::internal(error.to_string()))?;
        let bound: i64 = tx
            .query_row(&format!("PRAGMA max_page_count={pages}"), [], |row| {
                row.get(0)
            })
            .map_err(|error| StoreError::internal(error.to_string()))?;
        Ok((pages, bound))
    });
    assert_eq!(pages, bound);
    pause(&engine, UserPause::Resume);
    wait(|| {
        engine
            .indexer()
            .status(engine.resources())
            .is_ok_and(|state| state.io_pause().is_some())
    });
    let state = status(&engine);
    assert_eq!(
        state.io_pause().unwrap().code(),
        sb_resources::SearchIoCode::StorageFull
    );
    assert_eq!(state.io_pause().unwrap().retry_at().as_millis(), 11_000);
    assert_eq!(state.io_attempts(), 1);
    assert_eq!((count(&engine, DOCS), count(&engine, QUEUE)), (0, 1));
    clock.0.store(11_000, Ordering::SeqCst);
    engine.indexer().wake();
    wait(|| {
        engine
            .indexer()
            .status(engine.resources())
            .is_ok_and(|state| state.io_attempts() == 2)
    });
    assert_eq!(
        status(&engine).io_pause().unwrap().retry_at().as_millis(),
        13_000
    );
    assert_eq!((count(&engine, DOCS), count(&engine, QUEUE)), (0, 1));
    write(&engine, |tx| {
        tx.query_row("PRAGMA max_page_count=1000000", [], |row| {
            row.get::<_, i64>(0)
        })
        .map_err(|error| StoreError::internal(error.to_string()))
    });
    retry(&engine);
    drained(&engine);
    assert_eq!(count(&engine, DOCS), 1);
    assert!(status(&engine).io_pause().is_none());
    block_on(engine.shutdown()).unwrap();
}

#[derive(Clone, Copy)]
enum End {
    Shutdown,
    Drop,
}

fn close_and_reopen(engine: Engine, root: &Path, end: End) -> Resources {
    let (sent, received) = mpsc::channel();
    let root = root.to_owned();
    let thread = std::thread::spawn(move || {
        match end {
            End::Shutdown => block_on(engine.shutdown()).unwrap(),
            End::Drop => drop(engine),
        }
        let mut opened = None;
        wait(|| match Resources::open(&root) {
            Ok(resources) => {
                opened = Some(resources);
                true
            }
            Err(StoreError::Locked) => false,
            Err(error) => panic!("reopen after stop: {error}"),
        });
        sent.send(opened.unwrap()).unwrap();
    });
    let resources = received.recv_timeout(Duration::from_secs(300)).unwrap();
    thread.join().unwrap();
    resources
}

#[test]
fn shutdown_and_drop_wake_a_parked_retry_without_waiting_for_its_deadline() {
    for end in [End::Shutdown, End::Drop] {
        let scratch = ScratchDir::new("index-stop-parked");
        let root = scratch.path().join("store");
        let engine = held(&root);
        // A persisted deadline can be far away after a wall-clock rollback.
        write(&engine, |tx| {
            sb_resources::pause_for_io(
                tx,
                sb_resources::SearchIoCode::OtherIo,
                "fixture",
                Timestamp::from_millis(3_610_000),
                7,
            )
        });
        block_on(engine.shutdown()).unwrap();
        let (engine, _) = clocked(&root, real_fs(), 10_000);
        pause(&engine, UserPause::Resume);
        parked(&engine);
        let resources = close_and_reopen(engine, &root, end);
        assert_eq!(snapshot(&resources).queue, 1);
        assert_eq!(snapshot(&resources).docs, 0);
        resources.close().unwrap();
    }
}

#[test]
fn shutdown_and_drop_discard_unsubmitted_preparation_and_release_every_handle() {
    for end in [End::Shutdown, End::Drop] {
        let scratch = ScratchDir::new("index-stop-prepared");
        let root = scratch.path().join("store");
        let engine = held(&root);
        let (ready, reached) = mpsc::channel();
        let (release, released) = mpsc::channel();
        let released = Mutex::new(released);
        hook(&engine, move |event| {
            if matches!(event, Event::Prepared { .. }) {
                ready.send(()).unwrap();
                released
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(300))
                    .unwrap();
            }
        });
        pause(&engine, UserPause::Resume);
        reached.recv_timeout(Duration::from_secs(300)).unwrap();
        let index = engine.indexer().clone();
        let owned = root.clone();
        let thread = std::thread::spawn(move || close_and_reopen(engine, &owned, end));
        wait(|| index.stopped());
        release.send(()).unwrap();
        let resources = thread.join().unwrap();
        assert_eq!(
            (snapshot(&resources).docs, snapshot(&resources).queue),
            (0, 1)
        );
        resources.close().unwrap();
        let engine = block_on(Engine::open(&root)).unwrap();
        drained(&engine);
        assert_eq!(count(&engine, DOCS), 1);
        block_on(engine.shutdown()).unwrap();
    }
}

const CUT_N: &str = "SB_INDEX_WORKER_CUT_N";
const CUT_ROOT: &str = "SB_INDEX_WORKER_CUT_ROOT";

#[test]
fn a_blocking_worker_panic_surfaces_an_error_and_preserves_unsubmitted_work() {
    let scratch = ScratchDir::new("index-task-failure");
    let engine = held(&scratch.path().join("store"));
    let (sent, reached) = mpsc::channel();
    hook(&engine, move |event| {
        if matches!(event, Event::Prepared { .. }) {
            sent.send(()).unwrap();
            panic!("injected blocking-task failure");
        }
    });
    pause(&engine, UserPause::Resume);
    reached.recv_timeout(Duration::from_secs(30)).unwrap();
    let deadline = Instant::now() + Duration::from_secs(5);
    while engine.indexer().status(engine.resources()).is_ok() {
        assert!(
            Instant::now() < deadline,
            "a dead worker left status readable"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!((count(&engine, DOCS), count(&engine, QUEUE)), (0, 1));
    block_on(engine.shutdown()).unwrap();
}

#[test]
#[ignore = "spawned by the process-crash acceptance test"]
fn worker_cut_child() {
    use sb_resources::Fs;
    let nth = std::env::var(CUT_N).unwrap().parse().unwrap();
    let root = std::path::PathBuf::from(std::env::var_os(CUT_ROOT).unwrap());
    let engine = block_on(Engine::open(&root)).unwrap();
    let cuts = Arc::new(sb_test_support::CutFs::new(real_fs(), nth));
    let observed = cuts.clone();
    let directory = root.clone();
    hook(&engine, move |event| {
        if matches!(event, Event::Prepared { .. } | Event::Committed) {
            observed.metadata(&directory).unwrap();
        }
    });
    pause(&engine, UserPause::Resume);
    drained(&engine);
    block_on(engine.shutdown()).unwrap();
    assert_eq!(cuts.operations(), 2);
}

#[test]
fn process_crashes_on_both_sides_of_worker_publication_converge_after_restart() {
    let scratch = ScratchDir::new("index-worker-cuts");
    let generations = std::cell::RefCell::new(std::collections::BTreeMap::new());
    let (mut saw_old, mut saw_new) = (false, false);
    let cuts = sb_test_support::run_crash_cuts(
        CUT_N,
        CUT_ROOT,
        "index_tests::worker_cut_child",
        &mut |n| {
            let root = scratch.path().join(format!("cut-{n}"));
            let engine = held(&root);
            generations
                .borrow_mut()
                .insert(root.clone(), generation(&engine));
            block_on(engine.shutdown()).unwrap();
            root
        },
        &mut |root| {
            let resources = Resources::open(root).unwrap();
            let state = snapshot(&resources);
            let baseline = generations.borrow()[root];
            let old = Snapshot {
                docs: 0,
                queue: 1,
                generation: baseline + 1,
                hits: 0,
                acquisitions: 1,
            };
            let new = Snapshot {
                docs: 1,
                queue: 0,
                generation: baseline + 2,
                hits: 1,
                acquisitions: 1,
            };
            assert!(state == old || state == new, "mixed publication: {state:?}");
            saw_old |= state == old;
            saw_new |= state == new;
            resources.close().unwrap();
            let engine = block_on(Engine::open(root)).unwrap();
            drained(&engine);
            assert_eq!(snapshot(engine.resources()), new);
            block_on(engine.shutdown()).unwrap();
        },
    );
    assert_eq!(cuts, 2);
    assert!(saw_old && saw_new);
}

/// Real-worker rebuild driver acceptance (spec/06 "Runtime rebuild
/// driver"): bounded phases, holds, reconciliation and crash cuts,
/// reusing this file's helpers so no private surface widens.
mod rebuild {
    use super::*;

    fn phase(engine: &Engine) -> String {
        engine
            .resources()
            .read(|tx| {
                tx.query_row(
                    "SELECT phase FROM search_rebuild_jobs ORDER BY epoch DESC LIMIT 1",
                    [],
                    |row| row.get(0),
                )
                .map_err(|error| StoreError::internal(error.to_string()))
            })
            .unwrap()
    }

    fn job(engine: &Engine) -> sb_resources::RebuildJob {
        engine
            .resources()
            .read(|tx| Ok(sb_resources::latest_rebuild(tx)?.unwrap()))
            .unwrap()
    }

    /// Every durable document field, state and mask plus the FTS hit
    /// set — the same-version identical-rebuild acceptance compares
    /// these, never job ids or generations.
    fn doc_rows(engine: &Engine) -> (Option<String>, Vec<i64>) {
        engine
            .resources()
            .read(|tx| {
                let fields: Option<String> = tx
                    .query_row(
                        "SELECT group_concat(doc_id||'|'||extractor_version||'|'||state||'|'||
                     truncated_fields||'|'||subject||'|'||from_text||'|'||to_text||'|'||cc_text||'|'||
                     body_authored||'|'||body_quoted||'|'||body_signature||'|'||attachment_names, ';')
                     FROM (SELECT * FROM d_search_docs ORDER BY doc_id)",
                        [],
                        |row| row.get(0),
                    )
                    .map_err(|error| StoreError::internal(error.to_string()))?;
                let hits = tx
                    .prepare(
                        "SELECT rowid FROM d_search_fts
                         WHERE d_search_fts MATCH 'indexneedle' ORDER BY rowid",
                    )
                    .map_err(|error| StoreError::internal(error.to_string()))?
                    .query_map([], |row| row.get(0))
                    .map_err(|error| StoreError::internal(error.to_string()))?
                    .collect::<Result<Vec<i64>, _>>()
                    .map_err(|error| StoreError::internal(error.to_string()))?;
                Ok((fields, hits))
            })
            .unwrap()
    }

    /// 201 acquisitions past both 200-row bounds rebuild to identical
    /// documents; a foreground User write lands between phase batches.
    #[test]
    fn a_bounded_rebuild_converges_identically_around_foreground_writes() {
        let scratch = ScratchDir::new("rebuild-bounded");
        let root = scratch.path().join("store");
        let engine = block_on(Engine::open(&root)).unwrap();
        pause(&engine, UserPause::Pause);
        let mut bytes = Vec::new();
        for n in 0..201 {
            bytes.extend_from_slice(
                format!("From s Sat Oct 17 12:00:00 2026\nSubject: {n}\n\nindexneedle {n}\n")
                    .as_bytes(),
            );
        }
        import(&engine, &scratch.path().join("many.mbox"), &bytes);
        pause(&engine, UserPause::Resume);
        drained(&engine);
        let before = doc_rows(&engine);
        wait(|| {
            !engine
                .resources()
                .read(sb_resources::thread_revisions)
                .unwrap()
                .stale()
        });
        assert_eq!(count(&engine, DOCS), 201);
        assert_eq!(before.1.len(), 201);
        // A real user hold makes the rebuild wait for its start.
        pause(&engine, UserPause::Pause);
        write(&engine, |tx| {
            sb_resources::start_search_rebuild(tx, "job", 1)
        });
        let before_gen = generation(&engine);
        let once = Arc::new(AtomicUsize::new(0));
        let calls = once.clone();
        // The first phase commit's barrier interleaves a real User
        // write before the next bounded batch.
        let gate = Arc::new(std::sync::Barrier::new(2));
        let hooked = gate.clone();
        hook(&engine, move |event| {
            if event == Event::Committed {
                let step = calls.fetch_add(1, Ordering::SeqCst);
                if matches!(step, 0 | 2 | 3) {
                    hooked.wait();
                }
            }
        });
        pause(&engine, UserPause::Resume);
        wait(|| once.load(Ordering::SeqCst) > 0);
        let checkpoint = job(&engine);
        assert_eq!(checkpoint.phase(), sb_resources::RebuildPhase::Reset);
        assert_eq!(
            (checkpoint.doc_cursor(), checkpoint.phase_count()),
            (200, 200)
        );
        assert_eq!((count(&engine, DOCS), count(&engine, QUEUE)), (1, 0));
        assert_eq!(
            generation(&engine),
            before_gen + 2,
            "Resume and reset committed"
        );
        write(&engine, |tx| {
            sb_resources::set_user_pause(tx, UserPause::Resume)
        });
        gate.wait();
        // First enqueue batch stops at id200. A real duplicate import
        // refreshes id1 behind that cursor while the worker is parked.
        wait(|| once.load(Ordering::SeqCst) >= 3);
        assert_eq!(job(&engine).acq_cursor(), 200);
        let token = engine
            .resources()
            .pending_index_batch()
            .unwrap()
            .documents
            .into_iter()
            .find(|pending| pending.doc_id == 1)
            .unwrap();
        let bytes = std::fs::read(object(&engine, &root)).unwrap();
        import(&engine, &scratch.path().join("duplicate.eml"), &bytes);
        let refreshed = engine
            .resources()
            .pending_index_batch()
            .unwrap()
            .documents
            .into_iter()
            .find(|pending| pending.doc_id == 1)
            .unwrap();
        assert!(refreshed.revision > token.revision);
        assert_eq!(refreshed.enqueued_at, token.enqueued_at);
        gate.wait();
        wait(|| once.load(Ordering::SeqCst) >= 4);
        assert_eq!(job(&engine).phase(), sb_resources::RebuildPhase::Index);
        let retained = engine
            .resources()
            .pending_index_batch()
            .unwrap()
            .documents
            .into_iter()
            .find(|pending| pending.doc_id == 1)
            .unwrap();
        assert_eq!(
            retained, refreshed,
            "enqueue lost a producer behind its cursor"
        );
        gate.wait();
        wait(|| phase(&engine) == "complete");
        assert!(generation(&engine) > before_gen);
        assert_eq!(doc_rows(&engine), before);
        assert_eq!(count(&engine, QUEUE), 0);
        block_on(engine.shutdown()).unwrap();
    }

    /// Startup resumes a stored uncancelled job at its committed phase
    /// and leaves a user-held one parked until the hold is released.
    #[test]
    fn startup_resumes_an_uncancelled_job_and_keeps_the_user_hold() {
        let scratch = ScratchDir::new("rebuild-resume");
        let root = scratch.path().join("store");
        let engine = held(&root);
        pause(&engine, UserPause::Resume);
        drained(&engine);
        pause(&engine, UserPause::Pause);
        write(&engine, |tx| {
            sb_resources::start_search_rebuild(tx, "job", 1)
        });
        block_on(engine.shutdown()).unwrap();
        // The held job stays parked across reopen: no auto-resume.
        let engine = block_on(Engine::open(&root)).unwrap();
        parked(&engine);
        let stored = job(&engine);
        assert_eq!(stored.id(), "job");
        assert_eq!(phase(&engine), "reset");
        assert!(status(&engine).user_paused());
        assert_eq!(count(&engine, DOCS), 1);
        pause(&engine, UserPause::Resume);
        wait(|| phase(&engine) == "complete");
        assert_eq!((count(&engine, DOCS), count(&engine, QUEUE)), (1, 0));
        block_on(engine.shutdown()).unwrap();
        // Plant an uncancelled checkpoint with no engine worker running.
        let resources = Resources::open(&root).unwrap();
        block_on(resources.write(WritePriority::User, |tx| {
            sb_resources::start_search_rebuild(tx, "uncancelled", 1)
        }))
        .unwrap();
        resources.close().unwrap();
        let engine = block_on(Engine::open(&root)).unwrap();
        wait(|| phase(&engine) == "complete");
        assert_eq!(job(&engine).id(), "uncancelled");
        assert_eq!((count(&engine, DOCS), count(&engine, QUEUE)), (1, 0));
        block_on(engine.shutdown()).unwrap();
    }

    /// The durable reconciliation's private available-version input:
    /// 1->2 creates and reuses jobs under holds while keeping
    /// completed ids; a same-or-newer target is never touched.
    #[test]
    fn version_reconciliation_creates_and_reuses_jobs_under_both_holds() {
        let scratch = ScratchDir::new("rebuild-versions");
        let root = scratch.path().join("store");
        let engine = block_on(Engine::open(&root)).unwrap();
        pause(&engine, UserPause::Pause);
        write(&engine, |tx| {
            sb_resources::pause_for_io(
                tx,
                sb_resources::SearchIoCode::OtherIo,
                "fixture",
                Timestamp::from_millis(9_000),
                3,
            )
        });
        // A completed predecessor keeps its id; the active job is
        // restarted at reset with the same id and a fresh epoch.
        write(&engine, |tx| {
            sb_resources::start_search_rebuild(tx, "old", 1)?;
            tx.execute(
                "UPDATE search_rebuild_jobs SET phase='complete' WHERE job_id='old'",
                [],
            )
            .map_err(|error| StoreError::internal(error.to_string()))?;
            sb_resources::start_search_rebuild(tx, "live", 1)?;
            Ok(())
        });
        let epoch = status(&engine).epoch();
        assert!(!write(&engine, |tx| sb_resources::reconcile_search_target(
            tx, "ignored", 1
        )));
        assert!(write(&engine, |tx| {
            sb_resources::reconcile_search_target(tx, "reused", 2)
        }));
        let stored = job(&engine);
        assert_eq!(stored.id(), "live");
        assert_eq!(stored.phase(), sb_resources::RebuildPhase::Reset);
        assert_eq!(stored.epoch(), epoch + 1);
        assert_eq!(stored.doc_cursor(), 0);
        let state = status(&engine);
        assert!(state.user_paused());
        assert_eq!(
            state.io_pause().unwrap().retry_at(),
            Timestamp::from_millis(9_000)
        );
        assert_eq!(state.io_attempts(), 3);
        assert_eq!(
            write(&engine, |tx| {
                Ok(sb_resources::rebuild_job(tx, "old")?.phase())
            }),
            sb_resources::RebuildPhase::Complete
        );
        assert_eq!(write(&engine, |tx| sb_resources::target_version(tx)), 2);
        for available in [1, 2] {
            assert!(!write(&engine, move |tx| {
                sb_resources::reconcile_search_target(tx, "ignored", available)
            }));
        }
        // The stored target now exceeds the build: the worker cannot
        // drive it and open's real reconciliation leaves it alone.
        block_on(engine.shutdown()).unwrap();
        let engine = block_on(Engine::open(&root)).unwrap();
        parked(&engine);
        assert_eq!(write(&engine, |tx| sb_resources::target_version(tx)), 2);
        assert_eq!(phase(&engine), "reset");
        assert!(status(&engine).user_paused());
        block_on(engine.shutdown()).unwrap();
    }

    /// The durable user hold is the cancellation boundary: after its
    /// receipt no data or phase write commits, and reopen keeps the
    /// same id, phase and cursors until the hold is released.
    #[test]
    fn a_user_hold_stops_all_later_progress_and_reopen_resumes_on_release() {
        let scratch = ScratchDir::new("rebuild-hold");
        let root = scratch.path().join("store");
        let engine = held(&root);
        pause(&engine, UserPause::Resume);
        drained(&engine);
        pause(&engine, UserPause::Pause);
        write(&engine, |tx| {
            sb_resources::start_search_rebuild(tx, "job", 1)
        });
        let commits = Arc::new(AtomicUsize::new(0));
        let counted = commits.clone();
        let gate = Arc::new(std::sync::Barrier::new(2));
        let hooked = gate.clone();
        hook(&engine, move |event| {
            if event == Event::Committed && counted.fetch_add(1, Ordering::SeqCst) == 0 {
                hooked.wait();
            }
        });
        pause(&engine, UserPause::Resume);
        wait(|| commits.load(Ordering::SeqCst) > 0);
        // The durable hold commits while the worker sits between
        // phase commits: ZERO data or phase writes may commit after.
        pause(&engine, UserPause::Pause);
        gate.wait();
        parked(&engine);
        std::thread::sleep(Duration::from_millis(50));
        assert_eq!(commits.load(Ordering::SeqCst), 1);
        let stored = job(&engine);
        let held = (
            stored.id().to_owned(),
            stored.phase(),
            stored.doc_cursor(),
            stored.acq_cursor(),
        );
        block_on(engine.shutdown()).unwrap();
        let engine = block_on(Engine::open(&root)).unwrap();
        parked(&engine);
        let stored = job(&engine);
        assert_eq!(
            (
                stored.id().to_owned(),
                stored.phase(),
                stored.doc_cursor(),
                stored.acq_cursor()
            ),
            held
        );
        pause(&engine, UserPause::Resume);
        wait(|| phase(&engine) == "complete");
        assert_eq!((count(&engine, DOCS), count(&engine, QUEUE)), (1, 0));
        block_on(engine.shutdown()).unwrap();
    }

    /// A producer enqueueing behind an empty capture is never stranded:
    /// the rejected completion recaptures, indexes it and completes.
    #[test]
    fn a_producer_behind_completion_is_recaptured_and_finishes() {
        let scratch = ScratchDir::new("rebuild-producer");
        let root = scratch.path().join("store");
        let engine = held(&root);
        pause(&engine, UserPause::Resume);
        drained(&engine);
        pause(&engine, UserPause::Pause);
        write(&engine, |tx| {
            sb_resources::start_search_rebuild(tx, "job", 1)
        });
        let engine = Arc::new(engine);
        let weak = Arc::downgrade(&engine);
        let source = scratch.path().join("late.eml");
        let once = AtomicUsize::new(0);
        let (sent, observed) = mpsc::channel();
        hook(&engine, move |event| {
            if event != Event::Captured {
                return;
            }
            // The empty Index capture is committed: this producer's
            // real import lands before the completion transaction.
            let engine = weak.upgrade().unwrap();
            if count(&engine, QUEUE) == 0
                && once
                    .compare_exchange(0, 1, Ordering::SeqCst, Ordering::SeqCst)
                    .is_ok()
            {
                // A legitimate due metadata retry retains history before
                // the captured completion is rejected by a new producer.
                let now = engine.resources().now();
                let fence = engine.resources().pending_index_batch().unwrap().fence;
                write(&engine, move |tx| {
                    sb_resources::pause_for_io(
                        tx,
                        sb_resources::SearchIoCode::OtherIo,
                        "fixture",
                        now,
                        5,
                    )
                });
                write(&engine, move |tx| {
                    sb_resources::begin_index_retry(tx, fence, now)
                });
                import(&engine, &source, b"Subject: late\n\nindexneedle\n");
            } else if once
                .compare_exchange(1, 2, Ordering::SeqCst, Ordering::SeqCst)
                .is_ok()
            {
                sent.send(status(&engine).io_attempts()).unwrap();
            }
        });
        pause(&engine, UserPause::Resume);
        wait(|| phase(&engine) == "complete");
        assert_eq!(
            observed.recv_timeout(Duration::from_secs(30)).unwrap(),
            5,
            "a rejected completion is not successful indexing"
        );
        assert_eq!((count(&engine, DOCS), count(&engine, QUEUE)), (2, 0));
        block_on(Arc::try_unwrap(engine).unwrap().shutdown()).unwrap();
    }

    /// A real generation failure on a phase write gates reads until
    /// the validated recovery commits; the phase then resumes.
    #[test]
    fn a_phase_generation_failure_gates_reads_and_recovers() {
        let scratch = ScratchDir::new("rebuild-gen-failure");
        let root = scratch.path().join("store");
        let engine = held(&root);
        pause(&engine, UserPause::Resume);
        drained(&engine);
        pause(&engine, UserPause::Pause);
        write(&engine, |tx| {
            sb_resources::start_search_rebuild(tx, "job", 1)
        });
        external(
            &root.join("state.sqlite"),
            "CREATE TRIGGER reject_rebuild_generation BEFORE UPDATE OF value ON store_meta WHEN OLD.key='search_generation' BEGIN SELECT RAISE(ABORT,'fixture transaction failure'); END",
        );
        // The release itself cannot commit a generation under the
        // trigger, so the hold is lifted outside the counted ops.
        external(
            &root.join("state.sqlite"),
            "UPDATE search_index_state SET user_paused=0",
        );
        engine.indexer().wake();
        wait(|| {
            matches!(
                engine.indexer().status(engine.resources()),
                Err(StoreError::Unavailable { .. })
            )
        });
        assert_eq!(phase(&engine), "reset");
        assert_eq!((count(&engine, DOCS), count(&engine, QUEUE)), (1, 0));
        external(
            &root.join("state.sqlite"),
            "DROP TRIGGER reject_rebuild_generation",
        );
        engine.indexer().wake();
        wait(|| phase(&engine) == "complete");
        assert_eq!((count(&engine, DOCS), count(&engine, QUEUE)), (1, 0));
        block_on(engine.shutdown()).unwrap();
    }

    #[test]
    fn native_enqueue_full_records_its_phase_fence_and_cannot_pause_a_new_epoch() {
        for supersede in [false, true] {
            let scratch = ScratchDir::new("rebuild-native-full");
            let root = scratch.path().join("store");
            let resources = Resources::open(&root).unwrap();
            block_on(resources.write(WritePriority::User, |tx| {
                tx.execute_batch(
                    "WITH RECURSIVE ids(id) AS (VALUES(1) UNION ALL SELECT id+1 FROM ids WHERE id<1001)
                     INSERT INTO acquisitions(acquisition_id,content_sha256,byte_length,object_state,first_acquired_at)
                     SELECT id,printf('%064x',id),1,'published',1 FROM ids;",
                ).map_err(|error| StoreError::internal(error.to_string()))?;
                sb_resources::start_search_rebuild(tx, "native-phase", 1)
            })).unwrap();
            let epoch = resources
                .read(|tx| Ok(sb_resources::search_index_status(tx)?.epoch()))
                .unwrap();
            block_on(resources.write(WritePriority::User, move |tx| {
                sb_resources::rebuild_reset_batch(tx, "native-phase", epoch, 200)
            }))
            .unwrap();
            block_on(resources.write(WritePriority::User, |tx| {
                sb_resources::set_user_pause(tx, UserPause::Pause)
            }))
            .unwrap();
            resources.close().unwrap();
            let (engine, _) = clocked(&root, real_fs(), 10_000);
            let engine = Arc::new(engine);
            parked(&engine);
            write(&engine, |tx| {
                let pages: i64 = tx
                    .query_row("PRAGMA page_count", [], |row| row.get(0))
                    .map_err(|error| StoreError::internal(error.to_string()))?;
                tx.query_row(&format!("PRAGMA max_page_count={pages}"), [], |row| {
                    row.get::<_, i64>(0)
                })
                .map_err(|error| StoreError::internal(error.to_string()))
            });
            let weak = Arc::downgrade(&engine);
            let once = AtomicUsize::new(0);
            let (sent, failed) = mpsc::channel();
            let (barrier, recovered) = mpsc::channel();
            hook(&engine, move |event| {
                if event == Event::Recovering && once.fetch_add(1, Ordering::SeqCst) == 0 {
                    let engine = weak.upgrade().unwrap();
                    sent.send((
                        phase(&engine),
                        job(&engine).acq_cursor(),
                        count(&engine, QUEUE),
                    ))
                    .unwrap();
                    if supersede {
                        // Isolate epoch fencing under the SAME supported
                        // target, rather than mask it with capability2.
                        write(&engine, |tx| {
                            tx.execute_batch(
                                "UPDATE search_index_state SET epoch=epoch+1;
                             UPDATE search_rebuild_jobs SET epoch=epoch+1;",
                            )
                            .map_err(|error| StoreError::internal(error.to_string()))
                        });
                    }
                }
                if supersede && event == Event::Recovered {
                    let engine = weak.upgrade().unwrap();
                    let state = engine
                        .resources()
                        .read(|tx| sb_resources::search_index_status(tx))
                        .unwrap();
                    barrier
                        .send((
                            state.epoch(),
                            state.io_pause().map(|pause| pause.code()),
                            state.io_attempts(),
                        ))
                        .unwrap();
                    write(&engine, |tx| {
                        sb_resources::set_user_pause(tx, UserPause::Pause)
                    });
                }
            });
            pause(&engine, UserPause::Resume);
            assert_eq!(
                failed.recv_timeout(Duration::from_secs(30)).unwrap(),
                ("enqueue".into(), 0, 0)
            );
            if supersede {
                assert_eq!(
                    recovered.recv_timeout(Duration::from_secs(30)).unwrap(),
                    (epoch + 1, None, 0)
                );
                wait(|| {
                    engine
                        .indexer()
                        .status(engine.resources())
                        .is_ok_and(|state| state.user_paused())
                });
                assert_eq!(job(&engine).id(), "native-phase");
                assert_eq!(job(&engine).phase(), sb_resources::RebuildPhase::Enqueue);
                assert_eq!(job(&engine).epoch(), epoch + 1);
                assert_eq!(
                    engine
                        .resources()
                        .read(|tx| sb_resources::target_version(tx))
                        .unwrap(),
                    1
                );
                assert!(status(&engine).io_pause().is_none());
                assert_eq!(status(&engine).io_attempts(), 0);
            } else {
                wait(|| {
                    engine
                        .indexer()
                        .status(engine.resources())
                        .is_ok_and(|state| state.io_pause().is_some())
                });
                let state = status(&engine);
                assert_eq!(
                    state.io_pause().unwrap().code(),
                    sb_resources::SearchIoCode::StorageFull
                );
                assert_eq!(state.io_pause().unwrap().retry_at().as_millis(), 11_000);
                assert_eq!(state.io_attempts(), 1);
                assert_eq!(phase(&engine), "enqueue");
                assert_eq!(job(&engine).acq_cursor(), 0);
            }
            assert_eq!((count(&engine, DOCS), count(&engine, QUEUE)), (0, 0));
            block_on(Arc::try_unwrap(engine).unwrap().shutdown()).unwrap();
        }
    }

    const REBUILD_CUT_N: &str = "SB_REBUILD_WORKER_CUT_N";
    const REBUILD_CUT_ROOT: &str = "SB_REBUILD_WORKER_CUT_ROOT";

    #[test]
    #[ignore = "spawned by the rebuild crash-cut acceptance test"]
    fn rebuild_cut_child() {
        use sb_resources::Fs;
        let nth = std::env::var(REBUILD_CUT_N).unwrap().parse().unwrap();
        let root = std::path::PathBuf::from(std::env::var_os(REBUILD_CUT_ROOT).unwrap());
        let engine = block_on(Engine::open(&root)).unwrap();
        let cuts = Arc::new(sb_test_support::CutFs::new(real_fs(), nth));
        // One counted operation before the first resume/phase commit,
        // so the sweep also sees the pre-drive durable state.
        cuts.metadata(&root).unwrap();
        let observed = cuts.clone();
        hook(&engine, move |event| {
            if matches!(event, Event::Captured | Event::Committed) {
                observed.metadata(&root).unwrap();
            }
        });
        pause(&engine, UserPause::Resume);
        wait(|| phase(&engine) == "complete");
        // The manual op, reset, enqueue, three index captures, the
        // publication and the completion: eight counted operations.
        wait(|| cuts.operations() == 8);
        block_on(engine.shutdown()).unwrap();
        assert_eq!(cuts.operations(), 8);
    }

    #[test]
    fn process_crashes_across_every_rebuild_phase_converge_after_restart() {
        let scratch = ScratchDir::new("rebuild-worker-cuts");
        let baselines = std::cell::RefCell::new(std::collections::BTreeMap::new());
        let (mut saw_old, mut saw_new) = (false, false);
        let cuts = sb_test_support::run_crash_cuts(
            REBUILD_CUT_N,
            REBUILD_CUT_ROOT,
            "index_tests::rebuild::rebuild_cut_child",
            &mut |n| {
                let root = scratch.path().join(format!("rebuild-cut-{n}"));
                let engine = held(&root);
                pause(&engine, UserPause::Resume);
                drained(&engine);
                pause(&engine, UserPause::Pause);
                write(&engine, |tx| {
                    sb_resources::start_search_rebuild(tx, "job", 1)
                });
                let inputs: i64 = engine
                    .resources()
                    .read(|tx| {
                        tx.query_row(
                    "SELECT CAST(value AS INTEGER) FROM store_meta WHERE key='index_inputs'",
                    [], |row| row.get(0),
                ).map_err(|error| StoreError::internal(error.to_string()))
                    })
                    .unwrap();
                baselines.borrow_mut().insert(
                    root.clone(),
                    (generation(&engine), job(&engine).epoch(), inputs),
                );
                block_on(engine.shutdown()).unwrap();
                root
            },
            &mut |root| {
                let resources = Resources::open(root).unwrap();
                let state = snapshot(&resources);
                let job_phase: String = resources
                    .read(|tx| {
                        tx.query_row("SELECT phase FROM search_rebuild_jobs", [], |row| {
                            row.get(0)
                        })
                        .map_err(|error| StoreError::internal(error.to_string()))
                    })
                    .unwrap();
                let (id, epoch, doc_cursor, acq_cursor, processed, inputs, held): (
                    String,
                    i64,
                    i64,
                    i64,
                    i64,
                    i64,
                    bool,
                ) = resources
                    .read(|tx| {
                        tx.query_row(
                            "SELECT job_id,epoch,doc_cursor,acq_cursor,phase_count,
                        (SELECT CAST(value AS INTEGER) FROM store_meta WHERE key='index_inputs'),
                        (SELECT user_paused FROM search_index_state) FROM search_rebuild_jobs",
                            [],
                            |row| {
                                Ok((
                                    row.get(0)?,
                                    row.get(1)?,
                                    row.get(2)?,
                                    row.get(3)?,
                                    row.get(4)?,
                                    row.get(5)?,
                                    row.get(6)?,
                                ))
                            },
                        )
                        .map_err(|error| StoreError::internal(error.to_string()))
                    })
                    .unwrap();
                assert_eq!(id, "job");
                assert_eq!(
                    resources
                        .read(|tx| sb_resources::target_version(tx))
                        .unwrap(),
                    1
                );
                let (base_gen, base_epoch, base_inputs) = baselines.borrow()[root];
                // Every kill lands between committed transactions:
                // the durable state is always one of the sequential
                // phase boundaries, never a torn mix.
                let valid = [
                    (
                        (1, 0, 1, 1),
                        ("reset", base_epoch, 0, 0, 0),
                        (base_gen, base_inputs, true),
                    ),
                    (
                        (0, 0, 0, 1),
                        ("enqueue", base_epoch, 1, 0, 0),
                        (base_gen + 2, base_inputs, false),
                    ),
                    (
                        (0, 1, 0, 1),
                        ("index", base_epoch, 1, 1, 0),
                        (base_gen + 3, base_inputs + 1, false),
                    ),
                    (
                        (1, 0, 1, 1),
                        ("index", base_epoch, 1, 1, 1),
                        (base_gen + 4, base_inputs + 1, false),
                    ),
                    (
                        (1, 0, 1, 1),
                        ("complete", base_epoch, 1, 1, 1),
                        (base_gen + 5, base_inputs + 1, false),
                    ),
                ];
                let observed = (
                    (state.docs, state.queue, state.hits, state.acquisitions),
                    (job_phase.as_str(), epoch, doc_cursor, acq_cursor, processed),
                    (state.generation, inputs, held),
                );
                assert!(
                    valid.contains(&observed),
                    "torn rebuild state: {observed:?}"
                );
                saw_old |= observed == valid[0];
                saw_new |= observed == *valid.last().unwrap();
                resources.close().unwrap();
                let engine = block_on(Engine::open(root)).unwrap();
                if status(&engine).user_paused() {
                    pause(&engine, UserPause::Resume);
                }
                wait(|| phase(&engine) == "complete");
                assert_eq!((count(&engine, DOCS), count(&engine, QUEUE)), (1, 0));
                assert_eq!(generation(&engine), base_gen + 5);
                block_on(engine.shutdown()).unwrap();
            },
        );
        assert_eq!(cuts, 8);
        assert!(saw_old && saw_new);
    }
}
