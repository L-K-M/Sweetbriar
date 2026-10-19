//! The engine (spec/01 "Process model and threading", spec/07
//! "Lifecycle"): the one owner of the Tokio runtime and the store
//! handle. `open_core` creates it, `Core::shutdown` ends it. Store
//! reads run on the runtime's blocking pool; writes await the writer's
//! own future, which works under any executor — so the futures this
//! crate returns are runtime-neutral and a shell (the CLI, later the
//! FFI layer) can drive them with anything.

use std::path::Path;
use std::sync::{Arc, Condvar, Mutex};

use tokio::runtime::{Handle, Runtime};
use tokio::task::{JoinError, JoinHandle};

use crate::EngineError;
use crate::import::ImportJobs;
use crate::windows::Windows;
use sb_resources::{Resources, list_generation};

/// The name of the worker a dropped engine closes its store on.
const CLOSE_THREAD_NAME: &str = "sb-engine-close";

/// The name of the thread an engine is opened on.
const OPEN_THREAD_NAME: &str = "sb-engine-open";

/// The open thread's body, as handed to a [`SpawnOpen`].
pub(crate) type OpenBody = Box<dyn FnOnce() + Send>;

/// Starts the thread an engine is opened on. Production spawns an OS
/// thread; a test can refuse, as an exhausted OS would.
pub(crate) type SpawnOpen = fn(OpenBody) -> std::io::Result<()>;

/// Opens the store's resources on the open thread: production uses the
/// real seams, crash-cut tests an injected filesystem.
type OpenResources = Box<dyn FnOnce(&Path) -> Result<Resources, sb_resources::StoreError> + Send>;

/// The production [`SpawnOpen`]: a named, detached OS thread.
fn spawn_open_thread(body: OpenBody) -> std::io::Result<()> {
    std::thread::Builder::new()
        .name(OPEN_THREAD_NAME.to_owned())
        .spawn(body)
        .map(drop)
}

/// What a dropped engine hands its cleanup worker: the store's last
/// handle and the count of blocking tasks still using it.
type Cleanup = (
    Arc<Resources>,
    Arc<BlockingWork>,
    Arc<crate::bodies::Leases>,
);

/// The engine: one multi-threaded runtime and the store's one handle
/// (spec/01), plus a cleanup worker reserved at open. Every operation
/// goes through here.
///
/// On drop the store handle goes to the cleanup worker (closing a store
/// joins its writer thread, which must not block an async caller), then
/// the runtime shuts down in the background.
pub struct Engine {
    resources: Option<Arc<Resources>>,
    blocking: Arc<BlockingWork>,
    runtime: OwnedRuntime,
    /// The open windows and the debug source behind them (spec/07
    /// "Queries"): in memory only — no store reads. Shared with the
    /// runtime tasks the window calls run on.
    windows: Arc<Windows>,
    /// Import jobs with a live worker, and their cancel flags (spec/09).
    imports: Arc<ImportJobs>,
    leases: Arc<crate::bodies::Leases>,
    /// The worker reserved at open that closes the store if the engine
    /// is dropped without `shutdown`; it ends when this sender drops.
    cleanup: std::sync::mpsc::Sender<Cleanup>,
}

/// The runtime, shut down without blocking however it is dropped: an
/// engine, or an `open` or `shutdown` future, may be dropped inside
/// another runtime's async context, where dropping a `Runtime` would
/// block and panic.
struct OwnedRuntime(Option<Runtime>);

impl OwnedRuntime {
    /// Start the runtime; an OS that refuses its threads is
    /// `Unavailable` (spec/01: threads are a startup resource). Tokio
    /// panics rather than erring when a worker thread can't be spawned,
    /// so the panic is caught and reported like the error.
    fn start() -> Result<Self, EngineError> {
        match std::panic::catch_unwind(Runtime::new) {
            Ok(Ok(runtime)) => Ok(Self(Some(runtime))),
            Ok(Err(e)) => Err(EngineError::unavailable(format!(
                "cannot start the engine's runtime: {e}"
            ))),
            Err(_) => Err(EngineError::unavailable(
                "cannot start the engine's runtime: the OS refused its threads",
            )),
        }
    }

