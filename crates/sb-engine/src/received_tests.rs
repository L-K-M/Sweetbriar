//! Actual imports, counted workers, native failure and stop/reopen proof.

use std::future::Future;
use std::pin::pin;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex, mpsc};
use std::task::{Context, Waker};
use std::time::Duration;

use sb_resources::{ImportJobState, Resources, StoreError, Timestamp, UserPause, WritePriority};
use sb_test_support::{ScratchDir, block_on, execute_sql, real_fs};

use crate::Engine;
use crate::index_tests::{End, clocked, close_and_reopen, count, import, wait, write};
use crate::received::Event;

mod repair;

const RAW: &[u8] = b"From: Sender <sender@EXAMPLE.test>\r\nTo: other@example.test\r\nSubject: current\r\n\r\nindexneedle\r\n";
const BUMP: &str = "UPDATE store_meta SET value=CAST(CAST(value AS INTEGER)+1 AS TEXT) WHERE key='seen_address_inputs'";
const LEDGER: &str = "SELECT count(*) FROM seen_address_messages";

fn settled(engine: &Engine) {
    wait(|| {
        let revisions = engine.resources().received_address_revisions().unwrap();
        revisions.inputs == revisions.swept && engine.received().waiting()
    });
    assert!(engine.received().failure().is_none());
}

fn change(engine: &Engine, sql: &str) {
    let sql = sql.to_owned();
    write(engine, move |tx| {
        tx.execute_batch(&sql).unwrap();
        assert_eq!(tx.execute(BUMP, []).unwrap(), 1);
        Ok(())
    });
}

fn hook(engine: &Engine, body: impl Fn(Event) + Send + Sync + 'static) {
    *engine.received().hook.lock().unwrap() = Some(Arc::new(body));
}

fn held(engine: &Engine) {
    settled(engine);
    write(engine, |tx| {
        assert_eq!(
            tx.execute(
                "UPDATE store_meta SET value='2' WHERE key='seen_address_target'",
                []
            )
            .unwrap(),
            1
        );
        Ok(())
    });
    wait(|| engine.received().failure().is_some() && engine.received().waiting());
}

fn release(engine: &Engine) {
    write(engine, |tx| {
        assert_eq!(
            tx.execute(
                "UPDATE store_meta SET value='1' WHERE key='seen_address_target'",
                []
            )
            .unwrap(),
            1
        );
        Ok(())
    });
}

fn history(resources: &Resources) -> String {
    resources.read(|tx| {
        tx.query_row("SELECT json_group_array(json_array(canonical,email,display_name,last_seen_at,times_received,times_sent_to,last_account_id)) FROM (SELECT * FROM seen_addresses ORDER BY canonical)", [], |row| row.get(0))
            .map_err(|error| StoreError::internal(error.to_string()))
    }).unwrap()
}

fn hold_writer(engine: &Engine) -> (mpsc::Sender<()>, sb_resources::WriteFuture<()>) {
    let (release, released) = mpsc::channel();
    let (inside, held) = mpsc::channel();
    let pending = engine.resources().write(WritePriority::User, move |_| {
        inside.send(()).unwrap();
        released.recv_timeout(Duration::from_secs(300)).unwrap();
        Ok(())
    });
    held.recv_timeout(Duration::from_secs(300)).unwrap();
    (release, pending)
}

