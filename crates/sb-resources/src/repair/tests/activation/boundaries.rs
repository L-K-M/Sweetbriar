use super::*;
use sb_store::imports::{
    ImportBatch, ImportFlags, ImportSourceKind, commit_import_batch, record_import_source,
};

#[test]
fn message_removed_after_preparation_recaptures_without_a_false_error_or_write() {
    let (_scratch, resources) = published(RICH, 1);
    let pending = prepared(&resources, 1).0;
    edit(
        &resources,
        "DELETE FROM d_list_rows WHERE message_id=1; DELETE FROM occurrences WHERE message_id=1; DELETE FROM logical_messages WHERE message_id=1",
    );
    let before = state(&resources);
    assert_eq!(
        block_on(resources.commit_activation(pending)).unwrap(),
        ActivationCommit::Recapture
    );
    assert_eq!(state(&resources), before);
    resources.close().unwrap();
}

#[test]
fn cancellation_during_envelope_preparation_returns_cancelled_without_a_decision() {
    let (_scratch, resources) = published(RICH, 1);
    let work = begin(&resources, 1);
    let calls = AtomicUsize::new(0);
    let before = (state(&resources), snapshot(&resources));
    assert!(matches!(
        resources
            .prepare_activation(work, &|| calls.fetch_add(1, Ordering::SeqCst) > 0)
            .unwrap(),
        ActivationPreparation::Cancelled
    ));
    assert_eq!((state(&resources), snapshot(&resources)), before);
    resources.close().unwrap();
}

#[test]
fn older_binding_is_read_at_its_exact_target_then_advances_atomically() {
    let (_scratch, resources) = published(RICH, 2);
    bind_older(&resources);
    assert_eq!(reader(&resources, 1).subject.as_deref(), Some("EARLY"));
    assert_eq!(
        resources.activation_status(1).unwrap().bound_target,
        Some(1)
    );
    assert_eq!(activate(&resources, 1), ActivationCommit::Activated);
    assert_eq!(reader(&resources, 1).subject.as_deref(), Some("café"));
    assert_eq!(
        resources.activation_status(1).unwrap().bound_target,
        Some(2)
    );
    assert_eq!(
        number(
            &resources,
            "SELECT count(*) FROM d_list_rows WHERE subject='café'"
        ),
        2
    );
    assert_eq!(
        number(
            &resources,
            "SELECT count(*) FROM d_repair_assemblies WHERE completed_at IS NOT NULL"
        ),
        2
    );
    resources.close().unwrap();
}

#[test]
fn absent_or_incomplete_candidate_keeps_a_valid_older_binding_and_checks_corruption_first() {
    let (_scratch, resources) = published(RICH, 1);
    assert_eq!(activate(&resources, 1), ActivationCommit::Activated);
    // A newer build's supported metadata reconciliation can precede its publication.
    assert!(
        block_on(resources.store.begin_activation(1, 3))
            .unwrap()
            .is_none()
    );
    assert_eq!(reader(&resources, 1).subject.as_deref(), Some("café"));
    edit(
        &resources,
        "INSERT INTO d_repair_assemblies SELECT acquisition_id,3,source_sha256,source_bytes,date_header,has_attachments,field_lengths,NULL FROM d_repair_assemblies WHERE parser_target=2",
    );
    assert!(
        block_on(resources.store.begin_activation(1, 3))
            .unwrap()
            .is_none()
    );
    edit(
        &resources,
        "UPDATE acquisitions SET byte_length=byte_length+1",
    );
    assert!(matches!(
        block_on(resources.store.begin_activation(1, 3)),
        Err(StoreError::Corrupt { .. })
    ));
    resources.close().unwrap();
}

