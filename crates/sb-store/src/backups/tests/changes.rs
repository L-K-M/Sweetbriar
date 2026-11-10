//! M1.8h1: "a generation is taken every hour while there were changes"
//! (spec/03 "Backup generations"). A committed write that changed a row
//! makes the store due; a generation's snapshot clears it; the store's
//! own bookkeeping (backups, retention, GC, reservations, close) never
//! makes it due by itself.

use super::*;
use crate::changes::{BACKED_UP_CHANGES, CONTENT_CHANGES};
use crate::{Seams, WritePriority};

/// Both counters, read through the pool.
fn counters(store: &Store) -> (u64, u64) {
    reading(store, |tx| {
        Ok((
            crate::changes::read(tx, CONTENT_CHANGES)?,
            crate::changes::read(tx, BACKED_UP_CHANGES)?,
        ))
    })
}

fn due(store: &Store) -> bool {
    store.backup_due().unwrap()
}

/// One complete generation in the data root.
fn back_up(store: &Store) {
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    store.complete_backup(snapshot).unwrap();
}

/// One archive change: a row in a fixture table.
fn change(store: &Store, seq: i64) {
    sql(
        store,
        format!(
            "CREATE TABLE IF NOT EXISTS seqs (seq INTEGER PRIMARY KEY, pad TEXT);
             INSERT INTO seqs (seq, pad) VALUES ({seq}, 'x')"
        ),
    );
}

/// Edit `state.sqlite` with the store closed, as damage or an older
/// version would leave it.
fn edit_closed(paths: &StorePaths, statement: &str) {
    let conn = Connection::open(paths.state_db()).unwrap();
    conn.execute_batch(statement).unwrap();
}

#[test]
fn a_fresh_store_is_due_until_its_first_generation() {
    let (_scratch, _paths, store) = open("changes-fresh");
    assert_eq!(counters(&store), (1, 0));
    assert!(due(&store), "no generation yet");

    back_up(&store);
    // The generation holds the snapshot's counter; its own rows,
    // pins and retention counted nothing.
    assert_eq!(counters(&store), (1, 1));
    assert!(!due(&store));
}

#[test]
fn each_change_makes_the_store_due_and_a_generation_clears_it() {
    let (_scratch, _paths, store) = open("changes-cycle");
    back_up(&store);

    change(&store, 1);
    assert_eq!(counters(&store), (2, 1));
    assert!(due(&store));
    // Two changes before the next generation are still one due store.
    change(&store, 2);
    assert_eq!(counters(&store).0, 3);

    back_up(&store);
    assert_eq!(counters(&store), (3, 3));
    assert!(!due(&store));
}

#[test]
fn a_write_that_changes_nothing_or_rolls_back_is_no_change() {
    let (_scratch, _paths, store) = open("changes-none");
    change(&store, 1);
    back_up(&store);
    let before = counters(&store);

    // A transaction that matched no row.
    sql(&store, "UPDATE seqs SET pad = 'y' WHERE seq = 99".into());
    // One that wrote, then failed: rolled back with its bump.
    let failed = block_on(store.write(WritePriority::User, |tx| {
        tx.execute("INSERT INTO seqs (seq, pad) VALUES (7, 'x')", [])
            .map_err(bug)?;
        Err::<(), _>(StoreError::conflict("the test refuses"))
    }));
    assert!(matches!(failed, Err(StoreError::Conflict { .. })));
    // A read-only closure on the writer.
    writing(&store, |tx| {
        tx.query_row("SELECT count(*) FROM seqs", [], |row| row.get::<_, i64>(0))
            .map_err(bug)
    });

    assert_eq!(counters(&store), before);
    assert!(!due(&store));
}

/// A clock the bookkeeping test moves past GC's grace period.
struct MovableClock(std::sync::atomic::AtomicI64);

impl crate::Clock for MovableClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0.load(std::sync::atomic::Ordering::SeqCst))
    }
}

