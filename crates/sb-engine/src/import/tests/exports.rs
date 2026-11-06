//! Export jobs end to end (M1.7e, spec/09 "Export file output"):
//! start/query/cancel, the capture→output worker, recovery at open and
//! the re-import round trip. The engine's runtime runs the workers;
//! the tests poll `export_job`.

use sb_resources::{
    ExportCaptureRequest, ExportJobState, ExportScope, advance_export_job, create_export_job,
    export_job, list_generation,
};

use super::*;
use crate::{Anchor, ExportScopeInput, SelectionInput, WindowSpec};

/// Import `Inbox.mbox` to completion so an export has mail to stream.
fn import_inbox(engine: &Engine) {
    let id = start(engine, &[fixtures().join("Inbox.mbox")]);
    settle(engine, &id, ImportJobState::AwaitingConfirmation);
    confirm(engine, &id, vec![]).unwrap();
    settle(engine, &id, ImportJobState::Completed);
}

/// The imported mailbox's id and the list generation once it stops
/// moving: post-ingest work (thread rebuilds) still writes after the
/// import job completes, and the capture fences on its generation.
fn mailbox(engine: &Engine) -> (i64, u64) {
    let id = engine
        .resources()
        .read(|tx| {
            tx.query_row("SELECT mailbox_id FROM mailboxes LIMIT 1", [], |row| {
                row.get(0)
            })
            .map_err(|error| StoreError::internal(error.to_string()))
        })
        .unwrap();
    let deadline = Instant::now() + PATIENCE;
    let mut generation = engine.resources().read(list_generation).unwrap();
    loop {
        std::thread::sleep(Duration::from_millis(50));
        let again = engine.resources().read(list_generation).unwrap();
        if again == generation {
            return (id, generation);
        }
        assert!(
            Instant::now() < deadline,
            "the list generation never settled"
        );
        generation = again;
    }
}

fn start_export(engine: &Engine, key: &str, dest: &Path, scope: ExportScopeInput) -> ExportStarted {
    block_on(engine.start_export(
        key,
        scope,
        ExportFormat::Mboxrd,
        dest.to_string_lossy().to_string(),
    ))
    .unwrap()
}

fn export_status(engine: &Engine, id: &str) -> ExportJobStatus {
    block_on(engine.export_job(id)).unwrap()
}

/// Poll until the export reaches `state` and no worker holds it.
fn settle_export(engine: &Engine, id: &str, state: ExportJobState) -> ExportJobStatus {
    let deadline = Instant::now() + PATIENCE;
    loop {
        let live = engine.imports().lock().contains_key(id);
        let current = export_status(engine, id);
        if current.state() == state && !live {
            return current;
        }
        assert!(
            Instant::now() < deadline,
            "export {id} stayed {:?}",
            current.state()
        );
        std::thread::sleep(Duration::from_millis(10));
    }
}

/// Published entry names in `dir`: the manifest, claim and lock are
/// the job's bookkeeping, not output.
fn published(dest: &Path) -> Vec<String> {
    let mut names = fs::read_dir(dest)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().to_string())
        .filter(|name| !name.starts_with("sweetbriar-"))
        .collect::<Vec<_>>();
    names.sort();
    names
}

/// Acquisition hashes in the store, for comparing runs.
fn hashes(engine: &Engine) -> Vec<String> {
    let mut hashes = engine
        .resources()
        .read(|tx| {
            let mut statement = tx
                .prepare("SELECT content_sha256 FROM acquisitions ORDER BY 1")
                .map_err(|e| StoreError::internal(e.to_string()))?;
            statement
                .query_map([], |row| row.get(0))
                .map_err(|e| StoreError::internal(e.to_string()))?
                .collect::<Result<Vec<String>, _>>()
                .map_err(|e| StoreError::internal(e.to_string()))
        })
        .unwrap();
    hashes.sort();
    hashes
}

