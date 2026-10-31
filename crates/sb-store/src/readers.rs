//! The reader pool (spec/03 "Connections"; spec/01 "Store threads"):
//! a small set of read-only connections from the one connection
//! constructor, each serving one query as one short transaction — a
//! query window is never a long-lived read transaction (spec/07).
//! Readers set `wal_autocheckpoint=0` and `query_only=ON`, and never
//! checkpoint on close, so they can't checkpoint or write anything.
//! Each keeps its own page cache, so the pool lends the most recently
//! returned reader: consecutive reads find a warm cache (M1.6r4c).
//! A commit between two reads empties it: WAL readers drop their cache
//! when the database changed.

use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};

use rusqlite::Connection;
use rusqlite::Transaction;

use crate::StoreError;
use crate::schema::classify;
use crate::store::Access;
use crate::store::PragmaSpec;
use crate::store::open_connection;

/// How many reader connections the pool holds (spec/01: "a small pool
/// of reader connections (default 4)"). One per concurrent query; more
/// callers wait rather than fail.
pub(crate) const READER_COUNT: usize = 4;

/// Check inside sorts without an atomic load on every VM instruction.
const STOP_CHECK_OPS: std::ffi::c_int = 100;

/// One-shot signal shared by a window and its admitted ID reads (spec/07).
#[derive(Debug, Clone, Default)]
pub struct ReadStop(Arc<AtomicBool>);

impl ReadStop {
    /// Raise the signal.
    pub fn stop(&self) {
        self.0.store(true, Ordering::Release);
    }

    /// Whether the signal is raised.
    pub fn is_stopped(&self) -> bool {
        self.0.load(Ordering::Acquire)
    }
}

/// A scoped read's complete result or cancellation, never partial data.
#[derive(Debug)]
pub enum Scoped<T> {
    Done(T),
    Stopped,
}

/// A pool of `READER_COUNT` read-only connections. Taking one blocks
/// until a reader is free, so callers can't exhaust it — they queue.
pub(crate) struct Readers {
    /// The idle readers, ready to serve. Held in a mutex with
    /// [`Readers::free`]: pop to lend, push to return, so the reader
    /// lent is the warmest.
    idle: Mutex<Vec<Connection>>,
    free: Condvar,
}

impl Readers {
    /// Open `READER_COUNT` readers from the one connection constructor
    /// (spec/03), read-only, with the reader's pragma table.
    pub(crate) fn open(state_db: &Path, pragmas: &[PragmaSpec]) -> Result<Self, StoreError> {
        let mut idle = Vec::with_capacity(READER_COUNT);
        for _ in 0..READER_COUNT {
            idle.push(open_connection(state_db, Access::ReadOnly, pragmas)?);
        }
        Ok(Self {
            idle: Mutex::new(idle),
            free: Condvar::new(),
        })
    }

    /// Take a free reader, blocking until one is. A panic while
    /// waiting can only be lock poisoning; nothing else runs between
    /// lock and wait.
    fn acquire(&self) -> Connection {
        let mut idle = self
            .idle
            .lock()
            .expect("a panicking thread held the reader lock");
        loop {
            if let Some(conn) = idle.pop() {
                return conn;
            }
            idle = self
                .free
                .wait(idle)
                .expect("a panicking thread held the reader lock");
        }
    }

    /// How many readers are idle in the pool right now.
    #[cfg(test)]
    pub(crate) fn idle_count(&self) -> usize {
        self.idle
            .lock()
            .expect("a panicking thread held the reader lock")
            .len()
    }

    /// Give a reader back, ready for the next caller.
    fn release(&self, conn: Connection) {
        self.idle
            .lock()
            .expect("a panicking thread held the reader lock")
            .push(conn);
        self.free.notify_one();
    }

