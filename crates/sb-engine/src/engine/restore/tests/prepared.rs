//! `prepare_restore` (plan M1.8i4): a recorded generation becomes a
//! stage the swap takes, from its row alone; what can't be restored is
//! refused before anything is staged; a raised flag ends it with
//! nothing; and the prepared stage holds the backup operation.

use std::sync::atomic::Ordering;

use sb_resources::IntegrityCheck;
use sb_test_support::damage_an_index;

use super::*;
use crate::{BackupEnd, Integrity};

/// Only the answer a raised flag produces counts as cancelled.
#[test]
fn a_step_s_error_is_a_cancellation_only_under_the_flag() {
    use super::super::prepare::ended_by_the_flag;

    let stopped = || StoreError::Unavailable {
        reason: "restore staging cancelled".into(),
        io_kind: None,
    };
    let full = || StoreError::Unavailable {
        reason: "cannot copy an object: no space".into(),
        io_kind: Some(std::io::ErrorKind::StorageFull),
    };
    assert!(ended_by_the_flag(stopped(), true).is_ok());
    assert!(matches!(
        ended_by_the_flag(stopped(), false),
        Err(StoreError::Unavailable { io_kind: None, .. })
    ));
    assert!(matches!(
        ended_by_the_flag(full(), true),
        Err(StoreError::Unavailable {
            io_kind: Some(std::io::ErrorKind::StorageFull),
            ..
        })
    ));
    assert!(matches!(
        ended_by_the_flag(StoreError::internal("a bug"), true),
        Err(StoreError::Internal { .. })
    ));
}

fn unflagged() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

/// The stage directories beside the data root.
fn stages(root: &Path) -> usize {
    std::fs::read_dir(root.parent().unwrap())
        .unwrap()
        .filter(|entry| {
            let name = entry.as_ref().unwrap().file_name();
            name.to_string_lossy().contains(".restoring-")
        })
        .count()
}

fn sql(engine: &Engine, statement: String) {
    block_on(engine.resources().write(WritePriority::User, move |tx| {
        tx.execute_batch(&statement).map_err(sql_error)
    }))
    .unwrap();
}