#[test]
fn the_store_s_own_bookkeeping_never_makes_it_due() {
    let (_scratch, paths) = scratch_paths("changes-bookkeeping");
    let clock = Arc::new(MovableClock(FIXED_NOW_MS.into()));
    let seams = Seams::new(real_fs(), clock.clone(), Arc::new(SeededRandom::new(7)));
    let store = crate::open(&paths, &seams).unwrap();
    // An orphan: published, then its reservation released by a write
    // that does count, before the generation.
    let _orphan = object(&store, PAYLOAD);
    back_up(&store);
    let before = counters(&store);

    // A second generation: its row, ownership, pins, completion and
    // retention.
    back_up(&store);
    // GC: the orphan's grace pin, then, past the grace period, its
    // removal.
    let report = store.collect_garbage(GC_GRACE_PERIOD).unwrap();
    assert_eq!(report.grace_started, 1, "{report:?}");
    let grace = i64::try_from(GC_GRACE_PERIOD.as_millis()).unwrap();
    clock.0.store(
        FIXED_NOW_MS + grace + 1,
        std::sync::atomic::Ordering::SeqCst,
    );
    let report = store.collect_garbage(GC_GRACE_PERIOD).unwrap();
    assert_eq!(report.removed, 1, "{report:?}");
    // A publication's reservation alone.
    let _reserved = store
        .objects()
        .publish(&b"reserved, never referenced"[..])
        .unwrap();
    // Close (the clean-shutdown marker) and reopen.
    store.close().unwrap();
    let store = crate::open(&paths, &seams).unwrap();

    assert_eq!(counters(&store).0, before.0);
    assert!(!due(&store));
}

#[test]
fn a_commit_after_the_snapshot_keeps_the_store_due() {
    let (_scratch, _paths, store) = open("changes-after-snapshot");
    back_up(&store);
    let store = Arc::new(store);

    // Park the snapshot once it is written, before its references are
    // read; a change commits there, after the snapshot point.
    let gate = Arc::new(Barrier::new(2));
    store.backups.set_pauses(Pauses {
        before_referenced_read: Some(gate.clone()),
        ..Pauses::default()
    });
    let background = {
        let store = Arc::clone(&store);
        std::thread::spawn(move || store.backup_snapshot(BackupDestination::DataRoot))
    };
    gate.wait();
    change(&store, 1);
    gate.wait();
    let snapshot = background.join().unwrap().unwrap();
    store.backups.set_pauses(Pauses::default());
    store.complete_backup(snapshot).unwrap();

    // The generation holds the counter from before the change.
    assert_eq!(counters(&store), (2, 1));
    assert!(due(&store));
}

#[test]
fn a_commit_during_the_copy_is_in_the_generation() {
    let (_scratch, _paths, store) = open("changes-mid-copy");
    seed_rows(&store, 1, 2_048);
    back_up(&store);
    let store = Arc::new(store);

    // The online backup restarts over a source changed between steps,
    // so a mid-copy commit lands in the snapshot, its bump included.
    let gate = Arc::new(Barrier::new(2));
    store.backups.set_pauses(Pauses {
        after_first_step: Some(gate.clone()),
        ..Pauses::default()
    });
    let background = {
        let store = Arc::clone(&store);
        std::thread::spawn(move || store.backup_snapshot(BackupDestination::DataRoot))
    };
    gate.wait();
    seed_rows(&store, 2_049, 2_048);
    gate.wait();
    let snapshot = background.join().unwrap().unwrap();
    store.backups.set_pauses(Pauses::default());
    store.complete_backup(snapshot).unwrap();

    let (changes, backed_up) = counters(&store);
    assert_eq!(changes, backed_up);
    assert!(!due(&store));
}

#[test]
fn the_counters_survive_a_reopen() {
    let (_scratch, paths, store) = open("changes-reopen");
    back_up(&store);
    change(&store, 1);
    store.close().unwrap();

    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    assert!(due(&store), "the change outlives the session");
    back_up(&store);
    store.close().unwrap();

    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    assert!(!due(&store));
}

