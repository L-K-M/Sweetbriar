//! Tests for engine import jobs (M1.2c3a, spec/09 "Job lifecycle"):
//! commands and the job query end to end over real stores, recovery at
//! open, shutdown pauses, cancellation and failures. The engine's own
//! runtime runs the workers; the tests poll `import_job`.

use std::fs;
use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sb_test_support::{ScratchDir, block_on};

use super::*;

/// How long a test waits for a worker before failing.
const PATIENCE: Duration = Duration::from_secs(120);
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

fn job_id(receipt: &Receipt) -> String {
    receipt
        .result_json()
        .split('"')
        .nth(3)
        .expect("the receipt names its job")
        .to_owned()
}

fn start(engine: &Engine, paths: &[PathBuf]) -> String {
    let key = engine.new_idempotency_key();
    job_id(&block_on(engine.start_import(&key, paths.to_vec())).unwrap())
}

fn status(engine: &Engine, id: &str) -> ImportJobStatus {
    block_on(engine.import_job(id)).unwrap()
}

/// Poll until the job reaches `state` and no worker holds it.
fn settle(engine: &Engine, id: &str, state: ImportJobState) -> ImportJobStatus {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let current = status(engine, id);
        let live = ImportJobs::lock(&engine.imports().live).contains(id);
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
    while ImportJobs::lock(&engine.imports().live).contains(id) {
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
    assert_eq!(first.result_json(), second.result_json());
    settle(
        &engine,
        &job_id(&first),
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
    fs::write(&path, mbox(300)).unwrap();
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
    assert_eq!((totals.processed, totals.imported), (300, 300));
    assert_eq!((totals.read_from_headers, totals.read_default), (1, 299));
    assert_eq!(count(&engine, "SELECT count(*) FROM occurrences"), 300);
    assert_eq!(
        count(&engine, "SELECT count(*) FROM acquisition_sources"),
        300
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
        if state == ImportJobState::Inventory {
            return Ok(());
        }
        record_import_sources(tx, &job, 0, &sources)?;
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
            progress: SourceProgress::Partial,
            flags_incomplete: 0,
        };
        commit_import_job_batch(tx, &batch, record.checkpoint(), now).map(drop)
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
