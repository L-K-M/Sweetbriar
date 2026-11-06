//! The scheduled maintenance loop (spec/03, plan M1.8h): one long-lived
//! worker wakes on a tick and runs each due job — the generation while
//! there were changes, the GC pass, the weekly rotating object
//! verification and the weekly `integrity_check` — recording every
//! run's evidence and the cursors the next due-check needs in the
//! store's `settings` (the durable recovery-point evidence the plan
//! asks for).
//!
//! Due-checks compare the seam clock's "now" against the `at` cursor
//! each job last ran with: a missing cursor means the job has never
//! run and is due. The backup cursor is the list generation the last
//! generation covered; a generation is only taken when the list
//! generation has since moved ("while there were changes").
//!
//! The worker counts as blocking work: a restore's quiesce drains it
//! before the swap (its `stop` is `BlockingWork::closing`, which the
//! restore pause raises), so its held `Resources` is released before
//! `try_unwrap` runs. Shutdown unwinds it the same way, and exiting
//! clears `running` so the post-restore restart can spawn it again.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread::sleep;
use std::time::{Duration, Instant};

use sb_resources::{
    Resources, StoreError, Timestamp, WritePriority, list_generation, read_all, upsert,
};

use crate::engine::{Engine, Spawn, Worker};

/// How often the worker wakes to check what is due: the hourly
/// generation needs minute-scale punctuality.
const TICK: Duration = Duration::from_secs(60);

/// The production cadences (spec/03): a generation every hour while
/// there were changes, GC on the same hourly beat, and the weekly
/// object-verification and `integrity_check` passes.
const PRODUCTION: SchedulePeriods = SchedulePeriods {
    tick: TICK,
    backup: Duration::from_secs(60 * 60),
    gc: Duration::from_secs(60 * 60),
    verify: Duration::from_secs(7 * 24 * 60 * 60),
    integrity: Duration::from_secs(7 * 24 * 60 * 60),
};

/// `stop` polls between sleeps of this size, so a pause or shutdown
/// unwinds the worker well inside its tick.
const SUBTICK: Duration = Duration::from_millis(250);

/// Settings keys: `maintenance.<job>.at` is the millisecond the job
/// last ran, `maintenance.backup.cursor` the list generation the last
/// generation covered, `maintenance.verify.rotation` the pass index
/// just taken, and `maintenance.<job>.last` the run's JSON evidence.
mod keys {
    pub(super) const BACKUP_AT: &str = "maintenance.backup.at";
    pub(super) const BACKUP_CURSOR: &str = "maintenance.backup.cursor";
    pub(super) const BACKUP_LAST: &str = "maintenance.backup.last";
    pub(super) const GC_AT: &str = "maintenance.gc.at";
    pub(super) const GC_LAST: &str = "maintenance.gc.last";
    pub(super) const VERIFY_AT: &str = "maintenance.verify.at";
    pub(super) const VERIFY_ROTATION: &str = "maintenance.verify.rotation";
    pub(super) const VERIFY_LAST: &str = "maintenance.verify.last";
    pub(super) const INTEGRITY_AT: &str = "maintenance.integrity.at";
    pub(super) const INTEGRITY_LAST: &str = "maintenance.integrity.last";
}

/// The four cadences plus the worker tick. Tests replace them with
/// millisecond periods; the seam clock decides what is due.
#[derive(Clone, Copy)]
pub(crate) struct SchedulePeriods {
    pub(crate) tick: Duration,
    pub(crate) backup: Duration,
    pub(crate) gc: Duration,
    pub(crate) verify: Duration,
    pub(crate) integrity: Duration,
}

/// The scheduled-maintenance owner: holds the current cadences and
/// admits exactly one worker. Restore resets neither it nor the
/// cursors — the restored store's `settings` carry them.
pub(crate) struct Schedules {
    periods: Mutex<SchedulePeriods>,
    running: AtomicBool,
}

impl Schedules {
    pub(crate) fn new() -> Self {
        Self {
            periods: Mutex::new(PRODUCTION),
            running: AtomicBool::new(false),
        }
    }