#[test]
fn metadata_only_eligibility_change_dirties_population_without_false_list_search_or_thread_effects()
{
    let (_scratch, resources) = published(b"Subject: actual\r\n\r\n", 1);
    edit(&resources, "UPDATE d_message_summary SET parse_state='ok';
        INSERT INTO message_headers(message_id,raw_fields,subject,from_json,to_json,cc_json,header_message_id,references_json,in_reply_to,date_header,fetched_at) VALUES(1,x'','MASK','[{\"name\":\"MASK SENDER\",\"address\":\"mask@x.test\"}]','[]','[]','<mask@x.test>','[]','<parent@x.test>',17,0)");
    refresh(&resources);
    let before = generations(&resources);
    let row = state(&resources);
    assert_eq!(activate(&resources, 1), ActivationCommit::Activated);
    assert_eq!(
        generations(&resources),
        [before[0], before[1], before[2], before[3] + 1]
    );
    assert_eq!(
        number(
            &resources,
            "SELECT count(*) FROM d_list_rows WHERE subject='MASK' AND sender='old@x.test'"
        ),
        0
    );
    assert!(
        state(&resources) != row,
        "the binding, not row invalidation, changed"
    );
    resources.close().unwrap();
}

#[test]
fn population_captured_before_activation_recaptures_without_publishing_old_keys() {
    let (_scratch, resources) = published(RICH, 1);
    let sweep = resources.begin_received_sweep().unwrap().unwrap();
    let pending = resources.prepare_received_step(sweep).unwrap();
    assert_eq!(activate(&resources, 1), ActivationCommit::Activated);
    assert!(matches!(
        block_on(resources.commit_received_step(pending)).unwrap(),
        ReceivedCommit::Recapture(_)
    ));
    assert_eq!(
        number(&resources, "SELECT count(*) FROM seen_address_messages"),
        0
    );
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
        18
    );
    resources.close().unwrap();
}

#[test]
fn large_envelope_inspection_yields_after_one_legal_row_and_completes() {
    let count = (sb_mime::MAX_HEADER_BYTES - "To: \r\n\r\n".len()) / "é,".len();
    let raw = format!("To: {}\r\n\r\n", "é,".repeat(count));
    let value = expected(raw.as_bytes());
    let addresses = value.to_json.unwrap();
    assert!(addresses.len() * 3 > 4_000_000);
    let (_scratch, resources) = published(raw.as_bytes(), 3);
    write(&resources, move |tx| {
        tx.execute(
            "UPDATE d_message_summary SET to_json=?1,reply_to_json=?1",
            [addresses],
        )
        .unwrap();
        Ok(())
    });
    let (pending, steps) = prepared(&resources, 1);
    assert_eq!(
        steps, 6,
        "envelope, three single-row quanta, raw EOF, short cache page with observed EOF"
    );
    assert_eq!(
        block_on(resources.commit_activation(pending)).unwrap(),
        ActivationCommit::Activated
    );
    assert_eq!(
        resources.activation_status(1).unwrap().bound_target,
        Some(2)
    );
    resources.close().unwrap();
}

#[test]
fn observer_failure_during_own_dml_rolls_back_activation_but_ordinary_writes_remain_usable() {
    let (_scratch, resources) = published(RICH, 1);
    let pending = prepared(&resources, 1).0;
    edit(
        &resources,
        "CREATE TEMP TRIGGER activation_latch AFTER INSERT ON main.d_message_bindings BEGIN SELECT sb_activation_input('invalid',0,0); END",
    );
    let before = (state(&resources), snapshot(&resources));
    assert!(matches!(
        block_on(resources.commit_activation(pending)),
        Err(StoreError::Internal { .. })
    ));
    assert_eq!((state(&resources), snapshot(&resources)), before);
    edit(
        &resources,
        "UPDATE occurrences SET f_seen=1 WHERE occurrence_id=1",
    );
    assert_eq!(
        number(
            &resources,
            "SELECT f_seen FROM occurrences WHERE occurrence_id=1"
        ),
        1
    );
    resources.close().unwrap();
}

#[test]
fn deferral_commit_failure_keeps_the_old_message_and_emits_no_receipt() {
    let (_scratch, resources) = published(RICH, 4000);
    let pending = prepared(&resources, 1).0;
    edit(
        &resources,
        "CREATE TEMP TABLE activation_parent(id INTEGER PRIMARY KEY); CREATE TEMP TABLE activation_probe(id INTEGER REFERENCES activation_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE TEMP TRIGGER activation_cut AFTER INSERT ON main.d_activation_deferrals BEGIN INSERT INTO activation_probe VALUES(1); END",
    );
    let before = (state(&resources), snapshot(&resources));
    assert!(block_on(resources.commit_activation(pending)).is_err());
    assert_eq!((state(&resources), snapshot(&resources)), before);
    assert!(
        resources
            .activation_status(1)
            .unwrap()
            .recorded_deferral
            .is_none()
    );
    edit(&resources, "DROP TRIGGER activation_cut");
    assert_eq!(activate(&resources, 1), ActivationCommit::Deferred);
    resources.close().unwrap();
}

