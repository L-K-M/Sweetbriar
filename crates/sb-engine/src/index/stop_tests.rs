//! A late index failure must not replace a closed window's cancellation.

use std::sync::mpsc;
use std::time::Instant;

use sb_test_support::{ScratchDir, block_on};

use super::*;
use crate::{Anchor, EngineError, SearchScope, SearchSort, SearchSpec, WindowSpec};

const PATIENCE: Duration = Duration::from_secs(30);

#[test]
fn a_stopped_search_ids_read_wins_over_a_later_status_failure() {
    let scratch = ScratchDir::new("closed-search-late-failure");
    let engine = Arc::new(block_on(Engine::open(scratch.path().join("store"))).unwrap());
    let spec = WindowSpec::Search(SearchSpec::new(
        r#"{"v":1,"q":{"text":"alpha"}}"#.into(),
        SearchScope::All,
        SearchSort::Relevance,
    ));
    let deadline = Instant::now() + PATIENCE;
    while !engine.indexer().waiting() {
        assert!(Instant::now() < deadline, "the index worker never parked");
        std::thread::sleep(Duration::from_millis(5));
    }
    let closed = block_on(engine.open_window(spec.clone(), Anchor::Top, 10)).unwrap();
    let live = block_on(engine.open_window(spec, Anchor::Top, 10)).unwrap();

    let (captured, paused) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let released = Mutex::new(released);
    *engine.indexer().hook.lock().unwrap() = Some(Arc::new(move |event| {
        if event == Event::SearchReadCaptured {
            captured.send(()).unwrap();
            released.lock().unwrap().recv_timeout(PATIENCE).unwrap();
        }
    }));
    let window_id = closed.window_id();
    let read = |window_id| {
        let request = Arc::clone(&engine);
        std::thread::spawn(move || block_on(request.window_ids(window_id)))
    };
    let reading = read(window_id);
    let live_read = read(live.window_id());
    for _ in 0..2 {
        paused.recv_timeout(PATIENCE).unwrap();
    }

    // Fail a real SQLite statement through the worker's production write gate.
    let (index, resources) = (Arc::clone(engine.indexer()), Arc::clone(engine.resources()));
    let failure = block_on(engine.runtime_handle().spawn_blocking(move || {
        index.write(&resources, |tx| {
            tx.execute_batch("SELECT missing_column FROM missing_index_test_table")
                .map_err(|error| StoreError::internal(error.to_string()))
        })
    }))
    .unwrap();
    assert!(matches!(failure, Err(StoreError::Internal { .. })));
    assert!(engine.indexer().readable().is_err());

    block_on(engine.close_window(window_id));
    release.send(()).unwrap();
    release.send(()).unwrap();
    let result = reading.join().unwrap();
    assert!(
        matches!(result, Err(EngineError::NotFound { .. })),
        "{result:?}"
    );
    assert!(matches!(
        live_read.join().unwrap(),
        Err(EngineError::Unavailable { .. })
    ));
    block_on(engine.close_window(live.window_id()));
    block_on(Arc::try_unwrap(engine).unwrap().shutdown()).unwrap();
}
