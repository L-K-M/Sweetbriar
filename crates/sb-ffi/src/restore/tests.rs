use std::future::Future;
use std::pin::Pin;
use std::sync::atomic::Ordering;
use std::task::{Context, Waker};
use std::time::{Duration, Instant};

use sb_test_support::{ScratchDir, block_on};

use super::*;
use crate::{BackupEnd, CoreConfig, RestoreArtifactKind, open_core};
use probe::{Gate, Phase};

const BACKUP_KEY: &str = "00000000-0000-4000-8000-000000000041";
const RESTORE_KEY: &str = "00000000-0000-4000-8000-000000000042";
const CUE: &str = "appearance.compose_cue";

fn opened(scratch: &ScratchDir) -> Arc<Core> {
    let root = scratch.path().canonicalize().unwrap();
    block_on(open_core(
        CancelToken::new(),
        CoreConfig {
            data_root: root.join("data").to_str().unwrap().into(),
            cache_root: root.join("cache").to_str().unwrap().into(),
        },
    ))
    .unwrap()
}

fn set_cue(core: &Core, key: &str, value: &str) {
    block_on(core.set_setting(CancelToken::new(), key.into(), CUE.into(), value.into())).unwrap();
}

fn cue(core: &Core) -> String {
    block_on(core.settings(CancelToken::new()))
        .unwrap()
        .into_iter()
        .find(|s| s.key == CUE)
        .unwrap()
        .value
}

fn backup(core: &Core) -> String {
    let started = block_on(core.start_backup(CancelToken::new(), BACKUP_KEY.into())).unwrap();
    assert_eq!(
        block_on(started.copy.unwrap().finished(CancelToken::new())).unwrap(),
        BackupEnd::Complete { after: None }
    );
    started.generation_id
}

fn reach<F: Future>(mut work: Pin<&mut F>, gate: &Gate) {
    let deadline = Instant::now() + Duration::from_secs(10);
    let mut cx = Context::from_waker(Waker::noop());
    while !gate.reached.load(Ordering::SeqCst) {
        assert!(
            work.as_mut().poll(&mut cx).is_pending(),
            "restore ended before its gate"
        );
        assert!(Instant::now() < deadline, "restore did not reach its gate");
        std::thread::sleep(Duration::from_millis(1));
    }
}