#[test]
fn a_store_migrating_in_starts_due() {
    let (_scratch, paths, store) = open("changes-migrate");
    back_up(&store);
    store.close().unwrap();
    // As a version-24 store holding a complete generation.
    edit_closed(
        &paths,
        "DELETE FROM store_meta WHERE key IN ('content_changes', 'backed_up_changes');
         DROP TABLE maintenance_jobs;
         PRAGMA user_version = 24;",
    );

    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    assert_eq!(counters(&store), (1, 0));
    assert!(due(&store), "changes since its last generation are unknown");
}

#[test]
fn a_missing_malformed_or_exhausted_counter_is_corrupt() {
    for (label, statement, message) in [
        (
            "changes-missing",
            "DELETE FROM store_meta WHERE key = 'content_changes'",
            "missing or malformed",
        ),
        (
            "changes-malformed",
            "UPDATE store_meta SET value = '-1' WHERE key = 'content_changes'",
            "missing or malformed",
        ),
        (
            "changes-exhausted",
            "UPDATE store_meta SET value = '9223372036854775807' WHERE key = 'content_changes'",
            "exhausted",
        ),
    ] {
        let (_scratch, paths, store) = open(label);
        change(&store, 1);
        back_up(&store);
        store.close().unwrap();
        edit_closed(&paths, statement);
        let store = crate::open(&paths, &test_seams(real_fs())).unwrap();

        // A change can't commit without its bump: it rolls back.
        let write = block_on(store.write(WritePriority::User, |tx| {
            tx.execute("INSERT INTO seqs (seq, pad) VALUES (2, 'x')", [])
                .map_err(bug)
        }));
        match write {
            Err(StoreError::Corrupt { detail: found }) => {
                assert!(found.contains(message), "{label}: {found}");
            }
            other => panic!("{label}: expected Corrupt, got {other:?}"),
        }
        let rows: i64 = reading(&store, |tx| {
            tx.query_row("SELECT count(*) FROM seqs", [], |row| row.get(0))
                .map_err(bug)
        });
        assert_eq!(rows, 1, "{label}: the change rolled back");
        if message != "exhausted" {
            assert!(
                matches!(store.backup_due(), Err(StoreError::Corrupt { .. })),
                "{label}"
            );
        }
    }

    // The generation's own counter: due can't be answered, and no
    // generation can record itself.
    let (_scratch, paths, store) = open("changes-backed-up-malformed");
    back_up(&store);
    store.close().unwrap();
    edit_closed(
        &paths,
        "UPDATE store_meta SET value = 'x' WHERE key = 'backed_up_changes'",
    );
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    assert!(matches!(
        store.backup_due(),
        Err(StoreError::Corrupt { .. })
    ));
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    assert!(matches!(
        store.complete_backup(snapshot),
        Err(StoreError::Corrupt { .. })
    ));
}

#[test]
fn a_store_migrated_part_way_counts_nothing() {
    // A test-only migration list that stops before version 25: the
    // writer has no counter to bump, and writes still commit.
    let (_scratch, paths) = scratch_paths("changes-part-way");
    let seams: Seams = test_seams(real_fs());
    let store = crate::store::open_with(
        &paths,
        &seams,
        crate::store::WRITER_PRAGMAS,
        crate::store::READER_PRAGMAS,
        &crate::store::PINNED_SQLITE,
        &crate::schema::MIGRATIONS[..24],
    )
    .unwrap();
    change(&store, 1);
    let keys: i64 = reading(&store, |tx| {
        tx.query_row(
            "SELECT count(*) FROM store_meta WHERE key IN ('content_changes', 'backed_up_changes')",
            [],
            |row| row.get(0),
        )
        .map_err(bug)
    });
    assert_eq!(keys, 0);
}
