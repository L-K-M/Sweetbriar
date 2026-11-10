//! M1.8h's acceptance (plan M1.8h6): the recovery point over simulated
//! hours at spec/03's test setting, crash cuts through a scheduled
//! pass, and a restore during each scheduled job.

use std::env;
use std::pin::pin;
use std::task::{Context, Poll, Waker};

use sb_resources::{BackupReceipt, QualifiedStage, Resources, RestoreMetadata};
use sb_test_support::{CutFs, run_crash_cut_points};

use super::*;

/// spec/03 "Backup generations": "The research's 5 minutes is the test
/// setting."
const TEST_PERIOD: Duration = Duration::from_secs(5 * 60);
const MINUTE: Duration = Duration::from_secs(60);
/// How long a restore or a job's start may take in real time. The
/// swap is disk work, and the suite shares its disk with others.
const PATIENCE: Duration = Duration::from_secs(300);
const CUE: &str = "appearance.compose_cue";
const IN_PROGRESS: &str = "SELECT count(*) FROM backup_generations WHERE state='in_progress'";
const PINS: &str = "SELECT count(*) FROM object_pins WHERE pin_kind='backup_in_progress'";
const MESSAGES: &str = "SELECT count(*) FROM occurrences";

fn now(engine: &Engine) -> i64 {
    engine.resources().now().as_millis()
}

fn sql_error(error: impl std::fmt::Display) -> StoreError {
    StoreError::internal(error.to_string())
}

/// Commit a data write: the cue's other value.
fn toggle(engine: &Engine, nth: usize) {
    let value = if nth.is_multiple_of(2) {
        "strong"
    } else {
        "standard"
    };
    block_on(engine.set_setting(&engine.new_idempotency_key(), CUE, value)).unwrap();
}

/// Every generation as stored: its id, state and directory.
fn generations(engine: &Engine) -> Vec<(String, String, PathBuf)> {
    engine
        .resources()
        .read(|tx| {
            let mut statement = tx
                .prepare(
                    "SELECT generation_id, state, location FROM backup_generations
                     ORDER BY started_at, generation_id",
                )
                .map_err(sql_error)?;
            let rows = statement
                .query_map([], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        PathBuf::from(row.get::<_, String>(2)?),
                    ))
                })
                .map_err(sql_error)?;
            rows.collect::<Result<_, _>>().map_err(sql_error)
        })
        .unwrap()
}

/// Every complete generation's bytes are the ones it published: a
/// generation is never `complete` and anything less. Returns how many
/// there are.
fn assert_complete_generations_intact(engine: &Engine) -> usize {
    let complete: Vec<_> = generations(engine)
        .into_iter()
        .filter(|(_, state, _)| state == "complete")
        .collect();
    for (id, _, location) in &complete {
        let check = engine
            .resources()
            .verify_generation(id, &AtomicBool::new(false))
            .unwrap();
        assert_eq!(
            check,
            GenerationCheck::Recorded(GenerationVerdict::Intact),
            "{}",
            location.display()
        );
    }
    complete.len()
}

// --- the recovery point ---

