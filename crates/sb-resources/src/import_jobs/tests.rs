//! Tests for typed import-job payloads (M1.2c3a): checkpoint JSON,
//! variant confirmation rules, and one job's rows through a real store.

use std::fs;
use std::ops::ControlFlow;
use std::path::Path;

use sb_store::WritePriority;
use sb_test_support::{ScratchDir, block_on};

use super::*;
use crate::{ImportInventoryOutcome, Resources};

const JOB: &str = "00000000-0000-4000-8000-0000000000c3";

fn resources(label: &str) -> (ScratchDir, Resources) {
    let scratch = ScratchDir::new(label);
    let resources = Resources::open(&scratch.path().join("store")).unwrap();
    (scratch, resources)
}

fn inventory(res: &Resources, roots: &[PathBuf]) -> (Vec<SourceInventory>, Vec<ImportProblem>) {
    let ImportInventoryOutcome::Complete(inventory) =
        res.inventory_import_sources(roots, |_| ControlFlow::Continue(()))
    else {
        panic!("the inventory was cancelled");
    };
    (inventory.sources().to_vec(), inventory.problems().to_vec())
}

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/import")
}

fn write<T: Send + 'static>(
    res: &Resources,
    f: impl FnOnce(&Transaction) -> Result<T, StoreError> + Send + 'static,
) -> Result<T, StoreError> {
    block_on(res.write(WritePriority::Sync, f))
}

#[test]
fn checkpoints_round_trip_and_fresh_rows_read_as_defaults() {
    let (scratch, res) = resources("checkpoint");
    let missing = scratch.path().join("missing.mbox");
    let (sources, problems) = inventory(
        &res,
        &[
            fixtures().join("quoting.mbox"),
            fixtures().join("Inbox.mbox"),
            missing,
        ],
    );
    let checkpoint = ImportJobCheckpoint::inventoried(&sources, &problems)
        .with_variants(&[(0, MboxVariant::Mboxo)])
        .unwrap()
        .with_failure("stopped");
    assert_eq!(checkpoint.sources(), 2);
    assert_eq!(checkpoint.ambiguous(), [0]);
    assert_eq!(checkpoint.variant(0), Some(MboxVariant::Mboxo));
    assert_eq!(checkpoint.variant(1), None);
    assert_eq!(checkpoint.problem_count(), 1);
    assert_eq!(
        checkpoint.problems()[0].kind(),
        ImportProblemKind::Inaccessible
    );

    let json = checkpoint.to_json();
    let parsed = ImportJobCheckpoint::parse(&json).unwrap();
    assert_eq!(parsed.to_json(), json);
    assert_eq!(parsed.failure(), Some("stopped"));

    let fresh = ImportJobCheckpoint::parse("{\"v\":1}").unwrap();
    assert_eq!((fresh.sources(), fresh.next()), (0, 0));
    assert_eq!(fresh.totals(), ImportTotals::default());
    assert_eq!(
        position_from_json("{\"v\":1}").unwrap(),
        SourcePosition::START
    );
}

#[test]
fn malformed_checkpoints_are_refused() {
    for json in [
        "[]",
        "{\"v\":2}",
        "{\"v\":1,\"sources\":1,\"next\":2}",
        "{\"v\":1,\"sources\":2,\"ambiguous\":[1,0]}",
        "{\"v\":1,\"sources\":2,\"ambiguous\":[2]}",
        "{\"v\":1,\"sources\":2,\"ambiguous\":[1],\"variants\":[[0,\"mboxo\"]]}",
        "{\"v\":1,\"sources\":2,\"ambiguous\":[1],\"variants\":[[1,\"mbox\"]]}",
        "{\"v\":1,\"problems\":[{\"path\":\"/x\",\"kind\":\"odd\",\"detail\":\"\"}]}",
        "{\"v\":1,\"totals\":{\"processed\":1}}",
        "{\"v\":1,\"failure\":3}",
        "{\"v\":1,\"sources\":2,\"ambiguous\":[1],\"variants\":[[1,\"mboxo\"],[1,\"mboxrd\"]]}",
        "{\"v\":1,\"problem_count\":0,\"problems\":[{\"path\":\"/x\",\"kind\":\"failed\",\"detail\":\"\"}]}",
    ] {
        assert!(ImportJobCheckpoint::parse(json).is_err(), "{json}");
    }
    assert!(position_from_json("{\"v\":1,\"offset\":3}").is_err());
    assert!(position_from_json("{\"v\":1,\"messages\":3}").is_err());
    assert_eq!(
        position_from_json("{\"v\":1,\"offset\":0,\"messages\":0}").unwrap(),
        SourcePosition::START
    );
}

