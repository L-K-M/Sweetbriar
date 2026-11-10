//! The engine's restore (plan M1.8g3): a swap through a live engine
//! restores the generation's rows with every account paused and answers
//! subscriptions with `ResetRequired`; a refusal hands back a working
//! engine and a stage that swaps on retry; a failure ends the engine and
//! the next open resolves what the swap left. The quiesce refuses store
//! work but still runs owed cleanup, and resumes.

use std::path::{Path, PathBuf};
use std::pin::pin;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, mpsc};
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use sb_resources::{
    BackupDestination, BackupReceipt, QualifiedStage, RestoreMetadata, StoreError, WritePriority,
};
use sb_test_support::{ScratchDir, block_on};

use super::super::BlockingWork;
use super::RestoreError;
use crate::{
    AccountAppearanceChange, AccountColor, Engine, EngineError, ImportJobState, WindowEvent,
};

const PATIENCE: Duration = Duration::from_secs(60);

/// Poll `future` until it finishes or `PATIENCE` runs out: a broken
/// restore fails here instead of hanging the suite.
fn finish<F: std::future::Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
            return output;
        }
        assert!(Instant::now() < deadline, "the future never finished");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A read through the engine's admission gate, which a quiesce that
/// never resumed would refuse.
fn gated_read_works(engine: &Engine) {
    block_on(engine.accounts()).expect("the engine takes commands");
}

const KEPT: &str = "test.restore.kept";
const LATER: &str = "test.restore.later";

fn data_root(scratch: &ScratchDir) -> PathBuf {
    std::fs::canonicalize(scratch.path()).unwrap().join("store")
}

fn open(root: &Path) -> Engine {
    block_on(Engine::open(root)).unwrap()
}

fn sql_error(error: impl std::fmt::Display) -> StoreError {
    StoreError::internal(error.to_string())
}

fn add_setting(engine: &Engine, key: &'static str) {
    block_on(engine.resources().write(WritePriority::User, move |tx| {
        tx.execute(
            "INSERT INTO settings (key, value_json, updated_at) VALUES (?1, '1', 0)",
            [key],
        )
        .map(drop)
        .map_err(sql_error)
    }))
    .unwrap();
}

/// The test settings the store holds, in key order.
fn settings(engine: &Engine) -> Vec<String> {
    engine
        .resources()
        .read(|tx| {
            let mut statement = tx
                .prepare("SELECT key FROM settings WHERE key LIKE 'test.restore.%' ORDER BY key")
                .map_err(sql_error)?;
            statement
                .query_map([], |row| row.get(0))
                .map_err(sql_error)?
                .collect::<Result<_, _>>()
                .map_err(sql_error)
        })
        .unwrap()
}

/// Every account's state, in id order.
fn account_states(engine: &Engine) -> Vec<String> {
    engine
        .resources()
        .read(|tx| {
            let mut statement = tx
                .prepare("SELECT state FROM accounts ORDER BY account_id")
                .map_err(sql_error)?;
            statement
                .query_map([], |row| row.get(0))
                .map_err(sql_error)?
                .collect::<Result<_, _>>()
                .map_err(sql_error)
        })
        .unwrap()
}

fn appearance_generation(engine: &Engine) -> u64 {
    engine
        .resources()
        .read(sb_resources::appearance_generation)
        .unwrap()
}

/// An engine whose store holds `KEPT`, backed up inside its data root,
/// and then `LATER`, which the generation does not hold.
fn backed_up(root: &Path) -> (Engine, BackupReceipt) {
    let engine = open(root);
    add_setting(&engine, KEPT);
    let snapshot = engine
        .resources()
        .backup_snapshot(BackupDestination::DataRoot)
        .unwrap();
    let receipt = engine.resources().complete_backup(snapshot).unwrap();
    add_setting(&engine, LATER);
    (engine, receipt)
}

fn metadata(engine: &Engine, generation: &str) -> RestoreMetadata {
    let generation = generation.to_owned();
    engine
        .resources()
        .read(move |tx| {
            tx.query_row(
                "SELECT manifest_sha256, object_count, total_bytes, completed_at
                 FROM backup_generations WHERE generation_id = ?1",
                [&generation],
                |row| {
                    let hex: String = row.get(0)?;
                    let mut manifest_sha256 = [0u8; 32];
                    for (byte, pair) in manifest_sha256.iter_mut().zip(hex.as_bytes().chunks(2)) {
                        *byte = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap();
                    }
                    Ok(RestoreMetadata {
                        manifest_sha256,
                        object_count: row.get(1)?,
                        total_bytes: row.get(2)?,
                        completed_at: row.get(3)?,
                    })
                },
            )
            .map_err(sql_error)
        })
        .unwrap()
}

