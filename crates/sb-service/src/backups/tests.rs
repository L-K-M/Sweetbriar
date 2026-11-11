//! `list_backups` and `start_backup` through the service (plan
//! M1.8i1, M1.8i3).

use std::path::PathBuf;

use sb_test_support::{ScratchDir, block_on};

use super::{BackupEnd, BackupGeneration, BackupState, BackupVerdict, BackupVerification};
use crate::{CoreConfig, CoreError, open_core};

fn recorded(state: sb_engine::BackupGenerationState) -> sb_engine::BackupGeneration {
    sb_engine::BackupGeneration {
        generation_id: "00000000-0000-4000-8000-0000000000b1".to_owned(),
        state,
        started_at: sb_engine::Timestamp::from_millis(1_000),
        completed_at: None,
        location: PathBuf::from("/backups/00000000-0000-4000-8000-0000000000b1"),
        manifest_sha256: None,
        object_count: None,
        total_bytes: None,
        verification: None,
    }
}

#[test]
fn a_fresh_store_lists_no_backups() {
    let scratch = ScratchDir::new("service-list-backups");
    let core = block_on(open_core(CoreConfig {
        data_root: scratch.path().join("store"),
        cache_root: scratch.path().join("cache"),
    }))
    .unwrap();
    assert_eq!(block_on(core.list_backups()).unwrap(), Vec::new());
    block_on(core.shutdown());
}

#[test]
fn every_field_and_variant_is_carried_over() {
    use sb_engine::BackupGenerationState as State;
    use sb_engine::GenerationVerdict as Verdict;

    for (stored, shown) in [
        (State::InProgress, BackupState::InProgress),
        (State::Failed, BackupState::Failed),
    ] {
        assert_eq!(
            BackupGeneration::from_engine(recorded(stored)),
            BackupGeneration {
                generation_id: "00000000-0000-4000-8000-0000000000b1".to_owned(),
                state: shown,
                started_at_ms: 1_000,
                completed_at_ms: None,
                location: PathBuf::from("/backups/00000000-0000-4000-8000-0000000000b1"),
                object_count: None,
                total_bytes: None,
                verification: None,
            }
        );
    }

    for (stored, shown) in [
        (Verdict::Intact, BackupVerdict::Intact),
        (Verdict::Damaged, BackupVerdict::Damaged),
        (Verdict::Missing, BackupVerdict::Missing),
    ] {
        let complete = sb_engine::BackupGeneration {
            completed_at: Some(sb_engine::Timestamp::from_millis(2_000)),
            manifest_sha256: Some("ab".repeat(32)),
            object_count: Some(3),
            total_bytes: Some(4_096),
            verification: Some(sb_engine::GenerationVerification {
                verdict: stored,
                checked_at: sb_engine::Timestamp::from_millis(3_000),
            }),
            ..recorded(State::Complete)
        };
        let generation = BackupGeneration::from_engine(complete);
        assert_eq!(generation.state, BackupState::Complete);
        assert_eq!(generation.completed_at_ms, Some(2_000));
        assert_eq!(generation.object_count, Some(3));
        assert_eq!(generation.total_bytes, Some(4_096));
        assert_eq!(
            generation.verification,
            Some(BackupVerification {
                verdict: shown,
                checked_at_ms: 3_000,
            })
        );
    }
}

#[test]
fn a_started_backup_completes_and_its_key_starts_no_second() {
    let scratch = ScratchDir::new("service-start-backup");
    let core = block_on(open_core(CoreConfig {
        data_root: scratch.path().join("store"),
        cache_root: scratch.path().join("cache"),
    }))
    .unwrap();

    let key = "00000000-0000-4000-8000-0000000000c1";
    let started = block_on(core.start_backup(key)).unwrap();
    let id = started.generation_id.clone();
    assert_eq!(started.receipt.command, "start_backup");
    assert_eq!(
        started.receipt.result_json,
        format!(r#"{{"generation_id":"{id}"}}"#)
    );
    let copy = started.copy.expect("this call started the copy");
    assert_eq!(
        block_on(copy.finished()),
        BackupEnd::Complete { after: None }
    );

    let listed = block_on(core.list_backups()).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].generation_id, id);
    assert_eq!(listed[0].state, BackupState::Complete);

    let again = block_on(core.start_backup(key)).unwrap();
    assert_eq!(again.receipt, started.receipt);
    assert_eq!(again.generation_id, id);
    assert!(again.copy.is_none());
    assert_eq!(block_on(core.list_backups()).unwrap().len(), 1);

    match block_on(core.start_backup("")) {
        Err(CoreError::InvalidInput { field, .. }) => assert_eq!(field, "idempotency_key"),
        other => panic!("expected InvalidInput, got {other:?}"),
    }
    block_on(core.shutdown());
}

#[test]
fn every_end_is_carried_over() {
    for (engine, shown) in [
        (
            sb_engine::BackupEnd::Complete { after: None },
            BackupEnd::Complete { after: None },
        ),
        (
            sb_engine::BackupEnd::Complete {
                after: Some("retention failed".to_owned()),
            },
            BackupEnd::Complete {
                after: Some("retention failed".to_owned()),
            },
        ),
        (sb_engine::BackupEnd::Stopped, BackupEnd::Stopped),
        (
            sb_engine::BackupEnd::Failed("no space".to_owned()),
            BackupEnd::Failed("no space".to_owned()),
        ),
    ] {
        assert_eq!(BackupEnd::from_engine(engine), shown);
    }
}
