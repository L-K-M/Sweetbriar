//! Import jobs (spec/09 "Job lifecycle"): the commands, the job query
//! and the two workers that do the long work off every caller's thread.
//!
//! ```text
//! start_import ─▶ inventory worker ─▶ awaiting_confirmation
//!                                           │ confirm_import
//!                                           ▼
//!                  ingest worker ◀─────── running ─▶ completed | failed
//!
//! cancel_job: cancelled at once, durably; a live worker's next commit
//! then conflicts and the worker ends.
//! ```

use std::collections::HashMap;
use std::future::Future;
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, MutexGuard};

use sb_resources::{
    AcquiredMessage, FlagExtraction, ImportInventory, ImportInventoryOutcome, ImportJobBatch,
    ImportJobCheckpoint, ImportJobRecord, ImportJobState, ImportMessage, MAX_IMPORT_BATCH,
    MAX_IMPORT_CHANGES_BYTES, MboxVariant, Receipt, Resources, SourceAcquisition, SourceInventory,
    SourcePosition, StoreError, WritePriority, advance_import_job, commit_import_job_batch,
    create_import_job, finished_import_jobs, import_job, import_job_source, import_job_sources,
    import_report, record_import_sources, recorded_import_job_sources, recoverable_import_jobs,
    run_once,
};

use crate::engine::{Spawn, Worker};
use crate::{Engine, EngineError};

/// The commands' names in `command_receipts` (spec/07).
const START_IMPORT: &str = "start_import";
const CONFIRM_IMPORT: &str = "confirm_import";
const CANCEL_JOB: &str = "cancel_job";

/// Imports are background work behind user and sync writes (spec/03).
const PRIORITY: WritePriority = WritePriority::Index;

/// Room a job checkpoint may grow within one batch (its totals' digits),
/// plus the source checkpoint: both share the batch's write budget.
const CHECKPOINT_SLACK: usize = 1024;

/// What a source row costs beyond its JSON: keys, checkpoint, framing.
const SOURCE_ROW_BYTES: usize = 256;

const INTERRUPTED_INVENTORY: &str = "the inventory was interrupted; start a new import";

/// Jobs with a live worker in this process. One worker owns a job at a
/// time; an ingest asked for meanwhile is handed to that worker before
/// it lets go, under the same lock, so no request is lost in a handoff.
#[derive(Default)]
pub(crate) struct ImportJobs {
    live: Mutex<HashMap<String, Live>>,
}

/// What a live worker's job has been asked since it started.
#[derive(Default)]
struct Live {
    ingest_requested: bool,
    cancelled: bool,
}

impl ImportJobs {
    fn lock(&self) -> MutexGuard<'_, HashMap<String, Live>> {
        self.live
            .lock()
            .expect("a panicking thread held the import job registry")
    }

    /// Tell a live worker its job was cancelled, so it stops early.
    fn cancel(&self, id: &str) {
        if let Some(live) = self.lock().get_mut(id) {
            live.cancelled = true;
        }
    }
}

/// What a worker does with its job.
enum Task {
    Inventory(Vec<PathBuf>),
    Ingest,
}

/// A worker's hold on its job, released at the worker's end.
struct Claim {
    jobs: Arc<ImportJobs>,
    id: String,
    held: bool,
}

impl Claim {
    /// Claim `id` for a new worker, or hand an ingest request to the
    /// worker that holds it.
    fn new(jobs: &Arc<ImportJobs>, id: &str, task: &Task) -> Option<Self> {
        let mut live = jobs.lock();
        if let Some(holder) = live.get_mut(id) {
            holder.ingest_requested |= matches!(task, Task::Ingest);
            return None;
        }
        live.insert(id.to_owned(), Live::default());
        Some(Self {
            jobs: Arc::clone(jobs),
            id: id.to_owned(),
            held: true,
        })
    }

    /// Whether `cancel_job` reached this job.
    fn cancelled(&self) -> bool {
        self.jobs
            .lock()
            .get(&self.id)
            .is_some_and(|live| live.cancelled)
    }

