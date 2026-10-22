use super::{import, listed, message, open, settled, wait};
use crate::{
    Anchor, Core, CoreError, Coverage, ImportJobState, ImportStarted, ImportTarget, ListRow,
    Receipt, SearchIndexState, SearchIoCode, SearchRebuildPhase, SearchScope, SearchSort,
    SearchSpec, WindowSpec,
};
use sb_test_support::{ScratchDir, block_on, execute_sql, with_sqlite_write_lock};
use std::future::Future;
use std::path::{Path, PathBuf};
use std::pin::Pin;
use std::task::{Context, Waker};

const STORED_ID: &str = "stored-rebuild-é";
const RETRY_AT: i64 = 4_000_000_000_000;

#[derive(Clone, Copy)]
enum Hold {
    User,
    UserAndIo,
}

fn db(scratch: &ScratchDir) -> PathBuf {
    scratch.path().join("store/state.sqlite")
}

/// Caller has shut Core down: no worker or active observer sees fixture writes.
fn pause_closed(scratch: &ScratchDir, hold: Hold) {
    let state = match hold {
        Hold::User => "user_paused=1",
        Hold::UserAndIo => {
            "user_paused=1,io_code='other_io',io_message='Reconnect é',io_retry_at=4000000000000,io_attempts=5"
        }
    };
    execute_sql(
        &db(scratch),
        &format!(
            "BEGIN IMMEDIATE; UPDATE search_index_state SET {state} WHERE id=0;
         UPDATE store_meta SET value=CAST(CAST(value AS INTEGER)+1 AS TEXT)
         WHERE key='search_generation'; COMMIT;"
        ),
    );
}

fn held(scratch: &ScratchDir, hold: Hold) -> Core {
    let core = open(scratch);
    settled(&core, &[]);
    block_on(core.shutdown());
    // Preserve the real automatic job's epoch; never plant a future epoch collision.
    execute_sql(
        &db(scratch),
        &format!(
            "BEGIN IMMEDIATE; UPDATE search_rebuild_jobs SET job_id='{STORED_ID}'
         WHERE phase='complete'; UPDATE store_meta SET value=CAST(CAST(value AS INTEGER)+1 AS TEXT)
         WHERE key='search_generation'; COMMIT;"
        ),
    );
    pause_closed(scratch, hold);
    let core = open(scratch);
    let progress = coverage(&core).rebuild.unwrap();
    assert_eq!(
        (progress.job_id.as_str(), progress.phase),
        (STORED_ID, SearchRebuildPhase::Complete)
    );
    assert_eq!(coverage(&core).index_state, SearchIndexState::PausedByUser);
    core
}

fn coverage(core: &Core) -> Coverage {
    block_on(core.coverage(SearchScope::All)).unwrap()
}

fn rows(core: &Core, name: &str) -> Vec<ListRow> {
    let account = block_on(core.accounts()).unwrap()[0].account_id.clone();
    let mailboxes = block_on(core.mailboxes(&account)).unwrap();
    let mailbox = mailboxes
        .mailboxes
        .iter()
        .find(|mailbox| mailbox.name == name)
        .unwrap()
        .mailbox_id;
    let rows = listed(core, mailbox);
    wait(|| {
        rows.iter().all(|row| {
            block_on(core.get_message(row.message_id))
                .unwrap()
                .thread_id
                .is_some()
        })
    });
    rows
}

fn queued(core: &Core, scratch: &ScratchDir) -> Vec<ListRow> {
    import(
        core,
        &scratch.path().join("control.mbox"),
        &message(21, "control", "controlneedle"),
    );
    let rows = rows(core, "control");
    assert_eq!(rows.len(), 1);
    let state = coverage(core);
    assert_eq!(
        (state.pending, state.queue_length, state.index_state),
        (1, 1, SearchIndexState::PausedByUser)
    );
    rows
}

fn state_receipt(receipt: &Receipt, command: &str, id: &str, state: &str) {
    assert_eq!(receipt.command, command);
    assert_eq!(
        receipt.result_json,
        format!("{{\"job_id\":\"{id}\",\"state\":\"{state}\"}}")
    );
    assert!(receipt.created_at_ms > 0);
}

fn complete(core: &Core, rows: &[ListRow], id: &str) {
    settled(core, rows);
    let state = coverage(core);
    assert_eq!(
        (
            state.indexed,
            state.partial,
            state.failed,
            state.pending,
            state.queue_length
        ),
        (rows.len() as u64, 0, 0, 0, 0)
    );
    let progress = state.rebuild.unwrap();
    assert_eq!(
        (
            progress.job_id.as_str(),
            progress.phase,
            progress.pending_documents
        ),
        (id, SearchRebuildPhase::Complete, 0)
    );
}

