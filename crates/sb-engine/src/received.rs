//! Fair counted population, activation and repair, independent of Search holds.

use std::future::Future;
#[cfg(test)]
use std::sync::Arc;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

use sb_resources::{
    CatchupCommit, CatchupPreparation, CatchupSweep, ReceivedCommit, ReceivedSweep, Resources,
    StoreError, Timestamp, index_retry_at,
};

use crate::Engine;
use crate::engine::Spawn;

mod repair;
use repair::Sources;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Lane {
    Population,
    Corpus,
    SourceRetry,
    SourcePark,
}
const LANES: usize = 4;
const CORPUS_PREPARATIONS_PER_TURN: usize = 200;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Step {
    Population,
    BeginCorpus,
    Corpus,
    BeginRepair,
    Repair,
    Diagnosis,
}

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
    failures: [Option<Failure>; LANES],
    #[cfg(test)]
    waiting: bool,
    #[cfg(test)]
    starts: usize,
}

#[derive(Clone)]
pub(crate) struct Failure {
    pub(crate) error: StoreError,
    pub(crate) attempts: u32,
    pub(crate) retry_at: Option<Timestamp>,
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
    BeforeWrite(Step),
    WriteSubmitted(Step),
    WriteCommitted(Step),
    SourcePass {
        inputs: u64,
        cutoff: i64,
    },
    SourceComplete,
    Failure(Lane),
    RepairBegin(i64),
    RepairPrepared,
    CatchupCommitted {
        processed: u32,
        activated: u32,
        bytes: u64,
    },
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

    #[cfg(test)]
    pub(crate) fn failure(&self) -> Option<Failure> {
        self.lock().failures.iter().find_map(Clone::clone)
    }

    pub(crate) fn failure_for(&self, lane: Lane) -> Option<Failure> {
        self.lock().failures[lane as usize].clone()
    }

    fn failed(&self, lane: Lane, error: StoreError, now: Timestamp) {
        let mut state = self.lock();
        let attempts = state.failures[lane as usize]
            .as_ref()
            .filter(|failure| retryable(&failure.error) == retryable(&error))
            .map_or(1, |failure| failure.attempts.saturating_add(1));
        let retry_at = retryable(&error).then(|| index_retry_at(now, attempts));
        state.failures[lane as usize] = Some(Failure {
            error,
            attempts,
            retry_at,
        });
        drop(state);
        #[cfg(test)]
        self.event(Event::Failure(lane));
    }

    fn recovered(&self, lane: Lane) {
        self.lock().failures[lane as usize] = None;
    }

    fn disarm(&self, lane: Lane) {
        if let Some(failure) = &mut self.lock().failures[lane as usize] {
            failure.retry_at = None;
        }
    }

    pub(crate) fn retry_at(&self) -> Option<Timestamp> {
        self.lock()
            .failures
            .iter()
            .filter_map(|failure| failure.as_ref()?.retry_at)
            .min_by_key(|time| time.as_millis())
    }

    fn retry_due(&self, lane: Lane, now: Timestamp) -> bool {
        self.failure_for(lane)
            .and_then(|failure| failure.retry_at)
            .is_some_and(|time| time <= now)
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
        let threads = self.imports().threads().clone();
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
                let spawner = worker.clone();
                let result = worker
                    .blocking(move |resources| {
                        run(resources, &registry, &stop, &|| {
                            crate::threads::schedule_threads(&spawner, &threads)
                        })
                    })
                    .await;
                if result.is_err() {
                    owner.failed(
                        Lane::Population,
                        StoreError::internal(
                            "The received-address worker failed. Restart the app to retry.",
                        ),
                        worker.resources().now(),
                    );
                    owner.lock().failures[Lane::Population as usize]
                        .as_mut()
                        .unwrap()
                        .retry_at = None;
                }
                owner.lock().running = false;
            })
            .is_err()
        {
            received.lock().running = false;
        }
    }
}

struct ThreadDispatch<'a> {
    pending: bool,
    schedule: &'a dyn Fn(),
}

impl ThreadDispatch<'_> {
    fn flush(&mut self) {
        if std::mem::take(&mut self.pending) {
            (self.schedule)();
        }
    }
}

