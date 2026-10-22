//! Subscriptions (spec/07 "Subscriptions"): pull-based invalidation
//! for one window. `next` waits on the window table's versions, which
//! the store's commit announcements and debug fills move, and answers
//! with the latest generation or, past the lag bound, `ResetRequired`.
//!
//! ```text
//!   writer commits g ─► Windows::store_moved(g) ─► versions ─┐
//!   debug fill       ─► versions.synthetic                   ├─► wake every next
//!   close / shutdown ─► versions.closes / shut                ┘
//!   next: mark (window's last page) < source generation ? event : wait
//! ```

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};

use tokio::runtime::Handle;

use super::{OpenWindow, Versions, WindowId, WindowSpec, Windows};
use crate::engine::join_failed;
use crate::{Engine, EngineError};

/// How many generations a consumer may fall behind before `next`
/// answers `ResetRequired` (spec/07).
pub(super) const MAX_UNSEEN_GENERATIONS: u64 = 32;

/// What a window's subscription reports (spec/07).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WindowEvent {
    /// The window's rows changed; re-read at this generation.
    Invalidated { generation: u64 },
    /// The consumer fell too far behind: rebuild the id index, then
    /// the pages.
    ResetRequired,
}

/// One window's stream of events, pulled with [`Subscription::next`].
pub struct Subscription {
    window_id: WindowId,
    windows: Arc<Windows>,
    runtime: Handle,
    /// The generation the last event named: at first, that of the last
    /// page or id index the window's caller had received when
    /// subscribing. `None` for a window not open then.
    mark: Mutex<Option<u64>>,
    waiting: AtomicBool,
}

impl Engine {
    /// `subscribe` (spec/07): a subscription to `window_id`'s changes,
    /// marked at the generation its caller last received. Only that
    /// in-memory mark is read here; no I/O.
    pub fn subscribe(&self, window_id: WindowId) -> Subscription {
        Subscription {
            window_id,
            windows: Arc::clone(self.windows()),
            runtime: self.runtime_handle(),
            mark: Mutex::new(self.windows().seen(window_id)),
            waiting: AtomicBool::new(false),
        }
    }
}

impl Subscription {
    /// Wait until the window's source moved past this subscription's
    /// mark, then name the latest generation, or `ResetRequired` once
    /// more than 32 generations passed (spec/07). Returns at
    /// once when the source already moved. One call at a time; a
    /// cancelled call moves nothing.
    pub async fn next(&self) -> Result<WindowEvent, EngineError> {
        let _waiting = Waiting::claim(&self.waiting)?;
        let mut versions = self.windows.versions.subscribe();
        loop {
            let seen = *versions.borrow_and_update();
            if seen.shut {
                return Err(shut_down());
            }
            let open = self.window().await?;
            let current = source_generation(&open.spec, seen);
            let mark = *self.lock_mark().get_or_insert(open.seen());
            if current > mark {
                *self.lock_mark() = Some(current);
                return Ok(if current - mark > MAX_UNSEEN_GENERATIONS {
                    WindowEvent::ResetRequired
                } else {
                    WindowEvent::Invalidated {
                        generation: current,
                    }
                });
            }
            versions.changed().await.map_err(|_| shut_down())?;
        }
    }

    /// The window as the table holds it, read on the blocking pool so
    /// the table's lock is never taken on the caller's thread (spec/07
    /// "One runtime").
    async fn window(&self) -> Result<OpenWindow, EngineError> {
        let windows = Arc::clone(&self.windows);
        let window_id = self.window_id;
        match self
            .runtime
            .spawn_blocking(move || windows.window(window_id))
            .await
        {
            Ok(open) => open,
            // A panic is a bug; a cancelled task means the runtime is gone.
            Err(join) if join.is_panic() => Err(join_failed("read the window table", join)),
            Err(_) => Err(shut_down()),
        }
    }

    fn lock_mark(&self) -> std::sync::MutexGuard<'_, Option<u64>> {
        self.mark
            .lock()
            .expect("a panicking thread held a subscription's mark")
    }
}

/// The generation of the source `spec` lists.
fn source_generation(spec: &WindowSpec, versions: Versions) -> u64 {
    match spec {
        WindowSpec::DebugSynthetic => versions.synthetic,
        WindowSpec::Search(_) => versions.search,
        _ => versions.store,
    }
}

fn shut_down() -> EngineError {
    EngineError::unavailable("the engine is shutting down")
}

/// The one `next` in flight; dropping it, as a cancelled `next` does,
/// frees the subscription for the next call.
struct Waiting<'a>(&'a AtomicBool);

impl<'a> Waiting<'a> {
    fn claim(flag: &'a AtomicBool) -> Result<Self, EngineError> {
        if flag.swap(true, Ordering::AcqRel) {
            return Err(EngineError::invalid_input(
                "next",
                "a subscription answers one next at a time",
            ));
        }
        Ok(Self(flag))
    }
}

impl Drop for Waiting<'_> {
    fn drop(&mut self) {
        self.0.store(false, Ordering::Release);
    }
}
