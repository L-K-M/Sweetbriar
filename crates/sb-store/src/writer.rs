//! The writer (spec/03 "Connections", "Write discipline"; spec/01
//! "Store threads"): one OS thread owns the store's only write
//! `Connection` and runs every submitted closure in one `BEGIN
//! IMMEDIATE` transaction, in priority order — user commands, then
//! sync batches, then index and backup jobs. Submitting returns a
//! [`WriteFuture`], a one-shot slot that works under any executor, so
//! this crate takes no async dependency.

use std::any::Any;
use std::collections::VecDeque;
use std::fmt;
use std::future::Future;
use std::io;
use std::sync::Arc;
use std::sync::Condvar;
use std::sync::Mutex;
use std::task::{Poll, Waker};
use std::thread::JoinHandle;

use rusqlite::Connection;
use rusqlite::Transaction;
use rusqlite::TransactionBehavior;
use rusqlite::config::DbConfig;

use crate::StoreError;
use crate::list_rows;
use crate::schema::classify;

/// A write's place in the writer's queue (spec/01): user commands
/// first, then sync batches, then index and backup jobs. The variant
/// order is the serving order; within one priority, FIFO.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum WritePriority {
    /// A user-initiated command: latency the user can feel.
    User,
    /// A sync batch from an account worker.
    Sync,
    /// An index, backup or other background job.
    Index,
}

impl WritePriority {
    /// This priority's queue, by position in the serving order.
    fn queue(self) -> usize {
        self as usize
    }
}

/// How many priority queues the writer serves: one per
/// [`WritePriority`] variant.
const PRIORITY_QUEUES: usize = 3;

/// The writer's queue state, shared with every handle that submits a
/// write. The mutex guards the queues and the closed flag; the
/// condvar wakes the writer thread when a job arrives or the writer
/// closes.
struct WriterShared {
    state: Mutex<WriterState>,
    ready: Condvar,
    /// Told each new `list_generation` after the job that made it
    /// committed (spec/07 "Subscriptions").
    observer: Mutex<Option<ListObserver>>,
}

/// What hears of each committed list generation. Called on the writer
/// thread between jobs, so it must return at once.
pub type ListObserver = Arc<dyn Fn(u64) + Send + Sync>;

struct WriterState {
    /// One FIFO queue per priority, indexed by [`WritePriority::queue`].
    queues: [VecDeque<Box<dyn QueuedWrite>>; PRIORITY_QUEUES],
    /// Whether a job has run since the queues last drained — the idle
    /// checkpoint (spec/03) fires when this is set and the queues are
    /// empty.
    worked_since_drain: bool,
    /// Set by `close`: no further jobs are accepted, the queued ones
    /// still run, then the thread ends.
    closed: bool,
}

impl WriterState {
    /// Pop the next job in priority order (spec/01): from the first
    /// queue that holds one, `None` when all are empty.
    fn pop(&mut self) -> Option<Box<dyn QueuedWrite>> {
        for queue in &mut self.queues {
            if let Some(job) = queue.pop_front() {
                self.worked_since_drain = true;
                return Some(job);
            }
        }
        None
    }
}

/// What the writer thread should do next.
enum Next {
    /// Run this job on the connection.
    Job(Box<dyn QueuedWrite>),
    /// The queue drained after work ran: checkpoint outside the lock.
    Drained,
    /// Closed and drained: the thread ends.
    Done,
}

impl WriterShared {
    /// Wait for the next job. Returns [`Next::Drained`] when the
    /// queues are empty after work ran (spec/03's idle checkpoint) and
    /// [`Next::Done`] once closed and drained.
    fn next(&self) -> Next {
        let mut state = lock(&self.state);
        loop {
            if let Some(job) = state.pop() {
                return Next::Job(job);
            }
            if state.closed {
                return Next::Done;
            }
            if state.worked_since_drain {
                state.worked_since_drain = false;
                return Next::Drained;
            }
            state = wait_for(&self.ready, state);
        }
    }

