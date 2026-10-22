use super::*;
use crate::search::coverage::tests::{all, open, wait};
use crate::{
    ImportJobState, ImportStarted, ImportTarget, SearchIndexState, SearchIoCode,
    SearchRebuildPhase, UniFfiTag,
};
use sb_test_support::{ScratchDir, block_on, execute_sql, with_sqlite_write_lock};
use std::future::Future;
use std::path::PathBuf;
use std::task::{Context, Waker};

const STORED: &str = "stored-rebuild-é";
const RETRY_AT: i64 = 4_000_000_000_000;

fn token() -> Arc<CancelToken> {
    CancelToken::new()
}

fn key(core: &Core) -> String {
    core.service
        .try_read()
        .unwrap()
        .as_ref()
        .unwrap()
        .new_idempotency_key()
}

fn db(scratch: &ScratchDir) -> PathBuf {
    scratch.path().join("data/state.sqlite")
}

#[derive(Clone, Copy)]
enum Hold {
    User,
    UserAndIo,
}

/// Caller shut Core down; preserve the real epoch and prepare only valid state.
fn hold_closed(scratch: &ScratchDir, hold: Hold, phase: &str) {
    let state = match hold {
        Hold::User => "user_paused=1",
        Hold::UserAndIo => {
            "user_paused=1,io_code='other_io',io_message='Reconnect é',io_retry_at=4000000000000,io_attempts=5"
        }
    };
    execute_sql(&db(scratch), &format!(
        "BEGIN IMMEDIATE; UPDATE search_index_state SET {state} WHERE id=0;
         UPDATE search_rebuild_jobs SET phase='{phase}';
         UPDATE store_meta SET value=CAST(CAST(value AS INTEGER)+1 AS TEXT) WHERE key='search_generation'; COMMIT;"
    ));
}

fn held(scratch: &ScratchDir, hold: Hold) -> Arc<Core> {
    let core = open(scratch);
    complete(&core, 0);
    block_on(core.shutdown());
    execute_sql(&db(scratch), &format!(
        "BEGIN IMMEDIATE; UPDATE search_rebuild_jobs SET job_id='{STORED}' WHERE phase='complete';
         UPDATE store_meta SET value=CAST(CAST(value AS INTEGER)+1 AS TEXT) WHERE key='search_generation'; COMMIT;"
    ));
    hold_closed(scratch, hold, "complete");
    let core = open(scratch);
    assert_eq!(all(&core).rebuild.unwrap().job_id, STORED);
    assert_eq!(all(&core).index_state, SearchIndexState::PausedByUser);
    core
}

fn complete(core: &Core, count: u64) {
    wait(|| {
        let state = all(core);
        state.total_messages == count
            && state.indexed == count
            && state.pending == 0
            && state.queue_length == 0
            && state.index_state == SearchIndexState::Ready
            && state
                .rebuild
                .is_some_and(|job| job.phase == SearchRebuildPhase::Complete)
    });
}

fn waiting_import(core: &Core, scratch: &ScratchDir, name: &str) -> ImportStarted {
    let path = scratch.path().join(format!("{name}.mbox"));
    std::fs::write(&path, format!("From sender@example.test Sat Oct 17 12:00:00 2026\nSubject: {name}\nMessage-ID: <{name}@controls.test>\n\ncontrolneedle\n")).unwrap();
    let value = block_on(core.start_import(
        token(),
        key(core),
        vec![path.to_string_lossy().into_owned()],
        ImportTarget::LocalArchive,
    ))
    .unwrap();
    wait(|| {
        block_on(core.import_job(token(), value.job_id.clone()))
            .unwrap()
            .state
            == ImportJobState::AwaitingConfirmation
    });
    value
}

fn import(core: &Core, scratch: &ScratchDir, name: &str) -> String {
    let started = waiting_import(core, scratch, name);
    block_on(core.confirm_import(token(), key(core), started.job_id.clone(), vec![])).unwrap();
    wait(|| {
        block_on(core.import_job(token(), started.job_id.clone()))
            .unwrap()
            .state
            == ImportJobState::Completed
    });
    let account = block_on(core.accounts(token())).unwrap()[0]
        .account_id
        .clone();
    let mailbox = block_on(core.mailboxes(token(), account))
        .unwrap()
        .mailboxes
        .into_iter()
        .find(|value| value.name == name)
        .unwrap();
    let window = block_on(core.open_window(
        token(),
        crate::WindowSpec::Mailbox {
            mailbox_id: mailbox.mailbox_id,
        },
        crate::Anchor::Top,
        50,
    ))
    .unwrap();
    // Thread commits also advance Search generation; fence them before exact counter assertions.
    wait(|| {
        window.rows.iter().all(|row| {
            block_on(core.get_message(token(), row.message_id))
                .unwrap()
                .thread_id
                .is_some()
        })
    });
    block_on(core.close_window(window.window_id));
    started.job_id
}

