//! Subscriptions (plan M1.3f, spec/07 "Subscriptions"): a flag change
//! wakes a pending `next`, a slow consumer gets `ResetRequired`, one
//! `next` at a time, and closes and shutdown end a wait.

use std::pin::pin;
use std::sync::mpsc;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

use sb_test_support::{ScratchDir, block_on};

use super::store::{import, mbox, rewrite, sql_error, value};
use crate::{Anchor, Engine, EngineError, WindowEvent, WindowSpec};

const PATIENCE: Duration = Duration::from_secs(60);

/// An engine holding one imported message, and its mailbox's window,
/// opened once the import's thread rebuild has published: that moves
/// the generation too.
fn listed(label: &str) -> (ScratchDir, Engine, crate::Window) {
    let scratch = ScratchDir::new(label);
    let engine = block_on(Engine::open(scratch.path().join("store"))).unwrap();
    import(
        &engine,
        mbox(
            scratch.path(),
            "Inbox.mbox",
            &[("Hello", Some("Thu, 15 Oct 2026 10:00:00 +0000"))],
        ),
    );
    let deadline = Instant::now() + PATIENCE;
    while engine
        .resources()
        .read(sb_resources::thread_revisions)
        .unwrap()
        .stale()
    {
        assert!(Instant::now() < deadline, "threads were never built");
        std::thread::sleep(Duration::from_millis(5));
    }
    let mailbox = value(
        &engine,
        "SELECT mailbox_id FROM mailboxes WHERE remote_name = 'Inbox'",
    );
    let window =
        block_on(engine.open_window(WindowSpec::Mailbox(mailbox), Anchor::Top, 10)).unwrap();
    (scratch, engine, window)
}

/// Toggle every occurrence's flag `times` times, one commit each, as a
/// later flag command would.
fn toggle_flags(engine: &Engine, times: usize) {
    for _ in 0..times {
        rewrite(engine, |tx| {
            tx.execute("UPDATE occurrences SET f_flagged = 1 - f_flagged", [])
                .map(|_| ())
                .map_err(sql_error)
        });
    }
}

/// Poll `future` with a no-op waker until it finishes or `PATIENCE`
/// runs out, sleeping between polls.
fn finish<F: std::future::Future>(future: std::pin::Pin<&mut F>) -> F::Output {
    let mut future = future;
    let mut cx = Context::from_waker(Waker::noop());
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
            return output;
        }
        assert!(Instant::now() < deadline, "the future never finished");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// A flag change commits a new generation and wakes a `next` already
/// waiting, which names it; the page read there shows the flag (plan
/// M1.3 acceptance).
#[test]
fn a_flag_change_wakes_a_pending_next() {
    let (_scratch, engine, window) = listed("subscribe-flag");
    let subscription = engine.subscribe(window.window_id());
    let mut next = pin!(subscription.next());
    let mut cx = Context::from_waker(Waker::noop());
    // Let the wait start: nothing has changed yet.
    for _ in 0..20 {
        assert!(next.as_mut().poll(&mut cx).is_pending());
        std::thread::sleep(Duration::from_millis(5));
    }
    toggle_flags(&engine, 1);
    let event = finish(next.as_mut()).unwrap();
    let WindowEvent::Invalidated { generation } = event else {
        panic!("expected Invalidated, got {event:?}");
    };
    assert_eq!(generation, window.generation() + 1);
    let page =
        block_on(engine.read_window(window.window_id(), generation, Anchor::Top, 10)).unwrap();
    assert!(page.rows()[0].flagged());
    block_on(engine.shutdown()).unwrap();
}

/// A change before the first `next` is reported at once: the mark
/// starts at the window's last page, not at the subscription.
#[test]
fn a_change_before_subscribing_is_not_missed() {
    let (_scratch, engine, window) = listed("subscribe-early");
    toggle_flags(&engine, 1);
    let subscription = engine.subscribe(window.window_id());
    assert_eq!(
        block_on(subscription.next()).unwrap(),
        WindowEvent::Invalidated {
            generation: window.generation() + 1
        }
    );
    block_on(engine.shutdown()).unwrap();
}

