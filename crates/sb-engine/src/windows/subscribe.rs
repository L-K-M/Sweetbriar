//! Subscriptions (spec/07 "Subscriptions"): pull-based invalidation
//! for one window. `next` waits on the window table's versions, which
//! the store's commit announcements and debug fills move, and answers
//! with the latest generation or, past the lag bound, `ResetRequired`.
//!
//! ```text
//!   writer commits g ─► Windows::store_moved(g) ─► versions ─┐
//!   debug fill       ─► versions.synthetic                   ├─► wake every next
//!   close / shutdown ─► versions.closes / shut                ┘
//!   store swap (restore) ─► versions.restores ─► ResetRequired once
//!   store latch / its end ─► versions.health ─► wake health subscriptions
//!   next: mark (window's last page) < source generation ? event : wait
//! ```

use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
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
    /// The store swaps this subscription has answered.
    restores: AtomicU64,
    waiting: AtomicBool,
}

impl Engine {
    /// `subscribe` (spec/07): a subscription to `window_id`'s changes,
    /// marked at the generation its caller last received. Only that
    /// in-memory mark is read here; no I/O.
    pub fn subscribe(&self, window_id: WindowId) -> Subscription {
        let (mark, restores) = self.windows().subscription_start(window_id);
        Subscription {
            window_id,
            windows: Arc::clone(self.windows()),
            runtime: self.runtime_handle(),
            mark: Mutex::new(mark),
            restores: AtomicU64::new(restores),
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
            if versions.borrow_and_update().shut {
                return Err(shut_down());
            }
            let open = self.window().await?;
            // The versions as of after the window was read, so a swap
            // meanwhile is answered rather than a replaced generation.
            // Read without marking them seen: a change the window read
            // predates (a close) still wakes the wait below.
            let seen = *versions.borrow();
            if seen.shut {
                return Err(shut_down());
            }
            let current = source_generation(&open.spec, seen);
            // Every subscription takes the swap as answered; only a store
            // window owes a reset for it (the debug source is untouched).
            let swapped = answered_restore(&self.restores, seen);
            if swapped && !matches!(open.spec, WindowSpec::DebugSynthetic) {
                *self.lock_mark() = Some(current);
                return Ok(WindowEvent::ResetRequired);
            }
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

/// The appearance source's stream of events (spec/07): the same pull
/// contract as a window's [`Subscription`], except the caller supplies
/// the mark — there is no window to read it from.
pub struct AppearanceSubscription {
    windows: Arc<Windows>,
    mark: Mutex<u64>,
    /// The store swaps this subscription has answered.
    restores: AtomicU64,
    waiting: AtomicBool,
}

impl Engine {
    /// `subscribe_appearance` (spec/07): appearance changes, pulled
    /// with `next`, marked at `generation` — what the caller's last
    /// `appearance()` answer reported. In-memory only.
    pub fn subscribe_appearance(&self, generation: u64) -> AppearanceSubscription {
        AppearanceSubscription {
            windows: Arc::clone(self.windows()),
            mark: Mutex::new(generation),
            restores: AtomicU64::new(self.windows().restores()),
            waiting: AtomicBool::new(false),
        }
    }
}

impl AppearanceSubscription {
    /// Wait until the appearance source moved past the mark, then name
    /// the latest generation — or `ResetRequired` past 32, after which
    /// the caller re-reads `appearance()` anyway. One call at a time;
    /// a cancelled call moves nothing; shutdown ends a wait with
    /// `Unavailable`.
    pub async fn next(&self) -> Result<WindowEvent, EngineError> {
        let _waiting = Waiting::claim(&self.waiting)?;
        let mut versions = self.windows.versions.subscribe();
        loop {
            let seen = *versions.borrow_and_update();
            if seen.shut {
                return Err(shut_down());
            }
            let current = seen.appearance;
            if answered_restore(&self.restores, seen) {
                *self.lock_mark() = current;
                return Ok(WindowEvent::ResetRequired);
            }
            // A store's appearance generation never goes back, so a mark
            // past it was read from a store a swap replaced, even one
            // that swapped before this subscription began counting.
            if current < *self.lock_mark() {
                *self.lock_mark() = current;
                return Ok(WindowEvent::ResetRequired);
            }
            let mark = *self.lock_mark();
            if current > mark {
                *self.lock_mark() = current;
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

    fn lock_mark(&self) -> std::sync::MutexGuard<'_, u64> {
        self.mark
            .lock()
            .expect("a panicking thread held an appearance subscription's mark")
    }
}

/// Tells when the answer of `health()` changed (spec/07): the store
/// latched read-only on an integrity finding (spec/03 "Integrity"), or
/// a restore swapped in a store without one. The same pull contract as
/// the other subscriptions, except an event names nothing: the caller
/// reads `health()` again.
pub struct HealthSubscription {
    windows: Arc<Windows>,
    /// The health changes this subscription has answered.
    answered: AtomicU64,
    waiting: AtomicBool,
}

impl Engine {
    /// `subscribe_health` (spec/07), starting at the changes so far:
    /// subscribe first, then read `health()`, and no change is missed.
    /// In-memory only.
    pub fn subscribe_health(&self) -> HealthSubscription {
        HealthSubscription {
            windows: Arc::clone(self.windows()),
            answered: AtomicU64::new(self.windows().health_changes()),
            waiting: AtomicBool::new(false),
        }
    }
}

impl HealthSubscription {
    /// Wait until health changed since this subscription's last
    /// answer; several changes are one answer. One call at a time; a
    /// cancelled call moves nothing; shutdown ends a wait with
    /// `Unavailable`.
    pub async fn next(&self) -> Result<(), EngineError> {
        let _waiting = Waiting::claim(&self.waiting)?;
        let mut versions = self.windows.versions.subscribe();
        loop {
            let seen = *versions.borrow_and_update();
            if seen.shut {
                return Err(shut_down());
            }
            if self.answered.swap(seen.health, Ordering::AcqRel) != seen.health {
                return Ok(());
            }
            versions.changed().await.map_err(|_| shut_down())?;
        }
    }
}

/// Whether a store swap happened since this subscription last
/// answered one, recording that it now has. The swapped-in store's
/// generations may be lower than the mark, so a swap is never compared
/// as a generation.
fn answered_restore(restores: &AtomicU64, versions: Versions) -> bool {
    restores.swap(versions.restores, Ordering::AcqRel) != versions.restores
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
