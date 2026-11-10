//! `list_backups` and `start_backup` through the engine (plan M1.8i1,
//! M1.8i3).

use std::future::Future;
use std::pin::pin;
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Mutex, mpsc};
use std::task::{Context, Waker};
use std::time::Duration;

use sb_resources::{BackupDestination, IntegrityCheck, StoreError};
use sb_test_support::{ScratchDir, block_on, damage_an_index};

use super::copy_end;
use crate::index_tests::{count, import, wait};
use crate::{BackupEnd, BackupGenerationState, Engine, EngineError, Integrity};

const RAW: &[u8] = b"From: Sender <sender@example.test>\r\nTo: other@example.test\r\nSubject: kept\r\n\r\nbody\r\n";
const GENERATIONS: &str = "SELECT count(*) FROM backup_generations";
const RECEIPTS: &str = "SELECT count(*) FROM command_receipts WHERE command = 'start_backup'";

/// One complete generation in the data root: its id.
fn back_up(engine: &Engine) -> String {
    let resources = engine.resources();
    let snapshot = resources
        .backup_snapshot(BackupDestination::DataRoot)
        .unwrap();
    let receipt = resources.complete_backup(snapshot).unwrap();
    receipt.generation_id().to_owned()
}

#[test]
fn the_generations_are_listed_as_recorded() {
    let scratch = ScratchDir::new("engine-list-backups");
    let engine = block_on(Engine::open(scratch.path().join("store"))).unwrap();
    assert_eq!(block_on(engine.list_backups()).unwrap(), Vec::new());

    let id = back_up(&engine);
    let listed = block_on(engine.list_backups()).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].generation_id, id);
    assert_eq!(listed[0].state, BackupGenerationState::Complete);
    assert!(
        listed[0].location.ends_with(&id),
        "{:?}",
        listed[0].location
    );
    assert!(listed[0].completed_at.is_some());

    // A snapshot whose copy hasn't run is in progress, and newest.
    let snapshot = engine
        .resources()
        .backup_snapshot(BackupDestination::DataRoot)
        .unwrap();
    let states: Vec<_> = block_on(engine.list_backups())
        .unwrap()
        .into_iter()
        .map(|generation| (generation.generation_id, generation.state))
        .collect();
    assert_eq!(
        states,
        [
            (
                snapshot.generation_id().to_owned(),
                BackupGenerationState::InProgress
            ),
            (id, BackupGenerationState::Complete),
        ]
    );
    drop(snapshot);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_read_only_engine_still_lists_them() {
    let scratch = ScratchDir::new("engine-list-backups-read-only");
    let root = std::fs::canonicalize(scratch.path()).unwrap().join("store");
    let engine = block_on(Engine::open(&root)).unwrap();
    let id = back_up(&engine);
    block_on(engine.shutdown()).unwrap();

    damage_an_index(&root.join("state.sqlite"));
    let engine = block_on(Engine::open(&root)).unwrap();
    let check = engine
        .resources()
        .check_integrity(&AtomicBool::new(false))
        .unwrap();
    assert!(matches!(check, IntegrityCheck::Finding(_)), "{check:?}");
    assert!(matches!(
        engine.health().integrity,
        Integrity::ReadOnly { .. }
    ));

    let listed = block_on(engine.list_backups()).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].generation_id, id);
    block_on(engine.shutdown()).unwrap();
}

// --- start_backup (M1.8i3) ---

/// An engine holding one imported message, so a backup has an object
/// to copy and a stop has a step to land on.
fn with_a_message(scratch: &ScratchDir) -> Engine {
    let engine = block_on(Engine::open(scratch.path().join("store"))).unwrap();
    import(&engine, &scratch.path().join("one.eml"), RAW);
    engine
}

/// Hold every started copy before its first object. Dropping the
/// sender lets them go; `reached` hears each one arrive.
fn hold_copies(engine: &Engine) -> (mpsc::Sender<()>, mpsc::Receiver<()>) {
    let (release, held) = mpsc::channel::<()>();
    let (entered, reached) = mpsc::channel();
    let held = Mutex::new(held);
    *engine.backup_copies().before_copy.lock().unwrap() = Some(Arc::new(move || {
        entered.send(()).unwrap();
        let _ = held.lock().unwrap().recv();
    }));
    (release, reached)
}