/// Six simulated hours at the 5-minute setting, a minute at a time,
/// with data writes in seeded minutes and a quiet stretch of over an
/// hour. No committed write is ever as old as the period without a
/// complete generation holding it, and after the quiet stretch the
/// first write is backed up at the next look.
#[test]
fn the_recovery_point_holds_over_simulated_hours() {
    const HOURS: u64 = 6;
    const QUIET: std::ops::Range<u64> = 120..200;
    let scratch = ScratchDir::new("maintenance-acceptance-recovery-point");
    let (engine, clock) = scheduled(scratch.path(), MONDAY);
    *engine.maintenance().backup_period.lock().unwrap() = Some(TEST_PERIOD);
    import(&engine, &scratch.path().join("kept.eml"), RAW);

    let mut seed: u64 = 0x5EED;
    let mut writes = 0;
    // When the oldest write no complete generation holds was committed.
    let mut uncovered_since: Option<i64> = Some(now(&engine));
    let mut oldest = 0;
    let mut waited = 0;
    for minute in 0..HOURS * 60 {
        seed = seed
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        let after_quiet = minute == QUIET.end;
        if after_quiet || (!QUIET.contains(&minute) && (seed >> 60) < 5) {
            toggle(&engine, writes);
            writes += 1;
            uncovered_since.get_or_insert(now(&engine));
        }

        advance(&engine, &clock, MINUTE);

        if !engine.resources().backup_due().unwrap() {
            uncovered_since = None;
        }
        if let Some(since) = uncovered_since {
            let age = now(&engine) - since;
            assert!(
                age < ms(TEST_PERIOD),
                "minute {minute}: a write is {age} ms old with no generation holding it"
            );
            oldest = oldest.max(age);
            waited += 1;
        }
        if after_quiet {
            assert!(
                uncovered_since.is_none(),
                "the first write after a quiet hour waits for no period"
            );
        }
        if QUIET.contains(&minute) && minute >= QUIET.start + 5 {
            assert!(uncovered_since.is_none(), "minute {minute}");
        }
    }

    // The run exercised what it claims: writes that waited for their
    // period, many generations taken and retention keeping one an hour.
    assert!(writes > 60, "{writes} writes");
    assert!(
        waited > 60 && oldest >= ms(TEST_PERIOD) - ms(MINUTE),
        "{waited} looks with a write waiting, the oldest {oldest} ms"
    );
    assert_eq!(count(&engine, FAILED), 0);
    assert_eq!(count(&engine, IN_PROGRESS), 0);
    assert_eq!(count(&engine, PINS), 0);
    let kept = assert_complete_generations_intact(&engine);
    assert!(
        (HOURS as usize..=HOURS as usize + 2).contains(&kept),
        "{kept} generations"
    );
    for job in [MaintenanceJob::Backup, MaintenanceJob::Gc] {
        assert_eq!(outcome_of(&engine, job), Some(MaintenanceOutcome::Complete));
    }
    block_on(engine.shutdown()).unwrap();
}

// --- crash cuts through a scheduled pass ---

const CUT_N_ENV: &str = "SB_MAINTENANCE_CUT_N";
const CUT_ROOT_ENV: &str = "SB_MAINTENANCE_CUT_ROOT";
const CUT_POINTS: usize = 24;

/// An engine on `fs` holding one imported message, its scheduler
/// started and everything else at rest.
fn ready_for_a_pass(root: &Path, fs: Arc<dyn sb_resources::Fs>) -> (Engine, Arc<TestClock>) {
    let (engine, clock) = clocked(root, fs, MONDAY);
    import(&engine, &root.parent().unwrap().join("kept.eml"), RAW);
    idle(&engine);
    engine.schedule_maintenance();
    parked(&engine);
    (engine, clock)
}

/// `CUT_POINTS` seeded points in `first..=last`, ascending.
fn cut_points(first: u64, last: u64) -> Vec<u64> {
    assert!(first <= last, "the pass made no filesystem operation");
    let span = last - first + 1;
    assert!(span >= CUT_POINTS as u64, "only {span} operations to cut");
    let mut state: u64 = 0x0018_C0FF_EE00;
    let mut points = std::collections::BTreeSet::new();
    while points.len() < CUT_POINTS {
        state = state
            .wrapping_mul(6_364_136_223_846_793_005)
            .wrapping_add(1_442_695_040_888_963_407);
        points.insert(first + (state >> 33) % span);
    }
    points.into_iter().collect()
}

