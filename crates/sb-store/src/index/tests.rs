use super::*;
use std::path::Path;

use crate::Fs;
use crate::schema::{MIGRATIONS, run_migrations};
use crate::testing::{reading, scratch_paths, test_seams, writing};
use rusqlite::Connection;
use sb_test_support::{CutFs, ScratchDir, real_fs, run_crash_cuts};

fn now(ms: i64) -> Timestamp {
    Timestamp::from_millis(ms)
}

fn connection() -> Connection {
    let mut conn = Connection::open_in_memory().unwrap();
    conn.execute_batch("PRAGMA foreign_keys=ON; PRAGMA trusted_schema=OFF;")
        .unwrap();
    run_migrations(&mut conn, &test_seams(real_fs()), MIGRATIONS).unwrap();
    conn
}

fn acquisitions(conn: &Connection, count: usize) {
    for id in 1..=count {
        conn.execute(
            "INSERT INTO acquisitions (acquisition_id, content_sha256, byte_length,
             object_state, first_acquired_at) VALUES (?1, ?2, 1, 'published', ?1)",
            params![id as i64, format!("{id:064x}")],
        )
        .unwrap();
    }
}

fn document(text: &str) -> IndexDocument {
    let mut columns = std::array::from_fn(|_| String::new());
    columns[0] = text.to_owned();
    IndexDocument {
        columns,
        extractor_version: 1,
        state: IndexState::Indexed,
        truncated_fields: vec![],
    }
}

fn queue(conn: &mut Connection, count: usize, time: i64) {
    let tx = conn.transaction().unwrap();
    let requests: Vec<_> = (1..=count)
        .map(|id| (id as i64, IndexReason::Reextract))
        .collect();
    enqueue(&tx, &requests, now(time)).unwrap();
    tx.commit().unwrap();
}

fn batch(conn: &Connection, text: &str) -> (IndexFence, Vec<IndexPublication>) {
    let captured = pending_batch(conn, MAX_INDEX_BATCH).unwrap();
    let documents = captured
        .documents
        .into_iter()
        .map(|pending| IndexPublication {
            pending,
            document: Some(document(text)),
        })
        .collect();
    (captured.fence, documents)
}

fn publish(conn: &mut Connection, fence: IndexFence, batch: &[IndexPublication]) -> usize {
    let tx = conn.transaction().unwrap();
    let count = publish_batch(&tx, fence, batch).unwrap();
    tx.commit().unwrap();
    count
}

