use super::*;
use crate::index::{
    self,
    rebuild::{self, UserPause},
};
use crate::testing::{reading, scratch_paths, writing};
use crate::{Store, WritePriority};
use sb_test_support::block_on;
use std::sync::{Arc, Mutex, mpsc};

fn counter(store: &Store) -> u64 {
    reading(store, |tx| {
        Ok(index::search_generation(tx)?.try_into().unwrap())
    })
}

#[test]
fn committed_search_is_visible_before_callback_and_future_resolution() {
    use std::future::Future;
    use std::task::{Context, Poll, Waker};
    use std::time::Duration;
    let (_scratch, paths) = scratch_paths("search-observer-order");
    let store = Arc::new(crate::open(&paths, &test_seams(real_fs())).unwrap());
    let before = counter(&store);
    let weak = Arc::downgrade(&store);
    let (entered, noticed) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let released = Mutex::new(released);
    store.observe_search_generation(Arc::new(move |generation| {
        let observed = weak.upgrade().and_then(|store| {
            store
                .read(|tx| Ok(index::search_generation(tx)?.try_into().ok()))
                .ok()
                .flatten()
        });
        let _ = entered.send((generation, observed));
        let _ = released
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(30));
    }));
    let mut future = Box::pin(store.write(WritePriority::User, |tx| {
        rebuild::set_user_pause(tx, UserPause::Pause)
    }));
    let observed = noticed.recv_timeout(Duration::from_secs(30)).unwrap();
    let pending = matches!(
        future
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    );
    release.send(()).unwrap();
    block_on(future).unwrap();
    assert_eq!(observed, (before + 1, Some(before + 1)));
    assert!(
        pending,
        "the write resolves only after its callback returns"
    );
    Arc::try_unwrap(store)
        .expect("only the test owns the store")
        .close()
        .unwrap();
}

#[test]
fn independent_observers_emit_once_only_for_successful_moved_counters() {
    let (_scratch, paths) = scratch_paths("search-observer-isolation");
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    let search_before = counter(&store);
    let list_before = reading(&store, |tx| generation(tx));
    let lists = Arc::new(Mutex::new(Vec::new()));
    let searches = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&lists);
    store.observe_list_generation(Arc::new(move |value| sink.lock().unwrap().push(value)));
    let sink = Arc::clone(&searches);
    store.observe_search_generation(Arc::new(move |value| sink.lock().unwrap().push(value)));
    writing(&store, |tx| rebuild::set_user_pause(tx, UserPause::Pause));
    assert!(lists.lock().unwrap().is_empty());
    writing(&store, crate::threads::note_membership_change);
    let failed: Result<(), _> = block_on(store.write(WritePriority::User, |tx| {
        crate::threads::note_membership_change(tx)?;
        Err(StoreError::internal("rollback fixture"))
    }));
    assert!(failed.is_err());
    assert!(
        block_on(
            store.write(WritePriority::User, |tx| -> Result<(), StoreError> {
                crate::threads::note_membership_change(tx)?;
                panic!("write panic fixture");
            })
        )
        .is_err()
    );
    // The FK is checked at COMMIT, so even a late transaction failure emits nothing.
    assert!(
        block_on(store.write(WritePriority::User, |tx| {
            tx.execute_batch(
                "PRAGMA defer_foreign_keys=ON;
            INSERT INTO message_tags(message_id,tag_id,added_at) VALUES(99999,'absent',1)",
            )
            .map_err(sql_error)?;
            crate::threads::note_membership_change(tx)
        }))
        .is_err()
    );
    writing(&store, |_| Ok(()));
    assert_eq!(*lists.lock().unwrap(), [list_before + 1]);
    assert_eq!(
        *searches.lock().unwrap(),
        [search_before + 1, search_before + 2]
    );
    assert_eq!(counter(&store), search_before + 2);
    assert_eq!(reading(&store, |tx| generation(tx)), list_before + 1);
    store.close().unwrap();
}

