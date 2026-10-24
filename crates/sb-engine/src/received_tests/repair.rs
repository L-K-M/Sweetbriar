use super::*;
use crate::received::{Lane, Step};
use std::sync::atomic::AtomicU64;

const SIMPLE: &[u8] = b"From: new@x.test\r\nSubject: new\r\n\r\nbody";
const BINDINGS: &str = "SELECT count(*) FROM d_message_bindings";
const MESSAGES: i64 = 100_000;
const PARTIAL: &[u8] = b"Content-Type: multipart/mixed; boundary=missing\r\n\r\nbody";

fn image(resources: &Resources) -> String {
    resources.read(|tx| Ok(tx.query_row("SELECT json_array(
        (SELECT count(*) FROM d_repair_assemblies),(SELECT count(*) FROM d_repair_pieces),
        (SELECT count(*) FROM d_repair_assemblies WHERE completed_at IS NOT NULL),
        (SELECT count(*) FROM d_repair_diagnoses),(SELECT count(*) FROM d_message_bindings),
        (SELECT count(*) FROM seen_address_messages),
        (SELECT json_group_array(json_array(key,value)) FROM (SELECT key,value FROM store_meta WHERE key IN('seen_address_swept','repair_swept','repair_unready','repair_deferred') ORDER BY key)))", [], |row| row.get(0)).unwrap())).unwrap()
}

fn lane_parked(engine: &Engine, lane: Lane, attempts: u32) {
    wait(|| {
        engine
            .received()
            .failure_for(lane)
            .is_some_and(|f| f.attempts == attempts && f.retry_at.is_some())
            && engine.received().waiting()
    });
}

#[derive(Clone, Copy)]
enum WriteCut {
    Statement,
    Commit,
}

fn fault(engine: &Engine, table: &str, acquisition: i64, cut: WriteCut) {
    let body = match cut {
        WriteCut::Statement => "SELECT RAISE(ABORT,'fixture')",
        WriteCut::Commit => "INSERT INTO fault_probe VALUES(1)",
    };
    let sql = format!(
        "CREATE TEMP TABLE fault_parent(id INTEGER PRIMARY KEY); CREATE TEMP TABLE fault_probe(id INTEGER REFERENCES fault_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE TEMP TRIGGER repair_fault AFTER INSERT ON main.{table} WHEN NEW.acquisition_id={acquisition} BEGIN {body}; END"
    );
    write(engine, move |tx| {
        tx.execute_batch(&sql).unwrap();
        Ok(())
    });
}

fn seed(messages: i64, bytes: &[u8]) -> (ScratchDir, Engine, Arc<crate::index_tests::TestClock>) {
    let scratch = ScratchDir::new("received-repair");
    let (engine, clock) = seed_at(&scratch.path().join("store"), messages, bytes);
    (scratch, engine, clock)
}

fn seed_at(
    root: &std::path::Path,
    messages: i64,
    bytes: &[u8],
) -> (Engine, Arc<crate::index_tests::TestClock>) {
    let (engine, clock) = clocked(root, real_fs(), 10_000);
    settled(&engine);
    write(&engine, |tx| {
        sb_resources::set_user_pause(tx, UserPause::Pause)?;
        tx.execute_batch("UPDATE store_meta SET value='2' WHERE key='seen_address_target'; UPDATE store_meta SET value='3' WHERE key='mime_repair_target'").unwrap();
        Ok(())
    });
    engine.indexer().wake();
    wait(|| engine.received().waiting());
    import(&engine, &root.with_extension("eml"), bytes);
    wait(|| {
        !engine
            .resources()
            .read(sb_resources::thread_revisions)
            .unwrap()
            .stale()
    });
    write(&engine, move |tx| {
        tx.execute("WITH RECURSIVE n(id) AS (VALUES(2) UNION ALL SELECT id+1 FROM n WHERE id<?1) INSERT INTO logical_messages(message_id,account_id,acquisition_id,created_at) SELECT id,account_id,1,0 FROM n CROSS JOIN accounts WHERE provider='local_archive' AND id<=?1", [messages]).unwrap();
        tx.execute_batch("INSERT INTO occurrences(occurrence_id,account_id,mailbox_id,message_id,state,first_seen_at,last_seen_at) SELECT m.message_id,m.account_id,1,m.message_id,'present',10,10 FROM logical_messages m WHERE m.message_id>1;
            INSERT INTO d_list_rows SELECT occurrence_id,mailbox_id,NULL,account_id,10,message_id,'old@x.test','OLD','old',1,0,0,1 FROM occurrences WHERE message_id>1;
            UPDATE d_message_summary SET parser_version=1,subject='OLD',from_addr='old@x.test',snippet='old'; UPDATE d_list_rows SET sender='old@x.test',subject='OLD',snippet='old';
            INSERT INTO seen_addresses(canonical,email,last_seen_at,times_received,times_sent_to) VALUES('kept@x.test','kept@x.test',-1,11,7)").unwrap();
        Ok(())
    });
    (engine, clock)
}

fn start(engine: &Engine) {
    write(engine, |tx| {
        tx.execute_batch("UPDATE store_meta SET value='1' WHERE key='seen_address_target'; UPDATE store_meta SET value='2' WHERE key='mime_repair_target'").unwrap();
        Ok(())
    });
}

fn complete(engine: &Engine) {
    wait(|| {
        let revisions = engine.resources().received_address_revisions().unwrap();
        revisions.inputs == revisions.swept
            && engine.resources().catchup_status().unwrap().caught_up()
            && engine.received().failure().is_none()
            && engine.received().waiting()
    });
    assert!(engine.received().failure().is_none());
}

#[test]
fn startup_worker_repairs_shared_sources_while_fts_is_held() {
    let (_scratch, engine, _) = seed(3, SIMPLE);
    start(&engine);
    complete(&engine);
    assert!(engine.resources().catchup_status().unwrap().complete());
    assert_eq!(count(&engine, BINDINGS), 3);
    assert_eq!(count(&engine, LEDGER), 3);
    assert_eq!(count(&engine, "SELECT count(*) FROM d_search_docs"), 0);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_finite_activation_pass_coalesces_thread_dispatch_until_its_boundary() {
    let (_scratch, engine, _) = seed(1000, SIMPLE);
    let threads = engine.imports().threads().clone();
    let baseline = threads.requests();
    let observed = Arc::new(Mutex::new(None));
    let captured = observed.clone();
    hook(&engine, move |event| {
        if matches!(event, Event::CatchupCommitted { activated, .. } if activated > 0) {
            let mut captured = captured.lock().unwrap();
            if captured.is_none() {
                *captured = Some(threads.requests() - baseline);
            }
        }
    });
    start(&engine);
    wait(|| observed.lock().unwrap().is_some());
    engine.received().stop();
    block_on(engine.shutdown()).unwrap();
    assert_eq!(observed.lock().unwrap().unwrap(), 0);
}

#[test]
fn whole_message_deferral_parks_incomplete_while_a_neighbor_progresses_then_reconsiders() {
    let (_scratch, engine, _) = seed(2, SIMPLE);
    write(&engine, |tx| {
        tx.execute_batch("WITH RECURSIVE n(id) AS (VALUES(3) UNION ALL SELECT id+1 FROM n WHERE id<4002) INSERT INTO occurrences(occurrence_id,account_id,mailbox_id,message_id,state,first_seen_at,last_seen_at) SELECT id,account_id,1,1,'present',10,10 FROM n CROSS JOIN accounts WHERE provider='local_archive';
            INSERT INTO d_list_rows SELECT occurrence_id,mailbox_id,NULL,account_id,10,message_id,'old@x.test','OLD','old',1,0,0,0 FROM occurrences WHERE occurrence_id>2").unwrap();
        Ok(())
    });
    start(&engine);
    complete(&engine);
    let status = engine.resources().catchup_status().unwrap();
    assert!(status.caught_up() && !status.complete());
    assert_eq!((status.deferred, status.unready), (1, 0));
    assert_eq!(count(&engine, BINDINGS), 1);
    assert_eq!(count(&engine, LEDGER), 1);
    assert_eq!(
        count(
            &engine,
            "SELECT count(*) FROM d_list_rows WHERE message_id=1 AND (subject IS NOT 'OLD' OR sender IS NOT 'old@x.test')"
        ),
        0
    );
    write(&engine, |tx| {
        tx.execute_batch("DELETE FROM d_list_rows WHERE occurrence_id>2; DELETE FROM occurrences WHERE occurrence_id>2").unwrap();
        Ok(())
    });
    complete(&engine);
    assert!(engine.resources().catchup_status().unwrap().complete());
    assert_eq!(count(&engine, BINDINGS), 2);
    assert_eq!(count(&engine, LEDGER), 2);
    assert_eq!(
        count(
            &engine,
            "SELECT times_received FROM seen_addresses WHERE canonical='new@x.test'"
        ),
        2
    );
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn mixed_source_failures_do_not_starve_fresh_mail_or_hot_retry_on_irrelevant_wakes() {
    for cut in [WriteCut::Statement, WriteCut::Commit] {
        let (scratch, engine, clock) = seed(1, SIMPLE);
        import(
            &engine,
            &scratch.path().join("good.eml"),
            b"From: good@x.test\r\n\r\ngood",
        );
        import(
            &engine,
            &scratch.path().join("fresh.eml"),
            b"From: fresh@x.test\r\n\r\nfresh",
        );
        wait(|| {
            !engine
                .resources()
                .read(sb_resources::thread_revisions)
                .unwrap()
                .stale()
        });
        write(&engine, |tx| {
            tx.execute_batch("UPDATE d_message_summary SET parser_version=1 WHERE acquisition_id=2; UPDATE acquisitions SET object_state='quarantined' WHERE acquisition_id=1").unwrap();
            Ok(())
        });
        fault(&engine, "d_repair_assemblies", 2, cut);
        let begins = Arc::new(AtomicU64::new(0));
        let observed = begins.clone();
        let resources = Arc::downgrade(engine.resources());
        let registry = Arc::downgrade(engine.received());
        let (done, folded) = mpsc::channel();
        hook(&engine, move |event| {
            if matches!(event, Event::RepairBegin(_)) {
                observed.fetch_add(1, Ordering::SeqCst);
            }
            if event == Event::SourceComplete
                && resources
                    .upgrade()
                    .unwrap()
                    .read(|tx| {
                        Ok(tx
                            .query_row("SELECT count(*) FROM d_repair_assemblies WHERE completed_at IS NOT NULL", [], |row| row.get::<_, i64>(0))
                            .unwrap())
                    })
                    .unwrap()
                    > 0
            {
                let registry = registry.upgrade().unwrap();
                let _ = done.send((
                    registry.failure_for(Lane::SourceRetry),
                    registry.failure_for(Lane::SourcePark),
                ));
            }
        });
        start(&engine);
        lane_parked(&engine, Lane::SourceRetry, 1);
        assert!(matches!(
            engine
                .received()
                .failure_for(Lane::SourcePark)
                .unwrap()
                .error,
            StoreError::Corrupt { .. }
        ));
        assert_eq!(
            engine
                .received()
                .failure_for(Lane::SourceRetry)
                .unwrap()
                .retry_at
                .unwrap()
                .as_millis(),
            11_000
        );
        assert_eq!(count(&engine, LEDGER), 1);
        let before = begins.load(Ordering::SeqCst);
        for _ in 0..3 {
            change(&engine, "SELECT 1");
            wait(|| engine.received().waiting());
        }
        assert_eq!(begins.load(Ordering::SeqCst), before);
        clock.0.store(11_000, Ordering::SeqCst);
        lane_parked(&engine, Lane::SourceRetry, 2);
        assert_eq!(
            engine
                .received()
                .failure_for(Lane::SourceRetry)
                .unwrap()
                .retry_at
                .unwrap()
                .as_millis(),
            13_000
        );
        write(&engine, |tx| {
            tx.execute_batch("DROP TRIGGER repair_fault").unwrap();
            Ok(())
        });
        clock.0.store(13_000, Ordering::SeqCst);
        let (retry, parked) = folded.recv_timeout(Duration::from_secs(300)).unwrap();
        engine.received().stop();
        block_on(engine.shutdown()).unwrap();
        assert!(
            retry.is_none(),
            "a fully revalidated resolved retry class stayed armed"
        );
        assert!(parked.is_some());
    }
}

#[test]
fn terminal_persistence_failure_is_explicit_retries_and_then_suppresses_unchanged_work() {
    for cut in [WriteCut::Statement, WriteCut::Commit] {
        let (scratch, engine, clock) = seed(1, SIMPLE);
        import(
            &engine,
            &scratch.path().join("partial.eml"),
            b"Content-Type: multipart/mixed; boundary=missing\r\n\r\nbody",
        );
        wait(|| {
            !engine
                .resources()
                .read(sb_resources::thread_revisions)
                .unwrap()
                .stale()
        });
        write(&engine, |tx| {
            tx.execute_batch("UPDATE d_message_summary SET parser_version=2,subject='new',from_addr='new@x.test',snippet='body' WHERE acquisition_id=1; UPDATE d_list_rows SET sender='new@x.test',subject='new',snippet='body' WHERE message_id=1; UPDATE d_message_summary SET parser_version=1 WHERE acquisition_id=2").unwrap();
            Ok(())
        });
        fault(&engine, "d_repair_diagnoses", 2, cut);
        start(&engine);
        lane_parked(&engine, Lane::SourceRetry, 1);
        assert_eq!(count(&engine, "SELECT count(*) FROM d_repair_diagnoses"), 0);
        assert_eq!(count(&engine, BINDINGS), 0);
        assert_eq!(count(&engine, LEDGER), 1);
        write(&engine, |tx| {
            tx.execute_batch("DROP TRIGGER repair_fault").unwrap();
            Ok(())
        });
        clock.0.store(11_000, Ordering::SeqCst);
        wait(|| {
            engine
                .resources()
                .repair_status(2)
                .unwrap()
                .diagnosis
                .is_some()
        });
        complete(&engine);
        let status = engine.resources().catchup_status().unwrap();
        assert!(status.caught_up() && !status.complete() && status.unready == 1);
        assert_eq!(
            engine
                .resources()
                .repair_status(2)
                .unwrap()
                .diagnosis
                .unwrap()
                .kind,
            sb_resources::RepairTerminalKind::Partial
        );
        let attempts = Arc::new(AtomicU64::new(0));
        let observed = attempts.clone();
        hook(&engine, move |event| {
            if matches!(event, Event::RepairBegin(_)) {
                observed.fetch_add(1, Ordering::SeqCst);
            }
        });
        for _ in 0..3 {
            change(&engine, "SELECT 1");
            wait(|| engine.received().waiting());
        }
        assert_eq!(attempts.load(Ordering::SeqCst), 0);
        assert_eq!(count(&engine, "SELECT count(*) FROM d_repair_diagnoses"), 1);
        block_on(engine.shutdown()).unwrap();
    }
}

#[test]
fn actual_object_io_failures_keep_their_kind_and_anchored_automatic_retry() {
    use sb_test_support::{FaultFs, FaultKind, FsOp};
    for (fault, kind) in [
        (FaultKind::NoSpace, Some(std::io::ErrorKind::StorageFull)),
        (FaultKind::Eio, None),
        (
            FaultKind::Kind(std::io::ErrorKind::PermissionDenied),
            Some(std::io::ErrorKind::PermissionDenied),
        ),
    ] {
        let (scratch, engine, _) = seed(1, SIMPLE);
        let root = scratch.path().join("store");
        block_on(engine.shutdown()).unwrap();
        let fs = Arc::new(FaultFs::new(real_fs(), FsOp::Open, 1, fault));
        let (engine, clock) = clocked(&root, fs.clone(), 10_000);
        start(&engine);
        lane_parked(&engine, Lane::SourceRetry, 1);
        let actual = engine
            .received()
            .failure_for(Lane::SourceRetry)
            .unwrap()
            .error
            .io_kind();
        assert!(actual.is_some());
        if let Some(kind) = kind {
            assert_eq!(actual, Some(kind));
        }
        assert_eq!(fs.calls(FsOp::Open), 1);
        clock.0.store(10_500, Ordering::SeqCst);
        change(&engine, "SELECT 1");
        wait(|| engine.received().waiting());
        assert_eq!(
            engine
                .received()
                .failure_for(Lane::SourceRetry)
                .unwrap()
                .retry_at
                .unwrap()
                .as_millis(),
            11_000
        );
        assert_eq!(fs.calls(FsOp::Open), 1);
        assert_eq!(count(&engine, BINDINGS), 0);
        clock.0.store(11_000, Ordering::SeqCst);
        complete(&engine);
        assert_eq!(fs.calls(FsOp::Open), 2);
        assert_eq!(count(&engine, BINDINGS), 1);
        assert_eq!(count(&engine, LEDGER), 1);
        block_on(engine.shutdown()).unwrap();
    }
}

#[test]
fn an_expired_source_retry_is_consumed_before_a_global_nonretryable_abort() {
    use sb_test_support::{FaultFs, FaultKind, FsOp};
    let (scratch, engine, _) = seed(1, SIMPLE);
    let root = scratch.path().join("store");
    block_on(engine.shutdown()).unwrap();
    let (engine, clock) = clocked(
        &root,
        Arc::new(FaultFs::new(real_fs(), FsOp::Open, 1, FaultKind::NoSpace)),
        10_000,
    );
    start(&engine);
    lane_parked(&engine, Lane::SourceRetry, 1);
    let registry = Arc::downgrade(engine.received());
    let (failed, failure) = mpsc::channel();
    hook(&engine, move |event| {
        if event == Event::Failure(Lane::SourcePark) {
            let _ = failed.send(registry.upgrade().unwrap().failure_for(Lane::SourceRetry));
        }
    });
    clock.0.store(11_000, Ordering::SeqCst);
    write(&engine, |tx| {
        tx.execute(
            "UPDATE store_meta SET value='3' WHERE key='mime_repair_target'",
            [],
        )
        .unwrap();
        Ok(())
    });
    let retained = failure
        .recv_timeout(Duration::from_secs(300))
        .unwrap()
        .unwrap();
    engine.received().stop();
    block_on(engine.shutdown()).unwrap();
    assert!(
        retained.retry_at.is_none(),
        "expired deadline survived the attempt and would hot-poll the global parked error"
    );
    assert_eq!(retained.attempts, 1);
}

#[test]
fn a_parked_status_error_recovers_when_valid_metadata_returns_to_the_same_pair() {
    let (_scratch, engine, _) = seed(1, SIMPLE);
    start(&engine);
    complete(&engine);
    let inputs = engine
        .resources()
        .catchup_status()
        .unwrap()
        .inputs
        .to_string();
    write(&engine, |tx| {
        tx.execute(
            "UPDATE store_meta SET value='bad' WHERE key='repair_inputs'",
            [],
        )
        .unwrap();
        Ok(())
    });
    wait(|| {
        engine.received().failure_for(Lane::SourcePark).is_some() && engine.received().waiting()
    });
    write(&engine, move |tx| {
        tx.execute(
            "UPDATE store_meta SET value=?1 WHERE key='repair_inputs'",
            [inputs],
        )
        .unwrap();
        Ok(())
    });
    complete(&engine);
    assert_eq!(count(&engine, BINDINGS), 1);
    assert_eq!(count(&engine, LEDGER), 1);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn replaying_an_unchanged_received_key_does_not_rewrite_its_kept_row() {
    let (_scratch, engine, _) = seed(1, SIMPLE);
    start(&engine);
    complete(&engine);
    let kept = history(engine.resources());
    write(&engine, |tx| {
        tx.execute_batch("CREATE TEMP TRIGGER reject_replayed_history BEFORE UPDATE ON main.seen_addresses WHEN OLD.canonical='new@x.test' BEGIN SELECT RAISE(ABORT,'unchanged history rewrite'); END").unwrap();
        Ok(())
    });
    let (finished, result) = mpsc::channel();
    hook(&engine, move |event| {
        if event == Event::Failure(Lane::Population) {
            let _ = finished.send(false);
        }
        if event == Event::Complete {
            let _ = finished.send(true);
        }
    });
    change(&engine, "SELECT 1");
    let successful = result.recv_timeout(Duration::from_secs(300)).unwrap();
    engine.received().stop();
    assert_eq!(history(engine.resources()), kept);
    block_on(engine.shutdown()).unwrap();
    assert!(
        successful,
        "replayed ledger membership issued an unnecessary kept-row UPDATE"
    );
}

fn current_source(engine: &Engine) {
    write(engine, |tx| {
        tx.execute_batch("UPDATE d_message_summary SET parser_version=2,subject='new',from_addr='new@x.test',snippet='body'; UPDATE d_list_rows SET sender='new@x.test',subject='new',snippet='body'").unwrap();
        Ok(())
    });
}

#[test]
fn current_population_batches_many_messages_within_one_key_and_byte_budget() {
    let (_scratch, engine, _) = seed(1000, SIMPLE);
    current_source(&engine);
    let commits = Arc::new(AtomicU64::new(0));
    let seen = commits.clone();
    let before = Arc::new(AtomicU64::new(0));
    let starting = before.clone();
    let resources = Arc::downgrade(engine.resources());
    hook(&engine, move |event| {
        if !matches!(
            event,
            Event::BeforeWrite(Step::Population) | Event::WriteCommitted(Step::Population)
        ) {
            return;
        }
        let keys = resources
            .upgrade()
            .unwrap()
            .read(|tx| {
                Ok(tx
                    .query_row(LEDGER, [], |row| row.get::<_, i64>(0))
                    .unwrap())
            })
            .unwrap() as u64;
        if event == Event::BeforeWrite(Step::Population) {
            starting.store(keys, Ordering::SeqCst);
        } else {
            assert!(keys - starting.load(Ordering::SeqCst) <= 200);
            seen.fetch_add(1, Ordering::SeqCst);
        }
    });
    start(&engine);
    complete(&engine);
    assert_eq!(count(&engine, LEDGER), 1000);
    let writes = commits.load(Ordering::SeqCst);
    block_on(engine.shutdown()).unwrap();
    assert!(
        writes <= 8,
        "1000 single-key messages required {writes} transactions instead of bounded batches"
    );
}

#[test]
fn a_later_message_write_or_commit_failure_rolls_back_the_entire_population_batch() {
    for cut in [WriteCut::Statement, WriteCut::Commit] {
        let (_scratch, engine, clock) = seed(2, SIMPLE);
        current_source(&engine);
        let action = match cut {
            WriteCut::Statement => "SELECT RAISE(ABORT,'fixture')",
            WriteCut::Commit => "INSERT INTO batch_probe VALUES(1)",
        };
        write(&engine, move |tx| {
            tx.execute_batch(&format!("CREATE TEMP TABLE batch_parent(id INTEGER PRIMARY KEY); CREATE TEMP TABLE batch_probe(id INTEGER REFERENCES batch_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE TEMP TRIGGER reject_later_message AFTER INSERT ON main.seen_address_messages WHEN NEW.message_id=2 BEGIN {action}; END")).unwrap();
            Ok(())
        });
        start(&engine);
        lane_parked(&engine, Lane::Population, 1);
        let ledger = count(&engine, LEDGER);
        write(&engine, |tx| {
            tx.execute_batch("DROP TRIGGER reject_later_message")
                .unwrap();
            Ok(())
        });
        clock.0.store(11_000, Ordering::SeqCst);
        complete(&engine);
        assert_eq!(count(&engine, LEDGER), 2);
        assert_eq!(
            count(
                &engine,
                "SELECT times_received FROM seen_addresses WHERE canonical='new@x.test'"
            ),
            2
        );
        block_on(engine.shutdown()).unwrap();
        assert_eq!(
            ledger, 0,
            "an earlier message escaped the later-message rollback"
        );
    }
}

#[test]
fn a_changed_later_source_recaptures_before_any_earlier_message_mutation() {
    let (scratch, engine, _) = seed(2, SIMPLE);
    current_source(&engine);
    let resources = Arc::downgrade(engine.resources());
    let registry = Arc::downgrade(engine.received());
    let changed = AtomicBool::new(false);
    let (finished, result) = mpsc::channel();
    hook(&engine, move |event| {
        let resources = resources.upgrade().unwrap();
        if event == Event::BeforeWrite(Step::Population) && !changed.swap(true, Ordering::SeqCst) {
            block_on(resources.write(WritePriority::User, |tx| {
                tx.execute_batch(&format!(
                    "UPDATE occurrences SET first_seen_at=20 WHERE message_id=2; {BUMP}"
                ))
                .unwrap();
                Ok(())
            }))
            .unwrap();
        }
        if event == Event::WriteCommitted(Step::Population) {
            let keys = resources
                .read(|tx| {
                    Ok(tx
                        .query_row(LEDGER, [], |row| row.get::<_, i64>(0))
                        .unwrap())
                })
                .unwrap();
            registry.upgrade().unwrap().stop();
            let _ = finished.send(keys);
        }
    });
    start(&engine);
    let keys = result.recv_timeout(Duration::from_secs(300)).unwrap();
    block_on(engine.shutdown()).unwrap();
    assert_eq!(keys, 0);
    let engine = block_on(Engine::open(&scratch.path().join("store"))).unwrap();
    complete(&engine);
    assert_eq!(count(&engine, LEDGER), 2);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn corpus_write_and_commit_failures_preserve_published_sources_and_retry_independently() {
    for cut in [WriteCut::Statement, WriteCut::Commit] {
        let (_scratch, engine, clock) = seed(2, SIMPLE);
        fault(&engine, "d_message_bindings", 1, cut);
        start(&engine);
        lane_parked(&engine, Lane::Corpus, 1);
        assert_eq!(
            count(
                &engine,
                "SELECT count(*) FROM d_repair_assemblies WHERE completed_at IS NOT NULL"
            ),
            1
        );
        assert_eq!(count(&engine, BINDINGS), 0);
        assert_eq!(count(&engine, LEDGER), 0);
        assert!(engine.received().failure_for(Lane::SourceRetry).is_none());
        assert!(engine.received().failure_for(Lane::SourcePark).is_none());
        assert_eq!(
            engine
                .received()
                .failure_for(Lane::Corpus)
                .unwrap()
                .retry_at
                .unwrap()
                .as_millis(),
            11_000
        );
        write(&engine, |tx| {
            tx.execute_batch("DROP TRIGGER repair_fault").unwrap();
            Ok(())
        });
        clock.0.store(11_000, Ordering::SeqCst);
        complete(&engine);
        assert_eq!(count(&engine, BINDINGS), 2);
        assert_eq!(count(&engine, LEDGER), 2);
        block_on(engine.shutdown()).unwrap();
    }
}

#[test]
fn a_failed_corpus_write_arms_backoff_from_completion_not_the_round_start() {
    let (_scratch, engine, clock) = seed(1, SIMPLE);
    fault(&engine, "d_message_bindings", 1, WriteCut::Statement);
    let resources = Arc::downgrade(engine.resources());
    let advanced = AtomicBool::new(false);
    let observed = clock.clone();
    let registry = Arc::downgrade(engine.received());
    let (failed, failure) = mpsc::channel();
    hook(&engine, move |event| {
        if event == Event::WriteSubmitted(Step::Corpus)
            && resources
                .upgrade()
                .unwrap()
                .repair_status(1)
                .unwrap()
                .completed_at
                .is_some()
            && !advanced.swap(true, Ordering::SeqCst)
        {
            observed.0.store(20_000, Ordering::SeqCst);
        }
        if event == Event::Failure(Lane::Corpus) {
            let failure = registry
                .upgrade()
                .unwrap()
                .failure_for(Lane::Corpus)
                .unwrap();
            if matches!(failure.error, StoreError::Internal { .. }) {
                let _ = failed.send(failure);
            }
        }
    });
    start(&engine);
    let failure = failure.recv_timeout(Duration::from_secs(300)).unwrap();
    engine.received().stop();
    block_on(engine.shutdown()).unwrap();
    assert_eq!(failure.attempts, 1);
    assert_eq!(failure.retry_at.unwrap().as_millis(), 21_000);
}

#[test]
fn repeated_corpus_publication_failure_does_not_reset_backoff_on_a_successful_begin() {
    for messages in [1, 401] {
        let (_scratch, engine, clock) = seed(messages, SIMPLE);
        write(&engine, move |tx| {
            tx.execute_batch(&format!("CREATE TEMP TRIGGER repair_fault AFTER INSERT ON main.d_message_bindings WHEN NEW.message_id={messages} BEGIN SELECT RAISE(ABORT,'fixture'); END")).unwrap();
            Ok(())
        });
        start(&engine);
        lane_parked(&engine, Lane::Corpus, 1);
        clock.0.store(11_000, Ordering::SeqCst);
        wait(|| {
            engine
                .received()
                .failure_for(Lane::Corpus)
                .is_some_and(|failure| failure.retry_at.unwrap().as_millis() > 11_000)
                && engine.received().waiting()
        });
        let failure = engine.received().failure_for(Lane::Corpus).unwrap();
        engine.received().stop();
        block_on(engine.shutdown()).unwrap();
        assert_eq!(failure.attempts, 2);
        assert_eq!(failure.retry_at.unwrap().as_millis(), 13_000);
    }
}

#[test]
fn captured_source_pass_revisits_old_and_new_ids_after_a_dropped_accepted_confirm() {
    let (scratch, engine, _) = seed(1, SIMPLE);
    import(
        &engine,
        &scratch.path().join("old.eml"),
        b"From: old@x.test\r\n\r\nold",
    );
    wait(|| {
        !engine
            .resources()
            .read(sb_resources::thread_revisions)
            .unwrap()
            .stale()
    });
    let passes = Arc::new(Mutex::new(Vec::new()));
    let recorded = passes.clone();
    let first = AtomicBool::new(true);
    let (ready, reached) = mpsc::channel();
    let (release, released) = mpsc::channel();
    let released = Mutex::new(released);
    hook(&engine, move |event| {
        if let Event::SourcePass { inputs, cutoff } = event {
            recorded.lock().unwrap().push((inputs, cutoff));
            if first.swap(false, Ordering::SeqCst) {
                ready.send(()).unwrap();
                released
                    .lock()
                    .unwrap()
                    .recv_timeout(Duration::from_secs(300))
                    .unwrap();
            }
        }
    });
    start(&engine);
    reached.recv_timeout(Duration::from_secs(300)).unwrap();
    let path = scratch.path().join("late.eml");
    std::fs::write(&path, b"From: late@x.test\r\n\r\nlate").unwrap();
    let started = block_on(engine.start_import(&engine.new_idempotency_key(), vec![path])).unwrap();
    wait(|| {
        block_on(engine.import_job(started.job_id()))
            .unwrap()
            .state()
            == ImportJobState::AwaitingConfirmation
    });
    let (unhold, writer) = hold_writer(&engine);
    let key = engine.new_idempotency_key();
    let before = engine.accepted_commands();
    {
        let mut pending = pin!(engine.confirm_import(&key, started.job_id(), vec![]));
        wait(|| {
            assert!(
                pending
                    .as_mut()
                    .poll(&mut Context::from_waker(Waker::noop()))
                    .is_pending()
            );
            engine.accepted_commands() > before
        });
    }
    unhold.send(()).unwrap();
    block_on(writer).unwrap();
    wait(|| {
        block_on(engine.import_job(started.job_id()))
            .unwrap()
            .state()
            == ImportJobState::Completed
    });
    change(
        &engine,
        "UPDATE d_message_summary SET parser_version=1 WHERE acquisition_id IN(2,3)",
    );
    release.send(()).unwrap();
    complete(&engine);
    assert_eq!(count(&engine, BINDINGS), 3);
    assert_eq!(count(&engine, LEDGER), 3);
    assert_eq!(
        count(
            &engine,
            "SELECT count(*) FROM d_repair_assemblies WHERE completed_at IS NOT NULL"
        ),
        3
    );
    let passes = passes.lock().unwrap();
    assert_eq!(passes[0].1, 2);
    assert!(
        passes
            .iter()
            .skip(1)
            .any(|&(revision, cutoff)| revision > passes[0].0 && cutoff == 3)
    );
    drop(passes);
    block_on(engine.shutdown()).unwrap();
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Boundary {
    Before,
    Submitted,
}

#[test]
fn shutdown_and_drop_stop_all_six_eager_sites_and_drain_actual_submitted_writes() {
    for end in [End::Shutdown, End::Drop] {
        for step in [
            Step::Population,
            Step::BeginCorpus,
            Step::Corpus,
            Step::BeginRepair,
            Step::Repair,
            Step::Diagnosis,
        ] {
            for boundary in [Boundary::Before, Boundary::Submitted] {
                let (scratch, engine, _) = seed(
                    1,
                    if step == Step::Diagnosis {
                        PARTIAL
                    } else {
                        SIMPLE
                    },
                );
                let root = scratch.path().join("store");
                let resources = Arc::downgrade(engine.resources());
                let before = Arc::new(Mutex::new(None));
                let after = Arc::new(Mutex::new(None));
                let captured = before.clone();
                let committed = after.clone();
                let first = AtomicBool::new(true);
                let (prepared, preparing) = mpsc::channel();
                let (permit, permitted) = mpsc::channel();
                let permitted = Mutex::new(permitted);
                let (accepted, accepting) = mpsc::channel();
                let (release, released) = mpsc::channel();
                let released = Mutex::new(released);
                hook(&engine, move |event| {
                    if event == Event::BeforeWrite(step) && first.swap(false, Ordering::SeqCst) {
                        *captured.lock().unwrap() = Some(image(&resources.upgrade().unwrap()));
                        prepared.send(()).unwrap();
                        permitted
                            .lock()
                            .unwrap()
                            .recv_timeout(Duration::from_secs(300))
                            .unwrap();
                    }
                    if event == Event::WriteSubmitted(step) && boundary == Boundary::Submitted {
                        accepted.send(()).unwrap();
                        released
                            .lock()
                            .unwrap()
                            .recv_timeout(Duration::from_secs(300))
                            .unwrap();
                    }
                    if event == Event::WriteCommitted(step) {
                        *committed.lock().unwrap() = Some(image(&resources.upgrade().unwrap()));
                    }
                });
                start(&engine);
                preparing.recv_timeout(Duration::from_secs(300)).unwrap();
                let writer = if boundary == Boundary::Submitted {
                    let held = hold_writer(&engine);
                    permit.send(()).unwrap();
                    accepting.recv_timeout(Duration::from_secs(300)).unwrap();
                    Some(held)
                } else {
                    None
                };
                let registry = engine.received().clone();
                let owned = root.clone();
                let closing = std::thread::spawn(move || close_and_reopen(engine, &owned, end));
                wait(|| registry.stopped());
                if boundary == Boundary::Before {
                    permit.send(()).unwrap();
                } else {
                    release.send(()).unwrap();
                }
                if let Some((release, pending)) = writer {
                    release.send(()).unwrap();
                    block_on(pending).unwrap();
                }
                let resources = closing.join().unwrap();
                let expected = if boundary == Boundary::Before {
                    before.lock().unwrap().clone()
                } else {
                    after.lock().unwrap().clone()
                }
                .unwrap();
                assert_eq!(image(&resources), expected);
                resources.close().unwrap();
                let engine = block_on(Engine::open(&root)).unwrap();
                complete(&engine);
                assert_eq!(
                    count(&engine, LEDGER),
                    if step == Step::Diagnosis { 0 } else { 1 }
                );
                assert_eq!(
                    count(
                        &engine,
                        "SELECT times_received+times_sent_to FROM seen_addresses WHERE canonical='kept@x.test'"
                    ),
                    18
                );
                block_on(engine.shutdown()).unwrap();
            }
        }
    }
}

#[test]
fn index_writer_failure_does_not_gate_real_repair_activation_or_address_history() {
    let (_scratch, engine, _) = seed(3, SIMPLE);
    write(&engine, |tx| {
        tx.execute_batch("CREATE TEMP TRIGGER reject_fts BEFORE INSERT ON main.d_search_docs BEGIN SELECT RAISE(ABORT,'fixture'); END").unwrap();
        sb_resources::set_user_pause(tx, UserPause::Resume)
    });
    engine.indexer().wake();
    wait(|| engine.indexer().status(engine.resources()).is_err());
    start(&engine);
    complete(&engine);
    assert!(engine.resources().catchup_status().unwrap().complete());
    assert_eq!(count(&engine, BINDINGS), 3);
    assert_eq!(count(&engine, LEDGER), 3);
    assert!(block_on(engine.coverage(crate::SearchScope::All)).is_err());
    let kept = history(engine.resources());
    write(&engine, |tx| {
        tx.execute_batch("DROP TRIGGER reject_fts").unwrap();
        Ok(())
    });
    engine.indexer().wake();
    wait(|| count(&engine, "SELECT count(*) FROM d_search_docs") == 1);
    assert_eq!(history(engine.resources()), kept);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_source_task_panic_after_corpus_ack_reopens_without_losing_transient_report_work() {
    let (scratch, engine, _) = seed(1, SIMPLE);
    hook(&engine, |event| {
        if event == Event::RepairPrepared {
            panic!("fixture source panic");
        }
    });
    start(&engine);
    wait(|| {
        engine
            .received()
            .failure_for(Lane::Population)
            .is_some_and(|failure| matches!(failure.error, StoreError::Internal { .. }))
    });
    assert_eq!(count(&engine, "SELECT count(*) FROM d_repair_pieces"), 0);
    assert_eq!(count(&engine, BINDINGS), 0);
    assert_eq!(count(&engine, LEDGER), 0);
    let status = engine.resources().catchup_status().unwrap();
    assert!(status.caught_up() && status.unready == 1);
    block_on(engine.shutdown()).unwrap();
    let engine = block_on(Engine::open(&scratch.path().join("store"))).unwrap();
    complete(&engine);
    assert_eq!(count(&engine, BINDINGS), 1);
    assert_eq!(count(&engine, LEDGER), 1);
    block_on(engine.shutdown()).unwrap();
}

const CUT: &str = "SB_RECEIVED_CUT";
const CUT_ROOT: &str = "SB_RECEIVED_ROOT";

#[test]
#[ignore = "spawned by the received worker process-crash suite"]
fn worker_crash_child() {
    use sb_resources::Fs;
    let nth = std::env::var(CUT).unwrap().parse().unwrap();
    let root = std::path::PathBuf::from(std::env::var_os(CUT_ROOT).unwrap());
    let name = root.file_name().unwrap().to_str().unwrap();
    let step = if name.starts_with("staging") {
        Step::Repair
    } else if name.starts_with("activation") {
        Step::Corpus
    } else {
        Step::Population
    };
    let engine = block_on(Engine::open(&root)).unwrap();
    let cuts = Arc::new(sb_test_support::CutFs::new(real_fs(), nth));
    let observed = cuts.clone();
    let directory = root.clone();
    let resources = Arc::downgrade(engine.resources());
    let before = AtomicBool::new(false);
    let after = AtomicBool::new(false);
    hook(&engine, move |event| {
        let resources = resources.upgrade().unwrap();
        let (bound, ledger, published) = resources.read(|tx| Ok(tx.query_row("SELECT (SELECT count(*) FROM d_message_bindings),(SELECT count(*) FROM seen_address_messages),(SELECT count(*) FROM d_repair_assemblies WHERE completed_at IS NOT NULL)", [], |row| Ok((row.get::<_, i64>(0)?,row.get::<_, i64>(1)?,row.get::<_, i64>(2)?))).unwrap())).unwrap();
        let ready = match step {
            Step::Repair => true,
            Step::Corpus => published > 0,
            _ => bound > 0 && ledger == 0,
        };
        if event == Event::BeforeWrite(step) && ready && !before.swap(true, Ordering::SeqCst) {
            observed.metadata(&directory).unwrap();
        }
        let committed = match (step, event) {
            (Step::Corpus, Event::CatchupCommitted { activated, .. }) => activated > 0,
            (Step::Population, Event::WriteCommitted(Step::Population)) => ledger > 0,
            (Step::Repair, Event::WriteCommitted(Step::Repair)) => true,
            _ => false,
        };
        if before.load(Ordering::SeqCst) && committed && !after.swap(true, Ordering::SeqCst) {
            observed.metadata(&directory).unwrap();
        }
    });
    start(&engine);
    complete(&engine);
    block_on(engine.shutdown()).unwrap();
    assert_eq!(cuts.operations(), 2);
}

#[test]
fn actual_process_crashes_at_staging_activation_and_ledger_replay_without_recounting() {
    let scratch = ScratchDir::new("received-worker-crashes");
    let large = format!(
        "From: new@x.test\r\nSubject: new\r\nTo: {}\r\n\r\nbody",
        "é,".repeat(40_000)
    );
    for stage in ["staging", "activation", "ledger"] {
        let killed = sb_test_support::run_crash_cuts(
            CUT,
            CUT_ROOT,
            "received_tests::repair::worker_crash_child",
            &mut |n| {
                let root = scratch.path().join(format!("{stage}-{n}"));
                let (engine, _) = seed_at(
                    &root,
                    3,
                    if stage == "staging" {
                        large.as_bytes()
                    } else {
                        SIMPLE
                    },
                );
                block_on(engine.shutdown()).unwrap();
                root
            },
            &mut |root| {
                let resources = Resources::open(root).unwrap();
                resources.read(|tx| {
                assert_eq!(tx.query_row("SELECT count(*) FROM d_repair_pieces p LEFT JOIN d_repair_assemblies a USING(acquisition_id,parser_target) WHERE a.acquisition_id IS NULL", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
                assert_eq!(tx.query_row("SELECT count(*) FROM d_list_rows r LEFT JOIN d_message_bindings b USING(message_id) WHERE (b.message_id IS NULL AND r.subject IS NOT 'OLD') OR (b.message_id IS NOT NULL AND (r.subject IS NOT 'new' OR r.sender IS NOT 'new@x.test' OR r.snippet IS NOT 'body'))", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
                assert_eq!(tx.query_row("SELECT times_received+times_sent_to FROM seen_addresses WHERE canonical='kept@x.test'", [], |row| row.get::<_, i64>(0)).unwrap(), 18);
                assert_eq!(tx.query_row("SELECT coalesce((SELECT times_received FROM seen_addresses WHERE canonical='new@x.test'),0)-(SELECT count(*) FROM seen_address_messages WHERE canonical='new@x.test')", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
                Ok(())
            }).unwrap();
                if stage == "staging" && root.file_name().unwrap().to_str().unwrap().ends_with("-2")
                {
                    assert!(resources.repaired_summary(1).unwrap().is_none());
                    assert!(resources.repair_status(1).unwrap().pieces >= 16);
                }
                resources.close().unwrap();
                let engine = block_on(Engine::open(root)).unwrap();
                complete(&engine);
                assert!(engine.resources().catchup_status().unwrap().complete());
                assert_eq!(count(&engine, BINDINGS), 3);
                assert_eq!(count(&engine, LEDGER), 3);
                block_on(engine.shutdown()).unwrap();
            },
        );
        assert_eq!(killed, 2, "{stage}");
    }
}

#[test]
fn real_worker_repairs_activates_and_populates_one_hundred_thousand_shared_messages() {
    let started = std::time::Instant::now();
    let (scratch, engine, _) = seed(MESSAGES, SIMPLE);
    assert_eq!(
        count(
            &engine,
            "SELECT count(DISTINCT message_id) FROM logical_messages"
        ),
        MESSAGES
    );
    assert_eq!(count(&engine, "SELECT count(*) FROM acquisitions"), 1);
    let activated = Arc::new(AtomicU64::new(0));
    let observed = activated.clone();
    let resources = Arc::downgrade(engine.resources());
    hook(&engine, move |event| {
        if let Event::CatchupCommitted {
            processed,
            activated,
            bytes,
        } = event
        {
            assert!(processed <= 200 && bytes <= 2_000_000);
            let total =
                observed.fetch_add(u64::from(activated), Ordering::SeqCst) + u64::from(activated);
            let resources = resources.upgrade().unwrap();
            resources.read(|tx| {
                assert_eq!(tx.query_row(BINDINGS, [], |row| row.get::<_, i64>(0)).unwrap(), i64::try_from(total).unwrap());
                assert_eq!(tx.query_row("SELECT count(*) FROM d_list_rows r LEFT JOIN d_message_bindings b USING(message_id) WHERE (b.message_id IS NULL AND (r.subject IS NOT 'OLD' OR r.sender IS NOT 'old@x.test' OR r.snippet IS NOT 'old')) OR (b.message_id IS NOT NULL AND (r.subject IS NOT 'new' OR r.sender IS NOT 'new@x.test' OR r.snippet IS NOT 'body'))", [], |row| row.get::<_, i64>(0)).unwrap(), 0);
                assert_eq!(tx.query_row("SELECT times_received+times_sent_to FROM seen_addresses WHERE canonical='kept@x.test'", [], |row| row.get::<_, i64>(0)).unwrap(), 18);
                Ok(())
            }).unwrap();
        }
    });
    start(&engine);
    for milestone in (1000..=MESSAGES).step_by(1000) {
        wait(|| activated.load(Ordering::SeqCst) >= milestone as u64);
        eprintln!("worker activated {milestone}: {:?}", started.elapsed());
    }
    complete(&engine);
    assert!(engine.resources().catchup_status().unwrap().complete());
    assert_eq!(count(&engine, LEDGER), MESSAGES);
    assert_eq!(
        count(
            &engine,
            "SELECT times_received FROM seen_addresses WHERE canonical='new@x.test'"
        ),
        MESSAGES
    );
    assert_eq!(
        count(
            &engine,
            "SELECT count(*) FROM d_repair_assemblies WHERE completed_at IS NOT NULL"
        ),
        1
    );
    assert_eq!(
        count(&engine, "SELECT parser_version FROM d_message_summary"),
        1
    );
    assert_eq!(count(&engine, "SELECT count(*) FROM d_search_docs"), 0);
    wait(|| {
        !engine
            .resources()
            .read(sb_resources::thread_revisions)
            .unwrap()
            .stale()
    });
    let kept = history(engine.resources());
    let root = scratch.path().join("store");
    block_on(engine.shutdown()).unwrap();
    let engine = block_on(Engine::open(&root)).unwrap();
    complete(&engine);
    assert_eq!(history(engine.resources()), kept);
    assert_eq!(count(&engine, LEDGER), MESSAGES);
    block_on(engine.shutdown()).unwrap();
}
