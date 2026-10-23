//! Actual retained-source publication qualification.

use std::io::{Seek, SeekFrom, Write};
use std::sync::Arc;
use std::sync::atomic::{AtomicUsize, Ordering};

use sb_store::{Fs, ObjectId, ObjectStore, Transaction};
use sb_test_support::{FaultFs, FaultKind, FsOp, ScratchDir, block_on, real_fs};

use super::*;

const RICH: &[u8] = concat!(
    "From: Sender <User@EXAMPLE.test>\r\nTo: A <a@x.test>,b@x.test\r\n",
    "Cc: c@x.test\r\nReply-To: reply@x.test\r\nMessage-ID: <new@x.test>\r\n",
    "Subject: café\r\nDate: Tue, 6 Oct 2026 12:30:00 +0200\r\n",
    "References: <one@x.test> <two@x.test>\r\nIn-Reply-To: <two@x.test>\r\n",
    "List-ID: team <list.x.test>\r\nDelivered-To: owner@x.test\r\n",
    "X-Original-To: alias@x.test\r\nContent-Type: multipart/mixed; boundary=b\r\n\r\n",
    "--b\r\nContent-Type: text/plain; charset=utf-8\r\n\r\nbody\r\n",
    "--b\r\nContent-Type: application/pdf\r\nContent-Disposition: attachment; filename=a.pdf\r\n\r\npdf\r\n--b--\r\n"
).as_bytes();

fn write<T: Send + 'static>(
    resources: &Resources,
    f: impl FnOnce(&Transaction) -> Result<T, StoreError> + Send + 'static,
) -> T {
    block_on(resources.write(WritePriority::User, f)).unwrap()
}