#[test]
fn malformed_persisted_witness_is_corrupt_instead_of_a_skip_certificate() {
    let (_scratch, resources) = published(RICH, 1);
    edit(
        &resources,
        "INSERT INTO d_activation_deferrals VALUES(1,1,2,NULL,NULL,zeroblob(24),2000001,0)",
    );
    let before = state(&resources);
    assert!(matches!(
        block_on(resources.begin_activation(1)),
        Err(StoreError::Corrupt { .. })
    ));
    assert!(matches!(
        resources.activation_status(1),
        Err(StoreError::Corrupt { .. })
    ));
    assert_eq!(state(&resources), before);
    resources.close().unwrap();
}

#[test]
fn ordinary_import_prices_mixed_bound_and_base_sources_without_replacing_either() {
    const JOB: &str = "00000000-0000-4000-8000-00000000ac71";
    for first_summary in [false, true] {
        let (_scratch, resources) = published(RICH, 2);
        seed(&resources, 2, 1);
        assert_eq!(activate(&resources, 1), ActivationCommit::Activated);
        if first_summary {
            edit(&resources, "DELETE FROM d_message_summary");
            refresh(&resources);
        }
        let object = resources.store.objects().publish(RICH).unwrap();
        let summary = expected(RICH);
        let revision = write(&resources, |tx| {
            sb_store::imports::create_import_job(
                tx,
                JOB,
                "{\"v\":1,\"paths\":[]}",
                Timestamp::from_millis(1000),
            )?;
            record_import_source(
                tx,
                JOB,
                0,
                "{\"v\":1,\"path\":\"/synthetic/activation.mbox\"}",
            )?;
            sb_store::imports::advance_import_job(
                tx,
                JOB,
                1,
                crate::ImportJobState::AwaitingConfirmation,
                "{\"v\":1}",
                Timestamp::from_millis(1000),
            )?;
            Ok(sb_store::imports::advance_import_job(
                tx,
                JOB,
                2,
                crate::ImportJobState::Running,
                "{\"v\":1}",
                Timestamp::from_millis(1000),
            )?
            .revision())
        });
        let before = generations(&resources);
        write(&resources, move |tx| {
            let message = crate::ImportMessage {
                object,
                summary,
                source_kind: ImportSourceKind::Mbox,
                source_path: "/synthetic/activation.mbox".into(),
                source_sha256: "a".repeat(64),
                offset: 0,
                flags: ImportFlags::default(),
            };
            let stats = commit_import_batch(
                tx,
                &ImportBatch {
                    job_id: JOB,
                    expected_revision: revision,
                    source_ordinal: 0,
                    mailbox_name: "Imported",
                    messages: &[message],
                },
                Timestamp::from_millis(1000),
                |_| Ok(("{\"v\":1}".into(), "{\"v\":1}".into())),
            )?;
            assert_eq!((stats.imported, stats.duplicates), (1, 1));
            Ok(())
        });
        assert_eq!(reader(&resources, 1).subject.as_deref(), Some("café"));
        let unbound = if first_summary { "café" } else { "OLD" };
        assert_eq!(reader(&resources, 2).subject.as_deref(), Some(unbound));
        assert_eq!(reader(&resources, 3).subject.as_deref(), Some(unbound));
        assert_eq!(
            number(
                &resources,
                "SELECT count(*) FROM d_message_summary WHERE subject='OLD'"
            ),
            i64::from(!first_summary)
        );
        assert_eq!(generations(&resources), before.map(|value| value + 1));
        assert_eq!(
            number(
                &resources,
                "SELECT revision FROM import_jobs WHERE job_id='00000000-0000-4000-8000-00000000ac71'"
            ),
            revision + 1
        );
        resources.close().unwrap();
    }
}

