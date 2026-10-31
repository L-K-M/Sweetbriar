//! Native capture-ledger proof through Resources, including SQL guard bypasses.
//! All payloads and stores are synthetic.

use sb_resources::{
    ExportJob, ExportJobState, JobKind, MAX_EXPORT_CHANGED_BYTES, MAX_EXPORT_CHUNK_BYTES,
    MAX_EXPORT_CHUNKS, MAX_EXPORT_JSON_BYTES, Resources, StoreError, Timestamp, Transaction,
    WritePriority, advance_export_job, append_export_chunks, create_export_job, export_chunks,
    export_job, job_kind,
};
use sb_test_support::{ScratchDir, block_on};

const JOB: &str = "00000000-0000-4000-8000-0000000007b2";
const IMPORT_ID: &str = "00000000-0000-4000-8000-0000000007b1";
const SEARCH_ID: &str = "00000000-0000-4000-8000-0000000007b3";
const REQUEST: &str = "{\"v\":1,\"format\":\"eml\",\"scope\":\"selection\"}";
/// Key order and whitespace differ from `CANONICAL` on purpose.
const REQUEST_PRETTY: &str = "{ \"scope\": \"selection\", \"format\": \"eml\", \"v\": 1 }";
const CANONICAL: &str = "{\"format\":\"eml\",\"scope\":\"selection\",\"v\":1}";
const IMPORT_ROW: &str = "INSERT INTO import_jobs (job_id, state, request_json, checkpoint_json, \
                          revision, created_at, updated_at) VALUES ('{id}', 'inventory', \
                          '{\"v\":1}', '{\"v\":1}', 1, 0, 0)";
const EXPORT_ROW: &str = "INSERT INTO export_jobs (job_id, state, request_json, checkpoint_json, \
                          created_at, updated_at) VALUES ('{id}', 'capturing', \
                          '{\"v\":1}', '{\"v\":1}', 0, 0)";
const SEARCH_ROW: &str = "INSERT INTO search_rebuild_jobs (job_id, epoch, phase, doc_cursor, \
                          acq_cursor, phase_count) VALUES ('{id}', \
                          (SELECT coalesce(max(epoch), 0) + 1 FROM search_rebuild_jobs), 'complete', 0, 0, 0)";

fn resources(label: &str) -> (ScratchDir, Resources) {
    let scratch = ScratchDir::new(label);
    let resources = Resources::open(&scratch.path().join("store")).unwrap();
    (scratch, resources)
}

/// One facade write on the writer's own transaction.
fn write<T: Send + 'static>(
    res: &Resources,
    f: impl FnOnce(&Transaction) -> Result<T, StoreError> + Send + 'static,
) -> Result<T, StoreError> {
    block_on(res.write(WritePriority::User, f))
}

fn at(step: i64) -> Timestamp {
    Timestamp::from_millis(1_782_000_000_000 + step)
}

fn create(res: &Resources, id: &'static str, request: &str) -> Result<ExportJob, StoreError> {
    let request = request.to_owned();
    write(res, move |tx| create_export_job(tx, id, &request, at(80)))
}

fn advance(
    res: &Resources,
    id: &'static str,
    revision: i64,
    next: ExportJobState,
    checkpoint: &str,
) -> Result<ExportJob, StoreError> {
    let checkpoint = checkpoint.to_owned();
    write(res, move |tx| {
        advance_export_job(tx, id, revision, next, &checkpoint, at(81))
    })
}

fn append(
    res: &Resources,
    id: &'static str,
    revision: i64,
    from: u64,
    batch: Vec<Vec<u8>>,
) -> Result<ExportJob, StoreError> {
    write(res, move |tx| {
        append_export_chunks(tx, id, revision, from, &batch, at(82))
    })
}

/// Run raw SQL in a facade write; a refused statement reports `Internal`
/// so the whole transaction rolls back (spec/02's triggers).
fn sql(res: &Resources, statement: String) -> Result<(), StoreError> {
    write(res, move |tx| {
        tx.execute(&statement, ())
            .map(|_| ())
            .map_err(|error| StoreError::internal(error.to_string()))
    })
}

