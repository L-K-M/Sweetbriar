//! A backup omits pending publication bytes, even when its database
//! captured the still-live reservation. Restore must revoke that authority.
use super::*;
use crate::backups::BackupDestination;
use crate::backups::tests::open;
use crate::drafts::{ComposeFormat, DraftAttachment, DraftSource, RevisionInput};
use crate::testing::{FIXED_NOW_MS, reading, scratch_paths, test_seams, writing};
use crate::{ObjectStore, Timestamp, WritePriority};
use sb_test_support::{block_on, real_fs};

fn revision(id: &str, attachments: Vec<DraftAttachment>) -> RevisionInput {
    RevisionInput {
        revision_id: id.into(),
        identity_id: None,
        format: ComposeFormat::Plain,
        plain_flowed: true,
        subject: "Synthetic restore draft".into(),
        to: vec![],
        cc: vec![],
        bcc: vec![],
        body_source: "Preserve the saved content".into(),
        attachments,
    }
}

fn attachment(object: Arc<crate::PublishedObject>) -> Vec<DraftAttachment> {
    vec![DraftAttachment::New {
        object,
        filename: "pending.txt".into(),
        media_type: "text/plain".into(),
    }]
}

#[test]
fn restored_publications_cannot_spend_inherited_reservations() {
    let (_source_scratch, source_paths, source) = open("restore-publication-source");
    let now = Timestamp::from_millis(FIXED_NOW_MS);
    let before = writing(&source, move |tx| {
        crate::drafts::create_draft(tx, "pending", &DraftSource::New, now)?;
        crate::drafts::create_draft(tx, "saved", &DraftSource::New, now)?;
        crate::run_once(tx, "already-saved", "save_draft", now, |tx| {
            crate::drafts::save_draft(tx, "saved", None, revision("before", vec![]), now)?;
            Ok("{\"revision_id\":\"before\"}".into())
        })
    });
    let bytes = b"published but not referenced by the backup";
    let old = Arc::new(source.objects().publish(bytes.as_slice()).unwrap());
    let hash = old.id().sha256_hex();
    let control_hash = hash.clone();
    writing(&source, move |tx| {
        tx.execute(
            "INSERT INTO object_pins VALUES (?1,'quarantine','keep',?2)",
            params![control_hash, FIXED_NOW_MS],
        )
        .map_err(|e| classify(e, "seed independent pin"))?;
        Ok(())
    });
    let snapshot = source.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let backup = source.complete_backup(snapshot).unwrap();
    let metadata = reading(&source, |conn| {
        conn.query_row(
            "SELECT manifest_sha256,object_count,total_bytes,completed_at
            FROM backup_generations WHERE generation_id=?1",
            [backup.generation_id()],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .map_err(|e| classify(e, "read completed backup metadata"))
    });
    assert_eq!(
        metadata.1, 0,
        "pending publications are not backup references"
    );

    // Copy the real completed snapshot into a fresh synthetic root, just
    // as restore staging does. No pending object is present in that backup.
    let (_target_scratch, target_paths) = scratch_paths("restore-publication-target");
    for directory in target_paths.layout_dirs() {
        std::fs::create_dir_all(directory).unwrap();
    }
    std::fs::copy(
        backup.location().join("state.sqlite"),
        target_paths.state_db(),
    )
    .unwrap();
    let mut staged = Connection::open(target_paths.state_db()).unwrap();
    let preserved = source_paths.data_root().with_extension("preserved");
    apply(
        &mut staged,
        &Quarantine {
            new_incarnation: "00000000-0000-4000-8000-0000000000e1",
            generation: backup.generation_id(),
            source_location: backup.location().to_str().unwrap(),
            current_root: source_paths.data_root(),
            preserved_root: &preserved,
            manifest_sha256: &metadata.0,
            object_count: metadata.1,
            total_bytes: metadata.2,
            completed_at: metadata.3,
            cancel: &Arc::new(AtomicBool::new(false)),
        },
    )
    .unwrap();
    staged.close().unwrap();
    let restored = crate::open(&target_paths, &test_seams(real_fs())).unwrap();
    assert!(!restored.objects().object_path(old.id()).exists());
    let attempted = Arc::clone(&old);
    let result = block_on(restored.write(WritePriority::User, move |tx| {
        crate::run_once(tx, "fresh-save", "save_draft", now, |tx| {
            crate::drafts::save_draft(
                tx,
                "pending",
                None,
                revision("unsafe-missing-attachment", attachment(attempted)),
                now,
            )?;
            Ok("{\"revision_id\":\"unsafe-missing-attachment\"}".into())
        })
    }));
    assert!(
        matches!(result, Err(StoreError::Conflict { .. })),
        "a new incarnation acknowledged an absent attachment using old authority: {result:?}"
    );
    let counts = reading(&restored, |conn| {
        conn.query_row(
            "SELECT (SELECT count(*) FROM object_pins WHERE pin_kind='reservation'),
            (SELECT count(*) FROM object_pins WHERE pin_kind='quarantine'),
            (SELECT count(*) FROM command_receipts WHERE idempotency_key='fresh-save'),
            (SELECT count(*) FROM draft_revisions WHERE draft_id='pending')",
            [],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, i64>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, i64>(3)?,
                ))
            },
        )
        .map_err(|e| classify(e, "check restored reservation refusal"))
    });
    assert_eq!(counts, (0, 1, 0, 0));
    let replay = writing(&restored, move |tx| {
        crate::run_once(tx, "already-saved", "save_draft", now, |_| {
            panic!("a restored receipt must replay before fresh validation")
        })
    });
    assert_eq!(replay, before);

    // Revoking copied authority must not revoke the original store's
    // publication, and the restored store can publish fresh bytes normally.
    writing(&source, move |tx| {
        crate::drafts::save_draft(
            tx,
            "pending",
            None,
            revision("source-still-valid", attachment(old)),
            now,
        )
    });
    let fresh = Arc::new(restored.objects().publish(bytes.as_slice()).unwrap());
    writing(&restored, move |tx| {
        crate::drafts::save_draft(
            tx,
            "pending",
            None,
            revision("fresh-publication", attachment(fresh)),
            now,
        )
    });
    assert_eq!(
        reading(&restored, |conn| crate::drafts::draft(conn, "pending"))
            .unwrap()
            .revision
            .unwrap()
            .attachments[0]
            .object_sha256,
        hash
    );
    restored.close().unwrap();
    source.close().unwrap();
}