fn start(core: &Core) -> SearchRebuildStarted {
    block_on(core.start_search_rebuild(token(), key(core))).unwrap()
}

fn receipt(value: &Receipt, command: &str, id: &str, state: &str) {
    assert_eq!(value.command, command);
    assert_eq!(
        value.result_json,
        format!("{{\"job_id\":\"{id}\",\"state\":\"{state}\"}}")
    );
    assert!(value.created_at_ms > 0);
}

#[test]
fn started_record_maps_and_roundtrips_real_wire_without_narrowing() {
    // Codec/mapping fixtures, not fabricated native command output.
    for timestamp in [i64::MIN, i64::MAX] {
        let value = sb_service::SearchRebuildStarted {
            receipt: sb_service::Receipt {
                command: "start-é".into(),
                result_json: "opaque-😀\0".into(),
                created_at_ms: timestamp,
            },
            job_id: "stored-é\n\0".into(),
        };
        let mapped = SearchRebuildStarted::from_service(&value);
        assert_eq!(mapped.receipt.command, value.receipt.command);
        assert_eq!(mapped.receipt.result_json, value.receipt.result_json);
        assert_eq!(mapped.receipt.created_at_ms, timestamp);
        assert_eq!(mapped.job_id, value.job_id);
        let raw = <SearchRebuildStarted as uniffi::Lower<UniFfiTag>>::lower(mapped.clone());
        assert_eq!(
            <SearchRebuildStarted as uniffi::Lift<UniFfiTag>>::try_lift(raw).unwrap(),
            mapped
        );
    }
}