    /// Push a job, or report that the writer is closed. The caller
    /// wakes the thread.
    fn push(&self, priority: WritePriority, job: Box<dyn QueuedWrite>) -> Result<(), StoreError> {
        let mut state = lock(&self.state);
        if state.closed {
            return Err(closed_error());
        }
        state.queues[priority.queue()].push_back(job);
        Ok(())
    }

    /// The writer thread is ending: refuse every later submit and fail
    /// the jobs still queued (only a panicked thread leaves any), so no
    /// future hangs — the writer is gone before running them, which is
    /// `Unavailable`. Their futures resolve outside the lock, since
    /// waking one may run arbitrary code.
    fn shut_down(&self) {
        let orphans: Vec<Box<dyn QueuedWrite>> = {
            let mut state = lock(&self.state);
            state.closed = true;
            state
                .queues
                .iter_mut()
                .flat_map(|queue| queue.drain(..))
                .collect()
        };
        for job in orphans {
            job.abandon(closed_error());
        }
    }
}

/// The refusal a submit after close gets.
fn closed_error() -> StoreError {
    StoreError::unavailable("the writer is closing; this write was not queued")
}

/// Lock one of the writer's mutexes; only a panic while holding one
/// poisons it, and no code between lock and unlock can panic.
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .expect("a panicking thread held the writer lock")
}

/// Wait on the writer's condvar, re-acquiring the guard.
fn wait_for<'a, T>(
    condvar: &Condvar,
    guard: std::sync::MutexGuard<'a, T>,
) -> std::sync::MutexGuard<'a, T> {
    condvar
        .wait(guard)
        .expect("a panicking thread held the writer lock")
}

/// One queued write, type-erased so the queues hold closures of any
/// result type. `run` executes it in one immediate transaction on the
/// writer's connection and resolves its future; `abandon` resolves it
/// without running it.
trait QueuedWrite: Send {
    /// Run the job; `committed` runs after a successful commit and
    /// before the job's future resolves.
    fn run(self: Box<Self>, conn: &mut Connection, committed: &mut dyn FnMut(&Connection));
    fn abandon(self: Box<Self>, err: StoreError);
}

/// The type-erased job for one submitted closure and its slot.
struct Write<T, F> {
    closure: F,
    slot: Arc<Slot<T>>,
}

impl<T, F> QueuedWrite for Write<T, F>
where
    T: Send + 'static,
    F: FnOnce(&Transaction) -> Result<T, StoreError> + Send + 'static,
{
    fn run(self: Box<Self>, conn: &mut Connection, committed: &mut dyn FnMut(&Connection)) {
        let Write { closure, slot } = *self;
        run_write(conn, closure, &slot, committed);
    }

    fn abandon(self: Box<Self>, err: StoreError) {
        self.slot.resolve(Err(err));
    }
}

/// Execute one write closure exactly as spec/03 requires: one `BEGIN
/// IMMEDIATE` transaction, committed when the closure returns `Ok`,
/// rolled back when it returns `Err` or panics — the panic is caught,
/// so the writer keeps serving — and the future always resolves.
fn run_write<T, F>(
    conn: &mut Connection,
    closure: F,
    slot: &Arc<Slot<T>>,
    committed: &mut dyn FnMut(&Connection),
) where
    T: Send + 'static,
    F: FnOnce(&Transaction) -> Result<T, StoreError> + Send + 'static,
{
    let tx = match conn.transaction_with_behavior(TransactionBehavior::Immediate) {
        Ok(tx) => tx,
        Err(e) => {
            slot.resolve(Err(classify(e, "begin a write transaction")));
            return;
        }
    };
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| closure(&tx))) {
        Err(panic) => {
            // Dropping the transaction rolls the write back.
            drop(tx);
            slot.resolve(Err(StoreError::internal(format!(
                "a write closure panicked and was rolled back: {}",
                panic_message(panic.as_ref())
            ))));
        }
        Ok(Err(err)) => {
            drop(tx);
            slot.resolve(Err(err));
        }
        Ok(Ok(value)) => match tx.commit() {
            // A failed commit drops the transaction inside rusqlite,
            // rolling the write back.
            Ok(()) => {
                committed(conn);
                slot.resolve(Ok(value));
            }
            Err(e) => slot.resolve(Err(classify(e, "commit a write transaction"))),
        },
    }
}

