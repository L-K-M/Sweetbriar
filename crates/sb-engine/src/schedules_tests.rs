//! Tests for the scheduled maintenance loop (M1.8h, spec/03): the
//! worker runs each job when its interval elapses, records durable
//! evidence in `settings`, takes a generation only while there were
//! changes, and the quiesce stops it before a restore's swap.

use std::path::Path;
use std::time::{Duration, Instant};

use sb_test_support::{ScratchDir, block_on, real_fs};

use crate::Engine;
use crate::index_tests::{TestClock, clocked};
use crate::schedules::SchedulePeriods;

const PATIENCE: Duration = Duration::from_secs(60);

/// Millisecond periods so every job is due on its first tick.
const INSTANT: SchedulePeriods = SchedulePeriods {
    tick: Duration::from_millis(50),
    backup: Duration::from_millis(1),
    gc: Duration::from_millis(1),
    verify: Duration::from_millis(1),
    integrity: Duration::from_millis(1),
};

fn open(root: &Path, at: i64) -> (Engine, std::sync::Arc<TestClock>) {
    clocked(root, real_fs(), at)
}

#[track_caller]
fn wait_for(mut ready: impl FnMut() -> bool) {
    let end = Instant::now() + PATIENCE;
    while !ready() {
        assert!(Instant::now() < end, "scheduled work did not settle");
        std::thread::sleep(Duration::from_millis(10));
    }
}

fn setting(engine: &Engine, key: &str) -> Option<String> {
    engine
        .resources()
        .read(|tx| {
            sb_resources::read_all(tx).map(|all| {
                all.iter()
                    .find(|s| s.key() == key)
                    .map(|s| s.value_json().to_owned())
            })
        })
        .unwrap()
}

fn generations(engine: &Engine) -> Vec<(String, String)> {
    engine
        .resources()
        .read(|tx| {
            tx.prepare("SELECT generation_id, state FROM backup_generations")
                .map_err(|e| sb_resources::StoreError::internal(format!("{e}")))?
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .map_err(|e| sb_resources::StoreError::internal(format!("{e}")))?
                .collect::<Result<Vec<(String, String)>, _>>()
                .map_err(|e| sb_resources::StoreError::internal(format!("{e}")))
        })
        .unwrap()
}

#[test]
fn the_schedules_record_each_runs_evidence() {
    let scratch = ScratchDir::new("schedules-evidence");
    let root = scratch.path().join("store");
    let (engine, _clock) = open(&root, 1_000_000);
    engine.schedules.set_periods(INSTANT);
    wait_for(|| setting(&engine, "maintenance.integrity.last").is_some());
    wait_for(|| setting(&engine, "maintenance.verify.last").is_some());
    wait_for(|| setting(&engine, "maintenance.gc.last").is_some());
    wait_for(|| setting(&engine, "maintenance.backup.last").is_some());
    let integrity = setting(&engine, "maintenance.integrity.last").unwrap();
    assert!(integrity.contains("\"ok\":true"), "{integrity}");
    // The baseline generation landed: the first run's cursor is
    // missing, so `changed` holds and a complete generation exists.
    assert!(generations(&engine).iter().any(|(_, s)| s == "complete"));
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn an_unchanged_store_takes_no_new_generation() {
    let scratch = ScratchDir::new("schedules-idle");
    let root = scratch.path().join("store");
    let (engine, _clock) = open(&root, 1_000_000);
    engine.schedules.set_periods(INSTANT);
    wait_for(|| generations(&engine).iter().any(|(_, s)| s == "complete"));
    // Further ticks keep checking but take nothing: the cursor's list
    // generation has not moved.
    std::thread::sleep(Duration::from_millis(400));
    assert_eq!(generations(&engine).len(), 1);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn the_worker_stops_for_a_restore() {
    let scratch = ScratchDir::new("schedules-restore");
    let root = scratch.path().join("store");
    let (engine, _clock) = open(&root, 1_000_000);
    engine.schedules.set_periods(INSTANT);
    wait_for(|| generations(&engine).iter().any(|(_, s)| s == "complete"));
    // The quiesce must drain the scheduler before the swap's
    // try_unwrap; a restore that completes proves it did.
    let generation = generations(&engine)[0].0.clone();
    let location = engine
        .resources()
        .read(|tx| {
            tx.query_row(
                "SELECT location FROM backup_generations WHERE generation_id = ?1",
                [&generation],
                |row| row.get::<_, String>(0),
            )
            .map_err(|e| sb_resources::StoreError::internal(format!("{e}")))
        })
        .unwrap();
    block_on(engine.restore_backup(Path::new(&location).parent().unwrap(), &generation))
        .expect("the restore succeeds");
    block_on(engine.shutdown()).unwrap();
}
