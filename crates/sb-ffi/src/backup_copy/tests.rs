use std::pin::pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::{Context, Poll, Waker};

use sb_test_support::{ScratchDir, block_on, with_sqlite_write_lock};

use super::*;
use crate::{CoreConfig, open_core};

fn opened(scratch: &ScratchDir) -> Arc<Core> {
    block_on(open_core(
        CancelToken::new(),
        CoreConfig {
            data_root: scratch.path().join("store").to_string_lossy().into_owned(),
            cache_root: scratch.path().join("cache").to_string_lossy().into_owned(),
        },
    ))
    .unwrap()
}

struct Watched(Arc<AtomicBool>);
impl Drop for Watched {
    fn drop(&mut self) {
        self.0.store(true, Ordering::SeqCst);
    }
}

#[test]
fn cancelling_a_wait_keeps_its_future_and_the_next_waiter_gets_the_cached_answer() {
    let ready = Arc::new(tokio::sync::Notify::new());
    let dropped = Arc::new(AtomicBool::new(false));
    let (gate, watched) = (ready.clone(), Watched(dropped.clone()));
    let copy = BackupCopy {
        state: Mutex::new(CopyState::Waiting(Box::pin(async move {
            let _watched = watched;
            gate.notified().await;
            sb_service::BackupEnd::Complete {
                after: Some("retention warning".into()),
            }
        }))),
    };
    let token = CancelToken::new();
    let mut first = pin!(copy.finished(token.clone()));
    let mut second = pin!(copy.finished(CancelToken::new()));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(first.as_mut().poll(&mut cx).is_pending());
    assert!(second.as_mut().poll(&mut cx).is_pending());
    token.cancel();
    assert!(matches!(
        first.as_mut().poll(&mut cx),
        Poll::Ready(Err(CoreError::Cancelled))
    ));
    assert!(
        !dropped.load(Ordering::SeqCst),
        "a cancelled wait dropped the copy future"
    );
    ready.notify_one();
    let expected = BackupEnd::Complete {
        after: Some("retention warning".into()),
    };
    assert_eq!(block_on(second).unwrap(), expected.clone());
    assert!(dropped.load(Ordering::SeqCst));
    assert_eq!(
        block_on(copy.finished(CancelToken::new())).unwrap(),
        expected
    );
}

#[test]
fn all_end_variants_are_retained_and_an_already_cancelled_wait_consumes_nothing() {
    for (service, expected) in [
        (
            sb_service::BackupEnd::Complete { after: None },
            BackupEnd::Complete { after: None },
        ),
        (sb_service::BackupEnd::Stopped, BackupEnd::Stopped),
        (
            sb_service::BackupEnd::Failed("disk error".into()),
            BackupEnd::Failed {
                detail: "disk error".into(),
            },
        ),
    ] {
        let copy = BackupCopy {
            state: Mutex::new(CopyState::Waiting(Box::pin(async move { service }))),
        };
        let cancelled = CancelToken::new();
        cancelled.cancel();
        assert!(matches!(
            block_on(copy.finished(cancelled)),
            Err(CoreError::Cancelled)
        ));
        assert_eq!(
            block_on(copy.finished(CancelToken::new())).unwrap(),
            expected.clone()
        );
        assert_eq!(
            block_on(copy.finished(CancelToken::new())).unwrap(),
            expected
        );
    }
}

#[test]
fn native_start_returns_receipt_handle_and_replay_without_a_second_copy() {
    let scratch = ScratchDir::new("ffi-backup-start");
    let core = opened(&scratch);
    let cancelled = CancelToken::new();
    cancelled.cancel();
    assert!(matches!(
        block_on(core.start_backup(cancelled, "00000000-0000-4000-8000-000000000011".into())),
        Err(CoreError::Cancelled)
    ));
    let key = "00000000-0000-4000-8000-000000000012";
    let started = block_on(core.start_backup(CancelToken::new(), key.into())).unwrap();
    assert_eq!(started.receipt.command, "start_backup");
    assert_eq!(
        started.receipt.result_json,
        format!(r#"{{"generation_id":"{}"}}"#, started.generation_id)
    );
    let copy = started.copy.unwrap();
    assert_eq!(
        block_on(copy.finished(CancelToken::new())).unwrap(),
        BackupEnd::Complete { after: None }
    );
    let replayed = block_on(core.start_backup(CancelToken::new(), key.into())).unwrap();
    assert_eq!(replayed.receipt, started.receipt);
    assert_eq!(replayed.generation_id, started.generation_id);
    assert!(replayed.copy.is_none());
    let guard = block_on(core.service.read());
    let generations = block_on(guard.as_ref().unwrap().list_backups()).unwrap();
    assert_eq!(
        generations.len(),
        1,
        "pre-cancelled command spent no key or row"
    );
    assert_eq!(generations[0].generation_id, started.generation_id);
    assert_eq!(generations[0].state, sb_service::BackupState::Complete);
    drop(guard);
    block_on(core.shutdown());
    assert_eq!(
        block_on(copy.finished(CancelToken::new())).unwrap(),
        BackupEnd::Complete { after: None }
    );
    assert!(matches!(
        block_on(core.start_backup(CancelToken::new(), key.into())),
        Err(CoreError::Unavailable { .. })
    ));
}

#[test]
fn a_begun_backup_answers_its_receipt_despite_cancellation() {
    let scratch = ScratchDir::new("ffi-backup-begun");
    let core = opened(&scratch);
    let token = CancelToken::new();
    let mut start =
        pin!(core.start_backup(token.clone(), "00000000-0000-4000-8000-000000000013".into()));
    let mut cx = Context::from_waker(Waker::noop());
    with_sqlite_write_lock(&scratch.path().join("store/state.sqlite"), || {
        assert!(start.as_mut().poll(&mut cx).is_pending());
        token.cancel();
    });
    let started = block_on(start).expect("a begun command still returns its receipt");
    assert_eq!(started.receipt.command, "start_backup");
    assert_eq!(
        block_on(started.copy.unwrap().finished(CancelToken::new())).unwrap(),
        BackupEnd::Complete { after: None }
    );
    block_on(core.shutdown());
}
