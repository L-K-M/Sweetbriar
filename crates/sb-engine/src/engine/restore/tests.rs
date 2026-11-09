//! The engine's restore (plan M1.8g3): a swap through a live engine
//! restores the generation's rows with every account paused and answers
//! subscriptions with `ResetRequired`; a refusal hands back a working
//! engine and a stage that swaps on retry; a failure ends the engine and
//! the next open resolves what the swap left. The quiesce refuses store
//! work but still runs owed cleanup, and resumes.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use sb_resources::{
    BackupDestination, BackupReceipt, QualifiedStage, RestoreMetadata, StoreError, WritePriority,
};
use sb_test_support::{ScratchDir, block_on};

use super::super::BlockingWork;
use super::RestoreError;
use crate::{Engine, EngineError, WindowEvent};

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
        block_on(appearance.next()).unwrap(),
        WindowEvent::ResetRequired
    );
    // Commands run again, on the restored store.
    add_setting(&engine, LATER);
    assert_eq!(settings(&engine), [KEPT, LATER]);
    block_on(engine.shutdown()).unwrap();

    let reopened = open(&root);
    assert_eq!(settings(&reopened), [KEPT, LATER]);
    block_on(reopened.shutdown()).unwrap();
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
    while !work.closing() {
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
