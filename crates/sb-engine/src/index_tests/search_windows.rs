use super::*;
use crate::{
    Anchor, EngineError, SearchIndexState, SearchScope, SearchSort, SearchSpec, Subscription,
    WindowEvent, WindowId, WindowSpec,
};
use std::future::Future;
use std::task::{Context, Poll, Waker};

fn spec() -> WindowSpec {
    WindowSpec::Search(SearchSpec::new(
        r#"{"v":1,"q":{"and":[]}}"#.into(),
        SearchScope::All,
        SearchSort::Date,
    ))
}

fn event(subscription: &Subscription) -> WindowEvent {
    let mut next = Box::pin(subscription.next());
    let mut context = Context::from_waker(Waker::noop());
    let deadline = Instant::now() + Duration::from_secs(60);
    loop {
        if let Poll::Ready(result) = next.as_mut().poll(&mut context) {
            return result.unwrap();
        }
        assert!(
            Instant::now() < deadline,
            "real Search invalidation did not arrive"
        );
        std::thread::sleep(Duration::from_millis(5));
    }
}

fn changed(engine: &Engine, subscription: &Subscription) -> u64 {
    let current = generation(engine).try_into().unwrap();
    assert_eq!(
        event(subscription),
        WindowEvent::Invalidated {
            generation: current
        }
    );
    current
}

#[test]
fn real_worker_phase_pause_retry_cancel_resume_and_completion_invalidate_search() {
    let scratch = ScratchDir::new("search-worker-events");
    let engine = held(&scratch.path().join("store"));
    let search = block_on(engine.open_window(spec(), Anchor::Top, 1)).unwrap();
    let subscription = engine.subscribe(search.window_id());
    let list_generation = engine
        .resources()
        .read(sb_resources::list_generation)
        .unwrap();
    for (code, message, deadline) in [
        (sb_resources::SearchIoCode::OtherIo, "Reconnect", 1000),
        (
            sb_resources::SearchIoCode::PermissionDenied,
            "Restore access",
            2000,
        ),
    ] {
        write(&engine, move |tx| {
            sb_resources::pause_for_io(tx, code, message, Timestamp::from_millis(deadline), 1)
        });
        changed(&engine, &subscription);
        assert_eq!(count(&engine, QUEUE), 1);
    }
    block_on(engine.retry_search_index(&engine.new_idempotency_key())).unwrap();
    changed(&engine, &subscription);
    wait(|| engine.indexer().waiting());
    // Retry preserves the independent user hold. Clear the due I/O hold through
    // the real operation under the current fence, then pause again before work.
    write(&engine, |tx| {
        sb_resources::set_user_pause(tx, UserPause::Resume)
    });
    changed(&engine, &subscription);
    let fence = engine.resources().pending_index_batch().unwrap().fence;
    write(&engine, move |tx| {
        sb_resources::begin_index_retry(tx, fence, Timestamp::from_millis(i64::MAX))
    });
    changed(&engine, &subscription);
    write(&engine, |tx| {
        sb_resources::set_user_pause(tx, UserPause::Pause)
    });
    changed(&engine, &subscription);
    let job = block_on(engine.start_search_rebuild(&engine.new_idempotency_key()))
        .unwrap()
        .job_id()
        .to_owned();
    let current = changed(&engine, &subscription);
    assert!(matches!(
        block_on(engine.read_window(search.window_id(), current, Anchor::Top, 1)),
        Err(EngineError::Unavailable { .. })
    ));
    assert_eq!(
        block_on(engine.coverage(SearchScope::All))
            .unwrap()
            .rebuild
            .unwrap()
            .phase,
        sb_resources::RebuildPhase::Reset
    );
    let weak = Arc::downgrade(engine.resources());
    let (committed, notices) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let released = Mutex::new(released);
    hook(&engine, move |value| {
        if value == Event::Committed {
            let resources = weak.upgrade().unwrap();
            let phase = resources
                .read(|tx| Ok(sb_resources::latest_rebuild(tx)?.unwrap().phase()))
                .unwrap();
            let queue = resources
                .read(|tx| {
                    tx.query_row(QUEUE, [], |row| row.get::<_, i64>(0))
                        .map_err(|error| StoreError::internal(error.to_string()))
                })
                .unwrap();
            drop(resources);
            committed.send((phase, queue)).unwrap();
            released
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(30))
                .unwrap();
        }
    });
    block_on(engine.resume_search_rebuild(&engine.new_idempotency_key(), &job)).unwrap();
    assert_eq!(
        notices.recv_timeout(Duration::from_secs(30)).unwrap(),
        (sb_resources::RebuildPhase::Enqueue, 1)
    );
    let current = changed(&engine, &subscription);
    assert!(matches!(
        block_on(engine.read_window(search.window_id(), current, Anchor::Top, 1)),
        Err(EngineError::Unavailable { .. })
    ));
    release.send(()).unwrap();
    assert_eq!(
        notices.recv_timeout(Duration::from_secs(30)).unwrap(),
        (sb_resources::RebuildPhase::Index, 1)
    );
    changed(&engine, &subscription);
    block_on(engine.cancel_job(&engine.new_idempotency_key(), &job)).unwrap();
    let current = changed(&engine, &subscription);
    let page = block_on(engine.read_window(search.window_id(), current, Anchor::Top, 1)).unwrap();
    assert_eq!(
        page.search().unwrap().coverage().index_state,
        SearchIndexState::PausedByUser
    );
    release.send(()).unwrap();
    wait(|| engine.indexer().waiting());
    assert_eq!(count(&engine, QUEUE), 1);
    block_on(engine.resume_search_rebuild(&engine.new_idempotency_key(), &job)).unwrap();
    assert_eq!(
        notices.recv_timeout(Duration::from_secs(30)).unwrap(),
        (sb_resources::RebuildPhase::Index, 0)
    );
    let published = changed(&engine, &subscription);
    let page = block_on(engine.read_window(search.window_id(), published, Anchor::Top, 1)).unwrap();
    assert_eq!(page.search().unwrap().coverage().indexed, 1);
    release.send(()).unwrap();
    assert_eq!(
        notices.recv_timeout(Duration::from_secs(30)).unwrap(),
        (sb_resources::RebuildPhase::Complete, 0)
    );
    let completed = changed(&engine, &subscription);
    assert_eq!(
        completed,
        published + 1,
        "queue-empty completion is its own coverage-only commit"
    );
    let page = block_on(engine.read_window(search.window_id(), completed, Anchor::Top, 1)).unwrap();
    assert_eq!(
        page.search()
            .unwrap()
            .coverage()
            .rebuild
            .as_ref()
            .unwrap()
            .phase,
        sb_resources::RebuildPhase::Complete
    );
    assert_eq!(
        engine
            .resources()
            .read(sb_resources::list_generation)
            .unwrap(),
        list_generation
    );
    release.send(()).unwrap();
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_concurrent_writer_cannot_split_public_search_rows_from_status() {
    let scratch = ScratchDir::new("search-atomic-page");
    let engine = Arc::new(held(&scratch.path().join("store")));
    let first = block_on(engine.open_window(spec(), Anchor::Top, 1)).unwrap();
    let (captured, noticed) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let released = Mutex::new(released);
    let once = AtomicUsize::new(0);
    hook(&engine, move |value| {
        if value == Event::SearchReadCaptured && once.fetch_add(1, Ordering::SeqCst) == 0 {
            captured.send(()).unwrap();
            released
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(30))
                .unwrap();
        }
    });
    let request = Arc::clone(&engine);
    let id = first.window_id();
    let generation = first.generation();
    let reader = std::thread::spawn(move || {
        block_on(request.read_window(id, generation, Anchor::Top, 1)).unwrap()
    });
    noticed.recv_timeout(Duration::from_secs(30)).unwrap();
    write(&engine, |tx| {
        tx.execute_batch(
            "UPDATE occurrences SET f_seen=1-f_seen; UPDATE acquisitions SET object_state='missing'",
        )
        .map_err(|error| StoreError::internal(error.to_string()))?;
        let ids = tx
            .prepare("SELECT occurrence_id FROM occurrences")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<std::collections::BTreeSet<i64>, _>>()
            .unwrap();
        sb_resources::refresh_list_rows(tx, &ids)?;
        Ok(())
    });
    release.send(()).unwrap();
    let held = reader.join().unwrap();
    assert_eq!(held.generation(), first.generation());
    assert_eq!(held.rows()[0].unread(), first.rows()[0].unread());
    assert_eq!(held.search().unwrap(), first.search().unwrap());
    let current = block_on(engine.coverage(SearchScope::All))
        .unwrap()
        .generation;
    assert!(current > first.generation());
    let fresh = block_on(engine.read_window(id, current, Anchor::Top, 1)).unwrap();
    assert_ne!(fresh.rows()[0].unread(), first.rows()[0].unread());
    assert_eq!(fresh.search().unwrap().coverage().bodies_acquired, 0);
    assert_eq!(fresh.search().unwrap().coverage().generation, current);
    block_on(Arc::try_unwrap(engine).unwrap().shutdown()).unwrap();
}

