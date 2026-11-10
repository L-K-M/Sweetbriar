//! M1.8i1: the recorded generations as `list_backups` shows them.

use std::sync::atomic::AtomicBool;

use sb_test_support::real_fs;
use sb_types::time::Timestamp;

use super::{
    BackupGeneration, BackupGenerationState, FAILED_GENERATIONS_LISTED, GenerationVerification,
};
use crate::backups::BackupDestination;
use crate::schema::bug;
use crate::testing::{FIXED_NOW_MS, scratch_paths, test_seams, writing};
use crate::{GenerationCheck, GenerationVerdict, Store, StoreError, StorePaths};

fn open(label: &str) -> (sb_test_support::ScratchDir, Store) {
    let (scratch, _, store) = open_at(label);
    (scratch, store)
}

fn open_at(label: &str) -> (sb_test_support::ScratchDir, StorePaths, Store) {
    let (scratch, paths) = scratch_paths(label);
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    (scratch, paths, store)
}

/// A row as a past run left it: `state` at `started_at`, nothing else.
fn row(store: &Store, id: &str, state: &str, started_at: i64) {
    let statement = format!(
        "INSERT INTO backup_generations (generation_id, started_at, state, location)
         VALUES ('{id}', {started_at}, '{state}', '/backups/{id}')"
    );
    writing(store, move |tx| tx.execute_batch(&statement).map_err(bug));
}

fn listed(store: &Store) -> Vec<(String, BackupGenerationState)> {
    store
        .backup_generations()
        .unwrap()
        .into_iter()
        .map(|generation| (generation.generation_id, generation.state))
        .collect()
}

#[test]
fn a_store_without_generations_lists_none() {
    let (_scratch, store) = open("backup-list-empty");
    assert_eq!(store.backup_generations().unwrap(), Vec::new());
}

#[test]
fn a_generation_is_listed_as_its_row_stands() {
    let (_scratch, store) = open("backup-list-states");
    let at = Timestamp::from_millis(FIXED_NOW_MS);

    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let id = snapshot.generation_id().to_owned();
    let location = snapshot.location().to_owned();
    assert_eq!(
        store.backup_generations().unwrap(),
        vec![BackupGeneration {
            generation_id: id.clone(),
            state: BackupGenerationState::InProgress,
            started_at: at,
            completed_at: None,
            location: location.clone(),
            manifest_sha256: None,
            object_count: None,
            total_bytes: None,
            verification: None,
        }]
    );

    let receipt = store.complete_backup(snapshot).unwrap();
    let complete = store.backup_generations().unwrap();
    assert_eq!(complete.len(), 1);
    let generation = &complete[0];
    assert_eq!(generation.state, BackupGenerationState::Complete);
    assert_eq!(generation.completed_at, Some(at));
    assert_eq!(
        generation.manifest_sha256.as_deref(),
        Some(receipt.manifest_sha256())
    );
    assert_eq!(generation.object_count, Some(0));
    let database = std::fs::metadata(location.join("state.sqlite"))
        .unwrap()
        .len();
    assert_eq!(generation.total_bytes, Some(database));
    assert_eq!(generation.verification, None);

    let check = store.verify_generation(&id, &AtomicBool::new(false));
    assert_eq!(
        check.unwrap(),
        GenerationCheck::Recorded(GenerationVerdict::Intact)
    );
    assert_eq!(
        store.backup_generations().unwrap()[0].verification,
        Some(GenerationVerification {
            verdict: GenerationVerdict::Intact,
            checked_at: at,
        })
    );

    // The listing is the rows: a directory that went missing changes
    // nothing until the weekly check reads it.
    std::fs::remove_dir_all(&location).unwrap();
    assert_eq!(listed(&store), vec![(id, BackupGenerationState::Complete)]);
}

#[test]
fn generations_are_listed_newest_first_and_failures_only_since_the_last_complete_one() {
    let (_scratch, store) = open("backup-list-order");
    row(&store, "a-complete", "complete", 100);
    row(&store, "b-failed-before", "failed", 200);
    row(&store, "c-failed-before", "failed", 300);
    row(&store, "d-complete", "complete", 400);
    row(&store, "e-failed", "failed", 500);
    row(&store, "f-failed-tie", "failed", 500);
    row(&store, "g-in-progress", "in_progress", 600);

    use BackupGenerationState::{Complete, Failed, InProgress};
    assert_eq!(
        listed(&store),
        [
            ("g-in-progress", InProgress),
            ("f-failed-tie", Failed),
            ("e-failed", Failed),
            ("d-complete", Complete),
            ("a-complete", Complete),
        ]
        .map(|(id, state)| (id.to_owned(), state))
    );
}