#[test]
fn service_rebuild_receipts_replay_and_completed_ids_preserve_holds() {
    let scratch = ScratchDir::new("service-rebuild-replay");
    let core = held(&scratch, Hold::User);
    let rows = queued(&core, &scratch);
    for (receipt, command) in [
        (
            block_on(core.resume_search_rebuild(&core.new_idempotency_key(), STORED_ID)).unwrap(),
            "resume_search_rebuild",
        ),
        (
            block_on(core.cancel_job(&core.new_idempotency_key(), STORED_ID)).unwrap(),
            "cancel_job",
        ),
    ] {
        state_receipt(&receipt, command, STORED_ID, "complete");
    }
    assert_eq!(coverage(&core).index_state, SearchIndexState::PausedByUser);
    let start_key = core.new_idempotency_key();
    let started = block_on(core.start_search_rebuild(&start_key)).unwrap();
    assert_ne!(started.job_id, STORED_ID);
    assert_eq!(started.receipt.command, "start_search_rebuild");
    assert_eq!(
        started.receipt.result_json,
        format!("{{\"job_id\":\"{}\"}}", started.job_id)
    );
    assert!(started.receipt.created_at_ms > 0);
    assert_eq!(
        block_on(core.start_search_rebuild(&start_key)).unwrap(),
        started
    );
    let reused = block_on(core.start_search_rebuild(&core.new_idempotency_key())).unwrap();
    assert_eq!(
        (reused.job_id, reused.receipt.result_json),
        (started.job_id.clone(), started.receipt.result_json.clone())
    );
    let paused = coverage(&core);
    assert_eq!(paused.index_state, SearchIndexState::PausedByUser);
    assert_eq!(
        paused.rebuild.as_ref().unwrap().phase,
        SearchRebuildPhase::Reset
    );
    assert!(matches!(
        block_on(core.open_window(
            WindowSpec::Search(SearchSpec {
                query_json: r#"{"v":1,"q":{"and":[]}}"#.into(),
                scope: SearchScope::All,
                sort: SearchSort::Date,
            }),
            Anchor::Top,
            1
        )),
        Err(CoreError::Unavailable { .. })
    ));
    let cancel_key = core.new_idempotency_key();
    let cancelled = block_on(core.cancel_job(&cancel_key, &started.job_id)).unwrap();
    state_receipt(&cancelled, "cancel_job", &started.job_id, "paused");
    assert_eq!(
        block_on(core.cancel_job(&cancel_key, "unknown")).unwrap(),
        cancelled
    );
    assert!(matches!(
        block_on(core.resume_search_rebuild(&start_key, "unknown")),
        Err(CoreError::Conflict { .. })
    ));
    for error in [
        block_on(core.resume_search_rebuild(&core.new_idempotency_key(), "unknown")).unwrap_err(),
        block_on(core.cancel_job(&core.new_idempotency_key(), "unknown")).unwrap_err(),
    ] {
        assert!(matches!(error, CoreError::NotFound { .. }));
    }
    let resume_key = core.new_idempotency_key();
    let resumed = block_on(core.resume_search_rebuild(&resume_key, &started.job_id)).unwrap();
    state_receipt(
        &resumed,
        "resume_search_rebuild",
        &started.job_id,
        "running",
    );
    assert_eq!(
        block_on(core.resume_search_rebuild(&resume_key, "unknown")).unwrap(),
        resumed
    );
    complete(&core, &rows, &started.job_id);
    block_on(core.shutdown());
    pause_closed(&scratch, Hold::User);
    let core = open(&scratch);
    let next = block_on(core.start_search_rebuild(&core.new_idempotency_key())).unwrap();
    assert_ne!(next.job_id, started.job_id);
    let held = coverage(&core);
    for (receipt, command) in [
        (
            block_on(core.resume_search_rebuild(&core.new_idempotency_key(), &started.job_id))
                .unwrap(),
            "resume_search_rebuild",
        ),
        (
            block_on(core.cancel_job(&core.new_idempotency_key(), &started.job_id)).unwrap(),
            "cancel_job",
        ),
    ] {
        state_receipt(&receipt, command, &started.job_id, "complete");
    }
    assert_eq!(
        block_on(core.resume_search_rebuild(&resume_key, &next.job_id)).unwrap(),
        resumed
    );
    assert_eq!(
        block_on(core.cancel_job(&cancel_key, &next.job_id)).unwrap(),
        cancelled
    );
    assert_eq!(
        coverage(&core),
        held,
        "stale/replayed ids cannot release or rewrite another job"
    );
    block_on(core.resume_search_rebuild(&core.new_idempotency_key(), &next.job_id)).unwrap();
    complete(&core, &rows, &next.job_id);
    block_on(core.shutdown());
}

