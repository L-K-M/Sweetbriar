//! State and publication-fence acceptance. Phase driving belongs to M1.6d3.

mod batches;
mod controls;

use rusqlite::{Connection, params};
use sb_test_support::real_fs;

use super::*;
use crate::imports::{create_import_job, import_job};
use crate::index::{
    IndexDocument, IndexPublication, IndexReason, IndexState, enqueue, pending_batch,
    publish_batch, search_generation,
};
use crate::schema::{MIGRATIONS, run_migrations};
use crate::testing::fixtures::{
    acquisitions, document, index_connection as connection,
    index_connection_through as connection_through,
};
use crate::testing::{scratch_paths, test_seams, writing};

fn now(ms: i64) -> Timestamp {
    Timestamp::from_millis(ms)
}

fn drain(conn: &mut Connection, text: &str) -> usize {
    let mut total = 0;
    loop {
        let captured = pending_batch(conn, 200).unwrap();
        if captured.documents.is_empty() {
            return total;
        }
        let publications: Vec<_> = captured
            .documents
            .into_iter()
            .map(|pending| IndexPublication {
                document: if pending.reason == IndexReason::Delete {
                    None
                } else {
                    Some(IndexDocument {
                        extractor_version: captured.fence.version,
                        ..document(text)
                    })
                },
                pending,
            })
            .collect();
        let tx = conn.transaction().unwrap();
        total += publish_batch(&tx, captured.fence, &publications).unwrap();
        tx.commit().unwrap();
    }
}

fn indexed(conn: &mut Connection, count: usize, text: &str) {
    acquisitions(conn, count);
    let requests: Vec<_> = (1..=count as i64)
        .map(|id| (id, IndexReason::New))
        .collect();
    for page in requests.chunks(200) {
        let tx = conn.transaction().unwrap();
        enqueue(&tx, page, now(1)).unwrap();
        tx.commit().unwrap();
    }
    assert_eq!(drain(conn, text), count);
}

fn start(conn: &mut Connection, id: &str, target: u32) -> String {
    let tx = conn.transaction().unwrap();
    let job = start_search_rebuild(&tx, id, target).unwrap();
    tx.commit().unwrap();
    job
}

fn pause(conn: &mut Connection, pause: UserPause) {
    let tx = conn.transaction().unwrap();
    set_user_pause(&tx, pause).unwrap();
    tx.commit().unwrap();
}

fn job_at(conn: &Connection, id: &str) -> RebuildJob {
    rebuild_job(conn, id).unwrap()
}

// The state-only slice tests each writer fence from a persisted phase fixture.
// M1.6d3 separately drives real phase transitions and crash replay.
fn phase(conn: &Connection, id: &str, phase: &str) {
    conn.execute(
        "UPDATE search_rebuild_jobs SET phase = ?2 WHERE job_id = ?1",
        params![id, phase],
    )
    .unwrap();
}

