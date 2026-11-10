//! The maintenance scheduler (spec/03 "Scheduled maintenance", plan
//! M1.8h3b): one engine worker that takes a backup generation every
//! hour while there were changes, runs a GC pass daily and verifies
//! the objects weekly (M1.8h4c), against the injected clock, and
//! records each run in `maintenance_jobs`.
//!
//! The cadence lives in the records, not in memory: a job is due a
//! period after its last complete run started, sooner after one that
//! was blocked or failed, and at once when it never ran, was
//! interrupted or was cancelled. A restart, or a restored store, picks
//! it up from there. Jobs run one at a time, the backup first.

#[cfg(test)]
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Condvar, Mutex, MutexGuard};
use std::time::Duration;

use sb_resources::{
    BackupDestination, BackupReceipt, GC_GRACE_PERIOD, GcGate, GcReport, GenerationCheck,
    GenerationVerdict, MaintenanceJob, MaintenanceOutcome, MaintenanceRun, Resources, SampleReport,
    StoreError, Timestamp,
};

use crate::Engine;
use crate::engine::Spawn;

/// A generation every hour while there were changes (spec/03 "Backup
/// generations"), so the recovery point is at most an hour.
pub(crate) const BACKUP_PERIOD: Duration = Duration::from_secs(60 * 60);
/// A backup that was blocked (another backup or a restore held the
/// operation) or failed is tried again this soon, so one bad attempt
/// stretches the recovery point by minutes, not by an hour.
pub(crate) const BACKUP_RETRY: Duration = Duration::from_secs(5 * 60);
/// A GC pass a day (spec/03 "Scheduled maintenance").
pub(crate) const GC_PERIOD: Duration = Duration::from_secs(24 * 60 * 60);
/// A GC pass that was blocked by a gate or failed is tried again this
/// soon: a backup in progress ends in minutes, and a breaker or a
/// damaged object waits for the user, so an hour writes few records.
pub(crate) const GC_RETRY: Duration = Duration::from_secs(60 * 60);
/// The objects are verified weekly (spec/03 "Reads and verification").
pub(crate) const VERIFY_PERIOD: Duration = Duration::from_secs(7 * 24 * 60 * 60);
/// A verification that was blocked (a backup or a restore held the
/// operation a generation's check needs) or failed is tried again this
/// soon. The retry reads only what the week still owes.
pub(crate) const VERIFY_RETRY: Duration = Duration::from_secs(60 * 60);
/// The scheduler's first look waits this long after it starts: the
/// open's own work goes first, and a short-lived process (a CLI
/// command) never starts a job its shutdown would cancel at once.
pub(crate) const SETTLE: Duration = Duration::from_secs(60);
/// How soon a job is looked at again after a look that ran nothing (no
/// changes to back up, a record that couldn't be written), and the
/// longest park: the injected clock is read again at least this often,
/// so a wall-clock jump or a Mac waking from sleep is seen within it.
pub(crate) const POLL: Duration = Duration::from_secs(60);

/// Whether an engine starts the scheduler at open. sb-engine's own unit
/// tests open engines without it, so a backup and a GC pass never race
/// their assertions or their fault-injecting filesystems; the
/// scheduler's tests start it themselves. Every other build, other
/// crates' tests included, schedules it.
pub(crate) const SCHEDULED_AT_OPEN: bool = !cfg!(test);

/// One run of a scheduled job.
type RunJob = fn(&Resources, &MaintenanceJobs, &dyn Fn() -> bool);

/// When a job runs again after its last run ended.
#[derive(Clone, Copy, Debug)]
pub(crate) struct Cadence {
    /// After a complete run.
    pub(crate) period: Duration,
    /// After a blocked or failed one.
    pub(crate) retry: Duration,
}

