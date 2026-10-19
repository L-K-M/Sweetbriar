//! Real phase transitions and process-crash replay for M1.6d3.

use std::path::Path;

use sb_test_support::{CutFs, ScratchDir, run_crash_cuts};

use super::super::batches::{enqueue_batch_with_cut, reset_batch_with_cut};
use super::*;
use crate::Fs;

fn reset(conn: &mut Connection, id: &str, epoch: i64, limit: usize) -> usize {
    let tx = conn.transaction().unwrap();
    let n = rebuild_reset_batch(&tx, id, epoch, limit).unwrap();
    tx.commit().unwrap();
    n
}

fn enqueue_phase(conn: &mut Connection, id: &str, epoch: i64, limit: usize) -> usize {
    let tx = conn.transaction().unwrap();
    let n = rebuild_enqueue_batch(&tx, id, epoch, limit, now(2)).unwrap();
    tx.commit().unwrap();
    n
}

fn complete(conn: &mut Connection, id: &str, epoch: i64) -> bool {
    let tx = conn.transaction().unwrap();
    let done = complete_rebuild(&tx, id, epoch).unwrap();
    tx.commit().unwrap();
    done
}

#[test]
fn zero_sized_phase_batches_are_rejected_without_progress() {
    let mut conn = connection();
    start(&mut conn, "job", 1);
    let before = snapshot(&conn);
    let tx = conn.transaction().unwrap();
    assert!(rebuild_reset_batch(&tx, "job", 1, 0).is_err());
    assert!(rebuild_enqueue_batch(&tx, "job", 1, 0, now(2)).is_err());
    tx.rollback().unwrap();
    assert_eq!(snapshot(&conn), before);
}