fn native(bytes: &[u8], fs: Arc<dyn Fs>) -> (ScratchDir, Resources, ObjectId) {
    let scratch = ScratchDir::new("repair-publication");
    let resources = Resources::open_on(
        &scratch.path().join("store"),
        fs,
        Arc::new(sb_test_support::FixedClock::new(Timestamp::from_millis(
            1000,
        ))),
    )
    .unwrap();
    let object = resources.store.objects().publish(bytes).unwrap();
    let (hash, length) = (object.sha256_hex(), object.byte_length() as i64);
    write(&resources, move |tx| {
        tx.execute("INSERT INTO acquisitions(acquisition_id,content_sha256,byte_length,object_state,first_acquired_at) VALUES(1,?1,?2,'published',0)", (hash, length)).unwrap();
        tx.execute_batch("INSERT INTO d_message_summary(acquisition_id,parser_version,parse_state,subject,from_addr,has_attachments) VALUES(1,1,'partial','OLD','old@x.test',0);
          INSERT INTO seen_addresses(canonical,email,last_seen_at,times_received,times_sent_to) VALUES('kept@x.test','kept@x.test',-1,11,7)").unwrap();
        Ok(())
    });
    (scratch, resources, object)
}

fn number(resources: &Resources, sql: &str) -> i64 {
    resources
        .read(|tx| Ok(tx.query_row(sql, [], |r| r.get(0)).unwrap()))
        .unwrap()
}

fn edit(resources: &Resources, sql: &str) {
    let sql = sql.to_owned();
    write(resources, move |tx| {
        tx.execute_batch(&sql).unwrap();
        Ok(())
    });
}

fn snapshot(resources: &Resources) -> String {
    resources.read(|tx| Ok(tx.query_row("SELECT json_array(
      (SELECT json_array(acquisition_id,parser_version,parse_state,header_message_id,subject,from_addr,from_name,reply_to_json,to_json,cc_json,date_header,in_reply_to,references_json,list_id,delivered_to_json,has_attachments,snippet) FROM d_message_summary WHERE acquisition_id=1),
      (SELECT json_group_array(json_array(canonical,email,last_seen_at,times_received,times_sent_to)) FROM seen_addresses),
      (SELECT count(*) FROM seen_address_messages),
      (SELECT json_group_array(json_array(key,value)) FROM (SELECT key,value FROM store_meta WHERE key IN('list_generation','search_generation','index_inputs','seen_address_inputs','seen_address_swept','thread_inputs','threads_built') ORDER BY key)))", [], |r| r.get(0)).unwrap())).unwrap()
}

fn capture(resources: &Resources) -> RepairCapture {
    block_on(resources.begin_repair(1)).unwrap().unwrap()
}

fn prepare(resources: &Resources) -> RepairPrepared {
    match resources
        .prepare_repair(capture(resources), &|| false)
        .unwrap()
    {
        RepairPreparation::Prepared(value) => value,
        other => panic!("expected complete preparation: {other:?}"),
    }
}

fn terminal(resources: &Resources) -> RepairTerminal {
    match resources
        .prepare_repair(capture(resources), &|| false)
        .unwrap()
    {
        RepairPreparation::Terminal(value) => value,
        other => panic!("expected a real terminal attempt: {other:?}"),
    }
}

fn writer_changes(resources: &Resources) -> i64 {
    write(resources, |tx| {
        Ok(tx
            .query_row("SELECT total_changes()", [], |r| r.get(0))
            .unwrap())
    })
}

fn finish(resources: &Resources, mut prepared: RepairPrepared) -> usize {
    let mut steps = 0;
    loop {
        let before = resources.repair_status(1).unwrap();
        let committed = block_on(resources.commit_repair_step(prepared)).unwrap();
        let after = resources.repair_status(1).unwrap();
        assert!(after.pieces - before.pieces <= sb_store::repair::MAX_STAGE_PIECES as u32);
        let charged =
            after.bytes - before.bytes + u64::from(after.pieces - before.pieces) * 512 + 4096;
        assert!(charged <= sb_store::repair::MAX_REPAIR_BYTES as u64);
        steps += 1;
        assert!(steps <= 1024, "publication made no bounded progress");
        match committed {
            RepairCommit::Continue(next) => prepared = next,
            RepairCommit::Published | RepairCommit::Current => return steps,
            other => panic!("unexpected publication: {other:?}"),
        }
    }
}

fn expected(bytes: &[u8]) -> ImportSummary {
    let parsed = sb_mime::parse(bytes, &Limits::default());
    assert!(matches!(parsed, ParseOutcome::Parsed(_)), "{parsed:?}");
    crate::acquire::summary(&parsed)
}

fn object_path(scratch: &ScratchDir, object: &ObjectId) -> std::path::PathBuf {
    let hash = object.sha256_hex();
    scratch
        .path()
        .join("store/objects/sha256")
        .join(&hash[..2])
        .join(&hash[2..4])
        .join(hash)
}

#[test]
fn rich_verified_publication_round_trips_every_field_without_rewriting_history() {
    let (scratch, resources, _) = native(RICH, real_fs());
    let before = snapshot(&resources);
    let value = expected(RICH);
    assert!(
        value.header_message_id.is_some() && value.date_header.is_some() && value.has_attachments
    );
    assert!(
        value.reply_to_json.is_some()
            && value.references_json.is_some()
            && value.delivered_to_json.is_some()
    );
    assert_eq!(
        resources.repair_status(1).unwrap().target,
        sb_mime::PARSER_VERSION
    );
    assert_eq!(resources.repaired_summary(1).unwrap(), None);
    finish(&resources, prepare(&resources));
    assert_eq!(resources.repaired_summary(1).unwrap(), Some(value.clone()));
    assert!(resources.repair_status(1).unwrap().completed_at.is_some());
    edit(
        &resources,
        "UPDATE d_repair_assemblies SET source_bytes=source_bytes+1",
    );
    assert!(matches!(
        block_on(resources.begin_repair(1)),
        Err(StoreError::Corrupt { .. })
    ));
    assert!(matches!(
        resources.repair_status(1),
        Err(StoreError::Corrupt { .. })
    ));
    edit(
        &resources,
        "UPDATE d_repair_assemblies SET source_bytes=source_bytes-1",
    );
    assert_eq!(snapshot(&resources), before);
    assert!(block_on(resources.begin_repair(1)).unwrap().is_none());
    resources.close().unwrap();
    let resources = Resources::open(&scratch.path().join("store")).unwrap();
    assert_eq!(resources.repaired_summary(1).unwrap(), Some(value));
    assert_eq!(snapshot(&resources), before);
    edit(
        &resources,
        "UPDATE store_meta SET value='3' WHERE key='mime_repair_target'",
    );
    resources.close().unwrap();
    let resources = Resources::open(&scratch.path().join("store")).unwrap();
    assert_eq!(resources.repair_status(1).unwrap().target, 3);
    assert_eq!(
        number(
            &resources,
            "SELECT count(*) FROM d_repair_assemblies WHERE parser_target=3"
        ),
        0
    );
    assert!(matches!(
        block_on(resources.begin_repair(1)),
        Err(StoreError::Unavailable { .. })
    ));
    assert!(matches!(
        resources.repaired_summary(1),
        Err(StoreError::Unavailable { .. })
    ));
    resources.close().unwrap();
}

#[test]
fn large_admitted_utf8_output_is_private_until_complete_and_replays_bounded_prefix() {
    let prefix = "To: ";
    let count = (sb_mime::MAX_HEADER_BYTES - prefix.len() - "\r\n\r\n".len()) / "é,".len();
    let raw = format!("{prefix}{}\r\n\r\nbody", "é,".repeat(count));
    let value = expected(raw.as_bytes());
    assert!(value.to_json.as_ref().unwrap().len() > sb_store::repair::MAX_REPAIR_BYTES);
    let (scratch, resources, _) = native(raw.as_bytes(), real_fs());
    let before = snapshot(&resources);
    let first = block_on(resources.commit_repair_step(prepare(&resources))).unwrap();
    assert!(matches!(first, RepairCommit::Continue(_)));
    assert_eq!(resources.repair_status(1).unwrap().pieces, 16);
    assert_eq!(resources.repaired_summary(1).unwrap(), None);
    let committed = number(&resources, "SELECT count(*) FROM d_repair_pieces");
    drop(first);
    let original: String = resources.read(|tx| Ok(tx.query_row("SELECT text FROM d_repair_pieces WHERE byte_offset=0 ORDER BY field_index LIMIT 1", [], |r| r.get(0)).unwrap())).unwrap();
    edit(
        &resources,
        "UPDATE d_repair_pieces SET text='X'||substr(text,2) WHERE byte_offset=0",
    );
    assert!(matches!(
        block_on(resources.commit_repair_step(prepare(&resources))),
        Err(StoreError::Corrupt { .. })
    ));
    write(&resources, move |tx| {
        tx.execute(
            "UPDATE d_repair_pieces SET text=?1 WHERE byte_offset=0",
            [original],
        )
        .unwrap();
        Ok(())
    });
    resources.close().unwrap();
    let resources = Resources::open(&scratch.path().join("store")).unwrap();
    let replay = block_on(resources.commit_repair_step(prepare(&resources))).unwrap();
    assert_eq!(
        number(&resources, "SELECT count(*) FROM d_repair_pieces"),
        committed
    );
    let RepairCommit::Continue(mut next) = replay else {
        panic!("replay skipped unfinished work");
    };
    edit(
        &resources,
        "CREATE TEMP TABLE late_parent(id INTEGER PRIMARY KEY); CREATE TEMP TABLE late_probe(id INTEGER REFERENCES late_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE TEMP TRIGGER late_cut AFTER UPDATE OF completed_at ON main.d_repair_assemblies BEGIN INSERT INTO late_probe VALUES(1); END",
    );
    loop {
        match block_on(resources.commit_repair_step(next)) {
            Ok(RepairCommit::Continue(value)) => next = value,
            Err(_) => break,
            other => panic!("expected deferred COMMIT failure: {other:?}"),
        }
    }
    assert!(number(&resources, "SELECT count(*) FROM d_repair_pieces") > committed);
    assert_eq!(resources.repaired_summary(1).unwrap(), None);
    edit(&resources, "DROP TRIGGER late_cut");
    assert!(finish(&resources, prepare(&resources)) > 1);
    assert_eq!(resources.repaired_summary(1).unwrap(), Some(value));
    assert!(
        number(
            &resources,
            "SELECT max(length(CAST(field_lengths AS BLOB))) FROM d_repair_assemblies"
        ) < 256
    );
    assert!(
        number(
            &resources,
            "SELECT count(*) FROM d_repair_pieces WHERE length(CAST(text AS BLOB))>65536"
        ) == 0
    );
    assert_eq!(snapshot(&resources), before);
    resources.close().unwrap();
}

#[test]
fn complete_base_newer_partial_source_and_stale_targets_never_publish_a_downgrade() {
    let (_scratch, resources, _) = native(RICH, real_fs());
    for (version, state) in [(2, "ok"), (3, "partial"), (3, "failed")] {
        let pending = prepare(&resources);
        write(&resources, move |tx| {
            tx.execute(
                "UPDATE d_message_summary SET parser_version=?1,parse_state=?2",
                (version, state),
            )
            .unwrap();
            Ok(())
        });
        assert!(matches!(
            block_on(resources.commit_repair_step(pending)).unwrap(),
            RepairCommit::Current
        ));
        assert!(block_on(resources.begin_repair(1)).unwrap().is_none());
        edit(
            &resources,
            "UPDATE d_message_summary SET parser_version=1,parse_state='partial'",
        );
    }
    let pending = prepare(&resources);
    let captured = capture(&resources);
    edit(
        &resources,
        "UPDATE store_meta SET value='1' WHERE key='mime_repair_target'",
    );
    assert!(matches!(
        resources.prepare_repair(captured, &|| false).unwrap(),
        RepairPreparation::Stale
    ));
    assert!(matches!(
        block_on(resources.commit_repair_step(pending)).unwrap(),
        RepairCommit::Recapture
    ));
    let _ = capture(&resources);
    assert_eq!(resources.repair_status(1).unwrap().target, 2);
    let pending = prepare(&resources);
    edit(&resources, "UPDATE acquisitions SET object_state='missing'");
    assert!(matches!(
        block_on(resources.commit_repair_step(pending)).unwrap(),
        RepairCommit::Recapture
    ));
    edit(
        &resources,
        "UPDATE acquisitions SET object_state='published'",
    );
    let pending = prepare(&resources);
    edit(
        &resources,
        "UPDATE acquisitions SET byte_length=byte_length+1",
    );
    assert!(matches!(
        block_on(resources.commit_repair_step(pending)),
        Err(StoreError::Corrupt { .. })
    ));
    edit(
        &resources,
        "UPDATE acquisitions SET byte_length=byte_length-1",
    );
    for value in ["3", "0", "bad", "4294967296"] {
        let pending = prepare(&resources);
        let captured = capture(&resources);
        write(&resources, move |tx| {
            tx.execute(
                "UPDATE store_meta SET value=?1 WHERE key='mime_repair_target'",
                [value],
            )
            .unwrap();
            Ok(())
        });
        let begin = block_on(resources.begin_repair(1)).unwrap_err();
        let gated = resources.prepare_repair(captured, &|| false).unwrap_err();
        let commit = block_on(resources.commit_repair_step(pending)).unwrap_err();
        for error in [begin, gated, commit] {
            assert!(if value == "3" {
                matches!(error, StoreError::Unavailable { io_kind: None, .. })
            } else {
                matches!(error, StoreError::Corrupt { .. })
            });
        }
        edit(
            &resources,
            "UPDATE store_meta SET value='2' WHERE key='mime_repair_target'",
        );
    }
    assert!(matches!(
        block_on(resources.begin_repair(999)),
        Err(StoreError::NotFound { .. })
    ));
    assert!(matches!(
        resources.repair_status(999),
        Err(StoreError::NotFound { .. })
    ));
    assert_eq!(
        number(&resources, "SELECT count(*) FROM d_repair_assemblies"),
        0
    );
    edit(
        &resources,
        "DELETE FROM store_meta WHERE key='mime_repair_target'",
    );
    assert!(matches!(
        block_on(resources.begin_repair(1)),
        Err(StoreError::Corrupt { .. })
    ));
    resources.close().unwrap();
}

#[test]
fn real_terminal_kinds_persist_once_suppress_io_and_survive_reopen() {
    for (bytes, kind) in [
        (
            format!(
                "Subject: {}\r\n\r\nbody",
                "x".repeat(sb_mime::MAX_HEADER_BYTES)
            )
            .into_bytes(),
            RepairTerminalKind::Partial,
        ),
        (Vec::new(), RepairTerminalKind::Failed),
        (
            vec![b'x'; sb_mime::MAX_MESSAGE_BYTES + 1],
            RepairTerminalKind::Oversized,
        ),
    ] {
        let fs = Arc::new(FaultFs::new(
            real_fs(),
            FsOp::Read,
            u64::MAX,
            FaultKind::Eio,
        ));
        let (scratch, resources, object) = native(&bytes, fs.clone());
        let before = snapshot(&resources);
        let outcome = resources
            .prepare_repair(capture(&resources), &|| false)
            .unwrap();
        let RepairPreparation::Terminal(attempt) = outcome else {
            panic!("expected terminal {kind:?}");
        };
        assert_eq!(attempt.kind(), kind);
        assert_eq!(
            (attempt.acquisition_id(), attempt.target()),
            (1, sb_mime::PARSER_VERSION)
        );
        assert!(resources.repair_status(1).unwrap().completed_at.is_none());
        assert_eq!(resources.repaired_summary(1).unwrap(), None);
        assert_eq!(
            number(&resources, "SELECT count(*) FROM d_repair_pieces"),
            0
        );
        assert_eq!(snapshot(&resources), before);
        assert!(
            block_on(resources.begin_repair(1)).unwrap().is_some(),
            "an unpersisted attempt must remain eligible"
        );
        let pending = capture(&resources);
        let duplicate = terminal(&resources);
        let io = (fs.calls(FsOp::Open), fs.calls(FsOp::Read));
        assert_eq!(
            block_on(resources.persist_repair_terminal(attempt)).unwrap(),
            RepairPersist::Diagnosed
        );
        let diagnosis = RepairDiagnosis {
            kind,
            diagnosed_at: Timestamp::from_millis(1000),
        };
        assert_eq!(
            resources.repair_status(1).unwrap().diagnosis,
            Some(diagnosis)
        );
        let text = match kind {
            RepairTerminalKind::Partial => "partial",
            RepairTerminalKind::Failed => "failed",
            RepairTerminalKind::Oversized => "oversized",
        };
        let stored: (i64, u32, String, i64, String, i64) = resources
            .read(|tx| {
                Ok(tx
                    .query_row("SELECT * FROM d_repair_diagnoses", [], |r| {
                        Ok((
                            r.get(0)?,
                            r.get(1)?,
                            r.get(2)?,
                            r.get(3)?,
                            r.get(4)?,
                            r.get(5)?,
                        ))
                    })
                    .unwrap())
            })
            .unwrap();
        assert_eq!(
            stored,
            (
                1,
                sb_mime::PARSER_VERSION,
                object.sha256_hex(),
                object.byte_length() as i64,
                text.into(),
                1000
            )
        );
        assert!(block_on(resources.begin_repair(1)).unwrap().is_none());
        assert!(matches!(
            resources.prepare_repair(pending, &|| false).unwrap(),
            RepairPreparation::Current
        ));
        edit(
            &resources,
            "CREATE TEMP TRIGGER no_diagnosis_update BEFORE UPDATE ON main.d_repair_diagnoses BEGIN SELECT RAISE(ABORT,'immutable diagnosis'); END",
        );
        let changes = writer_changes(&resources);
        assert_eq!(
            block_on(resources.persist_repair_terminal(duplicate)).unwrap(),
            RepairPersist::Current
        );
        assert_eq!(writer_changes(&resources), changes);
        assert_eq!((fs.calls(FsOp::Open), fs.calls(FsOp::Read)), io);
        assert_eq!(snapshot(&resources), before);
        assert_eq!(resources.repaired_summary(1).unwrap(), None);
        resources.close().unwrap();
        let resources = Resources::open_on(
            &scratch.path().join("store"),
            fs.clone(),
            Arc::new(sb_test_support::FixedClock::new(Timestamp::from_millis(
                2000,
            ))),
        )
        .unwrap();
        let io = (fs.calls(FsOp::Open), fs.calls(FsOp::Read));
        assert!(block_on(resources.begin_repair(1)).unwrap().is_none());
        assert_eq!(
            resources.repair_status(1).unwrap().diagnosis,
            Some(diagnosis)
        );
        assert_eq!((fs.calls(FsOp::Open), fs.calls(FsOp::Read)), io);
        assert_eq!(snapshot(&resources), before);
        resources.close().unwrap();
    }
}

#[test]
fn diagnosis_fences_and_kind_conflicts_fail_closed_without_replacing_evidence() {
    for mutation in [
        "source_sha256=printf('%064d',0)",
        "source_bytes=source_bytes+1",
        "kind='partial'",
    ] {
        let (_scratch, resources, _) = native(b"", real_fs());
        let first = terminal(&resources);
        let second = terminal(&resources);
        let pending = capture(&resources);
        assert_eq!(
            block_on(resources.persist_repair_terminal(first)).unwrap(),
            RepairPersist::Diagnosed
        );
        edit(
            &resources,
            &format!("UPDATE d_repair_diagnoses SET {mutation}"),
        );
        let before = writer_changes(&resources);
        assert!(matches!(
            block_on(resources.persist_repair_terminal(second)),
            Err(StoreError::Corrupt { .. })
        ));
        assert_eq!(writer_changes(&resources), before);
        if !mutation.starts_with("kind") {
            assert!(matches!(
                block_on(resources.begin_repair(1)),
                Err(StoreError::Corrupt { .. })
            ));
            assert!(matches!(
                resources.prepare_repair(pending, &|| false),
                Err(StoreError::Corrupt { .. })
            ));
            assert!(matches!(
                resources.repair_status(1),
                Err(StoreError::Corrupt { .. })
            ));
        }
        assert_eq!(
            number(&resources, "SELECT count(*) FROM d_repair_diagnoses"),
            1
        );
        resources.close().unwrap();
    }
}

#[test]
fn persistent_attempts_refence_source_target_and_newer_base_inside_the_writer() {
    for (mutation, expected) in [
        (
            "UPDATE acquisitions SET object_state='missing'",
            RepairPersist::Recapture,
        ),
        (
            "UPDATE acquisitions SET object_state='quarantined'",
            RepairPersist::Recapture,
        ),
        (
            "DELETE FROM d_message_summary; DELETE FROM acquisitions",
            RepairPersist::Recapture,
        ),
        (
            "UPDATE store_meta SET value='1' WHERE key='mime_repair_target'",
            RepairPersist::Recapture,
        ),
        (
            "UPDATE d_message_summary SET parser_version=2,parse_state='ok'",
            RepairPersist::Current,
        ),
        (
            "UPDATE d_message_summary SET parser_version=3,parse_state='partial'",
            RepairPersist::Current,
        ),
        (
            "UPDATE d_message_summary SET parser_version=3,parse_state='failed'",
            RepairPersist::Current,
        ),
    ] {
        let (_scratch, resources, _) = native(b"", real_fs());
        let attempt = terminal(&resources);
        edit(&resources, mutation);
        let before = snapshot(&resources);
        assert_eq!(
            block_on(resources.persist_repair_terminal(attempt)).unwrap(),
            expected
        );
        assert_eq!(
            number(&resources, "SELECT count(*) FROM d_repair_diagnoses"),
            0
        );
        assert_eq!(snapshot(&resources), before);
        resources.close().unwrap();
    }
    for mutation in [
        "UPDATE acquisitions SET byte_length=byte_length+1",
        "UPDATE acquisitions SET content_sha256=printf('%064d',0)",
        "UPDATE store_meta SET value='3' WHERE key='mime_repair_target'",
        "UPDATE store_meta SET value='0' WHERE key='mime_repair_target'",
        "UPDATE store_meta SET value='bad' WHERE key='mime_repair_target'",
        "DELETE FROM store_meta WHERE key='mime_repair_target'",
    ] {
        let (_scratch, resources, _) = native(b"", real_fs());
        let attempt = terminal(&resources);
        edit(&resources, mutation);
        let error = block_on(resources.persist_repair_terminal(attempt)).unwrap_err();
        assert!(if mutation.contains("value='3'") {
            matches!(error, StoreError::Unavailable { io_kind: None, .. })
        } else {
            matches!(error, StoreError::Corrupt { .. })
        });
        assert_eq!(
            number(&resources, "SELECT count(*) FROM d_repair_diagnoses"),
            0
        );
        resources.close().unwrap();
    }
}

#[test]
fn recorded_diagnoses_do_not_hide_unavailable_sources_or_other_targets() {
    let fs = Arc::new(FaultFs::new(
        real_fs(),
        FsOp::Read,
        u64::MAX,
        FaultKind::Eio,
    ));
    let (_scratch, resources, _) = native(b"", fs.clone());
    assert_eq!(
        block_on(resources.persist_repair_terminal(terminal(&resources))).unwrap(),
        RepairPersist::Diagnosed
    );
    let before = snapshot(&resources);
    for state in ["missing", "quarantined"] {
        edit(
            &resources,
            &format!("UPDATE acquisitions SET object_state='{state}'"),
        );
        let io = (fs.calls(FsOp::Open), fs.calls(FsOp::Read));
        let error = resources
            .prepare_repair(capture(&resources), &|| false)
            .unwrap_err();
        assert!(if state == "missing" {
            matches!(error, StoreError::NotFound { .. })
        } else {
            matches!(error, StoreError::Corrupt { .. })
        });
        assert_eq!((fs.calls(FsOp::Open), fs.calls(FsOp::Read)), io);
        assert!(resources.repair_status(1).unwrap().diagnosis.is_some());
    }
    edit(
        &resources,
        "UPDATE acquisitions SET object_state='published'; UPDATE store_meta SET value='3' WHERE key='mime_repair_target'",
    );
    assert_eq!(resources.repair_status(1).unwrap().diagnosis, None);
    assert!(matches!(
        block_on(resources.begin_repair(1)),
        Err(StoreError::Unavailable { .. })
    ));
    edit(
        &resources,
        "UPDATE store_meta SET value='2' WHERE key='mime_repair_target'; UPDATE d_repair_diagnoses SET parser_target=1",
    );
    assert_eq!(resources.repair_status(1).unwrap().diagnosis, None);
    assert_eq!(
        block_on(resources.persist_repair_terminal(terminal(&resources))).unwrap(),
        RepairPersist::Diagnosed
    );
    assert_eq!(
        number(&resources, "SELECT count(*) FROM d_repair_diagnoses"),
        2
    );
    let object = resources.store.objects().publish(RICH).unwrap();
    write(&resources, move |tx| {
        tx.execute(
            "INSERT INTO acquisitions VALUES(2,?1,?2,'published',0)",
            (object.sha256_hex(), object.byte_length() as i64),
        )
        .unwrap();
        Ok(())
    });
    let capture = block_on(resources.begin_repair(2)).unwrap().unwrap();
    assert!(matches!(
        resources.prepare_repair(capture, &|| false).unwrap(),
        RepairPreparation::Prepared(_)
    ));
    assert_eq!(snapshot(&resources), before);
    resources.close().unwrap();
}

#[test]
fn failed_diagnosis_statement_and_commit_never_return_a_saved_receipt() {
    for commit_failure in [false, true] {
        let (_scratch, resources, _) = native(b"", real_fs());
        let attempt = terminal(&resources);
        let before = snapshot(&resources);
        edit(
            &resources,
            if commit_failure {
                "CREATE TEMP TABLE diagnosis_parent(id INTEGER PRIMARY KEY); CREATE TEMP TABLE diagnosis_probe(id INTEGER REFERENCES diagnosis_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE TEMP TRIGGER diagnosis_cut AFTER INSERT ON main.d_repair_diagnoses BEGIN INSERT INTO diagnosis_probe VALUES(1); END"
            } else {
                "CREATE TEMP TRIGGER diagnosis_cut BEFORE INSERT ON main.d_repair_diagnoses BEGIN SELECT RAISE(ABORT,'fixture'); END"
            },
        );
        let error = block_on(resources.persist_repair_terminal(attempt)).unwrap_err();
        assert!(
            matches!(error, StoreError::Internal { ref detail } if detail.contains(if commit_failure { "commit a write transaction" } else { "MIME repair publication" })),
            "{error:?}"
        );
        assert_eq!(
            number(&resources, "SELECT count(*) FROM d_repair_diagnoses"),
            0
        );
        assert!(resources.repair_status(1).unwrap().diagnosis.is_none());
        assert_eq!(snapshot(&resources), before);
        edit(&resources, "DROP TRIGGER diagnosis_cut");
        assert_eq!(
            block_on(resources.persist_repair_terminal(terminal(&resources))).unwrap(),
            RepairPersist::Diagnosed
        );
        assert_eq!(
            number(&resources, "SELECT count(*) FROM d_repair_diagnoses"),
            1
        );
        assert_eq!(snapshot(&resources), before);
        resources.close().unwrap();
    }
}

#[test]
fn io_cancel_availability_and_corrupt_oversized_reads_stay_typed_and_unpublished() {
    let counting = Arc::new(FaultFs::new(
        real_fs(),
        FsOp::Read,
        u64::MAX,
        FaultKind::Eio,
    ));
    let (_scratch, resources, _) = native(RICH, counting.clone());
    let opens = counting.calls(FsOp::Open);
    assert!(matches!(
        resources
            .prepare_repair(capture(&resources), &|| true)
            .unwrap(),
        RepairPreparation::Cancelled
    ));
    assert_eq!(counting.calls(FsOp::Open), opens);
    let calls = AtomicUsize::new(0);
    assert!(matches!(
        resources
            .prepare_repair(capture(&resources), &|| calls
                .fetch_add(1, Ordering::SeqCst)
                >= 3)
            .unwrap(),
        RepairPreparation::Cancelled
    ));
    for state in ["missing", "quarantined"] {
        write(&resources, move |tx| {
            tx.execute("UPDATE acquisitions SET object_state=?1", [state])
                .unwrap();
            Ok(())
        });
        let before = counting.calls(FsOp::Open);
        let error = resources
            .prepare_repair(capture(&resources), &|| false)
            .unwrap_err();
        assert!(if state == "missing" {
            matches!(error, StoreError::NotFound { .. })
        } else {
            matches!(error, StoreError::Corrupt { .. })
        });
        assert_eq!(counting.calls(FsOp::Open), before);
    }
    resources.close().unwrap();
    let (scratch, resources, object) = native(RICH, real_fs());
    std::fs::remove_file(object_path(&scratch, &object)).unwrap();
    assert!(matches!(
        resources.prepare_repair(capture(&resources), &|| false),
        Err(StoreError::NotFound { .. })
    ));
    resources.close().unwrap();
    for fault in [FaultKind::Eio, FaultKind::NoSpace] {
        let fs = Arc::new(FaultFs::new(real_fs(), FsOp::Read, 1, fault));
        let (_scratch, resources, _) = native(RICH, fs);
        assert!(matches!(
            resources.prepare_repair(capture(&resources), &|| false),
            Err(StoreError::Unavailable {
                io_kind: Some(_),
                ..
            })
        ));
        resources.close().unwrap();
    }
    let bytes = vec![b'x'; sb_mime::MAX_MESSAGE_BYTES + 1];
    let (scratch, resources, object) = native(&bytes, real_fs());
    let mut file = std::fs::OpenOptions::new()
        .write(true)
        .open(object_path(&scratch, &object))
        .unwrap();
    file.seek(SeekFrom::End(-1)).unwrap();
    file.write_all(b"y").unwrap();
    drop(file);
    assert!(matches!(
        resources.prepare_repair(capture(&resources), &|| false),
        Err(StoreError::Corrupt { .. })
    ));
    assert_eq!(
        number(&resources, "SELECT count(*) FROM d_repair_assemblies"),
        0
    );
    resources.close().unwrap();
}

#[test]
fn native_statement_and_commit_failures_return_no_advanced_publication() {
    for commit_failure in [false, true] {
        let (_scratch, resources, _) = native(RICH, real_fs());
        let before = snapshot(&resources);
        let pending = prepare(&resources);
        write(&resources, move |tx| {
            if commit_failure {
                tx.execute_batch("CREATE TEMP TABLE cut_parent(id INTEGER PRIMARY KEY); CREATE TEMP TABLE cut_probe(id INTEGER REFERENCES cut_parent(id) DEFERRABLE INITIALLY DEFERRED); CREATE TEMP TRIGGER repair_cut AFTER UPDATE OF completed_at ON main.d_repair_assemblies BEGIN INSERT INTO cut_probe VALUES(1); END").unwrap();
            } else {
                tx.execute_batch("CREATE TEMP TRIGGER repair_cut BEFORE INSERT ON main.d_repair_pieces WHEN NEW.field_index>0 BEGIN SELECT RAISE(ABORT,'fixture'); END").unwrap();
            }
            Ok(())
        });
        assert!(block_on(resources.commit_repair_step(pending)).is_err());
        assert_eq!(resources.repaired_summary(1).unwrap(), None);
        assert_eq!(
            number(&resources, "SELECT count(*) FROM d_repair_assemblies"),
            0
        );
        assert_eq!(
            number(&resources, "SELECT count(*) FROM d_repair_pieces"),
            0
        );
        assert_eq!(snapshot(&resources), before);
        edit(&resources, "DROP TRIGGER repair_cut");
        finish(&resources, prepare(&resources));
        assert_eq!(resources.repaired_summary(1).unwrap(), Some(expected(RICH)));
        assert_eq!(snapshot(&resources), before);
        resources.close().unwrap();
    }
}