    fn handle(&self) -> &Handle {
        self.0
            .as_ref()
            .expect("the runtime lives as long as its owner")
            .handle()
    }
}

impl Drop for OwnedRuntime {
    fn drop(&mut self) {
        if let Some(runtime) = self.0.take() {
            runtime.shutdown_background();
        }
    }
}

/// The blocking tasks still running on the store, and whether shutdown
/// has begun. A caller that stops awaiting a read doesn't stop its
/// blocking task, so shutdown waits here until every one has finished
/// and given its store handle back, and refuses new ones meanwhile.
#[derive(Default)]
struct BlockingWork {
    state: Mutex<BlockingState>,
    idle: Condvar,
}

#[derive(Default)]
struct BlockingState {
    running: usize,
    closing: bool,
}

impl BlockingWork {
    fn state(&self) -> std::sync::MutexGuard<'_, BlockingState> {
        self.state
            .lock()
            .expect("a panicking thread held the engine's work lock")
    }

    /// Count one more blocking task, unless shutdown has begun.
    fn start(self: &Arc<Self>) -> Result<Running, EngineError> {
        let mut state = self.state();
        if state.closing {
            return Err(EngineError::unavailable("the core is shutting down"));
        }
        state.running += 1;
        Ok(Running(Arc::clone(self)))
    }

    /// Whether shutdown has begun, for long work that pauses itself.
    fn closing(&self) -> bool {
        self.state().closing
    }

    /// Refuse new blocking tasks and wait until the running ones end.
    fn close_and_wait(&self) {
        let mut state = self.state();
        state.closing = true;
        while state.running > 0 {
            state = self
                .idle
                .wait(state)
                .expect("a panicking thread held the engine's work lock");
        }
    }
}

/// Runs cleanup on the engine's blocking pool without waiting for it:
/// for work a dropped value owes — a `Drop` may run on any thread, the
/// foreign caller's included, where nothing may block (spec/07 "One
/// runtime"). Counted like other blocking work, so shutdown waits for
/// it; skipped once shutdown has begun, which drops everything anyway.
pub(crate) struct Detached {
    handle: Handle,
    blocking: Arc<BlockingWork>,
}

impl Detached {
    /// Run `f` on the blocking pool, or not at all during shutdown.
    pub(crate) fn spawn(&self, f: impl FnOnce() + Send + 'static) {
        let Ok(running) = self.blocking.start() else {
            return;
        };
        self.handle.spawn_blocking(move || {
            f();
            drop(running);
        });
    }
}

/// A long-running engine task such as an import job (spec/01
/// "Background jobs"). It counts as blocking work for its whole life, so
/// shutdown waits for it, and it sees shutdown begin so it can pause.
/// Fields drop in order: the store handle before the count.
pub(crate) struct Worker {
    resources: Arc<Resources>,
    running: Arc<Running>,
    blocking: Arc<BlockingWork>,
    handle: Handle,
}

impl Worker {
    pub(crate) fn resources(&self) -> &Resources {
        &self.resources
    }

    /// A check that blocking work can poll: has shutdown begun?
    pub(crate) fn shutdown_check(&self) -> impl Fn() -> bool + Send + Sync + 'static {
        let blocking = Arc::clone(&self.blocking);
        move || blocking.closing()
    }

    /// Run `f` with the store on the blocking pool, inside this worker's
    /// count; the handle goes back before the count does.
    pub(crate) async fn blocking<T, F>(&self, f: F) -> Result<T, EngineError>
    where
        T: Send + 'static,
        F: FnOnce(&Resources) -> T + Send + 'static,
    {
        let resources = Arc::clone(&self.resources);
        let running = Arc::clone(&self.running);
        let task = self.handle.spawn_blocking(move || {
            let result = f(&resources);
            drop(resources);
            drop(running);
            result
        });
        task.await
            .map_err(|join| join_failed("run an engine job", join))
    }
}

/// Starts [`Worker`]s: the engine, or a worker handing work on. Refused
/// once shutdown has begun.
pub(crate) trait Spawn {
    fn spawn_worker<F, Fut>(&self, body: F) -> Result<(), EngineError>
    where
        F: FnOnce(Worker) -> Fut,
        Fut: std::future::Future<Output = ()> + Send + 'static;
}

