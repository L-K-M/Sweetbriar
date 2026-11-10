//! M1.8h3: each maintenance job's latest run, its end and its last
//! success, written as bookkeeping (spec/03 "Scheduled maintenance").

use std::sync::Arc;
use std::sync::atomic::{AtomicI64, Ordering};

use sb_test_support::{SeededRandom, real_fs};

use super::{
    MAINTENANCE_DETAIL_MAX, MaintenanceFinish, MaintenanceJob, MaintenanceOutcome, MaintenanceRun,
};
use crate::changes::{BACKED_UP_CHANGES, CONTENT_CHANGES};
use crate::schema::bug;
use crate::testing::{FIXED_NOW_MS, scratch_paths, writing};
use crate::{Clock, Seams, Store, StoreError, Timestamp, WritePriority};

/// A clock each step moves on.
struct TestClock(AtomicI64);

impl Clock for TestClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0.load(Ordering::SeqCst))
    }
}

fn open(label: &str) -> (sb_test_support::ScratchDir, Store, Arc<TestClock>) {
    let (scratch, paths) = scratch_paths(label);
    let clock = Arc::new(TestClock(AtomicI64::new(FIXED_NOW_MS)));
    let seams = Seams::new(real_fs(), clock.clone(), Arc::new(SeededRandom::new(7)));
    (scratch, crate::open(&paths, &seams).unwrap(), clock)
}

fn at(ms: i64) -> Timestamp {
    Timestamp::from_millis(ms)
}

fn run(store: &Store, job: MaintenanceJob) -> Option<MaintenanceRun> {
    store
        .maintenance_runs()
        .unwrap()
        .into_iter()
        .find(|run| run.job == job)
}

fn counters(store: &Store) -> (u64, u64) {
    store
        .read(|tx| {
            Ok((
                crate::changes::read(tx, CONTENT_CHANGES)?,
                crate::changes::read(tx, BACKED_UP_CHANGES)?,
            ))
        })
        .unwrap()
}

#[test]
fn a_run_records_its_start_end_and_last_success() {
    let (_scratch, store, clock) = open("maintenance-round-trip");
    assert_eq!(store.maintenance_runs().unwrap(), []);

    assert_eq!(
        store.start_maintenance(MaintenanceJob::Gc).unwrap(),
        at(FIXED_NOW_MS)
    );
    let in_flight = MaintenanceRun {
        job: MaintenanceJob::Gc,
        started_at: at(FIXED_NOW_MS),
        finished: None,
        succeeded_at: None,
    };
    assert_eq!(run(&store, MaintenanceJob::Gc), Some(in_flight));

    clock.0.store(FIXED_NOW_MS + 5, Ordering::SeqCst);
    store
        .finish_maintenance(
            MaintenanceJob::Gc,
            MaintenanceOutcome::Complete,
            Some("removed 2 of 9"),
        )
        .unwrap();
    let complete = MaintenanceFinish {
        at: at(FIXED_NOW_MS + 5),
        outcome: MaintenanceOutcome::Complete,
        detail: Some("removed 2 of 9".to_owned()),
    };
    assert_eq!(
        run(&store, MaintenanceJob::Gc),
        Some(MaintenanceRun {
            job: MaintenanceJob::Gc,
            started_at: at(FIXED_NOW_MS),
            finished: Some(complete),
            succeeded_at: Some(at(FIXED_NOW_MS + 5)),
        })
    );

    // The next run replaces the end; the last success stays through a
    // failure.
    clock.0.store(FIXED_NOW_MS + 10, Ordering::SeqCst);
    store.start_maintenance(MaintenanceJob::Gc).unwrap();
    let restarted = run(&store, MaintenanceJob::Gc).unwrap();
    assert_eq!(restarted.finished, None);
    assert_eq!(restarted.succeeded_at, Some(at(FIXED_NOW_MS + 5)));

    clock.0.store(FIXED_NOW_MS + 15, Ordering::SeqCst);
    store
        .finish_maintenance(MaintenanceJob::Gc, MaintenanceOutcome::Failed, Some("disk"))
        .unwrap();
    let failed = run(&store, MaintenanceJob::Gc).unwrap();
    assert_eq!(
        failed.finished.map(|end| (end.at, end.outcome)),
        Some((at(FIXED_NOW_MS + 15), MaintenanceOutcome::Failed))
    );
    assert_eq!(failed.succeeded_at, Some(at(FIXED_NOW_MS + 5)));
}