#[test]
fn exported_controls_replay_route_and_preserve_durable_holds_across_reopen() {
    let scratch = ScratchDir::new("ffi-controls-replay");
    let core = held(&scratch, Hold::User);
    let imported = import(&core, &scratch, "one");
    for (value, command) in [
        (
            block_on(core.resume_search_rebuild(token(), key(&core), STORED.into())).unwrap(),
            "resume_search_rebuild",
        ),
        (
            block_on(core.cancel_job(token(), key(&core), STORED.into())).unwrap(),
            "cancel_job",
        ),
    ] {
        receipt(&value, command, STORED, "complete");
    }
    assert_eq!(all(&core).index_state, SearchIndexState::PausedByUser);
    let start_key = key(&core);
    let started = block_on(core.start_search_rebuild(token(), start_key.clone())).unwrap();
    assert_ne!(started.job_id, STORED);
    assert_eq!(started.receipt.command, "start_search_rebuild");
    assert_eq!(
        started.receipt.result_json,
        format!("{{\"job_id\":\"{}\"}}", started.job_id)
    );
    assert_eq!(
        block_on(core.start_search_rebuild(token(), start_key.clone())).unwrap(),
        started
    );
    assert_eq!(start(&core).job_id, started.job_id);
    assert_eq!(all(&core).rebuild.unwrap().phase, SearchRebuildPhase::Reset);
    assert!(matches!(
        block_on(core.resume_search_rebuild(token(), start_key, "unknown".into())),
        Err(CoreError::Conflict { .. })
    ));
    assert!(matches!(
        block_on(core.cancel_job(token(), key(&core), imported.clone())),
        Err(CoreError::Conflict { .. })
    ));
    let waiting = waiting_import(&core, &scratch, "waiting");
    assert!(matches!(
        block_on(core.resume_search_rebuild(token(), key(&core), waiting.job_id.clone())),
        Err(CoreError::InvalidInput { .. })
    ));
    let import_key = key(&core);
    let cancelled_import =
        block_on(core.cancel_job(token(), import_key.clone(), waiting.job_id.clone())).unwrap();
    receipt(
        &cancelled_import,
        "cancel_job",
        &waiting.job_id,
        "cancelled",
    );
    let untouched = waiting_import(&core, &scratch, "untouched");
    assert_eq!(
        block_on(core.cancel_job(token(), import_key, untouched.job_id.clone())).unwrap(),
        cancelled_import
    );
    assert_eq!(
        block_on(core.import_job(token(), untouched.job_id.clone()))
            .unwrap()
            .state,
        ImportJobState::AwaitingConfirmation
    );
    block_on(core.cancel_job(token(), key(&core), untouched.job_id)).unwrap();
    let cancel_key = key(&core);
    let cancelled =
        block_on(core.cancel_job(token(), cancel_key.clone(), started.job_id.clone())).unwrap();
    receipt(&cancelled, "cancel_job", &started.job_id, "paused");
    assert_eq!(
        block_on(core.cancel_job(token(), cancel_key.clone(), imported)).unwrap(),
        cancelled
    );
    for value in [
        block_on(core.cancel_job(token(), key(&core), "unknown".into())).unwrap_err(),
        block_on(core.resume_search_rebuild(token(), key(&core), "unknown".into())).unwrap_err(),
    ] {
        assert!(matches!(value, CoreError::NotFound { .. }));
    }
    let paused = all(&core);
    block_on(core.shutdown());
    let core = open(&scratch);
    assert_eq!(all(&core), paused);
    import(&core, &scratch, "later");
    let paused = all(&core);
    assert_eq!(
        (paused.pending, paused.queue_length, paused.index_state),
        (2, 2, SearchIndexState::PausedByUser)
    );
    assert_eq!(start(&core).job_id, started.job_id);
    let resume_key = key(&core);
    let resumed =
        block_on(core.resume_search_rebuild(token(), resume_key.clone(), started.job_id.clone()))
            .unwrap();
    receipt(
        &resumed,
        "resume_search_rebuild",
        &started.job_id,
        "running",
    );
    assert_eq!(
        block_on(core.resume_search_rebuild(token(), resume_key.clone(), "unknown".into()))
            .unwrap(),
        resumed
    );
    complete(&core, 2);
    assert_eq!(all(&core).rebuild.unwrap().job_id, started.job_id);
    block_on(core.shutdown());
    hold_closed(&scratch, Hold::User, "complete");
    let core = open(&scratch);
    let next = start(&core);
    assert_ne!(next.job_id, started.job_id);
    let before = all(&core);
    assert_eq!(
        block_on(core.resume_search_rebuild(token(), resume_key, next.job_id.clone())).unwrap(),
        resumed
    );
    assert_eq!(
        block_on(core.cancel_job(token(), cancel_key, next.job_id.clone())).unwrap(),
        cancelled
    );
    receipt(
        &block_on(core.resume_search_rebuild(token(), key(&core), started.job_id.clone())).unwrap(),
        "resume_search_rebuild",
        &started.job_id,
        "complete",
    );
    receipt(
        &block_on(core.cancel_job(token(), key(&core), started.job_id.clone())).unwrap(),
        "cancel_job",
        &started.job_id,
        "complete",
    );
    assert_eq!(
        all(&core),
        before,
        "old ids/replayed keys must not release the new hold"
    );
    block_on(core.resume_search_rebuild(token(), key(&core), next.job_id)).unwrap();
    complete(&core, 2);
    block_on(core.shutdown());
}

#[test]
fn exported_resume_keeps_io_and_global_retry_preserves_user_hold_in_every_phase() {
    for phase in ["reset", "enqueue", "index", "complete"] {
        let scratch = ScratchDir::new(&format!("ffi-retry-{phase}"));
        let core = held(&scratch, Hold::UserAndIo);
        block_on(core.shutdown());
        hold_closed(&scratch, Hold::UserAndIo, phase);
        let core = open(&scratch);
        let before = all(&core);
        let retry_key = key(&core);
        let retried = block_on(core.retry_search_index(token(), retry_key.clone())).unwrap();
        assert_eq!(
            (retried.command.as_str(), retried.result_json.as_str()),
            ("retry_search_index", "{}")
        );
        let after = all(&core);
        assert_eq!(after.generation, before.generation + 1);
        assert_eq!(
            (after.index_state.clone(), after.rebuild.clone()),
            (SearchIndexState::PausedByUser, before.rebuild)
        );
        assert_eq!(
            block_on(core.retry_search_index(token(), retry_key)).unwrap(),
            retried
        );
        assert_eq!(all(&core), after);
        block_on(core.shutdown());
    }
    let scratch = ScratchDir::new("ffi-control-io-resume");
    let core = held(&scratch, Hold::UserAndIo);
    import(&core, &scratch, "io");
    let started = start(&core);
    block_on(core.resume_search_rebuild(token(), key(&core), started.job_id.clone())).unwrap();
    assert_eq!(
        all(&core).index_state,
        SearchIndexState::PausedIo {
            code: SearchIoCode::OtherIo,
            message: "Reconnect é".into(),
            retry_at: RETRY_AT
        }
    );
    block_on(core.cancel_job(token(), key(&core), started.job_id.clone())).unwrap();
    block_on(core.retry_search_index(token(), key(&core))).unwrap();
    assert_eq!(all(&core).index_state, SearchIndexState::PausedByUser);
    block_on(core.resume_search_rebuild(token(), key(&core), started.job_id)).unwrap();
    complete(&core, 1);
    let before = all(&core);
    block_on(core.retry_search_index(token(), key(&core))).unwrap();
    assert_eq!(all(&core), before, "idle retry must not change coverage");
    block_on(core.shutdown());
}