impl Spawn for Engine {
    fn spawn_worker<F, Fut>(&self, body: F) -> Result<(), EngineError>
    where
        F: FnOnce(Worker) -> Fut,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        spawn(
            self.resources(),
            &self.blocking,
            self.runtime.handle(),
            body,
        )
    }
}

impl Spawn for Worker {
    fn spawn_worker<F, Fut>(&self, body: F) -> Result<(), EngineError>
    where
        F: FnOnce(Worker) -> Fut,
        Fut: std::future::Future<Output = ()> + Send + 'static,
    {
        spawn(&self.resources, &self.blocking, &self.handle, body)
    }
}

fn spawn<F, Fut>(
    resources: &Arc<Resources>,
    blocking: &Arc<BlockingWork>,
    handle: &Handle,
    body: F,
) -> Result<(), EngineError>
where
    F: FnOnce(Worker) -> Fut,
    Fut: std::future::Future<Output = ()> + Send + 'static,
{
    let worker = Worker {
        resources: Arc::clone(resources),
        running: Arc::new(blocking.start()?),
        blocking: Arc::clone(blocking),
        handle: handle.clone(),
    };
    handle.spawn(body(worker));
    Ok(())
}

/// One counted blocking task; dropping it, at the task's end, counts it
/// out.
struct Running(Arc<BlockingWork>);

impl Drop for Running {
    fn drop(&mut self) {
        let mut state = self.0.state();
        state.running -= 1;
        if state.running == 0 {
            self.0.idle.notify_all();
        }
    }
}

impl Engine {
    /// Start the runtime, then open the store at `data_root` on a
    /// thread of its own whose result comes back over a oneshot
    /// (spec/07 "Lifecycle"), so awaiting this future never blocks the
    /// caller's thread; dropping it midway shuts the fresh runtime down
    /// in the background.
    pub async fn open(data_root: impl AsRef<Path>) -> Result<Self, EngineError> {
        Self::open_with(data_root, spawn_open_thread).await
    }

    /// Open with the disposable cache used by body and attachment leases.
    pub async fn open_with_cache(
        data_root: impl AsRef<Path>,
        cache_root: impl AsRef<Path>,
    ) -> Result<Self, EngineError> {
        let cache_root = cache_root.as_ref().to_path_buf();
        Self::open_on(
            data_root.as_ref(),
            spawn_open_thread,
            Box::new(move |data| Resources::open_with_cache(data, &cache_root)),
        )
        .await
        .map_err(|error| match error {
            EngineError::InvalidInput { message, .. } if message.starts_with("cache root") => {
                EngineError::invalid_input("cache_root", message)
            }
            other => other,
        })
    }

    pub(crate) fn leases(&self) -> &Arc<crate::bodies::Leases> {
        &self.leases
    }

    /// [`Engine::open`] with the open thread's spawner as a parameter,
    /// so a test can refuse it as an exhausted OS would.
    ///
    /// The whole engine is built on a thread of its own, not the
    /// runtime's blocking pool: tokio queues a blocking task when it
    /// can't start a thread, so an open there could wait forever, while
    /// spawning a thread fails at once, as `Unavailable` (spec/01:
    /// threads are a startup resource). Building the engine there also
    /// means a finished open whose result is never polled drops an
    /// `Engine`, whose `Drop` closes the store off the caller's thread.
    pub(crate) async fn open_with(
        data_root: impl AsRef<Path>,
        spawn: SpawnOpen,
    ) -> Result<Self, EngineError> {
        Self::open_on(data_root.as_ref(), spawn, Box::new(Resources::open)).await
    }

    /// [`Engine::open`] on an injected filesystem, for crash-cut suites
    /// (spec/11).
    #[cfg(test)]
    pub(crate) async fn open_with_fs(
        data_root: impl AsRef<Path>,
        fs: Arc<dyn sb_resources::Fs>,
    ) -> Result<Self, EngineError> {
        let open = Box::new(move |root: &Path| Resources::open_with_fs(root, fs));
        Self::open_on(data_root.as_ref(), spawn_open_thread, open).await
    }

