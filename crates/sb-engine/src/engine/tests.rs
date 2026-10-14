//! Tests for the engine (plan M0.7): the settings domain end to end
//! and the shutdown lifecycle. The engine's futures are
//! runtime-neutral, so the tests drive them with the test-support
//! executor while the engine's own runtime does the work.

use sb_test_support::ScratchDir;

use crate::{Engine, EngineError};

/// An engine over a fresh scratch data root, with its scratch dir
/// kept alive for the test.
fn scratch_engine(label: &str) -> (ScratchDir, Engine) {
    let scratch = ScratchDir::new(label);
    let engine = sb_test_support::block_on(Engine::open(scratch.path().join("store")))
        .expect("the engine opens");
    (scratch, engine)
}

/// A fresh engine reports every registered key at its default.
#[test]
fn settings_default_to_the_registry() {
    let (_scratch, engine) = scratch_engine("engine-defaults");
    let settings = sb_test_support::block_on(engine.settings()).unwrap();
    assert_eq!(settings.len(), 1, "the pilot's registry holds one key");
    assert_eq!(settings[0].key(), "appearance.compose_cue");
    assert_eq!(settings[0].value(), "standard");
    assert!(settings[0].is_default());
}

/// An unknown key and an unknown value are `InvalidInput`, each naming
/// its field.
#[test]
fn unknown_key_and_value_are_invalid_input() {
    let (_scratch, engine) = scratch_engine("engine-invalid-input");
    let key = engine.new_idempotency_key();

    match sb_test_support::block_on(engine.set_setting(&key, "no.such.setting", "standard")) {
        Err(EngineError::InvalidInput { field, .. }) => assert_eq!(field, "key"),
        err => panic!("unknown key must be InvalidInput, got {err:?}"),
    }
    match sb_test_support::block_on(engine.set_setting(&key, "appearance.compose_cue", "ultra")) {
        Err(EngineError::InvalidInput { field, .. }) => assert_eq!(field, "value"),
        err => panic!("unknown value must be InvalidInput, got {err:?}"),
    }
}

/// Setting then reading shows the stored value; a second set under a
/// fresh key bumps the revision, which the result JSON reports.
#[test]
fn set_then_settings_shows_it_and_revision_increments() {
    let (_scratch, engine) = scratch_engine("engine-set");

    let first = sb_test_support::block_on(engine.set_setting(
        &engine.new_idempotency_key(),
        "appearance.compose_cue",
        "strong",
    ))
    .unwrap();
    assert_eq!(
        first.result_json(),
        "{\"key\":\"appearance.compose_cue\",\"revision\":1}"
    );

    let settings = sb_test_support::block_on(engine.settings()).unwrap();
    assert_eq!(settings[0].value(), "strong");
    assert!(!settings[0].is_default());

    let second = sb_test_support::block_on(engine.set_setting(
        &engine.new_idempotency_key(),
        "appearance.compose_cue",
        "standard",
    ))
    .unwrap();
    assert_eq!(
        second.result_json(),
        "{\"key\":\"appearance.compose_cue\",\"revision\":2}"
    );
    let settings = sb_test_support::block_on(engine.settings()).unwrap();
    assert_eq!(settings[0].value(), "standard");
    // Set back to the default by an explicit write: the effective
    // value is the default again.
    assert!(settings[0].is_default());
}

/// A repeated idempotency key returns the first receipt without
/// running: setting `strong` with key K, then `standard` with the same
/// K, answers the first receipt and leaves the value `strong`.
#[test]
fn a_repeated_idempotency_key_returns_the_first_receipt() {
    let (_scratch, engine) = scratch_engine("engine-idempotent");
    let key = engine.new_idempotency_key();

    let first =
        sb_test_support::block_on(engine.set_setting(&key, "appearance.compose_cue", "strong"))
            .unwrap();
    let repeated =
        sb_test_support::block_on(engine.set_setting(&key, "appearance.compose_cue", "standard"))
            .unwrap();

    assert_eq!(first.result_json(), repeated.result_json());
    assert_eq!(first.created_at(), repeated.created_at());
    let settings = sb_test_support::block_on(engine.settings()).unwrap();
    assert_eq!(
        settings[0].value(),
        "strong",
        "the repeated key must not re-run"
    );
}