/// `result` must refuse, with the named error kind.
macro_rules! rejected {
    ($result:expr, $kind:expr, $label:expr $(, $arg:expr)* $(,)?) => {{
        let label = format!($label $(, $arg)*);
        match $result {
            Ok(_) => panic!("{label} was accepted"),
            Err(error) => assert!($kind(&error), "{label}: {error}"),
        }
    }};
}

fn invalid(error: &StoreError) -> bool {
    matches!(error, StoreError::InvalidInput { .. })
}

fn conflict(error: &StoreError) -> bool {
    matches!(error, StoreError::Conflict { .. })
}

fn internal(error: &StoreError) -> bool {
    matches!(error, StoreError::Internal { .. })
}

fn not_found(error: &StoreError) -> bool {
    matches!(error, StoreError::NotFound { .. })
}

/// Synthetic payload: every byte value cycles, and multibyte text is
/// spliced in so unicode sequences straddle chunk boundaries.
fn payload(total: usize) -> Vec<u8> {
    const TEXT: &[u8] =
        "Sweetbriar export \u{2709} na\u{ef}ve \u{2014} \u{65e5}\u{672c}\u{8a9e}\r\n".as_bytes();
    let mut bytes: Vec<u8> = (0..total).map(|position| position as u8).collect();
    for offset in [
        MAX_EXPORT_CHUNK_BYTES - 7,
        2 * MAX_EXPORT_CHUNK_BYTES - 3,
        total / 2,
    ] {
        let end = offset + TEXT.len();
        bytes[offset..end].copy_from_slice(TEXT);
    }
    bytes
}

fn chunks_of(bytes: &[u8]) -> Vec<Vec<u8>> {
    bytes
        .chunks(MAX_EXPORT_CHUNK_BYTES)
        .map(|c| c.to_vec())
        .collect()
}