#[test]
fn variant_choices_must_name_exactly_the_ambiguous_sources() {
    let (_scratch, res) = resources("variants");
    let (sources, _) = inventory(
        &res,
        &[
            fixtures().join("Inbox.mbox"),
            fixtures().join("quoting.mbox"),
        ],
    );
    let checkpoint = ImportJobCheckpoint::inventoried(&sources, &[]);
    assert_eq!(checkpoint.ambiguous(), [1]);
    for choices in [
        &[][..],
        &[(0, MboxVariant::Mboxo)],
        &[(1, MboxVariant::Mboxo), (1, MboxVariant::Mboxrd)],
        &[(1, MboxVariant::Mboxo), (0, MboxVariant::Mboxrd)],
    ] {
        assert!(checkpoint.with_variants(choices).is_err(), "{choices:?}");
    }
    let chosen = checkpoint
        .with_variants(&[(1, MboxVariant::Mboxrd)])
        .unwrap();
    assert_eq!(chosen.variant(1), Some(MboxVariant::Mboxrd));
}

#[test]
fn a_job_records_sources_and_commits_batches_with_both_checkpoints() {
    let (scratch, res) = resources("job");
    let path = scratch.path().join("Two.mbox");
    fs::write(
        &path,
        b"From a Sat Oct 17 12:00:00 2026\nStatus: RO\n\none\nFrom b Sat Oct 17 12:00:00 2026\n\ntwo\n",
    )
    .unwrap();
    let (sources, problems) = inventory(&res, std::slice::from_ref(&path));
    let now = res.now();
    let paths = vec![path];
    let recorded = sources.clone();
    let job = write(&res, move |tx| {
        create_import_job(tx, JOB, &paths, now)?;
        record_import_sources(tx, JOB, 0, &recorded)?;
        let job = import_job(tx, JOB)?;
        let checkpoint = ImportJobCheckpoint::inventoried(&recorded, &problems);
        let job = advance_import_job(
            tx,
            JOB,
            job.revision(),
            ImportJobState::AwaitingConfirmation,
            &checkpoint,
            now,
        )?;
        advance_import_job(
            tx,
            JOB,
            job.revision(),
            ImportJobState::Running,
            job.checkpoint(),
            now,
        )
    })
    .unwrap();
    assert_eq!(job.state(), ImportJobState::Running);
    assert!(!job.is_finished());

    // Acquire both messages, then commit them as two one-message batches.
    let mut acquisition = res
        .open_import_acquisition(&sources[0], None, SourcePosition::START)
        .unwrap();
    let mut revision = job.revision();
    let mut checkpoint = job.checkpoint().clone();
    for expected_next in [0, 1] {
        let acquired = res
            .acquire_import_message(&mut acquisition)
            .unwrap()
            .unwrap();
        let position = acquired.next_position();
        let mailbox = sources[0].mailbox().to_owned();
        let job = write(&res, move |tx| {
            let messages = [acquired.into_message()];
            let batch = ImportJobBatch {
                job_id: JOB,
                revision,
                ordinal: 0,
                mailbox: &mailbox,
                messages: &messages,
                position,
                flags_incomplete: 0,
            };
            commit_import_job_batch(tx, &batch, now)
        })
        .unwrap();
        revision += 1;
        assert_eq!(job.revision(), revision);
        checkpoint = job.checkpoint().clone();
        assert_eq!(checkpoint.next(), expected_next);
        let (_, stored) = res.read(|tx| import_job_source(tx, JOB, 0)).unwrap();
        assert_eq!(stored, position);
    }
    let totals = checkpoint.totals();
    assert_eq!((totals.processed, totals.imported), (2, 2));
    assert_eq!((totals.read_from_headers, totals.read_default), (1, 1));

    // A stale revision rolls back as `Conflict`.
    let stale = checkpoint.clone();
    let error = write(&res, move |tx| {
        advance_import_job(
            tx,
            JOB,
            revision - 1,
            ImportJobState::Completed,
            &stale,
            now,
        )
    })
    .unwrap_err();
    assert!(matches!(error, StoreError::Conflict { .. }), "{error:?}");
    let job = res.read(|tx| import_job(tx, JOB)).unwrap();
    assert_eq!(job.revision(), revision);
    assert_eq!(job.checkpoint().to_json(), checkpoint.to_json());
}

