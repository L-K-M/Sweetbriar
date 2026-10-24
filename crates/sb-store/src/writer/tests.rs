//! Acceptance tests for the writer (plan M0.6): serialization across
//! threads, priority scheduling, rollback on error, the caught panic,
//! the idle checkpoint and shutdown.

use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::thread;
use std::time::Duration;

use sb_test_support::{ScratchDir, block_on, real_fs};

use super::*;
use crate::Store;
use crate::StoreError;
use crate::schema::{bug, run_migrations};
use crate::store::{Access, WRITER_PRAGMAS, open_connection};
use crate::testing::{crash_write, reading, scratch_paths, test_seams, writing};

mod maintenance;

/// A store over a fresh scratch root, with a one-row probe table for
/// the counter tests.
fn store_with_probe(label: &str) -> (ScratchDir, Store) {
    let (scratch, paths) = scratch_paths(label);
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    writing(&store, |tx| {
        tx.execute_batch(
            "CREATE TABLE writer_probe (n INTEGER NOT NULL) STRICT;
             INSERT INTO writer_probe VALUES (0);",
        )
        .map_err(bug)?;
        Ok(())
    });
    (scratch, store)
}

/// The probe table's single integer.
fn probe_count(store: &Store) -> i64 {
    reading(store, |tx| {
        tx.query_row("SELECT n FROM writer_probe", [], |row| row.get(0))
            .map_err(bug)
    })
}

/// 1. Writes from many threads are serialized: eight threads submit
///    fifty increments each through one `Arc<Store>`; an in-flight
///    count inside the closures proves no two ran at once, and the
///    final count is the full 400.
#[test]
fn writes_from_many_threads_are_serialized() {
    const THREADS: usize = 8;
    const PER_THREAD: usize = 50;

    let (_scratch, store) = store_with_probe("writer-serialized");
    let store = Arc::new(store);
    let in_flight = Arc::new(AtomicUsize::new(0));
    let max_in_flight = Arc::new(AtomicUsize::new(0));

    thread::scope(|scope| {
        for _ in 0..THREADS {
            let store = Arc::clone(&store);
            let in_flight = Arc::clone(&in_flight);
            let max_in_flight = Arc::clone(&max_in_flight);
            scope.spawn(move || {
                for _ in 0..PER_THREAD {
                    let in_flight = Arc::clone(&in_flight);
                    let max_in_flight = Arc::clone(&max_in_flight);
                    block_on(store.write(WritePriority::User, move |tx| {
                        let during = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
                        max_in_flight.fetch_max(during, Ordering::SeqCst);
                        tx.execute("UPDATE writer_probe SET n = n + 1", [])
                            .map_err(bug)?;
                        let count: i64 = tx
                            .query_row("SELECT n FROM writer_probe", [], |row| row.get(0))
                            .map_err(bug)?;
                        in_flight.fetch_sub(1, Ordering::SeqCst);
                        Ok(count)
                    }))
                    .expect("a serialized write failed");
                }
            });
        }
    });

    assert_eq!(
        max_in_flight.load(Ordering::SeqCst),
        1,
        "two write closures ran at once"
    );
    assert_eq!(probe_count(&store), (THREADS * PER_THREAD) as i64);
}

