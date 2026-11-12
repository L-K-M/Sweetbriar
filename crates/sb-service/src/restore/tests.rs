//! `prepare_restore` and `restore` through the service (plan M1.8i4,
//! M1.8i5).

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use sb_test_support::{ScratchDir, block_on};

use crate::{BackupEnd, Core, CoreConfig, CoreError, RestoreFailure, Restored, open_core};

fn open(scratch: &ScratchDir) -> Core {
    // Staging opens the data root's parent without following links
    // (macOS's /var is one).
    let root = std::fs::canonicalize(scratch.path()).unwrap();
    block_on(open_core(CoreConfig {
        data_root: root.join("store"),
        cache_root: root.join("cache"),
    }))
    .unwrap()
}

/// One complete generation: its id.
fn back_up(core: &Core, key: &str) -> String {
    let started = block_on(core.start_backup(key)).unwrap();
    let copy = started.copy.expect("a fresh key starts a copy");
    assert_eq!(
        block_on(copy.finished()),
        BackupEnd::Complete { after: None }
    );
    started.generation_id
}

fn unflagged() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

#[test]
fn a_listed_generation_is_prepared_and_holds_the_backup_operation() {
    let scratch = ScratchDir::new("service-prepare-restore");
    let core = open(&scratch);
    let id = back_up(&core, "00000000-0000-4000-8000-0000000000d1");

    let prepared = block_on(core.prepare_restore(&id, unflagged())).unwrap();
    assert_eq!(prepared.generation_id(), id);
    // The core is in use, and nothing else takes the operation.
    assert_eq!(block_on(core.list_backups()).unwrap().len(), 1);
    match block_on(core.start_backup("00000000-0000-4000-8000-0000000000d2")) {
        Err(CoreError::Conflict { .. }) => {}
        other => panic!("expected Conflict, got {other:?}"),
    }

    drop(prepared);
    back_up(&core, "00000000-0000-4000-8000-0000000000d2");
    block_on(core.shutdown());
}

#[test]
fn a_raised_flag_is_cancelled_and_an_unknown_generation_not_found() {
    let scratch = ScratchDir::new("service-prepare-refused");
    let core = open(&scratch);
    let id = back_up(&core, "00000000-0000-4000-8000-0000000000d3");

    match block_on(core.prepare_restore(&id, Arc::new(AtomicBool::new(true)))) {
        Err(CoreError::Cancelled) => {}
        other => panic!("expected Cancelled, got {other:?}"),
    }
    match block_on(core.prepare_restore("no-such-generation", unflagged())) {
        Err(CoreError::NotFound { .. }) => {}
        other => panic!("expected NotFound, got {other:?}"),
    }
    // Neither left the operation held.
    let prepared = block_on(core.prepare_restore(&id, unflagged())).unwrap();
    assert_eq!(prepared.generation_id(), id);
    drop(prepared);
    block_on(core.shutdown());
}

#[test]
fn retained_restore_paths_survive_a_swap_and_restart_without_cleanup() {
    use crate::RestoreArtifactKind::{Preserved, Staged};

    let scratch = ScratchDir::new("service-restore-artifacts");
    let core = open(&scratch);
    assert!(block_on(core.list_restore_artifacts()).unwrap().is_empty());
    let id = back_up(&core, "00000000-0000-4000-8000-0000000000f1");
    let abandoned = block_on(core.prepare_restore(&id, unflagged())).unwrap();
    let paths = block_on(core.list_restore_artifacts()).unwrap();
    assert_eq!(paths.len(), 1);
    assert_eq!(paths[0].kind, Staged);
    let abandoned_path = paths[0].path.clone();
    assert!(abandoned_path.is_dir());
    drop(abandoned);
    assert_eq!(block_on(core.list_restore_artifacts()).unwrap(), paths);

    let prepared = block_on(core.prepare_restore(&id, unflagged())).unwrap();
    assert_eq!(block_on(core.list_restore_artifacts()).unwrap().len(), 2);
    let Restored { core, receipt } = block_on(core.restore(
        "00000000-0000-4000-8000-0000000000f2",
        prepared,
        unflagged(),
    ))
    .unwrap();
    receipt.unwrap();
    let paths = block_on(core.list_restore_artifacts()).unwrap();
    assert_eq!(paths.len(), 2, "one abandoned stage, one preserved root");
    assert_eq!(paths[0].kind, Preserved);
    assert_eq!(paths[1].kind, Staged);
    assert_eq!(paths[1].path, abandoned_path);
    assert!(paths.iter().all(|artifact| artifact.path.is_dir()));
    block_on(core.shutdown());

    let core = open(&scratch);
    assert_eq!(block_on(core.list_restore_artifacts()).unwrap(), paths);
    // This test acts as the user: a later refresh reflects manual removal.
    std::fs::remove_dir_all(&abandoned_path).unwrap();
    assert_eq!(block_on(core.list_restore_artifacts()).unwrap(), paths[..1]);
    block_on(core.shutdown());
}