/// The scheduled jobs in the order a turn considers them.
const SCHEDULE: [(MaintenanceJob, Cadence, RunJob); 3] = [
    (
        MaintenanceJob::Backup,
        Cadence {
            period: BACKUP_PERIOD,
            retry: BACKUP_RETRY,
        },
        back_up,
    ),
    (
        MaintenanceJob::Gc,
        Cadence {
            period: GC_PERIOD,
            retry: GC_RETRY,
        },
        collect_garbage,
    ),
    (
        MaintenanceJob::Verify,
        Cadence {
            period: VERIFY_PERIOD,
            retry: VERIFY_RETRY,
        },
        verify,
    ),
];

/// Starting, stopping and parking the scheduler. The engine holds one;
/// shutdown (and restore's quiesce, M1.8g3) stops it.
#[derive(Default)]
pub(crate) struct MaintenanceJobs {
    state: Mutex<State>,
    ready: Condvar,
    /// Raised by [`MaintenanceJobs::stop`]; the running GC pass or
    /// backup copy stops at its next step.
    cancel: AtomicBool,
    #[cfg(test)]
    pub(crate) hook: Mutex<Option<Hook>>,
}

#[derive(Default)]
struct State {
    running: bool,
    stopping: bool,
    /// The engine started the scheduler (at open, or a test): a resume
    /// after restore's quiesce starts it again.
    armed: bool,
    #[cfg(test)]
    again: bool,
    #[cfg(test)]
    waiting: bool,
}

#[cfg(test)]
type Hook = Arc<dyn Fn(Event) + Send + Sync>;

/// Points a test can observe or hold the worker at.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum Event {
    /// About to park.
    Parked,
    /// A run is recorded as started; its work comes next.
    Running(MaintenanceJob),
    /// A backup's snapshot is taken; its copy comes next.
    Snapshotted,
    /// A run's end is recorded.
    Finished(MaintenanceJob, MaintenanceOutcome),
}

impl MaintenanceJobs {
    fn lock(&self) -> MutexGuard<'_, State> {
        self.state
            .lock()
            .expect("the maintenance scheduler state poisoned")
    }

    /// Stop the scheduler: a park ends at once, and the running job
    /// stops at its next step and records `cancelled`.
    pub(crate) fn stop(&self) {
        self.cancel.store(true, Ordering::SeqCst);
        self.lock().stopping = true;
        self.ready.notify_all();
    }

    fn stopped(&self) -> bool {
        self.lock().stopping
    }

    /// Lift a stop, as restore's resume does (M1.8g3); the engine then
    /// starts the worker again if it had been started.
    pub(crate) fn resume(&self) {
        self.cancel.store(false, Ordering::SeqCst);
        self.lock().stopping = false;
    }

    pub(crate) fn armed(&self) -> bool {
        self.lock().armed
    }

    #[cfg(test)]
    pub(crate) fn running(&self) -> bool {
        self.lock().running
    }

    /// Look again now, as a test does after moving its clock.
    #[cfg(test)]
    pub(crate) fn wake(&self) {
        self.lock().again = true;
        self.ready.notify_all();
    }

    /// Parked, with no wake pending.
    #[cfg(test)]
    pub(crate) fn waiting(&self) -> bool {
        let state = self.lock();
        state.waiting && !state.again
    }

    #[cfg(test)]
    fn event(&self, event: Event) {
        let hook = self.hook.lock().unwrap().clone();
        if let Some(hook) = hook {
            hook(event);
        }
    }

    /// Park until `until` by the injected clock, a stop, or at most
    /// [`POLL`]; the caller looks again either way.
    fn wait(&self, resources: &Resources, until: Timestamp) {
        #[cfg_attr(not(test), allow(unused_mut))]
        let mut state = self.lock();
        #[cfg(test)]
        if std::mem::take(&mut state.again) {
            return;
        }
        if state.stopping {
            return;
        }
        let remaining = until
            .as_millis()
            .saturating_sub(resources.now().as_millis());
        let Ok(remaining) = u64::try_from(remaining) else {
            return;
        };
        if remaining == 0 {
            return;
        }
        #[cfg(test)]
        {
            state.waiting = true;
        }
        let pause = Duration::from_millis(remaining).min(POLL);
        #[cfg_attr(not(test), allow(unused_mut))]
        let (mut state, _) = self
            .ready
            .wait_timeout(state, pause)
            .expect("the maintenance scheduler wait poisoned");
        #[cfg(test)]
        {
            state.waiting = false;
        }
        drop(state);
    }
}

