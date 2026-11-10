//! The maintenance scheduler: its cadence from the records, the jobs'
//! outcomes, stopping, and the safety gates.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::time::{Duration, Instant};

use sb_resources::{
    BackupDestination, GcGate, GcReport, GenerationCheck, GenerationVerdict, IntegrityCheck,
    IntegrityFinding, MaintenanceFinish, MaintenanceJob, MaintenanceOutcome, MaintenanceRun,
    SampleReport, StoreError, Timestamp,
};
use sb_test_support::{ScratchDir, block_on, damage_an_index, real_fs};

use super::{
    BACKUP_PERIOD, BACKUP_RETRY, Cadence, Event, GC_PERIOD, GC_RETRY, GenerationChecks, Host,
    INTEGRITY_PERIOD, POLL, SETTLE, VERIFY_PERIOD, VERIFY_RETRY, backup_outcome, check_integrity,
    due_at, floor_at, gc_outcome, generations_follow, integrity_outcome, snapshot_outcome,
    tally_generations, verify_outcome,
};
use crate::index_tests::{TestClock, clocked, count, import, wait, write};
use crate::{Engine, Integrity};

mod acceptance;

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

#[test]
fn a_backup_that_outlasts_its_period_lets_the_next_job_run() {
    let scratch = ScratchDir::new("maintenance-long-backup");
    let (engine, clock) = scheduled(scratch.path(), MONDAY);
    let (entered, reached) = mpsc::channel();
    let (release, held) = mpsc::channel::<()>();
    let (entered, held) = (Mutex::new(entered), Mutex::new(held));
    let order = Arc::new(Mutex::new(Vec::new()));
    let (seen, first) = (order.clone(), AtomicBool::new(true));
    hook(&engine, move |event| match event {
        Event::Running(job) => seen.lock().unwrap().push(job),
        Event::Snapshotted if first.swap(false, Ordering::SeqCst) => {
            entered.lock().unwrap().send(()).unwrap();
            held.lock().unwrap().recv().unwrap();
        }
        _ => {}
    });

    // The first backup's copy runs for over an hour, and a change
    // commits meanwhile: by its record it is due again as it ends.
    clock.0.fetch_add(ms(SETTLE), Ordering::SeqCst);
    engine.maintenance().wake();
    reached.recv_timeout(Duration::from_secs(60)).unwrap();
    let long = BACKUP_PERIOD + Duration::from_secs(60);
    clock.0.fetch_add(ms(long), Ordering::SeqCst);
    change(&engine);
    release.send(()).unwrap();
    parked(&engine);

    // The jobs after it took their turn first, and the change waits.
    let ran = order.lock().unwrap().clone();
    assert_eq!(
        ran[..2],
        [MaintenanceJob::Backup, MaintenanceJob::Gc],
        "{ran:?}"
    );
    assert_eq!(count(&engine, COMPLETE), 1, "{ran:?}");
    assert!(engine.resources().backup_due().unwrap());

    // It is looked at again a minute after its run ended.
    advance(&engine, &clock, POLL - Duration::from_millis(1));
    assert_eq!(count(&engine, COMPLETE), 1);
    advance(&engine, &clock, Duration::from_millis(1));
    assert_eq!(count(&engine, COMPLETE), 2);
    assert_eq!(
        started_of(&engine, MaintenanceJob::Backup),
        MONDAY + ms(SETTLE) + ms(long) + ms(POLL)
    );
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
    // A worker that somehow still ran would look now.
    engine.maintenance().wake();
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

// --- the weekly verification (M1.8h4c) ---

/// The verification's recorded end.
fn verified(engine: &Engine) -> MaintenanceFinish {
    run_of(engine, MaintenanceJob::Verify)
        .unwrap()
        .finished
        .unwrap()
}

/// Each generation's recorded verdict, oldest first.
fn verdicts(engine: &Engine) -> Vec<Option<String>> {
    engine
        .resources()
        .read(|tx| {
            let mut statement = tx
                .prepare("SELECT verification FROM backup_generations ORDER BY started_at")
                .map_err(|error| StoreError::internal(error.to_string()))?;
            let rows = statement
                .query_map([], |row| row.get(0))
                .map_err(|error| StoreError::internal(error.to_string()))?;
            rows.collect::<Result<_, _>>()
                .map_err(|error| StoreError::internal(error.to_string()))
        })
        .unwrap()
}

#[test]
fn the_objects_and_the_generations_are_verified_weekly() {
    let scratch = ScratchDir::new("maintenance-verify-weekly");
    let (engine, clock) = clocked(scratch.path(), real_fs(), MONDAY);
    import(&engine, &scratch.path().join("kept.eml"), RAW);
    engine.schedule_maintenance();
    parked(&engine);

    // After the first backup and GC pass: the week's bucket, and the
    // generation the backup just completed.
    advance(&engine, &clock, SETTLE);
    let end = verified(&engine);
    assert_eq!(
        end.outcome,
        MaintenanceOutcome::Complete,
        "{:?}",
        end.detail
    );
    let detail = end.detail.unwrap();
    assert!(detail.starts_with("sample: buckets 1, "), "{detail}");
    assert!(
        detail.ends_with("generations: intact 1, damaged 0, missing 0, unreachable 0, failed 0"),
        "{detail}"
    );
    assert_eq!(verdicts(&engine), [Some("intact".to_owned())]);
    let first = started_of(&engine, MaintenanceJob::Verify);
    assert_eq!(first, MONDAY + ms(SETTLE));

    // Not again before a week since that start.
    advance(&engine, &clock, VERIFY_PERIOD - Duration::from_millis(1));
    assert_eq!(started_of(&engine, MaintenanceJob::Verify), first);
    advance(&engine, &clock, Duration::from_millis(1));
    assert_eq!(
        started_of(&engine, MaintenanceJob::Verify),
        first + ms(VERIFY_PERIOD)
    );
    let detail = verified(&engine).detail.unwrap();
    assert!(detail.starts_with("sample: buckets 1, "), "{detail}");
    assert!(detail.contains("generations: intact 1, "), "{detail}");
    block_on(engine.shutdown()).unwrap();
}

/// A damaged generation is what the check is for: the run that finds
/// it is complete, and the verdict is on the generation's row.
#[test]
fn a_damaged_generation_is_a_finding_of_a_complete_run() {
    let scratch = ScratchDir::new("maintenance-verify-damaged");
    let (engine, clock) = scheduled(scratch.path(), MONDAY);
    advance(&engine, &clock, SETTLE);
    assert_eq!(verdicts(&engine), [Some("intact".to_owned())]);
    let location: String = engine
        .resources()
        .read(|tx| {
            tx.query_row("SELECT location FROM backup_generations", [], |row| {
                row.get(0)
            })
            .map_err(|error| StoreError::internal(error.to_string()))
        })
        .unwrap();
    let image = Path::new(&location).join("state.sqlite");
    let mut bytes = std::fs::read(&image).unwrap();
    bytes[100] ^= 0xff;
    std::fs::write(&image, bytes).unwrap();

    advance(&engine, &clock, VERIFY_PERIOD);

    let end = verified(&engine);
    assert_eq!(
        end.outcome,
        MaintenanceOutcome::Complete,
        "{:?}",
        end.detail
    );
    let detail = end.detail.unwrap();
    assert!(
        detail.contains("generations: intact 0, damaged 1, "),
        "{detail}"
    );
    assert_eq!(verdicts(&engine), [Some("damaged".to_owned())]);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_stop_during_the_verification_records_it_cancelled() {
    let scratch = ScratchDir::new("maintenance-verify-stop");
    let (engine, clock) = scheduled(scratch.path(), MONDAY);
    let jobs = engine.maintenance().clone();
    hook(&engine, move |event| {
        if event == Event::Running(MaintenanceJob::Verify) {
            jobs.stop();
        }
    });
    clock.0.fetch_add(ms(SETTLE), Ordering::SeqCst);
    engine.maintenance().wake();
    wait(|| outcome_of(&engine, MaintenanceJob::Verify).is_some());

    let end = verified(&engine);
    assert_eq!(
        end.outcome,
        MaintenanceOutcome::Cancelled,
        "{:?}",
        end.detail
    );
    assert!(end.detail.unwrap().starts_with("stopped: "));
    // Nothing was read: the generation has no verdict, and the week's
    // work is still owed.
    assert_eq!(verdicts(&engine), [None]);
    assert!(engine.resources().verification_due().unwrap());
    block_on(engine.shutdown()).unwrap();
}

/// A generation's check needs the backup operation. While a backup
/// holds it the run is blocked, and tried again after the retry.
#[test]
fn a_held_backup_operation_blocks_the_generation_checks() {
    let scratch = ScratchDir::new("maintenance-verify-blocked");
    let (engine, clock) = scheduled(scratch.path(), MONDAY);
    advance(&engine, &clock, SETTLE);
    // A second generation, begun in a later hour so that retention
    // keeps both, and held open.
    let later = 2 * BACKUP_PERIOD;
    advance(&engine, &clock, later);
    let held = engine
        .resources()
        .backup_snapshot(BackupDestination::DataRoot)
        .unwrap();

    advance(&engine, &clock, VERIFY_PERIOD - later);

    let end = verified(&engine);
    assert_eq!(end.outcome, MaintenanceOutcome::Blocked, "{:?}", end.detail);
    let blocked = started_of(&engine, MaintenanceJob::Verify);
    // The sample ran; the first generation still has last week's check.
    assert!(end.detail.unwrap().starts_with("sample: buckets 1, "));

    engine.resources().complete_backup(held).unwrap();
    advance(&engine, &clock, VERIFY_RETRY - Duration::from_millis(1));
    assert_eq!(started_of(&engine, MaintenanceJob::Verify), blocked);
    advance(&engine, &clock, Duration::from_millis(1));
    let end = verified(&engine);
    assert_eq!(
        end.outcome,
        MaintenanceOutcome::Complete,
        "{:?}",
        end.detail
    );
    // The week's bucket is read already; both generations are checked.
    let detail = end.detail.unwrap();
    assert!(detail.starts_with("sample: buckets 0, "), "{detail}");
    assert!(detail.contains("generations: intact 2, "), "{detail}");
    block_on(engine.shutdown()).unwrap();
}

/// A destination that isn't there (an unmounted volume) is counted and
/// fails nothing; the generation keeps the verdict it had.
#[test]
fn an_absent_destination_is_counted_and_fails_nothing() {
    let scratch = ScratchDir::new("maintenance-verify-absent");
    let (engine, clock) = scheduled(scratch.path(), MONDAY);
    advance(&engine, &clock, SETTLE);
    assert_eq!(verdicts(&engine), [Some("intact".to_owned())]);
    let backups = scratch.path().join("backups");
    std::fs::rename(&backups, scratch.path().join("backups.away")).unwrap();

    advance(&engine, &clock, VERIFY_PERIOD);

    let end = verified(&engine);
    assert_eq!(
        end.outcome,
        MaintenanceOutcome::Complete,
        "{:?}",
        end.detail
    );
    let detail = end.detail.unwrap();
    assert!(
        detail.ends_with("generations: intact 0, damaged 0, missing 0, unreachable 1, failed 0"),
        "{detail}"
    );
    assert_eq!(verdicts(&engine), [Some("intact".to_owned())]);
    assert!(engine.resources().verification_due().unwrap());
    block_on(engine.shutdown()).unwrap();
}

/// With nothing owed a due job records nothing, so a failed run's
/// record stands until the next week brings work.
#[test]
fn a_verification_with_nothing_owed_records_nothing() {
    let scratch = ScratchDir::new("maintenance-verify-idle");
    let (engine, clock) = scheduled(scratch.path(), MONDAY);
    advance(&engine, &clock, SETTLE);
    assert!(!engine.resources().verification_due().unwrap());
    // A failed run, as an unreadable object would leave it.
    let resources = engine.resources();
    let failed_at = resources
        .start_maintenance(MaintenanceJob::Verify)
        .unwrap()
        .as_millis();
    resources
        .finish_maintenance(
            MaintenanceJob::Verify,
            MaintenanceOutcome::Failed,
            Some("an object couldn't be read"),
        )
        .unwrap();

    // Due by its retry, with nothing to read.
    advance(&engine, &clock, VERIFY_RETRY + POLL);
    assert_eq!(started_of(&engine, MaintenanceJob::Verify), failed_at);
    let end = verified(&engine);
    assert_eq!(end.outcome, MaintenanceOutcome::Failed);
    assert_eq!(end.detail.as_deref(), Some("an object couldn't be read"));

    // The next week owes a bucket and the generation's check.
    advance(&engine, &clock, VERIFY_PERIOD);
    assert_eq!(verified(&engine).outcome, MaintenanceOutcome::Complete);
    assert!(started_of(&engine, MaintenanceJob::Verify) > failed_at);
    block_on(engine.shutdown()).unwrap();
}

// --- the weekly integrity check (M1.8h5c) ---

/// The integrity check's recorded end.
fn checked(engine: &Engine) -> MaintenanceFinish {
    run_of(engine, MaintenanceJob::Integrity)
        .unwrap()
        .finished
        .unwrap()
}

/// No long work runs: the open's own index and thread work is done.
fn idle(engine: &Engine) {
    let host = engine.maintenance_host();
    wait(|| !(host.busy)());
}

#[test]
fn the_store_s_integrity_is_checked_weekly() {
    let scratch = ScratchDir::new("maintenance-integrity-weekly");
    let (engine, clock) = scheduled(scratch.path(), MONDAY);
    idle(&engine);

    advance(&engine, &clock, SETTLE);
    let end = checked(&engine);
    assert_eq!(
        end.outcome,
        MaintenanceOutcome::Complete,
        "{:?}",
        end.detail
    );
    assert_eq!(end.detail.as_deref(), Some("ok"));
    let first = started_of(&engine, MaintenanceJob::Integrity);
    assert_eq!(first, MONDAY + ms(SETTLE));

    // Not again before a week since that start.
    advance(&engine, &clock, INTEGRITY_PERIOD - Duration::from_millis(1));
    assert_eq!(started_of(&engine, MaintenanceJob::Integrity), first);
    advance(&engine, &clock, Duration::from_millis(1));
    assert_eq!(
        started_of(&engine, MaintenanceJob::Integrity),
        first + ms(INTEGRITY_PERIOD)
    );
    assert_eq!(checked(&engine).outcome, MaintenanceOutcome::Complete);
    assert_eq!(engine.health().integrity, Integrity::Sound);
    block_on(engine.shutdown()).unwrap();
}

/// A due check waits for long work to end: it records nothing while
/// an import's worker runs, and runs at the next look once it ended.
#[test]
fn a_due_check_waits_while_an_import_runs() {
    let scratch = ScratchDir::new("maintenance-integrity-busy");
    let (engine, clock) = scheduled(scratch.path(), MONDAY);
    idle(&engine);
    // Park the import's worker at its entry until released.
    let (release, held) = mpsc::channel::<()>();
    let held = Mutex::new(held);
    *engine.imports().before_task.lock().unwrap() = Some(Arc::new(move |_| {
        let _ = held.lock().unwrap().recv();
    }));
    let path = scratch.path().join("held.eml");
    std::fs::write(&path, RAW).unwrap();
    block_on(engine.start_import(&engine.new_idempotency_key(), vec![path])).unwrap();

    advance(&engine, &clock, SETTLE);
    // The jobs before it ran; the check left no record.
    assert!(run_of(&engine, MaintenanceJob::Gc).is_some());
    assert!(run_of(&engine, MaintenanceJob::Integrity).is_none());
    advance(&engine, &clock, POLL);
    assert!(run_of(&engine, MaintenanceJob::Integrity).is_none());

    drop(release);
    idle(&engine);
    advance(&engine, &clock, POLL);
    assert_eq!(checked(&engine).outcome, MaintenanceOutcome::Complete);
    block_on(engine.shutdown()).unwrap();
}

/// What counts as long work: an import or export with a worker, a
/// thread rebuild, and the Search indexer off its park.
#[test]
fn the_engine_is_busy_while_long_work_runs() {
    let scratch = ScratchDir::new("maintenance-integrity-host");
    let (engine, _clock) = clocked(scratch.path(), real_fs(), MONDAY);
    let host = engine.maintenance_host();
    wait(|| !(host.busy)());

    // A thread rebuild holds its slot.
    assert!(engine.imports().threads().occupy());
    assert!((host.busy)());
    engine.imports().threads().vacate();
    assert!(!(host.busy)());

    // The indexer, held on its way to its park.
    let (release, held) = mpsc::channel::<()>();
    let (entered, reached) = mpsc::channel();
    let held = Mutex::new(held);
    *engine.indexer().hook.lock().unwrap() = Some(Arc::new(move |event| {
        if event == crate::index::Event::Parked {
            entered.send(()).unwrap();
            let _ = held.lock().unwrap().recv();
        }
    }));
    engine.indexer().wake();
    reached.recv_timeout(Duration::from_secs(60)).unwrap();
    assert!((host.busy)());
    drop(release);
    wait(|| !(host.busy)());

    // An import's worker, parked at its entry.
    let (release, held) = mpsc::channel::<()>();
    let held = Mutex::new(held);
    *engine.imports().before_task.lock().unwrap() = Some(Arc::new(move |_| {
        let _ = held.lock().unwrap().recv();
    }));
    let path = scratch.path().join("held.eml");
    std::fs::write(&path, RAW).unwrap();
    block_on(engine.start_import(&engine.new_idempotency_key(), vec![path])).unwrap();
    assert!((host.busy)());
    drop(release);
    wait(|| !(host.busy)());
    block_on(engine.shutdown()).unwrap();
}

/// The scheduled check finds damage: the store latches, the engine
/// turns read-only and its scheduler ends. Health is announced twice,
/// as the store latches and once the check returned, so a shell that
/// read health at the first event reads the copy's name at the second.
/// The run has a start and no end: a read-only store records nothing.
#[test]
fn a_finding_turns_the_engine_read_only_and_announces_the_copy() {
    let scratch = ScratchDir::new("maintenance-integrity-finding");
    let root: PathBuf = scratch.path().to_owned();
    let (engine, _) = clocked(&root, real_fs(), MONDAY);
    block_on(engine.shutdown()).unwrap();
    damage_an_index(&root.join("state.sqlite"));
    let (engine, clock) = scheduled(&root, MONDAY);
    idle(&engine);
    let announced = engine.windows().health_changes();

    clock.0.fetch_add(ms(SETTLE), Ordering::SeqCst);
    engine.maintenance().wake();
    wait(|| engine.read_only() && !engine.maintenance().running());

    assert_eq!(engine.windows().health_changes(), announced + 2);
    let Integrity::ReadOnly { detail, copy, .. } = engine.health().integrity else {
        panic!("health reports no finding");
    };
    assert!(detail.contains("integrity_probe_pad"), "{detail}");
    let copy = copy.expect("the copy is named once the check returned");
    assert!(copy.join("manifest.json").is_file(), "{}", copy.display());
    let run = run_of(&engine, MaintenanceJob::Integrity).unwrap();
    assert_eq!(run.started_at.as_millis(), MONDAY + ms(SETTLE));
    assert!(run.finished.is_none(), "{:?}", run.finished);
    // The jobs before it ran on the sound store.
    assert_eq!(
        outcome_of(&engine, MaintenanceJob::Gc),
        Some(MaintenanceOutcome::Complete)
    );
    let found = engine.health();
    block_on(engine.shutdown()).unwrap();

    // The latch is durable: the next open is read-only with the same
    // finding and copy, and its schedule starts nothing.
    let (engine, _) = clocked(&root, real_fs(), MONDAY + ms(SETTLE));
    assert!(engine.read_only());
    assert_eq!(engine.health(), found);
    engine.schedule_maintenance();
    assert!(!engine.maintenance().running());
    let stored = run_of(&engine, MaintenanceJob::Integrity).unwrap();
    assert_eq!(stored.started_at, run.started_at);
    assert!(stored.finished.is_none(), "{:?}", stored.finished);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_stop_before_the_check_reads_records_it_cancelled() {
    let scratch = ScratchDir::new("maintenance-integrity-stop");
    let (engine, clock) = scheduled(scratch.path(), MONDAY);
    idle(&engine);
    let jobs = engine.maintenance().clone();
    hook(&engine, move |event| {
        if event == Event::Running(MaintenanceJob::Integrity) {
            jobs.stop();
        }
    });
    clock.0.fetch_add(ms(SETTLE), Ordering::SeqCst);
    engine.maintenance().wake();
    wait(|| outcome_of(&engine, MaintenanceJob::Integrity).is_some());

    let end = checked(&engine);
    assert_eq!(
        end.outcome,
        MaintenanceOutcome::Cancelled,
        "{:?}",
        end.detail
    );
    assert_eq!(engine.health().integrity, Integrity::Sound);
    block_on(engine.shutdown()).unwrap();
}

/// The job itself: a busy engine keeps it from starting, and nothing
/// is announced unless something was found.
#[test]
fn a_busy_engine_starts_no_check() {
    let scratch = ScratchDir::new("maintenance-integrity-deferred");
    let (engine, _clock) = clocked(scratch.path(), real_fs(), MONDAY);
    let host = |busy: bool| Host {
        busy: Box::new(move || busy),
        health_moved: Box::new(|| panic!("nothing was found")),
    };
    let running = || false;

    check_integrity(
        engine.resources(),
        engine.maintenance(),
        &running,
        &host(true),
    );
    assert!(run_of(&engine, MaintenanceJob::Integrity).is_none());
    check_integrity(
        engine.resources(),
        engine.maintenance(),
        &running,
        &host(false),
    );
    assert_eq!(checked(&engine).outcome, MaintenanceOutcome::Complete);
    block_on(engine.shutdown()).unwrap();
}

// --- the pure rules ---

fn run(started_at: i64, outcome: Option<MaintenanceOutcome>) -> MaintenanceRun {
    ran(started_at, started_at + 1, outcome)
}

fn ran(started_at: i64, ended_at: i64, outcome: Option<MaintenanceOutcome>) -> MaintenanceRun {
    MaintenanceRun {
        job: MaintenanceJob::Backup,
        started_at: Timestamp::from_millis(started_at),
        finished: outcome.map(|outcome| MaintenanceFinish {
            at: Timestamp::from_millis(ended_at),
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
    // A retry counts from the run's end: one that failed after half an
    // hour of work doesn't follow itself at once.
    for retried in [MaintenanceOutcome::Blocked, MaintenanceOutcome::Failed] {
        let latest = Some(ran(
            10 * hour - 30 * minute,
            10 * hour - minute,
            Some(retried),
        ));
        assert_eq!(due(latest), at(10 * hour + 4 * minute), "{retried:?}");
        let ahead = Some(ran(40 * hour, 40 * hour + 1, Some(retried)));
        assert_eq!(due(ahead), at(10 * hour + 5 * minute), "{retried:?} ahead");
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

#[test]
fn an_integrity_check_maps_to_one_outcome_or_none() {
    assert_eq!(
        integrity_outcome(Ok(IntegrityCheck::Clean)),
        Some((MaintenanceOutcome::Complete, "ok".to_owned()))
    );
    assert_eq!(
        integrity_outcome(Ok(IntegrityCheck::Stopped)),
        Some((MaintenanceOutcome::Cancelled, "stopped".to_owned()))
    );
    // A check that couldn't run says nothing about the store.
    let (outcome, detail) = integrity_outcome(Err(StoreError::Unavailable {
        reason: "cannot open state.sqlite: disk I/O error".into(),
        io_kind: None,
    }))
    .unwrap();
    assert_eq!(outcome, MaintenanceOutcome::Failed);
    assert!(detail.contains("disk I/O error"), "{detail}");
    // A finding leaves a read-only store, which records nothing.
    let finding = IntegrityFinding {
        detail: "row 3 missing from index messages_by_date".into(),
        latch: Ok(()),
        copy: Ok(PathBuf::from("recovery/integrity-1-00000000")),
    };
    assert_eq!(
        integrity_outcome(Ok(IntegrityCheck::Finding(finding))),
        None
    );
}

#[test]
fn the_generations_are_tallied_until_a_stop_or_a_held_operation() {
    let owed = |count: usize| -> Vec<String> { (1..=count).map(|n| format!("g{n}")).collect() };
    let recorded = |verdict| Ok(GenerationCheck::Recorded(verdict));
    let conflict = || StoreError::Conflict {
        detail: "a backup operation is already running".into(),
    };
    let running = AtomicBool::new(false);

    // Every verdict is counted, an error on one doesn't keep the next
    // from its check, and a generation retention removed counts nowhere.
    let mut asked = Vec::new();
    let checks = tally_generations(Ok(owed(7)), &running, |generation| {
        asked.push(generation.to_owned());
        match generation {
            "g1" | "g7" => recorded(GenerationVerdict::Intact),
            "g2" => recorded(GenerationVerdict::Damaged),
            "g3" => recorded(GenerationVerdict::Missing),
            "g4" => Ok(GenerationCheck::DestinationAbsent),
            "g5" => Ok(GenerationCheck::Gone),
            _ => Err(StoreError::internal("input/output error")),
        }
    });
    assert_eq!(asked, owed(7));
    assert_eq!(
        checks,
        GenerationChecks {
            intact: 2,
            damaged: 1,
            missing: 1,
            unreachable: 1,
            failures: vec![format!(
                "generation g6: {}",
                StoreError::internal("input/output error")
            )],
            blocked: None,
            stopped: false,
        }
    );

    // A held operation would refuse each of the rest: they wait.
    let mut asked = 0;
    let checks = tally_generations(Ok(owed(3)), &running, |generation| {
        asked += 1;
        match generation {
            "g1" => recorded(GenerationVerdict::Intact),
            _ => Err(conflict()),
        }
    });
    assert_eq!(asked, 2);
    assert_eq!(checks.intact, 1);
    assert_eq!(checks.blocked, Some(conflict().to_string()));
    assert!(checks.failures.is_empty() && !checks.stopped);

    // A check the stop ended ends the run; so does a stop raised
    // between two generations, before the next is read.
    let mut asked = 0;
    let checks = tally_generations(Ok(owed(3)), &running, |_| {
        asked += 1;
        Ok(GenerationCheck::Stopped)
    });
    assert_eq!((asked, checks.stopped), (1, true));
    let stop = AtomicBool::new(false);
    let mut asked = 0;
    let checks = tally_generations(Ok(owed(3)), &stop, |_| {
        asked += 1;
        stop.store(true, Ordering::SeqCst);
        recorded(GenerationVerdict::Intact)
    });
    assert_eq!((asked, checks.intact, checks.stopped), (1, 1, true));

    // A list that couldn't be read is a failure, and nothing is checked.
    let unread = tally_generations(Err(StoreError::internal("no reader")), &running, |_| {
        panic!("no generation is known")
    });
    assert_eq!(
        unread.failures,
        [StoreError::internal("no reader").to_string()]
    );

    // The sample decides whether they are checked at all.
    assert!(generations_follow(&Ok(SampleReport::default())));
    assert!(generations_follow(&Err(StoreError::internal("no reader"))));
    let stopped = SampleReport {
        stopped: true,
        ..SampleReport::default()
    };
    assert!(!generations_follow(&Ok(stopped)));
}

#[test]
fn a_verification_maps_to_one_outcome() {
    let sample = |edit: fn(&mut SampleReport)| {
        let mut report = SampleReport {
            buckets: vec![7],
            checked: 4,
            intact: 2,
            missing: 1,
            quarantined: 1,
            ..SampleReport::default()
        };
        edit(&mut report);
        Ok(report)
    };
    let checks = |edit: fn(&mut GenerationChecks)| {
        let mut checks = GenerationChecks {
            intact: 3,
            damaged: 1,
            ..GenerationChecks::default()
        };
        edit(&mut checks);
        checks
    };
    const COUNTS: &str = "sample: buckets 1, checked 4, intact 2, missing 1, quarantined 1, \
                          healed 0, gone 0, failed 0; generations: intact 3, damaged 1, \
                          missing 0, unreachable 0, failed 0";

    // Findings are results: the run that made them is complete.
    assert_eq!(
        verify_outcome(sample(|_| {}), &checks(|_| {})),
        (MaintenanceOutcome::Complete, COUNTS.to_owned())
    );
    let (outcome, detail) = verify_outcome(sample(|r| r.stopped = true), &checks(|_| {}));
    assert_eq!(outcome, MaintenanceOutcome::Cancelled);
    assert_eq!(detail, format!("stopped: {COUNTS}"));
    let stopped = verify_outcome(sample(|_| {}), &checks(|c| c.stopped = true));
    assert_eq!(stopped.0, MaintenanceOutcome::Cancelled);

    let held = |c: &mut GenerationChecks| {
        c.blocked = Some("a backup is running".into());
        c.stopped = true;
    };
    let (outcome, detail) = verify_outcome(sample(|_| {}), &checks(held));
    assert_eq!(
        outcome,
        MaintenanceOutcome::Blocked,
        "a gate outranks the stop"
    );
    assert!(detail.ends_with(": a backup is running"), "{detail}");

    // What couldn't be read outranks both.
    let unread = |r: &mut SampleReport| {
        r.failed = 1;
        r.failure_details = vec!["object ab: permission denied".into()];
    };
    let failing = |c: &mut GenerationChecks| {
        c.failures = vec!["generation g: input/output error".into()];
        c.blocked = Some("a backup is running".into());
        c.stopped = true;
    };
    let (outcome, detail) = verify_outcome(sample(unread), &checks(failing));
    assert_eq!(outcome, MaintenanceOutcome::Failed);
    assert!(
        detail.ends_with(": object ab: permission denied; generation g: input/output error"),
        "{detail}"
    );
    assert!(detail.contains("healed 0, gone 0, failed 1; "), "{detail}");
    assert!(detail.contains("unreachable 0, failed 1: "), "{detail}");

    // A sample that returned an error fails the run, whatever the
    // generations' checks came to.
    let (outcome, detail) = verify_outcome(Err(StoreError::internal("no reader")), &checks(|_| {}));
    assert_eq!(outcome, MaintenanceOutcome::Failed);
    assert!(
        detail.starts_with("sample: failed; generations: intact 3, "),
        "{detail}"
    );
}