fn run(
    resources: &Resources,
    received: &ReceivedJobs,
    shutting_down: &dyn Fn() -> bool,
    activated: &dyn Fn(),
) {
    #[cfg(test)]
    {
        received.lock().starts += 1;
    }
    let stop = || received.stopped() || shutting_down();
    let (mut population, mut corpus) = (None, None);
    let (mut population_due, mut corpus_due) = (true, true);
    let mut sources = Sources::default();
    let mut threads = ThreadDispatch {
        pending: false,
        schedule: activated,
    };
    while !stop() {
        // Commit clocks retain mid-pass changes; later pokes survive the park handoff.
        let woke = std::mem::take(&mut received.lock().again);
        let now = resources.now();
        population_due |= woke || received.retry_due(Lane::Population, now);
        corpus_due |= woke || received.retry_due(Lane::Corpus, now);
        let mut progress = false;
        if population_due && !stop() {
            match population_step(resources, received, &stop, &mut population) {
                Ok(worked) => {
                    population_due = worked;
                    progress |= worked;
                }
                Err(error) => {
                    population = None;
                    population_due = false;
                    received.failed(Lane::Population, error, resources.now());
                }
            }
        }
        if corpus_due && !stop() {
            match corpus_step(resources, received, &stop, &mut corpus, &mut threads) {
                Ok(worked) => {
                    corpus_due = worked;
                    progress |= worked;
                }
                Err(error) => {
                    threads.flush();
                    corpus = None;
                    corpus_due = false;
                    received.failed(Lane::Corpus, error, resources.now());
                }
            }
        }
        if !stop() {
            progress |= sources.turn(resources, received, &stop, woke);
        }
        if !progress && !stop() {
            let until = received.retry_at();
            #[cfg(test)]
            received.event(Event::Parked);
            received.wait(resources, until);
        }
    }
    threads.flush();
}

fn population_step(
    resources: &Resources,
    received: &ReceivedJobs,
    stop: &dyn Fn() -> bool,
    cursor: &mut Option<ReceivedSweep>,
) -> Result<bool, StoreError> {
    let captured = match cursor.take() {
        Some(sweep) => Some(sweep),
        None => resources.begin_received_sweep()?,
    };
    let sweep = match captured {
        Some(sweep) => sweep,
        None => {
            received.recovered(Lane::Population);
            return Ok(false);
        }
    };
    if stop() {
        return Ok(false);
    }
    let prepared = resources.prepare_received_step(sweep)?;
    #[cfg(test)]
    received.event(Event::Prepared);
    let Some(outcome) = commit(received, stop, Step::Population, || {
        resources.commit_received_step(prepared)
    })?
    else {
        return Ok(false);
    };
    match outcome {
        ReceivedCommit::Continue(next) | ReceivedCommit::Recapture(next) => *cursor = Some(next),
        ReceivedCommit::Complete => {
            #[cfg(test)]
            received.event(Event::Complete);
            *cursor = None;
        }
    }
    Ok(true)
}

fn corpus_step(
    resources: &Resources,
    jobs: &ReceivedJobs,
    stop: &dyn Fn() -> bool,
    cursor: &mut Option<CatchupSweep>,
    threads: &mut ThreadDispatch<'_>,
) -> Result<bool, StoreError> {
    let mut scan = match cursor.take() {
        Some(scan) => scan,
        None => {
            let Some(scan) = commit(jobs, stop, Step::BeginCorpus, || resources.begin_catchup())?
            else {
                return Ok(false);
            };
            *cursor = scan;
            if cursor.is_none() {
                jobs.recovered(Lane::Corpus);
            }
            return Ok(cursor.is_some());
        }
    };
    for _ in 0..CORPUS_PREPARATIONS_PER_TURN {
        if stop() {
            return Ok(false);
        }
        match resources.prepare_catchup_step(scan, stop)? {
            CatchupPreparation::Continue(next) => scan = next,
            CatchupPreparation::Recapture => {
                threads.flush();
                return Ok(true);
            }
            CatchupPreparation::Cancelled => return Ok(false),
            CatchupPreparation::Prepared(batch) => {
                let Some(outcome) = commit(jobs, stop, Step::Corpus, || {
                    resources.commit_catchup_batch(batch)
                })?
                else {
                    return Ok(false);
                };
                let report = match outcome {
                    CatchupCommit::Continue(next, report) => {
                        *cursor = Some(next);
                        Some(report)
                    }
                    CatchupCommit::Complete(report) => {
                        jobs.recovered(Lane::Corpus);
                        Some(report)
                    }
                    CatchupCommit::Recapture => None,
                };
                if let Some(report) = report {
                    threads.pending |= report.activated > 0;
                    #[cfg(test)]
                    jobs.event(Event::CatchupCommitted {
                        processed: report.processed,
                        activated: report.activated,
                        bytes: report.charged_bytes,
                    });
                }
                if cursor.is_none() {
                    threads.flush();
                }
                return Ok(true);
            }
        }
    }
    *cursor = Some(scan);
    Ok(true)
}

fn commit<T, F: Future<Output = Result<T, StoreError>>>(
    jobs: &ReceivedJobs,
    stop: &dyn Fn() -> bool,
    step: Step,
    submit: impl FnOnce() -> F,
) -> Result<Option<T>, StoreError> {
    #[cfg(test)]
    jobs.event(Event::BeforeWrite(step));
    if stop() {
        return Ok(None);
    }
    let submitted = submit();
    #[cfg(test)]
    {
        jobs.event(Event::WriteSubmitted(step));
        if step == Step::Population {
            jobs.event(Event::Submitted);
        }
    }
    let value = tokio::runtime::Handle::current().block_on(submitted)?;
    if step == Step::Population {
        jobs.recovered(Lane::Population);
    }
    #[cfg(test)]
    {
        jobs.event(Event::WriteCommitted(step));
        if step == Step::Population {
            jobs.event(Event::Committed);
        }
    }
    Ok(Some(value))
}