/// A process killed at seeded points of the scheduler's first pass (the
/// backup, GC, the verification and the integrity check) never leaves
/// a generation `complete` and less than intact. The next open's
/// schedule fails the generation the cut left in progress, removes its
/// pins and its directory, and takes a complete one.
#[test]
fn crash_cuts_through_a_scheduled_pass_never_complete_a_generation() {
    let scratch = ScratchDir::new("maintenance-acceptance-cuts");

    // An uncut run measures the pass's operations, after the open and
    // the import.
    let uncut = Arc::new(CutFs::new(real_fs(), u64::MAX));
    let root = scratch.path().join("uncut").join("store");
    std::fs::create_dir_all(root.parent().unwrap()).unwrap();
    let (engine, clock) = ready_for_a_pass(&root, uncut.clone());
    let first = uncut.operations() + 1;
    advance(&engine, &clock, SETTLE);
    let last = uncut.operations();
    assert_eq!(count(&engine, COMPLETE), 1);
    block_on(engine.shutdown()).unwrap();
    let points = cut_points(first, last);

    let cuts = run_crash_cut_points(
        CUT_N_ENV,
        CUT_ROOT_ENV,
        "maintenance::tests::acceptance::scheduled_pass_crash_child",
        &points,
        &mut |n| {
            let root = scratch.path().join(format!("cut-{n}")).join("store");
            std::fs::create_dir_all(root.parent().unwrap()).unwrap();
            root
        },
        &mut |root| {
            // An hour on, so the reopened store's schedule owes a backup
            // whatever the cut run recorded.
            let later = MONDAY + ms(SETTLE) + ms(BACKUP_PERIOD);
            let (engine, clock) = clocked(root, real_fs(), later);
            assert_eq!(count(&engine, MESSAGES), 1, "{}", root.display());
            assert_complete_generations_intact(&engine);

            idle(&engine);
            engine.schedule_maintenance();
            parked(&engine);
            advance(&engine, &clock, SETTLE);

            assert_eq!(count(&engine, IN_PROGRESS), 0, "{}", root.display());
            assert_eq!(count(&engine, PINS), 0, "{}", root.display());
            for (id, state, location) in generations(&engine) {
                if state == "failed" {
                    assert!(
                        !location.exists(),
                        "failed {id} kept {}",
                        location.display()
                    );
                }
            }
            assert!(assert_complete_generations_intact(&engine) >= 1);
            assert!(!engine.resources().backup_due().unwrap());
            assert_eq!(count(&engine, MESSAGES), 1);
            block_on(engine.shutdown()).unwrap();
        },
    );
    // The pass makes the same operations each run, so the points land.
    assert!(
        cuts.killed.len() >= CUT_POINTS - 2,
        "children outlived cut points {:?}",
        cuts.survived
    );
}

/// The crash-cut child: the scheduler's first pass on `CutFs`, killed
/// at the cut point.
#[test]
#[ignore = "run by the crash-cut suite as its child"]
fn scheduled_pass_crash_child() {
    let n: u64 = env::var(CUT_N_ENV).unwrap().parse().unwrap();
    let root = PathBuf::from(env::var(CUT_ROOT_ENV).unwrap());
    let (engine, clock) = ready_for_a_pass(&root, Arc::new(CutFs::new(real_fs(), n)));
    advance(&engine, &clock, SETTLE);
    block_on(engine.shutdown()).unwrap();
}

// --- a restore during each job ---