/// 2. Controlled scheduling: while a first `User` write blocks on a
///    channel, `Index` I, `Sync` S1, `Sync` S2 and `User` U are
///    queued; on release the order is first, U, S1, S2, I (spec/01).
#[test]
fn user_writes_overtake_queued_sync_and_index_writes() {
    let (_scratch, store) = store_with_probe("writer-priority");
    let store = Arc::new(store);
    let order: Arc<Mutex<Vec<&'static str>>> = Arc::new(Mutex::new(Vec::new()));

    // The first job signals it has the writer, then blocks on release.
    let (release, blocked) = mpsc::channel::<()>();
    let (announce, arrived) = mpsc::channel::<()>();
    let first_order = Arc::clone(&order);
    let first = store.write(WritePriority::User, move |tx| {
        first_order.lock().unwrap().push("first");
        tx.execute("UPDATE writer_probe SET n = n + 1", [])
            .map_err(bug)?;
        announce.send(()).expect("the test went away");
        blocked.recv().expect("the test went away");
        Ok(())
    });
    arrived
        .recv_timeout(Duration::from_secs(5))
        .expect("the first write never started");

    // Queue the rest while the writer is inside the first job: each
    // closure records itself when the writer finally runs it.
    let index_order = Arc::clone(&order);
    let index = store.write(WritePriority::Index, move |_| {
        index_order.lock().unwrap().push("I");
        Ok(())
    });
    let sync1_order = Arc::clone(&order);
    let sync1 = store.write(WritePriority::Sync, move |_| {
        sync1_order.lock().unwrap().push("S1");
        Ok(())
    });
    let sync2_order = Arc::clone(&order);
    let sync2 = store.write(WritePriority::Sync, move |_| {
        sync2_order.lock().unwrap().push("S2");
        Ok(())
    });
    let user_order = Arc::clone(&order);
    let user = store.write(WritePriority::User, move |_| {
        user_order.lock().unwrap().push("U");
        Ok(())
    });

    release.send(()).unwrap();
    for future in [first, user, sync1, sync2, index] {
        block_on(future).expect("a scheduled write failed");
    }
    assert_eq!(*order.lock().unwrap(), ["first", "U", "S1", "S2", "I"]);
}

/// 3. A closure's `Err` rolls back its writes; a panicking closure
///    resolves `Internal` and the next write succeeds.
#[test]
fn errors_roll_back_and_panics_are_caught() {
    let (_scratch, store) = store_with_probe("writer-rollback");

    // An Err after a write: the write must vanish with it.
    let err = block_on(store.write::<(), _>(WritePriority::User, |tx| {
        tx.execute("UPDATE writer_probe SET n = 1000", [])
            .map_err(bug)?;
        Err(StoreError::unavailable("injected failure"))
    }))
    .unwrap_err();
    assert!(matches!(err, StoreError::Unavailable { .. }), "{err:?}");
    assert_eq!(probe_count(&store), 0, "the rolled-back write is visible");

    // A panicking closure: Internal, rolled back, and the writer
    // keeps serving.
    let err = block_on(store.write::<(), _>(WritePriority::User, |tx| {
        tx.execute("UPDATE writer_probe SET n = 2000", [])
            .map_err(bug)?;
        panic!("injected panic");
        #[allow(unreachable_code)]
        Ok(())
    }))
    .unwrap_err();
    let StoreError::Internal { detail } = err else {
        panic!("expected Internal, got {err:?}");
    };
    assert!(detail.contains("injected panic"), "{detail}");
    assert_eq!(probe_count(&store), 0, "the panicked write committed");

    writing(&store, |tx| {
        tx.execute("UPDATE writer_probe SET n = n + 1", [])
            .map_err(bug)?;
        Ok(())
    });
    assert_eq!(
        probe_count(&store),
        1,
        "the writer did not survive the panic"
    );
}

/// 6. After a write and a drained queue, the idle checkpoint truncates
///    the WAL to zero bytes. The checkpoint runs after the future
///    resolves, so the test polls for it.
#[test]
fn an_idle_writer_truncates_the_wal() {
    let (scratch, paths) = scratch_paths("writer-checkpoint");
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    let wal = paths.data_root().join("state.sqlite-wal");

    writing(&store, |tx| {
        tx.execute_batch("CREATE TABLE writer_wal_probe (x INTEGER) STRICT;")
            .map_err(bug)?;
        Ok(())
    });

    let deadline = std::time::Instant::now() + Duration::from_secs(5);
    loop {
        let len = std::fs::metadata(&wal).map(|md| md.len()).unwrap_or(0);
        if len == 0 {
            break;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the WAL is still {} bytes after the queue drained",
            len
        );
        thread::sleep(Duration::from_millis(10));
    }
    drop(store);
    drop(scratch);
}

