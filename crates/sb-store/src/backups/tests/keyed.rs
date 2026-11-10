//! M1.8i2: a generation started by a command (spec/07 `start_backup`)
//! commits the command's receipt with its `in_progress` row, so a
//! repeated key starts nothing; and a generation is owed while an
//! interrupted one waits for the cleanup only the next one does
//! (spec/03 "Backup generations").

use super::*;
use crate::backups::{START_BACKUP, START_RESTORE, StartedBackup};
use crate::receipts::{Receipt, receipt_generation_id, run_once};

const KEY: &str = "00000000-0000-4000-8000-00000000b001";
const OTHER_KEY: &str = "00000000-0000-4000-8000-00000000b002";

/// Every stored receipt: `(key, command, result_json)`, by key.
fn receipts(store: &Store) -> Vec<(String, String, String)> {
    reading(store, |tx| {
        let mut stmt = tx
            .prepare(
                "SELECT idempotency_key, command, result_json FROM command_receipts ORDER BY 1",
            )
            .map_err(bug)?;
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
            .map_err(bug)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(bug)
    })
}

/// Every generation row: `(id, state)`, oldest first.
fn generations(store: &Store) -> Vec<(String, String)> {
    reading(store, |tx| {
        let mut stmt = tx
            .prepare(
                "SELECT generation_id, state FROM backup_generations
                 ORDER BY started_at, rowid",
            )
            .map_err(bug)?;
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(bug)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(bug)
    })
}

fn started(result: Result<StartedBackup, StoreError>) -> (Receipt, super::super::BackupSnapshot) {
    match result {
        Ok(StartedBackup::Started { receipt, snapshot }) => (receipt, snapshot),
        other => panic!("expected a started generation, got {other:?}"),
    }
}

fn replayed(result: Result<StartedBackup, StoreError>) -> Receipt {
    match result {
        Ok(StartedBackup::Replayed(receipt)) => receipt,
        other => panic!("expected a replayed receipt, got {other:?}"),
    }
}