#[test]
fn creation_is_canonical_replays_or_conflicts_and_refuses_bad_shapes() {
    let (_scratch, res) = resources("create");
    let job = create(&res, JOB, REQUEST_PRETTY).unwrap();
    assert_eq!(job.id(), JOB);
    assert_eq!(job.state(), ExportJobState::Capturing);
    assert_eq!((job.revision(), job.captured_chunks()), (1, 0));
    assert_eq!(job.request_json(), CANONICAL);
    assert_eq!(job.checkpoint_json(), "{\"v\":1}");

    // A canonical replay keeps the job: whitespace and key order vary.
    let job = create(
        &res,
        JOB,
        "{ \"v\" : 1 , \"format\" : \"eml\" , \"scope\" : \"selection\" }",
    )
    .unwrap();
    assert_eq!(
        (job.state(), job.revision()),
        (ExportJobState::Capturing, 1)
    );
    assert_eq!(job.request_json(), CANONICAL);

    // Another request for the same id conflicts and keeps nothing of it.
    rejected!(
        create(&res, JOB, "{\"v\":1,\"format\":\"mboxrd\"}"),
        conflict,
        "a changed request replayed"
    );
    assert_eq!(
        res.read(|tx| export_job(tx, JOB)).unwrap().request_json(),
        CANONICAL
    );

    // Requests must be version-1 objects within 4,096 bytes; exactly
    // 4,096 fits.
    let mut bad = vec!["[]".to_owned(), "{}".to_owned(), "{\"v\":2}".to_owned()];
    bad.push(format!(
        "{{\"v\":1,\"pad\":\"{}\"}}",
        "a".repeat(MAX_EXPORT_JSON_BYTES)
    ));
    for request in &bad {
        rejected!(create(&res, "shape", request), invalid, "{request}");
    }
    let exact = format!(
        "{{\"v\":1,\"pad\":\"{}\"}}",
        "a".repeat(MAX_EXPORT_JSON_BYTES - r#"{"v":1,"pad":""}"#.len())
    );
    assert_eq!(exact.len(), MAX_EXPORT_JSON_BYTES);
    write(&res, move |tx| create_export_job(tx, "edge", &exact, at(1))).unwrap();

    // Ids are 1 through 128 UTF-8 bytes.
    for id in [String::new(), "e".repeat(129)] {
        let bytes = id.len();
        rejected!(
            write(&res, move |tx| create_export_job(tx, &id, REQUEST, at(2))),
            invalid,
            "an id of {} bytes",
            bytes
        );
    }
    let bounded = "f".repeat(128);
    write(&res, move |tx| {
        create_export_job(tx, &bounded, REQUEST, at(3))
    })
    .unwrap();

    // Unknown ids never resolve through any entry point.
    let absent = "00000000-0000-4000-8000-0000000007ff";
    rejected!(
        res.read(|tx| export_job(tx, absent)),
        not_found,
        "unknown job"
    );
    rejected!(
        res.read(|tx| export_chunks(tx, absent, 0, 1)),
        not_found,
        "unknown page"
    );
    let probe = vec![b"x".to_vec()];
    rejected!(
        write(&res, move |tx| append_export_chunks(
            tx,
            absent,
            1,
            0,
            &probe,
            at(4)
        )),
        not_found,
        "unknown append"
    );
    rejected!(
        write(&res, |tx| {
            advance_export_job(tx, absent, 1, ExportJobState::Cancelled, "{\"v\":1}", at(4))
        }),
        not_found,
        "unknown advance"
    );
}

#[test]
fn appends_replay_exactly_and_reject_every_shortcut() {
    let (_scratch, res) = resources("append-fence");
    create(&res, JOB, REQUEST).unwrap();
    let expect: Vec<Vec<u8>> = vec![
        b"alpha".to_vec(),
        "\u{2709} na\u{ef}ve \u{2014} \u{65e5}\u{672c}\u{8a9e}"
            .as_bytes()
            .to_vec(),
        vec![0x00, 0xff, 0x80],
    ];
    let (a, b, c) = (&expect[0], &expect[1], &expect[2]);

    let job = append(&res, JOB, 1, 0, vec![a.clone(), b.clone()]).unwrap();
    assert_eq!((job.revision(), job.captured_chunks()), (2, 2));
    let before = res.read(|tx| export_job(tx, JOB)).unwrap();

    // An exact replay with the original, now stale, revision changes
    // nothing: not the revision, not the checkpoint.
    let job = append(&res, JOB, 1, 0, vec![a.clone(), b.clone()]).unwrap();
    assert_eq!((job.revision(), job.captured_chunks()), (2, 2));
    assert_eq!(
        res.read(|tx| export_job(tx, JOB))
            .unwrap()
            .checkpoint_json(),
        before.checkpoint_json()
    );

    // A changed replay, and one reaching past the committed range, are
    // conflicts that write nothing.
    rejected!(
        append(&res, JOB, 1, 0, vec![a.clone(), c.clone()]),
        conflict,
        "a changed replay"
    );
    rejected!(
        append(&res, JOB, 1, 0, vec![a.clone(), b.clone(), c.clone()]),
        conflict,
        "a partial-overlap replay"
    );

    // An ordinal gap is invalid input; a stale revision on a fresh range
    // is a conflict.
    rejected!(
        append(&res, JOB, 2, 5, vec![c.clone()]),
        invalid,
        "an ordinal gap"
    );
    rejected!(
        append(&res, JOB, 1, 2, vec![c.clone()]),
        conflict,
        "a stale fresh append"
    );

    // A fresh append at the fence commits; the first batch still replays.
    let job = append(&res, JOB, 2, 2, vec![c.clone()]).unwrap();
    assert_eq!((job.revision(), job.captured_chunks()), (3, 3));
    let job = append(&res, JOB, 1, 0, vec![a.clone(), b.clone()]).unwrap();
    assert_eq!(job.revision(), 3);

    // Empty batches, over-long batches, empty chunks and oversized
    // chunks are invalid input, all without partial effects.
    for (label, batch) in [
        ("an empty batch", Vec::new()),
        ("31 chunks", vec![b"x".to_vec(); MAX_EXPORT_CHUNKS + 1]),
        ("an empty chunk", vec![Vec::new()]),
        (
            "an oversized chunk",
            vec![vec![b'z'; MAX_EXPORT_CHUNK_BYTES + 1]],
        ),
    ] {
        rejected!(append(&res, JOB, 3, 3, batch), invalid, "{label}");
    }
    let job = res.read(|tx| export_job(tx, JOB)).unwrap();
    assert_eq!((job.revision(), job.captured_chunks()), (3, 3));
    assert_eq!(
        res.read(|tx| export_chunks(tx, JOB, 0, MAX_EXPORT_CHUNKS as u32))
            .unwrap(),
        expect
    );
}

#[test]
fn a_bounded_capture_keeps_exact_bytes_and_survives_reopen() {
    let (scratch, res) = resources("bounded-capture");
    create(&res, JOB, REQUEST_PRETTY).unwrap();
    let total = 33 * MAX_EXPORT_CHUNK_BYTES + 1_234;
    assert!(
        total > MAX_EXPORT_CHANGED_BYTES,
        "must outrun one append's budget"
    );
    let payload = payload(total);
    let chunks = chunks_of(&payload);
    assert_eq!((chunks.len(), chunks[33].len()), (34, 1_234));

    // Several bounded writes: a maximum append of maximum chunks, then
    // two more, ending past 2 MiB of captured bytes.
    let first = chunks[..MAX_EXPORT_CHUNKS].to_vec();
    let job = append(&res, JOB, 1, 0, first.clone()).unwrap();
    assert_eq!((job.revision(), job.captured_chunks()), (2, 30));
    let middle = chunks[MAX_EXPORT_CHUNKS..33].to_vec();
    append(&res, JOB, 2, 30, middle).unwrap();
    let last = chunks[33..].to_vec();
    let job = append(&res, JOB, 3, 33, last).unwrap();
    assert_eq!(job.captured_chunks(), 34);

    // Paged reads return the exact bytes: binary and unicode, across
    // boundaries the chunks split mid-sequence. The final page is empty.
    let mut read_back = Vec::with_capacity(total);
    for (from, limit) in [(0u64, 30u32), (30, 3), (33, 1), (34, 30)] {
        for chunk in res.read(|tx| export_chunks(tx, JOB, from, limit)).unwrap() {
            read_back.extend_from_slice(&chunk);
        }
    }
    assert_eq!(read_back, payload);

    // Paging bounds: past the captured end is invalid, and so are limits
    // outside 1 through 30.
    rejected!(
        res.read(|tx| export_chunks(tx, JOB, 35, 1)),
        invalid,
        "past-end page"
    );
    for limit in [0u32, 31] {
        rejected!(
            res.read(|tx| export_chunks(tx, JOB, 0, limit)),
            invalid,
            "limit {limit}"
        );
    }

    // The first batch still replays exactly after later appends; the
    // sealed ledger then survives close and reopen, bytes and all.
    let job = append(&res, JOB, 1, 0, first).unwrap();
    assert_eq!(job.revision(), 4);
    let job = advance(
        &res,
        JOB,
        4,
        ExportJobState::Ready,
        "{\"v\":1,\"sealed\":true}",
    )
    .unwrap();
    assert_eq!((job.state(), job.revision()), (ExportJobState::Ready, 5));
    res.close().unwrap();
    let res = Resources::open(&scratch.path().join("store")).unwrap();
    let job = res.read(|tx| export_job(tx, JOB)).unwrap();
    assert_eq!(
        (job.state(), job.revision(), job.captured_chunks()),
        (ExportJobState::Ready, 5, 34)
    );
    assert_eq!(job.request_json(), CANONICAL);
    assert_eq!(
        res.read(|tx| export_chunks(tx, JOB, 30, 4)).unwrap(),
        chunks[30..]
    );
    // Repeating creation with the same canonical request still returns
    // the kept job, sealed progress included.
    let job = create(
        &res,
        JOB,
        "{ \"v\" : 1 , \"format\" : \"eml\" , \"scope\" : \"selection\" }",
    )
    .unwrap();
    assert_eq!((job.state(), job.revision()), (ExportJobState::Ready, 5));
}

#[test]
fn transitions_follow_the_chain_and_terminal_jobs_are_immutable() {
    let (_scratch, res) = resources("transitions");
    create(&res, JOB, REQUEST).unwrap();

    // The revision fence: a stale revision never advances.
    rejected!(
        advance(&res, JOB, 99, ExportJobState::Ready, "{\"v\":1}"),
        conflict,
        "a stale revision advanced"
    );

    // Capture may record progress; sealing, execution and completion
    // follow in order.
    let job = advance(
        &res,
        JOB,
        1,
        ExportJobState::Capturing,
        "{ \"v\":1 , \"captured\":2 }",
    )
    .unwrap();
    assert_eq!(
        (job.state(), job.revision(), job.checkpoint_json()),
        (ExportJobState::Capturing, 2, "{\"captured\":2,\"v\":1}")
    );
    advance(
        &res,
        JOB,
        2,
        ExportJobState::Ready,
        "{\"v\":1,\"sealed\":true}",
    )
    .unwrap();
    advance(
        &res,
        JOB,
        3,
        ExportJobState::Running,
        "{\"v\":1,\"running\":true}",
    )
    .unwrap();
    let job = advance(
        &res,
        JOB,
        4,
        ExportJobState::Completed,
        "{\"done\":true,\"v\":1}",
    )
    .unwrap();
    assert_eq!(job.state(), ExportJobState::Completed);

    // Terminal jobs are immutable: no transition, no payload write.
    for next in [
        ExportJobState::Capturing,
        ExportJobState::Ready,
        ExportJobState::Running,
        ExportJobState::Completed,
        ExportJobState::Cancelled,
        ExportJobState::Failed,
    ] {
        rejected!(
            advance(&res, JOB, 5, next, "{\"v\":1}"),
            conflict,
            "{next:?}"
        );
    }
    rejected!(
        append(&res, JOB, 5, 0, vec![b"x".to_vec()]),
        conflict,
        "terminal append"
    );

    // No skipping: capture cannot jump to running or completed, and a
    // sealed job cannot jump straight to completed.
    create(&res, "skip", REQUEST).unwrap();
    for next in [ExportJobState::Running, ExportJobState::Completed] {
        rejected!(
            advance(&res, "skip", 1, next, "{\"v\":1}"),
            conflict,
            "capturing to {next:?}"
        );
    }
    advance(&res, "skip", 1, ExportJobState::Ready, "{\"v\":1}").unwrap();
    rejected!(
        advance(&res, "skip", 2, ExportJobState::Completed, "{\"v\":1}"),
        conflict,
        "sealed straight to completed"
    );

    // Ready seals the payload: appending conflicts and writes nothing.
    rejected!(
        append(
            &res,
            "skip",
            2,
            0,
            vec![b"From MAILER-DAEMON Thu Jan  1 00:00:00 1970\n".to_vec()]
        ),
        conflict,
        "sealed append"
    );
    assert_eq!(
        res.read(|tx| export_job(tx, "skip"))
            .unwrap()
            .captured_chunks(),
        0
    );

    // Cancellation and failure end any unfinished state, then hold.
    let job = advance(&res, "skip", 2, ExportJobState::Cancelled, "{\"v\":1}").unwrap();
    assert_eq!(job.state(), ExportJobState::Cancelled);
    rejected!(
        advance(&res, "skip", 3, ExportJobState::Failed, "{\"v\":1}"),
        conflict,
        "cancel then fail"
    );
    create(&res, "early", REQUEST).unwrap();
    let job = advance(
        &res,
        "early",
        1,
        ExportJobState::Failed,
        "{\"v\":1,\"error\":\"io\"}",
    )
    .unwrap();
    assert_eq!(job.state(), ExportJobState::Failed);

    // An oversized checkpoint is invalid input; running cannot step back.
    create(&res, "late", REQUEST).unwrap();
    let huge = format!(
        "{{\"v\":1,\"pad\":\"{}\"}}",
        "a".repeat(MAX_EXPORT_JSON_BYTES)
    );
    rejected!(
        advance(&res, "late", 1, ExportJobState::Ready, &huge),
        invalid,
        "an oversized checkpoint"
    );
    advance(&res, "late", 1, ExportJobState::Ready, "{\"v\":1}").unwrap();
    advance(&res, "late", 2, ExportJobState::Running, "{\"v\":1}").unwrap();
    rejected!(
        advance(&res, "late", 3, ExportJobState::Ready, "{\"v\":1}"),
        conflict,
        "running stepped back"
    );
    let job = advance(&res, "late", 3, ExportJobState::Cancelled, "{\"v\":1}").unwrap();
    assert_eq!(job.state(), ExportJobState::Cancelled);
}

#[test]
fn a_rolled_back_write_leaves_no_chunks_or_state_change() {
    let (_scratch, res) = resources("rollback");
    create(&res, JOB, REQUEST).unwrap();
    let batch = vec![b"From MAILER-DAEMON Thu Jan  1 00:00:00 1970\n".to_vec()];
    let result = write(&res, move |tx| {
        append_export_chunks(tx, JOB, 1, 0, &batch, at(1))?;
        advance_export_job(tx, JOB, 2, ExportJobState::Cancelled, "{\"v\":1}", at(1))?;
        Err::<(), _>(StoreError::internal("deliberate rollback"))
    });
    let Err(error) = result else {
        panic!("the deliberate rollback vanished")
    };
    assert!(matches!(error, StoreError::Internal { .. }), "{error}");

    let job = res.read(|tx| export_job(tx, JOB)).unwrap();
    assert_eq!(
        (job.state(), job.revision(), job.captured_chunks()),
        (ExportJobState::Capturing, 1, 0)
    );
    assert!(
        res.read(|tx| export_chunks(tx, JOB, 0, 1))
            .unwrap()
            .is_empty()
    );
}

#[test]
fn job_kinds_and_capture_identity_are_guarded_in_sql() {
    let (_scratch, res) = resources("job-kinds");
    create(&res, JOB, REQUEST).unwrap();
    sql(&res, IMPORT_ROW.replace("{id}", IMPORT_ID)).unwrap();
    sql(&res, SEARCH_ROW.replace("{id}", SEARCH_ID)).unwrap();
    append(&res, JOB, 1, 0, vec![b"payload".to_vec()]).unwrap();

    let kinds = res
        .read(|tx| {
            Ok((
                job_kind(tx, JOB)?,
                job_kind(tx, IMPORT_ID)?,
                job_kind(tx, SEARCH_ID)?,
            ))
        })
        .unwrap();
    assert_eq!(
        kinds,
        (JobKind::Export, JobKind::Import, JobKind::SearchRebuild)
    );
    rejected!(
        res.read(|tx| job_kind(tx, "unknown-kind")),
        not_found,
        "an unknown kind"
    );

    // The API refuses to make an import or search id an export.
    for id in [IMPORT_ID, SEARCH_ID] {
        rejected!(create(&res, id, REQUEST), conflict, "{id} became an export");
    }

    // The triggers refuse the same id from the other tables, on insert
    // and on key updates, in every direction.
    for (statement, reason) in [
        (IMPORT_ROW.replace("{id}", JOB), "job id names an export"),
        (SEARCH_ROW.replace("{id}", JOB), "job id names an export"),
        (
            EXPORT_ROW.replace("{id}", IMPORT_ID),
            "job id names another kind",
        ),
        (
            EXPORT_ROW.replace("{id}", SEARCH_ID),
            "job id names another kind",
        ),
        (
            format!("UPDATE import_jobs SET job_id = '{JOB}' WHERE job_id = '{IMPORT_ID}'"),
            "job id names an export",
        ),
        (
            format!("UPDATE search_rebuild_jobs SET job_id = '{JOB}' WHERE job_id = '{SEARCH_ID}'"),
            "job id names an export",
        ),
        (
            format!("UPDATE export_jobs SET job_id = '{IMPORT_ID}' WHERE job_id = '{JOB}'"),
            "export identity is immutable",
        ),
        (
            format!("UPDATE export_jobs SET job_id = '{SEARCH_ID}' WHERE job_id = '{JOB}'"),
            "export identity is immutable",
        ),
        (
            format!("UPDATE export_jobs SET request_json = '{{\"v\":1}}' WHERE job_id = '{JOB}'"),
            "export identity is immutable",
        ),
        (
            format!("INSERT INTO export_job_chunks VALUES ('{JOB}', 3, x'01')"),
            "noncontiguous",
        ),
        (
            format!("UPDATE export_job_chunks SET payload = x'02' WHERE job_id = '{JOB}'"),
            "export payload is immutable",
        ),
        (
            format!("DELETE FROM export_job_chunks WHERE job_id = '{JOB}'"),
            "export payload is retained",
        ),
        (
            format!("DELETE FROM export_jobs WHERE job_id = '{JOB}'"),
            "export history is retained",
        ),
    ] {
        let error = sql(&res, statement).unwrap_err();
        assert!(
            internal(&error) && error.to_string().contains(reason),
            "{error}"
        );
    }

    let job = res.read(|tx| export_job(tx, JOB)).unwrap();
    assert_eq!(
        (job.state(), job.revision(), job.captured_chunks()),
        (ExportJobState::Capturing, 2, 1)
    );
    assert_eq!(
        res.read(|tx| export_chunks(tx, JOB, 0, 1)).unwrap(),
        [b"payload".to_vec()]
    );
}
