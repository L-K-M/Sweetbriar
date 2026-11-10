//! The maintenance scheduler: its cadence from the records, the jobs'
//! outcomes, stopping, and the safety gates.

use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use sb_resources::{
    BackupDestination, GcGate, GcReport, MaintenanceFinish, MaintenanceJob, MaintenanceOutcome,
    MaintenanceRun, StoreError, Timestamp,
};
use sb_test_support::{ScratchDir, block_on, real_fs};

use super::{
    BACKUP_PERIOD, BACKUP_RETRY, Cadence, Event, GC_PERIOD, GC_RETRY, POLL, SETTLE, backup_outcome,
    due_at, floor_at, gc_outcome, snapshot_outcome,
};
use crate::Engine;
use crate::index_tests::{TestClock, clocked, count, import, wait, write};

/// A Monday, 2026-11-09 00:00 UTC.
const MONDAY: i64 = 1_794_182_400_000;
const RAW: &[u8] = b"From: Sender <sender@example.test>\r\nTo: other@example.test\r\nSubject: kept\r\n\r\nbody\r\n";
const COMPLETE: &str = "SELECT count(*) FROM backup_generations WHERE state='complete'";
const FAILED: &str = "SELECT count(*) FROM backup_generations WHERE state='failed'";
const GENERATIONS: &str = "SELECT count(*) FROM backup_generations";

fn ms(period: Duration) -> i64 {
    i64::try_from(period.as_millis()).unwrap()
}

/// An engine on a manual clock with its scheduler started.
fn scheduled(root: &Path, at: i64) -> (Engine, Arc<TestClock>) {
    let (engine, clock) = clocked(root, real_fs(), at);
    engine.schedule_maintenance();
    parked(&engine);
    (engine, clock)
}

fn parked(engine: &Engine) {
    wait(|| engine.maintenance().waiting());
}

/// Move the clock by `by`, wake the scheduler and wait for its park.
fn advance(engine: &Engine, clock: &TestClock, by: Duration) {
    clock.0.fetch_add(ms(by), Ordering::SeqCst);
    engine.maintenance().wake();
    parked(engine);
}

fn run_of(engine: &Engine, job: MaintenanceJob) -> Option<MaintenanceRun> {
    let runs = engine.resources().maintenance_runs().unwrap();
    runs.into_iter().find(|run| run.job == job)
}

fn outcome_of(engine: &Engine, job: MaintenanceJob) -> Option<MaintenanceOutcome> {
    run_of(engine, job)?.finished.map(|end| end.outcome)
}

fn started_of(engine: &Engine, job: MaintenanceJob) -> i64 {
    run_of(engine, job).unwrap().started_at.as_millis()
}

/// A committed data write (a setting's new revision), which makes a
/// backup due.
fn change(engine: &Engine) {
    let key = engine.new_idempotency_key();
    block_on(engine.set_setting(&key, "appearance.compose_cue", "strong")).unwrap();
    assert!(engine.resources().backup_due().unwrap());
}

fn hook(engine: &Engine, body: impl Fn(Event) + Send + Sync + 'static) {
    *engine.maintenance().hook.lock().unwrap() = Some(Arc::new(body));
}

// --- cadence ---

