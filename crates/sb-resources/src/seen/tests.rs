use super::*;
use sb_store::imports::{ImportParseState, ImportSummary};
use sb_test_support::{ScratchDir, block_on};
use serde_json::json;

fn native(label: &str) -> (ScratchDir, Resources) {
    let scratch = ScratchDir::new(label);
    let resources = Resources::open(scratch.path()).unwrap();
    (scratch, resources)
}

fn write<T: Send + 'static>(
    resources: &Resources,
    action: impl FnOnce(&crate::Transaction) -> Result<T, StoreError> + Send + 'static,
) -> T {
    block_on(resources.write(WritePriority::User, action)).unwrap()
}

fn summary(message: &str) -> ImportSummary {
    let outcome = sb_mime::parse(message.as_bytes(), &sb_mime::Limits::default());
    assert!(
        matches!(outcome, sb_mime::ParseOutcome::Parsed(_)),
        "real fixture must be completely admitted: {:?}",
        outcome.warnings()
    );
    crate::acquire::summary(&outcome)
}

/// Only input/source rows are seeded; every history/ledger result comes from the facade.
fn put(resources: &Resources, id: i64, acquisition: i64, at: i64, summary: ImportSummary) {
    write(resources, move |tx| {
        let account: String = tx
            .query_row(
                "SELECT account_id FROM accounts WHERE provider='local_archive'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        tx.execute("INSERT INTO acquisitions(acquisition_id,content_sha256,byte_length,object_state,first_acquired_at) VALUES(?1,?2,0,'published',0) ON CONFLICT(acquisition_id) DO NOTHING", (acquisition, format!("{acquisition:064x}"))).unwrap();
        tx.execute("INSERT INTO mailboxes(mailbox_id,account_id,remote_name,kind,selectable,sync_state) VALUES(?1,?2,?3,'folder',1,'synced')", (id, &account, format!("box{id}"))).unwrap();
        tx.execute("INSERT INTO logical_messages(message_id,account_id,acquisition_id,created_at) VALUES(?1,?2,?3,999999)", (id, &account, acquisition)).unwrap();
        tx.execute("INSERT INTO occurrences(occurrence_id,account_id,mailbox_id,message_id,state,first_seen_at,last_seen_at) VALUES(?1,?2,?1,?1,'present',?3,?3)", (id, account, at)).unwrap();
        let state = match summary.parse_state {
            ImportParseState::Parsed => "ok",
            ImportParseState::Partial => "partial",
            ImportParseState::Failed => "failed",
        };
        tx.execute("INSERT INTO d_message_summary(acquisition_id,parser_version,parse_state,from_addr,from_name,reply_to_json,to_json,cc_json,date_header,has_attachments) VALUES(?1,?2,?3,?4,?5,?6,?7,?8,9000000,?9) ON CONFLICT(acquisition_id) DO NOTHING",
            (acquisition, summary.parser_version, state, summary.from_addr, summary.from_name, summary.reply_to_json, summary.to_json, summary.cc_json, summary.has_attachments)).unwrap();
        tx.execute("UPDATE store_meta SET value=CAST(CAST(value AS INTEGER)+1 AS TEXT) WHERE key='seen_address_inputs'", []).unwrap();
        Ok(())
    });
}

fn number(resources: &Resources, sql: &str) -> i64 {
    resources
        .read(|tx| Ok(tx.query_row(sql, [], |row| row.get(0)).unwrap()))
        .unwrap()
}

fn drain(resources: &Resources) -> usize {
    let Some(mut sweep) = resources.begin_received_sweep().unwrap() else {
        return 0;
    };
    let mut commits = 0;
    loop {
        let before = number(resources, "SELECT count(*) FROM seen_address_messages");
        let prepared = resources.prepare_received_step(sweep).unwrap();
        let result = block_on(resources.commit_received_step(prepared)).unwrap();
        assert!(number(resources, "SELECT count(*) FROM seen_address_messages") - before <= 200);
        commits += 1;
        assert!(commits < 10_000, "finite input must converge");
        sweep = match result {
            ReceivedCommit::Continue(next) | ReceivedCommit::Recapture(next) => next,
            ReceivedCommit::Complete => return commits,
        };
    }
}

type Row = (String, Option<String>, i64, i64, i64, Option<String>);
fn address(resources: &Resources, canonical: &str) -> Row {
    resources.read(|tx| Ok(tx.query_row("SELECT email,display_name,last_seen_at,times_received,times_sent_to,last_account_id FROM seen_addresses WHERE canonical=?1", [canonical], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?))).unwrap())).unwrap()
}