/// A running job over `files` (name, mbox bytes); its revision.
fn running_job(
    res: &Resources,
    scratch: &ScratchDir,
    files: &[(&str, &[u8])],
) -> (Vec<SourceInventory>, i64) {
    let paths: Vec<PathBuf> = files
        .iter()
        .map(|(name, bytes)| {
            let path = scratch.path().join(name);
            fs::write(&path, bytes).unwrap();
            path
        })
        .collect();
    let (sources, problems) = inventory(res, &paths);
    let now = res.now();
    let recorded = sources.clone();
    let job = write(res, move |tx| {
        create_import_job(tx, JOB, &paths, now)?;
        record_import_sources(tx, JOB, 0, &recorded)?;
        let checkpoint = ImportJobCheckpoint::inventoried(&recorded, &problems);
        let waiting = ImportJobState::AwaitingConfirmation;
        let job = advance_import_job(tx, JOB, 1, waiting, &checkpoint, now)?;
        advance_import_job(tx, JOB, 2, ImportJobState::Running, job.checkpoint(), now)
    })
    .unwrap();
    (sources, job.revision())
}

const TWO: &[u8] =
    b"From a Sat Oct 17 12:00:00 2026\n\none\nFrom b Sat Oct 17 12:00:00 2026\n\ntwo\n";
const ONE: &[u8] = b"From a Sat Oct 17 12:00:00 2026\n\nonly\n";

#[test]
fn a_batch_must_continue_the_next_source() {
    let (scratch, res) = resources("batch-order");
    let (sources, revision) = running_job(&res, &scratch, &[("Two.mbox", TWO), ("One.mbox", ONE)]);
    let now = res.now();
    let mailbox = sources[1].mailbox().to_owned();
    let end = SourcePosition::new(sources[1].snapshot().byte_length(), 1);
    // Source 1 claims to finish while source 0 has not started.
    let error = write(&res, move |tx| {
        let batch = ImportJobBatch {
            job_id: JOB,
            revision,
            ordinal: 1,
            mailbox: &mailbox,
            messages: &[],
            position: end,
            flags_incomplete: 0,
        };
        commit_import_job_batch(tx, &batch, now)
    })
    .unwrap_err();
    assert!(matches!(error, StoreError::Internal { .. }), "{error:?}");
    let job = res.read(|tx| import_job(tx, JOB)).unwrap();
    assert_eq!((job.checkpoint().next(), job.revision()), (0, revision));
}

#[test]
fn batch_totals_build_on_the_stored_checkpoint() {
    let (scratch, res) = resources("batch-totals");
    let (sources, revision) = running_job(&res, &scratch, &[("Two.mbox", TWO)]);
    let mut acquisition = res
        .open_import_acquisition(&sources[0], None, SourcePosition::START)
        .unwrap();
    let now = res.now();
    for step in 0..2 {
        let acquired = res
            .acquire_import_message(&mut acquisition)
            .unwrap()
            .unwrap();
        let position = acquired.next_position();
        let mailbox = sources[0].mailbox().to_owned();
        write(&res, move |tx| {
            let messages = [acquired.into_message()];
            let batch = ImportJobBatch {
                job_id: JOB,
                revision: revision + step,
                ordinal: 0,
                mailbox: &mailbox,
                messages: &messages,
                position,
                flags_incomplete: 0,
            };
            // The base is the stored checkpoint, never the caller's.
            commit_import_job_batch(tx, &batch, now)
        })
        .unwrap();
    }
    let job = res.read(|tx| import_job(tx, JOB)).unwrap();
    assert_eq!(job.checkpoint().totals().processed, 2);
    assert_eq!(
        (job.checkpoint().sources(), job.checkpoint().next()),
        (1, 1)
    );
}

