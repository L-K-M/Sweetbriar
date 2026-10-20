//! One counted publication worker. Pokes survive a pass/park handoff;
//! only shutdown ends a parked worker (spec/06 runtime publication).

#[cfg(test)]
use std::sync::Arc;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

use sb_resources::{
    IndexFence, IndexPreparation, IndexReadFailure, IndexReadStage, ObservedAvailability,
    RebuildPhase, Resources, SEARCH_EXTRACTOR_VERSION, SearchIndexStatus, SearchIoCode, StoreError,
    Timestamp, Transaction, WritePriority, begin_index_retry, commit_index_recovery,
    latest_rebuild, note_observed_availability, pause_for_io_if_current, publish_batch,
    record_index_attempt_success, search_index_status, search_io_code, search_io_message,
};

use crate::Engine;
use crate::engine::Spawn;

#[derive(Default)]
pub(crate) struct IndexJobs {
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
    failure: Option<ReadFailure>,
}

#[derive(Clone)]
enum ReadFailure {
    Writer(u32),
    Invariant(StoreError),
}

enum Next {
    More,
    Park(Option<Timestamp>),
}

#[cfg(test)]
pub(crate) type Hook = Arc<dyn Fn(Event) + Send + Sync>;

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    Captured,
    Prepared { documents: usize, bytes: usize },
    Committed,
    Recovered,
    RecoveryFailed,
    Parked,
}

impl IndexJobs {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state.lock().expect("the index worker state poisoned")
    }

    pub(crate) fn wake(&self) {
        self.lock().again = true;
        self.ready.notify_all();
    }

    pub(crate) fn stop(&self) {
        self.lock().stopping = true;
        self.ready.notify_all();
    }

    pub(super) fn stopped(&self) -> bool {
        self.lock().stopping
    }

    fn failure(&self) -> Option<ReadFailure> {
        self.lock().failure.clone()
    }

    /// The read gate shared by worker status and later Search consumers.
    /// A recovery commit, not a wake or an elapsed deadline, releases it.
    pub(crate) fn status(&self, resources: &Resources) -> Result<SearchIndexStatus, StoreError> {
        match self.failure() {
            Some(ReadFailure::Writer(_)) => Err(StoreError::Unavailable {
                reason: "Index status could not be saved. Restore writable local storage; indexing retries automatically.".into(),
                io_kind: None,
            }),
            Some(ReadFailure::Invariant(error)) => Err(error),
            None => resources.read(|tx| search_index_status(tx)),
        }
    }

    fn write<T: Send + 'static>(
        &self,
        resources: &Resources,
        body: impl FnOnce(&Transaction) -> Result<T, StoreError> + Send + 'static,
    ) -> Result<T, StoreError> {
        let result =
            tokio::runtime::Handle::current().block_on(resources.write(WritePriority::Index, body));
        if let Err(error) = &result {
            let mut state = self.lock();
            let attempts = match state.failure {
                Some(ReadFailure::Writer(attempts)) => attempts.saturating_add(1),
                _ => 1,
            };
            state.failure = Some(
                if matches!(
                    error,
                    StoreError::Unavailable { .. } | StoreError::Internal { .. }
                ) {
                    ReadFailure::Writer(attempts)
                } else {
                    ReadFailure::Invariant(error.clone())
                },
            );
        }
        result
    }

    // Called only after an actual successful publication/history commit,
    // or the dedicated recovery status commit, never merely after a read.
    fn recovered(&self) {
        self.lock().failure = None;
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
                self.ready
                    .wait_timeout(state, Duration::from_millis(remaining as u64))
                    .expect("the index wait poisoned")
                    .0
            } else {
                self.ready.wait(state).expect("the index wait poisoned")
            };
        }
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
    pub(crate) fn schedule_index(&self) {
        let index = self.indexer().clone();
        {
            let mut state = index.lock();
            if state.running || state.stopping {
                return;
            }
            state.running = true;
        }
        let owner = index.clone();
        if self
            .spawn_worker(move |worker| async move {
                let stop = worker.shutdown_check();
                let registry = owner.clone();
                let _ = worker
                    .blocking(move |resources| run(resources, &registry, &stop))
                    .await;
                owner.lock().running = false;
            })
            .is_err()
        {
            index.lock().running = false;
        }
    }
}

