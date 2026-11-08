//! Operation deadlines for M2.2c1.
//!
//! Every command exchange is bounded by an absolute deadline: 30 s for
//! ordinary commands and a size-scaled 120 s through 498 s for a
//! full-body fetch. A deadline is never renewed: a body hint can only
//! move the original deadline later, never restart it. On expiry the
//! owned future is dropped and a typed [`ImapError::TimedOut`] is
//! returned. The connection driver then drops its retained socket and
//! unpublished sink. No partial observation is produced, nothing is flushed,
//! and nothing is published.

use std::future::Future;
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::Duration;

use crate::ImapError;

/// Fixed deadline for a non-body command exchange.
const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);

/// Seconds granted to a body fetch of at most [`BASE_BODY_BYTES`].
const BASE_BODY_TIMEOUT_SECS: u64 = 120;
/// The size covered by the base deadline.
const BASE_BODY_BYTES: u64 = 1_048_576;
/// One extra second is granted per additional 256 KiB.
const BODY_TIMEOUT_STEP_BYTES: u64 = 262_144;
/// The 100,000,000-byte message budget yields 498 s; never exceed it.
const MAX_BODY_TIMEOUT_SECS: u64 = 498;
/// The operation named in a body timeout.
const BODY_OPERATION: &str = "UID FETCH";

/// Bounds one ordinary command exchange at 30 s from the moment it
/// starts.
///
/// The future is pinned and owned here, so expiry drops it before the
/// typed error is returned. The driver owns and discards the socket. A ready
/// future is returned unchanged; a local refusal from the future is
/// propagated, not converted into a timeout.
pub(crate) async fn command<T>(
    operation: &'static str,
    future: impl Future<Output = Result<T, ImapError>>,
) -> Result<T, ImapError> {
    let mut future = std::pin::pin!(future);
    match tokio::time::timeout(COMMAND_TIMEOUT, &mut future).await {
        Ok(result) => result,
        Err(_) => Err(ImapError::TimedOut { operation }),
    }
}

/// A monotonic, bounded hint at the body size being streamed.
///
/// Producers call [`BodySizeHint::note`] as sizes are announced or
/// written. The stored value never decreases and never exceeds the
/// message budget, so the deadline it drives can only move later.
#[derive(Clone, Default)]
pub(crate) struct BodySizeHint(Arc<AtomicU64>);

impl BodySizeHint {
    /// Records a size hint.
    ///
    /// The hint is clamped to [`crate::body_framing::MAX_BODY_BYTES`]
    /// and combined with `fetch_max`, so a smaller later hint is
    /// ignored and an oversized hint saturates at the budget instead of
    /// overflowing.
    pub(crate) fn note(&self, bytes: u64) {
        let clamped = bytes.min(crate::body_framing::MAX_BODY_BYTES);
        self.0.fetch_max(clamped, Ordering::Relaxed);
    }

