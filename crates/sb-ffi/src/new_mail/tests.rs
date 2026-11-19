//! New-mail subscription over the exported surface (plan M2.7c4): it
//! waits with no arrivals, a cancelled pull moves nothing, a second
//! concurrent pull is `InvalidInput`, and shutdown ends a wait.

use std::future::Future;
use std::pin::{Pin, pin};
use std::task::{Context, Poll, Waker};

use sb_test_support::{ScratchDir, block_on};

use super::*;
use crate::api::{CoreConfig, open_core};

fn token() -> Arc<CancelToken> {
    CancelToken::new()
}

fn open(scratch: &ScratchDir) -> Arc<Core> {
    let config = CoreConfig {
        data_root: scratch.path().join("store").to_string_lossy().into_owned(),
        cache_root: scratch.path().join("cache").to_string_lossy().into_owned(),
    };
    block_on(open_core(token(), config)).unwrap()
}

/// Poll once with a no-op waker: `true` while the future waits.
fn pending(future: &mut Pin<&mut impl Future>) -> bool {
    let mut cx = Context::from_waker(Waker::noop());
    future.as_mut().poll(&mut cx).is_pending()
}

#[test]
fn a_cancelled_pull_moves_nothing_and_shutdown_ends_a_wait() {
    let scratch = ScratchDir::new("ffi-new-mail");
    let core = open(&scratch);
    let changes = block_on(core.subscribe_new_mail(token())).unwrap();
    let mut cx = Context::from_waker(Waker::noop());

    let waiting = token();
    let mut next = pin!(changes.next(waiting.clone()));
    assert!(pending(&mut next), "no arrival is pending");

    waiting.cancel();
    assert!(matches!(
        next.as_mut().poll(&mut cx),
        Poll::Ready(Err(CoreError::Cancelled))
    ));

    // The cancellation moved nothing: a fresh pull still waits.
    let mut again = pin!(changes.next(token()));
    assert!(pending(&mut again));

    block_on(core.shutdown());
    assert!(matches!(
        again.as_mut().poll(&mut cx),
        Poll::Ready(Err(CoreError::Unavailable { .. }))
    ));
}

#[test]
fn a_second_pull_while_one_waits_is_invalid_input() {
    let scratch = ScratchDir::new("ffi-new-mail-concurrent");
    let core = open(&scratch);
    let changes = block_on(core.subscribe_new_mail(token())).unwrap();
    let mut next = pin!(changes.next(token()));
    assert!(pending(&mut next));

    assert!(matches!(
        block_on(changes.next(token())),
        Err(CoreError::InvalidInput { .. })
    ));

    block_on(core.shutdown());
}