#[test]
fn one_undecodable_job_does_not_hide_the_others() {
    let (scratch, res) = resources("recover-corrupt");
    running_job(&res, &scratch, &[("One.mbox", ONE)]);
    let now = res.now();
    write(&res, move |tx| {
        create_import_job(tx, "00000000-0000-4000-8000-0000000000c5", &[], now)?;
        // Damage the first job's checkpoint behind the store's back.
        tx.execute(
            "UPDATE import_jobs SET checkpoint_json = '{\"v\":1,\"sources\":1,\"next\":5}' WHERE job_id = ?1",
            [JOB],
        )
        .map(drop)
        .map_err(|e| StoreError::internal(e.to_string()))
    })
    .unwrap();
    let jobs = res.read(recoverable_import_jobs).unwrap();
    assert_eq!(jobs.len(), 2);
    assert!(jobs.iter().any(|job| job.is_ok()));
    assert!(
        jobs.iter()
            .any(|job| matches!(job, Err(StoreError::Corrupt { .. })))
    );
    let error = res.read(|tx| import_job(tx, JOB)).unwrap_err();
    assert!(matches!(error, StoreError::Corrupt { .. }), "{error:?}");
}

#[test]
fn only_the_first_problems_are_listed_but_all_are_counted() {
    let (scratch, res) = resources("problems");
    let missing: Vec<PathBuf> = (0..LISTED_PROBLEMS + 5)
        .map(|n| scratch.path().join(format!("missing-{n}.mbox")))
        .collect();
    let (sources, problems) = inventory(&res, &missing);
    let checkpoint = ImportJobCheckpoint::inventoried(&sources, &problems);
    assert_eq!(checkpoint.problem_count(), (LISTED_PROBLEMS + 5) as u64);
    assert_eq!(checkpoint.problems().len(), LISTED_PROBLEMS);
    let parsed = ImportJobCheckpoint::parse(&checkpoint.to_json()).unwrap();
    assert_eq!(parsed.problems().len(), LISTED_PROBLEMS);
}

#[test]
fn an_oversized_request_is_refused_before_writing() {
    let (_scratch, res) = resources("request-budget");
    let now = res.now();
    let huge = vec![PathBuf::from(
        "/".to_owned() + &"x".repeat(MAX_IMPORT_CHANGES_BYTES),
    )];
    let error = write(&res, move |tx| create_import_job(tx, JOB, &huge, now)).unwrap_err();
    assert!(
        matches!(error, StoreError::InvalidInput { .. }),
        "{error:?}"
    );
    assert!(matches!(
        res.read(|tx| import_job(tx, JOB)),
        Err(StoreError::NotFound { .. })
    ));
}

/// A report stays inside the budget its readers allow, even for names
/// JSON escapes at six bytes a byte, and the budget grows per source.
#[test]
fn a_report_fits_its_budget() {
    let (scratch, res) = resources("report-budget");
    let name = format!("{}.mbox", "\u{1}".repeat(240));
    let (sources, _) = running_job(&res, &scratch, &[(name.as_str(), TWO)]);
    let job = res.read(|tx| import_job(tx, JOB)).unwrap();
    let rows = vec![(sources[0].clone(), SourcePosition::START)];
    let report = import_report(&job, &rows);
    assert!(report.contains("\\u0001"), "the name is escaped");
    assert!(report.len() as u64 <= import_report_budget(rows.len()));
    assert!(import_report_budget(2) > import_report_budget(1));
}

#[test]
fn a_report_lists_files_counts_and_warnings() {
    let (scratch, res) = resources("report");
    let (sources, revision) = running_job(&res, &scratch, &[("Two.mbox", TWO)]);
    let now = res.now();
    let job = write(&res, move |tx| {
        let job = import_job(tx, JOB)?;
        advance_import_job(
            tx,
            JOB,
            revision,
            ImportJobState::Completed,
            job.checkpoint(),
            now,
        )
    })
    .unwrap();
    let rows = vec![(sources[0].clone(), SourcePosition::START)];
    let report: Value = serde_json::from_str(&import_report(&job, &rows)).unwrap();
    assert_eq!(report["v"], 1);
    assert_eq!(report["job_id"], JOB);
    assert_eq!(report["state"], "completed");
    assert_eq!(report["failure"], Value::Null);
    let file = &report["files"][0];
    assert_eq!(file["mailbox"], "Two");
    assert_eq!(file["kind"], "mbox");
    assert_eq!(file["sha256"], sources[0].snapshot().sha256_hex());
    assert_eq!(file["bytes"], TWO.len());
    assert_eq!(file["messages"], 2);
    assert_eq!(file["variant"], Value::Null);
    assert_eq!(report["messages"]["found"], 2);
    assert_eq!(report["bytes"]["in"], TWO.len());
    assert_eq!(report["read_state"]["archive_default"], 0);
    assert_eq!(report["warnings"].as_array().unwrap().len(), 1);
}

