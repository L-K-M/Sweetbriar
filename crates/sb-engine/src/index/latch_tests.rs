//! A write the integrity latch refuses is not an index failure (plan
//! M1.8h5b2; spec/03 "Integrity"): the worker under way at the finding
//! must not fence Search, which goes on over what was committed.

use std::sync::atomic::AtomicBool;

use sb_resources::IntegrityCheck;
use sb_test_support::{ScratchDir, block_on, damage_an_index};

use super::*;
use crate::SearchScope;

/// An engine whose store just latched on a finding.
fn latched(scratch: &ScratchDir) -> Engine {
    let root = scratch.path().join("store");
    block_on(block_on(Engine::open(&root)).unwrap().shutdown()).unwrap();
    damage_an_index(&root.join("state.sqlite"));
    let engine = block_on(Engine::open(&root)).unwrap();
    let found = engine
        .resources()
        .check_integrity(&AtomicBool::new(false))
        .unwrap();
    assert!(matches!(found, IntegrityCheck::Finding(_)), "{found:?}");
    engine
}

#[test]
fn a_write_the_latch_refuses_does_not_fence_search() {
    let scratch = ScratchDir::new("index-latch-write");
    let engine = latched(&scratch);

    // The worker's write gate, as a turn under way at the finding
    // reaches it.
    let (index, resources) = (Arc::clone(engine.indexer()), Arc::clone(engine.resources()));
    let refused = block_on(
        engine
            .runtime_handle()
            .spawn_blocking(move || index.write(&resources, commit_index_recovery)),
    )
    .unwrap();
    assert!(
        matches!(refused, Err(StoreError::Corrupt { .. })),
        "{refused:?}"
    );

    assert!(engine.indexer().readable().is_ok());
    block_on(engine.coverage(SearchScope::All)).expect("coverage still answers");
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_turn_the_latch_fails_parks_without_fencing_search() {
    let scratch = ScratchDir::new("index-latch-turn");
    let engine = latched(&scratch);

    // A worker of its own, not yet told to stop, as one is between the
    // latch and its observer: its turn drives the store's unfinished
    // rebuild into the refused writer. It stops once it parks.
    let jobs = Arc::new(IndexJobs::default());
    let parked = Arc::downgrade(&jobs);
    *jobs.hook.lock().unwrap() = Some(Arc::new(move |event| {
        if event == Event::Parked
            && let Some(jobs) = parked.upgrade()
        {
            jobs.stop();
        }
    }));
    let (worker, resources) = (Arc::clone(&jobs), Arc::clone(engine.resources()));
    block_on(
        engine
            .runtime_handle()
            .spawn_blocking(move || run(&resources, &worker, &|| false)),
    )
    .unwrap();

    assert!(jobs.readable().is_ok());
    block_on(engine.shutdown()).unwrap();
}