/// `shutdown` closes the store cleanly (spec/03 "Integrity"), which
/// the sb-store tests observe as the marker; here the observable is
/// that a fresh engine reopens the same data root and reads back what
/// the closed one wrote.
#[test]
fn shutdown_leaves_a_store_the_next_engine_reopens() {
    let (scratch, engine) = scratch_engine("engine-shutdown");
    sb_test_support::block_on(engine.set_setting(
        &engine.new_idempotency_key(),
        "appearance.compose_cue",
        "strong",
    ))
    .unwrap();
    sb_test_support::block_on(engine.shutdown()).unwrap();
    let state_db = scratch.path().join("store/state.sqlite");
    assert!(
        sb_test_support::store_meta_value(&state_db, "clean_shutdown_at").is_some(),
        "shutdown didn't record the clean-shutdown marker"
    );

    let engine = sb_test_support::block_on(Engine::open(scratch.path().join("store")))
        .expect("the closed store reopens");
    let settings = sb_test_support::block_on(engine.settings()).unwrap();
    assert_eq!(settings[0].value(), "strong");
    sb_test_support::block_on(engine.shutdown()).unwrap();
}

/// A store an engine still holds is `Locked` (spec/03: one process
/// owns the store), and the refused open ends its own runtime without
/// blocking the caller.
#[test]
fn a_second_engine_on_one_store_is_locked() {
    let (scratch, engine) = scratch_engine("engine-locked");
    match sb_test_support::block_on(Engine::open(scratch.path().join("store"))) {
        Err(EngineError::Locked) => {}
        err => panic!("expected Locked, got {err:?}"),
    }
    sb_test_support::block_on(engine.shutdown()).unwrap();
}

/// A stored `value_json` no registered value matches is `Corrupt`
/// (spec/02): never coerced, never silently dropped. Written through
/// the store's own settings helper, as a foreign app version would
/// have.
#[test]
fn a_stored_value_no_registry_value_matches_is_corrupt() {
    use sb_resources::{StoreError, WritePriority, upsert};

    let (_scratch, engine) = scratch_engine("engine-corrupt-value");
    let now = engine.resources().now();
    let written =
        sb_test_support::block_on(engine.resources().write(WritePriority::User, move |tx| {
            upsert(tx, "appearance.compose_cue", "\"ultra\"", now)
        }))
        .map_err(|e: StoreError| e.to_string())
        .expect("the raw upsert runs");
    assert_eq!(written, 1);

    match sb_test_support::block_on(engine.settings()) {
        Err(EngineError::Corrupt { detail }) => {
            assert!(detail.contains("appearance.compose_cue"), "{detail}");
        }
        err => panic!("expected Corrupt, got {err:?}"),
    }
}

/// A current-thread tokio runtime standing in for a shell's own async
/// context, where dropping the engine's runtime directly would panic.
fn outer_runtime() -> tokio::runtime::Runtime {
    tokio::runtime::Builder::new_current_thread()
        .build()
        .expect("an outer runtime")
}

/// Poll a future once with a waker that does nothing.
fn poll_once<F: std::future::Future>(future: std::pin::Pin<&mut F>) -> std::task::Poll<F::Output> {
    future.poll(&mut std::task::Context::from_waker(std::task::Waker::noop()))
}

/// Retry opening until the store's lease is free again: a dropped
/// engine closes its store in the background.
fn reopen_eventually(root: &std::path::Path) -> Engine {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    loop {
        match sb_test_support::block_on(Engine::open(root)) {
            Ok(engine) => return engine,
            Err(EngineError::Locked) if std::time::Instant::now() < deadline => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            Err(err) => panic!("the store never reopened: {err:?}"),
        }
    }
}

