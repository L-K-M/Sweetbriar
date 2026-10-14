//! Cancellation (spec/07 "Threading, cancellation and FFI rules"):
//! the `CancelToken` object every async export takes, and the race
//! the cancellable calls are built from.
//!
//! The token is the only cancel path: UniFFI's generated Swift never
//! cancels the Rust future, so the Swift adapter wraps each call in
//! `withTaskCancellationHandler` and calls `cancel` when the task is
//! cancelled. The call then stops at its next wait with `Cancelled`.

use std::future::{Future, poll_fn};
use std::pin::pin;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::task::Poll;

use tokio::sync::Notify;

use crate::CoreError;

/// A `CancelToken` (spec/07): the FFI object the adapter makes per
/// call and cancels when the surrounding Swift task is cancelled.
/// The flag is the truth; the `Notify` wakes whoever is already
/// waiting. `cancel` is synchronous and in memory — it does no I/O.
#[derive(uniffi::Object)]
pub struct CancelToken {
    cancelled: AtomicBool,
    wake: Notify,
}

#[uniffi::export]
impl CancelToken {
    /// A fresh token; the adapter makes one per call.
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self {
            cancelled: AtomicBool::new(false),
            wake: Notify::new(),
        })
    }

    /// Cancel the call this token gates (spec/07): synchronous and in
    /// memory. Idempotent — a second `cancel` changes nothing.
    pub fn cancel(&self) {
        self.cancelled.store(true, Ordering::SeqCst);
        self.wake.notify_waiters();
    }

    /// Whether `cancel` ran.
    pub fn is_cancelled(&self) -> bool {
        self.cancelled.load(Ordering::SeqCst)
    }
}

impl CancelToken {
    /// Resolve when the token is cancelled — already cancelled counts.
    /// The `Notified` is enabled before the flag is read, so a `cancel`
    /// racing the first poll can't slip between the check and the wait:
    /// it either set the flag (seen here) or woke the enabled wait.
    pub(crate) async fn cancelled(&self) {
        let mut notified = pin!(self.wake.notified());
        notified.as_mut().enable();
        if self.is_cancelled() {
            return;
        }
        notified.await;
    }
}

/// `work` or `Cancelled`, whichever resolves first — a hand-written
/// select over two pinned futures, on `std` alone: UniFFI drives these
/// futures by polling them from Swift's tasks, so nothing
/// runtime-bound may sit underneath. The token polls first each time,
/// so a token cancelled before the call — or landing between the two
/// halves of a poll — wins over work that completed at the same time.
pub(crate) async fn until_cancelled<F, T>(token: &CancelToken, work: F) -> Result<T, CoreError>
where
    F: Future<Output = T>,
{
    let mut cancelled = pin!(token.cancelled());
    let mut work = pin!(work);
    poll_fn(move |cx| {
        if cancelled.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(CoreError::Cancelled));
        }
        work.as_mut().poll(cx).map(Ok)
    })
    .await
}

#[cfg(test)]
mod tests {
    use std::task::{Context, Waker};

    use super::*;

    /// `cancel` wakes a waiter that registered before it, and a waiter
    /// that starts after `cancel` returns at once (spec/07).
    #[test]
    fn cancel_wakes_a_registered_waiter_and_a_late_waiter_returns_at_once() {
        let token = CancelToken::new();
        let mut cx = Context::from_waker(Waker::noop());

        let mut waiting = pin!(token.cancelled());
        assert!(
            waiting.as_mut().poll(&mut cx).is_pending(),
            "the waiter returned before cancel"
        );
        token.cancel();
        assert!(
            matches!(waiting.as_mut().poll(&mut cx), Poll::Ready(())),
            "cancel didn't wake the waiter"
        );

        let mut late = pin!(token.cancelled());
        assert!(
            matches!(late.as_mut().poll(&mut cx), Poll::Ready(())),
            "a waiter starting after cancel didn't return at once"
        );
    }

    /// The race resolves `Cancelled` on an already-cancelled token
    /// without ever polling the work.
    #[test]
    fn until_cancelled_prefers_an_already_cancelled_token() {
        let token = CancelToken::new();
        token.cancel();
        let result = sb_test_support::block_on(until_cancelled(&token, async {
            panic!("the work ran under a cancelled token")
        }));
        assert!(matches!(result, Err(CoreError::Cancelled)));
    }

    /// Work that resolves first wins; the token stays dormant.
    #[test]
    fn until_cancelled_returns_completed_work() {
        let token = CancelToken::new();
        let result = sb_test_support::block_on(until_cancelled(&token, async { 41 + 1 }));
        assert_eq!(result.expect("the work completed"), 42);
        assert!(!token.is_cancelled());
    }
}