fn waiting_import(core: &Core, scratch: &ScratchDir, name: &str, day: u8) -> ImportStarted {
    let path = scratch.path().join(format!("{name}.mbox"));
    std::fs::write(&path, message(day, name, "controlneedle")).unwrap();
    let started = block_on(core.start_import(
        &core.new_idempotency_key(),
        vec![path],
        ImportTarget::LocalArchive,
    ))
    .unwrap();
    wait(|| {
        block_on(core.import_job(&started.job_id)).unwrap().state
            == ImportJobState::AwaitingConfirmation
    });
    started
}

#[test]
fn service_cancel_dispatches_by_kind_and_pause_survives_reopen_and_later_imports() {
    let scratch = ScratchDir::new("service-rebuild-dispatch");
    let core = held(&scratch, Hold::User);
    let a = waiting_import(&core, &scratch, "a", 22);
    let b = waiting_import(&core, &scratch, "b", 23);
    assert!(matches!(
        block_on(core.resume_search_rebuild(&core.new_idempotency_key(), &a.job_id)),
        Err(CoreError::InvalidInput { .. })
    ));
    let key = core.new_idempotency_key();
    let receipt = block_on(core.cancel_job(&key, &a.job_id)).unwrap();
    state_receipt(&receipt, "cancel_job", &a.job_id, "cancelled");
    assert_eq!(
        block_on(core.import_job(&a.job_id)).unwrap().state,
        ImportJobState::Cancelled
    );
    assert_eq!(block_on(core.cancel_job(&key, &b.job_id)).unwrap(), receipt);
    assert_eq!(
        block_on(core.import_job(&b.job_id)).unwrap().state,
        ImportJobState::AwaitingConfirmation
    );
    block_on(core.cancel_job(&core.new_idempotency_key(), &b.job_id)).unwrap();
    let imported = waiting_import(&core, &scratch, "completed", 24);
    block_on(core.confirm_import(&core.new_idempotency_key(), &imported.job_id, vec![])).unwrap();
    wait(|| {
        block_on(core.import_job(&imported.job_id)).unwrap().state == ImportJobState::Completed
    });
    assert!(matches!(
        block_on(core.cancel_job(&core.new_idempotency_key(), &imported.job_id)),
        Err(CoreError::Conflict { .. })
    ));
    let mut all = rows(&core, "completed");
    let started = block_on(core.start_search_rebuild(&core.new_idempotency_key())).unwrap();
    let rebuild_cancel_key = core.new_idempotency_key();
    let cancelled = block_on(core.cancel_job(&rebuild_cancel_key, &started.job_id)).unwrap();
    state_receipt(&cancelled, "cancel_job", &started.job_id, "paused");
    assert_eq!(
        block_on(core.cancel_job(&rebuild_cancel_key, &imported.job_id)).unwrap(),
        cancelled
    );
    assert_eq!(
        block_on(core.import_job(&imported.job_id)).unwrap().state,
        ImportJobState::Completed
    );
    let resume_key = core.new_idempotency_key();
    let old = block_on(core.resume_search_rebuild(&resume_key, STORED_ID)).unwrap();
    state_receipt(&old, "resume_search_rebuild", STORED_ID, "complete");
    assert_eq!(
        block_on(core.resume_search_rebuild(&resume_key, &imported.job_id)).unwrap(),
        old
    );
    let paused = coverage(&core);
    assert_eq!(
        (
            paused.pending,
            paused.queue_length,
            paused.index_state.clone()
        ),
        (1, 1, SearchIndexState::PausedByUser)
    );
    block_on(core.shutdown());
    let core = open(&scratch);
    assert_eq!(coverage(&core), paused);
    import(
        &core,
        &scratch.path().join("later.mbox"),
        &message(25, "later", "secondneedle"),
    );
    all.extend(rows(&core, "later"));
    let paused = coverage(&core);
    assert_eq!(
        (paused.pending, paused.queue_length, paused.index_state),
        (2, 2, SearchIndexState::PausedByUser)
    );
    let progress = paused.rebuild.unwrap();
    assert_eq!(
        (
            progress.job_id.as_str(),
            progress.phase,
            progress.pending_documents
        ),
        (started.job_id.as_str(), SearchRebuildPhase::Reset, 2)
    );
    let reused = block_on(core.start_search_rebuild(&core.new_idempotency_key())).unwrap();
    assert_eq!(reused.job_id, started.job_id);
    assert_eq!(coverage(&core).index_state, SearchIndexState::PausedByUser);
    block_on(core.resume_search_rebuild(&core.new_idempotency_key(), &started.job_id)).unwrap();
    complete(&core, &all, &started.job_id);
    block_on(core.shutdown());
}