    /// A check blocking work polls: cancelled, or shutting down.
    fn stop_check(&self, worker: &Worker) -> impl Fn() -> bool + Send + Sync + 'static {
        let shutdown = worker.shutdown_check();
        let jobs = Arc::clone(&self.jobs);
        let id = self.id.clone();
        move || shutdown() || jobs.lock().get(&id).is_some_and(|live| live.cancelled)
    }

    /// End the worker's turn: take an ingest handed over meanwhile, or
    /// let go of the job, in one step under the registry's lock.
    fn next_or_release(&mut self) -> Option<Task> {
        let mut live = self.jobs.lock();
        if let Some(holder) = live.get_mut(&self.id)
            && holder.ingest_requested
        {
            holder.ingest_requested = false;
            return Some(Task::Ingest);
        }
        live.remove(&self.id);
        self.held = false;
        None
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        if self.held {
            self.jobs.lock().remove(&self.id);
        }
    }
}

/// What `start_import` answers: its receipt and the job it names.
#[derive(Debug, Clone)]
pub struct ImportStarted {
    receipt: Receipt,
    job_id: String,
}

impl ImportStarted {
    pub fn receipt(&self) -> &Receipt {
        &self.receipt
    }

    pub fn job_id(&self) -> &str {
        &self.job_id
    }
}

/// One import job as `import_job` reports it (spec/07, 09).
#[derive(Debug, Clone)]
pub struct ImportJobStatus {
    state: ImportJobState,
    sources: Vec<ImportSourceStatus>,
    checkpoint: ImportJobCheckpoint,
    report_path: Option<PathBuf>,
}

impl ImportJobStatus {
    pub fn state(&self) -> ImportJobState {
        self.state
    }

    /// The recorded inventory, in ordinal order; empty until it finishes.
    pub fn sources(&self) -> &[ImportSourceStatus] {
        &self.sources
    }

    /// Problems, variant choices, totals and any failure.
    pub fn checkpoint(&self) -> &ImportJobCheckpoint {
        &self.checkpoint
    }

    /// Where the report lives once the job has finished (spec/09).
    pub fn report_path(&self) -> Option<&Path> {
        self.report_path.as_deref()
    }
}

/// One recorded source and how far its ingest has got.
#[derive(Debug, Clone)]
pub struct ImportSourceStatus {
    source: SourceInventory,
    position: SourcePosition,
}

impl ImportSourceStatus {
    pub fn source(&self) -> &SourceInventory {
        &self.source
    }

    pub fn position(&self) -> SourcePosition {
        self.position
    }
}

impl Engine {
    /// `start_import(paths, target)` (spec/07, 09): record the job under
    /// the idempotency key, then inventory it in the background. The
    /// receipt names the job; a repeated key returns that receipt and
    /// starts nothing.
    pub async fn start_import(
        &self,
        idempotency_key: &str,
        paths: Vec<PathBuf>,
    ) -> Result<ImportStarted, EngineError> {
        if paths.is_empty() {
            return Err(EngineError::invalid_input(
                "paths",
                "choose at least one file or folder to import",
            ));
        }
        let id = self.resources().new_uuid_string();
        let now = self.resources().now();
        let fresh = format!("{{\"job_id\":\"{id}\"}}");
        let key = idempotency_key.to_owned();
        let jobs = Arc::clone(self.imports());
        self.command(move |worker| async move {
            let (job, requested, result) = (id.clone(), paths.clone(), fresh.clone());
            let receipt = worker
                .resources()
                .write(WritePriority::User, move |tx| {
                    run_once(tx, &key, START_IMPORT, now, |tx| {
                        create_import_job(tx, &job, &requested, now)?;
                        Ok(result)
                    })
                })
                .await?;
            if receipt.result_json() == fresh {
                // Refused during shutdown: the job stays in inventory,
                // and the next open fails it visibly.
                spawn_job(&worker, &jobs, id, Task::Inventory(paths))?;
            }
            // A repeated key's receipt names the first job; this crate
            // wrote that JSON, so it reads back exactly.
            let job_id = receipt
                .result_json()
                .strip_prefix("{\"job_id\":\"")
                .and_then(|rest| rest.strip_suffix("\"}"))
                .ok_or_else(|| EngineError::internal("a start_import receipt names no job"))?
                .to_owned();
            Ok(ImportStarted { receipt, job_id })
        })
        .await
    }