    #[cfg(test)]
    pub(crate) fn set_periods(&self, periods: SchedulePeriods) {
        *lock(&self.periods) = periods;
    }

    fn periods(&self) -> SchedulePeriods {
        *lock(&self.periods)
    }

    /// Start the maintenance worker once; a second call is a no-op and
    /// a refusal to spawn leaves the engine running unscheduled (the
    /// next open retries, the way an exited worker's restart does).
    fn schedule(self: &Arc<Self>, engine: &Engine) {
        if self.running.swap(true, Ordering::AcqRel) {
            return;
        }
        let owner = Arc::clone(self);
        if engine
            .spawn_worker(move |worker| async move { run(worker, owner).await })
            .is_err()
        {
            self.running.store(false, Ordering::Release);
        }
    }

    /// One due-check tick: run every job whose interval elapsed and
    /// record each run's evidence. A job's failure leaves its `at`
    /// cursor behind, so the next tick retries it.
    fn tick(&self, resources: &Resources) -> Result<(), StoreError> {
        let now = resources.now().as_millis();
        let settings = resources.read(read_all)?;
        let get = |key: &str| {
            settings
                .iter()
                .find(|setting| setting.key() == key)
                .map(|setting| setting.value_json().to_owned())
        };
        let at = |key: &str| get(key).and_then(|value| value.parse::<i64>().ok());
        let periods = self.periods();

        if due(now, at(keys::BACKUP_AT), periods.backup) {
            self.run_backup(resources, now, &get)?;
        }
        if due(now, at(keys::GC_AT), periods.gc) {
            self.run_gc(resources, now)?;
        }
        if due(now, at(keys::VERIFY_AT), periods.verify) {
            let rotation = at(keys::VERIFY_ROTATION).map_or(0, |r| (r as u64) + 1);
            self.run_verify(resources, now, rotation)?;
        }
        if due(now, at(keys::INTEGRITY_AT), periods.integrity) {
            self.run_integrity(resources, now)?;
        }
        Ok(())
    }

    /// A generation while there were changes (spec/03): the cursor is
    /// the list generation the last generation covered; a newer list
    /// generation means changes, a missing cursor means none was ever
    /// taken. The check advances `at` either way so the next check is
    /// one interval away.
    fn run_backup(
        &self,
        resources: &Resources,
        now: i64,
        get: &dyn Fn(&str) -> Option<String>,
    ) -> Result<(), StoreError> {
        let generation = resources.read(list_generation)?;
        let cursor = get(keys::BACKUP_CURSOR).and_then(|v| v.parse::<u64>().ok());
        let changed = cursor.is_none_or(|c| generation > c);
        let mut last = None;
        let mut covered = cursor;
        if changed {
            match resources
                .backup_snapshot(sb_resources::BackupDestination::DataRoot)
                .and_then(|snapshot| resources.complete_backup(snapshot))
            {
                Ok(receipt) => {
                    covered = Some(generation);
                    last = Some(format!(
                        "{{\"generation\":\"{}\",\"location\":\"{}\"}}",
                        receipt.generation_id(),
                        escape(&receipt.location().display().to_string())
                    ));
                }
                Err(err) => {
                    last = Some(format!("{{\"error\":\"{}\"}}", escape(&err.to_string())));
                }
            }
        }
        write_evidence(
            resources,
            now,
            &[
                (keys::BACKUP_AT, now.to_string()),
                (keys::BACKUP_CURSOR, covered.unwrap_or(0).to_string()),
            ],
            keys::BACKUP_LAST,
            last,
        )
    }

    fn run_gc(&self, resources: &Resources, now: i64) -> Result<(), StoreError> {
        let report = resources.collect_garbage(sb_resources::GC_GRACE_PERIOD);
        let evidence = report.as_ref().map_or_else(
            |err| format!("{{\"error\":\"{}\"}}", escape(&err.to_string())),
            |report| {
                format!(
                    "{{\"scanned\":{},\"kept\":{},\"grace_started\":{},\"removed\":{}}}",
                    report.scanned, report.kept, report.grace_started, report.removed
                )
            },
        );
        write_evidence(
            resources,
            now,
            &[(keys::GC_AT, now.to_string())],
            keys::GC_LAST,
            Some(evidence),
        )
    }

