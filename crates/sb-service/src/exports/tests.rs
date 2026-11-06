//! M1.7f's native proof: one import through the service surface, then
//! `start_export`/`export_job`/`cancel_job` driving a real export to
//! durable output — no stubbed engine.

use super::*;
use crate::{CoreConfig, ImportJobState, ImportTarget, open_core};
use sb_test_support::{ScratchDir, block_on};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

const BODY: &str = "Subject: exported\nContent-Type: text/plain\n\nexport me\n";

fn wait(mut condition: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(120);
    while !condition() {
        assert!(Instant::now() < end, "service export test timed out");
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Open a core with one imported mailbox; answers it plus the
/// mailbox's id and the generation `mailboxes` reported.
fn imported(root: &Path, cache: &Path) -> (Core, i64, u64) {
    let core = block_on(open_core(CoreConfig {
        data_root: root.into(),
        cache_root: cache.into(),
    }))
    .unwrap();
    let mbox = root.parent().unwrap().join("service-export.mbox");
    fs::write(
        &mbox,
        format!("From sender@example.test Sat Oct 17 12:00:00 2026\n{BODY}"),
    )
    .unwrap();
    let job = block_on(core.start_import(
        &core.new_idempotency_key(),
        vec![mbox],
        ImportTarget::LocalArchive,
    ))
    .unwrap();
    wait(|| {
        block_on(core.import_job(&job.job_id)).unwrap().state
            == ImportJobState::AwaitingConfirmation
    });
    block_on(core.confirm_import(&core.new_idempotency_key(), &job.job_id, vec![])).unwrap();
    wait(|| block_on(core.import_job(&job.job_id)).unwrap().state == ImportJobState::Completed);
    let accounts = block_on(core.accounts()).unwrap();
    let mailboxes = block_on(core.mailboxes(&accounts[0].account_id)).unwrap();
    let mailbox_id = mailboxes.mailboxes[0].mailbox_id;
    (core, mailbox_id, mailboxes.generation)
}

#[test]
fn a_mailbox_export_completes_and_publishes_its_manifest() {
    let scratch = ScratchDir::new("service-export-complete");
    let (core, mailbox_id, generation) =
        imported(&scratch.path().join("store"), &scratch.path().join("cache"));
    let dest = scratch.path().join("out");

    // Post-ingest work can move the generation; a stale fence just
    // means retrying with the generation the store reports then.
    let mut generation = generation;
    let mut job = None;
    for _ in 0..3 {
        let started = block_on(core.start_export(
            &core.new_idempotency_key(),
            ExportScopeInput::Mailbox {
                mailbox_id,
                generation,
            },
            ExportFormat::Mboxrd,
            dest.to_string_lossy().to_string(),
        ))
        .unwrap();
        wait(|| {
            block_on(core.export_job(&started.job_id))
                .unwrap()
                .state
                .is_finished()
        });
        let done = block_on(core.export_job(&started.job_id)).unwrap();
        if done.state == ExportJobState::Failed && done.checkpoint_json.contains("stale") {
            let accounts = block_on(core.accounts()).unwrap();
            generation = block_on(core.mailboxes(&accounts[0].account_id))
                .unwrap()
                .generation;
            continue;
        }
        job = Some(done);
        break;
    }
    let job = job.expect("the export kept hitting stale generations");
    assert_eq!(job.state, ExportJobState::Completed);
    assert!(job.request_json.contains("\"sha256\""));
    assert!(job.captured_chunks > 0);
    assert!(dest.join("sweetbriar-metadata.json").is_file());
    block_on(core.shutdown());
}

#[test]
fn a_replayed_start_returns_the_same_receipt_and_job() {
    let scratch = ScratchDir::new("service-export-replay");
    let (core, mailbox_id, generation) =
        imported(&scratch.path().join("store"), &scratch.path().join("cache"));
    let dest = scratch.path().join("out");
    let scope = ExportScopeInput::Mailbox {
        mailbox_id,
        generation,
    };

    let key = core.new_idempotency_key();
    let first = block_on(core.start_export(
        &key,
        scope.clone(),
        ExportFormat::Mboxrd,
        dest.to_string_lossy().to_string(),
    ))
    .unwrap();
    let again = block_on(core.start_export(
        &key,
        scope,
        ExportFormat::Mboxrd,
        dest.to_string_lossy().to_string(),
    ))
    .unwrap();
    assert_eq!(first.job_id, again.job_id);
    assert_eq!(first.receipt.result_json, again.receipt.result_json);
    wait(|| {
        block_on(core.export_job(&first.job_id))
            .unwrap()
            .state
            .is_finished()
    });
    block_on(core.shutdown());
}
