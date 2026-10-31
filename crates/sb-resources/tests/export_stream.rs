//! Native streaming proof; no scope or file-output API.

use std::io::{self, Write};
use std::sync::Arc;

use sb_resources::{
    ExportJob, ExportJobState, MAX_EXPORT_CHANGED_BYTES, MAX_EXPORT_CHUNK_BYTES, MAX_EXPORT_CHUNKS,
    Resources, StoreError, Transaction, WritePriority, advance_export_job, create_export_job,
    export_chunks, export_job,
};
use sb_test_support::{ScratchDir, block_on};

const JOB: &str = "native-export-stream";
const REQUEST: &str = "{\"v\":1}";

fn resources(label: &str) -> (ScratchDir, Resources) {
    let scratch = ScratchDir::new(label);
    let res = Resources::open(&scratch.path().join("store")).unwrap();
    (scratch, res)
}

fn write<T: Send + 'static>(
    res: &Resources,
    f: impl FnOnce(&Transaction) -> Result<T, StoreError> + Send + 'static,
) -> T {
    block_on(res.write(WritePriority::User, f)).unwrap()
}

fn create(res: &Resources) -> ExportJob {
    let now = res.now();
    write(res, move |tx| create_export_job(tx, JOB, REQUEST, now))
}

fn current(res: &Resources) -> ExportJob {
    res.read(|tx| export_job(tx, JOB)).unwrap()
}

fn payload(res: &Resources) -> Vec<u8> {
    let mut bytes = Vec::new();
    for from in (0..current(res).captured_chunks()).step_by(MAX_EXPORT_CHUNKS) {
        let chunks = res
            .read(|tx| export_chunks(tx, JOB, from, MAX_EXPORT_CHUNKS as u32))
            .unwrap();
        bytes.extend(chunks.into_iter().flatten());
    }
    bytes
}

fn cause(error: &io::Error) -> &StoreError {
    error.get_ref().unwrap().downcast_ref().unwrap()
}

fn large_json() -> serde_json::Value {
    const NOTE: &str = "note✓";
    let repeats = (MAX_EXPORT_CHANGED_BYTES + MAX_EXPORT_CHUNK_BYTES) / NOTE.len() + 1;
    serde_json::json!({"v": 1, "note": NOTE.repeat(repeats)})
}

#[test]
fn unicode_json_streams_across_bounded_turns_and_reopens_unsealed() {
    let (scratch, res) = resources("export-stream-json");
    let job = create(&res);
    let value = large_json();
    let expected = serde_json::to_vec(&value).unwrap();
    assert!(
        expected
            .chunks(MAX_EXPORT_CHUNK_BYTES)
            .any(|part| std::str::from_utf8(part).is_err())
    );
    let mut sink = res.export_chunk_writer(&job).unwrap();
    serde_json::to_writer(&mut sink, &value).unwrap();
    let finished = sink.finish().unwrap();
    assert_eq!(finished.state(), ExportJobState::Capturing);
    assert_eq!(finished.checkpoint_json(), REQUEST);
    assert!(finished.captured_chunks() > MAX_EXPORT_CHUNKS as u64);
    assert_eq!(payload(&res), expected);
    res.close().unwrap();
    let reopened = Resources::open(&scratch.path().join("store")).unwrap();
    assert_eq!(payload(&reopened), expected);
    assert_eq!(
        serde_json::from_slice::<serde_json::Value>(&payload(&reopened)).unwrap(),
        value
    );
    assert_eq!(current(&reopened).state(), ExportJobState::Capturing);
}

