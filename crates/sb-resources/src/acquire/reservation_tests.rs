//! Native proof over the real `acquire_import_message` →
//! `commit_import_batch` path: acquiring reserves, and the commit
//! that references the publication releases only its own token —
//! atomically, on first reference and on replay (spec/03).

use std::fs;
use std::ops::ControlFlow;
use std::path::Path;

use sb_store::WritePriority;
use sb_store::imports::{
    ImportBatch, ImportBatchStats, ImportJobState, ImportMessage, advance_import_job,
    commit_import_batch, create_import_job, record_import_source,
};
use sb_test_support::{ScratchDir, block_on};

use super::*;
use crate::{ImportInventoryOutcome, Resources};

const JOB: &str = "00000000-0000-4000-8000-0000000000c4";
const MESSAGE: &[u8] = b"From: sender@example.test\r\nSubject: reserved\r\n\r\nreserved bytes\r\n";

fn resources(label: &str) -> (ScratchDir, Resources) {
    let scratch = ScratchDir::new(label);
    let resources = Resources::open(&scratch.path().join("store")).unwrap();
    (scratch, resources)
}

/// The committed `object_pins` reservation rows in the real store.
fn reservations(res: &Resources) -> i64 {
    res.read(|tx| {
        Ok(tx
            .query_row(
                "SELECT count(*) FROM object_pins WHERE pin_kind = 'reservation'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap())
    })
    .unwrap()
}

/// `SELECT count(*) FROM table` in the real store.
fn rows(res: &Resources, table: &str) -> i64 {
    res.read(|tx| {
        Ok(tx
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap())
    })
    .unwrap()
}

/// Write `MESSAGE` to `path` and inventory it as the one .eml source.
fn eml_source(res: &Resources, path: &Path) -> SourceInventory {
    fs::write(path, MESSAGE).unwrap();
    let outcome =
        res.inventory_import_sources(&[path.to_path_buf()], |_| ControlFlow::Continue(()));
    let ImportInventoryOutcome::Complete(inventory) = outcome else {
        panic!("the inventory was cancelled");
    };
    let [source] = inventory.sources() else {
        panic!("one source expected: {:?}", inventory.sources());
    };
    source.clone()
}

/// The whole acquisition: the published message carrying its
/// reservation, plus the position after it.
fn acquire(res: &Resources, source: &SourceInventory) -> AcquiredMessage {
    let mut acquisition = res
        .open_import_acquisition(source, None, SourcePosition::START)
        .unwrap();
    let message = res
        .acquire_import_message(&mut acquisition)
        .unwrap()
        .unwrap();
    assert!(
        res.acquire_import_message(&mut acquisition)
            .unwrap()
            .is_none(),
        "a one-message source has no second record"
    );
    message
}

/// A running job with `source` recorded at ordinal 0.
fn running_job(res: &Resources, source: &SourceInventory) -> i64 {
    let now = res.now();
    let json = source.to_json();
    block_on(res.write(WritePriority::Sync, move |tx| {
        create_import_job(tx, JOB, "{\"v\":1}", now)?;
        record_import_source(tx, JOB, 0, &json)?;
        advance_import_job(
            tx,
            JOB,
            1,
            ImportJobState::AwaitingConfirmation,
            "{\"v\":1}",
            now,
        )?;
        Ok(advance_import_job(tx, JOB, 2, ImportJobState::Running, "{\"v\":1}", now)?.revision())
    }))
    .unwrap()
}

/// Commit `messages` under `source`'s mailbox at `expected_revision`.
fn commit(
    res: &Resources,
    expected_revision: i64,
    source: &SourceInventory,
    messages: Vec<ImportMessage>,
) -> Result<ImportBatchStats, StoreError> {
    let now = res.now();
    let mailbox = source.mailbox().to_owned();
    block_on(res.write(WritePriority::Sync, move |tx| {
        commit_import_batch(
            tx,
            &ImportBatch {
                job_id: JOB,
                expected_revision,
                source_ordinal: 0,
                mailbox_name: &mailbox,
                messages: &messages,
            },
            now,
            |_| Ok(("{\"v\":1}".to_owned(), "{\"v\":1}".to_owned())),
        )
    }))
}

/// The real acquire → commit releases only the publication's own
/// token, and a re-acquired replay releases only its own: a same-hash
/// bystander's reservation survives both.
#[test]
fn the_real_commit_releases_only_the_publications_own_reservation() {
    let (scratch, res) = resources("reservation-release");
    let source = eml_source(&res, &scratch.path().join("one.eml"));
    let bystander_source = eml_source(&res, &scratch.path().join("two.eml"));

    let message = acquire(&res, &source);
    assert_eq!(reservations(&res), 1, "the acquisition reserved");
    let bystander = acquire(&res, &bystander_source);
    assert_eq!(
        reservations(&res),
        2,
        "same bytes publish distinct lifetimes"
    );

    let revision = running_job(&res, &source);
    commit(&res, revision, &source, vec![message.into_message()]).unwrap();
    assert_eq!(
        reservations(&res),
        1,
        "only the referenced publication's token released"
    );
    assert_eq!(rows(&res, "acquisitions"), 1);

    // Re-acquiring the same source is a replay: the commit confirms
    // the existing reference and releases the new token only.
    let replay = acquire(&res, &source);
    assert_eq!(reservations(&res), 2);
    commit(&res, revision + 1, &source, vec![replay.into_message()]).unwrap();
    assert_eq!(
        reservations(&res),
        1,
        "the bystander's same-hash pin survived first reference and replay"
    );
    drop(bystander);
    res.close().unwrap();
}

/// A commit the batch rejects rolls the release back with the
/// reference: the acquisition's reservation survives (spec/03).
#[test]
fn a_rejected_commit_retains_the_acquisitions_reservation() {
    let (scratch, res) = resources("reservation-rollback");
    let source = eml_source(&res, &scratch.path().join("one.eml"));
    let message = acquire(&res, &source);
    let revision = running_job(&res, &source);
    let outcome = commit(&res, revision + 7, &source, vec![message.into_message()]);
    assert!(outcome.is_err());
    assert_eq!(
        reservations(&res),
        1,
        "a rolled-back reference must retain the reservation"
    );
    assert_eq!(rows(&res, "acquisitions"), 0);
    res.close().unwrap();
}