/// Dropping an engine, or an `open` or `shutdown` future midway, inside
/// another runtime's async context never panics, and the store's lease
/// is released afterwards.
#[test]
fn engines_and_their_futures_drop_safely_inside_async_code() {
    let scratch = ScratchDir::new("engine-async-drop");
    let root = scratch.path().join("store");
    let outer = outer_runtime();

    outer.block_on(async {
        let engine = Engine::open(&root).await.unwrap();
        drop(engine);
    });
    let engine = reopen_eventually(&root);
    sb_test_support::block_on(engine.shutdown()).unwrap();

    outer.block_on(async {
        let mut opening = std::pin::pin!(Engine::open(&root));
        let _ = poll_once(opening.as_mut());
    });
    let engine = reopen_eventually(&root);

    outer.block_on(async {
        let mut closing = std::pin::pin!(engine.shutdown());
        let _ = poll_once(closing.as_mut());
    });
    let engine = reopen_eventually(&root);
    sb_test_support::block_on(engine.shutdown()).unwrap();
}

/// Shutdown waits for a read whose caller stopped waiting: until that
/// read ends the store stays open, and afterwards shutdown closes it
/// cleanly (the marker is recorded, the lease released).
#[test]
fn shutdown_waits_for_a_cancelled_read() {
    let (scratch, engine) = scratch_engine("engine-cancelled-read");
    let (release, gate) = std::sync::mpsc::channel::<()>();
    let (started, running) = std::sync::mpsc::channel::<()>();
    {
        let mut read = std::pin::pin!(engine.read_through(move |_| {
            started.send(()).unwrap();
            let _ = gate.recv();
            Ok(())
        }));
        let _ = poll_once(read.as_mut());
        running
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the read never started");
    }
    // The read's caller is gone; its blocking task still runs.
    let releaser = std::thread::spawn(move || {
        std::thread::sleep(std::time::Duration::from_millis(200));
        release.send(()).unwrap();
    });
    sb_test_support::block_on(engine.shutdown()).expect("shutdown waited and closed cleanly");
    releaser.join().unwrap();

    let state_db = scratch.path().join("store/state.sqlite");
    assert!(sb_test_support::store_meta_value(&state_db, "clean_shutdown_at").is_some());
    let engine = sb_test_support::block_on(Engine::open(scratch.path().join("store")))
        .expect("the lease was released");
    sb_test_support::block_on(engine.shutdown()).unwrap();
}

/// The env var that makes this test binary a core that gets killed.
const KILLED_ROOT_ENV: &str = "SB_ENGINE_TEST_KILLED_ROOT";

/// Not a test: `a_killed_core_leaves_no_marker_and_is_checked` runs it
/// as a child. It opens a core, writes a setting and aborts, running no
/// destructor, as a crash or `kill -9` would.
#[test]
#[ignore = "spawned by a_killed_core_leaves_no_marker_and_is_checked"]
fn killed_core_child() {
    let root = std::env::var(KILLED_ROOT_ENV).expect("root env");
    let engine = sb_test_support::block_on(Engine::open(&root)).unwrap();
    sb_test_support::block_on(engine.set_setting(
        &engine.new_idempotency_key(),
        "appearance.compose_cue",
        "strong",
    ))
    .unwrap();
    std::process::abort();
}

/// A killed core leaves no clean-shutdown marker, so the next open runs
/// `quick_check`: damage it finds is `Corrupt`.
#[test]
fn a_killed_core_leaves_no_marker_and_is_checked() {
    let scratch = ScratchDir::new("engine-killed");
    let root = scratch.path().join("store");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args(["--exact", "engine::tests::killed_core_child", "--ignored"])
        .env(KILLED_ROOT_ENV, &root)
        .status()
        .unwrap();
    assert!(!status.success(), "the child should have died");

    let state_db = root.join("state.sqlite");
    assert_eq!(
        sb_test_support::store_meta_value(&state_db, "clean_shutdown_at"),
        None
    );
    sb_test_support::execute_sql(
        &state_db,
        "PRAGMA ignore_check_constraints = ON;
         INSERT INTO accounts (account_id, kind, provider, display_name, state, sort_order, created_at)
           VALUES ('bad', 'not-a-kind', 'local_archive', 'x', 'active', 0, 0);",
    );
    match sb_test_support::block_on(Engine::open(&root)) {
        Err(EngineError::Corrupt { detail }) => assert!(detail.contains("quick_check"), "{detail}"),
        other => panic!("expected Corrupt from the startup check, got {other:?}"),
    }
}

