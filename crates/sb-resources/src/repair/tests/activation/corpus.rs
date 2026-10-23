use super::*;
use crate::{CatchupBatch, CatchupCommit, CatchupPreparation, CatchupReport, CatchupSweep};

const MESSAGE_COUNT: i64 = 100_000;
const SIMPLE: &[u8] = b"From: new@x.test\r\nSubject: new\r\n\r\nbody";

fn corpus(count: i64) -> (ScratchDir, Resources) {
    let (scratch, resources, _) = native(SIMPLE, real_fs());
    // Synthetic legacy inventory; every candidate comes from the actual retained MIME producer.
    write(&resources, move |tx| {
        tx.execute("INSERT INTO mailboxes(mailbox_id,account_id,remote_name,kind,selectable,sync_state) SELECT 1,account_id,'corpus','folder',1,'synced' FROM accounts WHERE provider='local_archive'", []).unwrap();
        tx.execute("WITH RECURSIVE n(id) AS (VALUES(1) UNION ALL SELECT id+1 FROM n WHERE id<?1) INSERT INTO logical_messages(message_id,account_id,acquisition_id,created_at) SELECT id,account_id,1,0 FROM n CROSS JOIN accounts WHERE provider='local_archive'", [count]).unwrap();
        tx.execute_batch("INSERT INTO occurrences(occurrence_id,account_id,mailbox_id,message_id,state,first_seen_at,last_seen_at) SELECT message_id,account_id,1,message_id,'present',10,10 FROM logical_messages;
            INSERT INTO d_list_rows SELECT occurrence_id,mailbox_id,NULL,account_id,10,message_id,'old@x.test','OLD',NULL,1,0,0,1 FROM occurrences").unwrap();
        Ok(())
    });
    (scratch, resources)
}

fn batch(resources: &Resources, mut scan: CatchupSweep) -> CatchupBatch {
    for _ in 0..1_000_000 {
        scan = match resources.prepare_catchup_step(scan, &|| false).unwrap() {
            CatchupPreparation::Continue(next) => next,
            CatchupPreparation::Prepared(batch) => return batch,
            other => panic!("unexpected preparation: {other:?}"),
        };
    }
    panic!("catch-up made no finite progress")
}

fn run(resources: &Resources) -> Vec<CatchupReport> {
    let mut reports = Vec::new();
    let Some(mut scan) = block_on(resources.begin_catchup()).unwrap() else {
        return reports;
    };
    for _ in 0..10_000 {
        match block_on(resources.commit_catchup_batch(batch(resources, scan))).unwrap() {
            CatchupCommit::Continue(next, report) => {
                reports.push(report);
                scan = next;
            }
            CatchupCommit::Complete(report) => {
                reports.push(report);
                return reports;
            }
            CatchupCommit::Recapture => {
                scan = block_on(resources.begin_catchup()).unwrap().unwrap()
            }
        }
    }
    panic!("catch-up failed to exhaust finite input")
}

#[test]
fn one_hundred_thousand_distinct_messages_converge_through_the_real_facade() {
    let started = std::time::Instant::now();
    let (_scratch, resources) = corpus(MESSAGE_COUNT);
    eprintln!("100k legacy fixture: {:?}", started.elapsed());
    finish(&resources, prepare(&resources));
    eprintln!("100k shared publication: {:?}", started.elapsed());
    assert_eq!(
        number(
            &resources,
            "SELECT count(DISTINCT message_id) FROM logical_messages"
        ),
        MESSAGE_COUNT
    );
    assert_eq!(number(&resources, "SELECT count(*) FROM acquisitions"), 1);
    let inputs = resources.catchup_status().unwrap().inputs;
    let mut scan = block_on(resources.begin_catchup()).unwrap().unwrap();
    let mut activated = 0i64;
    let mut commits = 0;
    loop {
        let before = generations(&resources);
        let outcome = block_on(resources.commit_catchup_batch(batch(&resources, scan))).unwrap();
        let (next, report) = match outcome {
            CatchupCommit::Continue(next, report) => (Some(next), report),
            CatchupCommit::Complete(report) => (None, report),
            other => panic!("stable corpus recaptured: {other:?}"),
        };
        assert!(report.processed <= 200 && report.charged_bytes <= 2_000_000);
        assert_eq!((report.deferred, report.unready), (0, 0));
        activated += i64::from(report.activated);
        if activated % 1_000 == 0 {
            eprintln!("activated {activated}: {:?}", started.elapsed());
        }
        assert_eq!(
            number(&resources, "SELECT count(*) FROM d_message_bindings"),
            activated
        );
        assert_eq!(
            number(
                &resources,
                "SELECT count(*) FROM d_list_rows r LEFT JOIN d_message_bindings b USING(message_id) WHERE (b.message_id IS NULL AND r.subject IS NOT 'OLD') OR (b.message_id IS NOT NULL AND (r.subject IS NOT 'new' OR r.sender IS NOT 'new@x.test' OR r.snippet IS NOT 'body'))"
            ),
            0
        );
        let after = generations(&resources);
        assert_eq!(
            after,
            if report.activated > 0 {
                before.map(|n| n + 1)
            } else {
                before
            }
        );
        assert_eq!(
            resources.catchup_status().unwrap().inputs,
            inputs,
            "own writes self-dirtied"
        );
        commits += 1;
        assert!(commits < 2000);
        let Some(next) = next else {
            break;
        };
        scan = next;
    }
    assert_eq!(activated, MESSAGE_COUNT);
    assert!(resources.catchup_status().unwrap().complete());
    assert!(block_on(resources.begin_catchup()).unwrap().is_none());
    assert_eq!(
        number(
            &resources,
            "SELECT times_received+times_sent_to FROM seen_addresses WHERE canonical='kept@x.test'"
        ),
        18
    );
    assert_eq!(
        number(&resources, "SELECT count(*) FROM seen_address_messages"),
        0
    );
    resources.close().unwrap();
}

#[test]
fn discovery_exposes_unready_sources_and_publication_leaves_new_inputs_owed() {
    let (_scratch, resources) = corpus(3);
    let reports = run(&resources);
    assert_eq!(reports.iter().map(|r| r.unready).sum::<u32>(), 3);
    assert_eq!(
        reports
            .iter()
            .flat_map(|r| r.repair_needed.iter())
            .copied()
            .collect::<BTreeSet<_>>(),
        BTreeSet::from([1])
    );
    let status = resources.catchup_status().unwrap();
    assert!(status.caught_up() && !status.complete());
    assert_eq!(status.unready, 3);
    assert!(block_on(resources.begin_catchup()).unwrap().is_none());
    finish(&resources, prepare(&resources));
    assert!(!resources.catchup_status().unwrap().caught_up());
    assert_eq!(run(&resources).iter().map(|r| r.activated).sum::<u32>(), 3);
    assert!(resources.catchup_status().unwrap().complete());
    resources.close().unwrap();
}

#[test]
fn captured_revision_ack_keeps_changed_old_ids_and_late_ids_owed() {
    let (_scratch, resources) = corpus(220);
    edit(
        &resources,
        "UPDATE logical_messages SET acquisition_id=NULL WHERE message_id=1",
    );
    refresh(&resources);
    finish(&resources, prepare(&resources));
    let captured = resources.catchup_status().unwrap().inputs;
    let scan = block_on(resources.begin_catchup()).unwrap().unwrap();
    let CatchupCommit::Continue(mut scan, report) =
        block_on(resources.commit_catchup_batch(batch(&resources, scan))).unwrap()
    else {
        panic!("first page was not bounded");
    };
    assert_eq!(report.processed, 200);
    edit(&resources, "UPDATE logical_messages SET acquisition_id=1 WHERE message_id=1;
        INSERT INTO logical_messages(message_id,account_id,acquisition_id,created_at) SELECT 221,account_id,1,0 FROM logical_messages WHERE message_id=1;
        INSERT INTO occurrences(occurrence_id,account_id,mailbox_id,message_id,state,first_seen_at,last_seen_at) SELECT 221,account_id,1,221,'present',10,10 FROM logical_messages WHERE message_id=221");
    refresh(&resources);
    loop {
        scan = match block_on(resources.commit_catchup_batch(batch(&resources, scan))).unwrap() {
            CatchupCommit::Continue(next, _) => next,
            CatchupCommit::Complete(_) => break,
            other => panic!("unrelated changes prevented captured pass completion: {other:?}"),
        };
    }
    let status = resources.catchup_status().unwrap();
    assert_eq!(status.swept, captured);
    assert!(status.recorded() && !status.caught_up());
    assert_eq!(
        number(
            &resources,
            "SELECT count(*) FROM d_message_bindings WHERE message_id IN(1,221)"
        ),
        0
    );
    assert_eq!(run(&resources).iter().map(|r| r.activated).sum::<u32>(), 2);
    assert!(resources.catchup_status().unwrap().complete());
    resources.close().unwrap();
}

#[test]
fn writer_statement_and_commit_failures_return_no_continuation_or_false_events() {
    for deferred in [false, true] {
        let (_scratch, resources) = corpus(3);
        finish(&resources, prepare(&resources));
        let scan = block_on(resources.begin_catchup()).unwrap().unwrap();
        let prepared = batch(&resources, scan);
        if deferred {
            edit(
                &resources,
                "CREATE TEMP TABLE late_parent(id INTEGER PRIMARY KEY); CREATE TEMP TABLE late_child(id INTEGER REFERENCES late_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE TEMP TRIGGER corpus_cut AFTER INSERT ON main.d_message_bindings BEGIN INSERT INTO late_child VALUES(1); END",
            );
        } else {
            edit(
                &resources,
                "CREATE TEMP TRIGGER corpus_cut AFTER UPDATE ON main.d_list_rows WHEN NEW.occurrence_id=2 BEGIN SELECT RAISE(ABORT,'corpus cut'); END",
            );
        }
        let before = (state(&resources), snapshot(&resources));
        let revisions = resources.catchup_status().unwrap();
        let calls = Arc::new(AtomicUsize::new(0));
        let observer = calls.clone();
        resources.observe_list_generation(Arc::new(move |_| {
            observer.fetch_add(1, Ordering::SeqCst);
        }));
        assert!(block_on(resources.commit_catchup_batch(prepared)).is_err());
        assert_eq!((state(&resources), snapshot(&resources)), before);
        assert_eq!(resources.catchup_status().unwrap().swept, revisions.swept);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        edit(&resources, "DROP TRIGGER corpus_cut");
        assert_eq!(run(&resources).iter().map(|r| r.activated).sum::<u32>(), 3);
        resources.close().unwrap();
    }
}

#[test]
fn actual_noops_and_failed_jobs_do_not_dirty_but_source_changes_do() {
    let (_scratch, resources) = corpus(1);
    let before = resources.catchup_status().unwrap().inputs;
    edit(
        &resources,
        "UPDATE d_message_summary SET subject=subject; UPDATE occurrences SET f_seen=f_seen",
    );
    assert_eq!(resources.catchup_status().unwrap().inputs, before);
    assert!(
        block_on(resources.write(WritePriority::User, |tx| {
            tx.execute("UPDATE d_message_summary SET subject='failed'", [])
                .unwrap();
            Err::<(), _>(StoreError::Internal {
                detail: "rollback".into(),
            })
        }))
        .is_err()
    );
    assert_eq!(resources.catchup_status().unwrap().inputs, before);
    edit(
        &resources,
        "UPDATE d_message_summary SET subject='actual'; UPDATE occurrences SET f_seen=1",
    );
    assert_eq!(resources.catchup_status().unwrap().inputs, before + 1);
    edit(
        &resources,
        "UPDATE store_meta SET value='bad' WHERE key='repair_inputs'",
    );
    assert!(matches!(
        resources.catchup_status(),
        Err(StoreError::Corrupt { .. })
    ));
    assert!(
        block_on(resources.write(WritePriority::User, |tx| {
            tx.execute("UPDATE d_message_summary SET subject='must rollback'", [])
                .unwrap();
            Ok(())
        }))
        .is_err()
    );
    assert_eq!(reader(&resources, 1).subject.as_deref(), Some("actual"));
    resources.close().unwrap();
}

#[test]
fn eager_drop_close_cancel_and_wrong_owner_preserve_replay() {
    let (scratch, resources) = corpus(3);
    finish(&resources, prepare(&resources));
    let cancelled = block_on(resources.begin_catchup()).unwrap().unwrap();
    assert!(matches!(
        resources.prepare_catchup_step(cancelled, &|| true).unwrap(),
        CatchupPreparation::Cancelled
    ));
    let wrong = block_on(resources.begin_catchup()).unwrap().unwrap();
    let (_other_scratch, other) = corpus(1);
    assert!(matches!(
        other.prepare_catchup_step(wrong, &|| false).unwrap(),
        CatchupPreparation::Recapture
    ));
    other.close().unwrap();
    let scan = block_on(resources.begin_catchup()).unwrap().unwrap();
    drop(resources.commit_catchup_batch(batch(&resources, scan)));
    resources.close().unwrap();
    let resources = Resources::open(&scratch.path().join("store")).unwrap();
    assert!(resources.catchup_status().unwrap().complete());
    assert_eq!(
        number(&resources, "SELECT count(*) FROM d_message_bindings"),
        3
    );
    resources.close().unwrap();
}

fn fanout(resources: &Resources, message: i64, copies: i64) {
    write(resources, move |tx| {
        tx.execute("WITH RECURSIVE n(id) AS (VALUES(1) UNION ALL SELECT id+1 FROM n WHERE id<?2) INSERT INTO occurrences(account_id,mailbox_id,message_id,state,first_seen_at,last_seen_at) SELECT account_id,1,message_id,'present',10,10 FROM logical_messages CROSS JOIN n WHERE message_id=?1", (message, copies-1)).unwrap();
        tx.execute("INSERT INTO d_list_rows SELECT occurrence_id,mailbox_id,NULL,account_id,10,message_id,'old@x.test','OLD',NULL,1,0,0,0 FROM occurrences WHERE message_id=?1 AND occurrence_id NOT IN(SELECT occurrence_id FROM d_list_rows)", [message]).unwrap();
        Ok(())
    });
}

#[test]
fn fitting_messages_carry_without_becoming_own_budget_deferrals() {
    let (_scratch, resources) = corpus(2);
    fanout(&resources, 1, 1500);
    fanout(&resources, 2, 1500);
    finish(&resources, prepare(&resources));
    let scan = block_on(resources.begin_catchup()).unwrap().unwrap();
    let CatchupCommit::Continue(scan, first) =
        block_on(resources.commit_catchup_batch(batch(&resources, scan))).unwrap()
    else {
        panic!("second message wasn't carried");
    };
    assert_eq!((first.activated, first.deferred), (1, 0));
    assert_eq!(
        number(&resources, "SELECT count(*) FROM d_activation_deferrals"),
        0
    );
    let CatchupCommit::Complete(second) =
        block_on(resources.commit_catchup_batch(batch(&resources, scan))).unwrap()
    else {
        panic!("carried message failed to progress");
    };
    assert_eq!((second.activated, second.deferred), (1, 0));
    assert!(first.charged_bytes <= 2_000_000 && second.charged_bytes <= 2_000_000);
    assert!(resources.catchup_status().unwrap().complete());
    resources.close().unwrap();
}

#[test]
fn full_allowance_item_commits_before_a_separate_eof_acknowledgement() {
    // Independent row-image oracle: 640 framing + 3 copies of 30 text bytes.
    const ROW_CHARGE: i64 = 730;
    let copies = (2_000_000 - 4096) / ROW_CHARGE;
    let (_scratch, resources) = corpus(1);
    fanout(&resources, 1, copies);
    finish(&resources, prepare(&resources));
    let scan = block_on(resources.begin_catchup()).unwrap().unwrap();
    let CatchupCommit::Continue(scan, first) =
        block_on(resources.commit_catchup_batch(batch(&resources, scan))).unwrap()
    else {
        panic!("ACK should require its own bounded turn");
    };
    assert_eq!((first.activated, first.deferred), (1, 0));
    assert!(first.charged_bytes > 2_000_000 - 4096 && first.charged_bytes <= 2_000_000);
    assert!(!resources.catchup_status().unwrap().caught_up());
    assert_eq!(
        number(
            &resources,
            "SELECT count(*) FROM d_list_rows WHERE subject='new'"
        ),
        copies
    );
    let CatchupCommit::Complete(last) =
        block_on(resources.commit_catchup_batch(batch(&resources, scan))).unwrap()
    else {
        panic!("metadata-only turn failed");
    };
    assert_eq!(
        (last.processed, last.activated, last.charged_bytes),
        (0, 0, 4096)
    );
    assert!(resources.catchup_status().unwrap().complete());
    resources.close().unwrap();
}

#[test]
fn caught_up_deferrals_stay_incomplete_and_reconsider_changed_live_inputs() {
    let (_scratch, resources) = corpus(2);
    fanout(&resources, 1, 4000);
    finish(&resources, prepare(&resources));
    let reports = run(&resources);
    assert_eq!(reports.iter().map(|r| r.activated).sum::<u32>(), 1);
    assert_eq!(reports.iter().map(|r| r.deferred).sum::<u32>(), 1);
    let status = resources.catchup_status().unwrap();
    assert!(status.caught_up() && !status.complete());
    assert_eq!((status.deferred, status.unready), (1, 0));
    let record = state(&resources);
    edit(&resources, "UPDATE occurrences SET f_seen=f_seen");
    assert!(block_on(resources.begin_catchup()).unwrap().is_none());
    assert_eq!(state(&resources), record);
    edit(
        &resources,
        "UPDATE occurrences SET state='vanished' WHERE message_id=1 AND occurrence_id>2",
    );
    refresh(&resources);
    assert_eq!(run(&resources).iter().map(|r| r.activated).sum::<u32>(), 1);
    assert!(resources.catchup_status().unwrap().complete());
    assert_eq!(
        number(&resources, "SELECT count(*) FROM d_activation_deferrals"),
        1,
        "obsolete evidence is retained, not counted as live deferral"
    );
    resources.close().unwrap();
}

#[test]
fn unavailable_input_is_unready_but_corrupt_cache_fails_without_acknowledgement() {
    let (_scratch, resources) = corpus(1);
    finish(&resources, prepare(&resources));
    edit(&resources, "UPDATE acquisitions SET object_state='missing'");
    assert_eq!(run(&resources).iter().map(|r| r.unready).sum::<u32>(), 1);
    assert!(!resources.catchup_status().unwrap().complete());
    assert_eq!(
        number(&resources, "SELECT count(*) FROM d_message_bindings"),
        0
    );
    edit(
        &resources,
        "UPDATE acquisitions SET object_state='published'; UPDATE d_list_rows SET subject='corrupt'",
    );
    let before = resources.catchup_status().unwrap().swept;
    let mut scan = block_on(resources.begin_catchup()).unwrap().unwrap();
    loop {
        scan = match resources.prepare_catchup_step(scan, &|| false) {
            Ok(CatchupPreparation::Continue(next)) => next,
            Err(StoreError::Corrupt { .. }) => break,
            other => panic!("corruption was skipped: {other:?}"),
        };
    }
    assert_eq!(resources.catchup_status().unwrap().swept, before);
    assert!(!resources.catchup_status().unwrap().caught_up());
    resources.close().unwrap();
}

#[test]
fn every_counter_rejects_malformed_state_and_exhaustion_rolls_back_source_writes() {
    for key in [
        "repair_inputs",
        "repair_swept",
        "repair_deferred",
        "repair_unready",
    ] {
        for value in ["bad", "18446744073709551615"] {
            let (_scratch, resources) = corpus(1);
            write(&resources, move |tx| {
                tx.execute("UPDATE store_meta SET value=?2 WHERE key=?1", (key, value))
                    .unwrap();
                Ok(())
            });
            assert!(
                matches!(resources.catchup_status(), Err(StoreError::Corrupt { .. })),
                "{key}={value}"
            );
            assert!(matches!(
                block_on(resources.begin_catchup()),
                Err(StoreError::Corrupt { .. })
            ));
            resources.close().unwrap();
        }
    }
    let (_scratch, resources) = corpus(1);
    edit(
        &resources,
        "UPDATE store_meta SET value='9223372036854775807' WHERE key='repair_inputs'",
    );
    assert!(matches!(
        block_on(resources.write(WritePriority::User, |tx| {
            tx.execute("UPDATE d_message_summary SET subject='lost'", [])
                .unwrap();
            Ok(())
        })),
        Err(StoreError::Corrupt { .. })
    ));
    assert_eq!(reader(&resources, 1).subject.as_deref(), Some("OLD"));
    resources.close().unwrap();
}

#[test]
fn actual_terminal_diagnosis_does_not_schedule_another_corpus_pass() {
    let (_scratch, resources, _) = native(b"", real_fs());
    seed(&resources, 1, 1);
    let before = resources.catchup_status().unwrap().inputs;
    let attempt = terminal(&resources);
    assert_eq!(
        block_on(resources.persist_repair_terminal(attempt)).unwrap(),
        RepairPersist::Diagnosed
    );
    assert_eq!(
        number(&resources, "SELECT count(*) FROM d_repair_diagnoses"),
        1
    );
    assert_eq!(resources.catchup_status().unwrap().inputs, before);
    assert!(block_on(resources.begin_repair(1)).unwrap().is_none());
    assert_eq!(resources.catchup_status().unwrap().inputs, before);
    resources.close().unwrap();
}
