//! Job-kind dispatch before export workers are exposed.

use sb_resources::{ExportJobState, advance_export_job, create_export_job, export_job};

use super::*;

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