fn due(store: &Store) -> bool {
    store.backup_due().unwrap()
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

#[test]
fn a_key_s_receipt_commits_with_the_generation_s_row() {
    let (_scratch, _paths, store) = open("keyed-start");
    let (receipt, snapshot) = started(store.start_backup(BackupDestination::DataRoot, KEY));
    let id = snapshot.generation_id().to_owned();

    assert_eq!(receipt.command(), START_BACKUP);
    assert_eq!(
        receipt.result_json(),
        format!(r#"{{"generation_id":"{id}"}}"#)
    );
    assert_eq!(receipt.created_at(), Timestamp::from_millis(FIXED_NOW_MS));
    assert_eq!(receipt_generation_id(&receipt).unwrap(), id);
    assert_eq!(
        receipts(&store),
        [(
            KEY.to_owned(),
            START_BACKUP.to_owned(),
            receipt.result_json().to_owned()
        )]
    );
    assert_eq!(
        generations(&store),
        [(id.clone(), "in_progress".to_owned())]
    );

    // The snapshot was taken after the commit: it holds the receipt.
    let copy = Connection::open(snapshot.state_db()).unwrap();
    let stored: String = copy
        .query_row(
            "SELECT result_json FROM command_receipts WHERE idempotency_key = ?1",
            [KEY],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored, receipt.result_json());
    drop(copy);

    let published = store.complete_backup(snapshot).unwrap();
    assert_eq!(published.generation_id(), id);
    assert_eq!(generations(&store), [(id, "complete".to_owned())]);
}

/// The key answers its receipt while its generation is being copied
/// (no `Conflict`), after it completed, and after it was abandoned;
/// each time nothing else happens.
#[test]
fn a_repeated_key_answers_its_receipt_and_starts_nothing() {
    let (_scratch, _paths, store) = open("keyed-replay");
    let (receipt, snapshot) = started(store.start_backup(BackupDestination::DataRoot, KEY));
    let id = snapshot.generation_id().to_owned();

    let again = replayed(store.start_backup(BackupDestination::DataRoot, KEY));
    assert_eq!(again, receipt, "while the copy runs");
    assert_eq!(
        generations(&store),
        [(id.clone(), "in_progress".to_owned())]
    );

    store.complete_backup(snapshot).unwrap();
    let again = replayed(store.start_backup(BackupDestination::DataRoot, KEY));
    assert_eq!(again, receipt, "after it completed");
    assert_eq!(generations(&store), [(id.clone(), "complete".to_owned())]);
    assert_eq!(receipts(&store).len(), 1);

    // An abandoned generation is left exactly as it stands: a replay
    // runs no recovery.
    let (second, snapshot) = started(store.start_backup(BackupDestination::DataRoot, OTHER_KEY));
    let abandoned = snapshot.generation_id().to_owned();
    let location = snapshot.location().to_owned();
    drop(snapshot);
    let again = replayed(store.start_backup(BackupDestination::DataRoot, OTHER_KEY));
    assert_eq!(again, second, "after it was abandoned");
    assert_eq!(
        generations(&store),
        [
            (id, "complete".to_owned()),
            (abandoned, "in_progress".to_owned())
        ]
    );
    assert!(location.join("state.sqlite").is_file());
}

#[test]
fn a_key_spent_on_another_command_is_a_conflict() {
    let (_scratch, _paths, store) = open("keyed-other-command");
    writing(&store, |tx| {
        run_once(
            tx,
            KEY,
            "set_setting",
            Timestamp::from_millis(FIXED_NOW_MS),
            |_| Ok("{}".to_owned()),
        )
        .map(drop)
    });

    let err = store
        .start_backup(BackupDestination::DataRoot, KEY)
        .unwrap_err();
    assert!(
        matches!(err, StoreError::Conflict { ref detail } if detail.contains("set_setting")),
        "{err:?}"
    );
    assert!(generations(&store).is_empty());
    assert_eq!(receipts(&store).len(), 1);
}

/// A start that is refused, or fails before its row, records no
/// receipt: the key starts a generation later.
#[test]
fn a_start_that_begins_nothing_spends_no_key() {
    let (_scratch, paths, store) = open("keyed-refused");
    let running = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let err = store
        .start_backup(BackupDestination::DataRoot, KEY)
        .unwrap_err();
    assert!(
        matches!(err, StoreError::Conflict { ref detail } if detail.contains("already in progress")),
        "{err:?}"
    );
    assert!(receipts(&store).is_empty());
    store.complete_backup(running).unwrap();

    let fs = FaultFs::new(real_fs(), FsOp::CreateDir, 1, FaultKind::NoSpace);
    let destination = paths.data_root().parent().unwrap().join("mount");
    let err = faulty_backups(&store, &paths, Arc::new(fs))
        .start(BackupDestination::Directory(destination), KEY)
        .unwrap_err();
    assert!(matches!(err, StoreError::Unavailable { .. }), "{err:?}");
    assert!(receipts(&store).is_empty());
    assert_eq!(generations(&store).len(), 1);

    let (receipt, snapshot) = started(store.start_backup(BackupDestination::DataRoot, KEY));
    assert_eq!(
        receipt_generation_id(&receipt).unwrap(),
        snapshot.generation_id()
    );
}

/// The row and the receipt are one commit, so a snapshot that fails
/// after it leaves both: the key names the generation that failed,
/// and starts no second one.
#[test]
fn a_failure_after_the_row_leaves_the_receipt_naming_it() {
    let (_scratch, paths, store) = open("keyed-fails-after-row");
    // The snapshot file's create: the first file work after the row.
    let fs = FaultFs::new(real_fs(), FsOp::Create, 1, FaultKind::Eio);
    let err = faulty_backups(&store, &paths, Arc::new(fs))
        .start(BackupDestination::DataRoot, KEY)
        .unwrap_err();
    assert!(matches!(err, StoreError::Unavailable { .. }), "{err:?}");

    let rows = generations(&store);
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].1, "in_progress");
    let stored = receipts(&store);
    assert_eq!(stored.len(), 1);
    assert_eq!(
        stored[0].2,
        format!(r#"{{"generation_id":"{}"}}"#, rows[0].0)
    );

    let again = replayed(store.start_backup(BackupDestination::DataRoot, KEY));
    assert_eq!(receipt_generation_id(&again).unwrap(), rows[0].0);
    assert_eq!(generations(&store), rows, "nothing was recovered or begun");
}

/// Disarming the hook while its first rendezvous is held must not
/// discard the second rendezvous that releases that same operation.
#[test]
fn disarming_the_key_look_hook_keeps_its_release() {
    use std::sync::mpsc;
    use std::time::{Duration, Instant};

    let (_scratch, _paths, store) = open("keyed-disarm-hook");
    let store = Arc::new(store);
    let gate = Arc::new(Barrier::new(2));
    store.backups.set_pauses(Pauses {
        after_key_look: Some(gate.clone()),
        ..Pauses::default()
    });
    let working = {
        let store = Arc::clone(&store);
        std::thread::spawn(move || store.start_backup(BackupDestination::DataRoot, KEY))
    };
    // The test and the armed hook own two references. The third proves
    // the worker captured its gate before we lock the hook registry.
    let deadline = Instant::now() + Duration::from_secs(60);
    while Arc::strong_count(&gate) < 3 {
        assert!(
            Instant::now() < deadline,
            "the worker never reached its hook"
        );
        std::thread::yield_now();
    }
    let mut pauses = store.backups.pauses.lock().unwrap();
    gate.wait();
    *pauses = Pauses::default();
    drop(pauses);

    let (released, done) = mpsc::channel();
    let release = std::thread::spawn({
        let gate = Arc::clone(&gate);
        move || {
            gate.wait();
            released.send(()).unwrap();
        }
    });
    let kept_release = done.recv_timeout(Duration::from_secs(5)).is_ok();
    if !kept_release {
        // A broken worker skipped its release. Free the helper before
        // failing, rather than leaving a test thread blocked forever.
        gate.wait();
    }
    release.join().unwrap();
    drop(started(working.join().unwrap()));
    Arc::try_unwrap(store).ok().unwrap().close().unwrap();
    assert!(kept_release, "disarming the hook lost the worker's release");
}

/// The key is asked again inside the commit. A start whose look found
/// it unspent, and that then finds it spent on a whole generation,
/// answers that receipt and inserts nothing.
#[test]
fn a_key_spent_between_the_look_and_the_commit_is_replayed() {
    let (_scratch, _paths, store) = open("keyed-race");
    let store = Arc::new(store);
    let gate = Arc::new(Barrier::new(2));
    store.backups.set_pauses(Pauses {
        after_key_look: Some(gate.clone()),
        ..Pauses::default()
    });
    let late = {
        let store = Arc::clone(&store);
        std::thread::spawn(move || store.start_backup(BackupDestination::DataRoot, KEY))
    };
    // The late start has looked: the key is unspent.
    gate.wait();
    store.backups.set_pauses(Pauses::default());
    let (receipt, snapshot) = started(store.start_backup(BackupDestination::DataRoot, KEY));
    let id = snapshot.generation_id().to_owned();
    store.complete_backup(snapshot).unwrap();
    gate.wait();

    let answered = replayed(late.join().unwrap());
    assert_eq!(answered, receipt);
    assert_eq!(generations(&store), [(id, "complete".to_owned())]);
    assert_eq!(receipts(&store).len(), 1);
    // The late start took no snapshot: no second directory.
    let directories = std::fs::read_dir(_paths.backups())
        .unwrap()
        .filter(|entry| entry.as_ref().unwrap().path().is_dir())
        .count();
    assert_eq!(directories, 1);
}

/// The same look, and the key's generation still being copied when
/// the late start goes on: the operation is held, and the answer is
/// the receipt all the same, never `Conflict`.
#[test]
fn a_key_spent_after_the_look_is_replayed_while_its_copy_runs() {
    let (_scratch, _paths, store) = open("keyed-race-copying");
    let store = Arc::new(store);
    let gate = Arc::new(Barrier::new(2));
    store.backups.set_pauses(Pauses {
        after_key_look: Some(gate.clone()),
        ..Pauses::default()
    });
    let late = {
        let store = Arc::clone(&store);
        std::thread::spawn(move || store.start_backup(BackupDestination::DataRoot, KEY))
    };
    gate.wait();
    store.backups.set_pauses(Pauses::default());
    let (receipt, snapshot) = started(store.start_backup(BackupDestination::DataRoot, KEY));
    gate.wait();

    let answered = replayed(late.join().unwrap());
    assert_eq!(answered, receipt);
    assert_eq!(
        generations(&store),
        [(
            snapshot.generation_id().to_owned(),
            "in_progress".to_owned()
        )]
    );
    // Another key is refused there, as before, and spends nothing.
    let err = store
        .start_backup(BackupDestination::DataRoot, OTHER_KEY)
        .unwrap_err();
    assert!(matches!(err, StoreError::Conflict { .. }), "{err:?}");
    assert_eq!(receipts(&store).len(), 1);
    store.complete_backup(snapshot).unwrap();
}

/// The same race against another command: the key is refused inside
/// the commit, and the generation's row rolls back with it.
#[test]
fn a_key_another_command_takes_before_the_commit_begins_nothing() {
    let (_scratch, _paths, store) = open("keyed-race-other");
    let store = Arc::new(store);
    let gate = Arc::new(Barrier::new(2));
    store.backups.set_pauses(Pauses {
        after_key_look: Some(gate.clone()),
        ..Pauses::default()
    });
    let late = {
        let store = Arc::clone(&store);
        std::thread::spawn(move || store.start_backup(BackupDestination::DataRoot, KEY))
    };
    gate.wait();
    writing(&store, |tx| {
        run_once(
            tx,
            KEY,
            "set_setting",
            Timestamp::from_millis(FIXED_NOW_MS),
            |_| Ok("{}".to_owned()),
        )
        .map(drop)
    });
    gate.wait();

    let err = late.join().unwrap().unwrap_err();
    assert!(
        matches!(err, StoreError::Conflict { ref detail } if detail.contains("set_setting")),
        "{err:?}"
    );
    assert!(generations(&store).is_empty());
    assert!(!due_to_cleanup(&store));
}

/// Whether the store is due for an interrupted generation alone: it
/// has a complete one and no change since.
fn due_to_cleanup(store: &Store) -> bool {
    reading(store, crate::backups::cleanup_owed)
}

/// A damaged store's generation is no backup, and its writer takes no
/// receipt: a key answers nothing there, spent or not.
#[test]
fn a_latched_store_answers_no_key() {
    let (_scratch, _paths, store) = open("keyed-latched");
    let (_, snapshot) = started(store.start_backup(BackupDestination::DataRoot, KEY));
    store.complete_backup(snapshot).unwrap();
    store.latch.latch_for_test("a test finding");

    for key in [KEY, OTHER_KEY] {
        let err = store
            .start_backup(BackupDestination::DataRoot, key)
            .unwrap_err();
        assert!(
            matches!(err, StoreError::Corrupt { ref detail } if detail.contains("a test finding")),
            "{key}: {err:?}"
        );
    }
    assert_eq!(receipts(&store).len(), 1);
    assert_eq!(generations(&store).len(), 1);
}

#[test]
fn the_writer_thread_cannot_start_a_backup_through_its_own_queue() {
    let (_scratch, _paths, store) = open("keyed-on-writer");
    let store = Arc::new(store);
    let inner = Arc::clone(&store);
    let err = block_on(store.write(crate::WritePriority::User, move |_| {
        Ok(inner
            .start_backup(BackupDestination::DataRoot, KEY)
            .unwrap_err())
    }))
    .unwrap();
    assert!(matches!(err, StoreError::Internal { .. }), "{err:?}");
    assert!(receipts(&store).is_empty());
}

/// A stopped copy on an archive nothing changes: the store owes a
/// generation for the cleanup alone, and taking it lets GC run again.
#[test]
fn an_interrupted_generation_makes_the_store_due() {
    let (_scratch, _paths, store) = open("due-interrupted");
    let first = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    store.complete_backup(first).unwrap();
    assert!(!due(&store));

    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let interrupted = snapshot.generation_id().to_owned();
    assert!(!due(&store), "its copy runs: nothing is interrupted");
    let clone = snapshot.clone();
    drop(snapshot);
    assert!(!due(&store), "one handle still copies it");
    drop(clone);
    assert!(due(&store), "nothing copies it now");
    assert_eq!(
        store.collect_garbage(GC_GRACE_PERIOD).unwrap().blocked,
        Some(GcGate::BackupInProgress)
    );

    let next = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    assert_eq!(generation_row(&store, &interrupted).unwrap().0, "failed");
    assert!(!due(&store), "the next one copies");
    store.complete_backup(next).unwrap();
    assert!(!due(&store));
    assert_eq!(
        store.collect_garbage(GC_GRACE_PERIOD).unwrap().blocked,
        None
    );
}

/// A snapshot that failed after its row is interrupted at once: its
/// handle never existed.
#[test]
fn a_failed_snapshot_s_generation_is_interrupted() {
    let (_scratch, _paths, store) = open("due-failed-snapshot");
    let first = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    store.complete_backup(first).unwrap();
    // Fail the store's own snapshot: its directory's name is taken.
    let taken = Arc::new(Barrier::new(2));
    store.backups.set_pauses(Pauses {
        after_snapshot_file: Some(taken.clone()),
        ..Pauses::default()
    });
    let store = Arc::new(store);
    let background = {
        let store = Arc::clone(&store);
        std::thread::spawn(move || store.backup_snapshot(BackupDestination::DataRoot))
    };
    taken.wait();
    assert!(!due(&store), "the snapshot is being taken");
    let location: String = reading(&store, |tx| {
        tx.query_row(
            "SELECT location FROM backup_generations WHERE state = 'in_progress'",
            [],
            |row| row.get(0),
        )
        .map_err(bug)
    });
    use std::os::unix::fs::PermissionsExt;
    let file = Path::new(&location).join("state.sqlite");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o444)).unwrap();
    taken.wait();
    let err = background.join().unwrap().unwrap_err();
    store.backups.set_pauses(Pauses::default());
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert!(matches!(err, StoreError::Unavailable { .. }), "{err:?}");

    assert!(due(&store));
}