#[test]
fn short_writes_flush_full_batches_before_accepting_more_and_drop_loses_only_tail() {
    let (_scratch, res) = resources("export-stream-bounds");
    let job = create(&res);
    let mut sink = res.export_chunk_writer(&job).unwrap();
    let pair = vec![7; 2 * MAX_EXPORT_CHUNK_BYTES];
    assert_eq!(sink.write(&pair).unwrap(), MAX_EXPORT_CHUNK_BYTES);
    for _ in 1..MAX_EXPORT_CHUNKS {
        sink.write_all(&pair[..MAX_EXPORT_CHUNK_BYTES]).unwrap();
    }
    assert_eq!(current(&res).captured_chunks(), 0);
    sink.write_all(b"unflushed tail").unwrap();
    assert_eq!(current(&res).captured_chunks(), MAX_EXPORT_CHUNKS as u64);
    drop(sink);
    assert_eq!(
        payload(&res),
        vec![7; MAX_EXPORT_CHUNKS * MAX_EXPORT_CHUNK_BYTES]
    );
}

#[test]
fn partial_flush_is_durable_and_empty_finish_does_not_change_rows() {
    let (_scratch, res) = resources("export-stream-flush");
    let job = create(&res);
    let changes = write(&res, |tx| Ok(tx.total_changes()));
    assert_eq!(
        res.export_chunk_writer(&job)
            .unwrap()
            .finish()
            .unwrap()
            .revision(),
        job.revision()
    );
    assert_eq!(write(&res, |tx| Ok(tx.total_changes())), changes);
    let mut sink = res.export_chunk_writer(&job).unwrap();
    assert_eq!(sink.write(&[]).unwrap(), 0);
    sink.write_all("partial ✓".as_bytes()).unwrap();
    sink.flush().unwrap();
    assert_eq!(payload(&res), "partial ✓".as_bytes());
    assert_eq!(sink.finish().unwrap().captured_chunks(), 1);
}

#[test]
fn replay_splits_a_batch_at_the_kept_end_without_duplicate_or_lost_bytes() {
    const KEPT_CHUNKS: usize = MAX_EXPORT_CHUNKS + 1;
    const TOTAL_CHUNKS: usize = 2 * MAX_EXPORT_CHUNKS + 5;
    let (scratch, res) = resources("export-stream-replay");
    let job = create(&res);
    let bytes: Vec<u8> = (0..TOTAL_CHUNKS * MAX_EXPORT_CHUNK_BYTES)
        .map(|offset| (offset % 256) as u8)
        .collect();
    let mut sink = res.export_chunk_writer(&job).unwrap();
    sink.write_all(&bytes[..KEPT_CHUNKS * MAX_EXPORT_CHUNK_BYTES])
        .unwrap();
    sink.flush().unwrap();
    drop(sink);
    let kept = current(&res);
    assert_eq!(kept.captured_chunks(), KEPT_CHUNKS as u64);
    res.close().unwrap();
    let res = Resources::open(&scratch.path().join("store")).unwrap();
    let kept = current(&res);
    let mut sink = res.export_chunk_writer(&kept).unwrap();
    sink.write_all(&bytes).unwrap();
    let complete = sink.finish().unwrap();
    assert_eq!(complete.captured_chunks(), TOTAL_CHUNKS as u64);
    assert_eq!(complete.revision(), kept.revision() + 2);
    assert_eq!(payload(&res), bytes);
    let mut replay = res.export_chunk_writer(&complete).unwrap();
    replay.write_all(&bytes).unwrap();
    assert_eq!(replay.finish().unwrap().revision(), complete.revision());
}

#[test]
fn changed_bytes_and_short_replay_never_replace_committed_payload() {
    let (_scratch, res) = resources("export-stream-conflicts");
    let job = create(&res);
    let bytes = vec![1; 3 * MAX_EXPORT_CHUNK_BYTES];
    let mut sink = res.export_chunk_writer(&job).unwrap();
    sink.write_all(&bytes).unwrap();
    let committed = sink.finish().unwrap();
    let mut short = res.export_chunk_writer(&committed).unwrap();
    short.write_all(&bytes[..MAX_EXPORT_CHUNK_BYTES]).unwrap();
    assert!(matches!(short.finish(), Err(StoreError::Conflict { .. })));
    let mut changed = res.export_chunk_writer(&committed).unwrap();
    changed.write_all(&vec![2; MAX_EXPORT_CHUNK_BYTES]).unwrap();
    assert!(matches!(
        cause(&changed.flush().unwrap_err()),
        StoreError::Conflict { .. }
    ));
    assert!(matches!(changed.finish(), Err(StoreError::Conflict { .. })));
    assert_eq!(payload(&res), bytes);
    assert_eq!(current(&res).revision(), committed.revision());
}

