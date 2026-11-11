//! `list_backups` through the service (plan M1.8i1).

use std::path::PathBuf;

use sb_test_support::{ScratchDir, block_on};

use super::{BackupGeneration, BackupState, BackupVerdict, BackupVerification};
use crate::{CoreConfig, open_core};

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