    /// Run `f` as one short read transaction on a pooled reader
    /// (spec/03): take a free reader (waiting for one), run, return
    /// the reader — also when `f` fails or panics, so a pool is never
    /// lost. A panicking `f` propagates after the reader went back.
    pub(crate) fn read<T>(
        &self,
        f: impl FnOnce(&Transaction) -> Result<T, StoreError>,
    ) -> Result<T, StoreError> {
        let mut conn = self.acquire();
        let outcome =
            std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| read_once(&mut conn, f)));
        self.release(conn);
        match outcome {
            Ok(result) => result,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }

    /// Interrupt this read when `stop` is raised, including before a sort's
    /// first row. Clear the handler before transaction/pool return on every
    /// exit. Other reads and writers are unaffected (spec/03).
    pub(crate) fn read_scoped<T>(
        &self,
        stop: &ReadStop,
        f: impl FnOnce(&Transaction) -> Result<T, StoreError>,
    ) -> Result<Scoped<T>, StoreError> {
        // A signal already raised answers without borrowing a reader.
        if stop.is_stopped() {
            return Ok(Scoped::Stopped);
        }
        let mut conn = self.acquire();
        let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
            read_once_scoped(&mut conn, stop, f)
        }));
        self.release(conn);
        match outcome {
            Ok(result) => result,
            Err(panic) => std::panic::resume_unwind(panic),
        }
    }

    /// Close every reader. Each was opened by the constructor with
    /// `NO_CKPT_ON_CLOSE` set and nobody lifted it, so closing one
    /// never checkpoints (spec/03).
    pub(crate) fn close(self) -> Result<(), StoreError> {
        let mut idle = self.idle.into_inner().expect("a reader was poisoned");
        let mut first_err = None;
        for conn in idle.drain(..) {
            if let Err((_conn, e)) = conn.close() {
                first_err.get_or_insert_with(|| classify(e, "close a reader connection"));
            }
        }
        first_err.map_or(Ok(()), Err)
    }
}

/// One read: a deferred transaction around `f`, committed to end it —
/// a single short snapshot (spec/03, spec/07).
fn read_once<T>(
    conn: &mut Connection,
    f: impl FnOnce(&Transaction) -> Result<T, StoreError>,
) -> Result<T, StoreError> {
    let tx = conn
        .transaction()
        .map_err(|e| classify(e, "begin a read transaction"))?;
    let value = f(&tx)?;
    tx.commit()
        .map_err(|e| classify(e, "end a read transaction"))?;
    Ok(value)
}

/// Preserve typed interruption so a raised stop cannot hide another failure.
fn read_once_scoped<T>(
    conn: &mut Connection,
    stop: &ReadStop,
    f: impl FnOnce(&Transaction) -> Result<T, StoreError>,
) -> Result<Scoped<T>, StoreError> {
    let tx = conn
        .transaction()
        .map_err(|e| classify(e, "begin a read transaction"))?;
    let mark = {
        let flag = Arc::clone(&stop.0);
        move || flag.load(Ordering::Acquire)
    };
    tx.progress_handler(STOP_CHECK_OPS, Some(mark))
        .map_err(|e| classify(e, "scope a read's stop signal"))?;
    let outcome = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| f(&tx)));
    // Clear the handler before commit/rollback, including during unwinding.
    let uninstalled = tx
        .progress_handler(0, None::<fn() -> bool>)
        .map_err(|e| classify(e, "unscope a read's stop signal"));
    match outcome {
        Err(panic) => std::panic::resume_unwind(panic),
        Ok(Err(error)) => {
            uninstalled?;
            match error {
                StoreError::Interrupted { .. } => Ok(Scoped::Stopped),
                error => Err(error),
            }
        }
        Ok(Ok(value)) => {
            uninstalled?;
            tx.commit()
                .map_err(|e| classify(e, "end a read transaction"))?;
            // A close after the last VM check still discards the result.
            if stop.is_stopped() {
                return Ok(Scoped::Stopped);
            }
            Ok(Scoped::Done(value))
        }
    }
}

#[cfg(test)]
mod tests;
