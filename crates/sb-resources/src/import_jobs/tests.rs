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
    ] {
        assert!(ImportJobCheckpoint::parse(json).is_err(), "{json}");
    }
    assert!(position_from_json("{\"v\":1,\"offset\":3}").is_err());
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
        let progress = if position.messages() == sources[0].message_count() {
            SourceProgress::Finished
        } else {
            SourceProgress::Partial
        };
        let mailbox = sources[0].mailbox().to_owned();
        let before = checkpoint.clone();
        checkpoint = write(&res, move |tx| {
            let messages = [acquired.into_message()];
            let batch = ImportJobBatch {
                job_id: JOB,
                revision,
                ordinal: 0,
                mailbox: &mailbox,
                messages: &messages,
                position,
                progress,
                flags_incomplete: 0,
            };
            commit_import_job_batch(tx, &batch, &before, now)
        })
        .unwrap();
        revision += 1;
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
