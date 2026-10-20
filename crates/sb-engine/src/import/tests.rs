//! Tests for engine import jobs (M1.2c3a, spec/09 "Job lifecycle"):
//! commands and the job query end to end over real stores, recovery at
//! open, shutdown pauses, cancellation and failures. The engine's own
//! runtime runs the workers; the tests poll `import_job`.

use std::fs;
use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::mpsc;
use std::task::{Context, Waker};
use std::time::{Duration, Instant};

use sb_test_support::{ScratchDir, block_on};

use super::*;
use crate::engine::Spawn;

mod acceptance;

/// How long a test waits for a worker before failing.
// Generous: ingest shares the disk with crash-cut children in CI.
const PATIENCE: Duration = Duration::from_secs(300);
const ENVELOPE: &str = "From sender@example.test Sat Oct 17 12:00:00 2026\n";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/import")
}

fn open(root: &Path) -> Engine {
    block_on(Engine::open(root)).expect("the engine opens")
}

/// `count` distinct messages; the first is unread by its status header.
fn mbox(count: usize) -> Vec<u8> {
    (0..count)
        .map(|n| {
            let status = if n == 0 { "Status: O\n" } else { "" };
            format!("{ENVELOPE}Subject: message {n}\n{status}\nbody {n}\n")
        })
        .collect::<String>()
        .into_bytes()
}

fn start(engine: &Engine, paths: &[PathBuf]) -> String {
    let key = engine.new_idempotency_key();
    block_on(engine.start_import(&key, paths.to_vec()))
        .unwrap()
        .job_id()
        .to_owned()
}

fn status(engine: &Engine, id: &str) -> ImportJobStatus {
    block_on(engine.import_job(id)).unwrap()
}