/// Poll `future` until it finishes or `PATIENCE` runs out.
fn finish<F: std::future::Future>(future: F) -> F::Output {
    let mut future = pin!(future);
    let mut cx = Context::from_waker(Waker::noop());
    let deadline = Instant::now() + PATIENCE;
    loop {
        if let Poll::Ready(output) = future.as_mut().poll(&mut cx) {
            return output;
        }
        assert!(Instant::now() < deadline, "the future never finished");
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn metadata(engine: &Engine, generation: &str) -> RestoreMetadata {
    let generation = generation.to_owned();
    engine
        .resources()
        .read(move |tx| {
            tx.query_row(
                "SELECT manifest_sha256, object_count, total_bytes, completed_at
                 FROM backup_generations WHERE generation_id = ?1",
                [&generation],
                |row| {
                    let hex: String = row.get(0)?;
                    let mut manifest_sha256 = [0u8; 32];
                    for (byte, pair) in manifest_sha256.iter_mut().zip(hex.as_bytes().chunks(2)) {
                        *byte = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap();
                    }
                    Ok(RestoreMetadata {
                        manifest_sha256,
                        object_count: row.get(1)?,
                        total_bytes: row.get(2)?,
                        completed_at: row.get(3)?,
                    })
                },
            )
            .map_err(sql_error)
        })
        .unwrap()
}

/// A generation of the store as it is now, verified, staged and
/// qualified: ready to swap in.
fn staged(engine: &Engine) -> QualifiedStage {
    let resources = engine.resources();
    let snapshot = resources
        .backup_snapshot(BackupDestination::DataRoot)
        .unwrap();
    let receipt: BackupReceipt = resources.complete_backup(snapshot).unwrap();
    let cancel = AtomicBool::new(false);
    let location = receipt.location();
    let selection = resources
        .verify_external_backup(location.parent().unwrap(), receipt.generation_id(), &cancel)
        .unwrap()
        .expect("not cancelled");
    let prepared = resources
        .stage_restore(
            selection,
            location,
            metadata(engine, receipt.generation_id()),
            &cancel,
        )
        .unwrap();
    resources
        .qualify_restore(prepared, &Arc::new(AtomicBool::new(false)))
        .unwrap()
}

/// How `job`'s latest run ended in the root a swap preserved: its
/// outcome and its detail.
fn preserved_end(root: &Path, job: &'static str) -> (Option<String>, Option<String>) {
    let mut preserved: Vec<PathBuf> = std::fs::read_dir(root.parent().unwrap())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            let name = path.file_name().unwrap().to_str().unwrap();
            path.is_dir() && name.starts_with("store.preserved-")
        })
        .collect();
    assert_eq!(preserved.len(), 1, "one preserved root: {preserved:?}");
    let resources = Resources::open(&preserved.remove(0)).unwrap();
    let end = resources
        .read(move |tx| {
            tx.query_row(
                "SELECT outcome, detail FROM maintenance_jobs WHERE job = ?1",
                [job],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .map_err(sql_error)
        })
        .unwrap();
    resources.close().unwrap();
    end
}

/// A restore started while `job` runs: the quiesce stops the job, which
/// records `cancelled` in the store being replaced, with a detail that
/// starts with `stopped_early` (the stop ended it before any of its
/// work); the swap goes through; and the schedule runs again on the
/// restored store, where the job completes.
fn a_restore_during(job: MaintenanceJob, stored: &'static str, stopped_early: &str) {
    let scratch = ScratchDir::new(&format!("maintenance-acceptance-restore-{stored}"));
    let root = std::fs::canonicalize(scratch.path()).unwrap().join("store");
    let (engine, clock) = scheduled(&root, MONDAY);
    idle(&engine);
    toggle(&engine, 0);
    let stage = staged(&engine);
    // A write the generation doesn't hold, so the swap shows.
    toggle(&engine, 1);

    // Hold the job at its start until the quiesce stops the schedule.
    // Once: the hook outlives the swap, and the restored store's run of
    // the job must go through.
    let (entered, reached) = mpsc::channel();
    let entered = Mutex::new(entered);
    let jobs = engine.maintenance().clone();
    let held = AtomicBool::new(false);
    hook(&engine, move |event| {
        if event == Event::Running(job) && !held.swap(true, Ordering::SeqCst) {
            entered.lock().unwrap().send(()).unwrap();
            while !jobs.stopped() {
                std::thread::yield_now();
            }
        }
    });
    clock.0.fetch_add(ms(SETTLE), Ordering::SeqCst);
    engine.maintenance().wake();
    reached.recv_timeout(PATIENCE).unwrap();

    let engine = finish(engine.restore(stage, Arc::new(AtomicBool::new(false))))
        .unwrap_or_else(|error| panic!("the restore during {stored} failed: {error:?}"));

    let (outcome, detail) = preserved_end(&root, stored);
    assert_eq!(outcome.as_deref(), Some("cancelled"), "{stored}");
    let detail = detail.unwrap_or_default();
    assert!(detail.starts_with(stopped_early), "{stored}: {detail}");
    let cue = block_on(engine.settings())
        .unwrap()
        .iter()
        .find(|setting| setting.key() == CUE)
        .unwrap()
        .value()
        .to_owned();
    assert_eq!(cue, "strong", "the generation's value is back");

    // The schedule runs on the restored store.
    assert!(engine.maintenance().running());
    idle(&engine);
    parked(&engine);
    toggle(&engine, 1);
    advance(&engine, &clock, SETTLE);
    assert_eq!(
        outcome_of(&engine, job),
        Some(MaintenanceOutcome::Complete),
        "{stored} on the restored store"
    );
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_restore_during_the_backup_swaps_and_the_schedule_goes_on() {
    a_restore_during(
        MaintenanceJob::Backup,
        "backup",
        "stopped before the snapshot",
    );
}

#[test]
fn a_restore_during_gc_swaps_and_the_schedule_goes_on() {
    a_restore_during(MaintenanceJob::Gc, "gc", "stopped: scanned 0, ");
}

#[test]
fn a_restore_during_the_verification_swaps_and_the_schedule_goes_on() {
    a_restore_during(
        MaintenanceJob::Verify,
        "verify",
        "stopped: sample: buckets 0, ",
    );
}

#[test]
fn a_restore_during_the_integrity_check_swaps_and_the_schedule_goes_on() {
    a_restore_during(MaintenanceJob::Integrity, "integrity", "stopped");
}