#[test]
fn facade_counts_real_header_keys_per_message_and_preserves_history_on_replay() {
    let (_scratch, resources) = native("received-identity");
    let parsed = summary(
        "From: User@EXAMPLE.net\r\nReply-To: User@example.net\r\nTo: user@example.net, \"User\"@example.net, \"a\\\"b\"@EXAMPLE.net, =?UTF-8?Q?Name?= <é@EXAMPLE.net>, e\u{301}@EXAMPLE.net, user@ＥＸＡＭＰＬＥ.net\r\nCc: bad-address, User@example.net\r\nBcc: hidden@example.net\r\n\r\nbody",
    );
    put(&resources, 1, 1, 10, parsed.clone());
    write(&resources, |tx| {
        tx.execute("UPDATE mailboxes SET role='sent' WHERE mailbox_id=1", [])
            .unwrap();
        tx.execute("INSERT INTO identities(identity_id,account_id,email,display_name,is_default,sendable) SELECT '11111111-1111-4111-8111-111111111111',account_id,'User@example.net','owner',1,'validated' FROM logical_messages WHERE message_id=1", []).unwrap();
        tx.execute("INSERT INTO mailboxes(mailbox_id,account_id,remote_name,kind,role,selectable,sync_state) SELECT 30,account_id,'label','label','sent',1,'synced' FROM logical_messages WHERE message_id=1", []).unwrap();
        tx.execute("INSERT INTO occurrences(occurrence_id,account_id,mailbox_id,message_id,state,first_seen_at,last_seen_at) SELECT 30,account_id,30,1,'vanished',5,5 FROM logical_messages WHERE message_id=1", []).unwrap();
        tx.execute("INSERT INTO seen_addresses(canonical,email,display_name,last_seen_at,times_sent_to,times_received) VALUES('User@example.net','User@EXAMPLE.NET','history',0,7,11)", []).unwrap();
        Ok(())
    });
    let generations = resources
        .read(|tx| Ok((crate::list_generation(tx)?, crate::search_generation(tx)?)))
        .unwrap();
    drain(&resources);
    assert_eq!(
        address(&resources, "User@example.net").2,
        5,
        "earliest even vanished occurrence supplies observation, never clock/Date"
    );
    let unstored = ImportSummary {
        parser_version: 2,
        parse_state: ImportParseState::Parsed,
        from_addr: Some("discarded@example.test".into()),
        ..ImportSummary::default()
    };
    put(&resources, 2, 1, 20, unstored);
    drain(&resources);
    assert_eq!(number(&resources, "SELECT count(*) FROM seen_addresses"), 7);
    assert_eq!(
        number(&resources, "SELECT count(*) FROM seen_address_messages"),
        14
    );
    let row = address(&resources, "User@example.net");
    assert_eq!(
        (row.0.as_str(), row.2, row.3, row.4),
        ("User@example.net", 20, 13, 7)
    );
    assert_eq!(address(&resources, "user@example.net").3, 2);
    assert_eq!(address(&resources, "\"User\"@example.net").3, 2);
    assert_eq!(address(&resources, "\"a\\\"b\"@example.net").3, 2);
    assert_eq!(address(&resources, "e\u{301}@example.net").3, 2);
    assert_eq!(address(&resources, "user@ｅｘａｍｐｌｅ.net").3, 2);
    assert_eq!(
        row.1, None,
        "missing names remain NULL; no role/self filtering"
    );
    assert_eq!(
        address(&resources, "é@example.net").1.as_deref(),
        Some("Name")
    );
    assert_eq!(
        resources
            .read(|tx| Ok((crate::list_generation(tx)?, crate::search_generation(tx)?)))
            .unwrap(),
        generations
    );
    assert!(resources.begin_received_sweep().unwrap().is_none());
    put(&resources, 3, 3, 30, ImportSummary::default());
    drain(&resources);
    assert_eq!(
        address(&resources, "User@example.net"),
        row,
        "replayed old keys preserve counts/winner/history"
    );
    let revisions = resources.received_address_revisions().unwrap();
    assert_eq!(revisions.inputs, revisions.swept);
}