fn hits(conn: &Connection, query: &str) -> Vec<i64> {
    let mut stmt = conn
        .prepare("SELECT rowid FROM d_search_fts WHERE d_search_fts MATCH ?1 ORDER BY rowid")
        .unwrap();
    stmt.query_map([query], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

// Compare postings with MATCH, not external-content SELECT *, which reads
// docs even when the index has no posting (in particular failed documents).
fn snapshot(conn: &Connection) -> String {
    let docs: Vec<String> = conn
        .prepare(
            "SELECT json_array(doc_id, extractor_version, state, truncated_fields,
         subject, from_text, to_text, cc_text, body_authored, body_quoted,
         body_signature, attachment_names) FROM d_search_docs ORDER BY doc_id",
        )
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let postings: Vec<i64> = conn
        .prepare("SELECT id FROM d_search_fts_docsize ORDER BY id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap();
    let pending = pending_batch(conn, MAX_INDEX_BATCH).unwrap().documents;
    format!(
        "{docs:?}/{pending:?}/{postings:?}/{:?}/{:?}/{}",
        hits(conn, "oldtoken"),
        hits(conn, "newtoken"),
        search_generation(conn).unwrap()
    )
}

#[test]
fn v1_upgrade_backfills_all_acquisitions_and_preserves_pending_age() {
    let mut conn = Connection::open_in_memory().unwrap();
    let seams = test_seams(real_fs());
    run_migrations(&mut conn, &seams, &MIGRATIONS[..1]).unwrap();
    acquisitions(&conn, 3);
    conn.execute_batch(
        "UPDATE acquisitions SET object_state='missing' WHERE acquisition_id=3;
         INSERT INTO d_index_queue VALUES (2, 'new', -10);
         INSERT INTO d_index_queue VALUES (99, 'delete', -20);",
    )
    .unwrap();
    run_migrations(&mut conn, &seams, MIGRATIONS).unwrap();
    let pending = pending_batch(&conn, 200).unwrap().documents;
    assert_eq!(
        pending.iter().map(|p| p.doc_id).collect::<Vec<_>>(),
        [99, 2, 1, 3]
    );
    assert_eq!(
        pending.iter().map(|p| p.revision).collect::<Vec<_>>(),
        [1, 2, 3, 4]
    );
    assert_eq!(pending[1].enqueued_at, now(-10));
    assert_eq!(pending[3].reason, IndexReason::Delete);
    assert_eq!(meta_integer(&conn, "index_inputs").unwrap(), 4);
    assert_eq!(target_version(&conn).unwrap(), 1);
    assert_eq!(search_generation(&conn).unwrap(), 1);
    queue(&mut conn, 1, 100);
    let pending = pending_batch(&conn, 200).unwrap().documents;
    assert_eq!(pending[2].revision, 5);
    assert_eq!(pending[2].enqueued_at, now(1));
    let plan: String = conn.query_row(
        "EXPLAIN QUERY PLAN SELECT doc_id, revision FROM d_index_queue ORDER BY enqueued_at, doc_id LIMIT 200", [], |r| r.get(3),
    ).unwrap();
    assert!(plan.contains("d_index_queue_oldest"), "{plan}");
}

#[test]
fn concurrent_writer_requeue_rejects_only_the_stale_document() {
    let (_scratch, paths) = scratch_paths("index-requeue");
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    writing(&store, |tx| {
        acquisitions(tx, 2);
        enqueue(tx, &[(1, IndexReason::New), (2, IndexReason::New)], now(10))
    });
    let (fence, extracted) = reading(&store, |tx| Ok(batch(tx, "oldtoken")));
    // A distinct writer submission commits while extraction holds old tokens.
    writing(&store, |tx| {
        enqueue(tx, &[(1, IndexReason::Reextract)], now(20))
    });
    let stale_revision = extracted[0].pending.revision;
    assert_eq!(
        writing(&store, move |tx| publish_batch(tx, fence, &extracted)),
        1
    );
    reading(&store, |tx| {
        let pending = pending_batch(tx, 200)?.documents;
        assert_eq!(pending.len(), 1);
        assert_eq!(pending[0].doc_id, 1);
        assert!(pending[0].revision > stale_revision);
        assert_eq!(pending[0].enqueued_at, now(10));
        assert_eq!(hits(tx, "oldtoken"), [2]);
        assert_eq!(search_generation(tx)?, 4);
        Ok(())
    });
    store.close().unwrap();
}

#[test]
fn old_target_and_already_acknowledged_tokens_write_nothing() {
    let mut conn = connection();
    acquisitions(&conn, 1);
    queue(&mut conn, 1, 1);
    let (fence, batch) = batch(&conn, "oldtoken");
    conn.execute(
        "UPDATE store_meta SET value='2' WHERE key='search_extractor_version'",
        [],
    )
    .unwrap();
    let before = snapshot(&conn);
    assert_eq!(publish(&mut conn, fence, &batch), 0);
    assert_eq!(snapshot(&conn), before);
    assert_eq!(target_version(&conn).unwrap(), 2);
    conn.execute(
        "UPDATE store_meta SET value='1' WHERE key='search_extractor_version'",
        [],
    )
    .unwrap();
    assert_eq!(publish(&mut conn, fence, &batch), 1);
    let after = snapshot(&conn);
    assert_eq!(publish(&mut conn, fence, &batch), 0);
    assert_eq!(snapshot(&conn), after);
}

#[test]
fn reextraction_replaces_postings_and_failed_rows_have_no_fts_entry() {
    let mut conn = connection();
    acquisitions(&conn, 1);
    queue(&mut conn, 1, 1);
    let (fence, first) = batch(&conn, "oldtoken");
    publish(&mut conn, fence, &first);
    assert_eq!(hits(&conn, "oldtoken"), [1]);
    queue(&mut conn, 1, 2);
    let (fence, replacement) = batch(&conn, "newtoken");
    publish(&mut conn, fence, &replacement);
    assert!(hits(&conn, "oldtoken").is_empty());
    assert_eq!(hits(&conn, "newtoken"), [1]);
    queue(&mut conn, 1, 3);
    let (fence, mut failed) = batch(&conn, "");
    failed[0].document.as_mut().unwrap().state = IndexState::Failed;
    publish(&mut conn, fence, &failed);
    assert!(hits(&conn, "newtoken").is_empty());
    assert_eq!(
        conn.query_row("SELECT state FROM d_search_docs", [], |r| r
            .get::<_, String>(0))
            .unwrap(),
        "failed"
    );
    assert!(pending_batch(&conn, 200).unwrap().documents.is_empty());
    assert_eq!(
        conn.query_row(
            "SELECT count(*) FROM d_search_fts_docsize WHERE id=1",
            [],
            |r| r.get::<_, i64>(0)
        )
        .unwrap(),
        0
    );
    // A failed old row has no posting to delete; a later retry still works.
    queue(&mut conn, 1, 4);
    let (fence, retry) = batch(&conn, "newtoken");
    publish(&mut conn, fence, &retry);
    assert_eq!(hits(&conn, "newtoken"), [1]);
    conn.execute("UPDATE acquisitions SET object_state='missing'", [])
        .unwrap();
    let tx = conn.transaction().unwrap();
    enqueue(&tx, &[(1, IndexReason::Delete)], now(5)).unwrap();
    tx.commit().unwrap();
    let captured = pending_batch(&conn, 200).unwrap();
    let pending = captured.documents.into_iter().next().unwrap();
    publish(
        &mut conn,
        captured.fence,
        &[IndexPublication {
            pending,
            document: None,
        }],
    );
    assert!(hits(&conn, "newtoken").is_empty());
    assert_eq!(
        conn.query_row("SELECT count(*) FROM d_search_docs", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        0
    );
    assert_eq!(
        conn.query_row("SELECT count(*) FROM acquisitions", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
}

#[test]
fn count_and_byte_bounds_reject_without_acknowledging_work() {
    let mut conn = connection();
    acquisitions(&conn, 201);
    assert!(pending_batch(&conn, 201).is_err());
    assert!(pending_batch(&conn, 0).unwrap().documents.is_empty());
    let tx = conn.transaction().unwrap();
    assert!(enqueue(&tx, &vec![(1, IndexReason::New); 201], now(1)).is_err());
    tx.rollback().unwrap();
    queue(&mut conn, 200, 1);
    let (fence, publications) = batch(&conn, "newtoken");
    let before = snapshot(&conn);
    let tx = conn.transaction().unwrap();
    let mut too_many = publications.clone();
    too_many.push(publications[0].clone());
    assert!(publish_batch(&tx, fence, &too_many).is_err());
    tx.rollback().unwrap();
    assert_eq!(snapshot(&conn), before);
    // Exactly 200 documents succeeds and advances the generation once.
    assert_eq!(publish(&mut conn, fence, &publications), 200);
    queue(&mut conn, 3, 2);
    let (fence, mut large) = batch(&conn, "newtoken");
    large[0].document = Some(document(&"x".repeat(1_000_000)));
    large[1].document = Some(document(&"y".repeat(1_000_000)));
    let tx = conn.transaction().unwrap();
    assert!(publish_batch(&tx, fence, &large).is_err());
    tx.rollback().unwrap();
    assert_eq!(pending_batch(&conn, 200).unwrap().documents.len(), 3);
    // Exactly 2,000,000 new column bytes fits; the third remains pending.
    assert_eq!(publish(&mut conn, fence, &large[..2]), 2);
    assert_eq!(pending_batch(&conn, 200).unwrap().documents.len(), 1);
}

fn prepare_cuts(conn: &mut Connection) -> (IndexFence, Vec<IndexPublication>) {
    acquisitions(conn, 4);
    queue(conn, 4, 1);
    let (fence, old) = batch(conn, "oldtoken");
    publish(conn, fence, &old);
    conn.execute(
        "UPDATE acquisitions SET object_state='missing' WHERE acquisition_id=4",
        [],
    )
    .unwrap();
    let tx = conn.transaction().unwrap();
    enqueue(
        &tx,
        &[
            (1, IndexReason::Reextract),
            (2, IndexReason::Reextract),
            (3, IndexReason::Reextract),
            (4, IndexReason::Delete),
        ],
        now(2),
    )
    .unwrap();
    tx.commit().unwrap();
    cut_batch(conn)
}

fn cut_batch(conn: &Connection) -> (IndexFence, Vec<IndexPublication>) {
    let (fence, mut batch) = batch(conn, "newtoken");
    let partial = batch[1].document.as_mut().unwrap();
    partial.state = IndexState::Partial;
    partial.truncated_fields = vec!["body_authored".to_owned()];
    batch[2].document = Some(IndexDocument {
        state: IndexState::Failed,
        ..document("")
    });
    batch[3].document = None;
    (fence, batch)
}

// 1 version read, 6/6/5/4 operations for indexed/partial/failed/delete,
// 1 generation read and update, and cuts before BEGIN and before/after COMMIT.
const PUBLICATION_CUTS: u64 = 27;

#[test]
fn rollback_at_every_publication_operation_preserves_the_whole_old_batch() {
    for nth in 1..PUBLICATION_CUTS {
        // post-commit is covered by the process-kill test
        let mut conn = connection();
        let (fence, batch) = prepare_cuts(&mut conn);
        let before = snapshot(&conn);
        let mut calls = 0;
        let mut cut = || {
            calls += 1;
            if calls == nth {
                return Err(StoreError::unavailable("injected publication fault"));
            }
            Ok(())
        };
        let result = (|| {
            cut()?;
            let tx = conn.transaction().unwrap();
            publish_with_cut(&tx, fence, &batch, &mut cut)?;
            cut()?;
            tx.commit().map_err(sql_error)
        })();
        assert!(result.is_err(), "cut {nth} was not exercised");
        assert_eq!(snapshot(&conn), before, "cut {nth}");
        assert_eq!(publish(&mut conn, fence, &batch), 4);
    }
}

const CUT_N: &str = "SB_INDEX_CUT_N";
const CUT_ROOT: &str = "SB_INDEX_CUT_ROOT";

#[test]
#[ignore = "spawned by run_crash_cuts"]
fn publication_cut_child() {
    let nth = std::env::var(CUT_N).unwrap().parse().unwrap();
    let root = std::env::var(CUT_ROOT).unwrap();
    let mut conn = Connection::open(Path::new(&root).join("index.sqlite")).unwrap();
    conn.execute_batch(
        "PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL; PRAGMA trusted_schema=OFF;",
    )
    .unwrap();
    let (fence, batch) = cut_batch(&conn);
    let fs = CutFs::new(real_fs(), nth);
    let mut cut = || {
        fs.metadata(Path::new(&root))
            .map(|_| ())
            .map_err(|e| StoreError::internal(e.to_string()))
    };
    cut().unwrap();
    let tx = conn.transaction().unwrap();
    publish_with_cut(&tx, fence, &batch, &mut cut).unwrap();
    cut().unwrap();
    tx.commit().unwrap();
    cut().unwrap();
    assert_eq!(fs.operations(), PUBLICATION_CUTS);
}

#[test]
fn crash_cuts_leave_old_or_new_docs_postings_queue_and_generation_never_a_mix() {
    let scratch = ScratchDir::new("index-cuts");
    let mut reference = connection();
    let (fence, batch) = prepare_cuts(&mut reference);
    let old = snapshot(&reference);
    publish(&mut reference, fence, &batch);
    let new = snapshot(&reference);
    assert_ne!(old, new);
    let mut saw_old = false;
    let mut saw_new = false;
    let cuts = run_crash_cuts(
        CUT_N,
        CUT_ROOT,
        "index::tests::publication_cut_child",
        &mut |n| {
            let root = scratch.path().join(format!("cut-{n}"));
            std::fs::create_dir(&root).unwrap();
            let mut conn = Connection::open(root.join("index.sqlite")).unwrap();
            conn.execute_batch("PRAGMA journal_mode=WAL; PRAGMA synchronous=FULL;")
                .unwrap();
            run_migrations(&mut conn, &test_seams(real_fs()), MIGRATIONS).unwrap();
            prepare_cuts(&mut conn);
            root
        },
        &mut |root| {
            let mut conn = Connection::open(root.join("index.sqlite")).unwrap();
            let actual = snapshot(&conn);
            assert!(
                actual == old || actual == new,
                "mixed publication: {actual}"
            );
            saw_old |= actual == old;
            saw_new |= actual == new;
            if actual == old {
                let (fence, batch) = cut_batch(&conn);
                assert_eq!(publish(&mut conn, fence, &batch), 4);
            }
            assert_eq!(snapshot(&conn), new);
            let check: String = conn
                .query_row("PRAGMA integrity_check", [], |r| r.get(0))
                .unwrap();
            assert_eq!(check, "ok");
        },
    );
    assert_eq!(cuts, PUBLICATION_CUTS);
    assert!(saw_old && saw_new, "must exercise both sides of commit");
}

#[test]
fn every_fts_column_is_deleted_and_reinserted_with_its_own_values() {
    let mut conn = connection();
    acquisitions(&conn, 1);
    for token in ["oldtoken", "newtoken"] {
        queue(&mut conn, 1, 1);
        let (fence, mut batch) = batch(&conn, "");
        batch[0].document.as_mut().unwrap().columns =
            std::array::from_fn(|i| format!("{token}{i}"));
        publish(&mut conn, fence, &batch);
    }
    for (i, field) in FIELDS.iter().enumerate() {
        assert!(hits(&conn, &format!("{field}:oldtoken{i}")).is_empty());
        assert_eq!(hits(&conn, &format!("{field}:newtoken{i}")), [1]);
    }
}

#[test]
fn invalid_documents_and_duplicate_tokens_leave_work_pending() {
    let mut conn = connection();
    acquisitions(&conn, 1);
    queue(&mut conn, 1, 1);
    let (fence, good) = batch(&conn, "newtoken");
    let before = snapshot(&conn);
    let mut bad_docs = vec![];
    let mut doc = document("newtoken");
    doc.extractor_version = 2;
    bad_docs.push(doc);
    let mut doc = document("newtoken");
    doc.state = IndexState::Failed;
    bad_docs.push(doc);
    let mut doc = document("newtoken");
    doc.state = IndexState::Partial;
    bad_docs.push(doc);
    let mut doc = document("newtoken");
    doc.state = IndexState::Partial;
    doc.truncated_fields = vec!["unknown".to_owned()];
    bad_docs.push(doc);
    let mut doc = document("newtoken");
    doc.truncated_fields = vec!["subject".to_owned()];
    bad_docs.push(doc);
    let mut doc = document("newtoken");
    doc.state = IndexState::Partial;
    doc.truncated_fields = vec!["subject".to_owned(), "subject".to_owned()];
    bad_docs.push(doc);
    let mut doc = document("");
    doc.columns[7] = "x".repeat(64_001);
    bad_docs.push(doc);
    bad_docs.push(document(&"x".repeat(1_000_001)));
    for bad in bad_docs {
        let mut batch = good.clone();
        batch[0].document = Some(bad);
        let tx = conn.transaction().unwrap();
        assert!(publish_batch(&tx, fence, &batch).is_err());
        tx.rollback().unwrap();
        assert_eq!(snapshot(&conn), before);
    }
    let tx = conn.transaction().unwrap();
    assert!(publish_batch(&tx, fence, &[good[0].clone(), good[0].clone()]).is_err());
    tx.rollback().unwrap();
    assert_eq!(snapshot(&conn), before);
}

#[test]
fn restored_bytes_supersede_delete_and_preserve_the_pending_age() {
    let mut conn = connection();
    acquisitions(&conn, 1);
    conn.execute("UPDATE acquisitions SET object_state='missing'", [])
        .unwrap();
    let tx = conn.transaction().unwrap();
    enqueue(&tx, &[(1, IndexReason::Delete)], now(10)).unwrap();
    tx.commit().unwrap();
    let delete_fence = pending_batch(&conn, 1).unwrap();
    let delete = IndexPublication {
        pending: delete_fence.documents.into_iter().next().unwrap(),
        document: None,
    };
    conn.execute("UPDATE acquisitions SET object_state='published'", [])
        .unwrap();
    queue(&mut conn, 1, 20);
    let before = snapshot(&conn);
    assert_eq!(publish(&mut conn, delete_fence.fence, &[delete]), 0);
    assert_eq!(snapshot(&conn), before);
    let pending = pending_batch(&conn, 1).unwrap().documents.remove(0);
    assert_eq!(pending.reason, IndexReason::Reextract);
    assert_eq!(pending.enqueued_at, now(10));
    let tx = conn.transaction().unwrap();
    assert!(enqueue(&tx, &[(1, IndexReason::Delete)], now(30)).is_err());
    tx.rollback().unwrap();
    assert_eq!(snapshot(&conn), before);
}
