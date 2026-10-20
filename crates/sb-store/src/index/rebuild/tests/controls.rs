//! The M1.6e1 runtime control transactions (spec/06 "Runtime control
//! transactions"): conditional gates, holds, rollback and exactly-once
//! generation.

use super::*;
use crate::index::{
    IndexFence, ObservedAvailability, PendingDocument, meta_integer, note_observed_availability,
    observe_availability_with_cut,
};

fn pending_token(conn: &Connection, doc_id: i64) -> PendingDocument {
    pending_batch(conn, 200)
        .unwrap()
        .documents
        .into_iter()
        .find(|pending| pending.doc_id == doc_id)
        .unwrap()
}

fn object_state(conn: &Connection, doc_id: i64) -> String {
    conn.query_row(
        "SELECT object_state FROM acquisitions WHERE acquisition_id = ?1",
        [doc_id],
        |row| row.get(0),
    )
    .unwrap()
}

fn search_gen(conn: &Connection) -> i64 {
    search_generation(conn).unwrap()
}

/// `list_generation` must never move for a search-only control.
fn list_gen(conn: &Connection) -> i64 {
    conn.query_row(
        "SELECT value FROM store_meta WHERE key = 'list_generation'",
        [],
        |row| row.get::<_, String>(0),
    )
    .unwrap()
    .parse()
    .unwrap()
}

fn io_pause_now(conn: &mut Connection, code: SearchIoCode, retry_at: i64, attempts: u32) {
    let tx = conn.transaction().unwrap();
    pause_for_io(&tx, code, "sanitized cause", now(retry_at), attempts).unwrap();
    tx.commit().unwrap();
}

fn observe(
    conn: &mut Connection,
    token: &PendingDocument,
    observed: ObservedAvailability,
    at: i64,
) -> bool {
    let tx = conn.transaction().unwrap();
    let applied = note_observed_availability(&tx, token, observed, now(at)).unwrap();
    tx.commit().unwrap();
    applied
}