    async fn open_on(
        data_root: &Path,
        spawn: SpawnOpen,
        open: OpenResources,
    ) -> Result<Self, EngineError> {
        let data_root = data_root.to_path_buf();
        let runtime = OwnedRuntime::start()?;
        let (opened, received) = tokio::sync::oneshot::channel();
        spawn(Box::new(move || {
            let engine = open(&data_root)
                .map_err(EngineError::from)
                .and_then(|resources| {
                    // Subscriptions hear every committed list generation
                    // from here on, starting from the current one.
                    let windows = Arc::new(Windows::new());
                    let announced = Arc::clone(&windows);
                    resources.observe_list_generation(Arc::new(move |generation| {
                        announced.store_moved(generation);
                    }));
                    windows.store_moved(resources.read(list_generation)?);
                    Ok(Self {
                        resources: Some(Arc::new(resources)),
                        blocking: Arc::default(),
                        runtime,
                        windows,
                        imports: Arc::default(),
                        leases: Arc::default(),
                        cleanup: start_cleanup_worker()?,
                    })
                })
                // Recovery runs here, off the caller's thread, before
                // the engine is handed out (spec/09).
                .and_then(|engine| engine.recover_imports().map(|()| engine))
                // Messages imported before the last complete rebuild
                // get their threads in the background (spec/02).
                .inspect(Engine::schedule_threads);
            // A caller that stopped waiting drops the engine here.
            let _ = opened.send(engine);
        }))
        .map_err(|e| EngineError::unavailable(format!("cannot start the open thread: {e}")))?;
        received
            .await
            .map_err(|_| EngineError::internal("the thread opening the store panicked"))?
    }

    /// The store handle; present until `shutdown` or drop takes it.
    pub(crate) fn resources(&self) -> &Arc<Resources> {
        self.resources
            .as_ref()
            .expect("the store handle lives until shutdown")
    }

    /// The window table; crate-visible so the `windows` module can
    /// implement its share of `Engine`.
    pub(crate) fn windows(&self) -> &Arc<Windows> {
        &self.windows
    }

    /// The runtime's handle, for work a subscription dispatches after
    /// the call that made it returned.
    pub(crate) fn runtime_handle(&self) -> tokio::runtime::Handle {
        self.runtime.handle().clone()
    }

    /// The import job registry; crate-visible for the `import` module.
    pub(crate) fn imports(&self) -> &Arc<ImportJobs> {
        &self.imports
    }

    /// Wait for `future` on the calling thread. Only for the open
    /// thread, which is outside the runtime and may block.
    pub(crate) fn block_on<F: std::future::Future>(&self, future: F) -> F::Output {
        self.runtime.handle().block_on(future)
    }

    /// A fresh UUID string for a command's idempotency key (spec/07
    /// "Commands"): for shells without their own UUID source.
    pub fn new_idempotency_key(&self) -> String {
        self.resources().new_uuid_string()
    }

    /// Shut the core down (spec/07 "Lifecycle"): wait for every
    /// blocking task on the store, including reads whose callers
    /// stopped waiting, then close the store — the clean-shutdown
    /// marker, the writer, the lease — and end the runtime without
    /// blocking an async context. A close failure is reported to the
    /// caller, who has nothing left to do: the next open's
    /// `quick_check` is the safety net (spec/03).
    pub async fn shutdown(mut self) -> Result<(), EngineError> {
        self.windows.shut();
        let resources = self
            .resources
            .take()
            .expect("the store handle lives until shutdown");
        let blocking = Arc::clone(&self.blocking);
        let leases = self.leases.clone();
        let closing = self.runtime.handle().spawn_blocking(move || {
            blocking.close_and_wait();
            leases.close();
            close_resources(resources)
        });
        match closing.await {
            Ok(closed) => closed.map_err(EngineError::from),
            Err(join) => Err(join_failed("close the store", join)),
        }
    }

    /// Run `f` on the runtime's blocking pool, where reads belong
    /// (spec/01: nothing blocks a caller's thread), and flatten the
    /// join and the closure's own result into one error. The task is
    /// counted until it ends, so shutdown waits for it.
    pub(crate) async fn read_through<T, F>(&self, f: F) -> Result<T, EngineError>
    where
        T: Send + 'static,
        F: FnOnce(&Resources) -> Result<T, sb_resources::StoreError> + Send + 'static,
    {
        let running = self.blocking.start()?;
        let resources = Arc::clone(self.resources());
        let task = self.runtime.handle().spawn_blocking(move || {
            let result = f(&resources);
            // The handle goes back before the task counts out, so
            // shutdown finds itself holding the last one.
            drop(resources);
            drop(running);
            result
        });
        self.join(task).await
    }

