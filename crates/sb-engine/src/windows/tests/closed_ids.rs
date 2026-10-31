//! Closed-window `window_ids` cancellation (plan M1.6r4c4, spec/07
//! "The id index"): closing an ordinary or Search window stops its
//! admitted id-index read — `NotFound`, never partial ids, no
//! observed-generation mark — while other windows, page reads, writers
//! and the pooled reader it borrowed are untouched. The hooks only
//! pause an admitted read inside its transaction; they hand it no data.

use std::future::Future;
use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::task::{Context, Waker};
use std::time::{Duration, Instant};

use sb_resources::{StoreError, WritePriority};
use sb_test_support::{ScratchDir, block_on};

use super::search::{indexed, spec};
use super::store::{import, mbox, rewrite, sql_error, value};
use super::*;
use crate::SearchSort;
use crate::index::Event;

const PATIENCE: Duration = Duration::from_secs(30);

fn hook(engine: &Engine, body: impl Fn(Event) + Send + Sync + 'static) {
    *engine.indexer().hook.lock().unwrap() = Some(Arc::new(body));
}

/// Pause the first `event` inside its read transaction: `paused` says
/// the read is admitted and waiting, `release` lets it finish.
pub(super) fn hold_on(engine: &Engine, event: Event) -> (mpsc::Receiver<()>, mpsc::SyncSender<()>) {
    let (captured, paused) = mpsc::channel();
    let (release, released) = mpsc::sync_channel(0);
    let released = Mutex::new(released);
    let once = AtomicUsize::new(0);
    hook(engine, move |value| {
        if value == event && once.fetch_add(1, Ordering::SeqCst) == 0 {
            captured.send(()).unwrap();
            released.lock().unwrap().recv_timeout(PATIENCE).unwrap();
        }
    });
    (paused, release)
}

/// An engine over an imported `n`-message mbox; answers the Inbox's id.
pub(super) fn mailboxed(label: &str, n: usize) -> (ScratchDir, Engine, i64) {
    let scratch = ScratchDir::new(label);
    let engine = block_on(Engine::open(scratch.path().join("store"))).unwrap();
    let subjects: Vec<String> = (0..n).map(|i| format!("subject {i}")).collect();
    let messages: Vec<(&str, Option<&str>)> = subjects.iter().map(|s| (s.as_str(), None)).collect();
    import(&engine, mbox(scratch.path(), "Inbox.mbox", &messages));
    let mailbox = value(
        &engine,
        "SELECT mailbox_id FROM mailboxes WHERE remote_name = 'Inbox'",
    );
    (scratch, engine, mailbox)
}