#[test]
fn search_observer_can_replace_itself_without_holding_the_registration_lock() {
    let (_scratch, paths) = scratch_paths("search-observer-replace");
    let store = Arc::new(crate::open(&paths, &test_seams(real_fs())).unwrap());
    let before = counter(&store);
    let first = Arc::new(Mutex::new(Vec::new()));
    let second = Arc::new(Mutex::new(Vec::new()));
    let weak = Arc::downgrade(&store);
    let one = Arc::clone(&first);
    let two = Arc::clone(&second);
    store.observe_search_generation(Arc::new(move |value| {
        one.lock().unwrap().push(value);
        if let Some(store) = weak.upgrade() {
            let sink = Arc::clone(&two);
            store
                .observe_search_generation(Arc::new(move |value| sink.lock().unwrap().push(value)));
        }
    }));
    writing(&store, |tx| rebuild::set_user_pause(tx, UserPause::Pause));
    writing(&store, |tx| rebuild::set_user_pause(tx, UserPause::Resume));
    assert_eq!(*first.lock().unwrap(), [before + 1]);
    assert_eq!(*second.lock().unwrap(), [before + 2]);
    Arc::try_unwrap(store)
        .expect("only the test owns the store")
        .close()
        .unwrap();
}

#[test]
fn malformed_counters_cannot_suppress_the_other_valid_source() {
    let (_scratch, paths) = scratch_paths("search-observer-corrupt");
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    let list_before = reading(&store, |tx| generation(tx));
    let search_before = counter(&store);
    let lists = Arc::new(Mutex::new(Vec::new()));
    let searches = Arc::new(Mutex::new(Vec::new()));
    let sink = Arc::clone(&lists);
    store.observe_list_generation(Arc::new(move |value| sink.lock().unwrap().push(value)));
    let sink = Arc::clone(&searches);
    store.observe_search_generation(Arc::new(move |value| sink.lock().unwrap().push(value)));
    writing(&store, move |tx| {
        tx.execute(
            "UPDATE store_meta SET value='broken' WHERE key='search_generation'",
            [],
        )
        .map_err(sql_error)?;
        tx.execute(
            "UPDATE store_meta SET value=?1 WHERE key='list_generation'",
            [(list_before + 1).to_string()],
        )
        .map_err(sql_error)?;
        Ok(())
    });
    assert_eq!(*lists.lock().unwrap(), [list_before + 1]);
    assert!(searches.lock().unwrap().is_empty());
    assert!(store.read(|tx| index::search_generation(tx)).is_err());
    writing(&store, move |tx| {
        tx.execute(
            "UPDATE store_meta SET value='broken' WHERE key='list_generation'",
            [],
        )
        .map_err(sql_error)?;
        tx.execute(
            "UPDATE store_meta SET value=?1 WHERE key='search_generation'",
            [(search_before + 1).to_string()],
        )
        .map_err(sql_error)?;
        Ok(())
    });
    assert_eq!(*searches.lock().unwrap(), [search_before + 1]);
    assert!(matches!(
        store.read(|tx| generation(tx)),
        Err(StoreError::Corrupt { .. })
    ));
    writing(&store, move |tx| {
        tx.execute(
            "UPDATE store_meta SET value=?1 WHERE key='list_generation'",
            [(list_before + 1).to_string()],
        )
        .map_err(sql_error)?;
        Ok(())
    });
    assert_eq!(*lists.lock().unwrap(), [list_before + 1]);
    assert_eq!(*searches.lock().unwrap(), [search_before + 1]);
    store.close().unwrap();
}

#[test]
fn exhausted_source_rolls_back_both_counters_and_emits_no_extra_event() {
    for (key, maximum) in [
        ("search_generation", i64::MAX.to_string()),
        ("list_generation", u64::MAX.to_string()),
    ] {
        let (_scratch, paths) = scratch_paths("search-observer-exhausted");
        let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
        let notices = Arc::new(Mutex::new(Vec::new()));
        let sink = Arc::clone(&notices);
        store.observe_search_generation(Arc::new(move |value| sink.lock().unwrap().push(value)));
        writing(&store, move |tx| {
            tx.execute(
                "UPDATE store_meta SET value=?2 WHERE key=?1",
                params![key, maximum],
            )
            .map_err(sql_error)?;
            Ok(())
        });
        notices.lock().unwrap().clear();
        let before = (reading(&store, |tx| generation(tx)), counter(&store));
        let result =
            block_on(store.write(WritePriority::User, crate::threads::note_membership_change));
        match key {
            "search_generation" => assert!(matches!(result, Err(StoreError::Unavailable { .. }))),
            _ => assert!(matches!(result, Err(StoreError::Corrupt { .. }))),
        }
        assert_eq!(
            (reading(&store, |tx| generation(tx)), counter(&store)),
            before
        );
        assert!(notices.lock().unwrap().is_empty());
        store.close().unwrap();
    }
}
