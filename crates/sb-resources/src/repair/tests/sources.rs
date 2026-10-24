use super::*;

fn referenced(resources: &Resources) {
    write(resources, |tx| {
        tx.execute_batch("INSERT INTO logical_messages(account_id,acquisition_id,created_at) SELECT account_id,1,0 FROM accounts WHERE provider='local_archive';
            INSERT INTO logical_messages(account_id,acquisition_id,created_at) SELECT account_id,1,1 FROM accounts WHERE provider='local_archive'").unwrap();
        Ok(())
    });
}

#[test]
fn native_rediscovery_caller_repairs_shared_sources_without_rewriting_base_or_history() {
    let (_scratch, resources, _) = native(RICH, real_fs());
    referenced(&resources);
    edit(
        &resources,
        "UPDATE store_meta SET value='1' WHERE key='mime_repair_target'",
    );
    let before = snapshot(&resources);
    let changes = writer_changes(&resources);
    let page = resources.repair_sources(0, None).unwrap();
    assert_eq!(page.acquisition_ids, [1]);
    assert_eq!((page.through, page.cutoff), (1, 1));
    assert_eq!(
        page.inputs as i64,
        number(
            &resources,
            "SELECT CAST(value AS INTEGER) FROM store_meta WHERE key='repair_inputs'"
        )
    );
    assert_eq!(resources.repair_status(1).unwrap().target, 1);
    assert!(
        resources
            .repair_sources(page.through, Some(page.cutoff))
            .unwrap()
            .acquisition_ids
            .is_empty()
    );
    assert_eq!(writer_changes(&resources), changes);
    assert_eq!(snapshot(&resources), before);

    for id in page.acquisition_ids {
        let capture = block_on(resources.begin_repair(id)).unwrap().unwrap();
        let RepairPreparation::Prepared(prepared) =
            resources.prepare_repair(capture, &|| false).unwrap()
        else {
            panic!("real complete source did not prepare")
        };
        finish(&resources, prepared);
        assert_eq!(
            resources.repaired_summary(id).unwrap(),
            Some(expected(RICH))
        );
    }
    assert_eq!(snapshot(&resources), before);
    // Discovery is conservative; the qualified begin gate suppresses completed work.
    let repeated = resources.repair_sources(0, None).unwrap();
    assert_eq!(repeated.acquisition_ids, [1]);
    assert!(
        block_on(resources.begin_repair(repeated.acquisition_ids[0]))
            .unwrap()
            .is_none()
    );
    assert_eq!(
        number(
            &resources,
            "SELECT count(*) FROM d_repair_assemblies WHERE completed_at IS NOT NULL"
        ),
        1
    );
    resources.close().unwrap();
}

#[test]
fn native_rediscovery_keeps_terminal_candidates_for_the_real_suppression_gate() {
    let (_scratch, resources, _) = native(
        b"Content-Type: multipart/mixed; boundary=missing\r\n\r\nbody",
        real_fs(),
    );
    referenced(&resources);
    let before = snapshot(&resources);
    let page = resources.repair_sources(0, None).unwrap();
    assert_eq!(page.acquisition_ids, [1]);
    let capture = block_on(resources.begin_repair(page.acquisition_ids[0]))
        .unwrap()
        .unwrap();
    let RepairPreparation::Terminal(attempt) =
        resources.prepare_repair(capture, &|| false).unwrap()
    else {
        panic!("real partial source did not diagnose")
    };
    assert_eq!(attempt.kind(), RepairTerminalKind::Partial);
    assert_eq!(
        block_on(resources.persist_repair_terminal(attempt)).unwrap(),
        RepairPersist::Diagnosed
    );
    let repeated = resources.repair_sources(0, None).unwrap();
    assert_eq!(repeated.acquisition_ids, [1]);
    assert!(
        block_on(resources.begin_repair(repeated.acquisition_ids[0]))
            .unwrap()
            .is_none()
    );
    assert_eq!(snapshot(&resources), before);
    assert_eq!(
        number(&resources, "SELECT count(*) FROM d_repair_diagnoses"),
        1
    );
    assert_eq!(
        number(&resources, "SELECT count(*) FROM d_repair_assemblies"),
        0
    );
    resources.close().unwrap();
}