/// Closing a window whose `window_ids` is already admitted — paused
/// inside its read transaction — ends the read's SQL work: the answer
/// is `NotFound`, never ids for a window that no longer exists.
/// Ordinary (list) windows and both Search sorts close the same way.
#[test]
fn closing_a_window_mid_ids_read_answers_not_found() {
    for fixture in ["mailbox", "date", "relevance"] {
        let (_scratch, engine, window, event) = match fixture {
            "mailbox" => {
                let (scratch, engine, mailbox) = mailboxed("closed-ids-mailbox", 40);
                let window =
                    block_on(engine.open_window(WindowSpec::Mailbox(mailbox), Anchor::Top, 10))
                        .unwrap();
                (scratch, engine, window, Event::IdsReadCaptured)
            }
            sort => {
                let (scratch, engine) = indexed("closed-ids-search", 40);
                let window = block_on(engine.open_window(
                    spec(
                        r#"{"text":"alpha"}"#,
                        match sort {
                            "date" => SearchSort::Date,
                            _ => SearchSort::Relevance,
                        },
                    ),
                    Anchor::Top,
                    10,
                ))
                .unwrap();
                (scratch, engine, window, Event::SearchReadCaptured)
            }
        };
        let engine = Arc::new(engine);
        let (paused, release) = hold_on(&engine, event);
        let window_id = window.window_id();
        let retained = engine.windows().window(window_id).unwrap();
        let opening_seen = retained.seen();
        // Give the IDs read a newer generation it must not mark as delivered.
        rewrite(&engine, |tx| {
            tx.execute("UPDATE occurrences SET f_flagged = 1 - f_flagged", [])
                .map(|_| ())
                .map_err(sql_error)
        });
        let key = match fixture {
            "mailbox" => {
                "SELECT CAST(value AS INTEGER) FROM store_meta WHERE key = 'list_generation'"
            }
            _ => "SELECT CAST(value AS INTEGER) FROM store_meta WHERE key = 'search_generation'",
        };
        let current: u64 = value(&engine, key).try_into().unwrap();
        assert!(current > opening_seen);
        let request = Arc::clone(&engine);
        let read = std::thread::spawn(move || block_on(request.window_ids(window_id)));
        paused.recv_timeout(PATIENCE).unwrap();
        assert_eq!(retained.seen(), opening_seen);
        block_on(engine.close_window(window_id));
        release.send(()).unwrap();
        match read.join().unwrap() {
            Err(EngineError::NotFound { .. }) => {}
            other => panic!("{fixture}: a mid-read close must stop window_ids, got {other:?}"),
        }
        assert_eq!(
            retained.seen(),
            opening_seen,
            "a stopped read moved its mark"
        );
        block_on(Arc::try_unwrap(engine).unwrap().shutdown()).unwrap();
    }
}

/// One window's stop can't reach another window's read: while a first
/// window's ids read is held, a second window over the same source
/// completes its ids read, a page read and a writer commit; after the
/// close and release, the stopped read is `NotFound` and the second
/// window's next ids read — served by the pooled reader the stopped
/// read returned — still completes.
#[test]
fn one_windows_stop_leaves_everyone_else_alone() {
    let (_scratch, engine, mailbox) = mailboxed("closed-ids-isolation", 40);
    let engine = Arc::new(engine);
    let spec = WindowSpec::Mailbox(mailbox);
    let first = block_on(engine.open_window(spec.clone(), Anchor::Top, 10)).unwrap();
    let second = block_on(engine.open_window(spec, Anchor::Top, 10)).unwrap();
    let (paused, release) = hold_on(&engine, Event::IdsReadCaptured);
    let first_id = first.window_id();
    let second_id = second.window_id();
    let request = Arc::clone(&engine);
    let read = std::thread::spawn(move || block_on(request.window_ids(first_id)));
    paused.recv_timeout(PATIENCE).unwrap();

    // Another window's ids read, a page read and a user write all run
    // while the first window's read is held: no signal reaches them.
    let ids = block_on(engine.window_ids(second_id)).unwrap();
    assert_eq!(ids.ids().len(), 40);
    let page = block_on(engine.read_window(second_id, ids.generation(), Anchor::Top, 5)).unwrap();
    assert_eq!(page.rows().len(), 5);
    block_on(engine.resources().write(WritePriority::User, |tx| {
        tx.execute_batch("UPDATE occurrences SET f_seen = f_seen")
            .map_err(|error| StoreError::internal(error.to_string()))
    }))
    .unwrap();

    block_on(engine.close_window(first_id));
    release.send(()).unwrap();
    match read.join().unwrap() {
        Err(EngineError::NotFound { .. }) => {}
        other => panic!("the closed window's read must be NotFound, got {other:?}"),
    }
    // The pooled reader the stopped read returned carries no leftover
    // handler: the next ids read, its likely borrower, is clean.
    let ids = block_on(engine.window_ids(second_id)).unwrap();
    assert_eq!(ids.ids().len(), 40);
    block_on(Arc::try_unwrap(engine).unwrap().shutdown()).unwrap();
}