/// Commit `messages` from acquisition at `position` for source 0.
fn commit_at(
    res: &Resources,
    sources: &[SourceInventory],
    revision: i64,
    messages: Vec<ImportMessage>,
    position: SourcePosition,
    flags_incomplete: u64,
) -> Result<ImportJobRecord, StoreError> {
    let mailbox = sources[0].mailbox().to_owned();
    let now = res.now();
    write(res, move |tx| {
        let batch = ImportJobBatch {
            job_id: JOB,
            revision,
            ordinal: 0,
            mailbox: &mailbox,
            messages: &messages,
            position,
            flags_incomplete,
        };
        commit_import_job_batch(tx, &batch, now)
    })
}

#[test]
fn a_stale_batch_is_a_conflict_and_impossible_ones_are_refused() {
    let (scratch, res) = resources("batch-claims");
    let (sources, revision) = running_job(&res, &scratch, &[("Two.mbox", TWO)]);
    let mut acquisition = res
        .open_import_acquisition(&sources[0], None, SourcePosition::START)
        .unwrap();
    let mut next = || {
        res.acquire_import_message(&mut acquisition)
            .unwrap()
            .unwrap()
    };
    let first = next();
    let after_first = first.next_position();
    let second = next();
    let end = second.next_position();
    let first = vec![first.into_message()];
    let job = commit_at(&res, &sources, revision, first, after_first, 0).unwrap();

    // Positions must advance by exactly the batch's messages, within
    // the inventory, and incomplete flags cannot exceed the messages.
    let current = job.revision();
    let beyond = SourcePosition::new(sources[0].snapshot().byte_length(), 3);
    for (position, flags) in [(SourcePosition::START, 0), (beyond, 0), (after_first, 1)] {
        let error = commit_at(&res, &sources, current, vec![], position, flags).unwrap_err();
        assert!(matches!(error, StoreError::Internal { .. }), "{error:?}");
    }

    // Finish the source, then replay a batch the job has moved past:
    // a quiet conflict, never an invariant failure that fails the job.
    let second = vec![second.into_message()];
    let job = commit_at(&res, &sources, current, second, end, 0).unwrap();
    assert_eq!(job.checkpoint().next(), 1);
    let stale = commit_at(&res, &sources, current, vec![], after_first, 0).unwrap_err();
    assert!(matches!(stale, StoreError::Conflict { .. }), "{stale:?}");
    let job = res.read(|tx| import_job(tx, JOB)).unwrap();
    assert_eq!(job.checkpoint().totals().processed, 2);

    // A job no longer running conflicts even at its current revision.
    let now = res.now();
    let cancelled = write(&res, move |tx| {
        let job = import_job(tx, JOB)?;
        let state = ImportJobState::Cancelled;
        advance_import_job(tx, JOB, job.revision(), state, job.checkpoint(), now)
    })
    .unwrap();
    let ended = commit_at(&res, &sources, cancelled.revision(), vec![], end, 0).unwrap_err();
    assert!(matches!(ended, StoreError::Conflict { .. }), "{ended:?}");
}