    /// `confirm_import(job_id, variants)` (spec/07, 09): choose a variant
    /// for exactly the ambiguous sources, then start ingest.
    pub async fn confirm_import(
        &self,
        idempotency_key: &str,
        job_id: &str,
        variants: Vec<(u64, MboxVariant)>,
    ) -> Result<Receipt, EngineError> {
        // Validate a waiting job's inventory first, so a bad choice names
        // its field. The write accepts only that revision; anything else
        // is decided there, where a repeated key still finds its receipt.
        let id = job_id.to_owned();
        let job = self
            .read_through(move |resources| resources.read(|tx| import_job(tx, &id)))
            .await?;
        let expected = match job.state() {
            ImportJobState::AwaitingConfirmation => Some((
                job.revision(),
                job.checkpoint()
                    .with_variants(&variants)
                    .map_err(|message| EngineError::invalid_input("variants", message))?,
            )),
            _ => None,
        };
        let now = self.resources().now();
        let (key, id) = (idempotency_key.to_owned(), job_id.to_owned());
        let jobs = Arc::clone(self.imports());
        self.command(move |worker| async move {
            let job = id.clone();
            let receipt = worker
                .resources()
                .write(WritePriority::User, move |tx| {
                    confirm_in(tx, &key, &job, expected, now)
                })
                .await?;
            spawn_job(&worker, &jobs, id, Task::Ingest)?;
            Ok(receipt)
        })
        .await
    }

    /// `cancel_job(id)` (spec/07, 09): the job is `cancelled` once this
    /// returns. A live worker stops at its next message; committed
    /// batches stay imported. A finished job is `Conflict`.
    pub async fn cancel_job(
        &self,
        idempotency_key: &str,
        job_id: &str,
    ) -> Result<Receipt, EngineError> {
        let now = self.resources().now();
        let (key, id) = (idempotency_key.to_owned(), job_id.to_owned());
        let receipt = self
            .resources()
            .write(WritePriority::User, move |tx| {
                run_once(tx, &key, CANCEL_JOB, now, |tx| {
                    let job = import_job(tx, &id)?;
                    if job.is_finished() {
                        return Err(StoreError::Conflict {
                            detail: format!("import job {id} has already finished"),
                        });
                    }
                    let state = ImportJobState::Cancelled;
                    advance_import_job(tx, &id, job.revision(), state, job.checkpoint(), now)?;
                    Ok(format!("{{\"job_id\":\"{id}\",\"state\":\"cancelled\"}}"))
                })
            })
            .await?;
        self.imports().cancel(job_id);
        // The cancel is durable either way; a failed cleanup is redone
        // at the next open, which finds no report.
        let id = job_id.to_owned();
        let _ = self
            .read_through(move |resources| finish(resources, &id))
            .await;
        Ok(receipt)
    }

    /// `import_job(job_id)` (spec/07): state, inventory, problems and
    /// counts, from one read.
    pub async fn import_job(&self, job_id: &str) -> Result<ImportJobStatus, EngineError> {
        let id = job_id.to_owned();
        self.read_through(move |resources| {
            resources.read(|tx| {
                let job = import_job(tx, &id)?;
                let sources = import_job_sources(tx, &job)?
                    .into_iter()
                    .map(|(source, position)| ImportSourceStatus { source, position })
                    .collect();
                let report_path = if job.is_finished() {
                    Some(resources.import_report_path(&id)?)
                } else {
                    None
                };
                Ok(ImportJobStatus {
                    state: job.state(),
                    sources,
                    checkpoint: job.checkpoint().clone(),
                    report_path,
                })
            })
        })
        .await
    }

