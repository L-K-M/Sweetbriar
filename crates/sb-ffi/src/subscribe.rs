//! Subscriptions over FFI (spec/07 "Subscriptions", "Subscriptions over
//! FFI"): `subscribe` is a query that hands back a Rust-owned
//! `Subscription`, pulled with `next` under a fresh token per call.
//!
//! The object holds the service's subscription, never the core's lock,
//! so a pending `next` can't hold up `shutdown`; the engine wakes it
//! with `Unavailable` instead.

use std::sync::Arc;

use crate::api::Core;
use crate::cancel::{CancelToken, until_cancelled};
use crate::error::CoreError;

/// What a window's subscription reports (spec/07).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum WindowEvent {
    /// The window's rows changed; re-read at this generation.
    Invalidated { generation: u64 },
    /// Fallen too far behind: rebuild the id index, then the pages.
    ResetRequired,
}

/// One window's stream of events.
#[derive(uniffi::Object)]
pub struct Subscription {
    inner: sb_service::Subscription,
}

#[uniffi::export]
impl Subscription {
    /// The next event: at once when the window's source already moved
    /// past the last one, else when it does. One call at a time — a
    /// second concurrent call is `InvalidInput`. A cancelled call
    /// answers `Cancelled` and moves no mark, so the next call still
    /// sees what it missed.
    pub async fn next(&self, token: Arc<CancelToken>) -> Result<WindowEvent, CoreError> {
        let event = until_cancelled(&token, self.inner.next()).await??;
        Ok(match event {
            sb_service::WindowEvent::Invalidated { generation } => {
                WindowEvent::Invalidated { generation }
            }
            sb_service::WindowEvent::ResetRequired => WindowEvent::ResetRequired,
        })
    }
}

#[uniffi::export]
impl Core {
    /// `subscribe` (spec/07): the window's changes from what it last
    /// delivered. A query: the token races the core's lock. An unknown
    /// or closed window answers `NotFound` on the first `next`.
    pub async fn subscribe(
        &self,
        token: Arc<CancelToken>,
        window_id: u64,
    ) -> Result<Arc<Subscription>, CoreError> {
        let inner = self
            .query(&token, async |core| Ok(core.subscribe(window_id)))
            .await?;
        Ok(Arc::new(Subscription { inner }))
    }
}

#[cfg(test)]
mod tests {
    use std::pin::pin;
    use std::task::{Context, Waker};

    use super::*;
    use crate::api::{CoreConfig, open_core};
    use crate::windows::{Anchor, WindowSpec};
    use sb_test_support::ScratchDir;

    fn token() -> Arc<CancelToken> {
        CancelToken::new()
    }

    fn scratch_core(label: &str) -> (ScratchDir, Arc<Core>) {
        let scratch = ScratchDir::new(label);
        let config = CoreConfig {
            data_root: scratch.path().join("store").to_string_lossy().into_owned(),
            cache_root: scratch.path().join("cache").to_string_lossy().into_owned(),
        };
        let core = sb_test_support::block_on(open_core(token(), config)).unwrap();
        (scratch, core)
    }

    /// A synthetic window and its subscription.
    fn subscribed(core: &Core) -> (u64, Arc<Subscription>) {
        let window = sb_test_support::block_on(core.open_window(
            token(),
            WindowSpec::DebugSynthetic,
            Anchor::Top,
            10,
        ))
        .unwrap();
        let subscription =
            sb_test_support::block_on(core.subscribe(token(), window.window_id)).unwrap();
        (window.window_id, subscription)
    }

    /// A fill invalidates the window at its new generation; a closed
    /// window's subscription is `NotFound`.
    #[test]
    fn a_fill_invalidates_a_subscribed_window() {
        let (_scratch, core) = scratch_core("ffi-subscribe");
        let (window, subscription) = subscribed(&core);

        let generation = sb_test_support::block_on(core.debug_fill_synthetic(token(), 5)).unwrap();
        let event = sb_test_support::block_on(subscription.next(token())).unwrap();
        assert_eq!(event, WindowEvent::Invalidated { generation });

        sb_test_support::block_on(core.close_window(window));
        let err = sb_test_support::block_on(subscription.next(token())).unwrap_err();
        assert!(matches!(err, CoreError::NotFound { .. }), "{err}");

        sb_test_support::block_on(core.shutdown());
    }

    /// A cancelled `next` answers `Cancelled` and frees the one-call
    /// slot; the change it waited for is still reported afterwards.
    #[test]
    fn a_cancelled_next_frees_the_subscription() {
        let (_scratch, core) = scratch_core("ffi-subscribe-cancel");
        let (_window, subscription) = subscribed(&core);

        let cancelled = token();
        cancelled.cancel();
        let err = sb_test_support::block_on(subscription.next(cancelled)).unwrap_err();
        assert!(matches!(err, CoreError::Cancelled));

        let generation = sb_test_support::block_on(core.debug_fill_synthetic(token(), 5)).unwrap();
        let event = sb_test_support::block_on(subscription.next(token())).unwrap();
        assert_eq!(event, WindowEvent::Invalidated { generation });

        sb_test_support::block_on(core.shutdown());
    }

    /// A pending `next` holds nothing `shutdown` waits for, and
    /// shutdown wakes it with `Unavailable` (spec/07).
    #[test]
    fn shutdown_wakes_a_pending_next() {
        let (_scratch, core) = scratch_core("ffi-subscribe-shutdown");
        let (_window, subscription) = subscribed(&core);

        let mut pending = pin!(subscription.next(token()));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(pending.as_mut().poll(&mut cx).is_pending());

        sb_test_support::block_on(core.shutdown());
        let err = sb_test_support::block_on(pending).unwrap_err();
        assert!(matches!(err, CoreError::Unavailable { .. }), "{err}");

        let err = sb_test_support::block_on(core.subscribe(token(), 1))
            .err()
            .unwrap();
        assert!(matches!(err, CoreError::Unavailable { .. }), "{err}");
    }
}