fn run(resources: &Resources, index: &IndexJobs, shutting_down: &dyn Fn() -> bool) {
    let stop = || index.stopped() || shutting_down();
    while !stop() {
        // A poke before this point is covered by the fresh capture; a
        // later poke remains set through the following park handoff.
        index.lock().again = false;
        let result = turn(resources, index, &stop).or_else(|error| {
            if !matches!(index.failure(), Some(ReadFailure::Writer(_)))
                && let Some(code) = search_io_code(&error)
            {
                return pause_io(resources, index, None, code);
            }
            Err(error)
        });
        let next = match result {
            Ok(next) => next,
            Err(error) => match index.failure() {
                Some(ReadFailure::Writer(attempts)) => {
                    Next::Park(Some(retry_at(resources.now(), attempts)))
                }
                _ => {
                    index.lock().failure = Some(ReadFailure::Invariant(error));
                    Next::Park(None)
                }
            },
        };
        if let Next::Park(until) = next {
            #[cfg(test)]
            index.event(Event::Parked);
            index.wait(resources, until);
        }
    }
}

fn retry_at(now: Timestamp, attempts: u32) -> Timestamp {
    let milliseconds = (1_000i64 << attempts.saturating_sub(1).min(6)).min(60_000);
    Timestamp::from_millis(now.as_millis().saturating_add(milliseconds))
}

#[cfg(test)]
#[test]
fn backoff_caps_and_timestamp_saturates() {
    for (attempts, delay) in [(1, 1_000), (2, 2_000), (7, 60_000), (u32::MAX, 60_000)] {
        assert_eq!(
            retry_at(Timestamp::from_millis(1), attempts).as_millis(),
            delay + 1
        );
    }
    assert_eq!(
        retry_at(Timestamp::from_millis(i64::MAX), 3).as_millis(),
        i64::MAX
    );
}

#[cfg(test)]
#[test]
fn metadata_faults_back_off_and_begin_due_retries_through_the_writer() {
    use sb_test_support::{FixedClock, ScratchDir, block_on, real_fs};
    let runtime = tokio::runtime::Runtime::new().unwrap();
    let _entered = runtime.enter();
    let scratch = ScratchDir::new("index-metadata-policy");
    let now = Timestamp::from_millis(10_000);
    let resources = Resources::open_with_clock(
        scratch.path().join("store").as_path(),
        real_fs(),
        Arc::new(FixedClock::new(now)),
    )
    .unwrap();
    let index = IndexJobs::default();
    assert!(matches!(
        pause_io(&resources, &index, None, SearchIoCode::StorageFull),
        Ok(Next::More)
    ));
    let generation = resources
        .read(|tx| sb_resources::search_generation(tx))
        .unwrap();
    assert!(matches!(
        pause_io(&resources, &index, None, SearchIoCode::StorageFull),
        Ok(Next::Park(Some(_)))
    ));
    assert_eq!(
        resources
            .read(|tx| sb_resources::search_generation(tx))
            .unwrap(),
        generation
    );
    block_on(resources.write(WritePriority::User, move |tx| {
        sb_resources::request_index_retry(tx, now)
    }))
    .unwrap();
    assert!(index.status(&resources).unwrap().io_pause().is_some());
    assert!(matches!(
        pause_io(&resources, &index, None, SearchIoCode::StorageFull),
        Ok(Next::More)
    ));
    assert!(index.status(&resources).unwrap().io_pause().is_none());
    assert_eq!(index.status(&resources).unwrap().io_attempts(), 1);
    assert!(matches!(
        pause_io(&resources, &index, None, SearchIoCode::StorageFull),
        Ok(Next::More)
    ));
    assert_eq!(
        index
            .status(&resources)
            .unwrap()
            .io_pause()
            .unwrap()
            .retry_at()
            .as_millis(),
        12_000
    );
    resources.close().unwrap();
}

