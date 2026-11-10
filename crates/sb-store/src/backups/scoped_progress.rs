//! A SQLite progress handler scoped to one borrowed read, for the
//! bounded image inspections (spec/03). rusqlite 0.41 installs hooks
//! only through `&mut Connection`, but the inspections keep prepared
//! statements borrowing the connection across reads. This installs
//! SQLite's handler directly for the duration of one closure and
//! removes it before returning, unwinding included.
//!
//! The handler reads and writes atomics only, in a [`WorkLimit`] the
//! caller keeps borrowed for the whole installation, so it never
//! aliases mutable data and cannot unwind into SQLite. Nothing else
//! may install a progress handler on the connection meanwhile: rusqlite
//! needs `&mut` for that, which the shared borrow here excludes.
//!
//! Preconditions, both upheld by every caller: the connection carries
//! no rusqlite progress handler on entry (this one takes SQLite's only
//! slot and clears it afterwards, so a prior one would stay silenced),
//! and scoped reads never nest on one thread (an inner one would clear
//! the outer handler and leave the outer read unbounded). Nesting is
//! refused with a panic rather than silently weakening the bound.

use std::cell::Cell;
use std::os::raw::{c_int, c_void};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};

use rusqlite::{Connection, ffi};

/// The work a read may do before SQLite interrupts it: `budget`
/// handler calls, or none at all once `stopped` is raised.
pub(crate) struct WorkLimit<'a> {
    stopped: &'a AtomicBool,
    budget: u32,
    calls: AtomicU32,
    exhausted: AtomicBool,
}

impl<'a> WorkLimit<'a> {
    pub(crate) fn new(stopped: &'a AtomicBool, budget: u32) -> Self {
        Self {
            stopped,
            budget,
            calls: AtomicU32::new(0),
            exhausted: AtomicBool::new(false),
        }
    }

    /// Whether the read ran out of its budget.
    pub(crate) fn exhausted(&self) -> bool {
        self.exhausted.load(Ordering::Acquire)
    }

    /// One handler call: true interrupts SQLite.
    fn tick(&self) -> bool {
        if self.stopped.load(Ordering::Acquire) {
            return true;
        }
        let calls = self.calls.fetch_add(1, Ordering::AcqRel).saturating_add(1);
        if calls >= self.budget {
            self.exhausted.store(true, Ordering::Release);
            return true;
        }
        false
    }
}

/// SQLite's progress callback. Returns nonzero to interrupt.
unsafe extern "C" fn tick(limit: *mut c_void) -> c_int {
    // SAFETY: `with_work_limit` passes a pointer to a `WorkLimit` it
    // keeps borrowed until the handler is removed, and only shared
    // access through atomics happens here.
    let limit = unsafe { &*limit.cast::<WorkLimit<'_>>() };
    c_int::from(limit.tick())
}

thread_local! {
    /// Whether this thread runs a scoped read now.
    static SCOPED: Cell<bool> = const { Cell::new(false) };
}

/// Removes the handler when dropped, on every path out of the read.
struct Installed(*mut ffi::sqlite3);

impl Drop for Installed {
    fn drop(&mut self) {
        // SAFETY: the handle belongs to the connection the caller still
        // borrows; clearing the handler needs no other invariant.
        unsafe {
            ffi::sqlite3_progress_handler(self.0, 0, None, std::ptr::null_mut());
        }
        SCOPED.with(|scoped| scoped.set(false));
    }
}

/// Run `read` with SQLite calling `limit` every `ops` virtual-machine
/// instructions. The handler is removed before this returns.
pub(crate) fn with_work_limit<T>(
    conn: &Connection,
    ops: c_int,
    limit: &WorkLimit<'_>,
    read: impl FnOnce() -> T,
) -> T {
    assert!(
        !SCOPED.with(|scoped| scoped.replace(true)),
        "a scoped SQLite read nested inside another would unbound the outer one"
    );
    // SAFETY: the raw handle stays valid while `conn` is borrowed, which
    // outlives `installed`; `limit` is borrowed for the same span, so
    // the pointer SQLite holds never dangles.
    let db = unsafe { conn.handle() };
    unsafe {
        ffi::sqlite3_progress_handler(
            db,
            ops,
            Some(tick),
            std::ptr::from_ref(limit).cast_mut().cast::<c_void>(),
        );
    }
    let installed = Installed(db);
    let value = read();
    drop(installed);
    value
}

#[cfg(test)]
mod tests;
