use super::*;

#[test]
fn completed_text_cannot_change_after_verified_publication() {
    const SUBJECT_FIELD: i64 = 1;
    let (scratch, resources, _) = native(RICH, real_fs());
    let value = expected(RICH);
    assert_eq!(value.subject.as_deref(), Some("café"));
    assert_eq!("café".len(), "Café".len());
    finish(&resources, prepare(&resources));
    assert_eq!(resources.repaired_summary(1).unwrap(), Some(value.clone()));
    let before = resources.repair_status(1).unwrap();
    let kept = snapshot(&resources);
    let artifacts = artifact_snapshot(&resources);
    let target = before.target;

    // A same-length replacement passes reconstruction's shape checks unless storage freezes it.
    let mutation = block_on(resources.write(WritePriority::User, move |tx| {
        Ok(tx.execute(
            "UPDATE d_repair_pieces SET text='Café' WHERE acquisition_id=1 AND parser_target=?1 AND field_index=?2 AND byte_offset=0",
            (target, SUBJECT_FIELD),
        ))
    }))
    .unwrap();
    let after = resources.repaired_summary(1).unwrap();
    let status = resources.repair_status(1).unwrap();
    assert!(
        mutation.is_err(),
        "completed mutation returned {mutation:?}; summary={after:?}; status={status:?}"
    );
    assert_eq!(after, Some(value.clone()));
    assert_eq!(
        (
            status.completed_at,
            status.pieces,
            status.bytes,
            status.diagnosis
        ),
        (
            before.completed_at,
            before.pieces,
            before.bytes,
            before.diagnosis
        )
    );
    assert_eq!(snapshot(&resources), kept);
    assert_eq!(artifact_snapshot(&resources), artifacts);
    resources.close().unwrap();
    let resources = Resources::open(&scratch.path().join("store")).unwrap();
    assert_eq!(resources.repaired_summary(1).unwrap(), Some(value));
    assert_eq!(snapshot(&resources), kept);
    assert_eq!(artifact_snapshot(&resources), artifacts);
    resources.close().unwrap();
}

#[test]
fn matching_replay_and_refused_transaction_keep_exact_publication() {
    let (scratch, resources, _) = native(RICH, real_fs());
    write(&resources, |tx| {
        tx.execute("INSERT INTO logical_messages(account_id,acquisition_id,created_at) SELECT account_id,1,0 FROM accounts WHERE provider='local_archive'", []).unwrap();
        tx.execute(
            "INSERT INTO seen_address_messages SELECT last_insert_rowid(),'kept@x.test'",
            [],
        )
        .unwrap();
        Ok(())
    });
    let first = prepare(&resources);
    let replay = prepare(&resources);
    finish(&resources, first);
    let kept = snapshot(&resources);
    let artifacts = artifact_snapshot(&resources);
    let status = format!("{:?}", resources.repair_status(1).unwrap());
    let changes = writer_changes(&resources);
    assert!(matches!(
        block_on(resources.commit_repair_step(replay)).unwrap(),
        RepairCommit::Current
    ));
    assert_eq!(writer_changes(&resources), changes);

    let failed = block_on(resources.write(WritePriority::User, |tx| {
        tx.execute(
            "UPDATE seen_addresses SET times_received=times_received+1",
            [],
        )
        .unwrap();
        tx.execute("UPDATE d_repair_assemblies SET date_header=NULL", [])
            .map_err(|error| StoreError::Internal {
                detail: error.to_string(),
            })?;
        Ok(())
    }));
    assert!(matches!(failed, Err(StoreError::Internal { .. })));
    assert_eq!(resources.repaired_summary(1).unwrap(), Some(expected(RICH)));
    assert_eq!(snapshot(&resources), kept);
    assert_eq!(artifact_snapshot(&resources), artifacts);
    assert_eq!(format!("{:?}", resources.repair_status(1).unwrap()), status);
    resources.close().unwrap();
    let resources = Resources::open(&scratch.path().join("store")).unwrap();
    assert_eq!(snapshot(&resources), kept);
    assert_eq!(artifact_snapshot(&resources), artifacts);
    assert_eq!(format!("{:?}", resources.repair_status(1).unwrap()), status);
    assert_eq!(resources.repaired_summary(1).unwrap(), Some(expected(RICH)));
    resources.close().unwrap();
}

#[test]
fn completed_sources_keep_availability_changes_and_identity_validation() {
    let (_scratch, resources, object) = native(RICH, real_fs());
    finish(&resources, prepare(&resources));
    let artifacts = artifact_snapshot(&resources);
    for state in ["missing", "quarantined", "published"] {
        write(&resources, move |tx| {
            tx.execute("UPDATE acquisitions SET object_state=?1", [state])
                .unwrap();
            Ok(())
        });
        assert_eq!(resources.repaired_summary(1).unwrap(), Some(expected(RICH)));
        assert!(resources.repair_status(1).unwrap().completed_at.is_some());
    }
    for change in [
        "byte_length=byte_length+1",
        "content_sha256=printf('%064d',9)",
    ] {
        edit(&resources, &format!("UPDATE acquisitions SET {change}"));
        assert!(matches!(
            resources.repaired_summary(1),
            Err(StoreError::Corrupt { .. })
        ));
        assert!(matches!(
            resources.repair_status(1),
            Err(StoreError::Corrupt { .. })
        ));
        assert!(matches!(
            block_on(resources.begin_repair(1)),
            Err(StoreError::Corrupt { .. })
        ));
        let (hash, bytes) = (object.sha256_hex(), object.byte_length() as i64);
        write(&resources, move |tx| {
            tx.execute(
                "UPDATE acquisitions SET content_sha256=?1,byte_length=?2",
                (hash, bytes),
            )
            .unwrap();
            Ok(())
        });
    }
    assert_eq!(resources.repaired_summary(1).unwrap(), Some(expected(RICH)));
    assert_eq!(artifact_snapshot(&resources), artifacts);
    resources.close().unwrap();
}
