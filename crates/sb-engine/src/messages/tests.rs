use crate::ImportJobState;
use sb_resources::WritePriority;
use sb_test_support::{ScratchDir, block_on};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

const PLAIN: &str = "Subject: plain\nContent-Type: text/plain\n\nplain body\n";

fn wait(mut condition: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(60);
    while !condition() {
        assert!(
            Instant::now() < end,
            "engine reading document test timed out"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// An engine over `root` that imported one plain message.
fn imported(root: &Path, cache: &Path) -> (crate::Engine, i64) {
    let engine = block_on(crate::Engine::open_with_cache(root, cache)).unwrap();
    let mbox = root.parent().unwrap().join("reading.mbox");
    fs::write(
        &mbox,
        format!("From sender@example.test Sat Oct 17 12:00:00 2026\n{PLAIN}"),
    )
    .unwrap();
    let job = block_on(engine.start_import(&engine.new_idempotency_key(), vec![mbox])).unwrap();
    wait(|| {
        block_on(engine.import_job(job.job_id())).unwrap().state()
            == ImportJobState::AwaitingConfirmation
    });
    block_on(engine.confirm_import(&engine.new_idempotency_key(), job.job_id(), vec![])).unwrap();
    wait(|| {
        block_on(engine.import_job(job.job_id())).unwrap().state() == ImportJobState::Completed
    });
    let id = engine
        .resources()
        .read(|tx| {
            tx.query_row("SELECT message_id FROM logical_messages", [], |row| {
                row.get(0)
            })
            .map_err(|error| sb_resources::StoreError::Internal {
                detail: error.to_string(),
            })
        })
        .unwrap();
    (engine, id)
}

/// The reading document follows the body query's missing-body rule: a
/// known message with no acquisition is `Unavailable`, not a document.
#[test]
fn reading_document_without_an_acquisition_is_unavailable() {
    let scratch = ScratchDir::new("engine-reading-unacquired");
    let (engine, id) = imported(&scratch.path().join("data"), &scratch.path().join("cache"));
    block_on(engine.resources().write(WritePriority::User, move |tx| {
        tx.execute(
            "UPDATE logical_messages SET acquisition_id = NULL WHERE message_id = ?1",
            [id],
        )
        .map_err(|error| sb_resources::StoreError::Internal {
            detail: error.to_string(),
        })?;
        Ok(())
    }))
    .unwrap();
    assert!(matches!(
        block_on(engine.reading_document(id)),
        Err(crate::EngineError::Unavailable { reason }) if reason.contains("not acquired")
    ));
}