/// 7. Close finishes queued jobs, joins the thread and closes the last
///    connection, so the WAL is gone. A submit after close resolves
///    `Unavailable`.
#[test]
fn close_removes_the_wal_and_refuses_late_submits() {
    let (scratch, paths) = scratch_paths("writer-close");
    let writer = writer_over(&paths);
    block_on(writer.submit(WritePriority::User, |tx| {
        tx.execute_batch("CREATE TABLE writer_close_probe (x INTEGER) STRICT;")
            .map_err(bug)?;
        Ok(())
    }))
    .expect("the write before close failed");
    writer.close().unwrap();
    assert!(!paths.data_root().join("state.sqlite-wal").exists());

    let late = block_on(writer.submit(WritePriority::User, |_| Ok(())));
    assert!(
        matches!(late, Err(StoreError::Unavailable { .. })),
        "a late submit was not Unavailable: {late:?}"
    );
    drop(scratch);
}

/// 7, the race: submits from several threads while the writer closes;
/// every future resolves — `Ok` when the write ran, `Unavailable` when
/// close won the race — so none can hang.
#[test]
fn submits_racing_shutdown_resolve_never_hang() {
    let (scratch, paths) = scratch_paths("writer-race");
    let writer = Arc::new(writer_over(&paths));

    let stop = Arc::new(AtomicUsize::new(0));
    thread::scope(|scope| {
        for _ in 0..4 {
            let writer = Arc::clone(&writer);
            let stop = Arc::clone(&stop);
            scope.spawn(move || {
                while stop.load(Ordering::Relaxed) == 0 {
                    match block_on(writer.submit(WritePriority::User, |_| Ok(()))) {
                        Ok(()) | Err(StoreError::Unavailable { .. }) => {}
                        err => panic!("unexpected result racing shutdown: {err:?}"),
                    }
                }
            });
        }
        thread::sleep(Duration::from_millis(50));
        writer.close().unwrap();
        stop.store(1, Ordering::Relaxed);
    });
    drop(scratch);
}

/// A writer over a fresh database built exactly as `open` builds it,
/// for the tests that exercise the writer alone.
fn writer_over(paths: &crate::StorePaths) -> Writer {
    let seams = test_seams(real_fs());
    crate::store::create_layout(seams.fs(), paths).unwrap();
    let mut conn = open_connection(paths.state_db(), Access::ReadWrite, WRITER_PRAGMAS).unwrap();
    run_migrations(&mut conn, &seams, crate::schema::MIGRATIONS).unwrap();
    Writer::new(conn).unwrap()
}

/// A write closure runs inside `BEGIN IMMEDIATE`: before it executes
/// any SQL, the writer already holds the write lock, so another
/// connection can't begin a write.
#[test]
fn a_write_closure_holds_the_write_lock_from_its_start() {
    let (scratch, paths) = scratch_paths("writer-immediate");
    let writer = writer_over(&paths);
    let db = paths.state_db().to_path_buf();
    let other_could_write = block_on(writer.submit(WritePriority::User, move |_| {
        let other = rusqlite::Connection::open(&db).map_err(bug)?;
        other.busy_timeout(Duration::ZERO).map_err(bug)?;
        Ok(other.execute_batch("BEGIN IMMEDIATE; ROLLBACK;").is_ok())
    }))
    .unwrap();
    assert!(!other_could_write, "the closure ran without the write lock");
    writer.close().unwrap();
    drop(scratch);
}

/// A value whose drop panics. Returned by a write nobody awaits, it is
/// dropped on the writer thread, outside the closure's `catch_unwind`.
struct PanicOnDrop;

impl Drop for PanicOnDrop {
    fn drop(&mut self) {
        panic!("injected drop panic on the writer thread");
    }
}

/// Poll a future once, with a waker that does nothing.
fn poll_once<T>(future: &mut WriteFuture<T>) -> Poll<Result<T, StoreError>> {
    let mut cx = std::task::Context::from_waker(std::task::Waker::noop());
    std::pin::Pin::new(future).poll(&mut cx)
}

/// Wait (bounded) until the writer refuses new jobs.
fn wait_until_closed(writer: &Writer) {
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while !writer.is_closed() {
        assert!(
            std::time::Instant::now() < deadline,
            "the writer thread didn't end"
        );
        thread::sleep(Duration::from_millis(5));
    }
}

