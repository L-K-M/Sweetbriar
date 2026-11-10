//! `start_restore` (plan M1.8i5): the swap as a command. Its receipt
//! is written in the restored store; a repeated key answers it and
//! moves nothing; a refusal hands everything back and spends no key;
//! and a restore whose receipt can't be recorded still stands.

use sb_resources::{IntegrityCheck, receipt_generation_id};
use sb_test_support::damage_an_index;

use super::*;
use crate::index_tests::count;
use crate::{Integrity, PreparedRestore, Restored};

const RESTORES: &str = "SELECT count(*) FROM command_receipts WHERE command = 'start_restore'";

fn unflagged() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

fn prepared(engine: &Engine, generation_id: &str) -> PreparedRestore {
    block_on(engine.prepare_restore(generation_id, unflagged()))
        .unwrap()
        .expect("not cancelled")
}

/// The roots earlier swaps preserved beside the data root.
fn preserved(root: &Path) -> usize {
    std::fs::read_dir(root.parent().unwrap())
        .unwrap()
        .filter(|entry| {
            let name = entry.as_ref().unwrap().file_name();
            name.to_string_lossy().contains(".preserved-")
        })
        .count()
}

#[test]
fn a_restore_under_a_key_records_its_receipt_in_the_restored_store() {
    let scratch = ScratchDir::new("engine-start-restore");
    let root = data_root(&scratch);
    let (engine, backup) = backed_up(&root);
    let key = engine.new_idempotency_key();
    let prepared = prepared(&engine, backup.generation_id());
    assert_eq!(count(&engine, RESTORES), 0);

    let Restored { engine, receipt } =
        finish(engine.start_restore(&key, prepared, unflagged())).unwrap();
    let receipt = receipt.unwrap();
    assert_eq!(receipt.command(), "start_restore");
    assert_eq!(
        receipt_generation_id(&receipt).unwrap(),
        backup.generation_id()
    );
    assert_eq!(settings(&engine), [KEPT]);
    assert_eq!(preserved(&root), 1);
    assert_eq!(count(&engine, RESTORES), 1);
    assert_eq!(
        engine.resources().restore_receipt(&key).unwrap(),
        Some(receipt)
    );
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_repeated_key_answers_its_receipt_and_moves_nothing() {
    let scratch = ScratchDir::new("engine-start-restore-again");
    let root = data_root(&scratch);
    let (engine, backup) = backed_up(&root);
    let key = engine.new_idempotency_key();
    let first = prepared(&engine, backup.generation_id());
    let Restored { engine, receipt } =
        finish(engine.start_restore(&key, first, unflagged())).unwrap();
    let receipt = receipt.unwrap();

    // What the restored store holds since is not in the generation.
    add_setting(&engine, "test.restore.since");
    let again = prepared(&engine, backup.generation_id());
    let Restored {
        engine,
        receipt: replayed,
    } = finish(engine.start_restore(&key, again, unflagged())).unwrap();
    assert_eq!(replayed.unwrap(), receipt);
    assert_eq!(settings(&engine), [KEPT, "test.restore.since"]);
    assert_eq!(preserved(&root), 1);
    assert_eq!(count(&engine, RESTORES), 1);
    gated_read_works(&engine);
    // The preparation it didn't use gave the backup operation back.
    drop(prepared(&engine, backup.generation_id()));
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_key_that_cannot_run_a_restore_is_refused_with_everything_handed_back() {
    let scratch = ScratchDir::new("engine-start-restore-key");
    let root = data_root(&scratch);
    let (engine, backup) = backed_up(&root);
    let spent = engine.new_idempotency_key();
    block_on(engine.set_setting(&spent, "appearance.compose_cue", "strong")).unwrap();
    let prepared = prepared(&engine, backup.generation_id());

    // A key spent on another command.
    let Err(RestoreError::Refused {
        cause,
        engine,
        stage,
    }) = finish(engine.start_restore(&spent, prepared, unflagged()))
    else {
        panic!("a key spent on another command is refused");
    };
    assert!(matches!(cause, EngineError::Conflict { .. }), "{cause:?}");
    // No key at all.
    let Err(RestoreError::Refused {
        cause,
        engine,
        stage,
    }) = finish(engine.start_restore("", *stage, unflagged()))
    else {
        panic!("an empty key is refused");
    };
    match cause {
        EngineError::InvalidInput { field, .. } => assert_eq!(field, "idempotency_key"),
        other => panic!("expected InvalidInput, got {other:?}"),
    }

    // Nothing moved, and the engine takes commands.
    gated_read_works(&engine);
    assert_eq!(settings(&engine), [KEPT, LATER]);
    assert_eq!((preserved(&root), count(&engine, RESTORES)), (0, 0));
    assert_eq!(stage.generation_id(), backup.generation_id());

    // What came back swaps under a key of its own.
    let key = engine.new_idempotency_key();
    let Restored { engine, receipt } =
        finish(engine.start_restore(&key, *stage, unflagged())).unwrap();
    receipt.unwrap();
    assert_eq!(settings(&engine), [KEPT]);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_refused_swap_spends_no_key() {
    let scratch = ScratchDir::new("engine-start-restore-refused");
    let root = data_root(&scratch);
    let (engine, backup) = backed_up(&root);
    let key = engine.new_idempotency_key();
    let prepared = prepared(&engine, backup.generation_id());

    let raised = Arc::new(AtomicBool::new(true));
    let Err(RestoreError::Refused {
        cause,
        engine,
        stage,
    }) = finish(engine.start_restore(&key, prepared, raised))
    else {
        panic!("a cancelled restore is refused");
    };
    assert!(
        matches!(cause, EngineError::Unavailable { .. }),
        "{cause:?}"
    );
    assert_eq!(settings(&engine), [KEPT, LATER]);
    assert_eq!(engine.resources().restore_receipt(&key).unwrap(), None);
    assert_eq!(stage.generation_id(), backup.generation_id());

    // The same key and the same preparation restore afterwards.
    let Restored { engine, receipt } =
        finish(engine.start_restore(&key, *stage, unflagged())).unwrap();
    assert_eq!(
        receipt_generation_id(&receipt.unwrap()).unwrap(),
        backup.generation_id()
    );
    assert_eq!(settings(&engine), [KEPT]);
    block_on(engine.shutdown()).unwrap();
}

/// The restored store refuses the receipt. Here its key was spent on
/// another command before the generation was taken, and the store
/// being replaced has lost that row since. The restore stands, and the
/// answer says its receipt doesn't.
#[test]
fn a_restore_whose_receipt_cannot_be_recorded_still_stands() {
    let scratch = ScratchDir::new("engine-start-restore-unrecorded");
    let root = data_root(&scratch);
    let engine = open(&root);
    let key = engine.new_idempotency_key();
    block_on(engine.set_setting(&key, "appearance.compose_cue", "strong")).unwrap();
    add_setting(&engine, KEPT);
    let snapshot = engine
        .resources()
        .backup_snapshot(BackupDestination::DataRoot)
        .unwrap();
    let backup = engine.resources().complete_backup(snapshot).unwrap();
    add_setting(&engine, LATER);
    block_on(engine.resources().write(WritePriority::User, |tx| {
        tx.execute("DELETE FROM command_receipts", [])
            .map(drop)
            .map_err(sql_error)
    }))
    .unwrap();
    let prepared = prepared(&engine, backup.generation_id());

    let Restored { engine, receipt } =
        finish(engine.start_restore(&key, prepared, unflagged())).unwrap();
    assert!(
        matches!(receipt, Err(EngineError::Conflict { .. })),
        "{receipt:?}"
    );
    assert_eq!(settings(&engine), [KEPT]);
    assert_eq!((preserved(&root), count(&engine, RESTORES)), (1, 0));
    gated_read_works(&engine);
    block_on(engine.shutdown()).unwrap();
}

/// A restore is the way out of a read-only engine, under a key too:
/// the key is looked up in the latched store and recorded in the
/// sound one.
#[test]
fn a_read_only_engine_restores_under_a_key() {
    let scratch = ScratchDir::new("engine-start-restore-read-only");
    let root = data_root(&scratch);
    let (engine, backup) = backed_up(&root);
    block_on(engine.shutdown()).unwrap();
    damage_an_index(&root.join("state.sqlite"));
    let engine = open(&root);
    let check = engine
        .resources()
        .check_integrity(&AtomicBool::new(false))
        .unwrap();
    assert!(matches!(check, IntegrityCheck::Finding(_)), "{check:?}");
    assert!(matches!(
        engine.health().integrity,
        Integrity::ReadOnly { .. }
    ));

    let key = engine.new_idempotency_key();
    let prepared = prepared(&engine, backup.generation_id());
    let Restored { engine, receipt } =
        finish(engine.start_restore(&key, prepared, unflagged())).unwrap();
    assert_eq!(
        receipt_generation_id(&receipt.unwrap()).unwrap(),
        backup.generation_id()
    );
    assert_eq!(engine.health().integrity, Integrity::Sound);
    assert_eq!(settings(&engine), [KEPT]);
    block_on(engine.shutdown()).unwrap();
}