#[derive(Debug, Clone, Copy)]
enum Control {
    Start,
    Resume,
    Retry,
}

impl Control {
    async fn call(
        self,
        core: &Core,
        token: Arc<CancelToken>,
        key: &str,
        id: &str,
    ) -> Result<Receipt, CoreError> {
        match self {
            Self::Start => core
                .start_search_rebuild(token, key.into())
                .await
                .map(|value| value.receipt),
            Self::Resume => {
                core.resume_search_rebuild(token, key.into(), id.into())
                    .await
            }
            Self::Retry => core.retry_search_index(token, key.into()).await,
        }
    }
}

#[test]
fn every_export_preserves_token_lifecycle_and_accepted_request_drop_semantics() {
    for control in [Control::Start, Control::Resume, Control::Retry] {
        let scratch = ScratchDir::new(&format!("ffi-control-drop-{control:?}"));
        let core = held(&scratch, Hold::UserAndIo);
        let before = all(&core);
        let cancelled = token();
        cancelled.cancel();
        assert!(matches!(
            block_on(control.call(&core, cancelled, &key(&core), STORED)),
            Err(CoreError::Cancelled)
        ));
        assert_eq!(all(&core), before);
        let queued_key = key(&core);
        let guard = block_on(core.service.write());
        let queued_token = token();
        let mut queued = Box::pin(control.call(&core, queued_token.clone(), &queued_key, STORED));
        assert!(
            queued
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending()
        );
        queued_token.cancel();
        assert!(matches!(block_on(queued), Err(CoreError::Cancelled)));
        drop(guard);
        block_on(control.call(&core, token(), &queued_key, STORED)).unwrap();
        block_on(core.shutdown());
        assert!(matches!(
            block_on(control.call(&core, token(), &queued_key, STORED)),
            Err(CoreError::Unavailable { .. })
        ));

        let scratch = ScratchDir::new(&format!("ffi-control-accepted-{control:?}"));
        let hold = if matches!(control, Control::Retry) {
            Hold::UserAndIo
        } else {
            Hold::User
        };
        let core = held(&scratch, hold);
        import(&core, &scratch, "accepted");
        let id = if matches!(control, Control::Start) {
            String::new()
        } else {
            start(&core).job_id
        };
        let before = all(&core);
        let command_key = key(&core);
        let mut request = Box::pin(control.call(&core, token(), &command_key, &id));
        with_sqlite_write_lock(&db(&scratch), || {
            assert!(
                request
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop()))
                    .is_pending()
            );
        });
        // Never poll the caller again; native coverage proves the accepted commit.
        wait(|| {
            let state = all(&core);
            match control {
                Control::Start => {
                    state.rebuild.as_ref().unwrap().job_id
                        != before.rebuild.as_ref().unwrap().job_id
                }
                Control::Resume => {
                    state.rebuild.as_ref().unwrap().phase == SearchRebuildPhase::Complete
                }
                Control::Retry => state.generation > before.generation,
            }
        });
        drop(request);
        let state = all(&core);
        let id = if matches!(control, Control::Start) {
            let replay = block_on(core.start_search_rebuild(token(), command_key.clone())).unwrap();
            assert_eq!(state.rebuild.as_ref().unwrap().job_id, replay.job_id);
            replay.job_id
        } else {
            id
        };
        let replay = block_on(control.call(&core, token(), &command_key, "unknown")).unwrap();
        match control {
            Control::Start => assert_eq!(replay.result_json, format!("{{\"job_id\":\"{id}\"}}")),
            Control::Resume => receipt(&replay, "resume_search_rebuild", &id, "running"),
            Control::Retry => {
                assert_eq!(replay.result_json, "{}");
                assert_eq!(state.generation, before.generation + 1);
            }
        }
        if !matches!(control, Control::Resume) {
            assert_eq!(state.index_state, SearchIndexState::PausedByUser);
            block_on(core.resume_search_rebuild(token(), key(&core), id)).unwrap();
        }
        complete(&core, 1);
        block_on(core.shutdown());
    }
}