#[test]
fn real_imports_populate_without_pokes_while_search_holds_and_partial_sources_stay_out() {
    let scratch = ScratchDir::new("received-import");
    let (engine, _) = clocked(&scratch.path().join("store"), real_fs(), 10_000);
    settled(&engine);
    write(&engine, |tx| {
        tx.execute_batch("INSERT INTO seen_addresses(canonical,email,display_name,last_seen_at,times_received,times_sent_to) VALUES('sender@example.test','sender@EXAMPLE.test','Kept',1000000,11,7)").unwrap();
        sb_resources::set_user_pause(tx, UserPause::Pause)?;
        sb_resources::pause_for_io(
            tx,
            sb_resources::SearchIoCode::OtherIo,
            "fixture",
            Timestamp::from_millis(3_610_000),
            5,
        )
    });
    engine.indexer().wake();
    import(&engine, &scratch.path().join("complete.eml"), RAW);
    import(&engine, &scratch.path().join("partial.eml"), b"From: partial@example.test\r\nContent-Type: multipart/mixed; boundary=neverthere\r\n\r\nno boundary\r\n");
    settled(&engine);
    assert_eq!(count(&engine, LEDGER), 2);
    assert_eq!(
        count(
            &engine,
            "SELECT count(*) FROM d_message_summary WHERE parse_state='partial'"
        ),
        1
    );
    assert_eq!(
        count(
            &engine,
            "SELECT count(*) FROM seen_addresses WHERE canonical='sender@example.test' AND display_name='Kept' AND times_received=12 AND times_sent_to=7"
        ),
        1
    );
    assert_eq!(
        count(&engine, "SELECT sum(times_received) FROM seen_addresses"),
        13
    );
    assert_eq!(count(&engine, "SELECT count(*) FROM d_search_docs"), 0);
    let coverage = block_on(engine.coverage(crate::SearchScope::All)).unwrap();
    assert_eq!(coverage.index_state, crate::SearchIndexState::PausedByUser);
    assert!(
        engine
            .indexer()
            .status(engine.resources())
            .unwrap()
            .io_pause()
            .is_some()
    );
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn index_writer_failure_does_not_gate_received_publication() {
    let scratch = ScratchDir::new("received-index-failure");
    let root = scratch.path().join("store");
    let (engine, _) = clocked(&root, real_fs(), 10_000);
    write(&engine, |tx| {
        sb_resources::set_user_pause(tx, UserPause::Pause)
    });
    engine.indexer().wake();
    import(&engine, &scratch.path().join("first.eml"), RAW);
    settled(&engine);
    let db = root.join("state.sqlite");
    let installed = AtomicBool::new(false);
    *engine.indexer().hook.lock().unwrap() = Some(Arc::new(move |event| {
        if matches!(event, crate::index::Event::Prepared { .. })
            && !installed.swap(true, Ordering::SeqCst)
        {
            execute_sql(
                &db,
                "PRAGMA busy_timeout=30000; CREATE TRIGGER reject_search BEFORE INSERT ON d_search_docs BEGIN SELECT RAISE(ABORT,'fixture'); END",
            );
        }
    }));
    write(&engine, |tx| {
        sb_resources::set_user_pause(tx, UserPause::Resume)
    });
    engine.indexer().wake();
    wait(|| engine.indexer().status(engine.resources()).is_err());
    assert_eq!(count(&engine, "SELECT count(*) FROM d_search_docs"), 0);
    import(
        &engine,
        &scratch.path().join("later.eml"),
        b"From: fresh@example.test\r\nSubject: later\r\n\r\nsecond\r\n",
    );
    settled(&engine);
    wait(|| engine.indexer().status(engine.resources()).is_err());
    assert_eq!(count(&engine, LEDGER), 3);
    assert_eq!(
        count(
            &engine,
            "SELECT count(*) FROM seen_addresses WHERE canonical='fresh@example.test' AND times_received=1"
        ),
        1
    );
    assert!(engine.indexer().status(engine.resources()).is_err());
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn captured_revision_ack_leaves_late_eligibility_of_an_old_id_owed() {
    let scratch = ScratchDir::new("received-old-id");
    let (engine, _) = clocked(&scratch.path().join("store"), real_fs(), 10_000);
    held(&engine);
    import(&engine, &scratch.path().join("old.eml"), RAW);
    import(
        &engine,
        &scratch.path().join("new.eml"),
        b"From: later@example.test\r\n\r\nlater\r\n",
    );
    change(
        &engine,
        "UPDATE d_message_summary SET parser_version=1 WHERE acquisition_id=(SELECT min(acquisition_id) FROM logical_messages)",
    );
    let captured = engine
        .resources()
        .received_address_revisions()
        .unwrap()
        .inputs;
    let first = AtomicBool::new(false);
    let weak = Arc::downgrade(engine.resources());
    let completions = Arc::new(Mutex::new(Vec::new()));
    let observed = completions.clone();
    hook(&engine, move |event| {
        let resources = weak.upgrade().unwrap();
        if event == Event::Committed && !first.swap(true, Ordering::SeqCst) {
            block_on(resources.write(WritePriority::User, |tx| {
                tx.execute_batch("UPDATE d_message_summary SET parser_version=2,from_name='Updated' WHERE acquisition_id=(SELECT min(acquisition_id) FROM logical_messages)").unwrap();
                assert_eq!(tx.execute(BUMP, []).unwrap(), 1);
                Ok(())
            })).unwrap();
        }
        if event == Event::Complete {
            observed
                .lock()
                .unwrap()
                .push(resources.received_address_revisions().unwrap());
        }
    });
    release(&engine);
    settled(&engine);
    let completions = completions.lock().unwrap();
    assert_eq!(completions.len(), 2);
    assert_eq!(
        (completions[0].swept, completions[0].inputs),
        (captured, captured + 1)
    );
    assert_eq!(completions[1].swept, captured + 1);
    assert_eq!(count(&engine, LEDGER), 3);
    assert_eq!(
        count(
            &engine,
            "SELECT count(*) FROM seen_addresses WHERE canonical='sender@example.test' AND display_name='Updated' AND times_received=1"
        ),
        1
    );
    drop(completions);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_park_handoff_wake_and_many_schedules_keep_one_worker() {
    let scratch = ScratchDir::new("received-wake");
    let root = scratch.path().join("store");
    let (engine, _) = clocked(&root, real_fs(), 10_000);
    import(&engine, &scratch.path().join("one.eml"), RAW);
    settled(&engine);
    let before = engine.resources().received_address_revisions().unwrap();
    let once = AtomicBool::new(false);
    let weak = Arc::downgrade(engine.received());
    let db = root.join("state.sqlite");
    hook(&engine, move |event| {
        if event == Event::Parked && !once.swap(true, Ordering::SeqCst) {
            execute_sql(
                &db,
                &format!(
                    "PRAGMA busy_timeout=30000; UPDATE d_message_summary SET from_name='Z winner'; {BUMP}"
                ),
            );
            weak.upgrade().unwrap().wake();
        }
    });
    for _ in 0..100 {
        engine.schedule_received();
        engine.received().wake();
    }
    wait(|| {
        engine
            .resources()
            .received_address_revisions()
            .unwrap()
            .swept
            > before.swept
    });
    settled(&engine);
    assert_eq!(engine.received().starts(), 1);
    assert_eq!(count(&engine, LEDGER), 2);
    assert_eq!(
        count(
            &engine,
            "SELECT count(*) FROM seen_addresses WHERE display_name='Z winner' AND times_received=1"
        ),
        1
    );
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn committed_old_message_inputs_wake_the_worker_without_an_import_or_manual_poke() {
    let scratch = ScratchDir::new("received-committed-wake");
    let (engine, _) = clocked(&scratch.path().join("store"), real_fs(), 10_000);
    import(&engine, &scratch.path().join("one.eml"), RAW);
    settled(&engine);
    let (completed, completion) = mpsc::channel();
    hook(&engine, move |event| {
        if event == Event::Complete {
            let _ = completed.send(());
        }
    });

    change(
        &engine,
        "UPDATE d_message_summary SET from_name='Updated after sweep'",
    );
    let observed = completion.recv_timeout(Duration::from_secs(30));
    let changed = count(
        &engine,
        "SELECT count(*) FROM seen_addresses WHERE canonical='sender@example.test' AND display_name='Updated after sweep' AND times_received=1",
    );
    block_on(engine.shutdown()).unwrap();
    assert!(
        observed.is_ok(),
        "committed input did not wake the parked worker"
    );
    assert_eq!(changed, 1);
}

#[test]
fn committed_target_repair_recovers_parked_failure_without_a_manual_poke() {
    for target in [Some("2"), Some("malformed"), None] {
        let scratch = ScratchDir::new("received-target-wake");
        let (engine, _) = clocked(&scratch.path().join("store"), real_fs(), 10_000);
        settled(&engine);
        write(&engine, move |tx| {
            if let Some(value) = target {
                tx.execute(
                    "UPDATE store_meta SET value=?1 WHERE key='seen_address_target'",
                    [value],
                )
                .unwrap();
            } else {
                tx.execute("DELETE FROM store_meta WHERE key='seen_address_target'", [])
                    .unwrap();
            }
            Ok(())
        });
        wait(|| engine.received().failure().is_some() && engine.received().waiting());
        let error = engine.received().failure().unwrap().error;
        assert!(if target == Some("2") {
            matches!(error, StoreError::Unavailable { io_kind: None, .. })
        } else {
            matches!(error, StoreError::Corrupt { .. })
        });
        write(&engine, |tx| {
            tx.execute("INSERT INTO store_meta(key,value) VALUES('seen_address_target','1') ON CONFLICT(key) DO UPDATE SET value=excluded.value", []).unwrap();
            Ok(())
        });
        settled(&engine);
        assert_eq!(engine.received().starts(), 1);
        block_on(engine.shutdown()).unwrap();
    }
}

#[test]
fn startup_replays_a_real_multichunk_import_without_recounting_committed_keys() {
    let scratch = ScratchDir::new("received-reopen");
    let root = scratch.path().join("store");
    let (engine, _) = clocked(&root, real_fs(), 10_000);
    held(&engine);
    let recipients = (0..1000)
        .map(|id| format!("u{id}@example.test"))
        .collect::<Vec<_>>()
        .join(",");
    import(
        &engine,
        &scratch.path().join("many.eml"),
        format!("To: {recipients}\r\n\r\nbody\r\n").as_bytes(),
    );
    let weak = Arc::downgrade(engine.received());
    hook(&engine, move |event| {
        if event == Event::Committed {
            weak.upgrade().unwrap().stop();
        }
    });
    release(&engine);
    wait(|| engine.received().stopped());
    let committed = count(&engine, LEDGER);
    assert!(committed > 0 && committed <= 200);
    let before = engine.resources().received_address_revisions().unwrap();
    assert!(before.swept < before.inputs);
    block_on(engine.shutdown()).unwrap();
    let engine = block_on(Engine::open(&root)).unwrap();
    settled(&engine);
    assert_eq!(count(&engine, LEDGER), 1000);
    assert_eq!(
        count(&engine, "SELECT sum(times_received) FROM seen_addresses"),
        1000
    );
    let kept = history(engine.resources());
    block_on(engine.shutdown()).unwrap();
    let engine = block_on(Engine::open(&root)).unwrap();
    settled(&engine);
    assert_eq!(history(engine.resources()), kept);
    assert_eq!(
        engine
            .resources()
            .received_address_revisions()
            .unwrap()
            .inputs,
        before.inputs
    );
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn native_write_and_commit_failures_roll_back_then_retry_at_bounded_deadlines() {
    for commit_failure in [false, true] {
        let scratch = ScratchDir::new("received-writer-failure");
        let (engine, clock) = clocked(&scratch.path().join("store"), real_fs(), 10_000);
        held(&engine);
        import(&engine, &scratch.path().join("one.eml"), RAW);
        let before = engine.resources().received_address_revisions().unwrap();
        write(&engine, move |tx| {
            if commit_failure {
                tx.execute_batch("CREATE TEMP TABLE cut_parent(id INTEGER PRIMARY KEY); CREATE TEMP TABLE cut_probe(id INTEGER REFERENCES cut_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE TEMP TRIGGER received_cut AFTER INSERT ON main.seen_address_messages BEGIN INSERT INTO cut_probe VALUES(1); END").unwrap();
            } else {
                tx.execute_batch("CREATE TEMP TRIGGER received_cut BEFORE INSERT ON main.seen_address_messages WHEN NEW.canonical='sender@example.test' BEGIN SELECT RAISE(ABORT,'fixture'); END").unwrap();
            }
            Ok(())
        });
        release(&engine);
        wait(|| {
            engine
                .received()
                .failure()
                .is_some_and(|f| matches!(f.error, StoreError::Internal { .. }))
                && engine.received().waiting()
        });
        assert_eq!(engine.received().failure().unwrap().attempts, 1);
        assert!(matches!(
            engine.received().failure().unwrap().error,
            StoreError::Internal { .. }
        ));
        assert_eq!(engine.received().retry_at().unwrap().as_millis(), 11_000);
        assert_eq!(count(&engine, LEDGER), 0);
        assert_eq!(count(&engine, "SELECT count(*) FROM seen_addresses"), 0);
        assert_eq!(
            engine
                .resources()
                .received_address_revisions()
                .unwrap()
                .swept,
            before.swept
        );
        // No poke: expiry of the real Condvar wait observes the injected clock.
        clock.0.store(11_000, Ordering::SeqCst);
        wait(|| {
            engine.received().failure().is_some_and(|f| f.attempts == 2)
                && engine.received().waiting()
        });
        assert_eq!(engine.received().retry_at().unwrap().as_millis(), 13_000);
        assert_eq!(count(&engine, LEDGER), 0);
        write(&engine, |tx| {
            tx.execute_batch("DROP TRIGGER received_cut").unwrap();
            Ok(())
        });
        clock.0.store(13_000, Ordering::SeqCst);
        settled(&engine);
        assert_eq!(count(&engine, LEDGER), 2);
        assert_eq!(
            count(&engine, "SELECT sum(times_received) FROM seen_addresses"),
            2
        );
        assert_eq!(
            engine
                .resources()
                .received_address_revisions()
                .unwrap()
                .inputs,
            before.inputs
        );
        block_on(engine.shutdown()).unwrap();
    }
}

#[test]
fn native_storage_full_is_typed_and_replays_any_committed_prefix() {
    let scratch = ScratchDir::new("received-full");
    let (engine, _) = clocked(&scratch.path().join("store"), real_fs(), 10_000);
    held(&engine);
    write(&engine, |tx| {
        sb_resources::set_user_pause(tx, UserPause::Pause)
    });
    engine.indexer().wake();
    let recipients = (0..800)
        .map(|id| format!("{} <u{id}@example.test>", "n".repeat(200)))
        .collect::<Vec<_>>()
        .join(",");
    import(
        &engine,
        &scratch.path().join("many.eml"),
        format!("To: {recipients}\r\n\r\nbody\r\n").as_bytes(),
    );
    write(&engine, |tx| {
        let pages: i64 = tx
            .query_row("PRAGMA page_count", [], |row| row.get(0))
            .unwrap();
        let bound: i64 = tx
            .query_row(&format!("PRAGMA max_page_count={pages}"), [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(pages, bound);
        Ok(())
    });
    release(&engine);
    wait(|| engine.received().failure().is_some() && engine.received().waiting());
    assert_eq!(
        engine.received().failure().unwrap().error.io_kind(),
        Some(std::io::ErrorKind::StorageFull)
    );
    let revisions = engine.resources().received_address_revisions().unwrap();
    assert!(revisions.swept < revisions.inputs);
    let committed = count(&engine, LEDGER);
    assert!(committed < 800);
    assert_eq!(
        count(
            &engine,
            "SELECT coalesce(sum(times_received),0) FROM seen_addresses"
        ),
        committed
    );
    assert!(block_on(engine.coverage(crate::SearchScope::All)).is_ok());
    write(&engine, |tx| {
        tx.query_row("PRAGMA max_page_count=1000000", [], |row| {
            row.get::<_, i64>(0)
        })
        .unwrap();
        Ok(())
    });
    engine.received().wake();
    settled(&engine);
    assert_eq!(count(&engine, LEDGER), 800);
    assert_eq!(
        count(&engine, "SELECT sum(times_received) FROM seen_addresses"),
        800
    );
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn corrupt_source_and_newer_target_park_without_gating_import_or_search() {
    for corrupt in [false, true] {
        let scratch = ScratchDir::new("received-terminal");
        let (engine, clock) = clocked(&scratch.path().join("store"), real_fs(), 10_000);
        held(&engine);
        import(&engine, &scratch.path().join("one.eml"), RAW);
        if corrupt {
            change(&engine, "UPDATE d_message_summary SET to_json='{' ");
            release(&engine);
        }
        wait(|| engine.received().failure().is_some() && engine.received().waiting());
        let failure = engine.received().failure().unwrap();
        assert!(if corrupt {
            matches!(failure.error, StoreError::Corrupt { .. })
        } else {
            matches!(failure.error, StoreError::Unavailable { io_kind: None, .. })
        });
        assert!(engine.received().retry_at().is_none());
        assert_eq!(count(&engine, LEDGER), 0);
        clock.0.store(1_000_000, Ordering::SeqCst);
        import(
            &engine,
            &scratch.path().join("later.eml"),
            b"From: fresh@example.test\r\n\r\nfreshneedle\r\n",
        );
        wait(|| count(&engine, "SELECT count(*) FROM d_search_docs") == 2);
        assert!(block_on(engine.coverage(crate::SearchScope::All)).is_ok());
        wait(|| engine.received().waiting());
        assert!(engine.received().failure().is_some());
        let before = engine.resources().received_address_revisions().unwrap();
        assert!(before.swept < before.inputs);
        change(
            &engine,
            "UPDATE d_message_summary SET to_json=NULL; UPDATE store_meta SET value='1' WHERE key='seen_address_target'",
        );
        settled(&engine);
        assert_eq!(count(&engine, LEDGER), 2);
        assert_eq!(
            count(&engine, "SELECT sum(times_received) FROM seen_addresses"),
            2
        );
        block_on(engine.shutdown()).unwrap();
    }
}

#[test]
fn a_dropped_accepted_confirm_still_imports_and_wakes_received_work() {
    let scratch = ScratchDir::new("received-caller-drop");
    let (engine, _) = clocked(&scratch.path().join("store"), real_fs(), 10_000);
    settled(&engine);
    let path = scratch.path().join("one.eml");
    std::fs::write(&path, RAW).unwrap();
    let started = block_on(engine.start_import(&engine.new_idempotency_key(), vec![path])).unwrap();
    wait(|| {
        block_on(engine.import_job(started.job_id()))
            .unwrap()
            .state()
            == ImportJobState::AwaitingConfirmation
    });
    let (release, writer) = hold_writer(&engine);
    let key = engine.new_idempotency_key();
    let before = engine.accepted_commands();
    {
        let mut future = pin!(engine.confirm_import(&key, started.job_id(), vec![]));
        wait(|| {
            assert!(
                future
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop()))
                    .is_pending()
            );
            engine.accepted_commands() > before
        });
    }
    release.send(()).unwrap();
    block_on(writer).unwrap();
    wait(|| {
        block_on(engine.import_job(started.job_id()))
            .unwrap()
            .state()
            == ImportJobState::Completed
    });
    settled(&engine);
    assert_eq!(count(&engine, LEDGER), 2);
    assert_eq!(
        count(&engine, "SELECT sum(times_received) FROM seen_addresses"),
        2
    );
    block_on(engine.shutdown()).unwrap();
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum StopAt {
    Parked,
    Prepared,
    Submitted,
}

#[test]
fn shutdown_and_drop_discard_preparation_but_drain_submitted_writes_and_release_the_store() {
    for end in [End::Shutdown, End::Drop] {
        for at in [StopAt::Parked, StopAt::Prepared, StopAt::Submitted] {
            let scratch = ScratchDir::new("received-stop");
            let root = scratch.path().join("store");
            let (engine, _) = clocked(&root, real_fs(), 10_000);
            held(&engine);
            import(&engine, &scratch.path().join("one.eml"), RAW);
            let resources = if at == StopAt::Parked {
                wait(|| engine.received().waiting());
                close_and_reopen(engine, &root, end)
            } else {
                let (ready, reached) = mpsc::channel();
                let (release, released) = mpsc::channel();
                let released = Mutex::new(released);
                let (prepared, preparing) = mpsc::channel();
                let (submit, submitting) = mpsc::channel();
                let submitting = Mutex::new(submitting);
                hook(&engine, move |event| {
                    // Hold the writer before accepting the write whose drain is tested.
                    if at == StopAt::Submitted && event == Event::Prepared {
                        prepared.send(()).unwrap();
                        submitting
                            .lock()
                            .unwrap()
                            .recv_timeout(Duration::from_secs(300))
                            .unwrap();
                    }
                    if (at == StopAt::Prepared && event == Event::Prepared)
                        || (at == StopAt::Submitted && event == Event::Submitted)
                    {
                        ready.send(()).unwrap();
                        released
                            .lock()
                            .unwrap()
                            .recv_timeout(Duration::from_secs(300))
                            .unwrap();
                    }
                });
                // The target's COMMIT now wakes the already-armed gate.
                write(&engine, |tx| {
                    tx.execute(
                        "UPDATE store_meta SET value='1' WHERE key='seen_address_target'",
                        [],
                    )
                    .unwrap();
                    Ok(())
                });
                let writer = (at == StopAt::Submitted).then(|| {
                    preparing.recv_timeout(Duration::from_secs(300)).unwrap();
                    let held = hold_writer(&engine);
                    submit.send(()).unwrap();
                    held
                });
                reached.recv_timeout(Duration::from_secs(300)).unwrap();
                let registry = engine.received().clone();
                let owned = root.clone();
                let closing = std::thread::spawn(move || close_and_reopen(engine, &owned, end));
                wait(|| registry.stopped());
                release.send(()).unwrap();
                if let Some((release, pending)) = writer {
                    release.send(()).unwrap();
                    block_on(pending).unwrap();
                }
                closing.join().unwrap()
            };
            let ledger: i64 = resources
                .read(|tx| Ok(tx.query_row(LEDGER, [], |row| row.get(0)).unwrap()))
                .unwrap();
            assert_eq!(ledger, if at == StopAt::Submitted { 2 } else { 0 });
            let revisions = resources.received_address_revisions().unwrap();
            assert!(revisions.swept < revisions.inputs);
            block_on(resources.write(WritePriority::User, |tx| {
                tx.execute(
                    "UPDATE store_meta SET value='1' WHERE key='seen_address_target'",
                    [],
                )
                .unwrap();
                Ok(())
            }))
            .unwrap();
            resources.close().unwrap();
            let engine = block_on(Engine::open(&root)).unwrap();
            settled(&engine);
            assert_eq!(count(&engine, LEDGER), 2);
            assert_eq!(
                count(&engine, "SELECT sum(times_received) FROM seen_addresses"),
                2
            );
            block_on(engine.shutdown()).unwrap();
        }
    }
}

#[test]
fn a_worker_task_panic_is_explicit_and_keeps_unsubmitted_work_owed() {
    let scratch = ScratchDir::new("received-task-panic");
    let root = scratch.path().join("store");
    let (engine, _) = clocked(&root, real_fs(), 10_000);
    held(&engine);
    import(&engine, &scratch.path().join("one.eml"), RAW);
    hook(&engine, |event| {
        if event == Event::Prepared {
            panic!("fixture worker panic");
        }
    });
    release(&engine);
    wait(|| {
        engine
            .received()
            .failure()
            .is_some_and(|f| matches!(f.error, StoreError::Internal { .. }))
    });
    assert_eq!(count(&engine, LEDGER), 0);
    let revisions = engine.resources().received_address_revisions().unwrap();
    assert!(revisions.swept < revisions.inputs);
    block_on(engine.shutdown()).unwrap();
    let engine = block_on(Engine::open(&root)).unwrap();
    settled(&engine);
    assert_eq!(count(&engine, LEDGER), 2);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn real_search_and_thread_rebuilds_keep_all_address_history() {
    let scratch = ScratchDir::new("received-rebuild");
    let (engine, _) = clocked(&scratch.path().join("store"), real_fs(), 10_000);
    import(&engine, &scratch.path().join("one.eml"), RAW);
    settled(&engine);
    wait(|| count(&engine, "SELECT count(*) FROM d_search_docs") == 1);
    wait(|| {
        !engine
            .resources()
            .read(sb_resources::thread_revisions)
            .unwrap()
            .stale()
    });
    write(&engine, |tx| {
        tx.execute_batch("UPDATE seen_addresses SET times_sent_to=7; UPDATE store_meta SET value=CAST(CAST(value AS INTEGER)+1 AS TEXT) WHERE key='thread_inputs'").unwrap();
        Ok(())
    });
    let before = history(engine.resources());
    let revisions = engine.resources().received_address_revisions().unwrap();
    let started = block_on(engine.start_search_rebuild(&engine.new_idempotency_key())).unwrap();
    engine.schedule_threads();
    wait(|| {
        engine
            .resources()
            .read(|tx| sb_resources::rebuild_job(tx, started.job_id()))
            .is_ok_and(|job| job.phase() == sb_resources::RebuildPhase::Complete)
    });
    wait(|| {
        !engine
            .resources()
            .read(sb_resources::thread_revisions)
            .unwrap()
            .stale()
    });
    settled(&engine);
    assert_eq!(count(&engine, "SELECT count(*) FROM d_search_docs"), 1);
    assert_eq!(
        count(
            &engine,
            "SELECT count(*) FROM d_search_fts WHERE d_search_fts MATCH 'indexneedle'"
        ),
        1
    );
    assert_eq!(count(&engine, LEDGER), 2);
    assert_eq!(history(engine.resources()), before);
    assert_eq!(
        engine.resources().received_address_revisions().unwrap(),
        revisions
    );
    block_on(engine.shutdown()).unwrap();
}