impl Engine {
    /// Start the scheduler's worker unless it runs or was stopped.
    /// Asking arms the schedule either way: a stopped one starts when
    /// a restore resumes on a store that isn't read-only.
    pub(crate) fn schedule_maintenance(&self) {
        let jobs = self.maintenance().clone();
        {
            let mut state = jobs.lock();
            state.armed = true;
            if state.running || state.stopping {
                return;
            }
            state.running = true;
        }
        let owner = jobs.clone();
        if self
            .spawn_worker(move |worker| async move {
                let shutting_down = worker.shutdown_check();
                let registry = owner.clone();
                // `running` clears inside the blocking task, before its
                // count drops: whoever waits for the count can schedule
                // a new worker at once. `stop` is final here; restore's
                // resume (M1.8g3) lifts it before scheduling again.
                let finished = worker
                    .blocking(move |resources| {
                        run(resources, &registry, &shutting_down);
                        registry.lock().running = false;
                    })
                    .await;
                // A task that panicked never cleared it, so no newer
                // worker can have started.
                if finished.is_err() {
                    owner.lock().running = false;
                }
            })
            .is_err()
        {
            jobs.lock().running = false;
        }
    }
}

/// The worker's loop: settle, then one job per turn while any is due,
/// parking until the next one is.
fn run(resources: &Resources, jobs: &MaintenanceJobs, shutting_down: &dyn Fn() -> bool) {
    let stopped = || jobs.stopped() || shutting_down();
    let settled_at = after(resources.now(), SETTLE);
    // A job isn't looked at again before this, whatever its record says.
    let mut not_before = [None; SCHEDULE.len()];
    while !stopped() {
        let now = resources.now();
        let until = if now < settled_at {
            settled_at
        } else {
            match turn(resources, jobs, &stopped, &mut not_before, now) {
                Turn::Ran => continue,
                Turn::Idle(until) => until,
            }
        };
        #[cfg(test)]
        jobs.event(Event::Parked);
        jobs.wait(resources, until);
    }
}

enum Turn {
    /// A job was looked at; look again at once.
    Ran,
    /// Nothing is due before this.
    Idle(Timestamp),
}

fn turn(
    resources: &Resources,
    jobs: &MaintenanceJobs,
    stopped: &dyn Fn() -> bool,
    not_before: &mut [Option<Timestamp>; SCHEDULE.len()],
    now: Timestamp,
) -> Turn {
    let Ok(runs) = resources.maintenance_runs() else {
        return Turn::Idle(after(now, POLL));
    };
    let mut next: Option<Timestamp> = None;
    for (slot, (job, cadence, run_job)) in SCHEDULE.into_iter().enumerate() {
        let latest = runs.iter().find(|run| run.job == job);
        let due = due_at(latest, cadence, now).max(floor_at(not_before[slot], now));
        if due > now {
            next = Some(next.map_or(due, |next| next.min(due)));
            continue;
        }
        not_before[slot] = Some(after(now, POLL));
        run_job(resources, jobs, stopped);
        return Turn::Ran;
    }
    Turn::Idle(next.unwrap_or_else(|| after(now, POLL)))
}

/// When a job is due next, from its latest recorded run: at once when
/// it never ran, its run never ended (interrupted) or was cancelled; a
/// period after a complete run started; a retry after a blocked or
/// failed one ended, so a long failing run can't follow itself at
/// once. A start or end in the future (a clock moved back) counts from
/// now, so the wait is never longer than the period or the retry.
pub(crate) fn due_at(
    latest: Option<&MaintenanceRun>,
    cadence: Cadence,
    now: Timestamp,
) -> Timestamp {
    let Some(run) = latest else {
        return now;
    };
    let Some(end) = &run.finished else {
        return now;
    };
    match end.outcome {
        MaintenanceOutcome::Cancelled => now,
        MaintenanceOutcome::Complete => after(run.started_at.min(now), cadence.period),
        MaintenanceOutcome::Blocked | MaintenanceOutcome::Failed => {
            after(end.at.min(now), cadence.retry)
        }
    }
}