/// Dropping an engine inside async code doesn't wait for its store to
/// close: a read whose caller stopped waiting still runs, yet the drop
/// returns at once, and the lease is released after the read ends.
#[test]
fn dropping_an_engine_inside_async_code_never_blocks_on_its_store() {
    let scratch = ScratchDir::new("engine-drop-nonblocking");
    let root = scratch.path().join("store");
    let engine = sb_test_support::block_on(Engine::open(&root)).unwrap();
    let (release, gate) = std::sync::mpsc::channel::<()>();
    let (started, running) = std::sync::mpsc::channel::<()>();
    {
        let mut read = std::pin::pin!(engine.read_through(move |_| {
            started.send(()).unwrap();
            let _ = gate.recv();
            Ok(())
        }));
        let _ = poll_once(read.as_mut());
        running
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the read never started");
    }

    let (dropped, drops) = std::sync::mpsc::channel::<()>();
    let dropper = std::thread::spawn(move || {
        outer_runtime().block_on(async move {
            drop(engine);
            dropped.send(()).unwrap();
        });
    });
    // The read is still gated: a drop that waited for it would hang here.
    let returned = drops
        .recv_timeout(std::time::Duration::from_secs(10))
        .is_ok();
    release.send(()).unwrap();
    dropper.join().unwrap();
    assert!(
        returned,
        "dropping the engine waited for its store to close"
    );

    let engine = reopen_eventually(&root);
    sb_test_support::block_on(engine.shutdown()).unwrap();
}

/// A waker that reports, once, that it was woken.
struct Signal(std::sync::Mutex<Option<std::sync::mpsc::Sender<()>>>);

impl std::task::Wake for Signal {
    fn wake(self: std::sync::Arc<Self>) {
        if let Some(woken) = self.0.lock().unwrap().take() {
            let _ = woken.send(());
        }
    }
}

/// An `open` that finished on the blocking pool but whose result is
/// never polled, dropped inside async code, still closes its store
/// cleanly off the caller's thread: the clean-shutdown marker appears
/// (a bare store dropped inline would record none).
#[test]
fn a_finished_open_dropped_unpolled_closes_its_store() {
    let scratch = ScratchDir::new("engine-open-finished-dropped");
    let root = scratch.path().join("store");
    let (woken, wakes) = std::sync::mpsc::channel();
    let waker = std::task::Waker::from(std::sync::Arc::new(Signal(std::sync::Mutex::new(Some(
        woken,
    )))));

    outer_runtime().block_on(async {
        let mut opening = std::pin::pin!(Engine::open(&root));
        let first = opening
            .as_mut()
            .poll(&mut std::task::Context::from_waker(&waker));
        assert!(
            first.is_pending(),
            "the open finished before its first poll"
        );
        wakes
            .recv_timeout(std::time::Duration::from_secs(30))
            .expect("the open never finished");
        // Finished, not polled again: dropped here.
    });

    let state_db = root.join("state.sqlite");
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(30);
    while sb_test_support::store_meta_value(&state_db, "clean_shutdown_at").is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "the dropped open's store was never closed"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
    let engine = reopen_eventually(&root);
    sb_test_support::block_on(engine.shutdown()).unwrap();
}

/// The env var that makes this test binary the thread-refusal child.
#[cfg(target_os = "linux")]
const REFUSAL_ROOT_ENV: &str = "SB_ENGINE_TEST_REFUSAL_ROOT";

/// Not a test: `a_drop_needs_no_new_thread` runs it as a child, since
/// refusing threads affects the whole process. With a write still
/// running and the OS refusing new threads, dropping the engine inside
/// async code returns at once: the cleanup worker reserved at open
/// takes the store.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "spawned by a_drop_needs_no_new_thread"]
fn thread_refusal_child() {
    let root = std::env::var(REFUSAL_ROOT_ENV).expect("root env");
    let engine = sb_test_support::block_on(Engine::open(&root)).unwrap();
    let outer = outer_runtime();
    let (release, gate) = std::sync::mpsc::channel::<()>();
    let (started, running) = std::sync::mpsc::channel::<()>();
    // A write the caller stopped awaiting; the writer still runs it.
    drop(
        engine
            .resources()
            .write(sb_resources::WritePriority::User, move |_| {
                started.send(()).unwrap();
                let _ = gate.recv();
                Ok(())
            }),
    );
    running
        .recv_timeout(std::time::Duration::from_secs(30))
        .unwrap();

    // Started before threads are refused: opens the gate after 500 ms.
    let (dropped, drops) = std::sync::mpsc::channel::<()>();
    let releaser = std::thread::spawn(move || {
        let returned = drops
            .recv_timeout(std::time::Duration::from_millis(500))
            .is_ok();
        release.send(()).unwrap();
        returned
    });
    sb_test_support::with_new_threads_refused(|| {
        assert!(
            std::thread::Builder::new().spawn(|| ()).is_err(),
            "the OS didn't refuse a new thread"
        );
        outer.block_on(async move {
            drop(engine);
            dropped.send(()).unwrap();
        });
    });
    assert!(
        releaser.join().unwrap(),
        "with threads refused, dropping the engine waited for the writer"
    );
}

