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

use std::collections::HashSet;
use std::ops::ControlFlow;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use sb_resources::{
    AcquiredMessage, FlagExtraction, ImportInventory, ImportInventoryOutcome, ImportJobBatch,
    ImportJobCheckpoint, ImportJobState, ImportMessage, MAX_IMPORT_BATCH, MAX_IMPORT_CHANGES_BYTES,
    MboxVariant, Receipt, Resources, SourceAcquisition, SourceInventory, SourcePosition,
    SourceProgress, StoreError, WritePriority, advance_import_job, commit_import_job_batch,
    create_import_job, import_job, import_job_source, record_import_sources,
    recoverable_import_jobs, run_once,
};

use crate::engine::Worker;
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

/// Jobs with a live worker in this process, each with its cancel flag.
/// One worker per job: a second start for the same job is a no-op.
#[derive(Default)]
pub(crate) struct ImportJobs {
    live: Mutex<HashSet<String>>,
    cancelled: Mutex<HashSet<String>>,
}

impl ImportJobs {
    fn lock<T>(set: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
        set.lock()
            .expect("a panicking thread held the import job registry")
    }

    /// Tell a live worker its job was cancelled, so it stops early.
    fn cancel(&self, id: &str) {
        if Self::lock(&self.live).contains(id) {
            Self::lock(&self.cancelled).insert(id.to_owned());
        }
    }
}

/// A worker's hold on its job; dropping it, at the worker's end, frees
/// the job for another worker.
struct Claim {
    jobs: Arc<ImportJobs>,
    id: String,
}

impl Claim {
    fn new(jobs: &Arc<ImportJobs>, id: &str) -> Option<Self> {
        if !ImportJobs::lock(&jobs.live).insert(id.to_owned()) {
            return None;
        }
        Some(Self {
            jobs: Arc::clone(jobs),
            id: id.to_owned(),
        })
    }

    /// Whether `cancel_job` reached this job.
    fn cancelled(&self) -> bool {
        ImportJobs::lock(&self.jobs.cancelled).contains(&self.id)
    }

    /// A check blocking work polls: cancelled, or shutting down.
    fn stop_check(&self, worker: &Worker) -> impl Fn() -> bool + Send + Sync + 'static {
        let shutdown = worker.shutdown_check();
        let jobs = Arc::clone(&self.jobs);
        let id = self.id.clone();
        move || shutdown() || ImportJobs::lock(&jobs.cancelled).contains(&id)
    }
}

impl Drop for Claim {
    fn drop(&mut self) {
        ImportJobs::lock(&self.jobs.cancelled).remove(&self.id);
        ImportJobs::lock(&self.jobs.live).remove(&self.id);
    }
}