    fn current(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// The absolute deadline, in seconds from the original start, for a
/// body fetch whose largest announced size is `bytes`.
fn deadline_secs(bytes: u64) -> u64 {
    if bytes <= BASE_BODY_BYTES {
        return BASE_BODY_TIMEOUT_SECS;
    }
    let extra = bytes - BASE_BODY_BYTES;
    let steps = extra.div_ceil(BODY_TIMEOUT_STEP_BYTES);
    BASE_BODY_TIMEOUT_SECS
        .saturating_add(steps)
        .min(MAX_BODY_TIMEOUT_SECS)
}

/// Bounds one full-body exchange at its size-scaled deadline.
///
/// The deadline is absolute from the original start. When the current
/// timer expires, the hint is re-read: if the larger size puts the
/// allowance later than now, the same pinned future is rearmed at that
/// later absolute deadline. Hints only grow, so the old timer always
/// wakes no later than the new deadline and no channel or select is
/// needed. A hint that has not grown past the current deadline yields
/// [`ImapError::TimedOut`], dropping the owned future and everything it
/// captured.
pub(crate) async fn body<T>(
    hint: BodySizeHint,
    future: impl Future<Output = Result<T, ImapError>>,
) -> Result<T, ImapError> {
    let start = tokio::time::Instant::now();
    let mut future = std::pin::pin!(future);
    let mut deadline = start + Duration::from_secs(deadline_secs(hint.current()));
    loop {
        match tokio::time::timeout_at(deadline, &mut future).await {
            Ok(result) => return result,
            Err(_) => {
                let extended = start + Duration::from_secs(deadline_secs(hint.current()));
                if extended > tokio::time::Instant::now() {
                    deadline = extended;
                    continue;
                }
                return Err(ImapError::TimedOut {
                    operation: BODY_OPERATION,
                });
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicUsize;

    /// An owned value whose drops are counted, proving the future and
    /// its captures are released on timeout and cancellation.
    struct DropMarker(Arc<AtomicUsize>);

    impl Drop for DropMarker {
        fn drop(&mut self) {
            self.0.fetch_add(1, Ordering::SeqCst);
        }
    }

    fn drop_counter() -> (DropMarker, Arc<AtomicUsize>) {
        let drops = Arc::new(AtomicUsize::new(0));
        (DropMarker(drops.clone()), drops)
    }

    #[test]
    fn deadline_math_is_exact_and_the_hint_is_monotonic_and_clamped() {
        let cases = [
            (0, 120),
            (1, 120),
            (BASE_BODY_BYTES, 120),
            (BASE_BODY_BYTES + 1, 121),
            (BASE_BODY_BYTES + BODY_TIMEOUT_STEP_BYTES, 121),
            (BASE_BODY_BYTES + BODY_TIMEOUT_STEP_BYTES + 1, 122),
            (100_000_000, 498),
            (u64::MAX, 498),
        ];
        let mut previous = 0;
        for (bytes, seconds) in cases {
            assert_eq!(deadline_secs(bytes), seconds);
            assert!(seconds >= previous, "deadline shrank at {bytes}");
            previous = seconds;
        }

        let hint = BodySizeHint::default();
        assert_eq!(hint.current(), 0);
        hint.note(1);
        assert_eq!(hint.current(), 1);
        hint.note(0);
        assert_eq!(hint.current(), 1);
        hint.note(u64::MAX);
        assert_eq!(hint.current(), crate::body_framing::MAX_BODY_BYTES);
        hint.note(crate::body_framing::MAX_BODY_BYTES - 1);
        assert_eq!(hint.current(), crate::body_framing::MAX_BODY_BYTES);

        let clone = hint.clone();
        clone.note(42);
        assert_eq!(hint.current(), crate::body_framing::MAX_BODY_BYTES);
    }

    #[tokio::test(start_paused = true)]
    async fn command_times_out_at_30s_and_drops_the_future_once() {
        let start = tokio::time::Instant::now();
        let (marker, drops) = drop_counter();
        let result = command("NOOP", async move {
            let _marker = marker;
            std::future::pending::<Result<(), ImapError>>().await
        })
        .await;
        assert!(matches!(
            result,
            Err(ImapError::TimedOut { operation: "NOOP" })
        ));
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_secs(30));
        assert!(elapsed <= Duration::from_secs(31));
    }

    #[tokio::test(start_paused = true)]
    async fn body_base_timeout_is_120s_and_drops_the_future_once() {
        let start = tokio::time::Instant::now();
        let (marker, drops) = drop_counter();
        let result = body(BodySizeHint::default(), async move {
            let _marker = marker;
            std::future::pending::<Result<(), ImapError>>().await
        })
        .await;
        assert!(matches!(
            result,
            Err(ImapError::TimedOut {
                operation: "UID FETCH"
            })
        ));
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_secs(120));
        assert!(elapsed <= Duration::from_secs(121));
    }

    #[tokio::test(start_paused = true)]
    async fn body_hint_extends_from_the_original_start_without_renewing() {
        let start = tokio::time::Instant::now();
        let hint = BodySizeHint::default();
        let inner_hint = hint.clone();
        let begun = Arc::new(AtomicUsize::new(0));
        let inner_begun = begun.clone();
        let (marker, drops) = drop_counter();
        let result = body(hint, async move {
            inner_begun.fetch_add(1, Ordering::SeqCst);
            let _marker = marker;
            tokio::time::sleep(Duration::from_secs(119)).await;
            inner_hint.note(2 * 1024 * 1024);
            std::future::pending::<Result<(), ImapError>>().await
        })
        .await;
        assert!(matches!(
            result,
            Err(ImapError::TimedOut {
                operation: "UID FETCH"
            })
        ));
        assert_eq!(begun.load(Ordering::SeqCst), 1);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_secs(124));
        assert!(elapsed <= Duration::from_secs(125));
    }

    #[tokio::test(start_paused = true)]
    async fn success_and_error_propagate_before_timeout() {
        let ok = command("NOOP", async { Ok(7_u8) }).await;
        assert_eq!(ok.unwrap(), 7);
        let err: Result<(), ImapError> = command("NOOP", async { Err(ImapError::Closed) }).await;
        assert!(matches!(err, Err(ImapError::Closed)));

        let ok = body(BodySizeHint::default(), async { Ok(7_u8) }).await;
        assert_eq!(ok.unwrap(), 7);
        let err: Result<(), ImapError> =
            body(BodySizeHint::default(), async { Err(ImapError::Closed) }).await;
        assert!(matches!(err, Err(ImapError::Closed)));
    }

    #[tokio::test(start_paused = true)]
    async fn body_abort_after_first_poll_drops_the_owned_marker() {
        let begun = Arc::new(AtomicUsize::new(0));
        let inner_begun = begun.clone();
        let (marker, drops) = drop_counter();
        let handle = tokio::spawn(body(BodySizeHint::default(), async move {
            inner_begun.fetch_add(1, Ordering::SeqCst);
            let _marker = marker;
            std::future::pending::<Result<(), ImapError>>().await
        }));
        tokio::task::yield_now().await;
        tokio::task::yield_now().await;
        assert_eq!(begun.load(Ordering::SeqCst), 1);
        handle.abort();
        let _ = handle.await;
        assert_eq!(begun.load(Ordering::SeqCst), 1);
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }

    #[tokio::test(start_paused = true)]
    async fn maximal_and_trickle_hints_cannot_exceed_the_original_498s() {
        // A maximal hint before the call.
        let start = tokio::time::Instant::now();
        let hint = BodySizeHint::default();
        hint.note(crate::body_framing::MAX_BODY_BYTES);
        let result = body(hint, std::future::pending::<Result<(), ImapError>>()).await;
        assert!(matches!(
            result,
            Err(ImapError::TimedOut {
                operation: "UID FETCH"
            })
        ));
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_secs(498));
        assert!(elapsed <= Duration::from_secs(499));

        // A late, growing hint is still measured from the original start.
        let start = tokio::time::Instant::now();
        let hint = BodySizeHint::default();
        let inner_hint = hint.clone();
        let result = body(hint, async move {
            for _ in 0..100 {
                tokio::time::sleep(Duration::from_secs(1)).await;
                inner_hint.note(inner_hint.current() + 1_000_000);
            }
            std::future::pending::<Result<(), ImapError>>().await
        })
        .await;
        assert!(matches!(
            result,
            Err(ImapError::TimedOut {
                operation: "UID FETCH"
            })
        ));
        let elapsed = start.elapsed();
        assert!(elapsed >= Duration::from_secs(498));
        assert!(elapsed <= Duration::from_secs(499));
    }
}