/// The payload of a caught panic, for the `Internal` message.
fn panic_message(panic: &(dyn Any + Send)) -> String {
    if let Some(text) = panic.downcast_ref::<&str>() {
        (*text).to_owned()
    } else if let Some(text) = panic.downcast_ref::<String>() {
        text.clone()
    } else {
        "an unknown panic payload".to_owned()
    }
}

/// The one-shot slot behind [`WriteFuture`]: the writer thread stores
/// the result and wakes the registered waker; polling reads it.
struct Slot<T> {
    state: Mutex<SlotState<T>>,
}

enum SlotState<T> {
    /// No result yet; the waker to call when one arrives.
    Waiting(Option<Waker>),
    /// Resolved; the result is taken by the first poll.
    Done(Option<Result<T, StoreError>>),
}

impl<T> Slot<T> {
    fn new() -> Self {
        Self {
            state: Mutex::new(SlotState::Waiting(None)),
        }
    }

    /// Store `result` and wake the future, if one is waiting.
    fn resolve(&self, result: Result<T, StoreError>) {
        let waker = {
            let mut state = lock_slot(&self.state);
            match &mut *state {
                SlotState::Waiting(waker) => {
                    let waker = waker.take();
                    *state = SlotState::Done(Some(result));
                    waker
                }
                // Resolving twice is a writer bug; keep the first.
                SlotState::Done(_) => None,
            }
        };
        // Wake outside the lock: the woken task may poll at once.
        if let Some(waker) = waker {
            waker.wake();
        }
    }
}

/// Lock a slot; poisoned only by a panic while held, which the writer
/// never does (it only moves a result in or a waker out).
fn lock_slot<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex.lock().expect("a panicking thread held the slot lock")
}

/// The result of a submitted write (spec/03). Completes when the
/// writer's transaction commits (`Ok`), rolls back, or the closure
/// panics (`StoreError::Internal`), or when the writer is gone before
/// running the job (`StoreError::Unavailable`). Works under any
/// executor: it only registers the executor's waker.
pub struct WriteFuture<T> {
    slot: Arc<Slot<T>>,
}

impl<T> Future for WriteFuture<T> {
    type Output = Result<T, StoreError>;

    fn poll(self: std::pin::Pin<&mut Self>, cx: &mut std::task::Context<'_>) -> Poll<Self::Output> {
        let mut state = lock_slot(&self.slot.state);
        match &mut *state {
            SlotState::Waiting(waker) => {
                *waker = Some(cx.waker().clone());
                Poll::Pending
            }
            SlotState::Done(result) => Poll::Ready(result.take().unwrap_or_else(|| {
                // Only a poll after completion lands here; the Future
                // contract makes that a caller bug, but hanging would
                // be worse than saying so.
                Err(StoreError::internal(
                    "the write future was polled after completing",
                ))
            })),
        }
    }
}

impl<T> fmt::Debug for WriteFuture<T> {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("WriteFuture")
    }
}

/// The store's writer (spec/03 "Connections", "Write discipline"): one
/// OS thread that owns the only write connection and serves submitted
/// closures by priority. Build it by moving the connection in (after
/// `open` ran the migrations); [`Writer::close`] joins the thread and
/// closes the connection — the store's last, so SQLite checkpoints
/// and removes the WAL.
pub struct Writer {
    shared: Arc<WriterShared>,
    /// The writer thread until `close` joins it. `close` holds this
    /// lock while joining, so a concurrent second `close` returns only
    /// once the thread has ended.
    handle: Mutex<Option<JoinHandle<Result<(), StoreError>>>>,
    /// Told when a `close` is about to join, with the handle lock held.
    #[cfg(test)]
    joining: Mutex<Option<std::sync::mpsc::Sender<()>>>,
}