    /// Recovery at open (spec/09): an interrupted inventory fails, a
    /// waiting job keeps waiting, a running job resumes, and a finished
    /// job without a report gets its cleanup again. A job whose row no
    /// longer decodes stays as stored, as evidence: its query reports
    /// `Corrupt`, and the rest of the store opens. Runs on the open
    /// thread, which may block. Cleanup failures wait for the next open.
    pub(crate) fn recover_imports(&self) -> Result<(), EngineError> {
        for job in self
            .resources()
            .read(finished_import_jobs)?
            .into_iter()
            .flatten()
        {
            // Best-effort, like the cleanup it gates: an unreadable
            // `recovery/` must not keep the store closed.
            if !matches!(self.resources().import_report_exists(job.id()), Ok(true)) {
                let _ = finish(self.resources(), job.id());
            }
        }
        let jobs = self.resources().read(recoverable_import_jobs)?;
        for job in jobs.into_iter().flatten() {
            match job.state() {
                ImportJobState::Inventory => {
                    let now = self.resources().now();
                    let checkpoint = job.checkpoint().with_failure(INTERRUPTED_INVENTORY);
                    let (id, revision) = (job.id().to_owned(), job.revision());
                    self.block_on(self.resources().write(PRIORITY, move |tx| {
                        let state = ImportJobState::Failed;
                        advance_import_job(tx, &id, revision, state, &checkpoint, now).map(drop)
                    }))?;
                    let _ = finish(self.resources(), job.id());
                }
                ImportJobState::Running => {
                    spawn_job(self, self.imports(), job.id().to_owned(), Task::Ingest)?;
                }
                _ => (),
            }
        }
        Ok(())
    }

    /// Run an accepted command and the work it schedules as one worker,
    /// so a caller that stops waiting cannot strand an accepted job.
    async fn command<T, F, Fut>(&self, body: F) -> Result<T, EngineError>
    where
        T: Send + 'static,
        F: FnOnce(Worker) -> Fut,
        Fut: Future<Output = Result<T, EngineError>> + Send + 'static,
    {
        let (answer, answered) = tokio::sync::oneshot::channel();
        self.spawn_worker(move |worker| {
            let run = body(worker);
            async move {
                let _ = answer.send(run.await);
            }
        })?;
        answered
            .await
            .map_err(|_| EngineError::internal("an import command ended without an answer"))?
    }
}

/// The confirmation's write: only the waiting revision the caller
/// validated becomes `running`. A job that changed since that read,
/// or was not waiting then (`None`), is `Conflict`; a repeated key still
/// returns its receipt first.
fn confirm_in(
    tx: &sb_resources::Transaction,
    key: &str,
    job: &str,
    expected: Option<(i64, ImportJobCheckpoint)>,
    now: sb_resources::Timestamp,
) -> Result<Receipt, StoreError> {
    run_once(tx, key, CONFIRM_IMPORT, now, |tx| {
        let current = import_job(tx, job)?;
        let Some((revision, checkpoint)) = expected.filter(|(revision, _)| {
            current.state() == ImportJobState::AwaitingConfirmation
                && current.revision() == *revision
        }) else {
            return Err(StoreError::Conflict {
                detail: format!("import job {job} is not awaiting the confirmation given"),
            });
        };
        advance_import_job(tx, job, revision, ImportJobState::Running, &checkpoint, now)?;
        Ok(format!("{{\"job_id\":\"{job}\",\"state\":\"running\"}}"))
    })
}

/// Start a worker for `id`, or hand an ingest to the one that holds it.
fn spawn_job(
    spawner: &impl Spawn,
    jobs: &Arc<ImportJobs>,
    id: String,
    task: Task,
) -> Result<(), EngineError> {
    let Some(claim) = Claim::new(jobs, &id, &task) else {
        return Ok(());
    };
    spawner.spawn_worker(move |worker| run_job(worker, claim, task))
}

/// One worker's turns on its job: the first task, then any ingest
/// handed over before it let go.
async fn run_job(worker: Worker, mut claim: Claim, task: Task) {
    let mut task = Some(task);
    while let Some(current) = task {
        match current {
            Task::Inventory(paths) => inventory(&worker, &claim, paths).await,
            Task::Ingest => ingest(&worker, &claim).await,
        }
        finished(&worker, &claim.id).await;
        task = claim.next_or_release();
    }
}