/// A job's in-memory floor as of `now`: never more than [`POLL`]
/// ahead, so a clock moved back doesn't hold a due job for the jump.
pub(crate) fn floor_at(floor: Option<Timestamp>, now: Timestamp) -> Timestamp {
    floor.map_or(now, |floor| floor.min(after(now, POLL)))
}

fn after(at: Timestamp, period: Duration) -> Timestamp {
    let millis = i64::try_from(period.as_millis()).unwrap_or(i64::MAX);
    Timestamp::from_millis(at.as_millis().saturating_add(millis))
}

/// The hourly generation, when there were changes: nothing is recorded
/// when there weren't. A store that can't say is backed up, so the
/// attempt and its failure are recorded.
fn back_up(resources: &Resources, jobs: &MaintenanceJobs, stopped: &dyn Fn() -> bool) {
    if matches!(resources.backup_due(), Ok(false)) || stopped() {
        return;
    }
    if resources.start_maintenance(MaintenanceJob::Backup).is_err() {
        return;
    }
    #[cfg(test)]
    jobs.event(Event::Running(MaintenanceJob::Backup));
    // The snapshot can't stop part way: a stop raised since the start
    // was recorded ends the run before it.
    let (outcome, detail) = if jobs.cancel.load(Ordering::SeqCst) {
        (
            MaintenanceOutcome::Cancelled,
            "stopped before the snapshot".to_owned(),
        )
    } else {
        match resources.backup_snapshot(BackupDestination::DataRoot) {
            Err(error) => snapshot_outcome(error),
            Ok(snapshot) => {
                #[cfg(test)]
                jobs.event(Event::Snapshotted);
                let result = resources.complete_backup_with(snapshot, &jobs.cancel);
                backup_outcome(result, jobs.cancel.load(Ordering::SeqCst))
            }
        }
    };
    finish(resources, jobs, MaintenanceJob::Backup, outcome, &detail);
}

/// The daily GC pass with the default grace period.
fn collect_garbage(resources: &Resources, jobs: &MaintenanceJobs, stopped: &dyn Fn() -> bool) {
    if stopped() || resources.start_maintenance(MaintenanceJob::Gc).is_err() {
        return;
    }
    #[cfg(test)]
    jobs.event(Event::Running(MaintenanceJob::Gc));
    let result = resources.collect_garbage_with(GC_GRACE_PERIOD, &jobs.cancel);
    let (outcome, detail) = gc_outcome(result);
    finish(resources, jobs, MaintenanceJob::Gc, outcome, &detail);
}

/// The weekly verification, when it has work: the live sample of the
/// objects, then every generation not yet checked this week. Nothing
/// is recorded when nothing is owed, so the last run's record, its
/// failures included, stands until there is work again. A store that
/// can't say is verified, so the attempt and its failure are recorded.
fn verify(resources: &Resources, jobs: &MaintenanceJobs, stopped: &dyn Fn() -> bool) {
    if matches!(resources.verification_due(), Ok(false)) || stopped() {
        return;
    }
    if resources.start_maintenance(MaintenanceJob::Verify).is_err() {
        return;
    }
    #[cfg(test)]
    jobs.event(Event::Running(MaintenanceJob::Verify));
    let sample = resources.verify_sample(&jobs.cancel);
    let checks = if generations_follow(&sample) {
        check_generations(resources, &jobs.cancel)
    } else {
        GenerationChecks::default()
    };
    let (outcome, detail) = verify_outcome(sample, &checks);
    finish(resources, jobs, MaintenanceJob::Verify, outcome, &detail);
}