/// When the writer thread dies, the job queued behind the fatal one
/// resolves `Unavailable` instead of hanging, so does a later submit,
/// and `close` reports the death. The fatal job is gated, so its
/// future is gone before it returns and the writer thread drops the
/// result itself, deterministically.
#[test]
fn a_dead_writer_resolves_queued_and_later_jobs_unavailable() {
    let (scratch, paths) = scratch_paths("writer-death");
    let writer = writer_over(&paths);
    let (release, gate) = mpsc::channel::<()>();
    let doomed = writer.submit(WritePriority::User, move |_| {
        let _ = gate.recv();
        Ok(PanicOnDrop)
    });
    let mut queued = writer.submit(WritePriority::User, |_| Ok(()));
    drop(doomed);
    release.send(()).unwrap();

    wait_until_closed(&writer);
    // Joining the dead thread waits until it has abandoned its queue.
    assert!(writer.close().is_err(), "close hid the writer's death");
    assert!(
        matches!(
            poll_once(&mut queued),
            Poll::Ready(Err(StoreError::Unavailable { .. }))
        ),
        "the job queued behind the fatal one was never resolved"
    );
    let mut late = writer.submit(WritePriority::User, |_| Ok(()));
    assert!(
        matches!(
            poll_once(&mut late),
            Poll::Ready(Err(StoreError::Unavailable { .. }))
        ),
        "a submit after the writer died didn't resolve Unavailable at once"
    );
    drop(scratch);
}

/// Two concurrent `close` calls both wait for the writer thread: the
/// first holds the handle lock while it joins (so a second blocks
/// until the thread has ended), and both succeed with every queued
/// write run. A held write keeps the thread busy; a notification
/// from inside `close` replaces any timing.
#[test]
fn concurrent_closes_both_wait_for_the_writer_thread() {
    let (scratch, paths) = scratch_paths("writer-close-twice");
    let writer = writer_over(&paths);
    let (release, gate) = mpsc::channel::<()>();
    let held = writer.submit(WritePriority::User, move |_| {
        let _ = gate.recv();
        Ok(())
    });
    let queued = writer.submit(WritePriority::User, |_| Ok(()));
    let (joining, joins) = mpsc::channel();
    writer.on_joining(joining);

    let (locked_while_joining, results) = thread::scope(|scope| {
        let first = scope.spawn(|| writer.close());
        let announced = joins.recv_timeout(Duration::from_secs(30)).is_ok();
        let locked = announced && writer.handle_is_locked();
        let second = scope.spawn(|| writer.close());
        // Released whatever happens, so a failure can't hang the scope.
        release.send(()).unwrap();
        (locked, [first.join().unwrap(), second.join().unwrap()])
    });
    assert!(
        locked_while_joining,
        "close released the handle lock before joining the thread"
    );
    assert!(results.iter().all(Result::is_ok), "{results:?}");
    assert!(block_on(held).is_ok());
    assert!(block_on(queued).is_ok());
    drop(scratch);
}

/// A spawner that refuses, as an OS out of threads would.
fn refuse(_body: WriterBody) -> std::io::Result<JoinHandle<Result<(), StoreError>>> {
    Err(std::io::Error::other("no threads left"))
}

/// An OS that refuses the writer thread makes it `Unavailable`, and
/// the connection, dropped unstarted, doesn't checkpoint: WAL frames a
/// crash left stay in the WAL.
#[test]
fn a_refused_writer_thread_is_unavailable_and_checkpoints_nothing() {
    let (scratch, paths) = scratch_paths("writer-spawn-refused");
    crate::open(&paths, &test_seams(real_fs()))
        .unwrap()
        .close()
        .unwrap();
    crash_write(&paths, "INSERT INTO store_meta VALUES ('crash', 'frame');");
    let before = std::fs::read(paths.state_db()).unwrap();

    let conn = open_connection(paths.state_db(), Access::ReadWrite, WRITER_PRAGMAS).unwrap();
    assert!(matches!(
        Writer::new_with(conn, refuse),
        Err(StoreError::Unavailable { .. })
    ));
    assert_eq!(
        std::fs::read(paths.state_db()).unwrap(),
        before,
        "the unstarted writer's connection checkpointed"
    );
    drop(scratch);
}
