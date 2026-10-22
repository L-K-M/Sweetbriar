use super::*;
use crate::{CoreConfig, ImportJobState, ImportTarget, UniFfiTag, open_core};
use sb_test_support::{ScratchDir, block_on, execute_sql};
use std::future::Future;
use std::task::{Context, Poll, Waker};
use std::time::{Duration, Instant};

macro_rules! wire {
    ($kind:ty, $value:expr) => {{
        let value = $value;
        let raw = <$kind as uniffi::Lower<UniFfiTag>>::lower(value.clone());
        assert_eq!(
            <$kind as uniffi::Lift<UniFfiTag>>::try_lift(raw).unwrap(),
            value
        );
    }};
}

#[test]
fn coverage_maps_every_field_variant_and_real_wire_payload() {
    let mut source = sb_service::Coverage {
        generation: u64::MAX,
        extractor_version: u32::MAX,
        total_messages: 31,
        headers_synced: 29,
        bodies_acquired: 27,
        indexed: 13,
        partial: 5,
        failed: 3,
        pending: 10,
        queue_length: 7,
        index_state: sb_service::SearchIndexState::Ready,
        rebuild: None,
        limitations: vec![
            sb_service::SearchLimitation::AttachmentTextUnavailable,
            sb_service::SearchLimitation::CjkUnsegmented,
        ],
    };
    let expected = Coverage {
        generation: u64::MAX,
        extractor_version: u32::MAX,
        total_messages: 31,
        headers_synced: 29,
        bodies_acquired: 27,
        indexed: 13,
        partial: 5,
        failed: 3,
        pending: 10,
        queue_length: 7,
        index_state: SearchIndexState::Ready,
        rebuild: None,
        limitations: vec![
            SearchLimitation::AttachmentTextUnavailable,
            SearchLimitation::CjkUnsegmented,
        ],
    };
    assert_eq!(Coverage::from_service(&source), expected);
    wire!(Coverage, expected);
    for (scope, native) in [
        (SearchScope::All, sb_service::SearchScope::All),
        (
            SearchScope::Account {
                account_id: "RAW-é".into(),
            },
            sb_service::SearchScope::Account("RAW-é".into()),
        ),
        (
            SearchScope::Mailbox {
                mailbox_id: i64::MAX,
            },
            sb_service::SearchScope::Mailbox(i64::MAX),
        ),
    ] {
        wire!(SearchScope, scope.clone());
        assert_eq!(scope.into_service(), native);
    }
    for (native, expected) in [
        (sb_service::SearchIndexState::Ready, SearchIndexState::Ready),
        (
            sb_service::SearchIndexState::Running,
            SearchIndexState::Running,
        ),
        (
            sb_service::SearchIndexState::PausedByUser,
            SearchIndexState::PausedByUser,
        ),
        (
            sb_service::SearchIndexState::BlockedNewerExtractor,
            SearchIndexState::BlockedNewerExtractor,
        ),
    ] {
        source.index_state = native;
        let actual = Coverage::from_service(&source);
        assert_eq!(actual.index_state, expected);
        wire!(Coverage, actual);
    }
    for (native, expected) in [
        (
            sb_service::SearchIoCode::StorageFull,
            SearchIoCode::StorageFull,
        ),
        (
            sb_service::SearchIoCode::PermissionDenied,
            SearchIoCode::PermissionDenied,
        ),
        (
            sb_service::SearchIoCode::StorageUnavailable,
            SearchIoCode::StorageUnavailable,
        ),
        (sb_service::SearchIoCode::OtherIo, SearchIoCode::OtherIo),
    ] {
        source.index_state = sb_service::SearchIndexState::PausedIo {
            code: native,
            message: "Reconnect é".into(),
            retry_at: i64::MAX,
        };
        let actual = Coverage::from_service(&source);
        assert_eq!(
            actual.index_state,
            SearchIndexState::PausedIo {
                code: expected,
                message: "Reconnect é".into(),
                retry_at: i64::MAX
            }
        );
        wire!(SearchIoCode, expected);
        wire!(Coverage, actual);
    }
    for (native, expected) in [
        (
            sb_service::SearchRebuildPhase::Reset,
            SearchRebuildPhase::Reset,
        ),
        (
            sb_service::SearchRebuildPhase::Enqueue,
            SearchRebuildPhase::Enqueue,
        ),
        (
            sb_service::SearchRebuildPhase::Index,
            SearchRebuildPhase::Index,
        ),
        (
            sb_service::SearchRebuildPhase::Complete,
            SearchRebuildPhase::Complete,
        ),
    ] {
        source.rebuild = Some(sb_service::SearchRebuildProgress {
            job_id: "stored-é".into(),
            phase: native,
            processed: u64::MAX,
            pending_documents: 71,
        });
        let actual = Coverage::from_service(&source);
        assert_eq!(
            actual.rebuild.as_ref().unwrap(),
            &SearchRebuildProgress {
                job_id: "stored-é".into(),
                phase: expected,
                processed: u64::MAX,
                pending_documents: 71
            }
        );
        wire!(SearchRebuildPhase, expected);
        wire!(SearchRebuildProgress, actual.rebuild.clone().unwrap());
        wire!(Coverage, actual);
    }
    for value in [
        SearchLimitation::CjkUnsegmented,
        SearchLimitation::AttachmentTextUnavailable,
    ] {
        wire!(SearchLimitation, value);
    }
}

