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
use sb_resources::Resources;

/// The name of the thread a dropped engine closes its store on.
const CLOSE_THREAD_NAME: &str = "sb-engine-close";

/// The engine: one multi-threaded runtime and the store's one handle
/// (spec/01). Every operation goes through here; nothing else in the
/// core owns a thread.
///
/// Field order matters on drop: the store handle goes to the runtime's
/// blocking pool first (closing a store joins its writer thread, which
/// must not block an async caller), then the runtime shuts down in the
/// background.
pub struct Engine {
    resources: Option<Arc<Resources>>,
    blocking: Arc<BlockingWork>,
    runtime: OwnedRuntime,
}

/// The runtime, shut down without blocking however it is dropped: an
/// engine, or an `open` or `shutdown` future, may be dropped inside
/// another runtime's async context, where dropping a `Runtime` would
/// block and panic.
struct OwnedRuntime(Option<Runtime>);

impl OwnedRuntime {
    fn start() -> Result<Self, EngineError> {
        Runtime::new()
            .map(|runtime| Self(Some(runtime)))
            .map_err(|e| EngineError::internal(format!("cannot start the engine's runtime: {e}")))
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
    /// Open the store at `data_root` and start the runtime that owns
    /// the work around it (spec/07 "Lifecycle"). The open itself runs
    /// on the runtime's blocking pool, so awaiting this future never
    /// blocks the caller's thread; dropping the future midway shuts the
    /// fresh runtime down in the background.
    pub async fn open(data_root: impl AsRef<Path>) -> Result<Self, EngineError> {
        let data_root = data_root.as_ref().to_path_buf();
        let runtime = OwnedRuntime::start()?;
        // The whole engine is built on the blocking pool, so a finished
        // open whose result is never polled drops an `Engine`, whose own
        // `Drop` closes the store off the caller's thread, never a bare
        // store that would close inline.
        let handle = runtime.handle().clone();
        let opened = handle
            .spawn_blocking(move || {
                Resources::open(&data_root).map(|resources| Self {
                    resources: Some(Arc::new(resources)),
                    blocking: Arc::default(),
                    runtime,
                })
            })
            .await;
        match opened {
            Ok(engine) => engine.map_err(EngineError::from),
            Err(join) => Err(join_failed("open the store", join)),
        }
    }

    /// The store handle; present until `shutdown` or drop takes it.
    pub(crate) fn resources(&self) -> &Arc<Resources> {
        self.resources
            .as_ref()
            .expect("the store handle lives until shutdown")
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
        let resources = self
            .resources
            .take()
            .expect("the store handle lives until shutdown");
        let blocking = Arc::clone(&self.blocking);
        let closing = self.runtime.handle().spawn_blocking(move || {
            blocking.close_and_wait();
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
    /// panic) still closes its store once its reads end, on a detached
    /// thread of its own: closing joins the writer thread, which mustn't
    /// block an async caller, and the runtime is shutting down, so its
    /// blocking pool might never start the task. If no thread can be
    /// started, or the process exits first, the end is unclean and the
    /// next open's `quick_check` covers it (spec/03).
    fn drop(&mut self) {
        let Some(resources) = self.resources.take() else {
            return;
        };
        let blocking = Arc::clone(&self.blocking);
        let _ = std::thread::Builder::new()
            .name(CLOSE_THREAD_NAME.to_owned())
            .spawn(move || {
                blocking.close_and_wait();
                let _ = close_resources(resources);
            });
    }
}

impl std::fmt::Debug for Engine {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Engine")
    }
}

/// A blocking task failed to run to completion: it was cancelled or
/// panicked, which the engine treats as an internal failure.
fn join_failed(what: &str, join: JoinError) -> EngineError {
    let why = if join.is_panic() {
        "panicked"
    } else {
        "was cancelled"
    };
    EngineError::internal(format!("the task to {what} {why}: {join}"))
}

#[cfg(test)]
mod tests;
