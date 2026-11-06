//! Tests for engine-level restore (M1.8g, spec/03 "Restore" steps
//! 4–5): the swap replaces the store under a running engine, windows
//! and subscriptions reset by epoch, commands pause for the swap's
//! duration, and the restored image's own recovery starts afterward.

use std::path::{Path, PathBuf};
use std::time::{Duration, Instant};

use sb_test_support::{ScratchDir, block_on};

use crate::{
    Anchor, BackupDestination, BackupReceipt, Engine, EngineError, ImportJobState, MailboxRole,
    MboxVariant, WindowEvent, WindowSpec,
};

/// How long a test waits for a worker before failing.
const PATIENCE: Duration = Duration::from_secs(300);

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/import")
}

fn open(root: &Path) -> Engine {
    block_on(Engine::open(root)).expect("the engine opens")
}

/// Import a fixture mailbox and wait for completion.
fn import_mailbox(engine: &Engine, name: &str) {
    let key = engine.new_idempotency_key();
    let started = block_on(engine.start_import(&key, vec![fixtures().join(name)])).unwrap();
    let id = started.job_id().to_owned();
    let deadline = Instant::now() + PATIENCE;
    let status = loop {
        let status = block_on(engine.import_job(&id)).unwrap();
        if matches!(status.state(), ImportJobState::AwaitingConfirmation) {
            break status;
        }
        assert!(Instant::now() < deadline, "the import never waited");
        std::thread::sleep(Duration::from_millis(25));
    };
    let variants = status
        .checkpoint()
        .ambiguous()
        .iter()
        .map(|&ordinal| (ordinal, MboxVariant::Mboxrd))
        .collect();
    let key = engine.new_idempotency_key();
    block_on(engine.confirm_import(&key, &id, variants)).unwrap();
    let deadline = Instant::now() + PATIENCE;
    loop {
        let status = block_on(engine.import_job(&id)).unwrap();
        if matches!(status.state(), ImportJobState::Completed) {
            return;
        }
        assert!(Instant::now() < deadline, "the import never completed");
        std::thread::sleep(Duration::from_millis(25));
    }
}

/// A completed backup generation of `engine`'s store in `destination`.
fn backup(engine: &Engine, destination: &Path) -> BackupReceipt {
    engine
        .resources()
        .backup_snapshot(BackupDestination::Directory(destination.to_owned()))
        .and_then(|snapshot| engine.resources().complete_backup(snapshot))
        .expect("the backup completes")
}

/// The occurrences in `mailbox`'s unread set — what an import leaves.
fn row_count(engine: &Engine, table: &'static str) -> i64 {
    engine
        .resources()
        .read(|tx| {
            tx.query_row(&format!("select count(*) from {table}"), [], |row| {
                row.get(0)
            })
            .map_err(|error| sb_resources::StoreError::internal(error.to_string()))
        })
        .unwrap()
}

/// A restore replaces the live store with the backup image while the
/// engine runs: the pre-restore window resets by epoch, its
/// subscription sees `ResetRequired`, and commands land on the
/// restored image afterwards.
#[test]
fn a_restore_swaps_the_store_and_resets_windows() {
    let scratch = ScratchDir::new("restore-engine-swap");
    let store = scratch.path().join("store");
    let backup_dir = scratch.path().join("backups");
    let engine = open(&store);
    import_mailbox(&engine, "Inbox.mbox");
    let before = row_count(&engine, "occurrences");
    let receipt = backup(&engine, &backup_dir);

    // A second import changes the live store beyond the backup: the
    // same mbox would deduplicate to identical occurrences.
    import_mailbox(&engine, "quoting.mbox");
    assert!(
        row_count(&engine, "occurrences") > before,
        "the second import added rows"
    );

    let window =
        block_on(engine.open_window(WindowSpec::Unified(MailboxRole::Inbox), Anchor::Top, 10))
            .unwrap();
    let subscription = engine.subscribe(window.window_id());
    let generation = window.generation();

    block_on(engine.restore_backup(
        receipt.location().parent().unwrap(),
        receipt.generation_id(),
    ))
    .expect("the restore succeeds");

    // The old window and its subscription are stale by epoch: the
    // restored generation may coincide with the recorded one, so the
    // epoch, not the number, must carry the change.
    match block_on(engine.read_window(window.window_id(), generation, Anchor::Top, 10)) {
        Err(EngineError::ResetRequired { .. }) => (),
        other => panic!("a pre-restore window must reset, got {other:?}"),
    }
    assert!(matches!(
        block_on(subscription.next()),
        Ok(WindowEvent::ResetRequired)
    ));

    // The restored image answers new commands: only the backup's rows.
    assert_eq!(row_count(&engine, "occurrences"), before);
    let reopened =
        block_on(engine.open_window(WindowSpec::Unified(MailboxRole::Inbox), Anchor::Top, 10))
            .expect("a post-restore window opens");
    assert!(reopened.generation() > 0);
    block_on(engine.shutdown()).unwrap();
}

/// The quiesce gate itself: while `paused`, commands refuse with
/// `Unavailable`; `resume` opens them again.
#[test]
fn quiesced_commands_are_unavailable_until_resume() {
    let scratch = ScratchDir::new("restore-engine-quiesce");
    let engine = open(&scratch.path().join("store"));

    engine.blocking().quiesce();
    assert!(matches!(
        block_on(engine.mailboxes("account")),
        Err(EngineError::Unavailable { .. })
    ));
    engine.blocking().resume();
    // The command answers again — `mailboxes` on an unknown account
    // is `NotFound`, proving it reached the store.
    assert!(matches!(
        block_on(engine.mailboxes("account")),
        Err(EngineError::NotFound { .. })
    ));
    block_on(engine.shutdown()).unwrap();
}

/// One restore at a time: while a restore owns the slot, another call
/// refuses before touching the store.
#[test]
fn a_second_restore_is_unavailable() {
    let scratch = ScratchDir::new("restore-engine-concurrent");
    let engine = open(&scratch.path().join("store"));
    assert!(engine.begin_restore(), "the slot is free");
    assert!(matches!(
        block_on(engine.restore_backup(&scratch.path().join("backups"), "generation")),
        Err(EngineError::Unavailable { .. })
    ));
    engine.end_restore();
    block_on(engine.shutdown()).unwrap();
}

/// A malformed generation name fails verification inside the store;
/// the engine reports the propagated error and stays healthy.
#[test]
fn a_malformed_generation_is_refused() {
    let scratch = ScratchDir::new("restore-engine-missing");
    let engine = open(&scratch.path().join("store"));
    std::fs::create_dir_all(scratch.path().join("backups")).unwrap();
    match block_on(engine.restore_backup(&scratch.path().join("backups"), "missing-generation")) {
        Err(EngineError::Corrupt { detail }) => {
            assert_eq!(detail, "invalid generation id")
        }
        other => panic!("a malformed generation must fail verification, got {other:?}"),
    }
    // The store survived: commands still answer.
    block_on(engine.accounts()).unwrap();
    block_on(engine.shutdown()).unwrap();
}

/// The restore slot frees after a call ends: a failed restore does not
/// wedge the engine.
#[test]
fn a_failed_restore_frees_the_slot_and_the_store() {
    let scratch = ScratchDir::new("restore-engine-failed");
    let engine = open(&scratch.path().join("store"));
    let _ = block_on(engine.restore_backup(&scratch.path().join("backups"), "generation"));
    assert!(engine.begin_restore(), "the slot freed after the failure");
    engine.end_restore();
    block_on(engine.accounts()).unwrap();
    block_on(engine.shutdown()).unwrap();
}