#[test]
fn a_recorded_generation_is_prepared_from_its_row_and_swaps_in() {
    let scratch = ScratchDir::new("engine-prepare-restore");
    let root = data_root(&scratch);
    let (engine, receipt) = backed_up(&root);
    assert_eq!(settings(&engine), [KEPT, LATER]);

    let prepared = block_on(engine.prepare_restore(receipt.generation_id(), unflagged()))
        .unwrap()
        .expect("not cancelled");
    assert_eq!(prepared.generation_id(), receipt.generation_id());
    // The store was in use throughout and holds what it held.
    assert_eq!(settings(&engine), [KEPT, LATER]);
    assert_eq!(stages(&root), 1);

    let engine = block_on(engine.restore(prepared.into_stage(), unflagged())).unwrap();
    assert_eq!(settings(&engine), [KEPT]);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn what_cannot_be_restored_is_refused_before_anything_is_staged() {
    let scratch = ScratchDir::new("engine-prepare-refused");
    let root = data_root(&scratch);
    let (engine, receipt) = backed_up(&root);

    match block_on(engine.prepare_restore("no-such-generation", unflagged())) {
        Err(EngineError::NotFound { what }) => assert!(what.contains("no-such-generation")),
        other => panic!("expected NotFound, got {other:?}"),
    }

    sql(
        &engine,
        "INSERT INTO backup_generations (generation_id, started_at, state, location)
         VALUES ('gen-failed', 1, 'failed', '/nowhere/gen-failed')"
            .to_owned(),
    );
    match block_on(engine.prepare_restore("gen-failed", unflagged())) {
        Err(EngineError::Conflict { detail }) => assert!(detail.contains("is failed"), "{detail}"),
        other => panic!("expected Conflict, got {other:?}"),
    }

    let copying = engine
        .resources()
        .backup_snapshot(BackupDestination::DataRoot)
        .unwrap();
    match block_on(engine.prepare_restore(copying.generation_id(), unflagged())) {
        Err(EngineError::Conflict { detail }) => {
            assert!(detail.contains("is in progress"), "{detail}");
        }
        other => panic!("expected Conflict, got {other:?}"),
    }
    // A complete one can't be prepared while that copy holds the
    // backup operation.
    match block_on(engine.prepare_restore(receipt.generation_id(), unflagged())) {
        Err(EngineError::Conflict { .. }) => {}
        other => panic!("expected Conflict, got {other:?}"),
    }
    drop(copying);

    // A complete row whose manifest digest is no digest.
    sql(
        &engine,
        format!(
            "UPDATE backup_generations SET manifest_sha256 = 'not a digest'
             WHERE generation_id = '{}'",
            receipt.generation_id()
        ),
    );
    match block_on(engine.prepare_restore(receipt.generation_id(), unflagged())) {
        Err(EngineError::Corrupt { detail }) => {
            assert!(detail.contains(receipt.generation_id()), "{detail}");
        }
        other => panic!("expected Corrupt, got {other:?}"),
    }

    assert_eq!(stages(&root), 0);
    gated_read_works(&engine);
    block_on(engine.shutdown()).unwrap();
}

/// The row's facts are what the manifest is checked against: a digest
/// that is well formed and not this manifest's refuses the generation.
#[test]
fn a_generation_that_does_not_match_its_row_is_refused() {
    let scratch = ScratchDir::new("engine-prepare-mismatch");
    let root = data_root(&scratch);
    let (engine, receipt) = backed_up(&root);
    sql(
        &engine,
        format!(
            "UPDATE backup_generations SET manifest_sha256 = '{}' WHERE generation_id = '{}'",
            "0".repeat(64),
            receipt.generation_id()
        ),
    );
    let refused = block_on(engine.prepare_restore(receipt.generation_id(), unflagged()));
    match refused {
        Err(EngineError::Corrupt { detail }) => {
            assert!(detail.contains("does not match"), "{detail}");
        }
        other => panic!("expected Corrupt, got {other:?}"),
    }
    assert_eq!(stages(&root), 0);
    gated_read_works(&engine);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_generation_whose_files_are_gone_or_changed_is_refused() {
    let scratch = ScratchDir::new("engine-prepare-damaged");
    let root = data_root(&scratch);
    let (engine, receipt) = backed_up(&root);

    // A byte of its database changed.
    let database = receipt.location().join("state.sqlite");
    let mut bytes = std::fs::read(&database).unwrap();
    let last = bytes.len() - 1;
    bytes[last] ^= 0x01;
    std::fs::write(&database, &bytes).unwrap();
    match block_on(engine.prepare_restore(receipt.generation_id(), unflagged())) {
        Err(EngineError::Corrupt { .. }) => {}
        other => panic!("expected Corrupt, got {other:?}"),
    }

    // Its directory gone, as on a volume that isn't mounted.
    std::fs::remove_dir_all(receipt.location()).unwrap();
    match block_on(engine.prepare_restore(receipt.generation_id(), unflagged())) {
        Err(EngineError::Unavailable { .. }) => {}
        other => panic!("expected Unavailable, got {other:?}"),
    }
    assert_eq!(stages(&root), 0);
    gated_read_works(&engine);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_raised_flag_ends_it_with_nothing_and_the_next_one_prepares() {
    let scratch = ScratchDir::new("engine-prepare-cancelled");
    let root = data_root(&scratch);
    let (engine, receipt) = backed_up(&root);

    let flag = Arc::new(AtomicBool::new(true));
    let cancelled = block_on(engine.prepare_restore(receipt.generation_id(), flag.clone()));
    assert!(matches!(cancelled, Ok(None)), "{cancelled:?}");
    assert_eq!(settings(&engine), [KEPT, LATER]);

    // The flag is the call's own: the next call isn't cancelled by it,
    // and the backup operation was given back.
    flag.store(false, Ordering::SeqCst);
    let prepared = block_on(engine.prepare_restore(receipt.generation_id(), flag))
        .unwrap()
        .expect("not cancelled");
    let engine = block_on(engine.restore(prepared.into_stage(), unflagged())).unwrap();
    assert_eq!(settings(&engine), [KEPT]);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_prepared_restore_holds_the_backup_operation_until_it_is_dropped() {
    let scratch = ScratchDir::new("engine-prepare-holds");
    let root = data_root(&scratch);
    let (engine, receipt) = backed_up(&root);
    let prepared = block_on(engine.prepare_restore(receipt.generation_id(), unflagged()))
        .unwrap()
        .expect("not cancelled");

    match block_on(engine.start_backup(&engine.new_idempotency_key())) {
        Err(EngineError::Conflict { .. }) => {}
        other => panic!("expected Conflict, got {other:?}"),
    }
    match block_on(engine.prepare_restore(receipt.generation_id(), unflagged())) {
        Err(EngineError::Conflict { .. }) => {}
        other => panic!("expected Conflict, got {other:?}"),
    }

    // Dropped unused: the operation is free, and the stage stays.
    drop(prepared);
    assert_eq!(stages(&root), 1);
    let started = block_on(engine.start_backup(&engine.new_idempotency_key())).unwrap();
    assert_eq!(
        block_on(started.into_copy().unwrap().finished()),
        BackupEnd::Complete { after: None }
    );
    block_on(engine.shutdown()).unwrap();
}

/// A read-only engine prepares and restores: the way out of a finding.
#[test]
fn a_read_only_engine_prepares_a_restore() {
    let scratch = ScratchDir::new("engine-prepare-read-only");
    let root = data_root(&scratch);
    let (engine, receipt) = backed_up(&root);
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

    let prepared = block_on(engine.prepare_restore(receipt.generation_id(), unflagged()))
        .unwrap()
        .expect("not cancelled");
    let engine = block_on(engine.restore(prepared.into_stage(), unflagged())).unwrap();
    assert_eq!(engine.health().integrity, Integrity::Sound);
    assert_eq!(settings(&engine), [KEPT]);
    block_on(engine.shutdown()).unwrap();
}