/// One statement as the store's own bookkeeping, which counts as no
/// change: a fixture row then makes the store due for cleanup or not
/// at all.
fn bookkeeping(store: &Store, statement: String) {
    crate::store::await_write(store.writer.upkeep(crate::WritePriority::Index, move |tx| {
        tx.execute_batch(&statement).map_err(bug)
    }))
    .unwrap();
}

/// A failed generation whose removal stopped part way is owed its
/// cleanup too; a failed row nothing marked (a restore inherits them)
/// is not.
#[test]
fn a_cleanup_that_stopped_part_way_makes_the_store_due() {
    let (_scratch, paths, store) = open("due-cleanup-pending");
    let first = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    store.complete_backup(first).unwrap();

    let inherited = "00000000-0000-4000-8000-00000000f001";
    bookkeeping(
        &store,
        format!(
            "INSERT INTO backup_generations (generation_id, started_at, state, location)
             VALUES ('{inherited}', 0, 'failed', '{}')",
            paths.backups().join(inherited).display()
        ),
    );
    assert!(!due(&store), "an inherited failed row owes nothing");

    bookkeeping(
        &store,
        format!("INSERT INTO backup_cleanup_pending VALUES ('{inherited}')"),
    );
    assert!(due(&store));

    // A marker on a complete generation is damage no backup clears
    // (recovery refuses it): it owes nothing, so it can't keep the
    // store due for good.
    let complete: String = reading(&store, |tx| {
        tx.query_row(
            "SELECT generation_id FROM backup_generations WHERE state = 'complete'",
            [],
            |row| row.get(0),
        )
        .map_err(bug)
    });
    bookkeeping(
        &store,
        format!(
            "DELETE FROM backup_cleanup_pending;
             INSERT INTO backup_cleanup_pending VALUES ('{complete}')"
        ),
    );
    assert!(!due(&store), "a marker recovery can't act on owes nothing");
    bookkeeping(
        &store,
        format!(
            "DELETE FROM backup_cleanup_pending;
             INSERT INTO backup_cleanup_pending VALUES ('{inherited}')"
        ),
    );
    assert!(due(&store));

    let next = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let pending: i64 = reading(&store, |tx| {
        tx.query_row("SELECT count(*) FROM backup_cleanup_pending", [], |row| {
            row.get(0)
        })
        .map_err(bug)
    });
    assert_eq!(pending, 0, "the next generation finished it");
    store.complete_backup(next).unwrap();
    assert!(!due(&store));
    assert!(!due_to_cleanup(&store));
}