fn turn(
    resources: &Resources,
    index: &IndexJobs,
    stop: &dyn Fn() -> bool,
) -> Result<Next, StoreError> {
    if matches!(index.failure(), Some(ReadFailure::Writer(_))) {
        let recovery = index.write(resources, commit_index_recovery);
        #[cfg(test)]
        if recovery.is_err() {
            index.event(Event::RecoveryFailed);
        }
        recovery?;
        #[cfg(test)]
        index.event(Event::Recovered);
        index.recovered();
    }
    // An invariant gets no timed retry. A normal producer wake may
    // revalidate/reprocess repaired inputs, while readers remain gated.
    let status = if matches!(index.failure(), Some(ReadFailure::Invariant(_))) {
        resources.read(|tx| search_index_status(tx))?
    } else {
        index.status(resources)?
    };
    let phase = resources.read(|tx| latest_rebuild(tx).map(|job| job.map(|job| job.phase())))?;
    if status.user_paused()
        || matches!(phase, Some(RebuildPhase::Reset | RebuildPhase::Enqueue))
        || stop()
    {
        return Ok(Next::Park(None));
    }
    if let Some(pause) = status.io_pause() {
        let now = resources.now();
        if pause.retry_at().as_millis() > now.as_millis() {
            return Ok(Next::Park(Some(pause.retry_at())));
        }
        let fence = IndexFence {
            epoch: status.epoch(),
            version: SEARCH_EXTRACTOR_VERSION,
        };
        return index
            .write(resources, move |tx| begin_index_retry(tx, fence, now))
            .map(|began| if began { Next::More } else { Next::Park(None) });
    }
    let batch = resources.pending_index_batch()?;
    #[cfg(test)]
    index.event(Event::Captured);
    if batch.fence.version != SEARCH_EXTRACTOR_VERSION {
        return Ok(Next::Park(None));
    }
    if batch.documents.is_empty() {
        return Ok(Next::Park(None));
    }
    let prepared = match resources.prepare_index_batch(batch, stop)? {
        IndexPreparation::Prepared(prepared) => prepared,
        IndexPreparation::Stale | IndexPreparation::Blocked(_) => return Ok(Next::More),
        IndexPreparation::Cancelled | IndexPreparation::BlockedNewerExtractor { .. } => {
            return Ok(Next::Park(None));
        }
    };
    let (fence, publications, failure) = prepared.into_parts();
    #[cfg(test)]
    index.event(Event::Prepared {
        documents: publications.len(),
        bytes: publications
            .iter()
            .filter_map(|item| item.document.as_ref())
            .map(|doc| doc.column_bytes())
            .sum(),
    });
    if stop() {
        return Ok(Next::Park(None));
    }
    let accepted = if publications.is_empty() {
        0
    } else {
        let accepted = index.write(resources, move |tx| publish_batch(tx, fence, &publications))?;
        #[cfg(test)]
        index.event(Event::Committed);
        accepted
    };
    if let Some(failure) = failure {
        return handle_failure(resources, index, fence, failure);
    } else if accepted > 0 {
        index.write(resources, move |tx| record_index_attempt_success(tx, fence))?;
        index.recovered();
    }
    Ok(Next::More)
}

fn handle_failure(
    resources: &Resources,
    index: &IndexJobs,
    fence: IndexFence,
    failure: IndexReadFailure,
) -> Result<Next, StoreError> {
    let (pending, stage, error) = failure.into_parts();
    let now = resources.now();
    if stage == IndexReadStage::Read {
        let observed = match error {
            StoreError::NotFound { .. } => Some(ObservedAvailability::Missing),
            StoreError::Corrupt { .. } => Some(ObservedAvailability::Quarantined),
            _ => None,
        };
        if let Some(observed) = observed {
            index.write(resources, move |tx| {
                note_observed_availability(tx, &pending, observed, now)
            })?;
            return Ok(Next::More);
        }
    }
    if let Some(code) = search_io_code(&error) {
        return pause_io(resources, index, Some((fence, pending)), code);
    }
    let current = resources.pending_index_batch()?;
    if current.fence == fence
        && current.documents.iter().any(|token| {
            token.doc_id == pending.doc_id
                && token.revision == pending.revision
                && token.reason == pending.reason
        })
    {
        return Err(error);
    }
    Ok(Next::More)
}

/// Metadata-read faults have no document token. The writer rechecks
/// current control state, so an unavailable reader can still back off.
fn pause_io(
    resources: &Resources,
    index: &IndexJobs,
    context: Option<(IndexFence, sb_resources::PendingDocument)>,
    code: SearchIoCode,
) -> Result<Next, StoreError> {
    let now = resources.now();
    index.write(resources, move |tx| {
        let status = search_index_status(tx)?;
        if status.user_paused()
            || latest_rebuild(tx)?.is_some_and(|job| {
                matches!(job.phase(), RebuildPhase::Reset | RebuildPhase::Enqueue)
            })
        {
            return Ok(Next::Park(None));
        }
        let fence = context.as_ref().map_or(
            IndexFence {
                epoch: status.epoch(),
                version: SEARCH_EXTRACTOR_VERSION,
            },
            |(fence, _)| *fence,
        );
        if let Some(pause) = status.io_pause() {
            if pause.retry_at().as_millis() > now.as_millis() {
                return Ok(Next::Park(Some(pause.retry_at())));
            }
            if context.is_some() || begin_index_retry(tx, fence, now)? {
                return Ok(Next::More);
            }
            return Ok(Next::Park(None));
        }
        let attempts = u32::try_from(status.io_attempts())
            .unwrap_or(u32::MAX)
            .saturating_add(1);
        let changed = pause_for_io_if_current(
            tx,
            fence,
            context.as_ref().map(|(_, pending)| pending),
            code,
            search_io_message(code),
            retry_at(now, attempts),
            attempts,
        )?;
        Ok(if changed {
            Next::More
        } else {
            Next::Park(None)
        })
    })
}