/// Up to 32 unseen generations coalesce into one `Invalidated` naming
/// the latest; more answer `ResetRequired`, never silence (plan M1.3
/// acceptance).
#[test]
fn a_slow_consumer_gets_reset_required() {
    let (_scratch, engine, window) = listed("subscribe-slow");
    let subscription = engine.subscribe(window.window_id());
    toggle_flags(&engine, 32);
    assert_eq!(
        block_on(subscription.next()).unwrap(),
        WindowEvent::Invalidated {
            generation: window.generation() + 32
        }
    );
    toggle_flags(&engine, 33);
    assert_eq!(
        block_on(subscription.next()).unwrap(),
        WindowEvent::ResetRequired
    );
    block_on(engine.shutdown()).unwrap();
}

/// One `next` at a time; closing the window ends a waiting one with
/// `NotFound`, and a cancelled one frees the subscription.
#[test]
fn closes_and_overlaps_end_or_refuse_a_next() {
    let (_scratch, engine, window) = listed("subscribe-close");
    let subscription = engine.subscribe(window.window_id());
    let mut cx = Context::from_waker(Waker::noop());
    {
        let mut waiting = pin!(subscription.next());
        assert!(waiting.as_mut().poll(&mut cx).is_pending());
        assert!(matches!(
            block_on(subscription.next()),
            Err(EngineError::InvalidInput { .. })
        ));
    }
    // The cancelled call freed the subscription.
    let mut waiting = pin!(subscription.next());
    assert!(waiting.as_mut().poll(&mut cx).is_pending());
    block_on(engine.close_window(window.window_id()));
    assert!(matches!(
        finish(waiting.as_mut()),
        Err(EngineError::NotFound { .. })
    ));
    block_on(engine.shutdown()).unwrap();
}

/// Shutdown ends a waiting `next` with `Unavailable`.
#[test]
fn shutdown_ends_a_waiting_next() {
    let (_scratch, engine, window) = listed("subscribe-shutdown");
    let subscription = engine.subscribe(window.window_id());
    let (sent, received) = mpsc::channel();
    let waiter = std::thread::spawn(move || {
        sent.send(block_on(subscription.next())).unwrap();
    });
    std::thread::sleep(Duration::from_millis(100));
    block_on(engine.shutdown()).unwrap();
    let ended = received.recv_timeout(PATIENCE).expect("next never ended");
    assert!(
        matches!(ended, Err(EngineError::Unavailable { .. })),
        "{ended:?}"
    );
    waiter.join().unwrap();
}

/// A debug fill invalidates synthetic windows, and only them.
#[test]
fn a_debug_fill_invalidates_synthetic_windows() {
    let (_scratch, engine, window) = listed("subscribe-synthetic");
    let filled = block_on(engine.debug_fill_synthetic(10)).unwrap();
    let synthetic =
        block_on(engine.open_window(WindowSpec::DebugSynthetic, Anchor::Top, 5)).unwrap();
    let refilled = block_on(engine.debug_fill_synthetic(20)).unwrap();
    assert!(refilled > filled);
    assert_eq!(
        block_on(engine.subscribe(synthetic.window_id()).next()).unwrap(),
        WindowEvent::Invalidated {
            generation: refilled
        }
    );
    // The store window's source did not move.
    let store = engine.subscribe(window.window_id());
    let mut cx = Context::from_waker(Waker::noop());
    let mut quiet = pin!(store.next());
    for _ in 0..20 {
        assert!(quiet.as_mut().poll(&mut cx).is_pending());
        std::thread::sleep(Duration::from_millis(5));
    }
    block_on(engine.shutdown()).unwrap();
}
