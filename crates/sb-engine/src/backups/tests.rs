//! `list_backups` through the engine (plan M1.8i1).

use std::sync::atomic::AtomicBool;

use sb_resources::{BackupDestination, IntegrityCheck};
use sb_test_support::{ScratchDir, block_on, damage_an_index};

use crate::{BackupGenerationState, Engine, Integrity};

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