/// The writer thread's body, as handed to a [`Spawn`].
pub(crate) type WriterBody = Box<dyn FnOnce() -> Result<(), StoreError> + Send>;

/// Starts the writer thread. Production spawns an OS thread; a test
/// can refuse, as an exhausted OS would.
pub(crate) type Spawn = fn(WriterBody) -> io::Result<JoinHandle<Result<(), StoreError>>>;

/// The writer thread's name, for debuggers and panic messages.
const WRITER_THREAD_NAME: &str = "sb-store-writer";

/// The production [`Spawn`]: a named OS thread.
fn spawn_os_thread(body: WriterBody) -> io::Result<JoinHandle<Result<(), StoreError>>> {
    std::thread::Builder::new()
        .name(WRITER_THREAD_NAME.to_owned())
        .spawn(body)
}

impl Writer {
    /// Start the writer thread over `conn`, which the caller hands
    /// over for good: from here only the thread touches it. An OS that
    /// refuses the thread is `Unavailable`.
    pub(crate) fn new(conn: Connection) -> Result<Self, StoreError> {
        Self::new_with(conn, spawn_os_thread)
    }

    /// Install the one observer of committed list generations,
    /// replacing any earlier one.
    pub(crate) fn observe_list_generation(&self, observer: ListObserver) {
        *lock(&self.shared.observer) = Some(observer);
    }

    /// [`Writer::new`] with the thread spawner as a parameter. If it
    /// refuses, `conn` is dropped here, still not checkpointing on
    /// close: only the running thread lifts that (spec/02: no
    /// checkpoint until open succeeds).
    pub(crate) fn new_with(conn: Connection, spawn: Spawn) -> Result<Self, StoreError> {
        let shared = Arc::new(WriterShared {
            state: Mutex::new(WriterState {
                queues: [VecDeque::new(), VecDeque::new(), VecDeque::new()],
                worked_since_drain: false,
                closed: false,
            }),
            ready: Condvar::new(),
            observer: Mutex::new(None),
        });
        let thread_shared = Arc::clone(&shared);
        let handle = spawn(Box::new(move || serve(conn, thread_shared)))
            .map_err(|e| StoreError::unavailable(format!("cannot start the writer thread: {e}")))?;
        Ok(Self {
            shared,
            handle: Mutex::new(Some(handle)),
            #[cfg(test)]
            joining: Mutex::new(None),
        })
    }

    /// Queue one write at `priority`; the returned future resolves
    /// with the closure's result. A closure that returns `Err` rolls
    /// its transaction back; one that panics is caught, rolled back
    /// and resolved as `StoreError::Internal` while the writer keeps
    /// serving; a submit after [`Writer::close`] resolves
    /// `StoreError::Unavailable` without queueing.
    pub fn submit<T, F>(&self, priority: WritePriority, closure: F) -> WriteFuture<T>
    where
        T: Send + 'static,
        F: FnOnce(&Transaction) -> Result<T, StoreError> + Send + 'static,
    {
        let slot = Arc::new(Slot::new());
        let future = WriteFuture {
            slot: Arc::clone(&slot),
        };
        let job = Write {
            closure,
            slot: Arc::clone(&slot),
        };
        match self.shared.push(priority, Box::new(job)) {
            Ok(()) => self.shared.ready.notify_one(),
            Err(err) => slot.resolve(Err(err)),
        }
        future
    }

    /// Stop accepting writes, finish the queued ones, join the thread
    /// and close the connection — the store's last, so SQLite
    /// checkpoints and removes the WAL (M0.4 lifted
    /// `NO_CKPT_ON_CLOSE` for the writer). Idempotent: a second call,
    /// even a concurrent one, returns once the thread has ended.
    pub fn close(&self) -> Result<(), StoreError> {
        lock(&self.shared.state).closed = true;
        self.shared.ready.notify_all();
        let mut handle = self
            .handle
            .lock()
            .expect("a panicking thread held the writer-thread lock");
        let Some(thread) = handle.take() else {
            return Ok(());
        };
        #[cfg(test)]
        self.announce_joining();
        match thread.join() {
            Ok(result) => result,
            Err(_) => Err(StoreError::internal(
                "the writer thread panicked before closing",
            )),
        }
    }
}