    /// Run in-memory work on the runtime's blocking pool, so a caller's
    /// thread never does it (spec/07 "One runtime": nothing blocks the
    /// caller's thread) — the window table's lock and a 500,000-id
    /// index included. Counted like reads, so shutdown waits for it.
    pub(crate) async fn compute<T, F>(&self, f: F) -> Result<T, EngineError>
    where
        T: Send + 'static,
        F: FnOnce() -> Result<T, EngineError> + Send + 'static,
    {
        let running = self.blocking.start()?;
        let task = self.runtime.handle().spawn_blocking(move || {
            let result = f();
            drop(running);
            result
        });
        task.await
            .map_err(|join| join_failed("run on the runtime", join))?
    }

    /// A handle for cleanup no caller waits for, run like `compute` on
    /// the blocking pool — see `Detached`.
    pub(crate) fn detached(&self) -> Detached {
        Detached {
            handle: self.runtime.handle().clone(),
            blocking: Arc::clone(&self.blocking),
        }
    }

    /// Await one blocking task, mapping a failed join to `Internal` and
    /// the task's own store failure through `EngineError`.
    pub(crate) async fn join<T>(
        &self,
        handle: JoinHandle<Result<T, sb_resources::StoreError>>,
    ) -> Result<T, EngineError> {
        handle
            .await
            .map_err(|join| join_failed("run on the runtime", join))?
            .map_err(EngineError::from)
    }
}

/// Start the worker that closes the store of an engine dropped without
/// `shutdown`, waiting for its blocking tasks first. Reserved at open,
/// so a drop never needs a new thread; an OS that refuses it fails the
/// open as `Unavailable`.
fn start_cleanup_worker() -> Result<std::sync::mpsc::Sender<Cleanup>, EngineError> {
    let (cleanup, handed) = std::sync::mpsc::channel::<Cleanup>();
    std::thread::Builder::new()
        .name(CLOSE_THREAD_NAME.to_owned())
        .spawn(move || {
            // A closed channel means the engine shut down normally.
            if let Ok((resources, blocking, leases)) = handed.recv() {
                blocking.close_and_wait();
                leases.close();
                let _ = close_resources(resources);
            }
        })
        .map_err(|e| EngineError::unavailable(format!("cannot start the cleanup worker: {e}")))?;
    Ok(cleanup)
}

/// Close the store through its last handle. Only the engine's
/// shutdown, after every blocking task ended, calls this.
fn close_resources(resources: Arc<Resources>) -> Result<(), sb_resources::StoreError> {
    match Arc::try_unwrap(resources) {
        Ok(resources) => resources.close(),
        Err(_) => Err(sb_resources::StoreError::internal(
            "a store handle outlived every blocking task",
        )),
    }
}

impl Drop for Engine {
    /// An engine dropped without `shutdown` (a cancelled future, a
    /// panic) hands its store to the cleanup worker reserved at open,
    /// which waits for the reads and closes it: closing joins the writer
    /// thread, which mustn't block an async caller, and dropping needs
    /// no new thread. A process exiting first is an unclean end, which
    /// the next open's `quick_check` covers (spec/03).
    fn drop(&mut self) {
        self.windows.shut();
        let Some(resources) = self.resources.take() else {
            return;
        };
        let _ = self
            .cleanup
            .send((resources, Arc::clone(&self.blocking), self.leases.clone()));
    }
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Engine")
    }
}

/// A blocking task failed to run to completion: it was cancelled or
/// panicked, which the engine treats as an internal failure.
pub(crate) fn join_failed(what: &str, join: JoinError) -> EngineError {
    let why = if join.is_panic() {
        "panicked"
    } else {
        "was cancelled"
    };
    EngineError::internal(format!("the task to {what} {why}: {join}"))
}

#[cfg(test)]
mod tests;