fn qualified(engine: &Engine, receipt: &BackupReceipt) -> QualifiedStage {
    let resources = engine.resources();
    let cancel = AtomicBool::new(false);
    let location = receipt.location();
    let selection = resources
        .verify_external_backup(location.parent().unwrap(), receipt.generation_id(), &cancel)
        .unwrap()
        .expect("not cancelled");
    let prepared = resources
        .stage_restore(
            selection,
            location,
            metadata(engine, receipt.generation_id()),
            &cancel,
        )
        .unwrap();
    resources
        .qualify_restore(prepared, &Arc::new(AtomicBool::new(false)))
        .unwrap()
}

#[test]
fn a_restore_swaps_the_generation_in_and_resets_subscriptions() {
    let scratch = ScratchDir::new("engine-restore");
    let root = data_root(&scratch);
    let (engine, receipt) = backed_up(&root);
    let stage = qualified(&engine, &receipt);
    let appearance = engine.subscribe_appearance(appearance_generation(&engine));

    let engine = block_on(engine.restore(stage, Arc::new(AtomicBool::new(false)))).unwrap();

    assert_eq!(settings(&engine), [KEPT]);
    assert!(
        account_states(&engine)
            .iter()
            .all(|state| state == "paused"),
        "the restored store opens quarantined"
    );
    assert_eq!(
        finish(appearance.next()).unwrap(),
        WindowEvent::ResetRequired
    );
    // Commands run again, on the restored store, and its new writer's
    // commits reach subscriptions.
    gated_read_works(&engine);
    let (account, revision): (String, i64) = engine
        .resources()
        .read(|tx| {
            tx.query_row(
                "SELECT account_id, revision FROM account_appearance LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(sql_error)
        })
        .unwrap();
    let change = AccountAppearanceChange::new(AccountColor::Custom("#336699".into()), None);
    block_on(engine.set_account_appearance(
        &engine.new_idempotency_key(),
        &account,
        revision,
        &change,
    ))
    .unwrap();
    assert!(matches!(
        finish(appearance.next()).unwrap(),
        WindowEvent::Invalidated { .. }
    ));
    add_setting(&engine, LATER);
    assert_eq!(settings(&engine), [KEPT, LATER]);
    block_on(engine.shutdown()).unwrap();

    let reopened = open(&root);
    assert_eq!(settings(&reopened), [KEPT, LATER]);
    block_on(reopened.shutdown()).unwrap();
}

/// The quiesce stops a running maintenance schedule at once (a parked
/// worker would otherwise hold it until its next look, a minute on)
/// and the resume starts it again on the restored store; a schedule
/// never started stays off.
#[test]
fn a_restore_stops_the_maintenance_schedule_and_starts_it_again() {
    let scratch = ScratchDir::new("engine-restore-maintenance");
    let root = data_root(&scratch);
    let (engine, receipt) = backed_up(&root);
    engine.schedule_maintenance();
    let stage = qualified(&engine, &receipt);

    let started = Instant::now();
    let engine = finish(engine.restore(stage, Arc::new(AtomicBool::new(false)))).unwrap();
    assert!(
        started.elapsed() < crate::maintenance::POLL / 2,
        "{:?}",
        started.elapsed()
    );
    assert!(engine.maintenance().running());
    let started = Instant::now();
    block_on(engine.shutdown()).unwrap();
    assert!(started.elapsed() < Duration::from_secs(30));

    let unscheduled = ScratchDir::new("engine-restore-unscheduled");
    let (engine, receipt) = backed_up(&data_root(&unscheduled));
    let stage = qualified(&engine, &receipt);
    let engine = finish(engine.restore(stage, Arc::new(AtomicBool::new(false)))).unwrap();
    assert!(!engine.maintenance().running());
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_refused_restore_hands_back_a_working_engine_and_a_stage_that_swaps_on_retry() {
    let scratch = ScratchDir::new("engine-restore-refused");
    let root = data_root(&scratch);
    let (engine, receipt) = backed_up(&root);
    let stage = qualified(&engine, &receipt);

    let Err(RestoreError::Refused {
        cause,
        engine,
        stage,
    }) = block_on(engine.restore(stage, Arc::new(AtomicBool::new(true))))
    else {
        panic!("a cancelled restore is refused");
    };
    assert!(
        matches!(cause, EngineError::Unavailable { .. }),
        "{cause:?}"
    );
    // Nothing moved, and the engine takes commands again.
    gated_read_works(&engine);
    assert_eq!(settings(&engine), [KEPT, LATER]);
    add_setting(&engine, "test.restore.refused");
    assert_eq!(settings(&engine).len(), 3);

    let engine = block_on(engine.restore(*stage, Arc::new(AtomicBool::new(false)))).unwrap();
    assert_eq!(settings(&engine), [KEPT]);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_failed_restore_ends_the_engine_and_the_next_open_resolves_it() {
    let scratch = ScratchDir::new("engine-restore-failed");
    let root = data_root(&scratch);
    let (engine, receipt) = backed_up(&root);
    let stage = qualified(&engine, &receipt);
    // A directory under the marker's temporary name: writing the marker
    // fails once the store has closed.
    let temporary = root.with_extension("restore-pending.tmp");
    std::fs::create_dir(&temporary).unwrap();

    let result = block_on(engine.restore(stage, Arc::new(AtomicBool::new(false))));

    assert!(matches!(result, Err(RestoreError::Failed(_))), "{result:?}");
    std::fs::remove_dir(&temporary).unwrap();
    let reopened = open(&root);
    assert_eq!(settings(&reopened), [KEPT, LATER]);
    block_on(reopened.shutdown()).unwrap();
}

/// The quiesce refuses store work, still admits cleanup that never
/// touches the store, asks long work to stop, and resumes.
#[test]
fn the_quiesce_refuses_store_work_admits_cleanup_and_resumes() {
    let work = Arc::new(BlockingWork::default());
    let running = work.start().unwrap();
    let quiescing = {
        let work = Arc::clone(&work);
        std::thread::spawn(move || work.quiesce_and_wait())
    };
    let deadline = Instant::now() + PATIENCE;
    while !work.closing() {
        assert!(Instant::now() < deadline, "the quiesce never began");
        std::thread::yield_now();
    }
    assert!(matches!(work.start(), Err(EngineError::Unavailable { .. })));
    let cleanup = work.start_cleanup().unwrap();
    drop(cleanup);
    assert!(
        !quiescing.is_finished(),
        "the running task holds the quiesce"
    );
    drop(running);
    quiescing.join().unwrap();

    work.resume();
    assert!(!work.closing());
    drop(work.start().unwrap());
}

/// One message to import.
fn mbox_file(dir: &Path) -> PathBuf {
    let path = dir.join("One.mbox");
    std::fs::write(
        &path,
        "From sender@example.test Sat Oct 17 12:00:00 2026\nSubject: one\n\nbody\n",
    )
    .unwrap();
    path
}

/// An import's inventory resumes from no checkpoint, so a restore is
/// refused while one runs, before anything stops: the inventory then
/// finishes as if no restore had been asked for.
#[test]
fn a_restore_waits_for_no_inventory_and_refuses_while_one_runs() {
    let scratch = ScratchDir::new("engine-restore-inventory");
    let root = data_root(&scratch);
    let (engine, receipt) = backed_up(&root);
    let stage = qualified(&engine, &receipt);
    let (entered, reached) = mpsc::channel();
    let (release, released) = mpsc::channel::<()>();
    let released = Mutex::new(released);
    *engine
        .imports()
        .before_task
        .lock()
        .expect("a panicking thread held the import job registry") = Some(Arc::new(move |_| {
        entered.send(()).unwrap();
        let _ = released.lock().unwrap().recv();
    }));
    let started = block_on(engine.start_import(
        &engine.new_idempotency_key(),
        vec![mbox_file(scratch.path())],
    ))
    .unwrap();
    reached.recv_timeout(PATIENCE).unwrap();

    let Err(RestoreError::Refused {
        cause,
        engine,
        stage: _,
    }) = block_on(engine.restore(stage, Arc::new(AtomicBool::new(false))))
    else {
        panic!("a restore during an inventory is refused");
    };
    assert!(matches!(cause, EngineError::Conflict { .. }), "{cause:?}");
    gated_read_works(&engine);

    release.send(()).unwrap();
    let deadline = Instant::now() + PATIENCE;
    loop {
        let job = block_on(engine.import_job(started.job_id())).unwrap();
        if job.state() != ImportJobState::Inventory {
            assert_eq!(job.state(), ImportJobState::AwaitingConfirmation);
            break;
        }
        assert!(Instant::now() < deadline, "the inventory never finished");
        std::thread::sleep(Duration::from_millis(20));
    }
    block_on(engine.shutdown()).unwrap();
}

/// While a restore holds starts, a new inventory is refused before any
/// write; once released, it starts.
#[test]
fn a_held_start_is_refused_before_it_writes() {
    let scratch = ScratchDir::new("engine-restore-held-start");
    let engine = open(&data_root(&scratch));
    let path = mbox_file(scratch.path());
    let held = engine.imports().hold_starts();
    let refused = block_on(engine.start_import(&engine.new_idempotency_key(), vec![path.clone()]));
    assert!(
        matches!(refused, Err(EngineError::Unavailable { .. })),
        "{refused:?}"
    );
    let jobs: i64 = engine
        .resources()
        .read(|tx| {
            tx.query_row("SELECT count(*) FROM import_jobs", [], |row| row.get(0))
                .map_err(sql_error)
        })
        .unwrap();
    assert_eq!(jobs, 0, "nothing was written");
    drop(held);
    block_on(engine.start_import(&engine.new_idempotency_key(), vec![path])).unwrap();
    block_on(engine.shutdown()).unwrap();
}

mod acceptance;

/// An engine like `backed_up`'s whose store also holds damage only the
/// full check finds; the generation was taken before the damage.
fn backed_up_then_damaged(root: &Path) -> (Engine, BackupReceipt) {
    let (engine, receipt) = backed_up(root);
    block_on(engine.shutdown()).unwrap();
    sb_test_support::damage_an_index(&root.join("state.sqlite"));
    (open(root), receipt)
}

fn find_the_damage(engine: &Engine) {
    let found = engine
        .resources()
        .check_integrity(&AtomicBool::new(false))
        .unwrap();
    assert!(
        matches!(found, sb_resources::IntegrityCheck::Finding(_)),
        "{found:?}"
    );
}

fn index_state(engine: &Engine) -> crate::SearchIndexState {
    block_on(engine.coverage(crate::SearchScope::All))
        .unwrap()
        .index_state
}

/// A restore decides whether read-only ends (plan M1.8h5b2): refused,
/// the engine comes back read-only on the latched store with no worker;
/// swapped, the restored store has no latch, the workers run again and
/// health subscriptions hear of it.
#[test]
fn a_restore_ends_read_only_and_a_refused_one_keeps_it() {
    let scratch = ScratchDir::new("engine-restore-read-only");
    let root = data_root(&scratch);
    let (engine, receipt) = backed_up_then_damaged(&root);
    // Staged before the finding: verification is refused after it
    // until M1.8h5b4.
    let stage = qualified(&engine, &receipt);
    find_the_damage(&engine);
    assert!(engine.read_only());
    // Asked for on a read-only engine, as a latched open asks: the
    // schedule is armed and starts nothing.
    engine.schedule_maintenance();
    assert!(!engine.maintenance().running());
    let rebuilds = engine.imports().threads().starts();
    let health = engine.subscribe_health();

    let Err(RestoreError::Refused { engine, stage, .. }) =
        block_on(engine.restore(stage, Arc::new(AtomicBool::new(true))))
    else {
        panic!("a cancelled restore is refused");
    };
    assert!(engine.read_only());
    gated_read_works(&engine);
    assert_eq!(settings(&engine), [KEPT, LATER]);
    // The workers were not resumed: each is still told to stop.
    assert!(engine.indexer().stopped());
    assert!(engine.received().stopped());
    assert!(!engine.indexer().running());
    assert!(!engine.received().running());
    assert!(!engine.maintenance().running());
    assert_eq!(engine.imports().threads().starts(), rebuilds);
    assert_eq!(index_state(&engine), crate::SearchIndexState::ReadOnly);

    let engine = finish(engine.restore(*stage, Arc::new(AtomicBool::new(false)))).unwrap();

    assert!(!engine.read_only());
    assert_eq!(engine.health().integrity, crate::Integrity::Sound);
    finish(health.next()).expect("the end of read-only wakes the subscription");
    assert_eq!(settings(&engine), [KEPT]);
    add_setting(&engine, "test.restore.after");
    assert!(engine.indexer().running());
    assert!(engine.received().running());
    // The schedule the read-only engine could only arm starts now,
    // and thread rebuilds are asked for again.
    assert!(engine.maintenance().running());
    assert!(engine.imports().threads().starts() > rebuilds);
    assert_ne!(index_state(&engine), crate::SearchIndexState::ReadOnly);
    block_on(engine.shutdown()).unwrap();
}