/// What one run's generation checks came to.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub(crate) struct GenerationChecks {
    pub(crate) intact: u64,
    pub(crate) damaged: u64,
    pub(crate) missing: u64,
    /// Generations whose destination wasn't there (an unmounted
    /// volume): nothing recorded, the next run tries again.
    pub(crate) unreachable: u64,
    /// Checks that ended in an error, and a list that couldn't be read.
    pub(crate) failures: Vec<String>,
    /// A backup or a restore held the backup operation: the
    /// generations left wait for the retry.
    pub(crate) blocked: Option<String>,
    /// The stop ended the checks; the generation in hand recorded
    /// nothing.
    pub(crate) stopped: bool,
}

/// Whether the generations are checked after this sample. A stopped
/// sample ends the run. A failed one doesn't keep them from their
/// check: the two read different files.
pub(crate) fn generations_follow(sample: &Result<SampleReport, StoreError>) -> bool {
    !matches!(sample, Ok(report) if report.stopped)
}

/// Check every generation the week still owes, never-checked first. An
/// error on one doesn't keep the others from their check, except the
/// held operation, which would refuse each of them.
fn check_generations(resources: &Resources, stop: &AtomicBool) -> GenerationChecks {
    tally_generations(resources.generations_to_verify(), stop, |generation| {
        resources.verify_generation(generation, stop)
    })
}

/// [`check_generations`] over the owed list as it was read, with the
/// check itself supplied: the stop is looked at before each
/// generation, and a list that couldn't be read is a failure.
pub(crate) fn tally_generations(
    owed: Result<Vec<String>, StoreError>,
    stop: &AtomicBool,
    mut check: impl FnMut(&str) -> Result<GenerationCheck, StoreError>,
) -> GenerationChecks {
    let mut checks = GenerationChecks::default();
    let owed = match owed {
        Ok(owed) => owed,
        Err(error) => {
            checks.failures.push(error.to_string());
            return checks;
        }
    };
    for generation in &owed {
        if stop.load(Ordering::SeqCst) {
            checks.stopped = true;
            break;
        }
        match check(generation) {
            Ok(GenerationCheck::Recorded(GenerationVerdict::Intact)) => checks.intact += 1,
            Ok(GenerationCheck::Recorded(GenerationVerdict::Damaged)) => checks.damaged += 1,
            Ok(GenerationCheck::Recorded(GenerationVerdict::Missing)) => checks.missing += 1,
            Ok(GenerationCheck::DestinationAbsent) => checks.unreachable += 1,
            // Retention removed it since the list was read.
            Ok(GenerationCheck::Gone) => {}
            Ok(GenerationCheck::Stopped) => {
                checks.stopped = true;
                break;
            }
            Err(error @ StoreError::Conflict { .. }) => {
                checks.blocked = Some(error.to_string());
                break;
            }
            Err(error) => checks
                .failures
                .push(format!("generation {generation}: {error}")),
        }
    }
    checks
}

/// How a verification ended. A damaged or missing object or generation
/// is a finding, recorded on its row, and the run that found it is
/// `complete`. What couldn't be read to a verdict makes the run
/// `failed`, outranking a held operation (`blocked`) and a stop
/// (`cancelled`): it is the evidence that needs looking at.
pub(crate) fn verify_outcome(
    sample: Result<SampleReport, StoreError>,
    checks: &GenerationChecks,
) -> (MaintenanceOutcome, String) {
    let mut failures = Vec::new();
    let (sampled, sample_stopped) = match sample {
        Ok(report) => {
            failures.extend(report.failure_details.iter().cloned());
            (
                format!(
                    "sample: buckets {}, checked {}, intact {}, missing {}, quarantined {}, \
                     healed {}, gone {}, failed {}",
                    report.buckets.len(),
                    report.checked,
                    report.intact,
                    report.missing,
                    report.quarantined,
                    report.healed,
                    report.gone,
                    report.failed,
                ),
                report.stopped,
            )
        }
        Err(error) => {
            failures.push(error.to_string());
            ("sample: failed".to_owned(), false)
        }
    };
    failures.extend(checks.failures.iter().cloned());
    let counts = format!(
        "{sampled}; generations: intact {}, damaged {}, missing {}, unreachable {}, failed {}",
        checks.intact,
        checks.damaged,
        checks.missing,
        checks.unreachable,
        checks.failures.len(),
    );
    if !failures.is_empty() {
        let failures = failures.join("; ");
        return (MaintenanceOutcome::Failed, format!("{counts}: {failures}"));
    }
    if let Some(held) = &checks.blocked {
        return (MaintenanceOutcome::Blocked, format!("{counts}: {held}"));
    }
    if sample_stopped || checks.stopped {
        return (MaintenanceOutcome::Cancelled, format!("stopped: {counts}"));
    }
    (MaintenanceOutcome::Complete, counts)
}