#[test]
fn a_failed_generation_at_the_newest_complete_one_s_time_follows_the_order() {
    let (_scratch, store) = open("backup-list-tie");
    row(&store, "a-failed", "failed", 100);
    row(&store, "b-complete", "complete", 100);
    row(&store, "c-failed", "failed", 100);

    use BackupGenerationState::{Complete, Failed};
    assert_eq!(
        listed(&store),
        [("c-failed", Failed), ("b-complete", Complete)].map(|(id, state)| (id.to_owned(), state))
    );
}

#[test]
fn only_the_newest_failed_generations_are_listed() {
    let (_scratch, store) = open("backup-list-failed-bound");
    row(&store, "complete", "complete", 1);
    let extra = 5;
    for n in 0..FAILED_GENERATIONS_LISTED + extra {
        row(&store, &format!("failed-{n:03}"), "failed", 10 + n as i64);
    }
    row(&store, "in-progress", "in_progress", 5);

    let all = listed(&store);
    let failed: Vec<&str> = all
        .iter()
        .filter(|(_, state)| *state == BackupGenerationState::Failed)
        .map(|(id, _)| id.as_str())
        .collect();
    let newest: Vec<String> = (extra..FAILED_GENERATIONS_LISTED + extra)
        .rev()
        .map(|n| format!("failed-{n:03}"))
        .collect();
    assert_eq!(failed, newest);
    // The bound is on failed rows only.
    assert_eq!(all.len(), FAILED_GENERATIONS_LISTED + 2);
    assert_eq!(all[all.len() - 2].0, "in-progress");
    assert_eq!(all[all.len() - 1].0, "complete");
}

#[test]
fn a_count_no_generation_can_have_is_corrupt() {
    for column in ["object_count", "total_bytes"] {
        let (_scratch, store) = open(&format!("backup-list-malformed-{column}"));
        row(&store, "sound", "complete", 1);
        row(&store, "malformed", "complete", 2);
        let statement = format!(
            "UPDATE backup_generations SET {column} = -1 WHERE generation_id = 'malformed'"
        );
        writing(&store, move |tx| tx.execute_batch(&statement).map_err(bug));

        let error = store.backup_generations().unwrap_err();
        assert!(
            matches!(&error, StoreError::Corrupt { .. }) && error.to_string().contains("malformed"),
            "{column}: {error:?}"
        );
    }
}

/// The schema's checks keep these rows out, so one that holds them
/// came from a damaged file or from another program. A connection
/// that skips the checks, as none of the store's does, writes each.
#[test]
fn a_row_the_schema_s_checks_forbid_is_corrupt() {
    for (case, change) in [
        ("an unknown state", "state = 'paused'"),
        ("a verdict without its time", "verification = 'intact'"),
        ("a time without its verdict", "verified_at = 5"),
        (
            "an unknown verdict",
            "verification = 'fine', verified_at = 5",
        ),
    ] {
        let (_scratch, paths, store) = open_at("backup-list-forbidden");
        row(&store, "sound", "complete", 1);
        row(&store, "malformed", "complete", 2);
        assert_eq!(store.backup_generations().unwrap().len(), 2);
        let outside = rusqlite::Connection::open(paths.state_db()).unwrap();
        outside
            .execute_batch(&format!(
                "PRAGMA ignore_check_constraints = ON;
                 UPDATE backup_generations SET {change} WHERE generation_id = 'malformed'"
            ))
            .unwrap();
        drop(outside);

        let error = store.backup_generations().unwrap_err();
        assert!(
            matches!(&error, StoreError::Corrupt { .. }) && error.to_string().contains("malformed"),
            "{case}: {error:?}"
        );
    }
}

/// A failed row the listing leaves out is not read past its state:
/// the list is how a generation is chosen for a restore, and a row it
/// wouldn't show must not take that away.
#[test]
fn a_malformed_row_that_is_not_listed_fails_nothing() {
    let (_scratch, store) = open("backup-list-unlisted-malformed");
    row(&store, "old-failed", "failed", 1);
    row(&store, "complete", "complete", 2);
    writing(&store, |tx| {
        tx.execute_batch(
            "UPDATE backup_generations SET object_count = -1 WHERE generation_id = 'old-failed'",
        )
        .map_err(bug)
    });

    assert_eq!(
        listed(&store),
        [("complete".to_owned(), BackupGenerationState::Complete)]
    );
}