fn states(engine: &Engine) -> Vec<(String, BackupGenerationState)> {
    block_on(engine.list_backups())
        .unwrap()
        .into_iter()
        .map(|generation| (generation.generation_id, generation.state))
        .collect()
}

#[test]
fn a_backup_answers_at_its_snapshot_and_its_copy_goes_on() {
    let scratch = ScratchDir::new("engine-start-backup");
    let engine = with_a_message(&scratch);
    let (release, reached) = hold_copies(&engine);

    let key = engine.new_idempotency_key();
    let started = block_on(engine.start_backup(&key)).unwrap();
    let id = started.generation_id().to_owned();
    assert_eq!(started.receipt().command(), "start_backup");
    assert_eq!(
        started.receipt().result_json(),
        format!(r#"{{"generation_id":"{id}"}}"#)
    );
    // Answered, and the copy hasn't begun: the snapshot is there, the
    // generation is in progress, and the engine counts long work.
    reached.recv_timeout(Duration::from_secs(60)).unwrap();
    assert_eq!(
        states(&engine),
        [(id.clone(), BackupGenerationState::InProgress)]
    );
    assert!(engine.backup_copies().running());

    let copy = started.into_copy().expect("this call started the copy");
    drop(release);
    assert_eq!(
        block_on(copy.finished()),
        BackupEnd::Complete { after: None }
    );
    assert_eq!(states(&engine), [(id, BackupGenerationState::Complete)]);
    wait(|| !engine.backup_copies().running());
    assert!(!engine.resources().backup_due().unwrap());
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_repeated_key_answers_its_receipt_and_another_key_conflicts() {
    let scratch = ScratchDir::new("engine-start-backup-replay");
    let engine = with_a_message(&scratch);
    let (release, reached) = hold_copies(&engine);

    let key = engine.new_idempotency_key();
    let started = block_on(engine.start_backup(&key)).unwrap();
    let id = started.generation_id().to_owned();
    reached.recv_timeout(Duration::from_secs(60)).unwrap();

    // While the copy runs: the same key answers, another is refused
    // and leaves nothing.
    let again = block_on(engine.start_backup(&key)).unwrap();
    assert_eq!(again.receipt(), started.receipt());
    assert_eq!(again.generation_id(), id);
    assert!(again.into_copy().is_none(), "a replay starts no copy");
    let other = engine.new_idempotency_key();
    match block_on(engine.start_backup(&other)) {
        Err(EngineError::Conflict { .. }) => {}
        other => panic!("expected Conflict, got {other:?}"),
    }
    assert_eq!(count(&engine, GENERATIONS), 1);
    assert_eq!(count(&engine, RECEIPTS), 1);

    drop(release);
    let receipt = started.receipt().clone();
    let copy = started.into_copy().unwrap();
    assert_eq!(
        block_on(copy.finished()),
        BackupEnd::Complete { after: None }
    );

    // After it completed: the same answer, and the refused key is
    // free to start one.
    let again = block_on(engine.start_backup(&key)).unwrap();
    assert_eq!(again.receipt(), &receipt);
    assert!(again.into_copy().is_none());
    assert_eq!(count(&engine, GENERATIONS), 1);
    *engine.backup_copies().before_copy.lock().unwrap() = None;
    let second = block_on(engine.start_backup(&other)).unwrap();
    let second_id = second.generation_id().to_owned();
    assert_ne!(second_id, id);
    assert_eq!(
        block_on(second.into_copy().unwrap().finished()),
        BackupEnd::Complete { after: None }
    );
    // Retention keeps the hour's newest: the first one is gone.
    assert_eq!(
        states(&engine),
        [(second_id, BackupGenerationState::Complete)]
    );
    assert_eq!(count(&engine, RECEIPTS), 2);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn an_empty_key_is_refused_before_anything_starts() {
    let scratch = ScratchDir::new("engine-start-backup-key");
    let engine = block_on(Engine::open(scratch.path().join("store"))).unwrap();
    match block_on(engine.start_backup("")) {
        Err(EngineError::InvalidInput { field, .. }) => assert_eq!(field, "idempotency_key"),
        other => panic!("expected InvalidInput, got {other:?}"),
    }
    assert_eq!(count(&engine, GENERATIONS), 0);
    assert!(!engine.backup_copies().running());
    block_on(engine.shutdown()).unwrap();
}

/// Shutdown waits for the copy and stops it at its next object. The
/// generation stays in progress, and the next open owes one for it.
#[test]
fn a_shutdown_stops_the_copy_and_the_next_open_owes_a_generation() {
    let scratch = ScratchDir::new("engine-start-backup-shutdown");
    let root = scratch.path().join("store");
    let engine = with_a_message(&scratch);
    let (release, reached) = hold_copies(&engine);
    let started = block_on(engine.start_backup(&engine.new_idempotency_key())).unwrap();
    let id = started.generation_id().to_owned();
    let copy = started.into_copy().unwrap();
    reached.recv_timeout(Duration::from_secs(60)).unwrap();

    let closing = std::thread::spawn(move || block_on(engine.shutdown()));
    // The stop is raised before shutdown waits; the copy is still
    // held, so shutdown can't have finished.
    std::thread::sleep(Duration::from_millis(200));
    assert!(!closing.is_finished(), "shutdown waits for the copy");
    drop(release);
    assert_eq!(block_on(copy.finished()), BackupEnd::Stopped);
    closing.join().unwrap().unwrap();

    let engine = block_on(Engine::open(&root)).unwrap();
    assert_eq!(states(&engine), [(id, BackupGenerationState::InProgress)]);
    assert!(engine.resources().backup_due().unwrap());
    block_on(engine.shutdown()).unwrap();
}

/// A copy that fails says why, and is not taken for a stop.
#[test]
fn a_failed_copy_says_why() {
    let scratch = ScratchDir::new("engine-start-backup-fails");
    let engine = with_a_message(&scratch);
    let (release, reached) = hold_copies(&engine);
    let started = block_on(engine.start_backup(&engine.new_idempotency_key())).unwrap();
    let id = started.generation_id().to_owned();
    reached.recv_timeout(Duration::from_secs(60)).unwrap();

    // The generation's directory goes before the first object lands.
    let location = block_on(engine.list_backups()).unwrap()[0].location.clone();
    std::fs::remove_dir_all(&location).unwrap();
    drop(release);
    match block_on(started.into_copy().unwrap().finished()) {
        BackupEnd::Failed(detail) => assert!(!detail.is_empty()),
        other => panic!("expected Failed, got {other:?}"),
    }
    assert_eq!(states(&engine), [(id, BackupGenerationState::InProgress)]);
    wait(|| !engine.backup_copies().running());
    assert!(
        engine.resources().backup_due().unwrap(),
        "it is interrupted"
    );
    block_on(engine.shutdown()).unwrap();
}

/// A caller that stops waiting once its command is accepted strands
/// nothing: the generation is taken and its copy completes.
#[test]
fn a_caller_that_stops_waiting_strands_nothing() {
    let scratch = ScratchDir::new("engine-start-backup-dropped");
    let engine = with_a_message(&scratch);
    let key = engine.new_idempotency_key();
    {
        let mut future = pin!(engine.start_backup(&key));
        // One poll submits the command; the caller then goes away.
        let _ = future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop()));
    }
    wait(|| {
        states(&engine).first().map(|(_, state)| *state) == Some(BackupGenerationState::Complete)
    });
    wait(|| !engine.backup_copies().running());

    let again = block_on(engine.start_backup(&key)).unwrap();
    assert_eq!(again.generation_id(), states(&engine)[0].0);
    assert!(again.into_copy().is_none());
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_read_only_engine_starts_no_backup() {
    let scratch = ScratchDir::new("engine-start-backup-read-only");
    let root = std::fs::canonicalize(scratch.path()).unwrap().join("store");
    let engine = block_on(Engine::open(&root)).unwrap();
    block_on(engine.shutdown()).unwrap();
    damage_an_index(&root.join("state.sqlite"));
    let engine = block_on(Engine::open(&root)).unwrap();
    let check = engine
        .resources()
        .check_integrity(&AtomicBool::new(false))
        .unwrap();
    assert!(matches!(check, IntegrityCheck::Finding(_)), "{check:?}");

    match block_on(engine.start_backup(&engine.new_idempotency_key())) {
        Err(EngineError::Corrupt { detail }) => {
            assert!(detail.contains("integrity"), "{detail}");
        }
        other => panic!("expected Corrupt, got {other:?}"),
    }
    assert_eq!(count(&engine, GENERATIONS), 0);
    assert!(!engine.backup_copies().running());
    block_on(engine.shutdown()).unwrap();
}

/// A store that latches under a copy refuses its next step, then
/// raises the stop. Whichever the copy met, its end is the finding.
#[test]
fn a_store_that_latched_under_a_copy_fails_it_with_the_finding() {
    let scratch = ScratchDir::new("engine-start-backup-latched");
    let root = std::fs::canonicalize(scratch.path()).unwrap().join("store");
    let engine = block_on(Engine::open(&root)).unwrap();
    block_on(engine.shutdown()).unwrap();
    damage_an_index(&root.join("state.sqlite"));
    let engine = block_on(Engine::open(&root)).unwrap();
    let resources = engine.resources();
    let stop = || {
        Err(StoreError::Unavailable {
            reason: "the backup was cancelled".into(),
            io_kind: None,
        })
    };
    // Until the check finds the damage, the stop is a shutdown's.
    assert_eq!(copy_end(resources, "gen", stop(), true), BackupEnd::Stopped);

    let check = resources.check_integrity(&AtomicBool::new(false)).unwrap();
    assert!(matches!(check, IntegrityCheck::Finding(_)), "{check:?}");
    let finding = resources.integrity_latch().unwrap().detail;
    let refused = resources
        .backup_snapshot(BackupDestination::DataRoot)
        .unwrap_err();
    assert!(matches!(refused, StoreError::Corrupt { .. }), "{refused:?}");
    for result in [stop(), Err(refused)] {
        match copy_end(resources, "gen", result, true) {
            BackupEnd::Failed(detail) => {
                assert!(detail.contains("read-only"), "{detail}");
                assert!(detail.contains(&finding), "{detail}");
            }
            other => panic!("expected Failed, got {other:?}"),
        }
    }
    block_on(engine.shutdown()).unwrap();
}

/// The copy's result, the stop and the row decide the end. A complete
/// row outranks the error: completion commits before retention runs.
#[test]
fn a_copy_s_end_is_read_from_its_result_the_stop_and_the_row() {
    let scratch = ScratchDir::new("engine-start-backup-end");
    let engine = block_on(Engine::open(scratch.path().join("store"))).unwrap();
    let complete = back_up(&engine);
    let resources = engine.resources();
    let snapshot = resources
        .backup_snapshot(BackupDestination::DataRoot)
        .unwrap();
    let unfinished = snapshot.generation_id().to_owned();
    let stop = || {
        Err(StoreError::Unavailable {
            reason: "the backup was cancelled".into(),
            io_kind: None,
        })
    };
    let full = || {
        Err(StoreError::Unavailable {
            reason: "cannot copy an object: no space".into(),
            io_kind: Some(std::io::ErrorKind::StorageFull),
        })
    };

    assert_eq!(
        copy_end(resources, &unfinished, stop(), true),
        BackupEnd::Stopped
    );
    // The same error with no stop raised, and an I/O error under one,
    // are failures.
    for (result, stopped) in [(stop(), false), (full(), true), (full(), false)] {
        match copy_end(resources, &unfinished, result, stopped) {
            BackupEnd::Failed(detail) => assert!(!detail.is_empty()),
            other => panic!("expected Failed, got {other:?}"),
        }
    }
    // Retention's error after the completion committed.
    match copy_end(resources, &complete, full(), false) {
        BackupEnd::Complete {
            after: Some(detail),
        } => {
            assert!(detail.contains("no space"), "{detail}");
        }
        other => panic!("expected Complete, got {other:?}"),
    }
    assert_eq!(
        copy_end(resources, &complete, stop(), true),
        BackupEnd::Complete {
            after: Some(stop().unwrap_err().to_string())
        }
    );
    let receipt = resources.complete_backup(snapshot).unwrap();
    assert_eq!(
        copy_end(resources, &unfinished, Ok(receipt), false),
        BackupEnd::Complete { after: None }
    );
    block_on(engine.shutdown()).unwrap();
}
