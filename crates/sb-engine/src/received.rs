//! One counted current-summary sweep, independent of Search holds and failures.

#[cfg(test)]
use std::sync::Arc;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

use sb_resources::{ReceivedCommit, Resources, StoreError, Timestamp, index_retry_at};

use crate::Engine;
use crate::engine::Spawn;

#[derive(Default)]
pub(crate) struct ReceivedJobs {
    state: Mutex<State>,
    ready: Condvar,
    #[cfg(test)]
    pub(crate) hook: Mutex<Option<Hook>>,
}

#[derive(Default)]
struct State {
    running: bool,
    again: bool,
    stopping: bool,
    failure: Option<Failure>,
    #[cfg(test)]
    waiting: bool,
    #[cfg(test)]
    starts: usize,
}

#[derive(Clone)]
pub(crate) struct Failure {
    pub(crate) error: StoreError,
    pub(crate) attempts: u32,
}

enum Next {
    More,
    Park,
}

fn retryable(error: &StoreError) -> bool {
    error.io_kind().is_some() || matches!(error, StoreError::Internal { .. })
}

#[cfg(test)]
type Hook = Arc<dyn Fn(Event) + Send + Sync>;

#[cfg(test)]
#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Event {
    Prepared,
    Submitted,
    Committed,
    Complete,
    Parked,
}

impl ReceivedJobs {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .expect("the received worker state poisoned")
    }

    pub(crate) fn wake(&self) {
        self.lock().again = true;
        self.ready.notify_all();
    }

    pub(crate) fn stop(&self) {
        self.lock().stopping = true;
        self.ready.notify_all();
    }

    pub(crate) fn stopped(&self) -> bool {
        self.lock().stopping
    }

    pub(crate) fn failure(&self) -> Option<Failure> {
        self.lock().failure.clone()
    }

    fn failed(&self, error: StoreError) {
        let mut state = self.lock();
        let attempts = state
            .failure
            .as_ref()
            .filter(|failure| retryable(&failure.error) == retryable(&error))
            .map_or(1, |failure| failure.attempts.saturating_add(1));
        state.failure = Some(Failure { error, attempts });
    }

    fn recovered(&self) {
        self.lock().failure = None;
    }

    pub(crate) fn retry_at(&self, resources: &Resources) -> Option<Timestamp> {
        self.failure().and_then(|failure| {
            retryable(&failure.error).then(|| index_retry_at(resources.now(), failure.attempts))
        })
    }

    fn wait(&self, resources: &Resources, until: Option<Timestamp>) {
        let mut state = self.lock();
        while !state.stopping && !state.again {
            state = if let Some(until) = until {
                let remaining = until
                    .as_millis()
                    .saturating_sub(resources.now().as_millis());
                if remaining <= 0 {
                    return;
                }
                #[cfg(test)]
                {
                    state.waiting = true;
                }
                self.ready
                    .wait_timeout(state, Duration::from_millis(remaining as u64))
                    .expect("the received wait poisoned")
                    .0
            } else {
                #[cfg(test)]
                {
                    state.waiting = true;
                }
                self.ready.wait(state).expect("the received wait poisoned")
            };
            #[cfg(test)]
            {
                state.waiting = false;
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn waiting(&self) -> bool {
        let state = self.lock();
        state.waiting && !state.again
    }

    #[cfg(test)]
    pub(crate) fn starts(&self) -> usize {
        self.lock().starts
    }

    #[cfg(test)]
    fn event(&self, event: Event) {
        let hook = self.hook.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook(event);
        }
    }
}

impl Engine {
    pub(crate) fn schedule_received(&self) {
        let received = self.received().clone();
        {
            let mut state = received.lock();
            if state.running || state.stopping {
                return;
            }
            state.running = true;
        }
        let owner = received.clone();
        if self
            .spawn_worker(move |worker| async move {
                let stop = worker.shutdown_check();
                let registry = owner.clone();
                let result = worker
                    .blocking(move |resources| run(resources, &registry, &stop))
                    .await;
                if result.is_err() {
                    owner.failed(StoreError::internal(
                        "The received-address worker failed. Restart the app to retry.",
                    ));
                }
                owner.lock().running = false;
            })
            .is_err()
        {
            received.lock().running = false;
        }
    }
}

fn run(resources: &Resources, received: &ReceivedJobs, shutting_down: &dyn Fn() -> bool) {
    #[cfg(test)]
    {
        received.lock().starts += 1;
    }
    let stop = || received.stopped() || shutting_down();
    while !stop() {
        // Earlier pokes are covered by begin's fresh read; later ones survive wait.
        received.lock().again = false;
        let next = match turn(resources, received, &stop) {
            Ok(next) => next,
            Err(error) => {
                received.failed(error);
                Next::Park
            }
        };
        if let Next::Park = next {
            let until = received.retry_at(resources);
            #[cfg(test)]
            received.event(Event::Parked);
            received.wait(resources, until);
        }
    }
}

fn turn(
    resources: &Resources,
    received: &ReceivedJobs,
    stop: &dyn Fn() -> bool,
) -> Result<Next, StoreError> {
    let Some(mut sweep) = resources.begin_received_sweep()? else {
        received.recovered();
        return Ok(Next::Park);
    };
    loop {
        if stop() {
            return Ok(Next::Park);
        }
        let prepared = resources.prepare_received_step(sweep)?;
        #[cfg(test)]
        received.event(Event::Prepared);
        if stop() {
            return Ok(Next::Park);
        }
        let pending = resources.commit_received_step(prepared);
        #[cfg(test)]
        received.event(Event::Submitted);
        let committed = tokio::runtime::Handle::current().block_on(pending)?;
        received.recovered();
        #[cfg(test)]
        received.event(Event::Committed);
        match committed {
            ReceivedCommit::Continue(next) | ReceivedCommit::Recapture(next) => sweep = next,
            ReceivedCommit::Complete => {
                #[cfg(test)]
                received.event(Event::Complete);
                return Ok(Next::More);
            }
        }
    }
}