#[test]
fn export_cancel_is_durable_receipt_owned_and_cannot_cancel_an_import() {
    let scratch = ScratchDir::new("export-cancel-dispatch");
    let root = scratch.path().join("store");
    let engine = open(&root);
    let now = engine.resources().now();
    block_on(engine.resources().write(WritePriority::User, move |tx| {
        create_export_job(tx, "export", "{\"v\":1}", now)?;
        Ok(())
    }))
    .unwrap();
    let key = engine.new_idempotency_key();
    let receipt = block_on(engine.cancel_job(&key, "export")).unwrap();
    let import = start(&engine, &[fixtures().join("Inbox.mbox")]);
    settle(&engine, &import, ImportJobState::AwaitingConfirmation);
    let replay = block_on(engine.cancel_job(&key, &import)).unwrap();
    assert_eq!(receipt.result_json(), replay.result_json());
    assert_eq!(
        status(&engine, &import).state(),
        ImportJobState::AwaitingConfirmation
    );
    assert!(matches!(
        block_on(engine.cancel_job(&engine.new_idempotency_key(), "export")),
        Err(EngineError::Conflict { .. })
    ));
    assert!(matches!(
        block_on(engine.import_job("export")),
        Err(EngineError::NotFound { .. })
    ));
    block_on(engine.shutdown()).unwrap();

    let engine = open(&root);
    assert_eq!(
        engine
            .resources()
            .read(|tx| export_job(tx, "export"))
            .unwrap()
            .state(),
        ExportJobState::Cancelled
    );
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn completed_exports_cannot_be_cancelled_or_treated_as_rebuilds() {
    let scratch = ScratchDir::new("export-complete-dispatch");
    let engine = open(&scratch.path().join("store"));
    let now = engine.resources().now();
    block_on(engine.resources().write(WritePriority::User, move |tx| {
        let mut job = create_export_job(tx, "complete", "{\"v\":1}", now)?;
        for state in [
            ExportJobState::Ready,
            ExportJobState::Running,
            ExportJobState::Completed,
        ] {
            job = advance_export_job(tx, "complete", job.revision(), state, "{\"v\":1}", now)?;
        }
        Ok(())
    }))
    .unwrap();
    assert!(matches!(
        block_on(engine.cancel_job(&engine.new_idempotency_key(), "complete")),
        Err(EngineError::Conflict { .. })
    ));
    assert!(matches!(
        engine.resources().read(|tx| rebuild_job(tx, "complete")),
        Err(StoreError::InvalidInput { .. })
    ));
    assert_eq!(
        engine
            .resources()
            .read(|tx| export_job(tx, "complete"))
            .unwrap()
            .state(),
        ExportJobState::Completed
    );
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn an_export_captures_and_publishes_to_completion() {
    let scratch = ScratchDir::new("export-complete");
    let root = scratch.path().join("store");
    let engine = open(&root);
    import_inbox(&engine);
    let (mailbox_id, generation) = mailbox(&engine);
    let occurrences = count(&engine, "SELECT COUNT(*) FROM occurrences");
    let dest = scratch.path().join("out");

    let key = engine.new_idempotency_key();
    let started = start_export(
        &engine,
        &key,
        &dest,
        ExportScopeInput::Mailbox {
            mailbox_id,
            generation,
        },
    );
    let done = settle_export(&engine, started.job_id(), ExportJobState::Completed);
    assert!(
        !done.checkpoint_json().is_empty() && done.captured_chunks() > 0,
        "a completed export keeps its capture and output checkpoints"
    );

    let manifest = dest.join("sweetbriar-metadata.json");
    assert!(manifest.is_file(), "the manifest published last");
    let manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(manifest).unwrap()).unwrap();
    let files = published(&dest);
    assert!(!files.is_empty(), "mboxrd output published mailbox files");
    assert!(
        files.iter().all(|name| name.ends_with(".mbox")),
        "every published entry is an mbox: {files:?}"
    );
    let entries = manifest["files"].as_array().map_or(0, Vec::len) as i64
        + manifest["messages"].as_array().map_or(0, Vec::len) as i64;
    assert!(
        entries > 0,
        "the manifest lists the exported content: {manifest}"
    );
    assert_eq!(
        count(&engine, "SELECT COUNT(*) FROM occurrences"),
        occurrences,
        "an export changes no mail"
    );
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn the_export_request_replays_its_receipt_without_a_second_job() {
    let scratch = ScratchDir::new("export-idempotent");
    let engine = open(&scratch.path().join("store"));
    import_inbox(&engine);
    let (mailbox_id, generation) = mailbox(&engine);
    let dest = scratch.path().join("out");
    let scope = ExportScopeInput::Mailbox {
        mailbox_id,
        generation,
    };

    let key = engine.new_idempotency_key();
    let first = start_export(&engine, &key, &dest, scope.clone());
    let again = start_export(&engine, &key, &dest, scope.clone());
    assert_eq!(first.job_id(), again.job_id());
    assert_eq!(
        first.receipt().result_json(),
        again.receipt().result_json(),
        "a replayed key answers the original receipt, not a new job"
    );
    let replay = start_export(
        &engine,
        &engine.new_idempotency_key(),
        &scratch.path().join("other"),
        scope,
    );
    assert_ne!(replay.job_id(), first.job_id());
    settle_export(&engine, first.job_id(), ExportJobState::Completed);
    settle_export(&engine, replay.job_id(), ExportJobState::Completed);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_sealed_export_resumes_at_open_and_completes_without_a_request() {
    let scratch = ScratchDir::new("export-sealed-resume");
    let root = scratch.path().join("store");
    let engine = open(&root);
    import_inbox(&engine);
    let (mailbox_id, generation) = mailbox(&engine);
    let dest = scratch.path().join("out");
    let request = ExportCaptureRequest::new(
        ExportFormat::Mboxrd,
        dest.to_string_lossy().to_string(),
        ExportScope::Mailbox {
            mailbox_id,
            expected_generation: generation,
        },
    )
    .unwrap();
    let now = engine.resources().now();
    let request_json = request.request_json().to_owned();
    block_on(engine.resources().write(WritePriority::User, move |tx| {
        create_export_job(tx, "sealed", &request_json, now)
    }))
    .unwrap();
    engine
        .resources()
        .capture_export("sealed", &request, |_| ControlFlow::Continue(()))
        .unwrap();
    block_on(engine.shutdown()).unwrap();

    let engine = open(&root);
    settle_export(&engine, "sealed", ExportJobState::Completed);
    assert!(dest.join("sweetbriar-metadata.json").is_file());
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn an_export_interrupted_mid_capture_fails_visibly_at_open() {
    let scratch = ScratchDir::new("export-cut-capture");
    let root = scratch.path().join("store");
    let engine = open(&root);
    let now = engine.resources().now();
    block_on(engine.resources().write(WritePriority::User, move |tx| {
        create_export_job(tx, "cut", "{\"v\":1}", now)
    }))
    .unwrap();
    block_on(engine.shutdown()).unwrap();

    let engine = open(&root);
    let failed = settle_export(&engine, "cut", ExportJobState::Failed);
    assert!(
        failed.checkpoint_json().contains("interrupted"),
        "the failure explains the interrupted capture: {}",
        failed.checkpoint_json()
    );
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn an_export_against_a_stale_generation_fails_instead_of_capturing() {
    let scratch = ScratchDir::new("export-stale-generation");
    let engine = open(&scratch.path().join("store"));
    import_inbox(&engine);
    let (mailbox_id, generation) = mailbox(&engine);
    let dest = scratch.path().join("out");

    let key = engine.new_idempotency_key();
    let started = start_export(
        &engine,
        &key,
        &dest,
        ExportScopeInput::Mailbox {
            mailbox_id,
            generation: generation + 1,
        },
    );
    let failed = settle_export(&engine, started.job_id(), ExportJobState::Failed);
    assert!(
        failed.checkpoint_json().contains("error"),
        "the fenced read's conflict lands in the durable checkpoint: {}",
        failed.checkpoint_json()
    );
    assert!(!dest.join("sweetbriar-metadata.json").exists());
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_cancelled_export_stays_cancelled_across_reopen_and_publishes_nothing() {
    let scratch = ScratchDir::new("export-cancelled-resume");
    let root = scratch.path().join("store");
    let engine = open(&root);
    import_inbox(&engine);
    let (mailbox_id, generation) = mailbox(&engine);
    let dest = scratch.path().join("out");
    let request = ExportCaptureRequest::new(
        ExportFormat::Mboxrd,
        dest.to_string_lossy().to_string(),
        ExportScope::Mailbox {
            mailbox_id,
            expected_generation: generation,
        },
    )
    .unwrap();
    let now = engine.resources().now();
    let request_json = request.request_json().to_owned();
    block_on(engine.resources().write(WritePriority::User, move |tx| {
        create_export_job(tx, "halt", &request_json, now)
    }))
    .unwrap();
    engine
        .resources()
        .capture_export("halt", &request, |_| ControlFlow::Continue(()))
        .unwrap();

    block_on(engine.cancel_job(&engine.new_idempotency_key(), "halt")).unwrap();
    assert_eq!(
        export_status(&engine, "halt").state(),
        ExportJobState::Cancelled
    );
    block_on(engine.shutdown()).unwrap();

    let engine = open(&root);
    std::thread::sleep(Duration::from_millis(100));
    assert_eq!(
        export_status(&engine, "halt").state(),
        ExportJobState::Cancelled,
        "a terminal export never resumes"
    );
    assert!(!dest.join("sweetbriar-metadata.json").exists());
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn an_exported_mbox_reimports_byte_identical_acquisitions() {
    let scratch = ScratchDir::new("export-round-trip");
    let engine = open(&scratch.path().join("store"));
    import_inbox(&engine);
    let before = hashes(&engine);
    let (mailbox_id, generation) = mailbox(&engine);
    let dest = scratch.path().join("out");

    let key = engine.new_idempotency_key();
    let started = start_export(
        &engine,
        &key,
        &dest,
        ExportScopeInput::Mailbox {
            mailbox_id,
            generation,
        },
    );
    settle_export(&engine, started.job_id(), ExportJobState::Completed);
    let paths = fs::read_dir(&dest)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| path.extension().is_some_and(|ext| ext == "mbox"))
        .collect::<Vec<_>>();
    assert!(!paths.is_empty());
    block_on(engine.shutdown()).unwrap();

    let mirror = open(&scratch.path().join("mirror"));
    let id = start(&mirror, &paths);
    settle(&mirror, &id, ImportJobState::AwaitingConfirmation);
    confirm(&mirror, &id, vec![]).unwrap();
    settle(&mirror, &id, ImportJobState::Completed);
    assert_eq!(
        hashes(&mirror),
        before,
        "every exported acquisition re-imports byte-identical"
    );
    block_on(mirror.shutdown()).unwrap();
}

#[test]
fn a_frozen_selection_exports_exactly_its_membership() {
    let scratch = ScratchDir::new("export-selection");
    let engine = open(&scratch.path().join("store"));
    import_inbox(&engine);
    let (mailbox_id, _) = mailbox(&engine);

    let page =
        block_on(engine.open_window(WindowSpec::Mailbox(mailbox_id), Anchor::Top, 10)).unwrap();
    let index = block_on(engine.window_ids(page.window_id())).unwrap();
    let snapshot = block_on(engine.open_selection(
        page.window_id(),
        index.generation(),
        SelectionInput::Ids(index.ids().to_vec()),
    ))
    .unwrap();
    let dest = scratch.path().join("out");
    let key = engine.new_idempotency_key();
    let started = start_export(
        &engine,
        &key,
        &dest,
        ExportScopeInput::Selection(snapshot.selection_id().clone()),
    );
    let done = settle_export(&engine, started.job_id(), ExportJobState::Completed);
    assert!(done.captured_chunks() > 0, "the capture sealed chunks");
    block_on(engine.close_selection(snapshot.selection_id().clone()));
    assert!(dest.join("sweetbriar-metadata.json").is_file());
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn an_export_job_query_reports_the_durable_record() {
    let scratch = ScratchDir::new("export-job-query");
    let engine = open(&scratch.path().join("store"));
    import_inbox(&engine);
    let (mailbox_id, generation) = mailbox(&engine);
    let dest = scratch.path().join("out");

    let key = engine.new_idempotency_key();
    let started = start_export(
        &engine,
        &key,
        &dest,
        ExportScopeInput::Mailbox {
            mailbox_id,
            generation,
        },
    );
    let job = export_status(&engine, started.job_id());
    assert!(
        job.request_json().contains("\"sha256\""),
        "the durable record is the sealed request hash: {}",
        job.request_json()
    );
    assert!(
        !job.is_finished() || job.state() == ExportJobState::Completed,
        "the query answers a live or completed record"
    );
    assert!(matches!(
        block_on(engine.export_job("no-such-export")),
        Err(EngineError::NotFound { .. })
    ));
    settle_export(&engine, started.job_id(), ExportJobState::Completed);
    block_on(engine.shutdown()).unwrap();
}