#[test]
fn ineligible_sources_change_neither_counts_nor_metadata_and_late_old_ids_remain_owed() {
    let (_scratch, resources) = native("received-eligibility");
    let parsed = summary("From: kept@EXAMPLE.test\n\nbody");
    for (id, state, version) in [
        (1, ImportParseState::Parsed, 1),
        (2, ImportParseState::Partial, 2),
        (3, ImportParseState::Failed, 2),
    ] {
        let mut value = parsed.clone();
        value.parse_state = state;
        value.parser_version = version;
        put(&resources, id, id, 1000, value);
    }
    write(&resources, |tx| {
        tx.execute("INSERT INTO seen_addresses(canonical,email,display_name,last_seen_at,times_sent_to) VALUES('kept@example.test','kept@example.test','history',0,5)", []).unwrap();
        Ok(())
    });
    let history = address(&resources, "kept@example.test");
    drain(&resources);
    assert_eq!(address(&resources, "kept@example.test"), history);
    assert_eq!(
        number(&resources, "SELECT count(*) FROM seen_address_messages"),
        0
    );
    let scan = resources.begin_received_sweep().unwrap();
    assert!(scan.is_none());
    write(&resources, |tx| {
        tx.execute(
            "UPDATE d_message_summary SET parser_version=2,from_name='A' WHERE acquisition_id=1",
            [],
        )
        .unwrap();
        tx.execute("UPDATE store_meta SET value=CAST(CAST(value AS INTEGER)+1 AS TEXT) WHERE key='seen_address_inputs'", []).unwrap();
        Ok(())
    });
    let captured = resources.received_address_revisions().unwrap().inputs;
    let mut scan = resources.begin_received_sweep().unwrap().unwrap();
    let step = resources.prepare_received_step(scan).unwrap();
    scan = match block_on(resources.commit_received_step(step)).unwrap() {
        ReceivedCommit::Continue(next) => next,
        other => panic!("{other:?}"),
    };
    put(&resources, 4, 4, 1000, parsed.clone());
    loop {
        let step = resources.prepare_received_step(scan).unwrap();
        scan = match block_on(resources.commit_received_step(step)).unwrap() {
            ReceivedCommit::Continue(next) | ReceivedCommit::Recapture(next) => next,
            ReceivedCommit::Complete => break,
        };
    }
    let revisions = resources.received_address_revisions().unwrap();
    assert_eq!(revisions.swept, captured);
    assert!(revisions.inputs > revisions.swept);
    drain(&resources);
    assert_eq!(address(&resources, "kept@example.test").3, 2);
    // Same ledger key, lexically greater eligible name; no new header appearance.
    write(&resources, |tx| {
        tx.execute(
            "UPDATE d_message_summary SET from_name='Z' WHERE acquisition_id=1",
            [],
        )
        .unwrap();
        tx.execute("UPDATE store_meta SET value=CAST(CAST(value AS INTEGER)+1 AS TEXT) WHERE key='seen_address_inputs'", []).unwrap();
        Ok(())
    });
    drain(&resources);
    assert_eq!(address(&resources, "kept@example.test").3, 2);
    assert_eq!(
        address(&resources, "kept@example.test").1.as_deref(),
        Some("Z")
    );
    write(&resources, |tx| {
        tx.execute_batch("INSERT INTO accounts(account_id,kind,provider,display_name,state,sort_order,created_at) VALUES('ffffffff-ffff-ffff-ffff-ffffffffffff','imap','generic_imap','other','active',1,0);
          INSERT INTO acquisitions(acquisition_id,content_sha256,byte_length,object_state,first_acquired_at) VALUES(5,'5555555555555555555555555555555555555555555555555555555555555555',0,'published',0);
          INSERT INTO d_message_summary(acquisition_id,parser_version,parse_state,from_addr,from_name,has_attachments) SELECT 5,parser_version,parse_state,from_addr,from_name,has_attachments FROM d_message_summary WHERE acquisition_id=1;
          INSERT INTO mailboxes(mailbox_id,account_id,remote_name,kind,selectable,sync_state) VALUES(5,'ffffffff-ffff-ffff-ffff-ffffffffffff','other','folder',1,'synced');
          INSERT INTO logical_messages(message_id,account_id,acquisition_id,created_at) VALUES(5,'ffffffff-ffff-ffff-ffff-ffffffffffff',5,9999);
          INSERT INTO occurrences(account_id,mailbox_id,message_id,state,first_seen_at,last_seen_at) VALUES('ffffffff-ffff-ffff-ffff-ffffffffffff',5,5,'present',1000,1000);
          UPDATE store_meta SET value=CAST(CAST(value AS INTEGER)+1 AS TEXT) WHERE key='seen_address_inputs';").unwrap();
        Ok(())
    });
    drain(&resources);
    assert_eq!(
        address(&resources, "kept@example.test").5.as_deref(),
        Some("ffffffff-ffff-ffff-ffff-ffffffffffff")
    );
    write(&resources, |tx| {
        tx.execute_batch("UPDATE occurrences SET first_seen_at=800 WHERE message_id=1; UPDATE d_message_summary SET from_name='ZZZ' WHERE acquisition_id=1;
          UPDATE store_meta SET value=CAST(CAST(value AS INTEGER)+1 AS TEXT) WHERE key='seen_address_inputs';").unwrap();
        Ok(())
    });
    drain(&resources);
    assert_eq!(address(&resources, "kept@example.test").3, 3);
    assert_eq!(address(&resources, "kept@example.test").2, 1000);
    assert_eq!(
        address(&resources, "kept@example.test").1.as_deref(),
        Some("Z")
    );
}

#[test]
fn recipient_expansion_and_maximum_admitted_key_progress_across_real_facade_commits() {
    let (_scratch, resources) = native("received-large");
    let recipients = (0..10_000)
        .map(|id| format!("u{id}@x.test"))
        .collect::<Vec<_>>()
        .join(",");
    let parsed = summary(&format!("To: {recipients}\r\n\r\nbody"));
    put(&resources, 1, 1, 1, parsed);
    assert!(drain(&resources) > 50);
    assert_eq!(
        number(&resources, "SELECT count(*) FROM seen_address_messages"),
        10_000
    );
    assert_eq!(
        number(&resources, "SELECT sum(times_received) FROM seen_addresses"),
        10_000
    );
    assert!(
        number(&resources, "SELECT count(*) FROM seen_address_messages") * 512
            > sb_store::seen::MAX_RECEIVED_BYTES as i64,
        "framing alone proves expansion beyond one transaction"
    );
    let suffix = "@EXAMPLE.test\r\n";
    let local =
        "A".repeat(sb_mime::MAX_HEADER_BYTES - "From: ".len() - suffix.len() - "\r\n".len());
    let parsed = summary(&format!("From: {local}{suffix}\r\nbody"));
    put(&resources, 2, 2, 2, parsed);
    drain(&resources);
    assert_eq!(address(&resources, &format!("{local}@example.test")).3, 1);
    assert_eq!(
        number(&resources, "SELECT count(*) FROM seen_address_messages"),
        10_001
    );
}

#[test]
fn source_staleness_and_target_capability_fail_without_effect_or_ack() {
    let (_scratch, resources) = native("received-fences");
    put(
        &resources,
        1,
        1,
        1,
        summary("From: first@example.test\n\nbody"),
    );
    let step = resources
        .prepare_received_step(resources.begin_received_sweep().unwrap().unwrap())
        .unwrap();
    write(&resources, |tx| {
        tx.execute(
            "UPDATE d_message_summary SET from_addr='second@example.test'",
            [],
        )
        .unwrap();
        Ok(())
    });
    assert!(matches!(
        block_on(resources.commit_received_step(step)).unwrap(),
        ReceivedCommit::Recapture(_)
    ));
    assert_eq!(number(&resources, "SELECT count(*) FROM seen_addresses"), 0);
    assert_eq!(resources.received_address_revisions().unwrap().swept, 0);
    for value in ["2", "0", "bad"] {
        let step = resources
            .prepare_received_step(resources.begin_received_sweep().unwrap().unwrap())
            .unwrap();
        let value = value.to_owned();
        write(&resources, move |tx| {
            tx.execute(
                "UPDATE store_meta SET value=?1 WHERE key='seen_address_target'",
                [value],
            )
            .unwrap();
            Ok(())
        });
        let begin = resources.begin_received_sweep().unwrap_err();
        let commit = block_on(resources.commit_received_step(step)).unwrap_err();
        if resources.received_address_revisions().is_ok() {
            assert!(
                matches!(begin, StoreError::Unavailable { .. })
                    && matches!(commit, StoreError::Unavailable { .. })
            );
        } else {
            assert!(
                matches!(begin, StoreError::Corrupt { .. })
                    && matches!(commit, StoreError::Corrupt { .. })
            );
        }
        assert_eq!(number(&resources, "SELECT count(*) FROM seen_addresses"), 0);
        write(&resources, |tx| {
            tx.execute(
                "UPDATE store_meta SET value='1' WHERE key='seen_address_target'",
                [],
            )
            .unwrap();
            Ok(())
        });
    }
    write(&resources, |tx| {
        tx.execute("DELETE FROM store_meta WHERE key='seen_address_target'", [])
            .unwrap();
        Ok(())
    });
    assert!(matches!(
        resources.begin_received_sweep(),
        Err(StoreError::Corrupt { .. })
    ));
}

#[test]
fn failed_partial_triple_and_failed_commit_return_no_continuation_or_history_changes() {
    for commit_failure in [false, true] {
        let (_scratch, resources) = native("received-rollback");
        put(
            &resources,
            1,
            1,
            1,
            summary("To: a@example.test,b@example.test\n\nbody"),
        );
        write(&resources, move |tx| {
            if commit_failure {
                tx.execute_batch("CREATE TEMP TABLE cut_parent(id INTEGER PRIMARY KEY); CREATE TEMP TABLE cut_probe(id INTEGER REFERENCES cut_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE TEMP TRIGGER received_cut AFTER INSERT ON main.seen_address_messages BEGIN INSERT INTO cut_probe VALUES(1); END;").unwrap();
            } else {
                tx.execute_batch("CREATE TEMP TRIGGER received_cut BEFORE INSERT ON main.seen_address_messages WHEN NEW.canonical='b@example.test' BEGIN SELECT RAISE(ABORT,'cut'); END;").unwrap();
            }
            Ok(())
        });
        let step = resources
            .prepare_received_step(resources.begin_received_sweep().unwrap().unwrap())
            .unwrap();
        assert!(block_on(resources.commit_received_step(step)).is_err());
        assert_eq!(
            number(&resources, "SELECT count(*) FROM seen_address_messages"),
            0
        );
        assert_eq!(number(&resources, "SELECT count(*) FROM seen_addresses"), 0);
        assert_eq!(resources.received_address_revisions().unwrap().swept, 0);
        write(&resources, |tx| {
            tx.execute_batch("DROP TRIGGER received_cut").unwrap();
            Ok(())
        });
        drain(&resources);
        assert_eq!(
            number(&resources, "SELECT count(*) FROM seen_address_messages"),
            2
        );
    }
}

#[test]
fn malformed_stored_addresses_and_exhausted_counters_fail_closed() {
    let (_scratch, resources) = native("received-corruption");
    let parsed = ImportSummary {
        parser_version: 2,
        parse_state: ImportParseState::Parsed,
        to_json: Some("{}".into()),
        ..ImportSummary::default()
    };
    put(&resources, 1, 1, 1, parsed);
    let scan = resources.begin_received_sweep().unwrap().unwrap();
    assert!(matches!(
        resources.prepare_received_step(scan),
        Err(StoreError::Corrupt { .. })
    ));
    assert_eq!(resources.received_address_revisions().unwrap().swept, 0);
    write(&resources, |tx| {
        tx.execute(
            "UPDATE d_message_summary SET to_json=?1",
            [json!([{"address":"a@example.test","name":null}]).to_string()],
        )
        .unwrap();
        tx.execute("INSERT INTO seen_addresses(canonical,email,last_seen_at,times_received) VALUES('a@example.test','a@example.test',0,9223372036854775807)", []).unwrap();
        Ok(())
    });
    let step = resources
        .prepare_received_step(resources.begin_received_sweep().unwrap().unwrap())
        .unwrap();
    assert!(matches!(
        block_on(resources.commit_received_step(step)),
        Err(StoreError::Corrupt { .. })
    ));
    assert_eq!(
        number(&resources, "SELECT count(*) FROM seen_address_messages"),
        0
    );
}

#[test]
fn retained_history_text_is_charged_before_a_publication_advances() {
    let (_scratch, resources) = native("received-retained-budget");
    let recipients = (0..100)
        .map(|id| format!("u{id}@x.test"))
        .collect::<Vec<_>>()
        .join(",");
    put(
        &resources,
        1,
        1,
        1,
        summary(&format!("To: {recipients}\n\nbody")),
    );
    write(&resources, |tx| {
        let name = "history".repeat(6000);
        for id in 0..100 {
            let email = format!("u{id}@x.test");
            tx.execute("INSERT INTO seen_addresses(canonical,email,display_name,last_seen_at,times_sent_to) VALUES(?1,?1,?2,100,7)", (&email, &name)).unwrap();
        }
        Ok(())
    });
    let prepared = resources
        .prepare_received_step(resources.begin_received_sweep().unwrap().unwrap())
        .unwrap();
    assert!(matches!(
        block_on(resources.commit_received_step(prepared)).unwrap(),
        ReceivedCommit::Continue(_)
    ));
    let charged = number(
        &resources,
        "SELECT sum(3*length(CAST(a.canonical AS BLOB))+length(CAST(a.email AS BLOB))+coalesce(length(CAST(a.display_name AS BLOB)),0)+coalesce(length(CAST(a.last_account_id AS BLOB)),0)+512) FROM seen_address_messages l JOIN seen_addresses a USING(canonical)",
    );
    assert!(
        charged <= sb_store::seen::MAX_RECEIVED_BYTES as i64,
        "retained metadata exceeded one committed budget: {charged}"
    );
    drain(&resources);
    assert_eq!(
        number(&resources, "SELECT count(*) FROM seen_address_messages"),
        100
    );
    assert_eq!(
        number(&resources, "SELECT sum(times_sent_to) FROM seen_addresses"),
        700
    );
}