#[test]
fn every_outcome_round_trips_and_runs_come_in_job_order() {
    let (_scratch, store, _clock) = open("maintenance-outcomes");
    let ends = [
        (MaintenanceJob::Integrity, MaintenanceOutcome::Cancelled),
        (MaintenanceJob::Verify, MaintenanceOutcome::Failed),
        (MaintenanceJob::Gc, MaintenanceOutcome::Blocked),
        (MaintenanceJob::Backup, MaintenanceOutcome::Complete),
    ];
    for (job, outcome) in ends {
        store.start_maintenance(job).unwrap();
        store.finish_maintenance(job, outcome, None).unwrap();
    }

    let runs = store.maintenance_runs().unwrap();
    let read: Vec<_> = runs
        .iter()
        .map(|run| (run.job, run.finished.as_ref().unwrap().outcome))
        .collect();
    let mut expected = ends.to_vec();
    expected.reverse();
    assert_eq!(read, expected);
    for run in &runs {
        let complete = run.finished.as_ref().unwrap().outcome == MaintenanceOutcome::Complete;
        assert_eq!(run.succeeded_at.is_some(), complete, "{run:?}");
    }
}

#[test]
fn finishing_without_a_run_in_flight_is_a_conflict() {
    let (_scratch, store, _clock) = open("maintenance-conflict");
    let never =
        store.finish_maintenance(MaintenanceJob::Backup, MaintenanceOutcome::Complete, None);
    assert!(
        matches!(never, Err(StoreError::Conflict { .. })),
        "{never:?}"
    );
    assert_eq!(store.maintenance_runs().unwrap(), []);

    store.start_maintenance(MaintenanceJob::Backup).unwrap();
    store
        .finish_maintenance(MaintenanceJob::Backup, MaintenanceOutcome::Cancelled, None)
        .unwrap();
    let before = store.maintenance_runs().unwrap();
    let again = store.finish_maintenance(
        MaintenanceJob::Backup,
        MaintenanceOutcome::Complete,
        Some("late"),
    );
    assert!(
        matches!(again, Err(StoreError::Conflict { .. })),
        "{again:?}"
    );
    assert_eq!(store.maintenance_runs().unwrap(), before, "nothing changed");
}

#[test]
fn a_long_detail_is_cut_at_a_character_boundary() {
    let (_scratch, store, _clock) = open("maintenance-detail");
    // Three-byte characters: the limit falls inside one.
    let long = "€".repeat(MAINTENANCE_DETAIL_MAX);
    store.start_maintenance(MaintenanceJob::Verify).unwrap();
    store
        .finish_maintenance(
            MaintenanceJob::Verify,
            MaintenanceOutcome::Failed,
            Some(&long),
        )
        .unwrap();

    let detail = run(&store, MaintenanceJob::Verify)
        .unwrap()
        .finished
        .unwrap()
        .detail
        .unwrap();
    assert_eq!(detail.len(), MAINTENANCE_DETAIL_MAX / 3 * 3);
    assert!(long.starts_with(&detail));
}

/// Recording runs is the store's bookkeeping: a backed-up store stays
/// not due through a whole run.
#[test]
fn recording_runs_never_makes_a_backup_due() {
    let (_scratch, store, _clock) = open("maintenance-bookkeeping");
    let snapshot = store
        .backup_snapshot(crate::BackupDestination::DataRoot)
        .unwrap();
    store.complete_backup(snapshot).unwrap();
    assert!(!store.backup_due().unwrap());
    let before = counters(&store);

    for job in [MaintenanceJob::Backup, MaintenanceJob::Gc] {
        store.start_maintenance(job).unwrap();
        store
            .finish_maintenance(job, MaintenanceOutcome::Complete, Some("done"))
            .unwrap();
    }

    assert_eq!(counters(&store), before);
    assert!(!store.backup_due().unwrap());
}