#[test]
fn a_201_document_rebuild_uses_two_bounded_batches_in_each_scan_phase() {
    let mut conn = connection();
    indexed(&mut conn, 201, "oldtoken");
    start(&mut conn, "job", 1);
    assert_eq!(reset(&mut conn, "job", 1, 200), 200);
    assert_eq!(job_at(&conn, "job").phase, RebuildPhase::Reset);
    assert_eq!(job_at(&conn, "job").phase_count, 200);
    assert_eq!(hits(&conn, "oldtoken"), [201]);
    assert_eq!(reset(&mut conn, "job", 1, 200), 1);
    assert_eq!(job_at(&conn, "job").phase, RebuildPhase::Enqueue);
    assert_eq!(enqueue_phase(&mut conn, "job", 1, 200), 200);
    assert_eq!(job_at(&conn, "job").phase, RebuildPhase::Enqueue);
    assert_eq!(job_at(&conn, "job").phase_count, 200);
    assert_eq!(enqueue_phase(&mut conn, "job", 1, 200), 1);
    assert_eq!(job_at(&conn, "job").phase, RebuildPhase::Index);
    assert_eq!(drain(&mut conn, "newtoken"), 201);
    assert_eq!(job_at(&conn, "job").phase_count, 201);
    assert!(complete(&mut conn, "job", 1));
    assert_eq!(hits(&conn, "newtoken").len(), 201);
    assert_eq!(
        conn.query_row("SELECT count(*) FROM acquisitions", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        201
    );
}

#[test]
fn index_progress_counts_only_still_current_publications() {
    let mut conn = connection();
    acquisitions(&conn, 2);
    start(&mut conn, "job", 1);
    reset(&mut conn, "job", 1, 200);
    enqueue_phase(&mut conn, "job", 1, 200);
    let captured = pending_batch(&conn, 200).unwrap();
    let publications: Vec<_> = captured
        .documents
        .into_iter()
        .map(|pending| IndexPublication {
            pending,
            document: Some(document("newtoken")),
        })
        .collect();
    let tx = conn.transaction().unwrap();
    enqueue(&tx, &[(1, IndexReason::Reextract)], now(9)).unwrap();
    tx.commit().unwrap();
    let tx = conn.transaction().unwrap();
    assert_eq!(
        publish_batch(&tx, captured.fence, &publications).unwrap(),
        1
    );
    tx.commit().unwrap();
    assert_eq!(job_at(&conn, "job").phase_count, 1);
    assert_eq!(drain(&mut conn, "newtoken"), 1);
    assert_eq!(job_at(&conn, "job").phase_count, 2);
    assert!(complete(&mut conn, "job", 1));
}

#[test]
fn index_progress_sql_failure_rolls_back_the_publication_and_acknowledgements() {
    let mut conn = connection();
    acquisitions(&conn, 2);
    start(&mut conn, "job", 1);
    reset(&mut conn, "job", 1, 200);
    enqueue_phase(&mut conn, "job", 1, 200);
    let captured = pending_batch(&conn, 200).unwrap();
    let publications: Vec<_> = captured
        .documents
        .into_iter()
        .map(|pending| IndexPublication {
            pending,
            document: Some(document("newtoken")),
        })
        .collect();
    conn.execute_batch(
        "CREATE TEMP TRIGGER reject_index_progress BEFORE UPDATE OF phase_count ON search_rebuild_jobs
         BEGIN SELECT RAISE(ABORT, 'injected checkpoint SQL failure'); END;",
    ).unwrap();
    let before = snapshot(&conn);
    let tx = conn.transaction().unwrap();
    assert!(publish_batch(&tx, captured.fence, &publications).is_err());
    tx.rollback().unwrap();
    assert_eq!(snapshot(&conn), before);
    conn.execute_batch("DROP TRIGGER reject_index_progress")
        .unwrap();
    assert_eq!(drain(&mut conn, "newtoken"), 2);
    assert_eq!(job_at(&conn, "job").phase_count, 2);
    assert!(complete(&mut conn, "job", 1));
}

#[test]
fn reset_deletes_indexed_postings_but_does_not_delete_nonexistent_failed_postings() {
    let mut conn = connection();
    acquisitions(&conn, 2);
    let tx = conn.transaction().unwrap();
    enqueue(&tx, &[(1, IndexReason::New), (2, IndexReason::New)], now(1)).unwrap();
    tx.commit().unwrap();
    let captured = pending_batch(&conn, 200).unwrap();
    let publications: Vec<_> = captured
        .documents
        .into_iter()
        .map(|pending| {
            let doc = if pending.doc_id == 1 {
                document("oldtoken")
            } else {
                IndexDocument {
                    state: IndexState::Failed,
                    ..document("")
                }
            };
            IndexPublication {
                pending,
                document: Some(doc),
            }
        })
        .collect();
    let tx = conn.transaction().unwrap();
    assert_eq!(
        publish_batch(&tx, captured.fence, &publications).unwrap(),
        2
    );
    tx.commit().unwrap();
    start(&mut conn, "job", 1);
    assert_eq!(reset(&mut conn, "job", 1, 200), 2);
    assert!(hits(&conn, "oldtoken").is_empty());
    conn.execute(
        "INSERT INTO d_search_fts(d_search_fts, rank) VALUES('integrity-check', 1)",
        [],
    )
    .unwrap();
}

#[test]
fn bounded_reset_and_enqueue_commit_progress_and_complete_atomically() {
    let mut conn = connection();
    indexed(&mut conn, 3, "oldtoken");
    start(&mut conn, "job", 1);
    let tx = conn.transaction().unwrap();
    assert!(rebuild_reset_batch(&tx, "job", 1, 201).is_err());
    assert!(rebuild_enqueue_batch(&tx, "job", 1, 201, now(1)).is_err());
    tx.rollback().unwrap();

    assert_eq!(reset(&mut conn, "job", 1, 2), 2);
    let job = job_at(&conn, "job");
    assert_eq!((job.doc_cursor, job.phase_count), (2, 2));
    assert_eq!(reset(&mut conn, "job", 1, 2), 1);
    let job = job_at(&conn, "job");
    assert_eq!(
        (job.phase, job.doc_cursor, job.phase_count),
        (RebuildPhase::Enqueue, 3, 0)
    );
    assert!(hits(&conn, "oldtoken").is_empty());
    assert_eq!(
        conn.query_row("SELECT count(*) FROM d_search_fts_docsize", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(enqueue_phase(&mut conn, "job", 1, 2), 2);
    let job = job_at(&conn, "job");
    assert_eq!(
        (job.phase, job.acq_cursor, job.phase_count),
        (RebuildPhase::Enqueue, 2, 2)
    );
    assert_eq!(enqueue_phase(&mut conn, "job", 1, 200), 1);
    assert_eq!(job_at(&conn, "job").phase, RebuildPhase::Index);
    let before = snapshot(&conn);
    assert!(!complete(&mut conn, "job", 1));
    assert_eq!(snapshot(&conn), before);
    assert_eq!(drain(&mut conn, "newtoken"), 3);
    assert_eq!(job_at(&conn, "job").phase_count, 3);
    assert!(complete(&mut conn, "job", 1));

    let tx = conn.transaction().unwrap();
    enqueue(&tx, &[(1, IndexReason::Reextract)], now(4)).unwrap();
    tx.commit().unwrap();
    assert_eq!(drain(&mut conn, "newtoken"), 1);
    assert_eq!(reset(&mut conn, "job", 2, 200), 0);
    assert!(!complete(&mut conn, "job", 1));
}

#[test]
fn enqueue_behind_the_scan_cursor_stays_pending() {
    let mut conn = connection();
    acquisitions(&conn, 4);
    conn.execute(
        "UPDATE acquisitions SET object_state='missing' WHERE acquisition_id=4",
        [],
    )
    .unwrap();
    start(&mut conn, "job", 1);
    reset(&mut conn, "job", 1, 200);
    assert_eq!(enqueue_phase(&mut conn, "job", 1, 2), 2);
    let old = pending_batch(&conn, 200).unwrap().documents;
    let old_first = old.iter().find(|p| p.doc_id == 1).unwrap().revision;

    // Availability changes behind the cursor; another request arrives ahead of it.
    let tx = conn.transaction().unwrap();
    tx.execute(
        "UPDATE acquisitions SET object_state='missing' WHERE acquisition_id=1",
        [],
    )
    .unwrap();
    enqueue(
        &tx,
        &[(1, IndexReason::Delete), (3, IndexReason::Reextract)],
        now(9),
    )
    .unwrap();
    tx.commit().unwrap();
    let fresh_first = pending_batch(&conn, 200)
        .unwrap()
        .documents
        .into_iter()
        .find(|p| p.doc_id == 1)
        .unwrap();
    assert!(fresh_first.revision > old_first);
    let tx = conn.transaction().unwrap();
    assert_eq!(
        rebuild_enqueue_batch(&tx, "job", 1, 200, now(10)).unwrap(),
        2
    );
    tx.commit().unwrap();
    assert_eq!(job_at(&conn, "job").phase, RebuildPhase::Index);
    let pending = pending_batch(&conn, 200).unwrap().documents;
    assert_eq!(pending.len(), 4);
    let first = pending.iter().find(|p| p.doc_id == 1).unwrap();
    assert_eq!(first.revision, fresh_first.revision);
    assert_eq!(first.enqueued_at, now(2));
    assert_eq!(first.reason, IndexReason::Delete);
    let third = pending.iter().find(|p| p.doc_id == 3).unwrap();
    assert_eq!(third.enqueued_at, now(9));
    assert_eq!(third.reason, IndexReason::Reextract);
    assert_eq!(
        pending.iter().find(|p| p.doc_id == 4).unwrap().reason,
        IndexReason::Delete
    );
    drain(&mut conn, "newtoken");
    assert!(complete(&mut conn, "job", 1));
}

#[test]
fn both_pauses_gate_each_real_phase_without_advancing_its_checkpoint() {
    let mut conn = connection();
    indexed(&mut conn, 2, "oldtoken");
    start(&mut conn, "job", 1);
    for phase in [
        RebuildPhase::Reset,
        RebuildPhase::Enqueue,
        RebuildPhase::Index,
    ] {
        assert_eq!(job_at(&conn, "job").phase, phase);
        for user in [true, false] {
            if user {
                pause(&mut conn, UserPause::Pause);
            } else {
                let tx = conn.transaction().unwrap();
                pause_for_io(&tx, SearchIoCode::StorageFull, "full", now(9), 1).unwrap();
                tx.commit().unwrap();
            }
            let before = snapshot(&conn);
            let tx = conn.transaction().unwrap();
            assert_eq!(rebuild_reset_batch(&tx, "job", 1, 200).unwrap(), 0);
            assert_eq!(
                rebuild_enqueue_batch(&tx, "job", 1, 200, now(2)).unwrap(),
                0
            );
            assert!(!complete_rebuild(&tx, "job", 1).unwrap());
            tx.commit().unwrap();
            assert_eq!(snapshot(&conn), before);
            if user {
                pause(&mut conn, UserPause::Resume);
            } else {
                let tx = conn.transaction().unwrap();
                clear_io_pause(&tx).unwrap();
                tx.commit().unwrap();
            }
        }
        match phase {
            RebuildPhase::Reset => {
                reset(&mut conn, "job", 1, 200);
            }
            RebuildPhase::Enqueue => {
                enqueue_phase(&mut conn, "job", 1, 200);
            }
            RebuildPhase::Index => {
                drain(&mut conn, "newtoken");
            }
            _ => unreachable!(),
        }
    }
    assert!(complete(&mut conn, "job", 1));
}

fn run_to_completion(conn: &mut Connection, id: &str, epoch: i64) {
    while job_at(conn, id).phase == RebuildPhase::Reset {
        reset(conn, id, epoch, 200);
    }
    while job_at(conn, id).phase == RebuildPhase::Enqueue {
        enqueue_phase(conn, id, epoch, 200);
    }
    drain(conn, "newtoken");
    assert!(complete(conn, id, epoch));
    assert_eq!(job_at(conn, id).phase, RebuildPhase::Complete);
}

const CUT_N: &str = "SB_REBUILD_CUT_N";
const CUT_ROOT: &str = "SB_REBUILD_CUT_ROOT";

fn crash_conn(root: &str) -> Connection {
    let conn = Connection::open(Path::new(root).join("index.sqlite")).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA trusted_schema=OFF;",
    )
    .unwrap();
    conn
}

#[test]
#[ignore = "spawned by run_crash_cuts"]
fn rebuild_cut_child() {
    let nth: u64 = std::env::var(CUT_N).unwrap().parse().unwrap();
    let root = std::env::var(CUT_ROOT).unwrap();
    let mut conn = crash_conn(&root);
    let fs = CutFs::new(real_fs(), nth);
    let mut cut = || {
        fs.metadata(Path::new(&root))
            .map(|_| ())
            .map_err(|e| StoreError::internal(e.to_string()))
    };
    cut().unwrap();
    let tx = conn.transaction().unwrap();
    match job_at(&tx, "job").phase {
        RebuildPhase::Reset => reset_batch_with_cut(&tx, "job", 1, 200, &mut cut).unwrap(),
        RebuildPhase::Enqueue => {
            enqueue_batch_with_cut(&tx, "job", 1, 200, now(2), &mut cut).unwrap()
        }
        _ => panic!("seeded job must be mid-run"),
    };
    cut().unwrap();
    tx.commit().unwrap();
    cut().unwrap();
}

fn boundary_sweep(
    label: &str,
    seed: impl Fn(&mut Connection),
    step: impl Fn(&mut Connection),
    expected: u64,
) {
    let scratch = ScratchDir::new(label);
    let mut reference = connection();
    seed(&mut reference);
    let old = snapshot(&reference);
    step(&mut reference);
    let new = snapshot(&reference);
    assert_ne!(old, new);
    let (mut saw_old, mut saw_new) = (false, false);
    let cuts = run_crash_cuts(
        CUT_N,
        CUT_ROOT,
        "index::rebuild::tests::batches::rebuild_cut_child",
        &mut |n| {
            let root = scratch.path().join(format!("{label}-{n}"));
            std::fs::create_dir(&root).unwrap();
            let mut conn = crash_conn(root.to_str().unwrap());
            run_migrations(&mut conn, &test_seams(real_fs()), MIGRATIONS).unwrap();
            seed(&mut conn);
            conn.close().unwrap();
            root
        },
        &mut |root| {
            let mut conn = crash_conn(root.to_str().unwrap());
            let actual = snapshot(&conn);
            assert!(
                actual == old || actual == new,
                "mixed {label} batch: {actual}"
            );
            saw_old |= actual == old;
            saw_new |= actual == new;
            run_to_completion(&mut conn, "job", 1);
            let check: String = conn
                .query_row("PRAGMA integrity_check", [], |r| r.get(0))
                .unwrap();
            assert_eq!(check, "ok");
        },
    );
    assert_eq!(cuts, expected);
    assert!(saw_old && saw_new, "must exercise both sides of commit");
}

#[test]
fn phase_errors_roll_back_docs_postings_queue_cursors_progress_and_generation() {
    for (phase, cuts) in [(RebuildPhase::Reset, 5), (RebuildPhase::Enqueue, 6)] {
        for nth in 1..=cuts {
            let mut conn = connection();
            if phase == RebuildPhase::Reset {
                indexed(&mut conn, 2, "oldtoken");
            } else {
                acquisitions(&conn, 3);
            }
            start(&mut conn, "job", 1);
            if phase == RebuildPhase::Enqueue {
                reset(&mut conn, "job", 1, 200);
            }
            let before = snapshot(&conn);
            let mut calls = 0;
            let mut cut = || {
                calls += 1;
                if calls == nth {
                    Err(StoreError::internal("injected rebuild operation failure"))
                } else {
                    Ok(())
                }
            };
            let result = (|| {
                let tx = conn.transaction().unwrap();
                if phase == RebuildPhase::Reset {
                    reset_batch_with_cut(&tx, "job", 1, 200, &mut cut)?;
                } else {
                    enqueue_batch_with_cut(&tx, "job", 1, 200, now(2), &mut cut)?;
                }
                tx.commit().map_err(sql_error)
            })();
            assert!(result.is_err(), "phase={phase:?}, cut={nth}");
            assert_eq!(calls, nth);
            assert_eq!(snapshot(&conn), before);
            run_to_completion(&mut conn, "job", 1);
            assert!(hits(&conn, "oldtoken").is_empty());
            assert_eq!(
                hits(&conn, "newtoken").len(),
                if phase == RebuildPhase::Reset { 2 } else { 3 }
            );
        }
    }
}

#[test]
fn reset_boundary_cuts_leave_the_old_batch_or_the_new_one_never_a_mix() {
    boundary_sweep(
        "reset",
        |conn| {
            indexed(conn, 2, "oldtoken");
            start(conn, "job", 1);
        },
        |conn| {
            reset(conn, "job", 1, 200);
        },
        8,
    );
}

#[test]
fn enqueue_boundary_cuts_leave_the_old_scan_or_the_new_one_never_a_mix() {
    boundary_sweep(
        "enqueue",
        |conn| {
            acquisitions(conn, 3);
            conn.execute(
                "UPDATE acquisitions SET object_state='missing' WHERE acquisition_id=3",
                [],
            )
            .unwrap();
            start(conn, "job", 1);
            reset(conn, "job", 1, 200);
        },
        |conn| {
            enqueue_phase(conn, "job", 1, 200);
        },
        9,
    );
}