#[test]
fn native_restore_swaps_and_replays_before_touching_a_now_missing_generation() {
    let scratch = ScratchDir::new("ffi-restore-replay");
    let core = opened(&scratch);
    set_cue(&core, "00000000-0000-4000-8000-000000000043", "strong");
    let id = backup(&core);
    set_cue(&core, "00000000-0000-4000-8000-000000000044", "standard");
    let RestoreOutcome::Restored { receipt } =
        block_on(core.start_restore(CancelToken::new(), RESTORE_KEY.into(), id.clone())).unwrap()
    else {
        panic!("a fresh restore must swap")
    };
    assert_eq!(receipt.command, "start_restore");
    assert_eq!(
        receipt.result_json,
        format!(r#"{{"generation_id":"{id}"}}"#)
    );
    assert_eq!(cue(&core), "strong");
    set_cue(&core, "00000000-0000-4000-8000-000000000045", "standard");
    let row = block_on(core.list_backups(CancelToken::new()))
        .unwrap()
        .into_iter()
        .find(|row| row.generation_id == id)
        .unwrap();
    // Deliberate manual fixture removal, not a product cleanup path.
    std::fs::remove_dir_all(row.location).unwrap();
    let before = block_on(core.list_restore_artifacts(CancelToken::new())).unwrap();
    let RestoreOutcome::Replayed { receipt: replayed } = block_on(core.start_restore(
        CancelToken::new(),
        RESTORE_KEY.into(),
        "different-and-missing".into(),
    ))
    .unwrap() else {
        panic!("a spent restore key must replay before preparation")
    };
    assert_eq!(replayed, receipt);
    assert_eq!(cue(&core), "standard");
    assert_eq!(
        block_on(core.list_restore_artifacts(CancelToken::new())).unwrap(),
        before
    );
    block_on(core.shutdown());
    assert!(matches!(
        block_on(core.start_restore(CancelToken::new(), RESTORE_KEY.into(), id)),
        Err(CoreError::Unavailable { .. })
    ));
}

#[test]
fn pre_cancelled_invalid_and_cross_command_keys_prepare_nothing() {
    let scratch = ScratchDir::new("ffi-restore-preflight");
    let core = opened(&scratch);
    let cancelled = CancelToken::new();
    cancelled.cancel();
    assert!(matches!(
        block_on(core.start_restore(cancelled, RESTORE_KEY.into(), "missing".into())),
        Err(CoreError::Cancelled)
    ));
    assert!(matches!(
        block_on(core.start_restore(CancelToken::new(), "".into(), "missing".into())),
        Err(CoreError::InvalidInput { .. })
    ));
    set_cue(&core, RESTORE_KEY, "strong");
    assert!(matches!(
        block_on(core.start_restore(CancelToken::new(), RESTORE_KEY.into(), "missing".into())),
        Err(CoreError::Conflict { .. })
    ));
    assert!(
        block_on(core.list_restore_artifacts(CancelToken::new()))
            .unwrap()
            .is_empty()
    );
    assert_eq!(cue(&core), "strong");
    block_on(core.shutdown());
}

#[test]
fn cancellation_does_not_abandon_begun_preparation_and_keeps_completed_stages() {
    for phase in [Phase::Preparing, Phase::Prepared] {
        let scratch = ScratchDir::new("ffi-restore-prepare-cancel");
        let core = opened(&scratch);
        let id = backup(&core);
        let gate = core.restore_probe.arm(phase);
        let token = CancelToken::new();
        let mut work = Box::pin(core.start_restore(token.clone(), RESTORE_KEY.into(), id));
        reach(work.as_mut(), &gate);
        token.cancel();
        // Even after cancellation, the begun preparation must reach its end.
        let mut cx = Context::from_waker(Waker::noop());
        assert!(
            work.as_mut().poll(&mut cx).is_pending(),
            "cancellation abandoned preparation"
        );
        gate.release();
        assert!(matches!(block_on(work), Err(CoreError::Cancelled)));
        assert_eq!(cue(&core), "standard");
        if phase == Phase::Prepared {
            let names = block_on(core.list_restore_artifacts(CancelToken::new())).unwrap();
            assert_eq!(names.len(), 1);
            assert_eq!(names[0].kind, RestoreArtifactKind::Staged);
            assert!(std::path::Path::new(&names[0].path).is_dir());
        }
        block_on(core.shutdown());
    }
}

#[test]
fn cancellation_after_taking_the_core_finishes_and_reinstalls_the_refused_core() {
    let scratch = ScratchDir::new("ffi-restore-consumed-cancel");
    let core = opened(&scratch);
    let id = backup(&core);
    let gate = core.restore_probe.arm(Phase::Taken);
    let token = CancelToken::new();
    let mut work = Box::pin(core.start_restore(token.clone(), RESTORE_KEY.into(), id));
    reach(work.as_mut(), &gate);
    token.cancel();
    let mut cx = Context::from_waker(Waker::noop());
    assert!(work.as_mut().poll(&mut cx).is_pending());
    gate.release();
    let result = block_on(work).unwrap();
    assert!(
        matches!(
            result,
            RestoreOutcome::Refused {
                cause: CoreError::Unavailable { .. }
            }
        ),
        "{result:?}"
    );
    assert_eq!(
        cue(&core),
        "standard",
        "the returned live core must not be lost"
    );
    let names = block_on(core.list_restore_artifacts(CancelToken::new())).unwrap();
    assert_eq!(names.len(), 1);
    assert_eq!(names[0].kind, RestoreArtifactKind::Staged);
    block_on(core.shutdown());
}

#[test]
fn a_key_spent_during_preparation_is_rechecked_before_consuming() {
    let scratch = ScratchDir::new("ffi-restore-late-key");
    let core = opened(&scratch);
    let id = backup(&core);
    let gate = core.restore_probe.arm(Phase::Prepared);
    let mut work = Box::pin(core.start_restore(CancelToken::new(), RESTORE_KEY.into(), id));
    reach(work.as_mut(), &gate);
    set_cue(&core, RESTORE_KEY, "strong");
    gate.release();
    assert!(matches!(block_on(work), Err(CoreError::Conflict { .. })));
    assert_eq!(cue(&core), "strong");
    block_on(core.shutdown());
}

#[test]
fn shutdown_can_win_between_preparation_and_consumption_without_losing_evidence() {
    let scratch = ScratchDir::new("ffi-restore-shutdown");
    let core = opened(&scratch);
    let id = backup(&core);
    let gate = core.restore_probe.arm(Phase::Prepared);
    let mut restore = Box::pin(core.start_restore(CancelToken::new(), RESTORE_KEY.into(), id));
    reach(restore.as_mut(), &gate);
    let mut shutdown = Box::pin(core.shutdown());
    let mut cx = Context::from_waker(Waker::noop());
    assert!(shutdown.as_mut().poll(&mut cx).is_pending());
    gate.release();
    assert!(restore.as_mut().poll(&mut cx).is_pending());
    block_on(shutdown);
    assert!(matches!(
        block_on(restore),
        Err(CoreError::Unavailable { .. })
    ));
    let names: Vec<_> = std::fs::read_dir(scratch.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    assert_eq!(
        names
            .iter()
            .filter(|name| name.to_str().unwrap().starts_with("data.restoring-"))
            .count(),
        1
    );
    assert!(
        !names
            .iter()
            .any(|name| name.to_str().unwrap().starts_with("data.preserved-"))
    );
    block_on(core.shutdown());
}

#[test]
fn receipt_failure_projection_preserves_the_returned_core_and_closed_has_none() {
    let scratch = ScratchDir::new("ffi-restore-result-projection");
    let core = opened(&scratch);
    let mut slot = block_on(core.service.write());
    let returned = slot.take().unwrap();
    let result = install_result(
        &mut slot,
        Ok(sb_service::Restored {
            core: returned,
            receipt: Err(sb_service::CoreError::Unavailable {
                reason: "receipt write failed".into(),
            }),
        }),
    );
    assert!(matches!(
        result,
        RestoreOutcome::ReceiptUnrecorded {
            cause: CoreError::Unavailable { .. }
        }
    ));
    assert!(slot.is_some());
    drop(slot);
    assert_eq!(cue(&core), "standard");
    block_on(core.shutdown());
    let mut slot = block_on(core.service.write());
    let result = install_result(
        &mut slot,
        Err(sb_service::RestoreFailure::Failed(
            sb_service::CoreError::Unavailable {
                reason: "closed during swap".into(),
            },
        )),
    );
    assert!(matches!(result, RestoreOutcome::Closed { .. }));
    assert!(slot.is_none());
    drop(slot);
    assert!(matches!(
        block_on(core.settings(CancelToken::new())),
        Err(CoreError::Unavailable { .. })
    ));
    block_on(core.shutdown());
}