#[test]
fn stale_empty_finish_and_non_capturing_snapshots_are_rejected() {
    let (_scratch, res) = resources("export-stream-fences");
    let job = create(&res);
    let empty = res.export_chunk_writer(&job).unwrap();
    let now = res.now();
    let next = write(&res, move |tx| {
        advance_export_job(tx, JOB, job.revision(), ExportJobState::Ready, REQUEST, now)
    });
    assert!(matches!(empty.finish(), Err(StoreError::Conflict { .. })));
    assert!(matches!(
        res.export_chunk_writer(&next),
        Err(StoreError::Conflict { .. })
    ));
    assert_eq!(current(&res).captured_chunks(), 0);
}

#[test]
fn serde_keeps_cancellation_and_the_sink_repeats_its_first_failure() {
    let (_scratch, res) = resources("export-stream-cancel");
    let job = create(&res);
    let mut sink = res.export_chunk_writer(&job).unwrap();
    let now = res.now();
    write(&res, move |tx| {
        advance_export_job(
            tx,
            JOB,
            job.revision(),
            ExportJobState::Cancelled,
            REQUEST,
            now,
        )
    });
    let error: io::Error = serde_json::to_writer(&mut sink, &large_json())
        .unwrap_err()
        .into();
    assert!(matches!(cause(&error), StoreError::Conflict { .. }));
    let original = cause(&error).to_string();
    assert_eq!(
        cause(&sink.write(b"later").unwrap_err()).to_string(),
        original
    );
    assert_eq!(cause(&sink.flush().unwrap_err()).to_string(), original);
    assert_eq!(sink.finish().unwrap_err().to_string(), original);
    assert_eq!(current(&res).state(), ExportJobState::Cancelled);
    assert_eq!(current(&res).captured_chunks(), 0);
}

#[test]
fn sqlite_storage_full_survives_serde_with_its_native_kind_and_rolls_back() {
    let (_scratch, res) = resources("export-stream-full");
    let job = create(&res);
    write(&res, |tx| {
        let pages: i64 = tx
            .query_row("PRAGMA page_count", [], |row| row.get(0))
            .unwrap();
        tx.pragma_update(None, "max_page_count", pages).unwrap();
        Ok(())
    });
    let mut sink = res.export_chunk_writer(&job).unwrap();
    let error: io::Error = serde_json::to_writer(&mut sink, &large_json())
        .unwrap_err()
        .into();
    assert!(
        matches!(cause(&error), StoreError::Unavailable { .. }),
        "{error:?}"
    );
    assert_eq!(cause(&error).io_kind(), Some(io::ErrorKind::StorageFull));
    assert_eq!(error.kind(), io::ErrorKind::StorageFull);
    let original = cause(&error).to_string();
    let finished = sink.finish().unwrap_err();
    assert_eq!(finished.to_string(), original);
    assert_eq!(finished.io_kind(), Some(io::ErrorKind::StorageFull));
    assert_eq!(current(&res).captured_chunks(), 0);
    assert_eq!(current(&res).revision(), job.revision());
}

#[test]
fn own_thread_construction_refuses_before_waiting() {
    let (_scratch, res) = resources("export-stream-thread");
    let res = Arc::new(res);
    let job = create(&res);
    let resource = Arc::clone(&res);
    let snapshot = job.clone();
    write(&res, move |_tx| {
        assert!(matches!(
            resource.export_chunk_writer(&snapshot),
            Err(StoreError::Internal { .. })
        ));
        Ok(())
    });
}
