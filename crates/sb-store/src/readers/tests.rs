//! Acceptance tests for the reader pool (plan M0.6): the reader
//! pragmas, the refusal of writes, and waiting past the pool's size.

use std::sync::{Mutex, mpsc};
use std::thread;
use std::time::Duration;

use sb_test_support::{ScratchDir, real_fs};

use super::{READER_COUNT, Readers};
use crate::StoreError;
use crate::schema::bug;
use crate::store::READER_PRAGMAS;
use crate::testing::{reading, scratch_paths, test_seams};

/// 5. A reader answers `query_only = 1` and `wal_autocheckpoint = 0`
///    (set-and-read-back, spec/03), and a write through the pool
///    fails: SQLite refuses it through the read-only, query-only
///    connection.
#[test]
fn readers_are_query_only_and_never_checkpoint() {
    let (_scratch, paths) = scratch_paths("readers-pragmas");
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();

    let (query_only, autocheckpoint, busy_timeout) = reading(&store, |tx| {
        let query_only: i64 = tx
            .query_row("PRAGMA query_only", [], |row| row.get(0))
            .map_err(bug)?;
        let autocheckpoint: i64 = tx
            .query_row("PRAGMA wal_autocheckpoint", [], |row| row.get(0))
            .map_err(bug)?;
        let busy_timeout: i64 = tx
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .map_err(bug)?;
        Ok((query_only, autocheckpoint, busy_timeout))
    });
    assert_eq!(query_only, 1, "query_only is not ON on a reader");
    assert_eq!(autocheckpoint, 0, "a reader would checkpoint on its own");
    assert_eq!(busy_timeout, 5000);
    let read_only = reading(&store, |tx| tx.is_readonly("main").map_err(bug));
    assert!(read_only, "a reader connection was opened read-write");

    let err = store
        .read(|tx| {
            tx.execute_batch("CREATE TABLE readers_must_not_write (x INTEGER) STRICT;")
                .map_err(bug)?;
            Ok(())
        })
        .unwrap_err();
    assert!(
        err.to_string().to_lowercase().contains("readonly"),
        "a write through a reader did not fail as readonly: {err}"
    );
}

/// A pool of `READER_COUNT` readers over a closed fresh store, for
/// tests that look inside it.
fn pool(label: &str) -> (ScratchDir, Readers) {
    let (scratch, paths) = scratch_paths(label);
    crate::open(&paths, &test_seams(real_fs()))
        .unwrap()
        .close()
        .unwrap();
    let readers = Readers::open(paths.state_db(), READER_PRAGMAS).unwrap();
    (scratch, readers)
}

/// A read that fails or panics gives its reader back: the pool stays
/// `READER_COUNT` deep.
#[test]
fn a_failed_or_panicking_read_returns_its_reader() {
    let (_scratch, pool) = pool("readers-return");
    assert_eq!(pool.idle_count(), READER_COUNT);

    assert!(
        pool.read::<()>(|_| Err(StoreError::internal("injected")))
            .is_err()
    );
    assert_eq!(
        pool.idle_count(),
        READER_COUNT,
        "a failed read lost its reader"
    );

    let panicked = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        pool.read::<()>(|_| panic!("injected read panic"))
    }))
    .is_err();
    assert!(panicked, "the injected read panic did not propagate");
    assert_eq!(
        pool.idle_count(),
        READER_COUNT,
        "a panicking read lost its reader"
    );
}

/// 5. More concurrent reads than the pool holds all complete: while
///    `READER_COUNT` reads hold every reader, the others wait (none
///    fails, none runs), and each gets a reader once one is free. No
///    sleeps: the holders block on a channel the test releases.
#[test]
fn reads_beyond_the_pool_wait_for_a_free_reader() {
    let (_scratch, pool) = pool("readers-wait");
    const READS: usize = READER_COUNT * 2;
    const DEADLINE: Duration = Duration::from_secs(30);
    let (acquired, acquisitions) = mpsc::channel::<()>();
    let (release, released) = mpsc::channel::<()>();
    let released = Mutex::new(released);

    let (held_at_once, idle_while_held, results) = thread::scope(|scope| {
        let readers: Vec<_> = (0..READS)
            .map(|_| {
                let acquired = acquired.clone();
                let (pool, released) = (&pool, &released);
                scope.spawn(move || {
                    pool.read(|_| {
                        acquired.send(()).unwrap();
                        // A dropped sender (the test failed) ends the
                        // wait too, so nothing hangs.
                        let _ = released.lock().unwrap().recv();
                        Ok(())
                    })
                })
            })
            .collect();

        // Exactly `READER_COUNT` reads get a reader; the rest wait.
        let mut held = 0;
        while held < READER_COUNT && acquisitions.recv_timeout(DEADLINE).is_ok() {
            held += 1;
        }
        let idle = pool.idle_count();
        if acquisitions.try_recv().is_ok() {
            held += 1;
        }
        for _ in 0..READS {
            release.send(()).unwrap();
        }
        let results: Vec<_> = readers.into_iter().map(|r| r.join().unwrap()).collect();
        (held, idle, results)
    });

    assert_eq!(
        held_at_once, READER_COUNT,
        "reads didn't fill the pool, or overfilled it"
    );
    assert_eq!(idle_while_held, 0);
    assert!(results.iter().all(Result::is_ok), "a pooled read failed");
    assert_eq!(pool.idle_count(), READER_COUNT);
}

/// Each reader keeps a 64 MiB page cache (set-and-read-back, spec/03).
/// The reads nest, so each holds a different reader.
#[test]
fn readers_keep_a_64_mib_page_cache() {
    fn caches(pool: &Readers, left: usize, seen: &mut Vec<i64>) -> Result<(), StoreError> {
        if left == 0 {
            return Ok(());
        }
        pool.read(|tx| {
            seen.push(
                tx.query_row("PRAGMA cache_size", [], |row| row.get(0))
                    .map_err(bug)?,
            );
            caches(pool, left - 1, seen)
        })
    }

    let (_scratch, pool) = pool("readers-cache");
    let mut seen = Vec::new();
    caches(&pool, READER_COUNT, &mut seen).unwrap();
    assert_eq!(
        seen, [-65536; READER_COUNT],
        "a reader's page cache isn't 64 MiB"
    );
}

/// The pool lends the most recently returned reader, whose cache is
/// warm (spec/03). Each read tags its connection through
/// `busy_timeout`, a per-connection setting a query-only reader may
/// change: an inner read nested in an outer one returns first, so the
/// next read gets the outer's reader.
#[test]
fn the_pool_lends_the_most_recently_returned_reader() {
    let (_scratch, pool) = pool("readers-recent");
    let tag = |tx: &rusqlite::Transaction, ms: i64| -> Result<(), StoreError> {
        tx.query_row(&format!("PRAGMA busy_timeout = {ms}"), [], |_| Ok(()))
            .map_err(bug)
    };
    pool.read(|outer| {
        tag(outer, 1111)?;
        pool.read(|inner| tag(inner, 2222))
    })
    .unwrap();

    let next: i64 = pool
        .read(|tx| {
            tx.query_row("PRAGMA busy_timeout", [], |row| row.get(0))
                .map_err(bug)
        })
        .unwrap();
    assert_eq!(
        next, 1111,
        "the next read didn't get the last returned reader"
    );
}

/// The pool is exactly `READER_COUNT` deep (spec/01: default 4).
#[test]
fn reader_count_is_four() {
    assert_eq!(READER_COUNT, 4);
}