/// One import job as `import_job` reports it (spec/07, 09).
#[derive(Debug, Clone)]
pub struct ImportJobStatus {
    state: ImportJobState,
    sources: Vec<ImportSourceStatus>,
    checkpoint: ImportJobCheckpoint,
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
    ) -> Result<Receipt, EngineError> {
        if paths.is_empty() {
            return Err(EngineError::invalid_input(
                "paths",
                "choose at least one file or folder to import",
            ));
        }
        let id = self.resources().new_uuid_string();
        let now = self.resources().now();
        let fresh = format!("{{\"job_id\":\"{id}\"}}");
        let (key, job, requested, result) = (
            idempotency_key.to_owned(),
            id.clone(),
            paths.clone(),
            fresh.clone(),
        );
        let receipt = self
            .resources()
            .write(WritePriority::User, move |tx| {
                run_once(tx, &key, START_IMPORT, now, |tx| {
                    create_import_job(tx, &job, &requested, now)?;
                    Ok(result)
                })
            })
            .await?;
        if receipt.result_json() == fresh {
            // Refused during shutdown: the job stays in inventory, and
            // the next open fails it visibly.
            self.spawn_inventory(id, paths)?;
        }
        Ok(receipt)
    }

    /// `confirm_import(job_id, variants)` (spec/07, 09): choose a variant
    /// for exactly the ambiguous sources, then start ingest.
    pub async fn confirm_import(
        &self,
        idempotency_key: &str,
        job_id: &str,
        variants: Vec<(u64, MboxVariant)>,
    ) -> Result<Receipt, EngineError> {
        // Validate against the stored inventory first, so a bad choice
        // names its field; the write re-checks the state.
        let id = job_id.to_owned();
        let job = self
            .read_through(move |resources| resources.read(|tx| import_job(tx, &id)))
            .await?;
        let checkpoint = job
            .checkpoint()
            .with_variants(&variants)
            .map_err(|message| EngineError::invalid_input("variants", message))?;
        let now = self.resources().now();
        let (key, id) = (idempotency_key.to_owned(), job_id.to_owned());
        let receipt = self
            .resources()
            .write(WritePriority::User, move |tx| {
                run_once(tx, &key, CONFIRM_IMPORT, now, |tx| {
                    let job = import_job(tx, &id)?;
                    if job.state() != ImportJobState::AwaitingConfirmation {
                        return Err(StoreError::Conflict {
                            detail: format!("import job {id} is not awaiting confirmation"),
                        });
                    }
                    let state = ImportJobState::Running;
                    advance_import_job(tx, &id, job.revision(), state, &checkpoint, now)?;
                    Ok(format!("{{\"job_id\":\"{id}\",\"state\":\"running\"}}"))
                })
            })
            .await?;
        self.spawn_ingest(job_id.to_owned())?;
        Ok(receipt)
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
        Ok(receipt)
    }

    /// `import_job(job_id)` (spec/07): state, inventory, problems and
    /// counts, from one read.
    pub async fn import_job(&self, job_id: &str) -> Result<ImportJobStatus, EngineError> {
        let id = job_id.to_owned();
        self.read_through(move |resources| {
            resources.read(|tx| {
                let job = import_job(tx, &id)?;
                let sources = (0..job.checkpoint().sources())
                    .map(|ordinal| {
                        let (source, position) = import_job_source(tx, &id, ordinal)?;
                        Ok(ImportSourceStatus { source, position })
                    })
                    .collect::<Result<_, StoreError>>()?;
                Ok(ImportJobStatus {
                    state: job.state(),
                    sources,
                    checkpoint: job.checkpoint().clone(),
                })
            })
        })
        .await
    }

    /// Recovery at open (spec/09): an interrupted inventory fails, a
    /// waiting job keeps waiting, a running job resumes. Runs on the
    /// open thread, which may block.
    pub(crate) fn recover_imports(&self) -> Result<(), EngineError> {
        let jobs = self.resources().read(recoverable_import_jobs)?;
        for job in jobs {
            match job.state() {
                ImportJobState::Inventory => {
                    let now = self.resources().now();
                    let checkpoint = job.checkpoint().with_failure(INTERRUPTED_INVENTORY);
                    let id = job.id().to_owned();
                    let revision = job.revision();
                    self.block_on(self.resources().write(PRIORITY, move |tx| {
                        let state = ImportJobState::Failed;
                        advance_import_job(tx, &id, revision, state, &checkpoint, now).map(drop)
                    }))?;
                }
                ImportJobState::Running => self.spawn_ingest(job.id().to_owned())?,
                _ => (),
            }
        }
        Ok(())
    }

    fn spawn_inventory(&self, id: String, paths: Vec<PathBuf>) -> Result<(), EngineError> {
        let Some(claim) = Claim::new(self.imports(), &id) else {
            return Ok(());
        };
        self.spawn_worker(move |worker| inventory(worker, claim, paths))
    }

    fn spawn_ingest(&self, id: String) -> Result<(), EngineError> {
        let Some(claim) = Claim::new(self.imports(), &id) else {
            return Ok(());
        };
        self.spawn_worker(move |worker| ingest(worker, claim))
    }
}

/// Inventory the chosen paths, record the sources, then wait for
/// confirmation. A cancel already recorded `cancelled`; a shutdown
/// leaves `inventory` for the next open to fail.
async fn inventory(worker: Worker, claim: Claim, paths: Vec<PathBuf>) {
    let stop = claim.stop_check(&worker);
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
        Ok(ImportInventoryOutcome::Cancelled(_)) => return,
        Err(error) => return fail(&worker, &claim.id, error.to_string()).await,
    };
    match record(&worker, &claim.id, &inventory).await {
        // A cancel that won the race already ended the job.
        Ok(()) | Err(StoreError::Conflict { .. }) => (),
        Err(error) => fail(&worker, &claim.id, error.to_string()).await,
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
async fn ingest(worker: Worker, claim: Claim) {
    if let Err(detail) = run_ingest(&worker, &claim).await {
        fail(&worker, &claim.id, detail).await;
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
                    progress: if finished {
                        SourceProgress::Finished
                    } else {
                        SourceProgress::Partial
                    },
                    flags_incomplete: gathered.flags_incomplete,
                };
                match commit(worker, batch, &checkpoint).await {
                    Ok(committed) => {
                        checkpoint = committed;
                        revision += 1;
                        position = gathered.position;
                    }
                    // Cancelled since the last commit: the batch goes.
                    Err(StoreError::Conflict { .. }) => return Ok(()),
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
        Ok(()) | Err(StoreError::Conflict { .. }) => Ok(()),
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
    progress: SourceProgress,
    flags_incomplete: u64,
}

async fn commit(
    worker: &Worker,
    batch: Batch,
    checkpoint: &ImportJobCheckpoint,
) -> Result<ImportJobCheckpoint, StoreError> {
    let now = worker.resources().now();
    let checkpoint = checkpoint.clone();
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
                progress: batch.progress,
                flags_incomplete: batch.flags_incomplete,
            };
            commit_import_job_batch(tx, &rows, &checkpoint, now)
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