/// Inventory the chosen paths, record the sources, then wait for
/// confirmation. A cancel already recorded `cancelled`; a shutdown
/// leaves `inventory` for the next open to fail.
async fn inventory(worker: &Worker, claim: &Claim, paths: Vec<PathBuf>) {
    let stop = claim.stop_check(worker);
    let outcome = worker
        .blocking(move |resources| {
            resources.inventory_import_sources(&paths, |_| {
                if stop() {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            })
        })
        .await;
    let inventory = match outcome {
        Ok(ImportInventoryOutcome::Complete(inventory)) => inventory,
        Ok(ImportInventoryOutcome::Cancelled(inventory)) => {
            return discard_if_cancelled(worker, claim, inventory).await;
        }
        Err(error) => return fail(worker, &claim.id, error.to_string()).await,
    };
    match record(worker, &claim.id, &inventory).await {
        Ok(()) => (),
        // A cancel that won the race already ended the job.
        Err(StoreError::Conflict { .. }) if moved_on(worker, &claim.id, None).await => {
            discard_if_cancelled(worker, claim, inventory).await;
        }
        Err(error) => fail(worker, &claim.id, error.to_string()).await,
    }
}

/// A cancelled inventory's unrecorded captures are known only to this
/// worker, so it removes them (spec/03). The durable state decides: the
/// in-memory flag lands after the cancel's write. A shutdown keeps them:
/// the next open fails the job, and they stay as evidence.
async fn discard_if_cancelled(worker: &Worker, claim: &Claim, inventory: ImportInventory) {
    let job = claim.id.clone();
    let state = worker
        .blocking(move |resources| resources.read(|tx| import_job(tx, &job)))
        .await;
    if !matches!(state, Ok(Ok(job)) if job.state() == ImportJobState::Cancelled) {
        return;
    }
    let snapshots: Vec<_> = inventory
        .sources()
        .iter()
        .map(|source| source.snapshot().clone())
        .collect();
    let _ = worker
        .blocking(move |resources| resources.remove_import_sources(&snapshots))
        .await;
}

/// Run terminal-job cleanup when a worker's turn ends; a paused job is
/// left alone, and a failed cleanup is redone at the next open.
async fn finished(worker: &Worker, id: &str) {
    let id = id.to_owned();
    let _ = worker
        .blocking(move |resources| finish(resources, &id))
        .await;
}

/// Terminal-job cleanup (spec/03, 09): a completed or cancelled job's
/// snapshots go, then its report is written, marking the cleanup done.
/// An unfinished job is left alone. Blocking.
fn finish(resources: &Resources, id: &str) -> Result<(), StoreError> {
    let (job, sources) = resources.read(|tx| {
        let job = import_job(tx, id)?;
        let sources = recorded_import_job_sources(tx, id)?;
        Ok((job, sources))
    })?;
    if !job.is_finished() {
        return Ok(());
    }
    if matches!(
        job.state(),
        ImportJobState::Completed | ImportJobState::Cancelled
    ) {
        let snapshots: Vec<_> = sources
            .iter()
            .map(|(source, _)| source.snapshot().clone())
            .collect();
        resources.remove_import_sources(&snapshots)?;
    }
    resources.write_import_report(id, &import_report(&job, &sources))
}

/// Whether the job left this worker's hands: no longer at `revision`
/// (`None`: no longer in inventory), as a cancel leaves it. Any other
/// `Conflict` is a real one and fails the job.
async fn moved_on(worker: &Worker, id: &str, revision: Option<i64>) -> bool {
    let job = id.to_owned();
    let current = worker
        .blocking(move |resources| resources.read(|tx| import_job(tx, &job)))
        .await;
    let Ok(Ok(current)) = current else {
        return false;
    };
    match revision {
        None => current.state() != ImportJobState::Inventory,
        Some(revision) => {
            current.state() != ImportJobState::Running || current.revision() != revision
        }
    }
}

/// Record the sources in budget-sized transactions while the job is in
/// inventory, then move it to `awaiting_confirmation` (spec/09).
async fn record(worker: &Worker, id: &str, inventory: &ImportInventory) -> Result<(), StoreError> {
    let sources = inventory.sources();
    let mut first = 0;
    while first < sources.len() {
        let mut end = first;
        let mut bytes = 0;
        while end < sources.len() {
            let cost = sources[end].to_json().len() + SOURCE_ROW_BYTES;
            if end > first && bytes + cost > MAX_IMPORT_CHANGES_BYTES {
                break;
            }
            bytes += cost;
            end += 1;
        }
        let chunk = sources[first..end].to_vec();
        let (job, ordinal) = (id.to_owned(), first as u64);
        worker
            .resources()
            .write(PRIORITY, move |tx| {
                record_import_sources(tx, &job, ordinal, &chunk)
            })
            .await?;
        first = end;
    }
    let checkpoint = ImportJobCheckpoint::inventoried(sources, inventory.problems());
    let now = worker.resources().now();
    let job = id.to_owned();
    worker
        .resources()
        .write(PRIORITY, move |tx| {
            let current = import_job(tx, &job)?;
            if current.state() != ImportJobState::Inventory {
                return Err(StoreError::Conflict {
                    detail: format!("import job {job} left inventory"),
                });
            }
            let state = ImportJobState::AwaitingConfirmation;
            advance_import_job(tx, &job, current.revision(), state, &checkpoint, now).map(drop)
        })
        .await
}

/// Ingest a running job from its checkpoints; a failure is recorded on
/// the job, checkpoints intact.
async fn ingest(worker: &Worker, claim: &Claim) {
    if let Err(detail) = run_ingest(worker, claim).await {
        fail(worker, &claim.id, detail).await;
    }
}

/// What one blocking batch step gathered.
struct Gathered {
    messages: Vec<ImportMessage>,
    /// The position after the last gathered message.
    position: SourcePosition,
    flags_incomplete: u64,
    /// Acquired, but over the budget: the first message of the next batch.
    carry: Option<AcquiredMessage>,
    error: Option<StoreError>,
    stopped: bool,
}

/// Every source from the checkpoint's next, in order; then `completed`.
/// `Ok` also covers a pause at shutdown and a cancelled job.
async fn run_ingest(worker: &Worker, claim: &Claim) -> Result<(), String> {
    let id = claim.id.clone();
    let read = id.clone();
    let job = worker
        .blocking(move |resources| resources.read(|tx| import_job(tx, &read)))
        .await
        .map_err(|e| e.to_string())?
        .map_err(|e| e.to_string())?;
    if job.state() != ImportJobState::Running {
        return Ok(());
    }
    let mut revision = job.revision();
    let mut checkpoint = job.checkpoint().clone();
    while checkpoint.next() < checkpoint.sources() {
        let ordinal = checkpoint.next();
        let variant = checkpoint.variant(ordinal);
        let job = id.clone();
        let (source, mut position, mut acquisition) = worker
            .blocking(move |resources| open(resources, &job, ordinal, variant))
            .await
            .map_err(|e| e.to_string())?
            .map_err(|e| e.to_string())?;
        let mut carry = None;
        loop {
            let used = source.mailbox().len() * 2 + checkpoint.to_json().len() + CHECKPOINT_SLACK;
            let budget = MAX_IMPORT_CHANGES_BYTES.saturating_sub(used);
            let stop = claim.stop_check(worker);
            let (back, gathered) = worker
                .blocking(move |resources| {
                    let gathered =
                        gather(resources, &mut acquisition, position, carry, budget, &stop);
                    (acquisition, gathered)
                })
                .await
                .map_err(|e| e.to_string())?;
            acquisition = back;
            carry = gathered.carry;
            let finished = gathered.position.messages() == source.message_count();
            if !gathered.messages.is_empty() || finished {
                let batch = Batch {
                    job: id.clone(),
                    revision,
                    ordinal,
                    mailbox: source.mailbox().to_owned(),
                    messages: gathered.messages,
                    position: gathered.position,
                    flags_incomplete: gathered.flags_incomplete,
                };
                match commit(worker, batch).await {
                    Ok(committed) => {
                        checkpoint = committed.checkpoint().clone();
                        revision = committed.revision();
                        position = gathered.position;
                    }
                    // Cancelled since the last commit: the batch goes.
                    Err(StoreError::Conflict { .. })
                        if moved_on(worker, &id, Some(revision)).await =>
                    {
                        return Ok(());
                    }
                    Err(error) => return Err(error.to_string()),
                }
            }
            if let Some(error) = gathered.error {
                return Err(error.to_string());
            }
            if gathered.stopped || claim.cancelled() {
                return Ok(());
            }
            if finished {
                break;
            }
        }
    }
    let now = worker.resources().now();
    let job = id.clone();
    match worker
        .resources()
        .write(PRIORITY, move |tx| {
            let state = ImportJobState::Completed;
            advance_import_job(tx, &job, revision, state, &checkpoint, now).map(drop)
        })
        .await
    {
        Ok(()) => Ok(()),
        Err(StoreError::Conflict { .. }) if moved_on(worker, &id, Some(revision)).await => Ok(()),
        Err(error) => Err(error.to_string()),
    }
}

/// Load a source row and open its verified snapshot at its position.
fn open(
    resources: &Resources,
    job: &str,
    ordinal: u64,
    variant: Option<MboxVariant>,
) -> Result<(SourceInventory, SourcePosition, SourceAcquisition), StoreError> {
    let (source, position) = resources.read(|tx| import_job_source(tx, job, ordinal))?;
    let acquisition = resources.open_import_acquisition(&source, variant, position)?;
    Ok((source, position, acquisition))
}

/// Acquire up to one batch: 250 messages within `budget` bytes of
/// metadata (spec/09, 03). A message that doesn't fit is carried over;
/// an error or stop ends the batch with what was gathered.
fn gather(
    resources: &Resources,
    acquisition: &mut SourceAcquisition,
    start: SourcePosition,
    mut carry: Option<AcquiredMessage>,
    budget: usize,
    stop: &dyn Fn() -> bool,
) -> Gathered {
    let mut gathered = Gathered {
        messages: Vec::new(),
        position: start,
        flags_incomplete: 0,
        carry: None,
        error: None,
        stopped: false,
    };
    let mut used = 0usize;
    while gathered.messages.len() < MAX_IMPORT_BATCH {
        if stop() {
            gathered.stopped = true;
            break;
        }
        let message = match carry.take() {
            Some(message) => message,
            None => match resources.acquire_import_message(acquisition) {
                Ok(Some(message)) => message,
                Ok(None) => break,
                Err(error) => {
                    gathered.error = Some(error);
                    break;
                }
            },
        };
        let cost = message.message().change_budget_bytes();
        if !gathered.messages.is_empty() && used.saturating_add(cost) > budget {
            gathered.carry = Some(message);
            break;
        }
        used = used.saturating_add(cost);
        gathered.position = message.next_position();
        gathered.flags_incomplete +=
            u64::from(message.flag_extraction() == FlagExtraction::Incomplete);
        gathered.messages.push(message.into_message());
    }
    gathered
}

/// An owned batch, for the writer's `'static` closure.
struct Batch {
    job: String,
    revision: i64,
    ordinal: u64,
    mailbox: String,
    messages: Vec<ImportMessage>,
    position: SourcePosition,
    flags_incomplete: u64,
}

async fn commit(worker: &Worker, batch: Batch) -> Result<ImportJobRecord, StoreError> {
    let now = worker.resources().now();
    worker
        .resources()
        .write(PRIORITY, move |tx| {
            let rows = ImportJobBatch {
                job_id: &batch.job,
                revision: batch.revision,
                ordinal: batch.ordinal,
                mailbox: &batch.mailbox,
                messages: &batch.messages,
                position: batch.position,
                flags_incomplete: batch.flags_incomplete,
            };
            commit_import_job_batch(tx, &rows, now)
        })
        .await
}

/// Record a failure on an unfinished job. If even that write fails, the
/// job stays as it was: a running one resumes at the next open.
async fn fail(worker: &Worker, id: &str, detail: String) {
    let now = worker.resources().now();
    let job = id.to_owned();
    let _ = worker
        .resources()
        .write(PRIORITY, move |tx| {
            let current = import_job(tx, &job)?;
            if current.is_finished() {
                return Ok(());
            }
            let checkpoint = current.checkpoint().with_failure(detail);
            let state = ImportJobState::Failed;
            advance_import_job(tx, &job, current.revision(), state, &checkpoint, now).map(drop)
        })
        .await;
}

#[cfg(test)]
mod tests;