#[test]
fn service_resume_preserves_io_hold_and_retry_never_releases_user_hold() {
    let scratch = ScratchDir::new("service-rebuild-io");
    let core = held(&scratch, Hold::UserAndIo);
    let rows = queued(&core, &scratch);
    let started = block_on(core.start_search_rebuild(&core.new_idempotency_key())).unwrap();
    block_on(core.resume_search_rebuild(&core.new_idempotency_key(), &started.job_id)).unwrap();
    assert_eq!(
        coverage(&core).index_state,
        SearchIndexState::PausedIo {
            code: SearchIoCode::OtherIo,
            message: "Reconnect é".into(),
            retry_at: RETRY_AT,
        }
    );
    block_on(core.cancel_job(&core.new_idempotency_key(), &started.job_id)).unwrap();
    assert_eq!(coverage(&core).index_state, SearchIndexState::PausedByUser);
    block_on(core.shutdown());
    let core = open(&scratch);
    let before = coverage(&core);
    assert_eq!(before.index_state, SearchIndexState::PausedByUser);
    assert_eq!(before.rebuild.as_ref().unwrap().job_id, started.job_id);
    let key = core.new_idempotency_key();
    let retried = block_on(core.retry_search_index(&key)).unwrap();
    assert_eq!(
        (retried.command.as_str(), retried.result_json.as_str()),
        ("retry_search_index", "{}")
    );
    let after = coverage(&core);
    assert_eq!(after.generation, before.generation + 1);
    assert_eq!(
        (
            after.index_state.clone(),
            after.rebuild.clone(),
            after.pending,
            after.queue_length
        ),
        (SearchIndexState::PausedByUser, before.rebuild, 1, 1)
    );
    assert_eq!(block_on(core.retry_search_index(&key)).unwrap(), retried);
    assert_eq!(coverage(&core), after);
    block_on(core.resume_search_rebuild(&core.new_idempotency_key(), &started.job_id)).unwrap();
    complete(&core, &rows, &started.job_id);
    let before = coverage(&core);
    let key = core.new_idempotency_key();
    let idle = block_on(core.retry_search_index(&key)).unwrap();
    assert_eq!(idle.result_json, "{}");
    assert_eq!(block_on(core.retry_search_index(&key)).unwrap(), idle);
    assert_eq!(
        coverage(&core),
        before,
        "no I/O hold means retry is a no-op"
    );
    block_on(core.shutdown());
}

fn submit<F: Future>(state_db: &Path, mut future: Pin<&mut F>) {
    with_sqlite_write_lock(state_db, || {
        assert!(
            future
                .as_mut()
                .poll(&mut Context::from_waker(Waker::noop()))
                .is_pending()
        );
    });
}

#[derive(Debug, Clone, Copy)]
enum Control {
    Start,
    Resume,
    Retry,
}

#[test]
fn committed_service_control_requests_survive_dropped_callers() {
    for control in [Control::Start, Control::Resume, Control::Retry] {
        let scratch = ScratchDir::new(&format!("service-control-drop-{control:?}"));
        let hold = match control {
            Control::Retry => Hold::UserAndIo,
            _ => Hold::User,
        };
        let core = held(&scratch, hold);
        let rows = queued(&core, &scratch);
        let id = match control {
            Control::Start => String::new(),
            Control::Resume | Control::Retry => {
                block_on(core.start_search_rebuild(&core.new_idempotency_key()))
                    .unwrap()
                    .job_id
            }
        };
        let before = coverage(&core);
        let key = core.new_idempotency_key();
        let mut request = Box::pin(async {
            match control {
                Control::Start => core.start_search_rebuild(&key).await.map(drop),
                Control::Resume => core.resume_search_rebuild(&key, &id).await.map(drop),
                Control::Retry => core.retry_search_index(&key).await.map(drop),
            }
        });
        submit(&db(&scratch), request.as_mut());
        // The caller is never polled again. Coverage, not a guessed delay, proves commit.
        wait(|| {
            let state = coverage(&core);
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
        let observed = coverage(&core);
        let id = match control {
            Control::Start => {
                let replay = block_on(core.start_search_rebuild(&key)).unwrap();
                assert_eq!(observed.rebuild.as_ref().unwrap().job_id, replay.job_id);
                assert_eq!(observed.index_state, SearchIndexState::PausedByUser);
                replay.job_id
            }
            Control::Resume => {
                let replay = block_on(core.resume_search_rebuild(&key, "unknown")).unwrap();
                state_receipt(&replay, "resume_search_rebuild", &id, "running");
                id
            }
            Control::Retry => {
                assert_eq!(observed.generation, before.generation + 1);
                assert_eq!(observed.index_state, SearchIndexState::PausedByUser);
                assert_eq!(
                    block_on(core.retry_search_index(&key)).unwrap().result_json,
                    "{}"
                );
                id
            }
        };
        if !matches!(control, Control::Resume) {
            block_on(core.resume_search_rebuild(&core.new_idempotency_key(), &id)).unwrap();
        }
        complete(&core, &rows, &id);
        block_on(core.shutdown());
    }
}