/// The table's checks keep every row the reader can read: an unknown
/// job or outcome, an end without an outcome, a detail without an end
/// or past the limit, a complete end that isn't the last success are
/// refused.
#[test]
fn the_schema_refuses_malformed_rows() {
    let (_scratch, store, _clock) = open("maintenance-checks");
    let long = "x".repeat(MAINTENANCE_DETAIL_MAX + 1);
    for statement in [
        "INSERT INTO maintenance_jobs (job, started_at) VALUES ('vacuum', 1)".to_owned(),
        "INSERT INTO maintenance_jobs (job, started_at, finished_at, outcome)
         VALUES ('gc', 1, 2, 'skipped')"
            .to_owned(),
        "INSERT INTO maintenance_jobs (job, started_at, finished_at) VALUES ('gc', 1, 2)"
            .to_owned(),
        "INSERT INTO maintenance_jobs (job, started_at, outcome) VALUES ('gc', 1, 'failed')"
            .to_owned(),
        "INSERT INTO maintenance_jobs (job, started_at, detail) VALUES ('gc', 1, 'early')"
            .to_owned(),
        "INSERT INTO maintenance_jobs (job, started_at, finished_at, outcome)
         VALUES ('gc', 1, 2, 'complete')"
            .to_owned(),
        format!(
            "INSERT INTO maintenance_jobs (job, started_at, finished_at, outcome, detail)
             VALUES ('gc', 1, 2, 'failed', '{long}')"
        ),
    ] {
        let insert = statement.clone();
        let refused =
            crate::store::await_write(store.writer.submit(WritePriority::Index, move |tx| {
                tx.execute_batch(&insert).map_err(bug)
            }));
        assert!(refused.is_err(), "{statement}");
    }
    assert_eq!(store.maintenance_runs().unwrap(), []);
}

/// A row written past the checks reads as `Corrupt`, never as a run:
/// an end without an outcome, a detail on a run in flight, a complete
/// end that isn't the last success.
#[test]
fn a_malformed_row_reads_as_corrupt() {
    for (label, values) in [
        ("maintenance-corrupt-end", "('gc', 1, 2, NULL, NULL, NULL)"),
        (
            "maintenance-corrupt-detail",
            "('gc', 1, NULL, NULL, 'early', NULL)",
        ),
        (
            "maintenance-corrupt-success",
            "('gc', 1, 2, 'complete', NULL, 1)",
        ),
    ] {
        let (_scratch, store, _clock) = open(label);
        let statement = format!(
            "PRAGMA ignore_check_constraints = ON;
             INSERT INTO maintenance_jobs
               (job, started_at, finished_at, outcome, detail, succeeded_at)
             VALUES {values};
             PRAGMA ignore_check_constraints = OFF;"
        );
        writing(&store, move |tx| tx.execute_batch(&statement).map_err(bug));
        let read = store.maintenance_runs();
        assert!(
            matches!(read, Err(StoreError::Corrupt { .. })),
            "{label}: {read:?}"
        );
    }
}

/// Recording awaits the writer: on the writer thread it's refused, never
/// a thread parked on its own queue.
#[test]
fn the_writer_thread_cannot_record_maintenance() {
    let (_scratch, store, _clock) = open("maintenance-own-writer");
    let store = Arc::new(store);
    let on_writer = Arc::clone(&store);
    let (start, finish) = sb_test_support::block_on(store.write(WritePriority::User, move |_tx| {
        Ok((
            on_writer.start_maintenance(MaintenanceJob::Gc).unwrap_err(),
            on_writer
                .finish_maintenance(MaintenanceJob::Gc, MaintenanceOutcome::Complete, None)
                .unwrap_err(),
        ))
    }))
    .unwrap();
    assert!(matches!(start, StoreError::Internal { .. }), "{start:?}");
    assert!(matches!(finish, StoreError::Internal { .. }), "{finish:?}");
    assert_eq!(store.maintenance_runs().unwrap(), []);
    Arc::into_inner(store).unwrap().close().unwrap();
}
