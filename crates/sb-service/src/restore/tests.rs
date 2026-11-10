//! `prepare_restore` through the service (plan M1.8i4).

use std::sync::Arc;
use std::sync::atomic::AtomicBool;

use sb_test_support::{ScratchDir, block_on};

use crate::{BackupEnd, Core, CoreConfig, CoreError, open_core};

fn open(scratch: &ScratchDir) -> Core {
    // Staging opens the data root's parent without following links
    // (macOS's /var is one).
    let root = std::fs::canonicalize(scratch.path()).unwrap();
    block_on(open_core(CoreConfig {
        data_root: root.join("store"),
        cache_root: root.join("cache"),
    }))
    .unwrap()
}

/// One complete generation: its id.
fn back_up(core: &Core, key: &str) -> String {
    let started = block_on(core.start_backup(key)).unwrap();
    let copy = started.copy.expect("a fresh key starts a copy");
    assert_eq!(
        block_on(copy.finished()),
        BackupEnd::Complete { after: None }
    );
    started.generation_id
}

fn unflagged() -> Arc<AtomicBool> {
    Arc::new(AtomicBool::new(false))
}

#[test]
fn a_listed_generation_is_prepared_and_holds_the_backup_operation() {
    let scratch = ScratchDir::new("service-prepare-restore");
    let core = open(&scratch);
    let id = back_up(&core, "00000000-0000-4000-8000-0000000000d1");

    let prepared = block_on(core.prepare_restore(&id, unflagged())).unwrap();
    assert_eq!(prepared.generation_id(), id);
    // The core is in use, and nothing else takes the operation.
    assert_eq!(block_on(core.list_backups()).unwrap().len(), 1);
    match block_on(core.start_backup("00000000-0000-4000-8000-0000000000d2")) {
        Err(CoreError::Conflict { .. }) => {}
        other => panic!("expected Conflict, got {other:?}"),
    }

    drop(prepared);
    back_up(&core, "00000000-0000-4000-8000-0000000000d2");
    block_on(core.shutdown());
}

#[test]
fn a_raised_flag_is_cancelled_and_an_unknown_generation_not_found() {
    let scratch = ScratchDir::new("service-prepare-refused");
    let core = open(&scratch);
    let id = back_up(&core, "00000000-0000-4000-8000-0000000000d3");

    match block_on(core.prepare_restore(&id, Arc::new(AtomicBool::new(true)))) {
        Err(CoreError::Cancelled) => {}
        other => panic!("expected Cancelled, got {other:?}"),
    }
    match block_on(core.prepare_restore("no-such-generation", unflagged())) {
        Err(CoreError::NotFound { .. }) => {}
        other => panic!("expected NotFound, got {other:?}"),
    }
    // Neither left the operation held.
    let prepared = block_on(core.prepare_restore(&id, unflagged())).unwrap();
    assert_eq!(prepared.generation_id(), id);
    drop(prepared);
    block_on(core.shutdown());
}
