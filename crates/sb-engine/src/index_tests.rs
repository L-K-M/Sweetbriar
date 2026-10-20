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
                "SELECT content_sha256 FROM acquisitions LIMIT 1",
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
fn reset_and_newer_target_do_not_extract_or_clear_a_due_hold() {
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
    assert_eq!(status(&engine).io_attempts(), 2);
    assert!(status(&engine).io_pause().is_some());
    assert_eq!(fs.calls(FsOp::Open), 0);
    assert_eq!(count(&engine, QUEUE), 1);
    external(
        &root.join("state.sqlite"),
        "UPDATE search_rebuild_jobs SET phase='complete'; UPDATE store_meta SET value='999' WHERE key='search_extractor_version'",
    );
    engine.indexer().wake();
    parked(&engine);
    assert_eq!(fs.calls(FsOp::Open), 0);
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