pub(in crate::search) fn open(scratch: &ScratchDir) -> Arc<Core> {
    block_on(open_core(
        CancelToken::new(),
        CoreConfig {
            data_root: scratch.path().join("data").to_string_lossy().into_owned(),
            cache_root: scratch.path().join("cache").to_string_lossy().into_owned(),
        },
    ))
    .unwrap()
}

pub(in crate::search) fn all(core: &Core) -> Coverage {
    block_on(core.coverage(CancelToken::new(), SearchScope::All)).unwrap()
}

pub(in crate::search) fn wait(mut condition: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(60);
    while !condition() {
        assert!(
            Instant::now() < end,
            "native coverage fixture did not settle"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

#[test]
fn native_coverage_preserves_operational_scope_phase_and_capability_facts() {
    let scratch = ScratchDir::new("ffi-coverage-native");
    let mut core = open(&scratch);
    wait(|| {
        all(&core).index_state == SearchIndexState::Ready
            && all(&core)
                .rebuild
                .as_ref()
                .is_some_and(|job| job.phase == SearchRebuildPhase::Complete)
    });
    let version = all(&core).extractor_version;
    block_on(core.shutdown());
    let db = scratch.path().join("data/state.sqlite");
    for (index, (code, expected)) in [
        ("storage_full", SearchIoCode::StorageFull),
        ("permission_denied", SearchIoCode::PermissionDenied),
        ("storage_unavailable", SearchIoCode::StorageUnavailable),
        ("other_io", SearchIoCode::OtherIo),
    ]
    .into_iter()
    .enumerate()
    {
        execute_sql(
            &db,
            &format!(
                "BEGIN IMMEDIATE; UPDATE search_index_state SET user_paused=0,io_code='{code}',io_message='Reconnect é',io_retry_at=4000000000000,io_attempts=5 WHERE id=0; UPDATE search_rebuild_jobs SET phase='index'; UPDATE store_meta SET value=CAST(value AS INTEGER)+1 WHERE key='search_generation'; COMMIT;"
            ),
        );
        core = open(&scratch);
        if index == 0 {
            let account = block_on(core.accounts(CancelToken::new())).unwrap()[0]
                .account_id
                .clone();
            let empty = block_on(core.coverage(
                CancelToken::new(),
                SearchScope::Account {
                    account_id: account,
                },
            ))
            .unwrap();
            assert_eq!(empty.total_messages, 0);
            assert_eq!(
                empty.index_state,
                SearchIndexState::PausedIo {
                    code: expected,
                    message: "Reconnect é".into(),
                    retry_at: 4_000_000_000_000
                }
            );
            let path = scratch.path().join("Coverage.mbox");
            std::fs::write(&path, "From sender@example.test Sat Oct 17 12:00:00 2026\nSubject: coverage\nMessage-ID: <coverage@test>\n\nbody\n").unwrap();
            let started = block_on(core.start_import(
                CancelToken::new(),
                "10000000-0000-4000-8000-000000000001".into(),
                vec![path.to_string_lossy().into_owned()],
                ImportTarget::LocalArchive,
            ))
            .unwrap();
            wait(|| {
                block_on(core.import_job(CancelToken::new(), started.job_id.clone()))
                    .unwrap()
                    .state
                    == ImportJobState::AwaitingConfirmation
            });
            block_on(core.confirm_import(
                CancelToken::new(),
                "10000000-0000-4000-8000-000000000002".into(),
                started.job_id.clone(),
                vec![],
            ))
            .unwrap();
            wait(|| {
                block_on(core.import_job(CancelToken::new(), started.job_id.clone()))
                    .unwrap()
                    .state
                    == ImportJobState::Completed
            });
        }
        let account = block_on(core.accounts(CancelToken::new())).unwrap()[0]
            .account_id
            .clone();
        let mailbox = block_on(core.mailboxes(CancelToken::new(), account.clone()))
            .unwrap()
            .mailboxes[0]
            .mailbox_id;
        let list = block_on(core.open_window(
            CancelToken::new(),
            crate::WindowSpec::Mailbox {
                mailbox_id: mailbox,
            },
            crate::Anchor::Top,
            1,
        ))
        .unwrap();
        wait(|| {
            block_on(core.get_message(CancelToken::new(), list.rows[0].message_id))
                .unwrap()
                .thread_id
                .is_some()
        });
        block_on(core.close_window(list.window_id));
        let value = all(&core);
        assert_eq!(
            value.index_state,
            SearchIndexState::PausedIo {
                code: expected,
                message: "Reconnect é".into(),
                retry_at: 4_000_000_000_000
            }
        );
        assert_eq!(
            (
                value.total_messages,
                value.headers_synced,
                value.bodies_acquired,
                value.indexed,
                value.partial,
                value.failed,
                value.pending,
                value.queue_length
            ),
            (1, 1, 1, 0, 0, 0, 1, 1)
        );
        for scope in [
            SearchScope::Account {
                account_id: account,
            },
            SearchScope::Mailbox {
                mailbox_id: mailbox,
            },
        ] {
            assert_eq!(
                block_on(core.coverage(CancelToken::new(), scope)).unwrap(),
                value
            );
        }
        block_on(core.shutdown());
    }
    for (phase, expected) in [
        ("reset", SearchRebuildPhase::Reset),
        ("enqueue", SearchRebuildPhase::Enqueue),
        ("index", SearchRebuildPhase::Index),
        ("complete", SearchRebuildPhase::Complete),
    ] {
        execute_sql(
            &db,
            &format!(
                "BEGIN IMMEDIATE; UPDATE search_index_state SET user_paused=1 WHERE id=0; UPDATE search_rebuild_jobs SET job_id='stored-é',phase='{phase}'; UPDATE store_meta SET value=CAST(value AS INTEGER)+1 WHERE key='search_generation'; COMMIT;"
            ),
        );
        core = open(&scratch);
        let value = all(&core);
        assert_eq!(value.index_state, SearchIndexState::PausedByUser);
        let job = value.rebuild.unwrap();
        assert_eq!(
            (job.job_id.as_str(), job.phase, job.pending_documents),
            (
                "stored-é",
                expected,
                if expected == SearchRebuildPhase::Complete {
                    0
                } else {
                    1
                }
            )
        );
        assert_eq!(value.pending, 1);
        block_on(core.shutdown());
    }
    execute_sql(
        &db,
        &format!(
            "BEGIN IMMEDIATE; UPDATE store_meta SET value='{}' WHERE key='search_extractor_version'; UPDATE store_meta SET value=CAST(value AS INTEGER)+1 WHERE key='search_generation'; COMMIT;",
            version + 1
        ),
    );
    core = open(&scratch);
    assert_eq!(
        all(&core).index_state,
        SearchIndexState::BlockedNewerExtractor
    );
    for scope in [
        SearchScope::Account {
            account_id: "bad".into(),
        },
        SearchScope::Mailbox { mailbox_id: 0 },
    ] {
        assert!(
            matches!(block_on(core.coverage(CancelToken::new(), scope)), Err(CoreError::InvalidInput { field, .. }) if field=="scope")
        );
    }
    assert!(matches!(
        block_on(core.coverage(
            CancelToken::new(),
            SearchScope::Mailbox {
                mailbox_id: i64::MAX
            }
        )),
        Err(CoreError::NotFound { .. })
    ));
    let token = CancelToken::new();
    token.cancel();
    assert!(matches!(
        block_on(core.coverage(token, SearchScope::All)),
        Err(CoreError::Cancelled)
    ));
    let writing = block_on(core.service.write());
    let token = CancelToken::new();
    let mut query = Box::pin(core.coverage(token.clone(), SearchScope::All));
    let mut cx = Context::from_waker(Waker::noop());
    assert!(query.as_mut().poll(&mut cx).is_pending());
    token.cancel();
    assert!(matches!(
        query.as_mut().poll(&mut cx),
        Poll::Ready(Err(CoreError::Cancelled))
    ));
    drop(query);
    drop(writing);
    block_on(core.shutdown());
    assert!(matches!(
        block_on(core.coverage(CancelToken::new(), SearchScope::All)),
        Err(CoreError::Unavailable { .. })
    ));
}