/// Dropping an engine never needs a new thread, so it never blocks an
/// async caller even when the OS refuses one (Linux, in a child process).
#[cfg(target_os = "linux")]
#[test]
fn a_drop_needs_no_new_thread() {
    let scratch = ScratchDir::new("engine-thread-refusal");
    let root = scratch.path().join("store");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "engine::tests::thread_refusal_child",
            "--ignored",
        ])
        .env(REFUSAL_ROOT_ENV, &root)
        .status()
        .unwrap();
    assert!(
        status.success(),
        "the thread-refusal child failed: {status}"
    );
    let engine = reopen_eventually(&root);
    sb_test_support::block_on(engine.shutdown()).unwrap();
}

/// The env var that makes this test binary the startup-refusal child.
#[cfg(target_os = "linux")]
const STARTUP_REFUSAL_ROOT_ENV: &str = "SB_ENGINE_TEST_STARTUP_REFUSAL_ROOT";

/// Not a test: `an_os_refusing_threads_fails_the_open_as_unavailable`
/// runs it as a child. With new threads refused, opening an engine
/// reports `Unavailable` instead of panicking.
#[cfg(target_os = "linux")]
#[test]
#[ignore = "spawned by an_os_refusing_threads_fails_the_open_as_unavailable"]
fn startup_refusal_child() {
    let root = std::env::var(STARTUP_REFUSAL_ROOT_ENV).expect("root env");
    sb_test_support::with_new_threads_refused(|| {
        assert!(
            std::thread::Builder::new().spawn(|| ()).is_err(),
            "the OS didn't refuse a new thread"
        );
        match sb_test_support::block_on(Engine::open(&root)) {
            Err(EngineError::Unavailable { .. }) => {}
            other => panic!("expected Unavailable, got {other:?}"),
        }
    });
}

/// Threads are a startup resource (spec/01): an OS that refuses them
/// fails the open as `Unavailable` (Linux, in a child process).
#[cfg(target_os = "linux")]
#[test]
fn an_os_refusing_threads_fails_the_open_as_unavailable() {
    let scratch = ScratchDir::new("engine-startup-refusal");
    let status = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "engine::tests::startup_refusal_child",
            "--ignored",
        ])
        .env(STARTUP_REFUSAL_ROOT_ENV, scratch.path().join("store"))
        .status()
        .unwrap();
    assert!(
        status.success(),
        "the startup-refusal child failed: {status}"
    );
}

/// A spawner that refuses, as an OS out of threads would.
fn refuse_open(_body: super::OpenBody) -> std::io::Result<()> {
    Err(std::io::Error::other("no threads left"))
}

/// An OS that refuses the open thread fails the open as `Unavailable`
/// at once (spec/01: threads are a startup resource), rather than
/// leaving the open queued forever, and the runtime it started steps
/// aside.
#[test]
fn a_refused_open_thread_is_unavailable() {
    let scratch = ScratchDir::new("engine-open-refused");
    let root = scratch.path().join("store");
    match sb_test_support::block_on(Engine::open_with(&root, refuse_open)) {
        Err(EngineError::Unavailable { reason }) => {
            assert!(reason.contains("open thread"), "{reason}")
        }
        other => panic!("expected Unavailable, got {other:?}"),
    }
    let engine = sb_test_support::block_on(Engine::open(&root)).unwrap();
    sb_test_support::block_on(engine.shutdown()).unwrap();
}
