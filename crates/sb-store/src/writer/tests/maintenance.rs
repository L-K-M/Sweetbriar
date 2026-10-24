use super::*;
use std::future::Future;
use std::task::{Context, Poll, Waker};

const ADVANCE: &str =
    "UPDATE store_meta SET value=CAST(value AS INTEGER)+1 WHERE key='seen_address_inputs'";
const INPUT: &str = "SELECT value FROM store_meta WHERE key='seen_address_inputs'";

fn watch(store: &Store) -> Arc<AtomicUsize> {
    let count = Arc::new(AtomicUsize::new(0));
    let observed = count.clone();
    store.observe_maintenance_inputs(Arc::new(move || {
        observed.fetch_add(1, Ordering::SeqCst);
    }));
    count
}

#[test]
fn maintenance_callback_sees_commit_before_the_write_resolves() {
    let (_scratch, store) = store_with_probe("maintenance-order");
    let store = Arc::new(store);
    let weak = Arc::downgrade(&store);
    let before: String = reading(&store, |tx| {
        tx.query_row(INPUT, [], |row| row.get(0)).map_err(bug)
    });
    let (entered, observed) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let released = Mutex::new(released);
    // A test-only gate observes the exact COMMIT/callback/future boundary.
    store.observe_maintenance_inputs(Arc::new(move || {
        let value: String = reading(&weak.upgrade().unwrap(), |tx| {
            tx.query_row(INPUT, [], |row| row.get(0)).map_err(bug)
        });
        entered.send(value).unwrap();
        released
            .lock()
            .unwrap()
            .recv_timeout(Duration::from_secs(30))
            .unwrap();
    }));
    let mut pending = Box::pin(store.write(WritePriority::User, |tx| {
        tx.execute_batch(ADVANCE).map_err(bug)
    }));
    let value = observed.recv_timeout(Duration::from_secs(30)).unwrap();
    let unresolved = matches!(
        pending
            .as_mut()
            .poll(&mut Context::from_waker(Waker::noop())),
        Poll::Pending
    );
    release.send(()).unwrap();
    block_on(pending).unwrap();
    assert_eq!(
        value.parse::<u64>().unwrap(),
        before.parse::<u64>().unwrap() + 1
    );
    assert!(unresolved);

    let notices = watch(&store);
    // Submission owns the write even when its future is never polled.
    drop(store.write(WritePriority::User, |tx| {
        tx.execute_batch(ADVANCE).map_err(bug)
    }));
    writing(&store, |_| Ok(()));
    assert_eq!(notices.load(Ordering::SeqCst), 1);
    Arc::try_unwrap(store).unwrap().close().unwrap();
}

#[test]
fn maintenance_compares_each_raw_key_and_ignores_noops_and_acknowledgements() {
    for key in MAINTENANCE_KEYS {
        let (_scratch, store) = store_with_probe("maintenance-keys");
        let notices = watch(&store);
        let original: String = reading(&store, |tx| {
            tx.query_row("SELECT value FROM store_meta WHERE key=?1", [key], |row| {
                row.get(0)
            })
            .map_err(bug)
        });
        writing(&store, move |tx| {
            tx.execute(
                "UPDATE store_meta SET value='malformed' WHERE key=?1",
                [key],
            )
            .map_err(bug)?;
            Ok(())
        });
        assert_eq!(notices.load(Ordering::SeqCst), 1, "{key}");
        writing(&store, move |tx| {
            tx.execute("UPDATE store_meta SET value=value WHERE key=?1", [key])
                .map_err(bug)?;
            tx.execute_batch("UPDATE store_meta SET value='1' WHERE key IN('repair_deferred','repair_unready'); UPDATE store_meta SET value='0' WHERE key IN('seen_address_swept','repair_swept')").map_err(bug)
        });
        assert_eq!(notices.load(Ordering::SeqCst), 1, "{key}");

        // Another key still wakes while the first remains unreadable to its owner.
        writing(&store, move |tx| {
            let other = if key == "seen_address_inputs" {
                "seen_address_target"
            } else {
                "seen_address_inputs"
            };
            tx.execute("UPDATE store_meta SET value='17' WHERE key=?1", [other])
                .map_err(bug)?;
            Ok(())
        });
        assert_eq!(notices.load(Ordering::SeqCst), 2, "{key}");
        writing(&store, move |tx| {
            tx.execute("DELETE FROM store_meta WHERE key=?1", [key])
                .map_err(bug)?;
            Ok(())
        });
        assert_eq!(notices.load(Ordering::SeqCst), 3, "{key}");
        writing(&store, move |tx| {
            tx.execute(
                "INSERT INTO store_meta(key,value) VALUES(?1,?2)",
                (key, original),
            )
            .map_err(bug)?;
            Ok(())
        });
        assert_eq!(notices.load(Ordering::SeqCst), 4, "{key}");
        store.close().unwrap();
    }
}

#[test]
fn distinct_malformed_text_bytes_remain_distinct_committed_inputs() {
    let (_scratch, store) = store_with_probe("maintenance-text-bytes");
    let notices = watch(&store);
    for (index, bytes) in ["ff", "fe"].into_iter().enumerate() {
        writing(&store, move |tx| {
            tx.execute_batch(&format!("UPDATE store_meta SET value=CAST(x'{bytes}' AS TEXT) WHERE key='seen_address_inputs'")).map_err(bug)
        });
        assert_eq!(notices.load(Ordering::SeqCst), index + 1);
    }
    store.close().unwrap();
}

