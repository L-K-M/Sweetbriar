//! Real retained MIME -> publisher -> activation -> all five runtime consumers.

use std::collections::BTreeSet;

use super::*;

mod boundaries;
mod corpus;
use crate::{
    ActivationCommit, ActivationPreparation, ActivationPrepared, ActivationWork, ReceivedCommit,
};

fn refresh(resources: &Resources) {
    write(resources, |tx| {
        let ids = tx
            .prepare("SELECT occurrence_id FROM occurrences")
            .unwrap()
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<BTreeSet<i64>, _>>()
            .unwrap();
        sb_store::refresh_list_rows(tx, &ids)?;
        Ok(())
    });
}

fn seed(resources: &Resources, message: i64, count: i64) {
    write(resources, move |tx| {
        let account: String = tx
            .query_row(
                "SELECT account_id FROM accounts WHERE provider='local_archive'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        tx.execute("INSERT INTO mailboxes(mailbox_id,account_id,remote_name,kind,selectable,sync_state) VALUES(?1,?2,?3,'folder',1,'synced')", (message, &account, format!("box{message}"))).unwrap();
        tx.execute("INSERT INTO logical_messages(message_id,account_id,acquisition_id,created_at) VALUES(?1,?2,1,0)", (message, &account)).unwrap();
        for _ in 0..count {
            tx.execute("INSERT INTO occurrences(account_id,mailbox_id,message_id,state,first_seen_at,last_seen_at) VALUES(?1,?2,?2,'present',10,10)", (&account, message)).unwrap();
        }
        Ok(())
    });
    refresh(resources);
}

fn published(bytes: &[u8], count: i64) -> (ScratchDir, Resources) {
    let (scratch, resources, _) = native(bytes, real_fs());
    seed(&resources, 1, count);
    finish(&resources, prepare(&resources));
    (scratch, resources)
}

fn begin(resources: &Resources, message: i64) -> ActivationWork {
    block_on(resources.begin_activation(message))
        .unwrap()
        .unwrap()
}

fn prepared(resources: &Resources, message: i64) -> (ActivationPrepared, usize) {
    let mut work = begin(resources, message);
    for steps in 1..10_000 {
        work = match resources.prepare_activation(work, &|| false).unwrap() {
            ActivationPreparation::Continue(next) => next,
            ActivationPreparation::Prepared(value) => return (value, steps),
            other => panic!("unexpected preparation: {other:?}"),
        };
    }
    panic!("activation failed to make bounded progress")
}

fn activate(resources: &Resources, message: i64) -> ActivationCommit {
    block_on(resources.commit_activation(prepared(resources, message).0)).unwrap()
}

fn drain_population(resources: &Resources) {
    let Some(mut sweep) = resources.begin_received_sweep().unwrap() else {
        return;
    };
    for _ in 0..10_000 {
        let prepared = resources.prepare_received_step(sweep).unwrap();
        sweep = match block_on(resources.commit_received_step(prepared)).unwrap() {
            ReceivedCommit::Continue(next) | ReceivedCommit::Recapture(next) => next,
            ReceivedCommit::Complete => return,
        };
    }
    panic!("received population failed to converge")
}

fn generations(resources: &Resources) -> [i64; 4] {
    [
        "list_generation",
        "search_generation",
        "thread_inputs",
        "seen_address_inputs",
    ]
    .map(|key| {
        number(
            resources,
            &format!("SELECT CAST(value AS INTEGER) FROM store_meta WHERE key='{key}'"),
        )
    })
}

fn reader(resources: &Resources, message: i64) -> crate::ReaderHeaders {
    resources
        .read(|tx| crate::get_message(tx, message).map(|value| value.headers))
        .unwrap()
}

fn query(text: &str) -> crate::Query {
    crate::Query::parse_json(&format!(
        r#"{{"v":1,"q":{{"op":"is","field":"from","value":"{text}"}}}}"#
    ))
    .unwrap()
}

fn state(resources: &Resources) -> String {
    resources.read(|tx| Ok(tx.query_row("SELECT json_array(
        (SELECT json_group_array(json_array(occurrence_id,message_id,sender,subject,snippet,sort_date,has_attachments,representative)) FROM (SELECT * FROM d_list_rows ORDER BY occurrence_id)),
        (SELECT json_group_array(json_array(message_id,acquisition_id,parser_target,bound_at)) FROM (SELECT * FROM d_message_bindings ORDER BY message_id)),
        (SELECT json_group_array(json_array(message_id,hex(witness),recorded_cost,recorded_at)) FROM (SELECT * FROM d_activation_deferrals ORDER BY message_id)))", [], |row| row.get(0)).unwrap())).unwrap()
}

#[test]
fn real_publication_activates_all_consumers_and_keeps_history_once() {
    let (scratch, resources) = published(RICH, 3);
    let expected = expected(RICH);
    let before = generations(&resources);
    let history = number(
        &resources,
        "SELECT times_received+times_sent_to FROM seen_addresses WHERE canonical='kept@x.test'",
    );
    assert_eq!(reader(&resources, 1).subject.as_deref(), Some("OLD"));
    assert_eq!(activate(&resources, 1), ActivationCommit::Activated);
    assert_eq!(generations(&resources), before.map(|value| value + 1));
    assert_eq!(reader(&resources, 1).subject, expected.subject);
    assert_eq!(reader(&resources, 1).from[0].address, expected.from_addr);
    assert_eq!(
        number(
            &resources,
            "SELECT count(*) FROM d_list_rows WHERE subject='café' AND sender='Sender' AND has_attachments=1"
        ),
        3
    );
    assert_eq!(
        number(
            &resources,
            "SELECT count(*) FROM d_list_rows WHERE representative=1"
        ),
        1
    );
    let threads = resources
        .read(|tx| crate::thread_sources(tx, 0, 10))
        .unwrap();
    assert_eq!(threads[0].subject, expected.subject);
    assert_eq!(threads[0].header_id, expected.header_message_id);
    let q = query("User@example.test");
    let counts = resources
        .read(|tx| sb_store::query::counts(tx, &q, crate::SearchScope::All))
        .unwrap();
    assert_eq!(counts.matching_messages(), 1);
    assert_eq!(counts.excluded_unknown(), 0);
    drain_population(&resources);
    assert_eq!(
        number(
            &resources,
            "SELECT times_received FROM seen_addresses WHERE canonical='User@example.test'"
        ),
        1
    );
    assert_eq!(
        number(
            &resources,
            "SELECT times_received+times_sent_to FROM seen_addresses WHERE canonical='kept@x.test'"
        ),
        history
    );
    let after = state(&resources);
    let changes = writer_changes(&resources);
    assert!(block_on(resources.begin_activation(1)).unwrap().is_none());
    assert_eq!(state(&resources), after);
    assert_eq!(writer_changes(&resources), changes);
    resources.close().unwrap();
    let resources = Resources::open(&scratch.path().join("store")).unwrap();
    assert_eq!(reader(&resources, 1).subject, expected.subject);
    assert_eq!(state(&resources), after);
    resources.close().unwrap();
}

#[test]
fn own_budget_defers_four_thousand_rows_but_sharing_neighbor_activates() {
    let (_scratch, resources) = published(RICH, 4000);
    seed(&resources, 2, 1);
    let before = generations(&resources);
    assert_eq!(activate(&resources, 1), ActivationCommit::Deferred);
    assert_eq!(generations(&resources), before);
    assert_eq!(reader(&resources, 1).subject.as_deref(), Some("OLD"));
    assert_eq!(
        number(&resources, "SELECT count(*) FROM d_message_bindings"),
        0
    );
    let status = resources
        .activation_status(1)
        .unwrap()
        .recorded_deferral
        .unwrap();
    assert!(status.recorded_cost > 2_000_000);
    assert!(status.count > 0 && status.count < 4000);
    let changes = writer_changes(&resources);
    let recorded = state(&resources);
    assert_eq!(activate(&resources, 1), ActivationCommit::Deferred);
    assert_eq!(writer_changes(&resources), changes);
    assert_eq!(state(&resources), recorded);
    assert_eq!(activate(&resources, 2), ActivationCommit::Activated);
    assert_eq!(reader(&resources, 2).subject.as_deref(), Some("café"));
    assert_eq!(
        number(
            &resources,
            "SELECT count(*) FROM d_list_rows WHERE message_id=1 AND subject='OLD'"
        ),
        4000
    );
    edit(
        &resources,
        "INSERT INTO message_headers(message_id,raw_fields,subject,from_json,fetched_at) VALUES(1,x'','MASK','[{\"name\":\"X\",\"address\":null}]',0)",
    );
    refresh(&resources);
    assert_eq!(activate(&resources, 1), ActivationCommit::Deferred);
    assert!(
        resources
            .activation_status(1)
            .unwrap()
            .recorded_deferral
            .unwrap()
            .count
            > status.count,
        "shrunk row costs require a fresh, larger live witness"
    );
    edit(
        &resources,
        "UPDATE occurrences SET message_id=2 WHERE occurrence_id=1",
    );
    refresh(&resources);
    assert_eq!(activate(&resources, 1), ActivationCommit::Deferred);
    assert_eq!(reader(&resources, 2).subject.as_deref(), Some("café"));
    edit(
        &resources,
        "UPDATE occurrences SET state='vanished' WHERE message_id=1 AND occurrence_id>2",
    );
    refresh(&resources);
    assert_eq!(activate(&resources, 1), ActivationCommit::Activated);
    assert!(
        resources
            .activation_status(1)
            .unwrap()
            .recorded_deferral
            .is_none()
    );
    assert_eq!(
        number(&resources, "SELECT count(*) FROM d_activation_deferrals"),
        1
    );
    assert_eq!(reader(&resources, 1).subject.as_deref(), Some("café"));
    resources.close().unwrap();
}

#[test]
fn statement_and_deferred_commit_failures_rollback_rows_binding_and_announcements() {
    for deferred in [false, true] {
        let (_scratch, resources) = published(RICH, 3);
        let calls = Arc::new(AtomicUsize::new(0));
        let list = calls.clone();
        resources.observe_list_generation(Arc::new(move |_| {
            list.fetch_add(1, Ordering::SeqCst);
        }));
        let search = calls.clone();
        resources.observe_search_generation(Arc::new(move |_| {
            search.fetch_add(1, Ordering::SeqCst);
        }));
        let pending = prepared(&resources, 1).0;
        if deferred {
            edit(
                &resources,
                "CREATE TEMP TABLE activation_parent(id INTEGER PRIMARY KEY); CREATE TEMP TABLE activation_probe(id INTEGER REFERENCES activation_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE TEMP TRIGGER activation_cut AFTER INSERT ON main.d_message_bindings BEGIN INSERT INTO activation_probe VALUES(1); END",
            );
        } else {
            edit(
                &resources,
                "CREATE TEMP TRIGGER activation_cut AFTER UPDATE ON main.d_list_rows WHEN NEW.occurrence_id=2 BEGIN SELECT RAISE(ABORT,'activation statement cut'); END",
            );
        }
        let before = (state(&resources), snapshot(&resources));
        assert!(block_on(resources.commit_activation(pending)).is_err());
        assert_eq!((state(&resources), snapshot(&resources)), before);
        assert_eq!(calls.load(Ordering::SeqCst), 0);
        edit(&resources, "DROP TRIGGER activation_cut");
        assert_eq!(activate(&resources, 1), ActivationCommit::Activated);
        assert_eq!(calls.load(Ordering::SeqCst), 2);
        resources.close().unwrap();
    }
}

#[test]
fn unrelated_writes_continue_but_source_cache_mailbox_and_root_changes_recapture() {
    let edits = [
        "UPDATE occurrences SET f_seen=1 WHERE occurrence_id=1",
        "UPDATE message_headers SET subject='new' WHERE message_id=1",
        "UPDATE d_message_summary SET subject='changed' WHERE acquisition_id=1",
        "UPDATE acquisitions SET object_state='missing' WHERE acquisition_id=1",
        "UPDATE d_list_rows SET subject='changed' WHERE occurrence_id=1",
        "UPDATE mailboxes SET kind='label' WHERE mailbox_id=1",
        "UPDATE store_meta SET value='changed' WHERE key='incarnation'",
    ];
    for sql in edits {
        let (_scratch, resources) = published(RICH, 2);
        edit(
            &resources,
            "INSERT INTO message_headers(message_id,raw_fields,fetched_at) VALUES(1,x'',0)",
        );
        let pending = prepared(&resources, 1).0;
        edit(&resources, sql);
        let before = state(&resources);
        assert_eq!(
            block_on(resources.commit_activation(pending)).unwrap(),
            ActivationCommit::Recapture,
            "{sql}"
        );
        assert_eq!(state(&resources), before);
        resources.close().unwrap();
    }
    let (_scratch, resources) = published(RICH, 2);
    let pending = prepared(&resources, 1).0;
    edit(
        &resources,
        "UPDATE store_meta SET value=CAST(CAST(value AS INTEGER)+1 AS TEXT) WHERE key='search_generation'; UPDATE mailboxes SET remote_name='unrelated' WHERE mailbox_id=1",
    );
    assert_eq!(
        block_on(resources.commit_activation(pending)).unwrap(),
        ActivationCommit::Activated
    );
    resources.close().unwrap();
}

#[test]
fn cache_already_candidate_wrong_representative_and_foreign_extra_rows_are_corrupt() {
    for sql in [
        "UPDATE d_list_rows SET subject='café' WHERE occurrence_id=1",
        "UPDATE d_list_rows SET representative=0; UPDATE d_list_rows SET representative=1 WHERE occurrence_id=2",
        "UPDATE d_list_rows SET message_id=1,representative=0 WHERE occurrence_id=3",
    ] {
        let (_scratch, resources) = published(RICH, 2);
        seed(&resources, 2, 1);
        edit(&resources, sql);
        let mut work = begin(&resources, 1);
        let before = state(&resources);
        loop {
            work = match resources.prepare_activation(work, &|| false) {
                Ok(ActivationPreparation::Continue(next)) => next,
                Err(StoreError::Corrupt { .. }) => break,
                other => panic!("{sql}: expected corrupt CURRENT, got {other:?}"),
            };
        }
        assert_eq!(state(&resources), before);
        resources.close().unwrap();
    }
}

#[test]
fn eager_unpolled_commit_survives_drop_and_close_and_wrong_store_recaptures() {
    for explicit in [false, true] {
        let (scratch, resources) = published(RICH, 2);
        let pending = prepared(&resources, 1).0;
        let other_work = begin(&resources, 1);
        let (_other_scratch, other) = published(RICH, 1);
        assert!(matches!(
            other.prepare_activation(other_work, &|| false).unwrap(),
            ActivationPreparation::Recapture
        ));
        other.close().unwrap();
        drop(resources.commit_activation(pending));
        if explicit {
            resources.close().unwrap();
        } else {
            drop(resources);
        }
        let resources = Resources::open(&scratch.path().join("store")).unwrap();
        assert_eq!(reader(&resources, 1).subject.as_deref(), Some("café"));
        assert!(block_on(resources.begin_activation(1)).unwrap().is_none());
        resources.close().unwrap();
    }
}

#[test]
fn repaired_null_empty_and_header_precedence_keep_the_five_distinct_rules() {
    let (_scratch, resources) = published(b"Subject:\r\n\r\n", 1);
    edit(
        &resources,
        "INSERT INTO message_headers(message_id,raw_fields,subject,from_json,header_message_id,references_json,date_header,fetched_at) VALUES(1,x'','','[{\"name\":\"HEADER SENDER\",\"address\":\"header@x.test\"}]','<header@x.test>','[]',17,0)",
    );
    refresh(&resources);
    let before = generations(&resources);
    assert_eq!(activate(&resources, 1), ActivationCommit::Activated);
    assert_eq!(
        generations(&resources),
        [before[0], before[1] + 1, before[2], before[3] + 1]
    );
    let headers = reader(&resources, 1);
    assert_eq!(headers.subject, None);
    assert!(headers.from.is_empty());
    assert!(headers.to.is_empty() && headers.cc.is_empty());
    assert_eq!(headers.message_id, None);
    assert_eq!(
        number(
            &resources,
            "SELECT count(*) FROM d_list_rows WHERE subject='' AND sender='HEADER SENDER' AND sort_date=17 AND has_attachments=0"
        ),
        1
    );
    let thread = resources
        .read(|tx| crate::thread_sources(tx, 0, 1))
        .unwrap()
        .remove(0);
    assert_eq!(thread.subject.as_deref(), Some(""));
    assert_eq!(thread.header_id.as_deref(), Some("<header@x.test>"));
    assert!(thread.references.is_empty());
    let count = resources
        .read(|tx| sb_store::query::counts(tx, &query("old@x.test"), crate::SearchScope::All))
        .unwrap();
    assert_eq!(
        (count.matching_messages(), count.excluded_unknown()),
        (0, 0)
    );
    drain_population(&resources);
    assert_eq!(number(&resources, "SELECT count(*) FROM seen_addresses"), 1);
    assert_eq!(
        number(&resources, "SELECT count(*) FROM seen_address_messages"),
        0
    );
    resources.close().unwrap();
}

#[test]
fn absent_summary_falls_back_but_repaired_absence_never_falls_through_to_base() {
    let (_scratch, resources, _) = native(b"\r\nbody", real_fs());
    seed(&resources, 1, 1);
    edit(
        &resources,
        "DELETE FROM d_message_summary; INSERT INTO message_headers(message_id,raw_fields,subject,from_json,fetched_at) VALUES(1,x'','HEADER','[{\"address\":\"header@x.test\",\"name\":null}]',0)",
    );
    refresh(&resources);
    assert_eq!(reader(&resources, 1).subject.as_deref(), Some("HEADER"));
    finish(&resources, prepare(&resources));
    assert_eq!(activate(&resources, 1), ActivationCommit::Activated);
    assert_eq!(reader(&resources, 1).subject, None);
    assert!(reader(&resources, 1).from.is_empty());
    // A later first BASE summary must leave this bound message on its repaired source.
    edit(
        &resources,
        "INSERT INTO d_message_summary(acquisition_id,parser_version,parse_state,subject,from_addr,has_attachments) VALUES(1,1,'partial','LATE BASE','late@x.test',0)",
    );
    refresh(&resources);
    assert_eq!(reader(&resources, 1).subject, None);
    assert!(reader(&resources, 1).from.is_empty());
    resources.close().unwrap();
}

#[test]
fn corrupt_bound_source_is_typed_even_under_masked_false_unknown_and_ineligible_reads() {
    for bound_target in [1, 2] {
        let (_scratch, resources) = published(RICH, 1);
        edit(
            &resources,
            "INSERT INTO message_headers(message_id,raw_fields,subject,from_json,to_json,cc_json,fetched_at) VALUES(1,x'','MASK','[]','[]','[]',0)",
        );
        refresh(&resources);
        if bound_target == 1 {
            boundaries::bind_older(&resources);
        } else {
            assert_eq!(activate(&resources, 1), ActivationCommit::Activated);
        }
        edit(
            &resources,
            "UPDATE acquisitions SET byte_length=byte_length+1 WHERE acquisition_id=1",
        );
        assert!(matches!(
            block_on(resources.begin_activation(1)),
            Err(StoreError::Corrupt { .. })
        ));
        assert!(matches!(
            resources.activation_status(1),
            Err(StoreError::Corrupt { .. })
        ));
        assert!(matches!(
            resources.read(|tx| crate::get_message(tx, 1)),
            Err(StoreError::Corrupt { .. })
        ));
        assert!(matches!(
            resources.read(|tx| crate::thread_sources(tx, 0, 1)),
            Err(StoreError::Corrupt { .. })
        ));
        assert!(matches!(
            resources.prepare_received_step(resources.begin_received_sweep().unwrap().unwrap()),
            Err(StoreError::Corrupt { .. })
        ));
        for node in [
            r#"{"and":[]}"#,
            r#"{"not":{"and":[]}}"#,
            r#"{"field":"from","op":"is","value":"absent@x.test"}"#,
            r#"{"field":"body","op":"contains","value":"missing"}"#,
        ] {
            let query = crate::Query::parse_json(&format!(r#"{{"v":1,"q":{node}}}"#)).unwrap();
            let scope = crate::SearchScope::All;
            assert!(
                matches!(
                    resources.read(|tx| sb_store::query::counts(tx, &query, scope.clone())),
                    Err(StoreError::Corrupt { .. })
                ),
                "{node}"
            );
            assert!(matches!(
                resources.read(|tx| crate::read_search_coverage(tx, scope.clone())),
                Err(StoreError::Corrupt { .. })
            ));
            assert!(
                matches!(
                    resources.read(|tx| crate::search_date_ids(tx, &query, scope.clone(), None)),
                    Err(StoreError::Corrupt { .. })
                ),
                "date: {node}"
            );
            assert!(
                matches!(
                    resources.read(|tx| crate::search_relevance_ids(
                        tx,
                        &query,
                        scope.clone(),
                        None
                    )),
                    Err(StoreError::Corrupt { .. })
                ),
                "relevance: {node}"
            );
            assert!(matches!(
                resources.read(|tx| crate::read_search_date_page(
                    tx,
                    &query,
                    scope.clone(),
                    crate::SearchAnchor::Top,
                    10,
                    None
                )),
                Err(StoreError::Corrupt { .. })
            ));
            assert!(matches!(
                resources.read(|tx| crate::read_search_relevance_page(
                    tx,
                    &query,
                    scope,
                    crate::SearchAnchor::Top,
                    10,
                    None
                )),
                Err(StoreError::Corrupt { .. })
            ));
        }
        resources.close().unwrap();
    }
}

#[test]
fn raw_paging_includes_vanished_rows_and_cancellation_has_no_durable_effects() {
    let (_scratch, resources) = published(RICH, 501);
    edit(
        &resources,
        "UPDATE occurrences SET state='vanished'; UPDATE occurrences SET first_seen_at=-17 WHERE occurrence_id=501",
    );
    refresh(&resources);
    let before = (state(&resources), snapshot(&resources));
    assert!(matches!(
        resources
            .prepare_activation(begin(&resources, 1), &|| true)
            .unwrap(),
        ActivationPreparation::Cancelled
    ));
    assert_eq!((state(&resources), snapshot(&resources)), before);
    let (pending, steps) = prepared(&resources, 1);
    assert_eq!(
        steps, 5,
        "envelope + three raw pages with observed EOF + cache EOF"
    );
    assert_eq!(
        block_on(resources.commit_activation(pending)).unwrap(),
        ActivationCommit::Activated
    );
    assert_eq!(number(&resources, "SELECT count(*) FROM d_list_rows"), 0);
    drain_population(&resources);
    assert_eq!(
        number(
            &resources,
            "SELECT last_seen_at FROM seen_addresses WHERE canonical='User@example.test'"
        ),
        -17
    );
    resources.close().unwrap();
}

#[test]
fn eligible_without_observation_is_corrupt_and_unready_or_current_inputs_are_quiet() {
    let (_scratch, resources, _) = native(RICH, real_fs());
    seed(&resources, 1, 0);
    assert!(block_on(resources.begin_activation(1)).unwrap().is_none());
    finish(&resources, prepare(&resources));
    let before = state(&resources);
    assert!(matches!(
        block_on(resources.begin_activation(1)),
        Err(StoreError::Corrupt { .. })
    ));
    assert_eq!(state(&resources), before);
    edit(
        &resources,
        "UPDATE d_message_summary SET parser_version=2,parse_state='ok'",
    );
    assert!(block_on(resources.begin_activation(1)).unwrap().is_none());
    edit(
        &resources,
        "UPDATE d_message_summary SET parser_version=3,parse_state='failed'; UPDATE acquisitions SET object_state='missing'",
    );
    assert!(block_on(resources.begin_activation(1)).unwrap().is_none());
    edit(
        &resources,
        "UPDATE store_meta SET value='3' WHERE key='mime_repair_target'",
    );
    assert!(matches!(
        block_on(resources.begin_activation(1)),
        Err(StoreError::Unavailable { .. })
    ));
    resources.close().unwrap();
}

#[test]
fn concurrent_preparations_return_current_and_old_runtime_work_cannot_cross_reopen() {
    let (scratch, resources) = published(RICH, 1);
    let first = prepared(&resources, 1).0;
    let second = prepared(&resources, 1).0;
    let old = begin(&resources, 1);
    assert_eq!(
        block_on(resources.commit_activation(first)).unwrap(),
        ActivationCommit::Activated
    );
    let changes = writer_changes(&resources);
    assert_eq!(
        block_on(resources.commit_activation(second)).unwrap(),
        ActivationCommit::Current
    );
    assert_eq!(writer_changes(&resources), changes);
    resources.close().unwrap();
    let resources = Resources::open(&scratch.path().join("store")).unwrap();
    assert!(matches!(
        resources.prepare_activation(old, &|| false).unwrap(),
        ActivationPreparation::Recapture
    ));
    resources.close().unwrap();
}
