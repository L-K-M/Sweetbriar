use sb_test_support::{ScratchDir, block_on};

use super::*;
use crate::{CoreConfig, open_core};

fn generation(state: sb_service::BackupState) -> sb_service::BackupGeneration {
    sb_service::BackupGeneration {
        generation_id: "opaque-e\u{301}\0".into(),
        state,
        started_at_ms: i64::MIN,
        completed_at_ms: None,
        location: PathBuf::from("/backup-é/e\u{301}\n"),
        object_count: None,
        total_bytes: None,
        verification: None,
    }
}

#[test]
fn generation_mapping_preserves_every_field_option_and_closed_variant() {
    for (service, expected) in [
        (sb_service::BackupState::InProgress, BackupState::InProgress),
        (sb_service::BackupState::Complete, BackupState::Complete),
        (sb_service::BackupState::Failed, BackupState::Failed),
    ] {
        let empty = BackupGeneration::from_service(generation(service)).unwrap();
        assert_eq!(empty.generation_id.as_bytes(), b"opaque-e\xcc\x81\0");
        assert_eq!(empty.state, expected);
        assert_eq!(empty.started_at_ms, i64::MIN);
        assert_eq!(empty.location, "/backup-é/e\u{301}\n");
        assert_eq!(empty.completed_at_ms, None);
        assert_eq!(empty.object_count, None);
        assert_eq!(empty.total_bytes, None);
        assert_eq!(empty.verification, None);
        for (verdict, expected_verdict) in [
            (sb_service::BackupVerdict::Intact, BackupVerdict::Intact),
            (sb_service::BackupVerdict::Damaged, BackupVerdict::Damaged),
            (sb_service::BackupVerdict::Missing, BackupVerdict::Missing),
        ] {
            let mut value = generation(service);
            value.completed_at_ms = Some(i64::MAX);
            value.object_count = Some(u64::MAX);
            value.total_bytes = Some(0);
            value.verification = Some(sb_service::BackupVerification {
                verdict,
                checked_at_ms: i64::MIN,
            });
            let mapped = BackupGeneration::from_service(value).unwrap();
            assert_eq!(mapped.completed_at_ms, Some(i64::MAX));
            assert_eq!(mapped.object_count, Some(u64::MAX));
            assert_eq!(mapped.total_bytes, Some(0));
            assert_eq!(
                mapped.verification,
                Some(BackupVerification {
                    verdict: expected_verdict,
                    checked_at_ms: i64::MIN,
                })
            );
        }
    }
}

#[test]
fn retained_names_preserve_paths_and_both_kinds() {
    for (kind, expected) in [
        (
            sb_service::RestoreArtifactKind::Preserved,
            RestoreArtifactKind::Preserved,
        ),
        (
            sb_service::RestoreArtifactKind::Staged,
            RestoreArtifactKind::Staged,
        ),
    ] {
        let value = RestoreArtifact::from_service(sb_service::RestoreArtifact {
            path: "/parent-é/data.e\u{301}".into(),
            kind,
        })
        .unwrap();
        assert_eq!(value.path, "/parent-é/data.e\u{301}");
        assert_eq!(value.kind, expected);
    }
}

#[cfg(unix)]
#[test]
fn an_unrepresentable_path_fails_instead_of_publishing_lossy_names() {
    use std::ffi::OsString;
    use std::os::unix::ffi::OsStringExt;
    let bad = PathBuf::from(OsString::from_vec(vec![b'/', 0xff]));
    let mut value = generation(sb_service::BackupState::Complete);
    value.location = bad.clone();
    assert!(matches!(
        BackupGeneration::from_service(value),
        Err(CoreError::Unavailable { .. })
    ));
    assert!(matches!(
        RestoreArtifact::from_service(sb_service::RestoreArtifact {
            path: bad,
            kind: sb_service::RestoreArtifactKind::Staged,
        }),
        Err(CoreError::Unavailable { .. })
    ));
}

#[test]
fn native_queries_read_real_rows_and_unqualified_names_with_cancellation_and_close() {
    let scratch = ScratchDir::new("ffi-backup-listings");
    let parent = scratch.path().canonicalize().unwrap();
    let core = block_on(open_core(
        CancelToken::new(),
        CoreConfig {
            data_root: parent.join("data").to_str().unwrap().into(),
            cache_root: parent.join("cache").to_str().unwrap().into(),
        },
    ))
    .unwrap();
    assert!(
        block_on(core.list_backups(CancelToken::new()))
            .unwrap()
            .is_empty()
    );
    let staged = parent.join("data.restoring-00000000-0000-4000-8000-000000000021");
    std::fs::write(&staged, b"an unqualified file, not a restore stage").unwrap();
    let guard = block_on(core.service.read());
    let started = block_on(
        guard
            .as_ref()
            .unwrap()
            .start_backup("00000000-0000-4000-8000-000000000022"),
    )
    .unwrap();
    assert_eq!(
        block_on(started.copy.unwrap().finished()),
        sb_service::BackupEnd::Complete { after: None }
    );
    drop(guard);
    let rows = block_on(core.list_backups(CancelToken::new())).unwrap();
    assert_eq!(rows.len(), 1);
    assert_eq!(rows[0].generation_id, started.generation_id);
    assert_eq!(rows[0].state, BackupState::Complete);
    assert!(rows[0].completed_at_ms.is_some());
    assert_eq!(rows[0].object_count, Some(0));
    assert!(rows[0].total_bytes.unwrap() > 0);
    std::fs::remove_dir_all(&rows[0].location).unwrap();
    assert_eq!(
        block_on(core.list_backups(CancelToken::new())).unwrap(),
        rows
    );
    let names = block_on(core.list_restore_artifacts(CancelToken::new())).unwrap();
    assert_eq!(
        names,
        vec![RestoreArtifact {
            path: staged.to_str().unwrap().into(),
            kind: RestoreArtifactKind::Staged,
        }]
    );
    assert_eq!(
        std::fs::read(staged).unwrap(),
        b"an unqualified file, not a restore stage"
    );
    let cancelled = CancelToken::new();
    cancelled.cancel();
    assert!(matches!(
        block_on(core.list_backups(cancelled.clone())),
        Err(CoreError::Cancelled)
    ));
    assert!(matches!(
        block_on(core.list_restore_artifacts(cancelled)),
        Err(CoreError::Cancelled)
    ));
    assert_eq!(
        block_on(core.list_restore_artifacts(CancelToken::new())).unwrap(),
        names
    );
    block_on(core.shutdown());
    assert!(matches!(
        block_on(core.list_backups(CancelToken::new())),
        Err(CoreError::Unavailable { .. })
    ));
    assert!(matches!(
        block_on(core.list_restore_artifacts(CancelToken::new())),
        Err(CoreError::Unavailable { .. })
    ));
}