#[test]
fn a_fresh_store_settles_then_backs_up_and_collects_garbage() {
    let scratch = ScratchDir::new("maintenance-fresh");
    let (engine, clock) = scheduled(scratch.path(), MONDAY);
    assert!(engine.resources().maintenance_runs().unwrap().is_empty());

    advance(&engine, &clock, SETTLE - Duration::from_millis(1));
    assert!(engine.resources().maintenance_runs().unwrap().is_empty());
    assert_eq!(count(&engine, COMPLETE), 0);

    advance(&engine, &clock, Duration::from_millis(1));
    let backup = run_of(&engine, MaintenanceJob::Backup).unwrap();
    let end = backup.finished.unwrap();
    assert_eq!(
        end.outcome,
        MaintenanceOutcome::Complete,
        "{:?}",
        end.detail
    );
    assert!(end.detail.unwrap().starts_with("generation "));
    assert_eq!(count(&engine, COMPLETE), 1);
    let gc = run_of(&engine, MaintenanceJob::Gc).unwrap();
    assert_eq!(gc.finished.unwrap().outcome, MaintenanceOutcome::Complete);
    assert!(!engine.resources().backup_due().unwrap());
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_backup_waits_an_hour_and_for_changes() {
    let scratch = ScratchDir::new("maintenance-hourly");
    let (engine, clock) = scheduled(scratch.path(), MONDAY);
    advance(&engine, &clock, SETTLE);
    let first = started_of(&engine, MaintenanceJob::Backup);

    // A change ten minutes on waits for the hour since the last start.
    advance(&engine, &clock, Duration::from_secs(10 * 60));
    change(&engine);
    advance(
        &engine,
        &clock,
        BACKUP_PERIOD - Duration::from_secs(10 * 60) - Duration::from_millis(1),
    );
    assert_eq!(started_of(&engine, MaintenanceJob::Backup), first);
    assert_eq!(count(&engine, COMPLETE), 1);
    advance(&engine, &clock, Duration::from_millis(1));
    assert_eq!(
        started_of(&engine, MaintenanceJob::Backup),
        first + ms(BACKUP_PERIOD)
    );
    assert_eq!(count(&engine, COMPLETE), 2);

    // An hour without changes records nothing.
    advance(&engine, &clock, BACKUP_PERIOD);
    assert_eq!(
        started_of(&engine, MaintenanceJob::Backup),
        first + ms(BACKUP_PERIOD)
    );
    assert_eq!(count(&engine, COMPLETE), 2);

    // After that idle hour, a change is backed up at the next look,
    // within a minute.
    change(&engine);
    advance(&engine, &clock, POLL);
    assert_eq!(count(&engine, COMPLETE), 3);
    assert_eq!(
        started_of(&engine, MaintenanceJob::Backup),
        first + 2 * ms(BACKUP_PERIOD) + ms(POLL)
    );
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn garbage_is_collected_daily() {
    let scratch = ScratchDir::new("maintenance-daily");
    let (engine, clock) = scheduled(scratch.path(), MONDAY);
    advance(&engine, &clock, SETTLE);
    let first = started_of(&engine, MaintenanceJob::Gc);

    advance(&engine, &clock, GC_PERIOD - Duration::from_millis(1));
    assert_eq!(started_of(&engine, MaintenanceJob::Gc), first);
    advance(&engine, &clock, Duration::from_millis(1));
    assert_eq!(
        started_of(&engine, MaintenanceJob::Gc),
        first + ms(GC_PERIOD)
    );
    assert_eq!(
        outcome_of(&engine, MaintenanceJob::Gc),
        Some(MaintenanceOutcome::Complete)
    );
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_reopen_picks_the_cadence_up_from_the_records() {
    let scratch = ScratchDir::new("maintenance-reopen");
    let (engine, clock) = scheduled(scratch.path(), MONDAY);
    advance(&engine, &clock, SETTLE);
    let first = started_of(&engine, MaintenanceJob::Backup);
    let gc = started_of(&engine, MaintenanceJob::Gc);
    block_on(engine.shutdown()).unwrap();

    // Thirty minutes on, with a change: the backup waits for its hour.
    let (engine, clock) = scheduled(scratch.path(), first + 30 * 60 * 1000);
    change(&engine);
    advance(&engine, &clock, SETTLE);
    assert_eq!(started_of(&engine, MaintenanceJob::Backup), first);
    advance(&engine, &clock, Duration::from_secs(30 * 60) - SETTLE);
    assert_eq!(
        started_of(&engine, MaintenanceJob::Backup),
        first + ms(BACKUP_PERIOD)
    );
    assert_eq!(started_of(&engine, MaintenanceJob::Gc), gc);

    // A run that never ended, or ended cancelled, is due at once.
    let (mut engine, mut clock) = (engine, clock);
    for end in [
        "finished_at = NULL, outcome = NULL, detail = NULL",
        "outcome = 'cancelled'",
    ] {
        let sql = format!("UPDATE maintenance_jobs SET {end} WHERE job = 'gc'");
        write(&engine, move |tx| {
            tx.execute_batch(&sql)
                .map_err(|e| StoreError::internal(e.to_string()))
        });
        let at = clock.0.load(Ordering::SeqCst);
        block_on(engine.shutdown()).unwrap();
        (engine, clock) = scheduled(scratch.path(), at);
        advance(&engine, &clock, SETTLE);
        assert_eq!(
            started_of(&engine, MaintenanceJob::Gc),
            at + ms(SETTLE),
            "{end}"
        );
        assert_eq!(
            outcome_of(&engine, MaintenanceJob::Gc),
            Some(MaintenanceOutcome::Complete)
        );
    }
    block_on(engine.shutdown()).unwrap();
}

// --- stopping ---

#[test]
fn a_shutdown_ends_a_parked_scheduler_at_once() {
    let scratch = ScratchDir::new("maintenance-parked");
    let (engine, _clock) = scheduled(scratch.path(), MONDAY);
    let started = Instant::now();
    block_on(engine.shutdown()).unwrap();
    assert!(started.elapsed() < POLL / 2, "{:?}", started.elapsed());
}

#[test]
fn a_stop_during_gc_records_it_cancelled() {
    let scratch = ScratchDir::new("maintenance-stop-gc");
    let (engine, clock) = scheduled(scratch.path(), MONDAY);
    let jobs = engine.maintenance().clone();
    hook(&engine, move |event| {
        if event == Event::Running(MaintenanceJob::Gc) {
            jobs.stop();
        }
    });
    clock.0.fetch_add(ms(SETTLE), Ordering::SeqCst);
    engine.maintenance().wake();
    wait(|| outcome_of(&engine, MaintenanceJob::Gc).is_some());

    let end = run_of(&engine, MaintenanceJob::Gc)
        .unwrap()
        .finished
        .unwrap();
    assert_eq!(
        end.outcome,
        MaintenanceOutcome::Cancelled,
        "{:?}",
        end.detail
    );
    assert!(end.detail.unwrap().starts_with("stopped: "));
    assert_eq!(
        outcome_of(&engine, MaintenanceJob::Backup),
        Some(MaintenanceOutcome::Complete)
    );
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_stop_after_the_start_record_skips_the_snapshot() {
    let scratch = ScratchDir::new("maintenance-stop-start");
    let (engine, clock) = scheduled(scratch.path(), MONDAY);
    let jobs = engine.maintenance().clone();
    hook(&engine, move |event| {
        if event == Event::Running(MaintenanceJob::Backup) {
            jobs.stop();
        }
    });
    clock.0.fetch_add(ms(SETTLE), Ordering::SeqCst);
    engine.maintenance().wake();
    wait(|| outcome_of(&engine, MaintenanceJob::Backup).is_some());

    let end = run_of(&engine, MaintenanceJob::Backup)
        .unwrap()
        .finished
        .unwrap();
    assert_eq!(end.outcome, MaintenanceOutcome::Cancelled);
    assert_eq!(end.detail.as_deref(), Some("stopped before the snapshot"));
    assert_eq!(count(&engine, GENERATIONS), 0);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_stop_during_a_backup_copy_records_it_cancelled_and_the_next_run_recovers() {
    let scratch = ScratchDir::new("maintenance-stop-backup");
    let (engine, clock) = clocked(scratch.path(), real_fs(), MONDAY);
    // An object, so the copy has a next object to stop at.
    import(&engine, &scratch.path().join("kept.eml"), RAW);
    let jobs = engine.maintenance().clone();
    hook(&engine, move |event| {
        if event == Event::Snapshotted {
            jobs.stop();
        }
    });
    engine.schedule_maintenance();
    parked(&engine);
    clock.0.fetch_add(ms(SETTLE), Ordering::SeqCst);
    engine.maintenance().wake();
    wait(|| outcome_of(&engine, MaintenanceJob::Backup).is_some());

    let end = run_of(&engine, MaintenanceJob::Backup)
        .unwrap()
        .finished
        .unwrap();
    assert_eq!(
        end.outcome,
        MaintenanceOutcome::Cancelled,
        "{:?}",
        end.detail
    );
    assert_eq!(
        run_of(&engine, MaintenanceJob::Gc),
        None,
        "stopped before GC"
    );
    assert_eq!(count(&engine, COMPLETE), 0);
    let at = clock.0.load(Ordering::SeqCst);
    block_on(engine.shutdown()).unwrap();

    // The reopen's run is due at once and recovers the generation.
    let (engine, clock) = scheduled(scratch.path(), at);
    advance(&engine, &clock, SETTLE);
    assert_eq!(
        outcome_of(&engine, MaintenanceJob::Backup),
        Some(MaintenanceOutcome::Complete)
    );
    assert_eq!((count(&engine, COMPLETE), count(&engine, FAILED)), (1, 1));
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_stopped_scheduler_does_not_start_again() {
    let scratch = ScratchDir::new("maintenance-stopped");
    let (engine, clock) = scheduled(scratch.path(), MONDAY);
    engine.maintenance().stop();
    wait(|| !engine.maintenance().lock().running);
    engine.schedule_maintenance();
    assert!(!engine.maintenance().lock().running);
    clock.0.fetch_add(ms(SETTLE), Ordering::SeqCst);
    std::thread::sleep(Duration::from_millis(50));
    assert!(engine.resources().maintenance_runs().unwrap().is_empty());
    block_on(engine.shutdown()).unwrap();
}

// --- safety gates ---

#[test]
fn a_generation_in_progress_blocks_both_jobs() {
    let scratch = ScratchDir::new("maintenance-blocked");
    let (engine, clock) = scheduled(scratch.path(), MONDAY);
    let held = engine
        .resources()
        .backup_snapshot(BackupDestination::DataRoot)
        .unwrap();
    advance(&engine, &clock, SETTLE);

    let backup = run_of(&engine, MaintenanceJob::Backup)
        .unwrap()
        .finished
        .unwrap();
    assert_eq!(
        backup.outcome,
        MaintenanceOutcome::Blocked,
        "{:?}",
        backup.detail
    );
    let gc = run_of(&engine, MaintenanceJob::Gc)
        .unwrap()
        .finished
        .unwrap();
    assert_eq!(gc.outcome, MaintenanceOutcome::Blocked);
    assert_eq!(
        gc.detail.as_deref(),
        Some("a backup generation is in progress")
    );

    // Blocked tries again after the retry, not the period.
    engine.resources().complete_backup(held).unwrap();
    change(&engine);
    advance(&engine, &clock, BACKUP_RETRY - Duration::from_millis(1));
    assert_eq!(
        outcome_of(&engine, MaintenanceJob::Backup),
        Some(MaintenanceOutcome::Blocked)
    );
    advance(&engine, &clock, Duration::from_millis(1));
    assert_eq!(
        outcome_of(&engine, MaintenanceJob::Backup),
        Some(MaintenanceOutcome::Complete)
    );
    assert_eq!(
        outcome_of(&engine, MaintenanceJob::Gc),
        Some(MaintenanceOutcome::Blocked)
    );
    advance(&engine, &clock, GC_RETRY - BACKUP_RETRY);
    assert_eq!(
        outcome_of(&engine, MaintenanceJob::Gc),
        Some(MaintenanceOutcome::Complete)
    );
    block_on(engine.shutdown()).unwrap();
}

// --- the pure rules ---

fn run(started_at: i64, outcome: Option<MaintenanceOutcome>) -> MaintenanceRun {
    MaintenanceRun {
        job: MaintenanceJob::Backup,
        started_at: Timestamp::from_millis(started_at),
        finished: outcome.map(|outcome| MaintenanceFinish {
            at: Timestamp::from_millis(started_at + 1),
            outcome,
            detail: None,
        }),
        succeeded_at: None,
    }
}

#[test]
fn a_job_is_due_a_period_after_its_last_start_unless_it_never_ended_well() {
    let hour = ms(BACKUP_PERIOD);
    let minute = 60 * 1000;
    let cadence = Cadence {
        period: BACKUP_PERIOD,
        retry: Duration::from_secs(5 * 60),
    };
    let now = Timestamp::from_millis(10 * hour);
    let due = |latest: Option<MaintenanceRun>| due_at(latest.as_ref(), cadence, now);
    let at = Timestamp::from_millis;
    assert_eq!(due(None), now, "never ran");
    assert_eq!(due(Some(run(9 * hour, None))), now, "interrupted");
    let cancelled = Some(MaintenanceOutcome::Cancelled);
    assert_eq!(due(Some(run(9 * hour, cancelled))), now);
    let complete = Some(MaintenanceOutcome::Complete);
    assert_eq!(due(Some(run(9 * hour, complete))), at(10 * hour));
    for retried in [MaintenanceOutcome::Blocked, MaintenanceOutcome::Failed] {
        let latest = Some(run(10 * hour - minute, Some(retried)));
        assert_eq!(due(latest), at(10 * hour + 4 * minute), "{retried:?}");
    }
    let ahead = Some(run(40 * hour, complete));
    assert_eq!(due(ahead), at(11 * hour), "a clock moved back");

    // The minute's floor holds within a minute, and a clock moved back
    // pulls it in rather than holding the job for the jump.
    assert_eq!(floor_at(None, now), now);
    assert_eq!(
        floor_at(Some(at(10 * hour + minute / 2)), now),
        at(10 * hour + minute / 2)
    );
    assert_eq!(
        floor_at(Some(at(10 * hour + 30 * minute)), now),
        at(10 * hour + minute)
    );
}

#[test]
fn a_backups_error_is_a_stop_a_gate_or_a_failure() {
    let conflict = || StoreError::Conflict {
        detail: "a backup operation is already running".into(),
    };
    let (outcome, detail) = snapshot_outcome(conflict());
    assert_eq!(outcome, MaintenanceOutcome::Blocked);
    assert!(detail.contains("already running"), "{detail}");
    let internal = || StoreError::internal("disk on fire");
    assert_eq!(snapshot_outcome(internal()).0, MaintenanceOutcome::Failed);

    // The copy: only `Unavailable` under a raised stop is the stop.
    let stopped = || {
        Err(StoreError::Unavailable {
            reason: "the backup was cancelled".into(),
            io_kind: None,
        })
    };
    assert_eq!(
        backup_outcome(stopped(), true).0,
        MaintenanceOutcome::Cancelled
    );
    assert_eq!(
        backup_outcome(stopped(), false).0,
        MaintenanceOutcome::Failed
    );
    assert_eq!(
        backup_outcome(Err(internal()), true).0,
        MaintenanceOutcome::Failed
    );
    let full = || {
        Err(StoreError::Unavailable {
            reason: "cannot copy an object: no space".into(),
            io_kind: Some(std::io::ErrorKind::StorageFull),
        })
    };
    assert_eq!(
        backup_outcome(full(), true).0,
        MaintenanceOutcome::Failed,
        "I/O under a stop"
    );
    assert_eq!(
        backup_outcome(Err(conflict()), false).0,
        MaintenanceOutcome::Failed
    );
}

#[test]
fn a_gc_report_maps_to_one_outcome() {
    let report = |edit: fn(&mut GcReport)| {
        let mut report = GcReport {
            scanned: 3,
            removed: 1,
            ..GcReport::default()
        };
        edit(&mut report);
        report
    };
    let (outcome, detail) = gc_outcome(Ok(report(|_| {})));
    assert_eq!(outcome, MaintenanceOutcome::Complete);
    assert_eq!(
        detail,
        "scanned 3, removed 1, grace started 0, grace cleared 0, kept 0, skipped 0, failed 0"
    );
    let gated = gc_outcome(Ok(report(|r| r.blocked = Some(GcGate::BreakerOpen))));
    assert_eq!(
        gated,
        (
            MaintenanceOutcome::Blocked,
            "an account's circuit breaker is open".into()
        )
    );
    let doubt = gc_outcome(Ok(report(|r| r.blocked = Some(GcGate::IntegrityDoubt))));
    assert_eq!(doubt.0, MaintenanceOutcome::Blocked);

    let stopped = gc_outcome(Ok(report(|r| r.stopped = true)));
    assert_eq!(stopped.0, MaintenanceOutcome::Cancelled);
    let failing = |r: &mut GcReport| {
        r.failed = 1;
        r.failure_details = vec!["unlink: busy".into()];
        r.stopped = true;
    };
    let (outcome, detail) = gc_outcome(Ok(report(failing)));
    assert_eq!(
        outcome,
        MaintenanceOutcome::Failed,
        "failures outrank the stop"
    );
    assert!(detail.ends_with(": unlink: busy"), "{detail}");

    let error = Err(StoreError::internal("no reader"));
    assert_eq!(
        gc_outcome(error).0,
        MaintenanceOutcome::Failed,
        "a stop is never an error"
    );
}