pub(super) fn bind_older(resources: &Resources) {
    // Model a retained older publication using the real producer's twelve-field packet.
    edit(resources, "INSERT INTO d_repair_assemblies SELECT acquisition_id,1,source_sha256,source_bytes,date_header,has_attachments,field_lengths,NULL FROM d_repair_assemblies WHERE parser_target=2;
        INSERT INTO d_repair_pieces SELECT acquisition_id,1,field_index,byte_offset,CASE WHEN field_index=1 THEN 'EARLY' ELSE text END FROM d_repair_pieces WHERE parser_target=2;
        UPDATE d_repair_assemblies SET completed_at=1 WHERE parser_target=1;
        INSERT INTO d_message_bindings VALUES(1,1,1,1)");
    refresh(resources);
}

#[test]
fn name_only_from_knownness_changes_search_without_changing_displayed_bytes() {
    let raw = b"From: \"NameOnly\"\r\nSubject: SAME\r\n\r\n";
    let expected = expected(raw);
    assert_eq!(expected.from_name.as_deref(), Some("NameOnly"));
    assert_eq!(expected.from_addr, None);
    let (_scratch, resources) = published(raw, 1);
    edit(
        &resources,
        "UPDATE d_message_summary SET parse_state='ok',from_name='NameOnly',from_addr=NULL,subject='SAME'",
    );
    refresh(&resources);
    let before = generations(&resources);
    let q = query("absent@x.test");
    let counts = resources
        .read(|tx| sb_store::query::counts(tx, &q, crate::SearchScope::All))
        .unwrap();
    assert_eq!(
        (counts.matching_messages(), counts.excluded_unknown()),
        (0, 1)
    );
    assert_eq!(activate(&resources, 1), ActivationCommit::Activated);
    assert_eq!(
        generations(&resources),
        [before[0], before[1] + 1, before[2], before[3] + 1]
    );
    let counts = resources
        .read(|tx| sb_store::query::counts(tx, &q, crate::SearchScope::All))
        .unwrap();
    // A name without an address stays unknown, but the selected source still changed.
    assert_eq!(
        (counts.matching_messages(), counts.excluded_unknown()),
        (0, 1)
    );
    assert_eq!(
        number(
            &resources,
            "SELECT count(*) FROM d_list_rows WHERE sender='NameOnly' AND subject='SAME'"
        ),
        1
    );
    resources.close().unwrap();
}

#[test]
fn cache_owner_phase_yields_on_inspected_bytes_even_when_no_rows_need_updates() {
    let count = (sb_mime::MAX_HEADER_BYTES - "Subject: \r\n\r\n".len()) / "é".len();
    let raw = format!("Subject: {}\r\n\r\n", "é".repeat(count));
    let subject = expected(raw.as_bytes()).subject.unwrap();
    assert!(subject.len() > 250_000);
    let (_scratch, resources) = published(raw.as_bytes(), 20);
    write(&resources, move |tx| {
        tx.execute("INSERT INTO message_headers(message_id,raw_fields,subject,from_json,date_header,fetched_at) VALUES(1,x'',?1,'[{\"name\":\"MASK\",\"address\":\"mask@x.test\"}]',17,0)", [subject]).unwrap();
        Ok(())
    });
    refresh(&resources);
    let before = generations(&resources);
    let (pending, steps) = prepared(&resources, 1);
    assert_eq!(
        steps, 10,
        "envelope + seven bounded raw turns + two bounded cache turns"
    );
    assert_eq!(
        block_on(resources.commit_activation(pending)).unwrap(),
        ActivationCommit::Activated
    );
    assert_eq!(
        generations(&resources)[0],
        before[0],
        "identical displayed images cost no row mutation"
    );
    resources.close().unwrap();
}

#[test]
fn full_cache_page_does_not_hide_a_foreign_owner_beyond_its_limit() {
    let (_scratch, resources) = published(RICH, 200);
    seed(&resources, 2, 1);
    edit(
        &resources,
        "UPDATE d_list_rows SET message_id=1,representative=0 WHERE occurrence_id=201",
    );
    let before = (state(&resources), snapshot(&resources));
    let mut work = begin(&resources, 1);
    let mut rejected = false;
    for _ in 0..10 {
        work = match resources.prepare_activation(work, &|| false) {
            Ok(ActivationPreparation::Continue(next)) => next,
            Err(StoreError::Corrupt { .. }) => {
                rejected = true;
                break;
            }
            other => panic!("full cache page hid its later foreign owner: {other:?}"),
        };
    }
    assert!(rejected, "foreign cache owner was not inspected");
    assert_eq!((state(&resources), snapshot(&resources)), before);
    assert_eq!(
        number(&resources, "SELECT count(*) FROM d_message_bindings"),
        0
    );
    resources.close().unwrap();
}

#[test]
fn oversized_current_envelope_is_corrupt_before_any_raw_page_or_durable_decision() {
    let (_scratch, resources) = published(RICH, 1);
    write(&resources, |tx| {
        tx.execute(
            "UPDATE d_message_summary SET subject=?1",
            ["x".repeat(9 * 1024 * 1024)],
        )
        .unwrap();
        Ok(())
    });
    let work = begin(&resources, 1);
    let before = writer_changes(&resources);
    assert!(matches!(
        resources.prepare_activation(work, &|| false),
        Err(StoreError::Corrupt { .. })
    ));
    assert_eq!(writer_changes(&resources), before);
    assert_eq!(
        number(&resources, "SELECT count(*) FROM d_message_bindings"),
        0
    );
    resources.close().unwrap();
}