#[test]
fn an_observation_marks_supersedes_and_preserves_everything_else() {
    let mut conn = connection();
    indexed(&mut conn, 1, "oldtoken");
    let tx = conn.transaction().unwrap();
    enqueue(&tx, &[(1, IndexReason::Reextract)], now(10)).unwrap();
    tx.commit().unwrap();
    let token = pending_token(&conn, 1);
    let generation = search_gen(&conn);
    let list_generation = list_gen(&conn);
    let inputs = meta_integer(&conn, "index_inputs").unwrap();

    assert!(observe(
        &mut conn,
        &token,
        ObservedAvailability::Missing,
        11
    ));
    assert_eq!(object_state(&conn, 1), "missing");
    // The request is superseded by a fresh delete revision that keeps
    // the oldest pending age; identity, docs, postings and authority
    // are untouched.
    let superseded = pending_token(&conn, 1);
    assert_eq!(superseded.reason, IndexReason::Delete);
    assert!(superseded.revision > token.revision);
    assert_eq!(superseded.enqueued_at, token.enqueued_at);
    assert_eq!(search_gen(&conn), generation + 1);
    assert_eq!(list_gen(&conn), list_generation);
    assert_eq!(hits(&conn, "oldtoken"), [1]);
    let (hash, length, acquired_at): (String, i64, i64) = conn
        .query_row(
            "SELECT content_sha256, byte_length, first_acquired_at FROM acquisitions
             WHERE acquisition_id = 1",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .unwrap();
    assert_eq!(hash, format!("{:064x}", 1));
    assert_eq!((length, acquired_at), (1, 1));

    // The delete request then publishes, removing derived state while
    // the acquisition row itself stays.
    let captured = pending_batch(&conn, 200).unwrap();
    let delete = IndexPublication {
        pending: captured.documents.into_iter().next().unwrap(),
        document: None,
    };
    let tx = conn.transaction().unwrap();
    assert_eq!(publish_batch(&tx, captured.fence, &[delete]).unwrap(), 1);
    tx.commit().unwrap();
    assert!(hits(&conn, "oldtoken").is_empty());
    let rows: i64 = conn
        .query_row("SELECT count(*) FROM acquisitions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(rows, 1);

    // A republished source requeues at a fresh revision; the old
    // observation is rejected and cannot downgrade it.
    conn.execute(
        "UPDATE acquisitions SET object_state = 'published' WHERE acquisition_id = 1",
        [],
    )
    .unwrap();
    let tx = conn.transaction().unwrap();
    enqueue(&tx, &[(1, IndexReason::Reextract)], now(20)).unwrap();
    tx.commit().unwrap();
    assert!(!observe(
        &mut conn,
        &token,
        ObservedAvailability::Missing,
        21
    ));
    // The acknowledged delete token is itself malformed input now.
    let tx = conn.transaction().unwrap();
    assert!(matches!(
        note_observed_availability(&tx, &superseded, ObservedAvailability::Missing, now(21)),
        Err(StoreError::InvalidInput { .. })
    ));
    tx.rollback().unwrap();
    assert_eq!(object_state(&conn, 1), "published");
    assert_eq!(meta_integer(&conn, "index_inputs").unwrap(), inputs + 2);
}

#[test]
fn malformed_observation_inputs_and_source_state_are_explicit_errors() {
    let mut conn = connection();
    acquisitions(&conn, 2);
    let tx = conn.transaction().unwrap();
    enqueue(&tx, &[(1, IndexReason::New)], now(1)).unwrap();
    tx.commit().unwrap();
    let token = pending_token(&conn, 1);
    let generation = search_gen(&conn);
    let mut delete_token = token.clone();
    delete_token.reason = IndexReason::Delete;
    let mut unknown_id = token.clone();
    unknown_id.doc_id = 0;
    let mut unknown_revision = token.clone();
    unknown_revision.revision = 0;
    let tx = conn.transaction().unwrap();
    for malformed in [&delete_token, &unknown_id, &unknown_revision] {
        assert!(
            matches!(
                note_observed_availability(&tx, malformed, ObservedAvailability::Missing, now(2)),
                Err(StoreError::InvalidInput { .. })
            ),
            "{malformed:?}"
        );
    }
    // A request the queue no longer holds is merely stale.
    let gone = PendingDocument {
        doc_id: 9,
        revision: 9,
        reason: IndexReason::New,
        enqueued_at: now(1),
    };
    assert!(
        !note_observed_availability(&tx, &gone, ObservedAvailability::Missing, now(2)).unwrap()
    );
    tx.commit().unwrap();
    assert_eq!(search_gen(&conn), generation);
    assert_eq!(object_state(&conn, 1), "published");

    // A still-current token naming a vanished acquisition is a hard
    // error, not bookkeeping.
    conn.execute("DELETE FROM acquisitions WHERE acquisition_id = 1", [])
        .unwrap();
    let tx = conn.transaction().unwrap();
    assert!(matches!(
        note_observed_availability(&tx, &token, ObservedAvailability::Missing, now(2)),
        Err(StoreError::NotFound { .. })
    ));
    tx.rollback().unwrap();
    assert_eq!(search_gen(&conn), generation);

    // An already-unavailable source under a still-current token is
    // impossible metadata: an error, never a silent repair.
    conn.execute(
        "UPDATE acquisitions SET object_state = 'missing' WHERE acquisition_id = 2",
        [],
    )
    .unwrap();
    conn.execute(
        "INSERT INTO d_index_queue (doc_id, reason, enqueued_at, revision) VALUES (2, 'reextract', 1, 42)",
        [],
    )
    .unwrap();
    let impossible = PendingDocument {
        doc_id: 2,
        revision: 42,
        reason: IndexReason::Reextract,
        enqueued_at: now(1),
    };
    let tx = conn.transaction().unwrap();
    assert!(matches!(
        note_observed_availability(&tx, &impossible, ObservedAvailability::Missing, now(2)),
        Err(StoreError::Corrupt { .. })
    ));
    tx.rollback().unwrap();
    assert_eq!(object_state(&conn, 2), "missing");
    assert_eq!(search_gen(&conn), generation);
}

#[test]
fn observation_stays_allowed_during_pauses_and_a_rebuild() {
    let mut conn = connection();
    indexed(&mut conn, 1, "oldtoken");
    let tx = conn.transaction().unwrap();
    enqueue(&tx, &[(1, IndexReason::Reextract)], now(10)).unwrap();
    tx.commit().unwrap();
    let token = pending_token(&conn, 1);
    pause(&mut conn, UserPause::Pause);
    io_pause_now(&mut conn, SearchIoCode::StorageFull, 50, 2);
    start(&mut conn, "job", 1);
    assert_eq!(job_at(&conn, "job").phase, RebuildPhase::Reset);

    // Source bookkeeping is not index publication or phase progress.
    assert!(observe(
        &mut conn,
        &token,
        ObservedAvailability::Quarantined,
        60
    ));
    assert_eq!(object_state(&conn, 1), "quarantined");
    assert_eq!(pending_token(&conn, 1).reason, IndexReason::Delete);
    let state = status(&conn).unwrap();
    assert!(state.user_paused());
    assert!(state.io_pause().is_some());
    assert_eq!(state.io_attempts(), 2);
    let job = job_at(&conn, "job");
    assert_eq!(job.phase, RebuildPhase::Reset);
    assert_eq!((job.doc_cursor, job.acq_cursor, job.phase_count), (0, 0, 0));
}

// One cut before each of: token recheck, source read, state update,
// delete enqueue, generation.
const OBSERVATION_CUTS: u64 = 5;

#[test]
fn operation_failures_roll_back_the_whole_observation_transition() {
    for nth in 1..=OBSERVATION_CUTS {
        let mut conn = connection();
        acquisitions(&conn, 1);
        let tx = conn.transaction().unwrap();
        enqueue(&tx, &[(1, IndexReason::Reextract)], now(10)).unwrap();
        tx.commit().unwrap();
        let token = pending_token(&conn, 1);
        let generation = search_gen(&conn);
        let inputs = meta_integer(&conn, "index_inputs").unwrap();
        let mut calls = 0;
        let mut cut = || {
            calls += 1;
            if calls == nth {
                return Err(StoreError::unavailable("injected observation fault"));
            }
            Ok(())
        };
        let tx = conn.transaction().unwrap();
        assert!(
            observe_availability_with_cut(
                &tx,
                &token,
                ObservedAvailability::Missing,
                now(11),
                &mut cut
            )
            .is_err(),
            "cut {nth} was not exercised"
        );
        tx.rollback().unwrap();
        // No part of the transition survives: state, queue revision,
        // revision counter and generation are all as captured.
        assert_eq!(object_state(&conn, 1), "published", "cut {nth}");
        assert_eq!(
            pending_token(&conn, 1).revision,
            token.revision,
            "cut {nth}"
        );
        assert_eq!(
            pending_token(&conn, 1).reason,
            IndexReason::Reextract,
            "cut {nth}"
        );
        assert_eq!(search_gen(&conn), generation, "cut {nth}");
        assert_eq!(
            meta_integer(&conn, "index_inputs").unwrap(),
            inputs,
            "cut {nth}"
        );
        assert!(observe(
            &mut conn,
            &token,
            ObservedAvailability::Missing,
            11
        ));
    }
}

fn pause_if_current(
    conn: &mut Connection,
    fence: IndexFence,
    token: Option<&PendingDocument>,
    code: SearchIoCode,
    retry_at: i64,
    attempts: u32,
) -> bool {
    let tx = conn.transaction().unwrap();
    let applied = pause_for_io_if_current(
        &tx,
        fence,
        token,
        code,
        "sanitized cause",
        now(retry_at),
        attempts,
    )
    .unwrap();
    tx.commit().unwrap();
    applied
}

#[test]
fn the_conditional_io_pause_writes_only_while_the_failure_is_current() {
    let mut conn = connection();
    indexed(&mut conn, 1, "oldtoken");
    let tx = conn.transaction().unwrap();
    enqueue(&tx, &[(1, IndexReason::Reextract)], now(10)).unwrap();
    tx.commit().unwrap();
    let token = pending_token(&conn, 1);
    let fence = pending_batch(&conn, 200).unwrap().fence;
    let generation = search_gen(&conn);
    let list_generation = list_gen(&conn);

    // A current per-document failure records code, message, deadline
    // and history in one generation; list generation is untouched.
    assert!(pause_if_current(
        &mut conn,
        fence,
        Some(&token),
        SearchIoCode::StorageFull,
        50,
        1
    ));
    let state = status(&conn).unwrap();
    let held = state.io_pause().unwrap();
    assert_eq!(held.code(), SearchIoCode::StorageFull);
    assert_eq!(held.message(), "sanitized cause");
    assert_eq!(held.retry_at(), now(50));
    assert_eq!(state.io_attempts(), 1);
    assert_eq!(search_gen(&conn), generation + 1);
    assert_eq!(list_gen(&conn), list_generation);

    // A recorded hold protects the newer status from being overwritten.
    let generation = search_gen(&conn);
    assert!(!pause_if_current(
        &mut conn,
        fence,
        Some(&token),
        SearchIoCode::OtherIo,
        60,
        2
    ));
    let tx = conn.transaction().unwrap();
    clear_io_pause(&tx).unwrap();
    tx.commit().unwrap();
    assert_eq!(search_gen(&conn), generation + 1);

    // A superseded token, a stale epoch or target, and a user hold all
    // write nothing.
    let tx = conn.transaction().unwrap();
    enqueue(&tx, &[(1, IndexReason::Reextract)], now(20)).unwrap();
    tx.commit().unwrap();
    let current = pending_token(&conn, 1);
    let mut stale_epoch = fence;
    stale_epoch.epoch += 1;
    let mut stale_target = fence;
    stale_target.version += 1;
    for (fence, token) in [
        (fence, Some(&token)),
        (stale_epoch, Some(&current)),
        (stale_target, Some(&current)),
    ] {
        let generation = search_gen(&conn);
        assert!(!pause_if_current(
            &mut conn,
            fence,
            token,
            SearchIoCode::OtherIo,
            60,
            2
        ));
        assert_eq!(search_gen(&conn), generation);
        assert!(status(&conn).unwrap().io_pause().is_none());
    }
    pause(&mut conn, UserPause::Pause);
    assert!(!pause_if_current(
        &mut conn,
        fence,
        Some(&current),
        SearchIoCode::OtherIo,
        60,
        2
    ));
    pause(&mut conn, UserPause::Resume);
    assert!(pause_if_current(
        &mut conn,
        fence,
        Some(&current),
        SearchIoCode::OtherIo,
        60,
        2
    ));
}

#[test]
fn document_failures_need_a_publication_phase_and_phase_failures_do_not() {
    let mut conn = connection();
    acquisitions(&conn, 1);
    let tx = conn.transaction().unwrap();
    enqueue(&tx, &[(1, IndexReason::New)], now(10)).unwrap();
    tx.commit().unwrap();
    let token = pending_token(&conn, 1);
    start(&mut conn, "job", 1);
    let fence = pending_batch(&conn, 200).unwrap().fence;

    // Reset and enqueue cannot publish, so a per-document failure is
    // blocked there; a phase-level failure is exactly where it
    // happens and must be recorded.
    for name in ["reset", "enqueue"] {
        phase(&conn, "job", name);
        assert!(!pause_if_current(
            &mut conn,
            fence,
            Some(&token),
            SearchIoCode::StorageUnavailable,
            50,
            1
        ));
        assert!(status(&conn).unwrap().io_pause().is_none());
        assert!(pause_if_current(
            &mut conn,
            fence,
            None,
            SearchIoCode::StorageUnavailable,
            50,
            1
        ));
        let state = status(&conn).unwrap();
        assert_eq!(
            state.io_pause().unwrap().code(),
            SearchIoCode::StorageUnavailable
        );
        let tx = conn.transaction().unwrap();
        clear_io_pause(&tx).unwrap();
        tx.commit().unwrap();
    }

    // Index is the publication phase; the document failure records.
    phase(&conn, "job", "index");
    assert!(pause_if_current(
        &mut conn,
        fence,
        Some(&token),
        SearchIoCode::StorageUnavailable,
        50,
        1
    ));
}

#[test]
fn pause_inputs_are_validated_before_any_write() {
    let mut conn = connection();
    acquisitions(&conn, 1);
    let tx = conn.transaction().unwrap();
    enqueue(&tx, &[(1, IndexReason::New)], now(10)).unwrap();
    tx.commit().unwrap();
    let token = pending_token(&conn, 1);
    let fence = pending_batch(&conn, 200).unwrap().fence;
    let generation = search_gen(&conn);
    let tx = conn.transaction().unwrap();
    assert!(matches!(
        pause_for_io_if_current(
            &tx,
            fence,
            Some(&token),
            SearchIoCode::OtherIo,
            &"é".repeat(300),
            now(10),
            1
        ),
        Err(StoreError::InvalidInput { .. })
    ));
    assert!(matches!(
        pause_for_io_if_current(
            &tx,
            fence,
            Some(&token),
            SearchIoCode::OtherIo,
            "m",
            now(-1),
            1
        ),
        Err(StoreError::InvalidInput { .. })
    ));
    // The 512-byte bound is inclusive.
    assert!(
        pause_for_io_if_current(
            &tx,
            fence,
            Some(&token),
            SearchIoCode::OtherIo,
            &"x".repeat(MAX_IO_MESSAGE_BYTES),
            now(10),
            1
        )
        .unwrap()
    );
    tx.commit().unwrap();
    assert_eq!(search_gen(&conn), generation + 1);
}

// One cut before each of: token recheck, phase check, status update,
// generation.
const PAUSE_CUTS: u64 = 4;

#[test]
fn operation_failures_roll_back_the_whole_conditional_pause() {
    for nth in 1..=PAUSE_CUTS {
        let mut conn = connection();
        acquisitions(&conn, 1);
        let tx = conn.transaction().unwrap();
        enqueue(&tx, &[(1, IndexReason::New)], now(10)).unwrap();
        tx.commit().unwrap();
        let token = pending_token(&conn, 1);
        let fence = pending_batch(&conn, 200).unwrap().fence;
        let generation = search_gen(&conn);
        let mut calls = 0;
        let mut cut = || {
            calls += 1;
            if calls == nth {
                return Err(StoreError::unavailable("injected pause fault"));
            }
            Ok(())
        };
        let tx = conn.transaction().unwrap();
        assert!(
            pause_if_current_with_cut(
                &tx,
                fence,
                Some(&token),
                IoPauseUpdate {
                    code: SearchIoCode::OtherIo,
                    message: "m",
                    retry_at: now(10),
                    attempts: 1,
                },
                &mut cut
            )
            .is_err(),
            "cut {nth} was not exercised"
        );
        tx.rollback().unwrap();
        let state = status(&conn).unwrap();
        assert!(state.io_pause().is_none(), "cut {nth}");
        assert_eq!(state.io_attempts(), 0, "cut {nth}");
        assert_eq!(search_gen(&conn), generation, "cut {nth}");
        assert!(pause_if_current(
            &mut conn,
            fence,
            Some(&token),
            SearchIoCode::OtherIo,
            10,
            1
        ));
    }
}

#[test]
fn generation_sql_errors_roll_back_source_and_pause_updates() {
    let mut conn = connection();
    acquisitions(&conn, 1);
    let tx = conn.transaction().unwrap();
    enqueue(&tx, &[(1, IndexReason::New)], now(10)).unwrap();
    tx.commit().unwrap();
    let token = pending_token(&conn, 1);
    let fence = pending_batch(&conn, 200).unwrap().fence;
    let before = snapshot(&conn);
    let inputs = meta_integer(&conn, "index_inputs").unwrap();
    conn.execute_batch(
        "CREATE TEMP TRIGGER reject_control_generation BEFORE UPDATE OF value ON store_meta
         WHEN OLD.key = 'search_generation'
         BEGIN SELECT RAISE(ABORT, 'injected generation SQL failure'); END;",
    )
    .unwrap();
    let tx = conn.transaction().unwrap();
    assert!(
        note_observed_availability(&tx, &token, ObservedAvailability::Missing, now(11)).is_err()
    );
    tx.rollback().unwrap();
    assert_eq!(object_state(&conn, 1), "published");
    assert_eq!(meta_integer(&conn, "index_inputs").unwrap(), inputs);
    assert_eq!(snapshot(&conn), before);
    let tx = conn.transaction().unwrap();
    assert!(
        pause_for_io_if_current(
            &tx,
            fence,
            Some(&token),
            SearchIoCode::OtherIo,
            "cause",
            now(20),
            1
        )
        .is_err()
    );
    tx.rollback().unwrap();
    assert_eq!(snapshot(&conn), before);
}

#[test]
fn retry_request_moves_the_deadline_and_keeps_error_history_and_user_hold() {
    let mut conn = connection();
    let fence = pending_batch(&conn, 200).unwrap().fence;
    let generation = search_gen(&conn);
    // No I/O pause: a request is a quiet no-op, as is a begin.
    let tx = conn.transaction().unwrap();
    assert!(!request_index_retry(&tx, now(1)).unwrap());
    assert!(!begin_index_retry(&tx, fence, now(1)).unwrap());
    tx.commit().unwrap();
    assert_eq!(search_gen(&conn), generation);

    io_pause_now(&mut conn, SearchIoCode::StorageFull, 100, 3);
    pause(&mut conn, UserPause::Pause);
    let generation = search_gen(&conn);
    let list_generation = list_gen(&conn);
    // The request brings the deadline to now under both holds; the
    // error, history and user hold are all retained.
    let tx = conn.transaction().unwrap();
    assert!(request_index_retry(&tx, now(40)).unwrap());
    tx.commit().unwrap();
    let state = status(&conn).unwrap();
    let held = state.io_pause().unwrap();
    assert_eq!(held.code(), SearchIoCode::StorageFull);
    assert_eq!(held.message(), "sanitized cause");
    assert_eq!(held.retry_at(), now(40));
    assert_eq!(state.io_attempts(), 3);
    assert!(state.user_paused());
    assert_eq!(search_gen(&conn), generation + 1);
    assert_eq!(list_gen(&conn), list_generation);

    // Begin is refused while the user holds even with the deadline
    // due, and negative times are rejected before any write; nothing
    // changes in either case.
    let tx = conn.transaction().unwrap();
    assert!(!begin_index_retry(&tx, fence, now(60)).unwrap());
    assert!(matches!(
        request_index_retry(&tx, now(-1)),
        Err(StoreError::InvalidInput { .. })
    ));
    assert!(matches!(
        begin_index_retry(&tx, fence, now(-1)),
        Err(StoreError::InvalidInput { .. })
    ));
    tx.commit().unwrap();
    let state = status(&conn).unwrap();
    assert_eq!(state.io_pause().unwrap().retry_at(), now(40));
    assert_eq!(search_gen(&conn), generation + 1);
}

#[test]
fn begin_is_refused_while_user_held_not_due_or_stale_and_due_begins_once() {
    let mut conn = connection();
    io_pause_now(&mut conn, SearchIoCode::OtherIo, 50, 3);
    pause(&mut conn, UserPause::Pause);
    let fence = pending_batch(&conn, 200).unwrap().fence;
    let generation = search_gen(&conn);
    // User-held first, then unpaused but early, then a stale fence.
    let mut stale = fence;
    stale.epoch += 1;
    let tx = conn.transaction().unwrap();
    assert!(!begin_index_retry(&tx, fence, now(60)).unwrap());
    tx.commit().unwrap();
    pause(&mut conn, UserPause::Resume);
    let tx = conn.transaction().unwrap();
    assert!(!begin_index_retry(&tx, fence, now(49)).unwrap());
    assert!(!begin_index_retry(&tx, stale, now(60)).unwrap());
    tx.commit().unwrap();
    let state = status(&conn).unwrap();
    assert_eq!(state.io_pause().unwrap().retry_at(), now(50));
    assert_eq!(search_gen(&conn), generation + 1, "only Resume advanced it");

    // Due under the current fence: code/message/deadline clear, the
    // persisted attempt count stays, generation advances once.
    let tx = conn.transaction().unwrap();
    assert!(begin_index_retry(&tx, fence, now(50)).unwrap());
    tx.commit().unwrap();
    let state = status(&conn).unwrap();
    assert!(state.io_pause().is_none());
    assert_eq!(state.io_attempts(), 3);
    assert_eq!(search_gen(&conn), generation + 2);
    // With no pause left there is nothing to begin.
    let tx = conn.transaction().unwrap();
    assert!(!begin_index_retry(&tx, fence, now(60)).unwrap());
    tx.commit().unwrap();
}

#[test]
fn retry_begin_is_phase_agnostic_across_reset_and_enqueue() {
    for name in ["reset", "enqueue"] {
        let mut conn = connection();
        start(&mut conn, "job", 1);
        phase(&conn, "job", name);
        io_pause_now(&mut conn, SearchIoCode::StorageUnavailable, 10, 1);
        let fence = pending_batch(&conn, 200).unwrap().fence;
        let tx = conn.transaction().unwrap();
        assert!(begin_index_retry(&tx, fence, now(10)).unwrap());
        tx.commit().unwrap();
        assert!(status(&conn).unwrap().io_pause().is_none());
        let expected = if name == "reset" {
            RebuildPhase::Reset
        } else {
            RebuildPhase::Enqueue
        };
        assert_eq!(job_at(&conn, "job").phase, expected);
    }
}

#[test]
fn success_resets_only_nonzero_history_under_a_current_unpaused_fence() {
    let mut conn = connection();
    let fence = pending_batch(&conn, 200).unwrap().fence;
    // Nothing recorded yet: a no-op, not an error.
    let tx = conn.transaction().unwrap();
    assert!(!record_index_attempt_success(&tx, fence).unwrap());
    tx.commit().unwrap();

    // Persist history, then let a due begin clear the pause: the
    // attempts survive as the only record.
    io_pause_now(&mut conn, SearchIoCode::StorageFull, 10, 4);
    let tx = conn.transaction().unwrap();
    assert!(begin_index_retry(&tx, fence, now(10)).unwrap());
    tx.commit().unwrap();
    assert_eq!(status(&conn).unwrap().io_attempts(), 4);
    let generation = search_gen(&conn);
    let mut stale = fence;
    stale.version += 1;
    let tx = conn.transaction().unwrap();
    assert!(!record_index_attempt_success(&tx, stale).unwrap());
    tx.commit().unwrap();
    let tx = conn.transaction().unwrap();
    assert!(record_index_attempt_success(&tx, fence).unwrap());
    tx.commit().unwrap();
    assert_eq!(status(&conn).unwrap().io_attempts(), 0);
    assert_eq!(search_gen(&conn), generation + 1);
    // History already clear: a quiet no-op.
    let tx = conn.transaction().unwrap();
    assert!(!record_index_attempt_success(&tx, fence).unwrap());
    tx.commit().unwrap();

    // Either hold blocks the reset; success is recorded only for a
    // real, unpaused attempt.
    io_pause_now(&mut conn, SearchIoCode::OtherIo, 10, 2);
    let tx = conn.transaction().unwrap();
    assert!(!record_index_attempt_success(&tx, fence).unwrap());
    tx.commit().unwrap();
    assert_eq!(status(&conn).unwrap().io_attempts(), 2);
    pause(&mut conn, UserPause::Pause);
    let tx = conn.transaction().unwrap();
    assert!(!record_index_attempt_success(&tx, fence).unwrap());
    tx.commit().unwrap();
}

#[test]
fn recovery_republishes_status_once_or_fails_on_missing_corrupt_state() {
    let mut conn = connection();
    acquisitions(&conn, 2);
    start(&mut conn, "job", 1);
    phase(&conn, "job", "enqueue");
    conn.execute(
        "UPDATE search_rebuild_jobs SET doc_cursor = 3, acq_cursor = 4, phase_count = 5 WHERE job_id = 'job'",
        [],
    )
    .unwrap();
    io_pause_now(&mut conn, SearchIoCode::PermissionDenied, 30, 2);
    pause(&mut conn, UserPause::Pause);
    let generation = search_gen(&conn);
    let list_generation = list_gen(&conn);
    let state = format!("{:?}", status(&conn).unwrap());
    let job = format!("{:?}", job_at(&conn, "job"));

    // The commit is allowed while held: it clears no hold, changes no
    // field, and advances search generation exactly once.
    let tx = conn.transaction().unwrap();
    commit_index_recovery(&tx).unwrap();
    tx.commit().unwrap();
    assert_eq!(search_gen(&conn), generation + 1);
    assert_eq!(list_gen(&conn), list_generation);
    assert_eq!(format!("{:?}", status(&conn).unwrap()), state);
    assert_eq!(format!("{:?}", job_at(&conn, "job")), job);

    // Missing or unparseable operational state is an explicit failure,
    // never false successful progress.
    conn.execute("DELETE FROM search_index_state", []).unwrap();
    let tx = conn.transaction().unwrap();
    assert!(matches!(
        commit_index_recovery(&tx),
        Err(StoreError::Corrupt { .. })
    ));
    tx.rollback().unwrap();
    conn.execute(
        "INSERT INTO search_index_state (id, epoch) VALUES (0, 0)",
        [],
    )
    .unwrap();
    conn.execute(
        "UPDATE store_meta SET value = 'x' WHERE key = 'search_extractor_version'",
        [],
    )
    .unwrap();
    let tx = conn.transaction().unwrap();
    assert!(matches!(
        commit_index_recovery(&tx),
        Err(StoreError::Corrupt { .. })
    ));
    tx.rollback().unwrap();
    assert_eq!(search_gen(&conn), generation + 1);
}