/// Dropping a claimed open whose ids read was already admitted stops it
/// too: the abandoned open's cleanup closes the window, raising the
/// same signal an explicit close does.
#[test]
fn a_dropped_open_stops_its_admitted_ids_read() {
    let (_scratch, engine) = indexed("closed-ids-dropped", 40);
    let engine = Arc::new(engine);
    let (registered, got_id) = mpsc::channel();
    let (captured, paused) = mpsc::channel();
    let (release_registered, registered_wait) = mpsc::sync_channel::<()>(0);
    let (release_read, read_wait) = mpsc::sync_channel::<()>(0);
    let (registered_wait, read_wait) = (Mutex::new(registered_wait), Mutex::new(read_wait));
    // The open's own first page captures `SearchReadCaptured` before
    // registration; only a capture after it is the ids read's.
    let seen_registered = Arc::new(AtomicBool::new(false));
    let gate = Arc::clone(&seen_registered);
    hook(&engine, move |value| match value {
        Event::SearchRegistered { window_id } => {
            gate.store(true, Ordering::SeqCst);
            registered.send(window_id).unwrap();
            registered_wait
                .lock()
                .unwrap()
                .recv_timeout(PATIENCE)
                .unwrap();
        }
        Event::SearchReadCaptured if gate.load(Ordering::SeqCst) => {
            captured.send(()).unwrap();
            read_wait.lock().unwrap().recv_timeout(PATIENCE).unwrap();
        }
        _ => {}
    });
    let request = Arc::clone(&engine);
    let opening = engine.runtime_handle().spawn(async move {
        request
            .open_window(
                spec(r#"{"text":"alpha"}"#, SearchSort::Date),
                Anchor::Top,
                10,
            )
            .await
    });
    let window_id = WindowId::from(got_id.recv_timeout(PATIENCE).unwrap());
    // The window is registered while its open is still held: an ids
    // read admits itself against it.
    let request = Arc::clone(&engine);
    let read = std::thread::spawn(move || block_on(request.window_ids(window_id)));
    paused.recv_timeout(PATIENCE).unwrap();
    opening.abort();
    release_registered.send(()).unwrap();
    let deadline = Instant::now() + PATIENCE;
    while engine.windows().lock().open.contains_key(&window_id) {
        assert!(
            Instant::now() < deadline,
            "the dropped open's window was never closed"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    release_read.send(()).unwrap();
    match read.join().unwrap() {
        Err(EngineError::NotFound { .. }) => {}
        other => panic!("a dropped open's cleanup must stop the read, got {other:?}"),
    }
    block_on(Arc::try_unwrap(engine).unwrap().shutdown()).unwrap();
}

/// An ids read still in flight when its caller goes away keeps running
/// — but shutdown counts it and drains it: `shutdown` does not close
/// the store underneath it.
#[test]
fn an_ids_read_in_flight_drains_before_shutdown() {
    let (_scratch, engine, mailbox) = mailboxed("closed-ids-drain", 40);
    let (paused, release) = hold_on(&engine, Event::IdsReadCaptured);
    let mut context = Context::from_waker(Waker::noop());
    let mut read = Box::pin(
        engine.window_ids(
            block_on(engine.open_window(WindowSpec::Mailbox(mailbox), Anchor::Top, 10))
                .unwrap()
                .window_id(),
        ),
    );
    let deadline = Instant::now() + PATIENCE;
    loop {
        assert!(read.as_mut().poll(&mut context).is_pending());
        if paused.try_recv().is_ok() {
            break;
        }
        assert!(
            Instant::now() < deadline,
            "the ids read never reached its transaction"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
    // The caller went away; the admitted read still counts as work.
    drop(read);
    let closer = std::thread::spawn(move || block_on(engine.shutdown()));
    std::thread::sleep(Duration::from_millis(200));
    assert!(
        !closer.is_finished(),
        "shutdown closed the store under an in-flight read"
    );
    release.send(()).unwrap();
    closer.join().unwrap().unwrap();
}