/// The two reasons stand apart: a change makes the store due while a
/// generation is being copied, an interrupted one doesn't hide it.
#[test]
fn a_change_is_owed_whatever_is_being_copied() {
    let (_scratch, _paths, store) = open("due-change-while-copying");
    let first = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    store.complete_backup(first).unwrap();

    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    change(&store, 1);
    assert!(due(&store), "the change is after the snapshot");
    store.complete_backup(snapshot).unwrap();
    assert!(due(&store));
}

/// Not a test: `run_crash_cuts` re-executes it per cut point, with
/// the cuts on the snapshot's own file operations as in
/// `snapshot_cut_child`.
#[test]
#[ignore = "spawned by run_crash_cuts"]
fn keyed_cut_child() {
    let nth = env::var(CUT_N).unwrap().parse().unwrap();
    let root = env::var(CUT_ROOT).unwrap();
    let paths = StorePaths::new(Path::new(&root)).unwrap();
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    let (id, _) = object(&store, b"one");
    reference(&store, &id, "published");
    let backups = faulty_backups(&store, &paths, Arc::new(CutFs::new(real_fs(), nth)));
    backups.start(BackupDestination::DataRoot, KEY).unwrap();
}

/// At every cut point the key's receipt and the generation's row are
/// both there or both absent, and the receipt names the row. A key
/// that committed starts nothing again; one that didn't starts a
/// generation; and the next generation recovers the cut one.
#[test]
fn crash_cuts_leave_the_receipt_and_the_row_together() {
    let scratch = ScratchDir::new("keyed-cuts");
    let (mut with_receipt, mut without) = (0, 0);
    let cuts = run_crash_cuts(
        CUT_N,
        CUT_ROOT,
        "backups::tests::keyed::keyed_cut_child",
        &mut |n| {
            let root = scratch.path().join(format!("cut-{n}"));
            std::fs::create_dir(&root).unwrap();
            root
        },
        &mut |root| {
            let paths = StorePaths::new(root).unwrap();
            let store = crate::open(&paths, &test_seams(real_fs()))
                .expect("the store must reopen after every cut");
            let rows = generations(&store);
            let stored = receipts(&store);
            assert_eq!(rows.len(), stored.len(), "{rows:?} {stored:?}");
            match rows.first() {
                None => {
                    without += 1;
                    let (_, snapshot) =
                        started(store.start_backup(BackupDestination::DataRoot, KEY));
                    store.complete_backup(snapshot).unwrap();
                }
                Some((id, state)) => {
                    with_receipt += 1;
                    assert_eq!(state, "in_progress");
                    assert_eq!(stored[0].2, format!(r#"{{"generation_id":"{id}"}}"#));
                    let again = replayed(store.start_backup(BackupDestination::DataRoot, KEY));
                    assert_eq!(&receipt_generation_id(&again).unwrap(), id);
                    assert_eq!(generations(&store), rows, "a replay recovers nothing");
                    assert!(store.backup_due().unwrap());

                    let (_, snapshot) =
                        started(store.start_backup(BackupDestination::DataRoot, OTHER_KEY));
                    store.complete_backup(snapshot).unwrap();
                    assert_eq!(generation_row(&store, id).unwrap().0, "failed");
                }
            }
        },
    );
    assert!(cuts > 0, "the sweep must have killed children");
    assert!(
        with_receipt > 0 && without > 0,
        "cuts fell on both sides of the commit: {with_receipt} after, {without} before"
    );
}

// --- start_restore's receipt (M1.8i5) ---

/// A restore's receipt is written in the store its swap put in place,
/// and the key answers it from then on. It is bookkeeping: no
/// generation is owed for it.
#[test]
fn a_restore_s_receipt_is_recorded_once_and_owes_no_generation() {
    let (_scratch, _paths, store) = open("keyed-restore");
    let (_, snapshot) = started(store.start_backup(BackupDestination::DataRoot, KEY));
    store.complete_backup(snapshot).unwrap();
    assert!(!due(&store));
    assert_eq!(store.restore_receipt(OTHER_KEY).unwrap(), None);

    let receipt = store.record_restore(OTHER_KEY, "gen-restored").unwrap();
    assert_eq!(receipt.command(), START_RESTORE);
    assert_eq!(receipt.result_json(), r#"{"generation_id":"gen-restored"}"#);
    assert_eq!(
        store.restore_receipt(OTHER_KEY).unwrap(),
        Some(receipt.clone())
    );
    assert!(!due(&store), "a receipt is no change to back up");

    // Recorded again, whatever the generation, it answers what it
    // stored.
    assert_eq!(
        store.record_restore(OTHER_KEY, "gen-other").unwrap(),
        receipt
    );
    assert_eq!(receipts(&store).len(), 2);
}

/// A key belongs to one command: a backup's runs no restore, and a
/// restore's starts no backup.
#[test]
fn a_restore_s_key_and_a_backup_s_conflict_both_ways() {
    let (_scratch, _paths, store) = open("keyed-restore-conflict");
    let (_, snapshot) = started(store.start_backup(BackupDestination::DataRoot, KEY));
    store.complete_backup(snapshot).unwrap();

    let asked = store.restore_receipt(KEY).map(drop);
    let recorded = store.record_restore(KEY, "gen").map(drop);
    for refused in [asked, recorded] {
        let err = refused.unwrap_err();
        assert!(
            matches!(err, StoreError::Conflict { ref detail } if detail.contains(START_BACKUP)),
            "{err:?}"
        );
    }

    store.record_restore(OTHER_KEY, "gen").unwrap();
    let err = store
        .start_backup(BackupDestination::DataRoot, OTHER_KEY)
        .unwrap_err();
    assert!(
        matches!(err, StoreError::Conflict { ref detail } if detail.contains(START_RESTORE)),
        "{err:?}"
    );
    assert_eq!(generations(&store).len(), 1);
    assert_eq!(receipts(&store).len(), 2);
}

/// A read-only store still says what a key answers: the restore that
/// is its way out asks before it moves anything.
#[test]
fn a_latched_store_still_answers_a_restore_s_key() {
    let (_scratch, _paths, store) = open("keyed-restore-latched");
    let receipt = store.record_restore(KEY, "gen").unwrap();
    store.latch.latch_for_test("a test finding");

    assert_eq!(store.restore_receipt(KEY).unwrap(), Some(receipt));
    assert_eq!(store.restore_receipt(OTHER_KEY).unwrap(), None);
}

#[test]
fn the_writer_thread_cannot_record_a_restore_through_its_own_queue() {
    let (_scratch, _paths, store) = open("keyed-restore-on-writer");
    let store = Arc::new(store);
    let inner = Arc::clone(&store);
    let err = block_on(store.write(crate::WritePriority::User, move |_| {
        Ok(inner.record_restore(KEY, "gen").unwrap_err())
    }))
    .unwrap();
    assert!(matches!(err, StoreError::Internal { .. }), "{err:?}");
    assert!(receipts(&store).is_empty());
}