const CUE: &str = "appearance.compose_cue";

fn cue(core: &Core) -> String {
    let settings = block_on(core.settings()).unwrap();
    settings
        .into_iter()
        .find(|setting| setting.key == CUE)
        .unwrap()
        .value
}

fn set_cue(core: &Core, value: &str) {
    block_on(core.set_setting(&core.new_idempotency_key(), CUE, value)).unwrap();
}

#[test]
fn receipt_preflight_validates_keys_and_refuses_other_commands_without_preparing() {
    let scratch = ScratchDir::new("service-restore-receipt");
    let core = open(&scratch);
    let key = "00000000-0000-4000-8000-0000000000f3";
    assert_eq!(block_on(core.restore_receipt(key)).unwrap(), None);
    assert!(matches!(
        block_on(core.restore_receipt("")),
        Err(CoreError::InvalidInput { .. })
    ));
    block_on(core.set_setting(key, CUE, "strong")).unwrap();
    assert!(matches!(
        block_on(core.restore_receipt(key)),
        Err(CoreError::Conflict { .. })
    ));
    assert!(block_on(core.list_restore_artifacts()).unwrap().is_empty());
    block_on(core.shutdown());
}

#[test]
fn a_prepared_generation_is_restored_under_a_key_once() {
    let scratch = ScratchDir::new("service-restore");
    let core = open(&scratch);
    set_cue(&core, "strong");
    let id = back_up(&core, "00000000-0000-4000-8000-0000000000e1");
    set_cue(&core, "standard");

    let key = "00000000-0000-4000-8000-0000000000e2";
    let prepared = block_on(core.prepare_restore(&id, unflagged())).unwrap();
    let Restored { core, receipt } = block_on(core.restore(key, prepared, unflagged())).unwrap();
    let receipt = receipt.unwrap();
    assert_eq!(receipt.command, "start_restore");
    assert_eq!(
        receipt.result_json,
        format!(r#"{{"generation_id":"{id}"}}"#)
    );
    assert_eq!(cue(&core), "strong");
    assert_eq!(
        block_on(core.restore_receipt(key)).unwrap(),
        Some(receipt.clone())
    );

    // The same key again: its receipt, and the store stays as it is.
    set_cue(&core, "standard");
    let prepared = block_on(core.prepare_restore(&id, unflagged())).unwrap();
    let Restored {
        core,
        receipt: replayed,
    } = block_on(core.restore(key, prepared, unflagged())).unwrap();
    assert_eq!(replayed.unwrap(), receipt);
    assert_eq!(cue(&core), "standard");
    block_on(core.shutdown());
}

#[test]
fn a_refused_restore_hands_the_core_and_the_preparation_back() {
    let scratch = ScratchDir::new("service-restore-refused");
    let core = open(&scratch);
    set_cue(&core, "strong");
    let id = back_up(&core, "00000000-0000-4000-8000-0000000000e3");
    set_cue(&core, "standard");
    let key = "00000000-0000-4000-8000-0000000000e4";
    let prepared = block_on(core.prepare_restore(&id, unflagged())).unwrap();

    // A raised flag, then no key at all.
    let raised = Arc::new(AtomicBool::new(true));
    let Err(RestoreFailure::Refused {
        cause,
        core,
        prepared,
    }) = block_on(core.restore(key, prepared, raised))
    else {
        panic!("a cancelled restore is refused");
    };
    assert!(matches!(cause, CoreError::Unavailable { .. }), "{cause:?}");
    let Err(RestoreFailure::Refused {
        cause,
        core,
        prepared,
    }) = block_on(core.restore("", *prepared, unflagged()))
    else {
        panic!("an empty key is refused");
    };
    assert!(matches!(cause, CoreError::InvalidInput { .. }), "{cause:?}");

    // The core runs on the same store, and what came back swaps in
    // under the key the refusal didn't spend.
    assert_eq!(cue(&core), "standard");
    assert_eq!(prepared.generation_id(), id);
    let Restored { core, receipt } = block_on(core.restore(key, *prepared, unflagged())).unwrap();
    receipt.unwrap();
    assert_eq!(cue(&core), "strong");
    block_on(core.shutdown());
}