/// Record a run's end. A record that can't be written leaves the run
/// unfinished, which reads as interrupted: the job is due again after
/// [`POLL`].
fn finish(
    resources: &Resources,
    jobs: &MaintenanceJobs,
    job: MaintenanceJob,
    outcome: MaintenanceOutcome,
    detail: &str,
) {
    let _ = resources.finish_maintenance(job, outcome, Some(detail));
    #[cfg(test)]
    jobs.event(Event::Finished(job, outcome));
    #[cfg(not(test))]
    let _ = jobs;
}

/// How a refused snapshot ends the run: another backup, a restore or
/// a generation check holding the backup operation, or a generation
/// in progress, is a safety gate; anything else a failure.
pub(crate) fn snapshot_outcome(error: StoreError) -> (MaintenanceOutcome, String) {
    match error {
        StoreError::Conflict { .. } => (MaintenanceOutcome::Blocked, error.to_string()),
        _ => (MaintenanceOutcome::Failed, error.to_string()),
    }
}

/// How a backup's copy and publication ended. A copy the stop ended
/// fails `Unavailable` without an I/O kind at its next object; with
/// the stop raised, that is the stop. Any other error is a failure,
/// an I/O error under the stop and retention's after the generation
/// completed included.
pub(crate) fn backup_outcome(
    result: Result<BackupReceipt, StoreError>,
    cancelled: bool,
) -> (MaintenanceOutcome, String) {
    match result {
        Ok(receipt) => (
            MaintenanceOutcome::Complete,
            format!("generation {}", receipt.generation_id()),
        ),
        Err(error @ StoreError::Unavailable { io_kind: None, .. }) if cancelled => {
            (MaintenanceOutcome::Cancelled, error.to_string())
        }
        Err(error) => (MaintenanceOutcome::Failed, error.to_string()),
    }
}

/// How a GC pass ended. A stop shows in the report, never as an error.
/// Failed removals outrank a stop: they are the evidence that needs
/// looking at.
pub(crate) fn gc_outcome(result: Result<GcReport, StoreError>) -> (MaintenanceOutcome, String) {
    let report = match result {
        Ok(report) => report,
        Err(error) => return (MaintenanceOutcome::Failed, error.to_string()),
    };
    if let Some(gate) = report.blocked {
        return (MaintenanceOutcome::Blocked, gate_detail(gate).to_owned());
    }
    let counts = format!(
        "scanned {}, removed {}, grace started {}, grace cleared {}, kept {}, skipped {}, failed {}",
        report.scanned,
        report.removed,
        report.grace_started,
        report.grace_cleared,
        report.kept,
        report.skipped,
        report.failed,
    );
    if report.failed > 0 {
        let failures = report.failure_details.join("; ");
        return (MaintenanceOutcome::Failed, format!("{counts}: {failures}"));
    }
    if report.stopped {
        return (MaintenanceOutcome::Cancelled, format!("stopped: {counts}"));
    }
    (MaintenanceOutcome::Complete, counts)
}

fn gate_detail(gate: GcGate) -> &'static str {
    match gate {
        GcGate::BackupInProgress => "a backup generation is in progress",
        GcGate::BreakerOpen => "an account's circuit breaker is open",
        GcGate::IntegrityDoubt => "an object is missing or quarantined",
    }
}

#[cfg(test)]
mod tests;
