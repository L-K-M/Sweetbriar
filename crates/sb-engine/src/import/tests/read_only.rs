//! Imports on a read-only store (plan M1.8h5b2; spec/03 "Integrity"):
//! an integrity finding stops a running import at its next step, and a
//! latched open recovers nothing, so every job row, snapshot and
//! report stays as stored.

use std::sync::atomic::AtomicBool;

use sb_resources::IntegrityCheck;
use sb_test_support::{damage_an_index, execute_sql, store_meta_value};

use super::*;

/// An engine on a store holding damage only the full check finds.
fn damaged(root: &Path) -> Engine {
    block_on(open(root).shutdown()).unwrap();
    damage_an_index(&root.join("state.sqlite"));
    open(root)
}

/// Run the full check, which finds the damage and latches the store.
fn find_the_damage(engine: &Engine) {
    let found = engine
        .resources()
        .check_integrity(&AtomicBool::new(false))
        .unwrap();
    assert!(matches!(found, IntegrityCheck::Finding(_)), "{found:?}");
}

/// A waiting job cancelled without its cleanup, as a crash right after
/// the transition leaves it: an open owes it a report.
fn plant_cancelled(engine: &Engine, path: &Path) -> String {
    let id = plant(engine, path, ImportJobState::AwaitingConfirmation, 0);
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
    id
}

/// Every rebuild job as stored: id, epoch, phase and cursors.
fn rebuild_jobs(engine: &Engine) -> String {
    engine
        .resources()
        .read(|tx| {
            tx.query_row(
                "SELECT coalesce(group_concat(job_id || '/' || epoch || '/' || phase || '/' ||
                        doc_cursor || '/' || acq_cursor || '/' || phase_count, ' '), '')
                 FROM (SELECT * FROM search_rebuild_jobs ORDER BY epoch)",
                [],
                |row| row.get(0),
            )
            .map_err(|e| StoreError::internal(e.to_string()))
        })
        .unwrap()
}

#[test]
fn a_finding_stops_a_running_import_at_its_next_step() {
    let scratch = ScratchDir::new("import-read-only-running");
    let path = scratch.path().join("Two.mbox");
    fs::write(&path, mbox(2)).unwrap();
    let root = scratch.path().join("store");
    let engine = damaged(&root);
    let id = plant(&engine, &path, ImportJobState::Running, 1);

    // Park the resumed worker before its next gather, until it is
    // told to stop.
    let (entered, reached) = mpsc::channel();
    *engine
        .imports()
        .before_task
        .lock()
        .expect("a panicking thread held the import job registry") =
        Some(Arc::new(move |stopping| {
            entered.send(()).unwrap();
            while !stopping() {
                std::thread::yield_now();
            }
        }));
    engine.recover_imports().unwrap();
    reached.recv_timeout(PATIENCE).unwrap();

    // The finding is the stop: nothing else releases the worker.
    find_the_damage(&engine);

    idle(&engine, &id);
    let paused = status(&engine, &id);
    assert_eq!(paused.state(), ImportJobState::Running);
    assert_eq!(paused.checkpoint().totals().processed, 1);
    assert_eq!(count(&engine, "SELECT count(*) FROM occurrences"), 1);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_latched_open_leaves_every_job_as_stored() {
    let scratch = ScratchDir::new("import-read-only-open");
    let path = scratch.path().join("Three.mbox");
    fs::write(&path, mbox(3)).unwrap();
    let root = scratch.path().join("store");
    let engine = damaged(&root);
    let interrupted = plant(&engine, &path, ImportJobState::Inventory, 0);
    let running = plant(&engine, &path, ImportJobState::Running, 1);
    let cancelled = plant_cancelled(&engine, &path);
    let kept = snapshots(&root);
    assert!(kept > 0);
    find_the_damage(&engine);
    let rebuilds = rebuild_jobs(&engine);
    block_on(engine.shutdown()).unwrap();
    // An older extractor target, which an open would start a rebuild
    // for (spec/06).
    let state_db = root.join("state.sqlite");
    execute_sql(
        &state_db,
        "UPDATE store_meta SET value='1' WHERE key='search_extractor_version'",
    );

    let engine = open(&root);

    assert!(engine.read_only());
    // An interrupted inventory is not failed, a running job gets no
    // worker, and a finished one's cleanup is not redone.
    assert_eq!(
        status(&engine, &interrupted).state(),
        ImportJobState::Inventory
    );
    let stored = status(&engine, &running);
    assert_eq!(stored.state(), ImportJobState::Running);
    assert_eq!(stored.checkpoint().totals().processed, 1);
    assert!(engine.imports().lock().is_empty(), "no import worker runs");
    assert_eq!(status(&engine, &cancelled).report_path(), None);
    assert_eq!(snapshots(&root), kept);
    assert_eq!(count(&engine, "SELECT count(*) FROM occurrences"), 1);
    // The target is as stored and no rebuild job was made for it.
    assert_eq!(rebuild_jobs(&engine), rebuilds);
    block_on(engine.shutdown()).unwrap();
    assert_eq!(
        store_meta_value(&state_db, "search_extractor_version").as_deref(),
        Some("1")
    );
}

/// A read-only store's recovery is best-effort: damage that reaches
/// the job table itself must not keep the store closed.
#[test]
fn a_latched_store_opens_even_when_its_jobs_cannot_be_read() {
    let scratch = ScratchDir::new("import-read-only-unreadable");
    let path = scratch.path().join("One.mbox");
    fs::write(&path, mbox(1)).unwrap();
    let root = scratch.path().join("store");
    let engine = damaged(&root);
    let cancelled = plant_cancelled(&engine, &path);
    find_the_damage(&engine);
    block_on(engine.shutdown()).unwrap();
    // The table is gone, as far as any read can tell.
    execute_sql(
        &root.join("state.sqlite"),
        "PRAGMA foreign_keys = OFF; ALTER TABLE import_jobs RENAME TO import_jobs_lost;",
    );

    let engine = open(&root);

    assert!(engine.read_only());
    assert!(block_on(engine.import_job(&cancelled)).is_err());
    // The rest of the store still answers.
    block_on(engine.settings()).unwrap();
    block_on(engine.shutdown()).unwrap();
}
