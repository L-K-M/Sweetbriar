//! Native metadata snapshots, independent of scope selection and object I/O.

use std::io::{self, Write};

use sb_resources::{
    ExportMetadata, Resources, StoreError, Transaction, WritePriority, export_occurrence_metadata,
};
use sb_test_support::{ScratchDir, block_on};
use serde_json::{Value, json};

const EMPTY_HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const ORIGINAL_NOTE: &str = "private note";
const SOURCE: i64 = 1;
const UNACQUIRED: i64 = 2;
const OTHER_COPY: i64 = 3;

fn write<T: Send + 'static>(
    res: &Resources,
    f: impl FnOnce(&Transaction) -> Result<T, StoreError> + Send + 'static,
) -> T {
    block_on(res.write(WritePriority::User, f)).unwrap()
}

fn fixture(label: &str) -> (ScratchDir, Resources, String) {
    let scratch = ScratchDir::new(label);
    let res = Resources::open(&scratch.path().join("store")).unwrap();
    let account: String = res
        .read(|tx| {
            Ok(tx
                .query_row(
                    "SELECT account_id FROM accounts WHERE provider='local_archive'",
                    [],
                    |r| r.get(0),
                )
                .unwrap())
        })
        .unwrap();
    let id = account.clone();
    write(&res, move |tx| {
        tx.execute(
            "UPDATE accounts SET display_name='Archive α' WHERE account_id=?1",
            [&id],
        )
        .unwrap();
        tx.execute(
            "INSERT INTO mailboxes (mailbox_id,account_id,remote_name,kind,selectable,sync_state)
                    VALUES (1,?1,'Inbox','folder',1,'synced'), (2,?1,'Other','folder',1,'synced')",
            [&id],
        )
        .unwrap();
        tx.execute("INSERT INTO acquisitions (acquisition_id,content_sha256,byte_length,object_state,first_acquired_at)
                    VALUES (1,?1,0,'published',1)", [EMPTY_HASH]).unwrap();
        tx.execute(
            "INSERT INTO logical_messages (message_id,account_id,acquisition_id,created_at)
                    VALUES (1,?1,1,1), (2,?1,NULL,1)",
            [&id],
        )
        .unwrap();
        tx.execute("INSERT INTO occurrences
                    (occurrence_id,account_id,mailbox_id,message_id,f_seen,f_flagged,f_answered,f_draft,f_deleted,
                     keywords,state,first_seen_at,last_seen_at) VALUES
                    (1,?1,1,1,1,0,1,0,1,'[\"$Forwarded\",\"custom\"]','present',1,1),
                    (2,?1,1,2,0,1,0,1,0,'[]','present',1,1),
                    (3,?1,2,1,0,0,0,0,0,'[]','vanished',1,1)", [&id]).unwrap();
        // A reader that fetches headers or object bytes would fail this fixture.
        tx.execute_batch("INSERT INTO message_headers (message_id,raw_fields,from_json,fetched_at)
                          VALUES (1,x'00','not-json',1);
                          INSERT INTO tags (tag_id,name,color_srgb) VALUES ('z','Zulu','#abcdef'), ('a','Alpha',NULL);
                          INSERT INTO message_tags (message_id,tag_id,added_at) VALUES (1,'z',1),(1,'a',1);").unwrap();
        tx.execute(
            "INSERT INTO notes (note_id,message_id,body_source,created_at,updated_at)
                    VALUES ('note',1,?1,1,1)",
            [ORIGINAL_NOTE],
        )
        .unwrap();
        Ok(())
    });
    (scratch, res, account)
}

fn snapshot(res: &Resources, id: i64) -> ExportMetadata {
    res.read(|tx| export_occurrence_metadata(tx, id)).unwrap()
}

fn mutate(res: &Resources, sql: &'static str) {
    write(res, move |tx| {
        tx.execute_batch(sql).unwrap();
        Ok(())
    });
}

#[test]
fn snapshot_has_exact_occurrence_metadata_without_headers_files_or_other_copies() {
    let (_scratch, res, account) = fixture("export-metadata-exact");
    let frozen = snapshot(&res, SOURCE);
    assert_eq!(
        serde_json::to_value(&frozen).unwrap(),
        json!({
            "v":1, "occurrence_id":1, "message_id":1,
            "account":{"id":account,"name":"Archive α"}, "mailbox":{"id":1,"name":"Inbox"},
            "occurrence_state":"present",
            "flags":{"seen":true,"flagged":false,"answered":true,"draft":false,"deleted":true},
            "keywords":["$Forwarded","custom"],
            "acquisition":{"id":1,"sha256":EMPTY_HASH,"bytes":0}, "body_availability":"acquired",
            "tags":[{"id":"a","name":"Alpha","color_srgb":null},{"id":"z","name":"Zulu","color_srgb":"#abcdef"}],
            "note":ORIGINAL_NOTE
        })
    );
    let first = serde_json::to_vec(&frozen).unwrap();
    assert_eq!(serde_json::to_vec(&frozen).unwrap(), first);
    let other = serde_json::to_value(snapshot(&res, OTHER_COPY)).unwrap();
    assert_eq!(other["message_id"], 1);
    assert_eq!(other["occurrence_id"], OTHER_COPY);
    assert_eq!(other["mailbox"], json!({"id":2,"name":"Other"}));
    assert_eq!(other["occurrence_state"], "vanished");
    assert!(other.get("occurrences").is_none());
}

#[test]
fn unacquired_missing_and_quarantined_are_recorded_without_substituting_bodies() {
    let (_scratch, res, _) = fixture("export-metadata-availability");
    let value = serde_json::to_value(snapshot(&res, UNACQUIRED)).unwrap();
    assert_eq!(value["acquisition"], Value::Null);
    assert_eq!(value["body_availability"], "not_acquired");
    assert_eq!(value["note"], Value::Null);
    assert_eq!(value["tags"], json!([]));
    assert_eq!(
        value["flags"],
        json!({"seen":false,"flagged":true,"answered":false,"draft":true,"deleted":false})
    );
    for (sql, expected) in [
        (
            "UPDATE acquisitions SET object_state='missing' WHERE acquisition_id=1",
            "missing",
        ),
        (
            "UPDATE acquisitions SET object_state='quarantined' WHERE acquisition_id=1",
            "quarantined",
        ),
    ] {
        mutate(&res, sql);
        let value = serde_json::to_value(snapshot(&res, SOURCE)).unwrap();
        assert_eq!(value["body_availability"], expected);
        assert_eq!(value["acquisition"]["sha256"], EMPTY_HASH);
    }
}

#[test]
fn all_retained_states_and_large_tag_collections_are_preserved() {
    const EXTRA_TAGS: usize = 1_201;
    let (_scratch, res, _) = fixture("export-metadata-retained");
    for state in [
        "present",
        "vanish_pending",
        "vanished",
        "epoch_stale",
        "superseded",
    ] {
        write(&res, move |tx| {
            tx.execute(
                "UPDATE occurrences SET state=?1 WHERE occurrence_id=1",
                [state],
            )
            .unwrap();
            Ok(())
        });
        assert_eq!(
            serde_json::to_value(snapshot(&res, SOURCE)).unwrap()["occurrence_state"],
            state
        );
    }
    write(&res, |tx| {
        for index in 0..EXTRA_TAGS {
            let id = format!("extra-{index}");
            let name = format!("Extra-{index:04}-{}", "x".repeat(2_000));
            tx.execute(
                "INSERT INTO tags (tag_id,name) VALUES (?1,?2)",
                (&id, &name),
            )
            .unwrap();
            tx.execute(
                "INSERT INTO message_tags (message_id,tag_id,added_at) VALUES (1,?1,1)",
                [&id],
            )
            .unwrap();
        }
        Ok(())
    });
    let value = serde_json::to_value(snapshot(&res, SOURCE)).unwrap();
    let tags = value["tags"].as_array().unwrap();
    assert_eq!(tags.len(), EXTRA_TAGS + 2);
    assert_eq!(tags[0]["name"], "Alpha");
    assert_eq!(tags[1]["name"], format!("Extra-0000-{}", "x".repeat(2_000)));
    assert_eq!(tags.last().unwrap()["name"], "Zulu");
}

#[test]
fn metadata_stays_frozen_after_source_edits_and_across_reopen() {
    let (scratch, res, _) = fixture("export-metadata-frozen");
    let frozen = snapshot(&res, SOURCE);
    let before = serde_json::to_vec(&frozen).unwrap();
    mutate(&res, "UPDATE accounts SET display_name='Changed';
                  UPDATE mailboxes SET remote_name='Renamed' WHERE mailbox_id=1;
                  UPDATE occurrences SET f_seen=0,state='superseded',keywords='[]' WHERE occurrence_id=1;
                  UPDATE notes SET body_source='changed note' WHERE message_id=1;
                  UPDATE tags SET name='Changed tag' WHERE tag_id='a';
                  DELETE FROM message_tags WHERE message_id=1 AND tag_id='z';
                  UPDATE acquisitions SET object_state='missing' WHERE acquisition_id=1;");
    assert_eq!(serde_json::to_vec(&frozen).unwrap(), before);
    let after = serde_json::to_vec(&snapshot(&res, SOURCE)).unwrap();
    assert_ne!(after, before);
    res.close().unwrap();
    let res = Resources::open(&scratch.path().join("store")).unwrap();
    assert_eq!(serde_json::to_vec(&frozen).unwrap(), before);
    assert_eq!(serde_json::to_vec(&snapshot(&res, SOURCE)).unwrap(), after);
    res.close().unwrap();
}

#[derive(Default)]
struct CountedWriter {
    bytes: usize,
}

impl Write for CountedWriter {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes += bytes.len();
        Ok(bytes.len())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_multimegabyte_note_streams_without_clipping_or_normalizing_text() {
    const TWO_MIB: usize = 2_097_152;
    let (_scratch, res, _) = fixture("export-metadata-large-note");
    let mut baseline = CountedWriter::default();
    serde_json::to_writer(&mut baseline, &snapshot(&res, SOURCE)).unwrap();
    let note = "Large note ✓ ".repeat(200_000);
    assert!(note.len() > TWO_MIB);
    let length = note.len();
    write(&res, move |tx| {
        tx.execute("UPDATE notes SET body_source=?1 WHERE message_id=1", [note])
            .unwrap();
        Ok(())
    });
    let large = snapshot(&res, SOURCE);
    let mut sink = CountedWriter::default();
    serde_json::to_writer(&mut sink, &large).unwrap();
    assert_eq!(sink.bytes, baseline.bytes + length - ORIGINAL_NOTE.len());
    let value = serde_json::to_value(&large).unwrap();
    assert_eq!(
        value["note"].as_str().unwrap(),
        "Large note ✓ ".repeat(200_000)
    );
    let note = "quote: \"; slash: \\; line\nnull\0end";
    write(&res, move |tx| {
        tx.execute("UPDATE notes SET body_source=?1 WHERE message_id=1", [note])
            .unwrap();
        Ok(())
    });
    assert_eq!(
        serde_json::to_value(snapshot(&res, SOURCE)).unwrap()["note"],
        note
    );
}

#[test]
fn unknown_ids_and_malformed_stored_fields_fail_typed() {
    let (_scratch, res, _) = fixture("export-metadata-unknown");
    for id in [-1, 0, 99] {
        assert!(matches!(
            res.read(|tx| export_occurrence_metadata(tx, id)),
            Err(StoreError::NotFound { .. })
        ));
    }
    for mutation in [
        "UPDATE occurrences SET f_seen=2 WHERE occurrence_id=1",
        "UPDATE occurrences SET state='unknown' WHERE occurrence_id=1",
        "UPDATE occurrences SET keywords='{}' WHERE occurrence_id=1",
        "UPDATE occurrences SET keywords='[1]' WHERE occurrence_id=1",
        "UPDATE occurrences SET keywords='[\"z\",\"a\"]' WHERE occurrence_id=1",
        "UPDATE occurrences SET keywords='[\"a\",\"a\"]' WHERE occurrence_id=1",
        "UPDATE occurrences SET keywords=CAST(x'ff' AS TEXT) WHERE occurrence_id=1",
        "UPDATE acquisitions SET content_sha256='broken' WHERE acquisition_id=1",
        "UPDATE acquisitions SET byte_length=-1 WHERE acquisition_id=1",
        "UPDATE tags SET color_srgb='#ABCDEF' WHERE tag_id='a'",
    ] {
        let (_scratch, res, _) = fixture("export-metadata-corrupt");
        write(&res, move |tx| {
            tx.execute_batch("PRAGMA ignore_check_constraints=ON")
                .unwrap();
            tx.execute_batch(mutation).unwrap();
            tx.execute_batch("PRAGMA ignore_check_constraints=OFF")
                .unwrap();
            Ok(())
        });
        let result = res.read(|tx| export_occurrence_metadata(tx, SOURCE));
        let Err(error) = result else {
            panic!("{mutation} was accepted");
        };
        assert!(
            matches!(error, StoreError::Corrupt { .. }),
            "{mutation}: {error}"
        );
    }
}

struct FailedWriter;
impl Write for FailedWriter {
    fn write(&mut self, _: &[u8]) -> io::Result<usize> {
        Err(io::ErrorKind::BrokenPipe.into())
    }
    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn failed_serialization_does_not_change_source_or_return_success() {
    let (_scratch, res, _) = fixture("export-metadata-write-error");
    let frozen = snapshot(&res, SOURCE);
    let error = serde_json::to_writer(FailedWriter, &frozen).unwrap_err();
    assert_eq!(error.io_error_kind(), Some(io::ErrorKind::BrokenPipe));
    assert_eq!(
        serde_json::to_vec(&frozen).unwrap(),
        serde_json::to_vec(&snapshot(&res, SOURCE)).unwrap()
    );
}
