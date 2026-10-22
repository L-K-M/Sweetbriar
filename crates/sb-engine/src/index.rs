//! One counted publication worker, now also driving bounded rebuild
//! phases (spec/06 "Runtime rebuild driver"). Pokes survive a
//! pass/park handoff; only shutdown ends a parked worker.

#[cfg(test)]
use std::sync::Arc;
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

use sb_resources::{
    IndexFence, IndexIoContext, IndexIoUpdate, IndexPreparation, IndexReadFailure, IndexReadStage,
    MAX_INDEX_BATCH, ObservedAvailability, RebuildPhase, Resources, SEARCH_EXTRACTOR_VERSION,
    SearchIndexStatus, StoreError, Timestamp, Transaction, WritePriority, begin_index_retry,
    commit_index_recovery, complete_rebuild, index_retry_at, latest_rebuild,
    note_observed_availability, publish_batch, rebuild_enqueue_batch, rebuild_job,
    rebuild_reset_batch, record_index_attempt_success, recover_index_io_failure,
    report_index_io_failure, search_index_status, search_io_code, target_version,
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
    #[cfg(test)]
    waiting: bool,
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

impl From<IndexIoUpdate> for Next {
    fn from(update: IndexIoUpdate) -> Self {
        match update {
            IndexIoUpdate::Blocked => Self::Park(None),
            IndexIoUpdate::Paused { retry_at } => Self::Park(Some(retry_at)),
            IndexIoUpdate::RetryBegun | IndexIoUpdate::Stale => Self::More,
        }
    }
}

#[cfg(test)]
pub(crate) type Hook = Arc<dyn Fn(Event) + Send + Sync>;

#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    SearchReadCaptured,
    SearchRegistered { window_id: u64 },
    Captured,
    Prepared { documents: usize, bytes: usize },
    Committed,
    Recovered,
    Recovering,
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

    /// Observe a real Condvar wait with no outstanding producer wake.
    #[cfg(test)]
    pub(crate) fn waiting(&self) -> bool {
        let state = self.lock();
        state.waiting && !state.again
    }

    fn failure(&self) -> Option<ReadFailure> {
        self.lock().failure.clone()
    }

    /// The read gate shared by worker status and later Search consumers.
    /// A recovery commit, not a wake or an elapsed deadline, releases it.
    pub(crate) fn status(&self, resources: &Resources) -> Result<SearchIndexStatus, StoreError> {
        self.readable()?;
        resources.read(|tx| search_index_status(tx))
    }

    /// In-memory failure fence, without opening a second metadata snapshot.
    pub(crate) fn readable(&self) -> Result<(), StoreError> {
        match self.failure() {
            Some(ReadFailure::Writer(_)) => Err(StoreError::Unavailable {
                reason: "Index status could not be saved. Restore writable local storage; indexing retries automatically.".into(),
                io_kind: None,
            }),
            Some(ReadFailure::Invariant(error)) => Err(error),
            None => Ok(()),
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
                #[cfg(test)]
                {
                    state.waiting = true;
                }
                self.ready
                    .wait_timeout(state, Duration::from_millis(remaining as u64))
                    .expect("the index wait poisoned")
                    .0
            } else {
                #[cfg(test)]
                {
                    state.waiting = true;
                }
                self.ready.wait(state).expect("the index wait poisoned")
            };
            #[cfg(test)]
            {
                state.waiting = false;
            }
        }
    }

    #[cfg(test)]
    pub(crate) fn event(&self, event: Event) {
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
                let result = worker
                    .blocking(move |resources| run(resources, &registry, &stop))
                    .await;
                let mut state = owner.lock();
                if result.is_err() {
                    state.failure = Some(ReadFailure::Invariant(StoreError::internal(
                        "The index publication worker failed. Restart the app to retry.",
                    )));
                }
                state.running = false;
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
                && search_io_code(&error).is_some()
            {
                return pause_io(resources, index, None, error);
            }
            Err(error)
        });
        let next = match result {
            Ok(next) => next,
            Err(error) => match index.failure() {
                Some(ReadFailure::Writer(attempts)) => {
                    Next::Park(Some(index_retry_at(resources.now(), attempts)))
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
    // The worker answers through the same status gate unless an
    // invariant is latched; the consistent read then supplies status,
    // latest job and stored target together.
    if !matches!(index.failure(), Some(ReadFailure::Invariant(_))) {
        index.status(resources)?;
    }
    // An active job's epoch must agree with the singleton, and
    // contradictory persistent metadata is an explicit Corrupt, never
    // an unchanged zero-progress busy loop.
    let (status, job, target) = resources.read(|tx| {
        let status = search_index_status(tx)?;
        let job = latest_rebuild(tx)?;
        if job.as_ref().is_some_and(|job| {
            job.phase() != RebuildPhase::Complete && job.epoch() != status.epoch()
        }) {
            return Err(StoreError::Corrupt {
                detail: "the active rebuild epoch disagrees with index status".into(),
            });
        }
        Ok((status, job, target_version(tx)?))
    })?;
    // Capability first, then the durable holds: an unsupported target
    // is never extracted, driven or downgraded.
    if stop() || target != SEARCH_EXTRACTOR_VERSION || status.user_paused() {
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
    // I/O gating came before ANY phase write; each bounded step is a
    // separate Index-priority submission under the fence it was read
    // under, so foreground User/Sync writes interleave between turns.
    let fence = IndexFence {
        epoch: status.epoch(),
        version: target,
    };
    if let Some(job) = job
        .as_ref()
        .filter(|job| job.phase() != RebuildPhase::Complete)
    {
        let id = job.id().to_owned();
        match job.phase() {
            RebuildPhase::Reset => {
                return drive(index, resources, fence, move |tx| {
                    let before = rebuild_job(tx, &id)?.phase();
                    let rows = rebuild_reset_batch(tx, &id, fence.epoch, MAX_INDEX_BATCH)?;
                    Ok(rows > 0 || rebuild_job(tx, &id)?.phase() != before)
                });
            }
            RebuildPhase::Enqueue => {
                let now = resources.now();
                return drive(index, resources, fence, move |tx| {
                    let before = rebuild_job(tx, &id)?.phase();
                    let rows = rebuild_enqueue_batch(tx, &id, fence.epoch, MAX_INDEX_BATCH, now)?;
                    Ok(rows > 0 || rebuild_job(tx, &id)?.phase() != before)
                });
            }
            _ => {}
        }
    }
    let batch = resources.pending_index_batch()?;
    #[cfg(test)]
    index.event(Event::Captured);
    if batch.fence.version != SEARCH_EXTRACTOR_VERSION {
        return Ok(Next::Park(None));
    }
    if batch.documents.is_empty() {
        // Completion shares its empty-queue observation with the gated
        // job check in ONE writer transaction; whether it is accepted
        // or rejected, recapture — never a blind park that strands a
        // producer behind a stale snapshot.
        let Some(job) = job.filter(|job| job.phase() == RebuildPhase::Index) else {
            return Ok(Next::Park(None));
        };
        let id = job.id().to_owned();
        let epoch = batch.fence.epoch;
        return drive(index, resources, batch.fence, move |tx| {
            complete_rebuild(tx, &id, epoch)
        });
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
        let accepted =
            match index.write(resources, move |tx| publish_batch(tx, fence, &publications)) {
                Ok(accepted) => accepted,
                Err(error) if search_io_code(&error).is_some() => {
                    return recover_io(index, resources, fence, error);
                }
                Err(error) => return Err(error),
            };
        #[cfg(test)]
        index.event(Event::Committed);
        accepted
    };
    if let Some(failure) = failure {
        return handle_failure(resources, index, fence, failure);
    } else if accepted > 0 {
        return record_success(index, resources, fence);
    }
    Ok(Next::More)
}

/// One bounded rebuild phase step under the fence it was captured
/// under. The store operation rechecks epoch, target, phase and both
/// holds inside the writer transaction, so a stale or newly held call
/// commits nothing and the caller simply recaptures. Only committed
/// work resets retry history — the success record stays gated on a
/// still-current fence, never written for a probe or a stale/held
/// operation.
fn drive(
    index: &IndexJobs,
    resources: &Resources,
    fence: IndexFence,
    step: impl FnOnce(&Transaction) -> Result<bool, StoreError> + Send + 'static,
) -> Result<Next, StoreError> {
    match index.write(resources, step) {
        Ok(changed) => {
            if changed {
                #[cfg(test)]
                index.event(Event::Committed);
                return record_success(index, resources, fence);
            }
            Ok(Next::More)
        }
        Err(error) if search_io_code(&error).is_some() => {
            recover_io(index, resources, fence, error)
        }
        Err(error) => Err(error),
    }
}

/// Committed work does not make its history write infallible: retain
/// that write's captured cause through the same validated I/O recovery.
fn record_success(
    index: &IndexJobs,
    resources: &Resources,
    fence: IndexFence,
) -> Result<Next, StoreError> {
    match index.write(resources, move |tx| record_index_attempt_success(tx, fence)) {
        Ok(_) => {
            index.recovered();
            Ok(Next::More)
        }
        Err(error) if search_io_code(&error).is_some() => {
            recover_io(index, resources, fence, error)
        }
        Err(error) => Err(error),
    }
}

/// A failed phase or publication write's typed I/O report under its
/// ORIGINAL captured fence, before the volatile gate may clear — never
/// a fresh-fence metadata replay.
fn recover_io(
    index: &IndexJobs,
    resources: &Resources,
    fence: IndexFence,
    error: StoreError,
) -> Result<Next, StoreError> {
    let now = resources.now();
    #[cfg(test)]
    index.event(Event::Recovering);
    let update = index.write(resources, move |tx| {
        recover_index_io_failure(tx, IndexIoContext::Batch { fence }, &error, now)
    })?;
    #[cfg(test)]
    index.event(Event::Recovered);
    index.recovered();
    Ok(update.into())
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
    if search_io_code(&error).is_some() {
        return pause_io(resources, index, Some((fence, pending)), error);
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
    error: StoreError,
) -> Result<Next, StoreError> {
    let now = resources.now();
    index
        .write(resources, move |tx| {
            let context = match context.as_ref() {
                Some((fence, pending)) => IndexIoContext::Document {
                    fence: *fence,
                    pending,
                },
                None => IndexIoContext::Metadata,
            };
            report_index_io_failure(tx, context, &error, now)
        })
        .map(Next::from)
}