#[cfg(test)]
impl Writer {
    /// Whether the writer refuses new jobs: closed, or its thread has
    /// ended.
    pub(crate) fn is_closed(&self) -> bool {
        lock(&self.shared.state).closed
    }

    /// Have `close` tell `to` when it is about to join the thread.
    pub(crate) fn on_joining(&self, to: std::sync::mpsc::Sender<()>) {
        *lock(&self.joining) = Some(to);
    }

    /// Whether some `close` holds the handle lock right now.
    pub(crate) fn handle_is_locked(&self) -> bool {
        self.handle.try_lock().is_err()
    }

    fn announce_joining(&self) {
        if let Some(to) = lock(&self.joining).as_ref() {
            let _ = to.send(());
        }
    }
}

impl Drop for Writer {
    fn drop(&mut self) {
        // Dropping cannot report; close anyway so the thread never
        // outlives the writer and the connection always closes.
        let _ = self.close();
    }
}

impl fmt::Debug for Writer {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str("Writer")
    }
}

/// The writer thread's body: serve jobs until closed and drained,
/// then close the connection. Even a panic in the loop itself (no
/// closure panic reaches it) still closes the connection, stops
/// accepting jobs and abandons the queue, so no future hangs.
fn serve(mut conn: Connection, shared: Arc<WriterShared>) -> Result<(), StoreError> {
    // The thread runs, so open succeeded: from now on the writer owns
    // checkpoints (spec/03), on close too.
    if let Err(e) = conn.set_db_config(DbConfig::SQLITE_DBCONFIG_NO_CKPT_ON_CLOSE, false) {
        shared.shut_down();
        let _ = conn.close();
        return Err(classify(e, "let the writer checkpoint on close"));
    }
    let served = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        writer_loop(&mut conn, &shared)
    }));
    // The writer is gone: refuse later submits and resolve whatever a
    // panic left queued, rather than leave futures pending forever.
    shared.shut_down();
    match served {
        Ok(()) => conn
            .close()
            .map_err(|(_conn, e)| classify(e, "close the writer connection")),
        Err(_) => {
            // The caught closure's borrow of `conn` has ended; closing
            // it discards any transaction it still holds.
            let _ = conn.close();
            Err(StoreError::internal("the writer thread panicked"))
        }
    }
}

/// The writer's serve loop: run jobs by priority, announcing each new
/// list generation once its job committed; when the queue drains after
/// work, checkpoint.
fn writer_loop(conn: &mut Connection, shared: &Arc<WriterShared>) {
    let mut announced = list_rows::generation(conn).ok();
    loop {
        match shared.next() {
            Next::Job(job) => {
                // A caller whose write resolved finds it announced.
                job.run(conn, &mut |conn| announce(conn, shared, &mut announced));
            }
            Next::Drained => {
                // The idle checkpoint (spec/03): with the queue drained
                // after work, truncate the WAL so it never grows
                // without bound between `wal_autocheckpoint`s. Nobody
                // awaits this — durability never depends on it, since
                // every write commits with `synchronous=FULL` before
                // its future resolves — so a busy result (readers
                // still active) and any error are ignored.
                let _ = conn.execute_batch("PRAGMA wal_checkpoint(TRUNCATE)");
            }
            Next::Done => return,
        }
    }
}

/// Tell the observer of a list generation the job just committed; one
/// that left the generation announces nothing.
fn announce(conn: &Connection, shared: &WriterShared, announced: &mut Option<u64>) {
    let Ok(current) = list_rows::generation(conn) else {
        return;
    };
    if *announced == Some(current) {
        return;
    }
    *announced = Some(current);
    let observer = lock(&shared.observer).clone();
    if let Some(observer) = observer {
        observer(current);
    }
}

#[cfg(test)]
mod tests;