#[test]
fn cancelled_registered_search_opens_close_and_cancelled_reads_move_no_mark() {
    let scratch = ScratchDir::new("search-cancellation");
    let engine = Arc::new(held(&scratch.path().join("store")));
    let (registered, notice) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let released = Mutex::new(released);
    hook(&engine, move |value| {
        if let Event::SearchRegistered { window_id } = value {
            registered.send(window_id).unwrap();
            released
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(30))
                .unwrap();
        }
    });
    let request = Arc::clone(&engine);
    let task = engine
        .runtime_handle()
        .spawn(async move { request.open_window(spec(), Anchor::Top, 1).await });
    let id = WindowId::from(notice.recv_timeout(Duration::from_secs(30)).unwrap());
    task.abort();
    assert!(block_on(task).unwrap_err().is_cancelled());
    release.send(()).unwrap();
    wait(|| {
        matches!(
            block_on(engine.window_ids(id)),
            Err(EngineError::NotFound { .. })
        )
    });
    hook(&engine, |_| {});
    let window = block_on(engine.open_window(spec(), Anchor::Top, 1)).unwrap();
    write(&engine, |tx| {
        sb_resources::set_user_pause(tx, UserPause::Pause)
    });
    let current = generation(&engine).try_into().unwrap();
    let (captured, notice) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let released = Mutex::new(released);
    hook(&engine, move |value| {
        if value == Event::SearchReadCaptured {
            captured.send(()).unwrap();
            released
                .lock()
                .unwrap()
                .recv_timeout(Duration::from_secs(30))
                .unwrap();
        }
    });
    let request = Arc::clone(&engine);
    let id = window.window_id();
    let task = engine
        .runtime_handle()
        .spawn(async move { request.read_window(id, current, Anchor::Top, 1).await });
    notice.recv_timeout(Duration::from_secs(30)).unwrap();
    task.abort();
    assert!(block_on(task).unwrap_err().is_cancelled());
    release.send(()).unwrap();
    assert_eq!(
        event(&engine.subscribe(id)),
        WindowEvent::Invalidated {
            generation: current
        }
    );
    block_on(Arc::try_unwrap(engine).unwrap().shutdown()).unwrap();
}