fn hits(conn: &Connection, query: &str) -> Vec<i64> {
    conn.prepare("SELECT rowid FROM d_search_fts WHERE d_search_fts MATCH ?1 ORDER BY rowid")
        .unwrap()
        .query_map([query], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn snapshot(conn: &Connection) -> String {
    let jobs: Vec<String> = conn
        .prepare(
            "SELECT job_id || ':' || epoch || ':' || phase || ':' || doc_cursor || ':' ||
           acq_cursor || ':' || phase_count FROM search_rebuild_jobs ORDER BY epoch",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let queue = pending_batch(conn, 200).unwrap().documents;
    let docs: i64 = conn
        .query_row("SELECT count(*) FROM d_search_docs", [], |r| r.get(0))
        .unwrap();
    let state = status(conn).unwrap();
    format!(
        "{jobs:?}/{queue:?}/{docs}/{:?}/{:?}/{}/{}/{}/{:?}/{}",
        hits(conn, "oldtoken"),
        hits(conn, "newtoken"),
        state.epoch,
        state.user_paused,
        search_generation(conn).unwrap(),
        state.io_pause,
        state.io_attempts
    )
}

#[test]
fn migration_6_preserves_imports_queue_revisions_and_documents() {
    let mut conn = connection_through(5);
    acquisitions(&conn, 2);
    for id in 1..=2i64 {
        conn.execute("INSERT INTO d_search_docs (doc_id, extractor_version, state, truncated_fields, subject)
                      VALUES (?1, 1, 'indexed', '[]', 'oldtoken')", [id]).unwrap();
        conn.execute(
            "INSERT INTO d_search_fts (rowid, subject) VALUES (?1, 'oldtoken')",
            [id],
        )
        .unwrap();
    }
    let tx = conn.transaction().unwrap();
    enqueue(&tx, &[(2, IndexReason::Reextract)], now(7)).unwrap();
    create_import_job(&tx, "import-1", "{\"v\":1,\"paths\":[]}", now(7)).unwrap();
    tx.commit().unwrap();
    let revision: i64 = conn
        .query_row(
            "SELECT revision FROM d_index_queue WHERE doc_id = 2",
            [],
            |r| r.get(0),
        )
        .unwrap();
    run_migrations(&mut conn, &test_seams(real_fs()), MIGRATIONS).unwrap();
    assert_eq!(hits(&conn, "oldtoken"), [1, 2]);
    let pending = pending_batch(&conn, 200).unwrap().documents;
    assert_eq!(pending.len(), 1);
    assert_eq!(pending[0].revision, revision);
    let tx = conn.transaction().unwrap();
    assert_eq!(import_job(&tx, "import-1").unwrap().id(), "import-1");
    drop(tx);
    let state = status(&conn).unwrap();
    assert_eq!(state.epoch, 0);
    assert!(!state.user_paused);
    assert!(state.io_pause.is_none());
    assert!(latest_rebuild(&conn).unwrap().is_none());
}

#[test]
fn cross_kind_ids_are_rejected_and_dispatch_reads_the_kind() {
    let mut conn = connection();
    let tx = conn.transaction().unwrap();
    create_import_job(&tx, "import-1", "{\"v\":1}", now(1)).unwrap();
    tx.commit().unwrap();
    start(&mut conn, "rebuild-1", 1);
    assert_eq!(job_kind(&conn, "import-1").unwrap(), JobKind::Import);
    assert_eq!(
        job_kind(&conn, "rebuild-1").unwrap(),
        JobKind::SearchRebuild
    );
    assert!(matches!(
        job_kind(&conn, "nothing"),
        Err(StoreError::NotFound { .. })
    ));
    assert!(matches!(
        rebuild_job(&conn, "import-1"),
        Err(StoreError::InvalidInput { .. })
    ));
    assert!(matches!(
        rebuild_job(&conn, "nothing"),
        Err(StoreError::NotFound { .. })
    ));
    for sql in [
        "INSERT INTO search_rebuild_jobs (job_id, epoch, phase) VALUES ('import-1', 5, 'complete')",
        "INSERT INTO import_jobs (job_id, state, request_json, checkpoint_json, created_at, updated_at)
         VALUES ('rebuild-1', 'running', '{}', '{}', 0, 0)",
        "INSERT INTO search_rebuild_jobs (job_id, epoch, phase) VALUES ('rebuild-2', 6, 'index')",
        "INSERT INTO search_rebuild_jobs (job_id, epoch, phase) VALUES ('rebuild-3', 1, 'complete')",
    ] { assert!(conn.execute(sql, []).is_err(), "{sql}"); }
    phase(&conn, "rebuild-1", "complete");
    conn.execute(
        "INSERT INTO search_rebuild_jobs (job_id, epoch, phase) VALUES ('rebuild-2', 6, 'index')",
        [],
    )
    .unwrap();
    assert_eq!(
        job_kind(&conn, "rebuild-2").unwrap(),
        JobKind::SearchRebuild
    );
}

#[test]
fn schema_keeps_pause_fields_paired_and_byte_bounded() {
    let conn = connection();
    for sql in [
        "INSERT INTO search_index_state (id, epoch) VALUES (1, 0)",
        "UPDATE search_index_state SET epoch=-1",
        "UPDATE search_index_state SET user_paused=2",
        "UPDATE search_index_state SET io_attempts=-1",
        "UPDATE search_index_state SET io_code='storage_full'",
        "UPDATE search_index_state SET io_code='other_io', io_message='m', io_retry_at=-1",
    ] {
        assert!(conn.execute(sql, []).is_err(), "{sql}");
    }
    assert!(
        conn.execute(
            "UPDATE search_index_state SET io_code='other_io', io_message=?1, io_retry_at=1",
            ["é".repeat(300)],
        )
        .is_err(),
        "the SQL message bound must count UTF-8 bytes, like pause_for_io"
    );
}

#[test]
fn job_kind_guards_also_reject_colliding_id_updates() {
    for sql in [
        "UPDATE import_jobs SET job_id='rebuild-1' WHERE job_id='import-1'",
        "UPDATE search_rebuild_jobs SET job_id='import-1' WHERE job_id='rebuild-1'",
    ] {
        let mut conn = connection();
        let tx = conn.transaction().unwrap();
        create_import_job(&tx, "import-1", "{\"v\":1}", now(1)).unwrap();
        tx.commit().unwrap();
        start(&mut conn, "rebuild-1", 1);
        assert!(conn.execute(sql, []).is_err(), "{sql}");
        assert_eq!(job_kind(&conn, "import-1").unwrap(), JobKind::Import);
        assert_eq!(
            job_kind(&conn, "rebuild-1").unwrap(),
            JobKind::SearchRebuild
        );
    }
}

#[test]
fn start_is_idempotent_singleton_and_completed_jobs_keep_their_ids() {
    let mut conn = connection();
    indexed(&mut conn, 2, "oldtoken");
    assert_eq!(start(&mut conn, "a", 1), "a");
    conn.execute(
        "UPDATE search_rebuild_jobs SET phase='index', doc_cursor=2, acq_cursor=2, phase_count=7",
        [],
    )
    .unwrap();
    pause(&mut conn, UserPause::Pause);
    let before = snapshot(&conn);
    assert_eq!(start(&mut conn, "b", 1), "a");
    assert_eq!(snapshot(&conn), before);
    assert_eq!(start(&mut conn, "c", 2), "a");
    let job = job_at(&conn, "a");
    assert_eq!(job.epoch, 2);
    assert_eq!(job.phase, RebuildPhase::Reset);
    assert_eq!((job.doc_cursor, job.acq_cursor, job.phase_count), (0, 0, 0));
    assert_eq!(target_version(&conn).unwrap(), 2);
    assert!(status(&conn).unwrap().user_paused());
    assert_eq!(hits(&conn, "oldtoken"), [1, 2]);
    phase(&conn, "a", "complete");
    assert_eq!(start(&mut conn, "d", 2), "d");
    assert_eq!(job_at(&conn, "a").phase, RebuildPhase::Complete);
    assert_eq!(job_at(&conn, "d").epoch, 3);
    assert_eq!(latest_rebuild(&conn).unwrap().unwrap().id(), "d");
    phase(&conn, "d", "complete");
    let before = snapshot(&conn);
    let tx = conn.transaction().unwrap();
    assert!(matches!(
        start_search_rebuild(&tx, "a", 3),
        Err(StoreError::InvalidInput { .. })
    ));
    tx.rollback().unwrap();
    assert_eq!(snapshot(&conn), before);
}

#[test]
fn a_start_never_downgrades_the_stored_target() {
    let mut conn = connection();
    start(&mut conn, "a", 2);
    for id in ["b", "c"] {
        assert!(matches!(
            start_search_rebuild(&conn.transaction().unwrap(), id, 1),
            Err(StoreError::InvalidInput { .. })
        ));
    }
    assert_eq!(target_version(&conn).unwrap(), 2);
}

#[test]
fn stale_epochs_and_current_reset_enqueue_phases_reject_real_publications() {
    let mut conn = connection();
    indexed(&mut conn, 2, "oldtoken");
    let tx = conn.transaction().unwrap();
    enqueue(&tx, &[(1, IndexReason::Reextract)], now(3)).unwrap();
    tx.commit().unwrap();
    let old = pending_batch(&conn, 200).unwrap();
    start(&mut conn, "job", 1);
    let current = pending_batch(&conn, 200).unwrap();
    let publications: Vec<_> = current
        .documents
        .into_iter()
        .map(|pending| IndexPublication {
            pending,
            document: Some(document("newtoken")),
        })
        .collect();
    for name in ["reset", "enqueue", "index", "complete"] {
        phase(&conn, "job", name);
        let before = snapshot(&conn);
        let tx = conn.transaction().unwrap();
        assert_eq!(publish_batch(&tx, old.fence, &publications).unwrap(), 0);
        if name == "reset" || name == "enqueue" {
            assert_eq!(publish_batch(&tx, current.fence, &publications).unwrap(), 0);
        }
        tx.commit().unwrap();
        assert_eq!(snapshot(&conn), before);
    }
    phase(&conn, "job", "index");
    assert_eq!(drain(&mut conn, "newtoken"), 1);
    assert_eq!(hits(&conn, "newtoken"), [1]);
    assert_eq!(hits(&conn, "oldtoken"), [2]);
}

#[test]
fn both_pauses_gate_nonempty_index_publications_and_remain_independent() {
    let mut conn = connection();
    acquisitions(&conn, 1);
    let tx = conn.transaction().unwrap();
    enqueue(&tx, &[(1, IndexReason::New)], now(1)).unwrap();
    tx.commit().unwrap();
    start(&mut conn, "job", 1);
    phase(&conn, "job", "index");
    let captured = pending_batch(&conn, 200).unwrap();
    let publications = vec![IndexPublication {
        pending: captured.documents[0].clone(),
        document: Some(document("newtoken")),
    }];
    for user in [true, false] {
        if user {
            pause(&mut conn, UserPause::Pause);
        } else {
            let tx = conn.transaction().unwrap();
            pause_for_io(&tx, SearchIoCode::StorageFull, "disk full", now(9), 2).unwrap();
            tx.commit().unwrap();
        }
        let before = snapshot(&conn);
        let tx = conn.transaction().unwrap();
        assert_eq!(
            publish_batch(&tx, captured.fence, &publications).unwrap(),
            0
        );
        tx.commit().unwrap();
        assert_eq!(snapshot(&conn), before);
        if user {
            pause(&mut conn, UserPause::Resume);
        }
    }
    pause(&mut conn, UserPause::Pause);
    pause(&mut conn, UserPause::Resume);
    assert!(status(&conn).unwrap().io_pause().is_some());
    let tx = conn.transaction().unwrap();
    assert!(pause_for_io(&tx, SearchIoCode::OtherIo, &"x".repeat(513), now(1), 1).is_err());
    assert!(pause_for_io(&tx, SearchIoCode::OtherIo, "m", now(-1), 1).is_err());
    clear_io_pause(&tx).unwrap();
    tx.commit().unwrap();
    assert_eq!(drain(&mut conn, "newtoken"), 1);
}

#[test]
fn user_and_io_pause_survive_reopen_enqueue_and_version_restart() {
    let (_scratch, paths) = scratch_paths("rebuild-pause");
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    writing(&store, |tx| {
        acquisitions(tx, 1);
        start_search_rebuild(tx, "job", 1)?;
        set_user_pause(tx, UserPause::Pause)?;
        pause_for_io(
            tx,
            SearchIoCode::StorageUnavailable,
            "storage unavailable",
            now(20),
            3,
        )
    });
    store.close().unwrap();
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    writing(&store, |tx| enqueue(tx, &[(1, IndexReason::New)], now(5)));
    writing(&store, |tx| {
        assert!(search_index_status(tx)?.user_paused());
        assert_eq!(start_search_rebuild(tx, "other", 2)?, "job");
        assert!(search_index_status(tx)?.user_paused());
        assert_eq!(search_index_status(tx)?.io_attempts(), 3);
        set_user_pause(tx, UserPause::Resume)?;
        assert!(search_index_status(tx)?.io_pause().is_some());
        assert_eq!(rebuild_job(tx, "job")?.phase, RebuildPhase::Reset);
        clear_io_pause(tx)?;
        Ok(())
    });
    store.close().unwrap();
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    writing(&store, |tx| {
        let state = search_index_status(tx)?;
        assert!(state.io_pause().is_none());
        assert_eq!(state.io_attempts(), 3);
        Ok(())
    });
    store.close().unwrap();
}

#[test]
fn a_missing_state_row_cannot_report_successful_pause_or_retry_progress() {
    let mut conn = connection();
    conn.execute("DELETE FROM search_index_state", []).unwrap();
    let generation = search_generation(&conn).unwrap();
    let tx = conn.transaction().unwrap();
    let outcomes = [
        set_user_pause(&tx, UserPause::Pause),
        pause_for_io(&tx, SearchIoCode::StorageFull, "full", now(10), 1),
        clear_io_pause(&tx),
    ];
    tx.commit().unwrap();
    assert!(
        outcomes
            .iter()
            .all(|result| matches!(result, Err(StoreError::Corrupt { .. })))
    );
    assert_eq!(search_generation(&conn).unwrap(), generation);
}

#[test]
fn status_writes_advance_generation_once_and_stale_work_does_not() {
    let mut conn = connection();
    acquisitions(&conn, 1);
    assert_eq!(search_generation(&conn).unwrap(), 1);
    start(&mut conn, "job", 1);
    assert_eq!(search_generation(&conn).unwrap(), 2);
    pause(&mut conn, UserPause::Pause);
    let tx = conn.transaction().unwrap();
    pause_for_io(&tx, SearchIoCode::PermissionDenied, "denied", now(4), 1).unwrap();
    tx.commit().unwrap();
    let tx = conn.transaction().unwrap();
    clear_io_pause(&tx).unwrap();
    tx.commit().unwrap();
    assert_eq!(search_generation(&conn).unwrap(), 5);
    pause(&mut conn, UserPause::Resume);
    let before = snapshot(&conn);
    let mut stale = pending_batch(&conn, 200).unwrap().fence;
    stale.epoch += 9;
    let tx = conn.transaction().unwrap();
    assert_eq!(publish_batch(&tx, stale, &[]).unwrap(), 0);
    tx.commit().unwrap();
    assert_eq!(snapshot(&conn), before);
}