#[test]
fn bounded_mime_predicates_follow_summary_completeness() {
    use sb_mime::Limits;
    use sb_store::imports::ImportParseState;
    use sb_store::query::{Query, SearchScope, counts};
    use serde_json::json;

    const MIXED: &[u8] = include_bytes!("../../../../fixtures/eml/multipart-mixed.eml");
    const FORWARD: &[u8] = include_bytes!("../../../../fixtures/eml/rfc822-forward.eml");
    const FLOWED: &[u8] = include_bytes!("../../../../fixtures/eml/format-flowed.eml");
    const CAROL: &str = "carol@example.com";
    const IVAN: &str = "ivan@example.com";

    // Separate stores keep complete controls from replacing partial summaries
    // for the same content hash. The forward retains its attachment before cut.
    for (name, bytes, cap, expected_state, attachment, from) in [
        ("cut.eml", MIXED, Some(2), "partial", false, CAROL),
        ("full.eml", MIXED, None, "ok", true, CAROL),
        ("cut.eml", FORWARD, Some(3), "partial", true, IVAN),
        ("full.eml", FORWARD, None, "ok", true, IVAN),
        ("absent.eml", FLOWED, None, "ok", false, "frank@example.com"),
    ] {
        let (scratch, res) = resources("predicate-mime");
        // Start after version reconciliation, like the index facade fixtures.
        write(&res, |tx| {
            tx.execute(
                "UPDATE store_meta SET value=?1 WHERE key='search_extractor_version'",
                [crate::SEARCH_EXTRACTOR_VERSION.to_string()],
            )
            .map(drop)
            .map_err(|error| StoreError::internal(error.to_string()))
        })
        .unwrap();
        let (sources, revision) = running_job(&res, &scratch, &[(name, bytes)]);
        let mut opened = res
            .open_import_acquisition(&sources[0], None, SourcePosition::START)
            .unwrap();
        let limits = cap.map_or_else(Limits::default, |parts| {
            Limits::default().with_max_parts(parts)
        });
        let acquired = crate::acquire::next(&res.store, &mut opened, &limits)
            .unwrap()
            .unwrap();
        let summary = &acquired.message().summary;
        let state = match summary.parse_state {
            ImportParseState::Parsed => "ok",
            ImportParseState::Partial => "partial",
            ImportParseState::Failed => "failed",
        };
        assert_eq!(state, expected_state, "{name}");
        assert_eq!(summary.has_attachments, attachment, "{name}");
        assert_eq!(summary.from_addr.as_deref(), Some(from), "{name}");

        let position = acquired.next_position();
        commit_at(
            &res,
            &sources,
            revision,
            vec![acquired.into_message()],
            position,
            0,
        )
        .unwrap();
        let stored = res
            .read(|tx| {
                tx.query_row(
                    "SELECT parse_state,has_attachments FROM d_message_summary",
                    [],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, bool>(1)?)),
                )
                .map_err(|error| StoreError::internal(error.to_string()))
            })
            .unwrap();
        assert_eq!(stored, (expected_state.to_owned(), attachment), "{name}");

        let crate::IndexPreparation::Prepared(prepared) = res
            .prepare_index_batch(res.pending_index_batch().unwrap(), &|| false)
            .unwrap()
        else {
            panic!("expected index preparation");
        };
        let (fence, publications, failure) = prepared.into_parts();
        assert!(failure.is_none());
        let published = block_on(res.write(WritePriority::Index, move |tx| {
            crate::publish_batch(tx, fence, &publications)
        }))
        .unwrap();
        assert_eq!(published, 1);
        let count = |node| {
            res.read(|tx| {
                let query = Query::parse_json(&json!({"v":1,"q":node}).to_string()).unwrap();
                counts(tx, &query, SearchScope::All)
                    .map(|value| (value.matching_messages(), value.excluded_unknown()))
            })
            .unwrap()
        };
        let unknown = u64::from(expected_state == "partial");
        let known = 1 - unknown;
        let retained = json!({"field":"from","op":"contains","value":from});
        assert_eq!(count(retained.clone()), (1, 0));
        for (node, hits) in [
            (json!({"has":"attachment"}), u64::from(attachment) * known),
            (json!({"field":"from","op":"is","value":from}), known),
            (
                json!({"field":"from","op":"domain","value":"example.com"}),
                known,
            ),
            (
                json!({"field":"from","op":"is","value":"absent@unknown.test"}),
                0,
            ),
            (
                json!({"field":"from","op":"domain","value":"unknown.test"}),
                0,
            ),
        ] {
            assert_eq!(count(node.clone()), (hits, unknown), "{name} {node}");
            assert_eq!(count(json!({"not":node.clone()})), (known - hits, unknown));
            assert_eq!(
                count(json!({"and":[retained.clone(),node]})),
                (hits, unknown)
            );
        }
        assert_eq!(count(json!({"or":[retained,{"has":"attachment"}]})), (1, 0));
        if unknown == 0 {
            continue;
        }

        for header in [Some(r#"[{"address":"known@hdr.test"}]"#), None] {
            write(&res, move |tx| {
                tx.execute(
                    "INSERT OR REPLACE INTO message_headers
                    (message_id,raw_fields,from_json,fetched_at) VALUES(1,x'01',?1,1)",
                    [header],
                )
                .map(drop)
                .map_err(|error| StoreError::internal(error.to_string()))
            })
            .unwrap();
            assert_eq!(
                count(json!({"field":"from","op":"is","value":from})),
                (0, 0)
            );
            assert_eq!(
                count(json!({"not":{"field":"from","op":"is","value":from}})),
                (1, 0)
            );
            assert_eq!(
                count(json!({"field":"from","op":"domain","value":"hdr.test"})),
                (u64::from(header.is_some()), 0)
            );
            assert_eq!(count(json!({"has":"attachment"})), (0, 1));
        }
    }
}