/// Poll until the job reaches `state` and no worker holds it.
fn settle(engine: &Engine, id: &str, state: ImportJobState) -> ImportJobStatus {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let current = status(engine, id);
        let live = engine.imports().lock().contains_key(id);
        if current.state() == state && !live {
            return current;
        }
        assert!(
            Instant::now() < deadline,
            "job {id} stayed {:?}, failure {:?}",
            current.state(),
            current.checkpoint().failure()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Wait until no worker holds `id`, whatever its state.
fn idle(engine: &Engine, id: &str) {
    let deadline = Instant::now() + PATIENCE;
    while engine.imports().lock().contains_key(id) {
        assert!(Instant::now() < deadline, "the worker for {id} never ended");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn confirm(
    engine: &Engine,
    id: &str,
    variants: Vec<(u64, MboxVariant)>,
) -> Result<Receipt, EngineError> {
    block_on(engine.confirm_import(&engine.new_idempotency_key(), id, variants))
}

/// Row counts straight from the store.
fn count(engine: &Engine, sql: &'static str) -> i64 {
    engine
        .resources()
        .read(|tx| {
            tx.query_row(sql, [], |row| row.get(0))
                .map_err(|e| StoreError::internal(e.to_string()))
        })
        .unwrap()
}

#[test]
fn an_import_runs_from_inventory_through_confirmation_to_completion() {
    let scratch = ScratchDir::new("import-complete");
    let engine = open(&scratch.path().join("store"));
    let missing = scratch.path().join("missing.mbox");
    let id = start(
        &engine,
        &[
            fixtures().join("Inbox.mbox"),
            fixtures().join("quoting.mbox"),
            missing,
        ],
    );

    // Nothing is ingested before confirmation (spec/09).
    let waiting = settle(&engine, &id, ImportJobState::AwaitingConfirmation);
    assert_eq!(waiting.sources().len(), 2);
    assert_eq!(waiting.checkpoint().ambiguous(), [1]);
    assert_eq!(waiting.checkpoint().problem_count(), 1);
    assert_eq!(count(&engine, "SELECT count(*) FROM occurrences"), 0);

    for wrong in [vec![], vec![(0, MboxVariant::Mboxo)]] {
        match confirm(&engine, &id, wrong) {
            Err(EngineError::InvalidInput { field, .. }) => assert_eq!(field, "variants"),
            other => panic!("a wrong choice must be InvalidInput: {other:?}"),
        }
    }
    let key = engine.new_idempotency_key();
    let receipt =
        block_on(engine.confirm_import(&key, &id, vec![(1, MboxVariant::Mboxo)])).unwrap();
    // A repeated key returns the stored receipt.
    let again = block_on(engine.confirm_import(&key, &id, vec![(1, MboxVariant::Mboxo)])).unwrap();
    assert_eq!(again.result_json(), receipt.result_json());

    let done = settle(&engine, &id, ImportJobState::Completed);
    let totals = done.checkpoint().totals();
    assert_eq!(
        (totals.processed, totals.imported, totals.duplicates),
        (3, 3, 1)
    );
    assert_eq!((totals.read_from_headers, totals.read_default), (1, 2));
    assert_eq!(done.checkpoint().variant(1), Some(MboxVariant::Mboxo));
    for source in done.sources() {
        let inventory = source.source();
        assert_eq!(
            source.position(),
            SourcePosition::new(
                inventory.snapshot().byte_length(),
                inventory.message_count()
            )
        );
    }
    assert_eq!(count(&engine, "SELECT count(*) FROM occurrences"), 3);
    assert_eq!(count(&engine, "SELECT count(*) FROM acquisitions"), 2);

    // A finished job cannot be confirmed or cancelled again.
    let error = block_on(engine.cancel_job(&engine.new_idempotency_key(), &id)).unwrap_err();
    assert!(matches!(error, EngineError::Conflict { .. }), "{error:?}");
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_repeated_start_key_returns_its_job_and_starts_nothing() {
    let scratch = ScratchDir::new("import-idempotent");
    let engine = open(&scratch.path().join("store"));
    let key = engine.new_idempotency_key();
    let paths = vec![fixtures().join("Inbox.mbox")];
    let first = block_on(engine.start_import(&key, paths.clone())).unwrap();
    let second = block_on(engine.start_import(&key, paths)).unwrap();
    assert_eq!(
        first.receipt().result_json(),
        second.receipt().result_json()
    );
    assert_eq!(first.job_id(), second.job_id());
    settle(
        &engine,
        first.job_id(),
        ImportJobState::AwaitingConfirmation,
    );
    assert_eq!(count(&engine, "SELECT count(*) FROM import_jobs"), 1);

    match block_on(engine.start_import(&engine.new_idempotency_key(), Vec::new())) {
        Err(EngineError::InvalidInput { field, .. }) => assert_eq!(field, "paths"),
        other => panic!("no paths must be InvalidInput: {other:?}"),
    }
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn cancelling_a_waiting_job_is_immediate_and_final() {
    let scratch = ScratchDir::new("import-cancel-waiting");
    let engine = open(&scratch.path().join("store"));
    let id = start(&engine, &[fixtures().join("Inbox.mbox")]);
    settle(&engine, &id, ImportJobState::AwaitingConfirmation);
    block_on(engine.cancel_job(&engine.new_idempotency_key(), &id)).unwrap();
    assert_eq!(status(&engine, &id).state(), ImportJobState::Cancelled);

    let error = confirm(&engine, &id, vec![]).unwrap_err();
    assert!(matches!(error, EngineError::Conflict { .. }), "{error:?}");
    let error =
        block_on(engine.cancel_job(&engine.new_idempotency_key(), "no-such-job")).unwrap_err();
    assert!(matches!(error, EngineError::NotFound { .. }), "{error:?}");
    assert_eq!(count(&engine, "SELECT count(*) FROM occurrences"), 0);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn cancelling_a_running_job_stops_it_durably() {
    let scratch = ScratchDir::new("import-cancel-running");
    let path = scratch.path().join("Big.mbox");
    fs::write(&path, mbox(3000)).unwrap();
    let root = scratch.path().join("store");
    let engine = open(&root);
    let id = start(&engine, &[path]);
    settle(&engine, &id, ImportJobState::AwaitingConfirmation);
    confirm(&engine, &id, vec![]).unwrap();
    // 3,000 durable publications outlast one write, so the cancel lands
    // mid-ingest; were the job ever faster, finishing first is fine too.
    let cancel = block_on(engine.cancel_job(&engine.new_idempotency_key(), &id));
    idle(&engine, &id);

    let ended = status(&engine, &id);
    match cancel {
        Ok(_) => assert_eq!(ended.state(), ImportJobState::Cancelled),
        Err(EngineError::Conflict { .. }) => assert_eq!(ended.state(), ImportJobState::Completed),
        Err(error) => panic!("{error:?}"),
    }
    // Only committed batches stay, and they match the checkpoint.
    let occurrences = count(&engine, "SELECT count(*) FROM occurrences");
    assert_eq!(occurrences as u64, ended.checkpoint().totals().imported);
    block_on(engine.shutdown()).unwrap();

    // Durable: the next open neither resumes nor changes it.
    let engine = open(&root);
    assert_eq!(status(&engine, &id).state(), ended.state());
    assert_eq!(
        count(&engine, "SELECT count(*) FROM occurrences"),
        occurrences
    );
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn shutdown_pauses_ingest_and_the_next_open_resumes_without_duplicates() {
    let scratch = ScratchDir::new("import-pause");
    let path = scratch.path().join("Big.mbox");
    fs::write(&path, mbox(260)).unwrap();
    let root = scratch.path().join("store");
    let engine = open(&root);
    let id = start(&engine, &[path]);
    settle(&engine, &id, ImportJobState::AwaitingConfirmation);
    confirm(&engine, &id, vec![]).unwrap();
    // Wherever ingest is, shutdown waits for it to pause or finish.
    block_on(engine.shutdown()).unwrap();

    let engine = open(&root);
    let done = settle(&engine, &id, ImportJobState::Completed);
    let totals = done.checkpoint().totals();
    assert_eq!((totals.processed, totals.imported), (260, 260));
    assert_eq!((totals.read_from_headers, totals.read_default), (1, 259));
    assert_eq!(count(&engine, "SELECT count(*) FROM occurrences"), 260);
    assert_eq!(
        count(&engine, "SELECT count(*) FROM acquisition_sources"),
        260
    );
    block_on(engine.shutdown()).unwrap();
}

/// Put a job straight into the store, bypassing the commands so no
/// worker starts: `state` and an optional first message committed.
fn plant(engine: &Engine, path: &Path, state: ImportJobState, committed: usize) -> String {
    let id = engine.resources().new_uuid_string();
    let outcome = engine
        .resources()
        .inventory_import_sources(&[path.to_path_buf()], |_| ControlFlow::Continue(()));
    let ImportInventoryOutcome::Complete(inventory) = outcome else {
        panic!("the inventory was cancelled");
    };
    let sources = inventory.sources().to_vec();
    let mut acquisition = engine
        .resources()
        .open_import_acquisition(&sources[0], None, SourcePosition::START)
        .unwrap();
    let acquired: Vec<_> = (0..committed)
        .map(|_| {
            engine
                .resources()
                .acquire_import_message(&mut acquisition)
                .unwrap()
                .unwrap()
        })
        .collect();
    let now = engine.resources().now();
    let (job, paths) = (id.clone(), vec![path.to_path_buf()]);
    block_on(engine.resources().write(WritePriority::Sync, move |tx| {
        create_import_job(tx, &job, &paths, now)?;
        // An inventory records its sources before it finishes.
        record_import_sources(tx, &job, 0, &sources)?;
        if state == ImportJobState::Inventory {
            return Ok(());
        }
        let checkpoint = ImportJobCheckpoint::inventoried(&sources, &[]);
        let waiting = ImportJobState::AwaitingConfirmation;
        let mut record = advance_import_job(tx, &job, 1, waiting, &checkpoint, now)?;
        if state == ImportJobState::Running {
            let running = ImportJobState::Running;
            record = advance_import_job(tx, &job, 2, running, record.checkpoint(), now)?;
        }
        let Some(last) = acquired.last() else {
            return Ok(());
        };
        let position = last.next_position();
        let messages: Vec<_> = acquired
            .into_iter()
            .map(AcquiredMessage::into_message)
            .collect();
        let batch = ImportJobBatch {
            job_id: &job,
            revision: record.revision(),
            ordinal: 0,
            mailbox: sources[0].mailbox(),
            messages: &messages,
            position,
            flags_incomplete: 0,
        };
        commit_import_job_batch(tx, &batch, now).map(drop)
    }))
    .unwrap();
    id
}

#[test]
fn open_fails_interrupted_inventories_keeps_waiting_jobs_and_resumes_running_ones() {
    let scratch = ScratchDir::new("import-recovery");
    let path = scratch.path().join("Three.mbox");
    fs::write(&path, mbox(3)).unwrap();
    let root = scratch.path().join("store");
    let engine = open(&root);
    let interrupted = plant(&engine, &path, ImportJobState::Inventory, 0);
    let waiting = plant(&engine, &path, ImportJobState::AwaitingConfirmation, 0);
    let running = plant(&engine, &path, ImportJobState::Running, 1);
    block_on(engine.shutdown()).unwrap();

    let engine = open(&root);
    let failed = status(&engine, &interrupted);
    assert_eq!(failed.state(), ImportJobState::Failed);
    assert_eq!(failed.checkpoint().failure(), Some(INTERRUPTED_INVENTORY));
    assert_eq!(
        status(&engine, &waiting).state(),
        ImportJobState::AwaitingConfirmation
    );
    // The resumed job continues after its committed message.
    let done = settle(&engine, &running, ImportJobState::Completed);
    assert_eq!(done.checkpoint().totals().processed, 3);
    assert_eq!(count(&engine, "SELECT count(*) FROM occurrences"), 3);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_missing_snapshot_fails_the_job_with_its_checkpoints_intact() {
    let scratch = ScratchDir::new("import-missing-snapshot");
    let path = scratch.path().join("Three.mbox");
    fs::write(&path, mbox(3)).unwrap();
    let root = scratch.path().join("store");
    let engine = open(&root);
    let id = start(&engine, &[path]);
    let waiting = settle(&engine, &id, ImportJobState::AwaitingConfirmation);
    let token = waiting.sources()[0].source().snapshot().token().to_owned();
    fs::remove_file(root.join("recovery").join(format!("import-source-{token}"))).unwrap();
    confirm(&engine, &id, vec![]).unwrap();

    let failed = settle(&engine, &id, ImportJobState::Failed);
    let failure = failed.checkpoint().failure().unwrap();
    assert!(failure.contains(&token), "{failure}");
    assert_eq!(failed.sources()[0].position(), SourcePosition::START);
    assert_eq!(count(&engine, "SELECT count(*) FROM occurrences"), 0);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_confirmation_read_before_the_inventory_finished_is_refused() {
    let scratch = ScratchDir::new("import-stale-confirm");
    let path = scratch.path().join("Three.mbox");
    fs::write(&path, mbox(3)).unwrap();
    let engine = open(&scratch.path().join("store"));
    let id = plant(&engine, &path, ImportJobState::Inventory, 0);
    // The confirmation read the job while it was still in inventory: it
    // validated nothing, so it expects nothing.
    let in_inventory = status(&engine, &id);
    assert_eq!(in_inventory.state(), ImportJobState::Inventory);

    // The inventory then finishes before the confirmation's write.
    let (job, now) = (id.clone(), engine.resources().now());
    let waiting = block_on(engine.resources().write(WritePriority::User, move |tx| {
        let current = import_job(tx, &job)?;
        let (source, _) = import_job_source(tx, &job, 0)?;
        let checkpoint = ImportJobCheckpoint::inventoried(&[source], &[]);
        let state = ImportJobState::AwaitingConfirmation;
        advance_import_job(tx, &job, current.revision(), state, &checkpoint, now)
    }))
    .unwrap();

    // A write expecting nothing, or an older revision, conflicts; only
    // the revision actually validated is accepted.
    let stale = [
        None,
        Some((waiting.revision() - 1, waiting.checkpoint().clone())),
    ];
    for expected in stale {
        let (job, key) = (id.clone(), engine.new_idempotency_key());
        let error = block_on(engine.resources().write(WritePriority::User, move |tx| {
            confirm_in(tx, &key, &job, expected, now)
        }))
        .unwrap_err();
        assert!(matches!(error, StoreError::Conflict { .. }), "{error:?}");
    }
    let job = status(&engine, &id);
    assert_eq!(job.state(), ImportJobState::AwaitingConfirmation);
    assert_eq!(job.sources().len(), 1);

    let (job, key) = (id.clone(), engine.new_idempotency_key());
    let expected = Some((waiting.revision(), waiting.checkpoint().clone()));
    block_on(engine.resources().write(WritePriority::User, move |tx| {
        confirm_in(tx, &key, &job, expected, now)
    }))
    .unwrap();
    assert_eq!(status(&engine, &id).state(), ImportJobState::Running);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_confirmation_during_the_inventory_handoff_still_ingests() {
    let scratch = ScratchDir::new("import-handoff");
    let path = scratch.path().join("Three.mbox");
    fs::write(&path, mbox(3)).unwrap();
    let engine = open(&scratch.path().join("store"));
    let id = plant(&engine, &path, ImportJobState::AwaitingConfirmation, 0);
    // The inventory worker has recorded the inventory but not let go:
    // the confirmation's ingest is handed to it, not dropped.
    let paths = vec![path.clone()];
    let mut inventory = Claim::new(engine.imports(), &id, &Task::Inventory(paths)).unwrap();
    confirm(&engine, &id, vec![]).unwrap();
    let Some(next) = inventory.next_or_release() else {
        panic!("the confirmation's ingest was dropped");
    };
    engine
        .spawn_worker(move |worker| run_job(worker, inventory, next))
        .unwrap();
    settle(&engine, &id, ImportJobState::Completed);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_reimport_with_the_other_variant_fails_visibly() {
    let scratch = ScratchDir::new("import-variant-conflict");
    let engine = open(&scratch.path().join("store"));
    let quoting = fixtures().join("quoting.mbox");
    let first = start(&engine, std::slice::from_ref(&quoting));
    settle(&engine, &first, ImportJobState::AwaitingConfirmation);
    confirm(&engine, &first, vec![(0, MboxVariant::Mboxo)]).unwrap();
    settle(&engine, &first, ImportJobState::Completed);

    // Same source identity, different decoded bytes: a real conflict.
    let second = start(&engine, &[quoting]);
    settle(&engine, &second, ImportJobState::AwaitingConfirmation);
    confirm(&engine, &second, vec![(0, MboxVariant::Mboxrd)]).unwrap();
    let failed = settle(&engine, &second, ImportJobState::Failed);
    let failure = failed.checkpoint().failure().unwrap();
    assert!(failure.contains("different bytes"), "{failure}");
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_start_dropped_after_submission_is_still_scheduled() {
    let scratch = ScratchDir::new("import-dropped-start");
    let engine = open(&scratch.path().join("store"));
    let key = engine.new_idempotency_key();
    let paths = vec![fixtures().join("Inbox.mbox")];
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    {
        let mut started = Box::pin(engine.start_import(&key, paths.clone()));
        let _ = started.as_mut().poll(&mut cx);
    }
    // The command's write lands without its caller.
    let deadline = Instant::now() + PATIENCE;
    while count(&engine, "SELECT count(*) FROM command_receipts") == 0 {
        assert!(Instant::now() < deadline, "the start never committed");
        std::thread::sleep(Duration::from_millis(10));
    }
    let started = block_on(engine.start_import(&key, paths)).unwrap();
    settle(
        &engine,
        started.job_id(),
        ImportJobState::AwaitingConfirmation,
    );
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_damaged_job_row_does_not_stop_the_store_from_opening() {
    let scratch = ScratchDir::new("import-damaged-row");
    let path = scratch.path().join("Three.mbox");
    fs::write(&path, mbox(3)).unwrap();
    let root = scratch.path().join("store");
    let engine = open(&root);
    let damaged = plant(&engine, &path, ImportJobState::Running, 0);
    let waiting = plant(&engine, &path, ImportJobState::AwaitingConfirmation, 0);
    let job = damaged.clone();
    block_on(engine.resources().write(WritePriority::Sync, move |tx| {
        tx.execute(
            "UPDATE import_jobs SET checkpoint_json = '{\"v\":1,\"next\":9}' WHERE job_id = ?1",
            [&job],
        )
        .map(drop)
        .map_err(|e| StoreError::internal(e.to_string()))
    }))
    .unwrap();
    block_on(engine.shutdown()).unwrap();

    // The damaged job stays as stored, as evidence; the rest still works.
    let engine = open(&root);
    let error = block_on(engine.import_job(&damaged)).unwrap_err();
    assert!(matches!(error, EngineError::Corrupt { .. }), "{error:?}");
    assert_eq!(
        status(&engine, &waiting).state(),
        ImportJobState::AwaitingConfirmation
    );
    block_on(engine.shutdown()).unwrap();
}

/// The private snapshots `recovery/` holds.
fn snapshots(root: &Path) -> usize {
    fs::read_dir(root.join("recovery"))
        .unwrap()
        .filter(|entry| {
            entry
                .as_ref()
                .unwrap()
                .file_name()
                .to_string_lossy()
                .starts_with("import-source-")
        })
        .count()
}

fn report(engine: &Engine, id: &str) -> String {
    let job = status(engine, id);
    let path = job.report_path().expect("a finished job names its report");
    fs::read_to_string(path).unwrap()
}

#[test]
fn a_finished_job_leaves_a_report_and_only_failures_keep_snapshots() {
    let scratch = ScratchDir::new("import-reports");
    let root = scratch.path().join("store");
    let engine = open(&root);

    let completed = start(&engine, &[fixtures().join("Inbox.mbox")]);
    settle(&engine, &completed, ImportJobState::AwaitingConfirmation);
    confirm(&engine, &completed, vec![]).unwrap();
    settle(&engine, &completed, ImportJobState::Completed);
    let text = report(&engine, &completed);
    assert!(text.contains("\"state\": \"completed\""), "{text}");
    assert!(text.contains("\"imported\": 2"), "{text}");
    assert_eq!(snapshots(&root), 0);

    let cancelled = start(&engine, &[fixtures().join("Inbox.mbox")]);
    settle(&engine, &cancelled, ImportJobState::AwaitingConfirmation);
    assert_eq!(snapshots(&root), 1);
    block_on(engine.cancel_job(&engine.new_idempotency_key(), &cancelled)).unwrap();
    assert!(report(&engine, &cancelled).contains("\"state\": \"cancelled\""));
    assert_eq!(snapshots(&root), 0);

    // A failure keeps the other snapshots as evidence.
    let two = scratch.path().join("Two.mbox");
    fs::write(&two, mbox(2)).unwrap();
    let failed = start(&engine, &[fixtures().join("Inbox.mbox"), two]);
    let waiting = settle(&engine, &failed, ImportJobState::AwaitingConfirmation);
    let token = waiting.sources()[1].source().snapshot().token().to_owned();
    fs::remove_file(root.join("recovery").join(format!("import-source-{token}"))).unwrap();
    confirm(&engine, &failed, vec![]).unwrap();
    settle(&engine, &failed, ImportJobState::Failed);
    let text = report(&engine, &failed);
    assert!(
        text.contains("\"state\": \"failed\"") && text.contains(&token),
        "{text}"
    );
    assert_eq!(snapshots(&root), 1);
    block_on(engine.shutdown()).unwrap();
}

/// `import_report` (spec/07): an unfinished job is `Conflict`, a
/// finished one returns its published report at the path `import_job`
/// names, and a finished job whose report isn't written yet — as a
/// crash right after the transition leaves it — is `None` there and
/// names no path in `import_job`. An unknown job is `NotFound`.
#[test]
fn import_report_returns_only_a_published_report() {
    let scratch = ScratchDir::new("import-report-query");
    let root = scratch.path().join("store");
    let engine = open(&root);

    let id = start(&engine, &[fixtures().join("Inbox.mbox")]);
    settle(&engine, &id, ImportJobState::AwaitingConfirmation);
    let unfinished = block_on(engine.import_report(&id)).unwrap_err();
    assert!(
        matches!(unfinished, EngineError::Conflict { .. }),
        "{unfinished:?}"
    );

    confirm(&engine, &id, vec![]).unwrap();
    let done = settle(&engine, &id, ImportJobState::Completed);
    let report = block_on(engine.import_report(&id))
        .unwrap()
        .expect("a finished job's report is published");
    assert_eq!(Some(report.path()), done.report_path());
    assert_eq!(report.json(), fs::read_to_string(report.path()).unwrap());
    assert!(report.json().contains("\"state\": \"completed\""));

    let pending = plant(
        &engine,
        &fixtures().join("Inbox.mbox"),
        ImportJobState::AwaitingConfirmation,
        0,
    );
    let (job, now) = (pending.clone(), engine.resources().now());
    block_on(engine.resources().write(WritePriority::Sync, move |tx| {
        let current = import_job(tx, &job)?;
        let state = ImportJobState::Cancelled;
        advance_import_job(
            tx,
            &job,
            current.revision(),
            state,
            current.checkpoint(),
            now,
        )
        .map(drop)
    }))
    .unwrap();
    assert_eq!(status(&engine, &pending).report_path(), None);
    assert_eq!(block_on(engine.import_report(&pending)).unwrap(), None);

    let unknown = block_on(engine.import_report("00000000-0000-4000-8000-00000000dead"));
    assert!(
        matches!(unknown, Err(EngineError::NotFound { .. })),
        "{unknown:?}"
    );
    block_on(engine.shutdown()).unwrap();
}

/// A caller dropping `cancel_job` while its cleanup runs still leaves
/// the report it wrote named by `import_job`: the record is kept inside
/// the cleanup, not after the caller's await.
#[test]
fn a_dropped_cancel_still_names_its_report() {
    let scratch = ScratchDir::new("import-dropped-cancel");
    let root = scratch.path().join("store");
    let engine = open(&root);
    let id = start(&engine, &[fixtures().join("Inbox.mbox")]);
    settle(&engine, &id, ImportJobState::AwaitingConfirmation);
    let written = root
        .join("recovery")
        .join(format!("import-report-{id}.json"));

    let key = engine.new_idempotency_key();
    {
        let mut cancelling = pin!(engine.cancel_job(&key, &id));
        let mut cx = Context::from_waker(Waker::noop());
        let deadline = Instant::now() + PATIENCE;
        // Poll until the cleanup has written the report, then stop: the
        // future is dropped, at this block's end, without the poll that
        // would see it finish.
        while !written.exists() {
            if cancelling.as_mut().poll(&mut cx).is_ready() {
                break;
            }
            assert!(
                Instant::now() < deadline,
                "the cleanup never wrote the report"
            );
            std::thread::sleep(Duration::from_millis(1));
        }
    }

    let deadline = Instant::now() + PATIENCE;
    while status(&engine, &id).report_path().is_none() {
        assert!(
            Instant::now() < deadline,
            "the written report stayed unnamed"
        );
        std::thread::sleep(Duration::from_millis(10));
    }
    assert_eq!(status(&engine, &id).report_path(), Some(written.as_path()));
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn open_redoes_cleanup_for_a_finished_job_without_a_report() {
    let scratch = ScratchDir::new("import-report-sweep");
    let path = scratch.path().join("Three.mbox");
    fs::write(&path, mbox(3)).unwrap();
    let root = scratch.path().join("store");
    let engine = open(&root);
    // Cancelled without its cleanup, as a crash right after the
    // transition leaves it.
    let id = plant(&engine, &path, ImportJobState::AwaitingConfirmation, 0);
    let (job, now) = (id.clone(), engine.resources().now());
    block_on(engine.resources().write(WritePriority::Sync, move |tx| {
        let current = import_job(tx, &job)?;
        let state = ImportJobState::Cancelled;
        advance_import_job(
            tx,
            &job,
            current.revision(),
            state,
            current.checkpoint(),
            now,
        )
        .map(drop)
    }))
    .unwrap();
    assert_eq!(snapshots(&root), 1);
    block_on(engine.shutdown()).unwrap();

    let engine = open(&root);
    assert!(report(&engine, &id).contains("\"state\": \"cancelled\""));
    assert_eq!(snapshots(&root), 0);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_durable_cancel_discards_an_inventory_without_its_flag() {
    let scratch = ScratchDir::new("import-durable-cancel");
    let path = scratch.path().join("Three.mbox");
    fs::write(&path, mbox(3)).unwrap();
    let root = scratch.path().join("store");
    let engine = open(&root);
    let id = plant(&engine, &path, ImportJobState::Inventory, 0);
    // Cancelled durably, but no worker heard of it.
    let (job, now) = (id.clone(), engine.resources().now());
    block_on(engine.resources().write(WritePriority::Sync, move |tx| {
        let current = import_job(tx, &job)?;
        let state = ImportJobState::Cancelled;
        advance_import_job(
            tx,
            &job,
            current.revision(),
            state,
            current.checkpoint(),
            now,
        )
        .map(drop)
    }))
    .unwrap();
    // An inventory worker still running finds the job gone at record.
    spawn_job(
        &engine,
        engine.imports(),
        id.clone(),
        Task::Inventory(vec![path]),
    )
    .unwrap();
    idle(&engine, &id);
    assert_eq!(snapshots(&root), 0);
    assert!(report(&engine, &id).contains("\"state\": \"cancelled\""));
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn reports_and_cleanup_cover_rows_recorded_before_the_inventory_finished() {
    let scratch = ScratchDir::new("import-partial-rows");
    let path = scratch.path().join("Three.mbox");
    fs::write(&path, mbox(3)).unwrap();
    let root = scratch.path().join("store");
    let engine = open(&root);
    // Rows recorded, final inventory checkpoint never written.
    let interrupted = plant(&engine, &path, ImportJobState::Inventory, 0);
    let cancelled = plant(&engine, &path, ImportJobState::Inventory, 0);
    let token = status(&engine, &interrupted).sources().len();
    assert_eq!(token, 0, "the query lists sources only once inventoried");
    block_on(engine.cancel_job(&engine.new_idempotency_key(), &cancelled)).unwrap();
    assert!(report(&engine, &cancelled).contains("Three.mbox"));
    // The cancelled job's snapshot goes; the interrupted one's stays.
    assert_eq!(snapshots(&root), 1);
    block_on(engine.shutdown()).unwrap();

    let engine = open(&root);
    let text = report(&engine, &interrupted);
    assert!(
        text.contains("\"state\": \"failed\"") && text.contains("Three.mbox"),
        "{text}"
    );
    assert_eq!(
        snapshots(&root),
        1,
        "a failed job keeps its snapshot as evidence"
    );
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn an_unreadable_recovery_directory_does_not_keep_the_store_closed() {
    use std::os::unix::fs::PermissionsExt;

    let scratch = ScratchDir::new("import-recovery-unreadable");
    let path = scratch.path().join("Three.mbox");
    fs::write(&path, mbox(3)).unwrap();
    let root = scratch.path().join("store");
    let engine = open(&root);
    // Cancelled without its cleanup, as a crash after the transition
    // leaves it: the next open owes it a report.
    let id = plant(&engine, &path, ImportJobState::AwaitingConfirmation, 0);
    let (job, now) = (id.clone(), engine.resources().now());
    block_on(engine.resources().write(WritePriority::Sync, move |tx| {
        let current = import_job(tx, &job)?;
        let state = ImportJobState::Cancelled;
        advance_import_job(
            tx,
            &job,
            current.revision(),
            state,
            current.checkpoint(),
            now,
        )
        .map(drop)
    }))
    .unwrap();
    block_on(engine.shutdown()).unwrap();

    // Nothing inside `recovery/` can be looked up.
    let recovery = root.join("recovery");
    fs::set_permissions(&recovery, fs::Permissions::from_mode(0o600)).unwrap();
    let opened = block_on(Engine::open(&root));
    fs::set_permissions(&recovery, fs::Permissions::from_mode(0o700)).unwrap();
    let engine = opened.expect("cleanup trouble must not keep the store closed");
    block_on(engine.shutdown()).unwrap();

    // Once readable again, the next open finishes the cleanup.
    let engine = open(&root);
    assert!(report(&engine, &id).contains("\"state\": \"cancelled\""));
    assert_eq!(snapshots(&root), 0);
    block_on(engine.shutdown()).unwrap();
}

/// Receipt-owned cancellation (spec/07 "Engine command boundary"): a
/// job cancelled under `key`, then `key` replayed naming a DIFFERENT
/// job must still answer the stored receipt while every post-commit
/// effect — the worker hint and the report cleanup — targets only the
/// receipt's own job. `job` is a live import the replay must not
/// reach.
fn replayed_cancel_on_live_import(engine: &Engine, job: &Path, key: &str) -> (String, Receipt) {
    let a = start(engine, &[fixtures().join("Inbox.mbox")]);
    settle(engine, &a, ImportJobState::AwaitingConfirmation);
    let receipt = block_on(engine.cancel_job(key, &a)).unwrap();

    let (entered, reached) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let released = Mutex::new(released);
    *engine
        .imports()
        .before_task
        .lock()
        .expect("a panicking thread held the import job registry") = Some(Arc::new(move || {
        entered.send(()).unwrap();
        released.lock().unwrap().recv().unwrap();
    }));
    let b = start(engine, &[job.to_path_buf()]);
    reached.recv_timeout(PATIENCE).unwrap();
    let replayed = block_on(engine.cancel_job(key, &b)).unwrap();
    release.send(()).unwrap();
    *engine
        .imports()
        .before_task
        .lock()
        .expect("a panicking thread held the import job registry") = None;
    assert_eq!(replayed.result_json(), receipt.result_json());
    idle(engine, &b);
    (b, replayed)
}

#[test]
fn a_replayed_cancel_key_cannot_stop_another_live_inventory() {
    let scratch = ScratchDir::new("import-replay-inventory");
    let job = scratch.path().join("Two.mbox");
    fs::write(&job, mbox(2)).unwrap();
    let engine = open(&scratch.path().join("store"));
    let key = engine.new_idempotency_key();
    let (b, _) = replayed_cancel_on_live_import(&engine, &job, &key);
    // Stranded in `inventory` here is the replay bug: B's worker was
    // stopped by an effect its key never authorized.
    assert_eq!(
        status(&engine, &b).state(),
        ImportJobState::AwaitingConfirmation
    );
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_replayed_cancel_key_cannot_stop_another_live_ingest() {
    let scratch = ScratchDir::new("import-replay-ingest");
    let job = scratch.path().join("Two.mbox");
    fs::write(&job, mbox(2)).unwrap();
    let engine = open(&scratch.path().join("store"));
    let a = start(&engine, &[fixtures().join("Inbox.mbox")]);
    settle(&engine, &a, ImportJobState::AwaitingConfirmation);
    let key = engine.new_idempotency_key();
    let receipt = block_on(engine.cancel_job(&key, &a)).unwrap();

    // B waits for confirmation; its ingest worker blocks at task entry.
    let b = start(&engine, &[job]);
    settle(&engine, &b, ImportJobState::AwaitingConfirmation);
    let (entered, reached) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let released = Mutex::new(released);
    *engine
        .imports()
        .before_task
        .lock()
        .expect("a panicking thread held the import job registry") = Some(Arc::new(move || {
        entered.send(()).unwrap();
        released.lock().unwrap().recv().unwrap();
    }));
    confirm(&engine, &b, vec![]).unwrap();
    reached.recv_timeout(PATIENCE).unwrap();
    let replayed = block_on(engine.cancel_job(&key, &b)).unwrap();
    release.send(()).unwrap();
    *engine
        .imports()
        .before_task
        .lock()
        .expect("a panicking thread held the import job registry") = None;
    assert_eq!(replayed.result_json(), receipt.result_json());
    idle(&engine, &b);
    // Stranded in `running` here is the same bug on the ingest task.
    assert_eq!(status(&engine, &b).state(), ImportJobState::Completed);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_replayed_cancel_key_cannot_rewrite_another_jobs_report() {
    let scratch = ScratchDir::new("import-replay-report");
    let job = scratch.path().join("Two.mbox");
    fs::write(&job, mbox(2)).unwrap();
    let engine = open(&scratch.path().join("store"));
    let a = start(&engine, &[fixtures().join("Inbox.mbox")]);
    settle(&engine, &a, ImportJobState::AwaitingConfirmation);
    let key = engine.new_idempotency_key();
    let receipt = block_on(engine.cancel_job(&key, &a)).unwrap();
    // The replay's original-target cleanup still runs: deleting the
    // finished job's report lets the redo be observed.
    let a_report = status(&engine, &a).report_path().unwrap().to_path_buf();
    fs::remove_file(&a_report).unwrap();

    // B finishes and its report is made distinct by extra whitespace —
    // still valid report JSON, but never what the writer emits.
    let b = start(&engine, &[job]);
    settle(&engine, &b, ImportJobState::AwaitingConfirmation);
    confirm(&engine, &b, vec![]).unwrap();
    settle(&engine, &b, ImportJobState::Completed);
    let b_report = status(&engine, &b).report_path().unwrap().to_path_buf();
    let mut sentinel = fs::read_to_string(&b_report).unwrap();
    sentinel.push_str("\n\t ");
    fs::write(&b_report, &sentinel).unwrap();

    let replayed = block_on(engine.cancel_job(&key, &b)).unwrap();
    assert_eq!(replayed.result_json(), receipt.result_json());
    assert_eq!(
        fs::read_to_string(&b_report).unwrap(),
        sentinel,
        "a replay rewrote a job its key never named"
    );
    assert!(
        a_report.exists(),
        "the replay did not redo the receipt target's cleanup"
    );
    block_on(engine.shutdown()).unwrap();
}