    fn run_verify(&self, resources: &Resources, now: i64, rotation: u64) -> Result<(), StoreError> {
        let report = resources.verify_objects(rotation);
        let evidence = report.as_ref().map_or_else(
            |err| format!("{{\"error\":\"{}\"}}", escape(&err.to_string())),
            |report| {
                format!(
                    "{{\"sampled\":{},\"verified\":{},\"missing\":{},\"quarantined\":{}}}",
                    report.sampled, report.verified, report.missing, report.quarantined
                )
            },
        );
        write_evidence(
            resources,
            now,
            &[
                (keys::VERIFY_AT, now.to_string()),
                (keys::VERIFY_ROTATION, rotation.to_string()),
            ],
            keys::VERIFY_LAST,
            Some(evidence),
        )
    }

    fn run_integrity(&self, resources: &Resources, now: i64) -> Result<(), StoreError> {
        let findings = resources.integrity_check();
        let evidence = findings.as_ref().map_or_else(
            |err| format!("{{\"error\":\"{}\"}}", escape(&err.to_string())),
            |findings| {
                let ok = findings.len() == 1 && findings[0] == "ok";
                format!(
                    "{{\"ok\":{ok},\"findings\":\"{}\"}}",
                    escape(&findings.join("; "))
                )
            },
        );
        write_evidence(
            resources,
            now,
            &[(keys::INTEGRITY_AT, now.to_string())],
            keys::INTEGRITY_LAST,
            Some(evidence),
        )
    }
}

/// Whether a job is due: never run, or `interval` since its `at`.
fn due(now: i64, at: Option<i64>, interval: Duration) -> bool {
    let millis = i64::try_from(interval.as_millis()).unwrap_or(i64::MAX);
    at.is_none_or(|at| now.saturating_sub(at) >= millis)
}

/// The worker body: sub-tick sleeps between `stop` polls, a due-check
/// once the tick elapsed, and `running` cleared on the way out so a
/// post-restore `schedule` can spawn again. A tick's error ends
/// nothing — the next wake retries what the failed jobs left due.
async fn run(worker: Worker, schedules: Arc<Schedules>) {
    let stop = worker.shutdown_check();
    let owner = Arc::clone(&schedules);
    let _ = worker
        .blocking(move |resources| {
            let mut last_tick = Instant::now();
            loop {
                if stop() {
                    return;
                }
                sleep(SUBTICK);
                if last_tick.elapsed() >= owner.periods().tick {
                    last_tick = Instant::now();
                    let _ = owner.tick(resources);
                }
            }
        })
        .await;
    schedules.running.store(false, Ordering::Release);
}

/// Persist a run's cursors and evidence atomically (spec/02 rule 9's
/// `upsert` keeps revision and `updated_at` itself).
fn write_evidence(
    resources: &Resources,
    now: i64,
    cursors: &[(&str, String)],
    last_key: &str,
    last: Option<String>,
) -> Result<(), StoreError> {
    let at = Timestamp::from_millis(now);
    let cursors: Vec<(String, String)> = cursors
        .iter()
        .map(|(key, value)| (key.to_string(), value.clone()))
        .collect();
    let last = last.map(|json| (last_key.to_string(), json));
    tokio::runtime::Handle::current().block_on(resources.write(WritePriority::Index, move |tx| {
        for (key, value) in &cursors {
            upsert(tx, key, value, at)?;
        }
        if let Some((key, json)) = &last {
            upsert(tx, key, json, at)?;
        }
        Ok(())
    }))
}

/// A JSON-string escape for the evidence fields (these are generated
/// paths and messages, never user markup).
fn escape(value: &str) -> String {
    value.replace('\\', "\\\\").replace('"', "\\\"")
}

fn lock<T>(value: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    value.lock().unwrap_or_else(|err| err.into_inner())
}

impl Engine {
    /// Spawn the maintenance worker once per open (spec/03); the
    /// restore path reschedules it the same way after the swap.
    pub(crate) fn schedule_maintenance(&self) {
        self.schedules.schedule(self);
    }
}