#[test]
fn an_unreadable_key_is_not_an_observed_deletion_and_cannot_hide_another_change() {
    let conn = Connection::open_in_memory().unwrap();
    // A damaged/nonconforming metadata value causes an actual SQLite conversion error.
    conn.execute_batch("CREATE TABLE store_meta(key TEXT PRIMARY KEY,value); INSERT INTO store_meta VALUES('seen_address_inputs','1'),('repair_inputs','1')").unwrap();
    let mut previous = MAINTENANCE_KEYS.map(|key| observed_maintenance_input(&conn, key).unwrap());
    conn.execute_batch("UPDATE store_meta SET value=2 WHERE key='seen_address_inputs'")
        .unwrap();
    assert!(!maintenance_changed(&conn, &mut previous));
    conn.execute_batch("UPDATE store_meta SET value='2' WHERE key='repair_inputs'")
        .unwrap();
    assert!(maintenance_changed(&conn, &mut previous));
    assert!(!maintenance_changed(&conn, &mut previous));
    conn.execute_batch("UPDATE store_meta SET value='1' WHERE key='seen_address_inputs'")
        .unwrap();
    assert!(!maintenance_changed(&conn, &mut previous));
}

#[test]
fn maintenance_replacement_and_list_search_observers_are_independent() {
    let (_scratch, store) = store_with_probe("maintenance-isolation");
    let store = Arc::new(store);
    let replaced = watch(&store);
    let current = Arc::new(AtomicUsize::new(0));
    let weak = Arc::downgrade(&store);
    let replacement = current.clone();
    store.observe_maintenance_inputs(Arc::new(move || {
        let replacement = replacement.clone();
        weak.upgrade()
            .unwrap()
            .observe_maintenance_inputs(Arc::new(move || {
                replacement.fetch_add(1, Ordering::SeqCst);
            }));
    }));
    let lists = Arc::new(AtomicUsize::new(0));
    let observed = lists.clone();
    store.observe_list_generation(Arc::new(move |_| {
        observed.fetch_add(1, Ordering::SeqCst);
    }));
    let searches = Arc::new(AtomicUsize::new(0));
    let observed = searches.clone();
    store.observe_search_generation(Arc::new(move |_| {
        observed.fetch_add(1, Ordering::SeqCst);
    }));

    writing(&store, crate::threads::note_membership_change);
    assert_eq!(lists.load(Ordering::SeqCst), 1);
    assert_eq!(searches.load(Ordering::SeqCst), 1);
    writing(&store, |tx| tx.execute_batch(ADVANCE).map_err(bug));
    assert_eq!(replaced.load(Ordering::SeqCst), 0);
    assert_eq!(current.load(Ordering::SeqCst), 0);
    writing(&store, |tx| {
        tx.execute_batch("UPDATE store_meta SET value='malformed' WHERE key IN('list_generation','search_generation')").map_err(bug)?;
        tx.execute_batch(ADVANCE).map_err(bug)
    });
    assert_eq!(current.load(Ordering::SeqCst), 1);
    assert_eq!(lists.load(Ordering::SeqCst), 1);
    assert_eq!(searches.load(Ordering::SeqCst), 1);
    Arc::try_unwrap(store).unwrap().close().unwrap();
}

#[derive(Clone, Copy)]
enum Cut {
    ReturnedError,
    Panic,
    Statement,
    Commit,
}

#[test]
fn maintenance_emits_nothing_for_failed_or_rolled_back_jobs() {
    for cut in [Cut::ReturnedError, Cut::Panic, Cut::Statement, Cut::Commit] {
        let (_scratch, store) = store_with_probe("maintenance-rollback");
        let notices = watch(&store);
        let before: String = reading(&store, |tx| {
            tx.query_row(INPUT, [], |row| row.get(0)).map_err(bug)
        });
        writing(&store, |tx| {
            tx.execute_batch("CREATE TEMP TABLE cut_parent(id INTEGER PRIMARY KEY); CREATE TEMP TABLE cut_probe(id INTEGER REFERENCES cut_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE TEMP TRIGGER cut_reject BEFORE UPDATE ON main.writer_probe BEGIN SELECT RAISE(ABORT,'fixture'); END").map_err(bug)
        });
        let result = block_on(store.write::<(), _>(WritePriority::User, move |tx| {
            tx.execute_batch(ADVANCE).map_err(bug)?;
            match cut {
                Cut::ReturnedError => Err(StoreError::internal("fixture rollback")),
                Cut::Panic => panic!("fixture rollback"),
                Cut::Statement => tx.execute_batch("UPDATE writer_probe SET n=1").map_err(bug),
                Cut::Commit => tx
                    .execute_batch("INSERT INTO cut_probe VALUES(1)")
                    .map_err(bug),
            }
        }));
        assert!(matches!(result, Err(StoreError::Internal { .. })));
        writing(&store, |_| Ok(()));
        assert_eq!(notices.load(Ordering::SeqCst), 0);
        assert_eq!(
            reading(&store, |tx| tx
                .query_row(INPUT, [], |row| row.get::<_, String>(0))
                .map_err(bug)),
            before
        );
        store.close().unwrap();
    }
}
