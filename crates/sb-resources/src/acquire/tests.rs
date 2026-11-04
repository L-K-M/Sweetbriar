//! Tests for import acquisition (M1.2c2b): exact publication, bounded
//! summaries and flags, and resumable positions over real snapshots in
//! `ScratchDir` stores, plus the `fixtures/import` corpus. No owner data.

use std::fs;
use std::io::{Cursor, Read};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

use sb_store::WritePriority;
use sb_store::imports::{
    ImportBatch, ImportJobState, advance_import_job, commit_import_batch, create_import_job,
    record_import_source,
};
use sb_test_support::{ScratchDir, block_on};

use super::*;
use crate::{ImportInventoryOutcome, Resources};

const MBOXO_SHA256: &str = "69afa20c11a485abc5a04cace01ff8b167658ea89ac7d3e3a31f0fc709cca413";
const MBOXRD_SHA256: &str = "1786e24a2decd0fa5fa2b4f3d22faecdc36357d8b117bb6e083a2fe68a329873";
const ENVELOPE: &str = "From sender@example.test Sat Oct 17 12:00:00 2026\n";
const RICH: &[u8] = b"Message-ID: <m1@example.test>\r\n\
Date: Sat, 17 Oct 2026 12:00:00 +0000\r\n\
From: \"Sender Name\" <sender@example.test>\r\n\
To: Reader <reader@example.test>, other@example.test\r\n\
Cc: cc@example.test\r\n\
Reply-To: reply@example.test\r\n\
In-Reply-To: <parent@example.test>\r\n\
References: <root@example.test> <parent@example.test>\r\n\
List-ID: <list.example.test>\r\n\
Delivered-To: me@example.test\r\n\
Subject: Rich summary\r\n\
X-Status: RD\r\n\
\r\n\
Hello body.\r\n";
const JOB: &str = "00000000-0000-4000-8000-0000000000c2";

fn fixtures() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/import")
}

fn resources(label: &str) -> (ScratchDir, Resources) {
    let scratch = ScratchDir::new(label);
    let resources = Resources::open(&scratch.path().join("store")).unwrap();
    (scratch, resources)
}

/// The one source a complete, problem-free inventory of `path` finds.
fn source(res: &Resources, path: &Path) -> SourceInventory {
    let outcome =
        res.inventory_import_sources(&[path.to_path_buf()], |_| ControlFlow::Continue(()));
    let ImportInventoryOutcome::Complete(inventory) = outcome else {
        panic!("the inventory was cancelled");
    };
    assert!(
        inventory.problems().is_empty(),
        "{:?}",
        inventory.problems()
    );
    let [source] = inventory.sources() else {
        panic!("one source expected: {:?}", inventory.sources());
    };
    source.clone()
}

/// Three messages; the middle one carries `Status: O`, so it is unread.
fn three_message_mbox() -> Vec<u8> {
    format!(
        "{ENVELOPE}Subject: one\n\nfirst\n{ENVELOPE}Subject: two\nStatus: O\n\nsecond\n{ENVELOPE}Subject: three\n\nthird\n"
    )
    .into_bytes()
}

fn acquire_from(
    res: &Resources,
    source: &SourceInventory,
    variant: Option<MboxVariant>,
    position: SourcePosition,
) -> Vec<AcquiredMessage> {
    let mut acquisition = res
        .open_import_acquisition(source, variant, position)
        .unwrap();
    let mut messages = Vec::new();
    while let Some(message) = res.acquire_import_message(&mut acquisition).unwrap() {
        assert_eq!(acquisition.position(), message.next_position());
        messages.push(message);
    }
    // A consumed source stays consumed.
    assert!(
        res.acquire_import_message(&mut acquisition)
            .unwrap()
            .is_none()
    );
    messages
}

fn published(res: &Resources, message: &AcquiredMessage) -> Vec<u8> {
    let mut bytes = Vec::new();
    res.store
        .objects()
        .open(message.message().object.id())
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    bytes
}

#[test]
fn fixture_variants_publish_their_expected_bytes() {
    let (_scratch, res) = resources("variants");
    let quoting = source(&res, &fixtures().join("quoting.mbox"));
    let error = res
        .open_import_acquisition(&quoting, None, SourcePosition::START)
        .unwrap_err();
    assert!(
        matches!(error, StoreError::InvalidInput { .. }),
        "{error:?}"
    );

    let cases = [
        (MboxVariant::Mboxo, "expected/mboxo.eml", MBOXO_SHA256),
        (MboxVariant::Mboxrd, "expected/mboxrd.eml", MBOXRD_SHA256),
    ];
    for (variant, expected, sha256) in cases {
        let messages = acquire_from(&res, &quoting, Some(variant), SourcePosition::START);
        let [message] = messages.as_slice() else {
            panic!("one message expected: {messages:?}");
        };
        assert_eq!(
            published(&res, message),
            fs::read(fixtures().join(expected)).unwrap()
        );
        let import = message.message();
        assert_eq!(import.object.id().sha256_hex(), sha256);
        assert_eq!(import.offset, 0);
        assert_eq!(import.source_path, quoting.path().to_str().unwrap());
        assert_eq!(import.source_sha256, quoting.snapshot().sha256_hex());
        // `Status: RO` and `X-Status: FA`.
        assert!(matches!(
            import.flags.read,
            ImportReadState::FromHeaders { seen: true }
        ));
        assert!(import.flags.flagged && import.flags.answered);
        assert!(!import.flags.deleted && !import.flags.draft);
        assert_eq!(message.flag_extraction(), FlagExtraction::Complete);
        assert_eq!(
            message.next_position(),
            SourcePosition::new(quoting.snapshot().byte_length(), 1)
        );
    }
}

#[test]
fn identical_messages_share_an_object_but_keep_their_offsets() {
    let (_scratch, res) = resources("duplicates");
    let inbox = source(&res, &fixtures().join("Inbox.mbox"));
    let messages = acquire_from(&res, &inbox, None, SourcePosition::START);
    let [first, second] = messages.as_slice() else {
        panic!("two messages expected: {messages:?}");
    };
    assert_eq!(first.message().object.id(), second.message().object.id());
    assert_eq!(first.message().offset, 0);
    assert_eq!(second.message().offset, first.next_position().offset());
    assert!(second.message().offset > 0);
    assert_eq!(
        second.next_position(),
        SourcePosition::new(inbox.snapshot().byte_length(), 2)
    );
    // No status headers: the archive default applies (spec/09).
    for message in &messages {
        assert!(matches!(
            message.message().flags.read,
            ImportReadState::ArchiveDefault
        ));
        assert_eq!(
            published(&res, message),
            b"From: sender@example.test\nSubject: Duplicate source message\n\nKeep both occurrences.\n"
        );
    }
}

#[test]
fn summaries_and_flags_map_onto_row_values() {
    let (scratch, res) = resources("summary");
    let path = scratch.path().join("Rich.eml");
    fs::write(&path, RICH).unwrap();
    let eml = source(&res, &path);
    let messages = acquire_from(&res, &eml, None, SourcePosition::START);
    let [message] = messages.as_slice() else {
        panic!("one message expected: {messages:?}");
    };
    assert_eq!(published(&res, message), RICH);
    let import = message.message();
    assert!(matches!(import.source_kind, ImportSourceKind::Eml));
    assert_eq!(import.offset, 0);
    // `X-Status: RD` alone decides read state and the deleted flag.
    assert!(matches!(
        import.flags.read,
        ImportReadState::FromHeaders { seen: true }
    ));
    assert!(import.flags.deleted && !import.flags.flagged);

    let summary = &import.summary;
    assert!(matches!(summary.parse_state, ImportParseState::Parsed));
    assert_eq!(summary.parser_version, sb_mime::PARSER_VERSION);
    assert_eq!(
        summary.header_message_id.as_deref(),
        Some("m1@example.test")
    );
    assert_eq!(summary.subject.as_deref(), Some("Rich summary"));
    assert_eq!(summary.from_addr.as_deref(), Some("sender@example.test"));
    assert_eq!(summary.from_name.as_deref(), Some("Sender Name"));
    assert_eq!(
        summary.to_json.as_deref(),
        Some(
            r#"[{"address":"reader@example.test","name":"Reader"},{"address":"other@example.test","name":null}]"#
        )
    );
    assert_eq!(
        summary.cc_json.as_deref(),
        Some(r#"[{"address":"cc@example.test","name":null}]"#)
    );
    assert_eq!(
        summary.reply_to_json.as_deref(),
        Some(r#"[{"address":"reply@example.test","name":null}]"#)
    );
    assert_eq!(summary.date_header, Some(1_792_238_400_000));
    assert_eq!(summary.in_reply_to.as_deref(), Some("parent@example.test"));
    assert_eq!(
        summary.references_json.as_deref(),
        Some(r#"["root@example.test","parent@example.test"]"#)
    );
    assert_eq!(summary.list_id.as_deref(), Some("list.example.test"));
    assert_eq!(
        summary.delivered_to_json.as_deref(),
        Some(r#"["me@example.test"]"#)
    );
    assert!(!summary.has_attachments);
    assert!(
        summary
            .snippet
            .as_deref()
            .unwrap()
            .starts_with("Hello body.")
    );
}

#[test]
fn acquired_messages_commit_as_import_batches() {
    let (scratch, res) = resources("commit");
    let path = scratch.path().join("Rich.eml");
    fs::write(&path, RICH).unwrap();
    let inbox = source(&res, &fixtures().join("Inbox.mbox"));
    let eml = source(&res, &path);
    let sources = [inbox, eml];
    let batches: Vec<Vec<ImportMessage>> = sources
        .iter()
        .map(|source| {
            acquire_from(&res, source, None, SourcePosition::START)
                .into_iter()
                .map(AcquiredMessage::into_message)
                .collect()
        })
        .collect();

    let now = res.now();
    let jsons: Vec<String> = sources.iter().map(SourceInventory::to_json).collect();
    let mut revision = block_on(res.write(WritePriority::Sync, move |tx| {
        create_import_job(tx, JOB, "{\"v\":1}", now)?;
        for (ordinal, json) in jsons.iter().enumerate() {
            record_import_source(tx, JOB, ordinal as u64, json)?;
        }
        advance_import_job(
            tx,
            JOB,
            1,
            ImportJobState::AwaitingConfirmation,
            "{\"v\":1}",
            now,
        )?;
        Ok(advance_import_job(tx, JOB, 2, ImportJobState::Running, "{\"v\":1}", now)?.revision())
    }))
    .unwrap();
    let mut stats = Vec::new();
    for (ordinal, (source, messages)) in sources.iter().zip(batches).enumerate() {
        let mailbox = source.mailbox().to_owned();
        let expected_revision = revision;
        stats.push(
            block_on(res.write(WritePriority::Sync, move |tx| {
                let batch = ImportBatch {
                    job_id: JOB,
                    expected_revision,
                    source_ordinal: ordinal as u64,
                    mailbox_name: &mailbox,
                    messages: &messages,
                };
                commit_import_batch(tx, &batch, now, |_| {
                    Ok(("{\"v\":1}".to_owned(), "{\"v\":1}".to_owned()))
                })
            }))
            .unwrap(),
        );
        revision += 1;
    }
    assert_eq!((stats[0].imported, stats[0].duplicates), (2, 1));
    assert_eq!((stats[1].imported, stats[1].read_from_headers), (1, 1));

    let (acquisitions, occurrences, deleted, to_json, date) = res
        .read(|tx| {
            let count = |query: &str| sql(tx.query_row(query, [], |row| row.get::<_, i64>(0)));
            let (to_json, date) = sql(tx.query_row(
                "SELECT to_json, date_header FROM d_message_summary WHERE subject = 'Rich summary'",
                [],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
            ))?;
            Ok((
                count("SELECT count(*) FROM acquisitions")?,
                count("SELECT count(*) FROM occurrences")?,
                count("SELECT count(*) FROM occurrences WHERE f_deleted = 1")?,
                to_json,
                date,
            ))
        })
        .unwrap();
    assert_eq!((acquisitions, occurrences, deleted), (2, 3, 1));
    assert_eq!(
        to_json,
        r#"[{"address":"reader@example.test","name":"Reader"},{"address":"other@example.test","name":null}]"#
    );
    assert_eq!(date, 1_792_238_400_000);

    // Reader operations reach the stored import through item re-exports and
    // verify the original outside the metadata transaction.
    let (message, acquisition) = res
        .read(|tx| {
            let occurrence = sql(tx.query_row(
            "SELECT o.occurrence_id FROM occurrences o JOIN logical_messages m USING (message_id)
             JOIN d_message_summary s USING (acquisition_id) WHERE s.subject = 'Rich summary'",
            [], |row| row.get(0),
        ))?;
            let id = crate::message_for_occurrence(tx, occurrence)?;
            Ok((
                crate::get_message(tx, id)?,
                crate::best_acquisition(tx, id)?.unwrap(),
            ))
        })
        .unwrap();
    assert_eq!(message.account_display_name, "Local Archive");
    assert_eq!(message.headers.subject.as_deref(), Some("Rich summary"));
    assert_eq!(
        message.headers.to[0].address.as_deref(),
        Some("reader@example.test")
    );
    assert_eq!(message.body_availability, crate::BodyAvailability::Acquired);
    assert_eq!(res.read_acquisition(&acquisition).unwrap(), RICH);
}

/// sb-resources has no SQLite dependency of its own to name errors with.
fn sql<T, E: std::fmt::Display>(result: Result<T, E>) -> Result<T, StoreError> {
    result.map_err(|e| StoreError::internal(e.to_string()))
}

#[test]
fn oversized_messages_keep_exact_bytes_and_parse_partially() {
    let (scratch, res) = resources("oversized");
    let path = scratch.path().join("big.eml");
    let mut bytes = b"Subject: big\n\n".to_vec();
    bytes.resize(4096, b'x');
    fs::write(&path, &bytes).unwrap();
    let eml = source(&res, &path);
    let limits = Limits::default().with_max_message_bytes(64);
    let mut acquisition = res
        .open_import_acquisition(&eml, None, SourcePosition::START)
        .unwrap();
    let message = next(&res.store, &mut acquisition, &limits)
        .unwrap()
        .unwrap();
    assert_eq!(published(&res, &message), bytes);
    assert_eq!(message.message().object.id().byte_length(), 4096);
    assert!(matches!(
        message.message().summary.parse_state,
        ImportParseState::Partial
    ));
}

#[test]
fn unparseable_messages_are_still_published() {
    let (scratch, res) = resources("empty");
    let path = scratch.path().join("empty.eml");
    fs::write(&path, b"").unwrap();
    let eml = source(&res, &path);
    let messages = acquire_from(&res, &eml, None, SourcePosition::START);
    let [message] = messages.as_slice() else {
        panic!("one message expected: {messages:?}");
    };
    assert_eq!(message.message().object.id().byte_length(), 0);
    assert!(matches!(
        message.message().summary.parse_state,
        ImportParseState::Failed
    ));
    assert_eq!(message.next_position(), SourcePosition::new(0, 1));
}

#[test]
fn resumed_sources_continue_at_their_recorded_position() {
    let (scratch, res) = resources("resume");
    let path = scratch.path().join("Three.mbox");
    let bytes = three_message_mbox();
    fs::write(&path, &bytes).unwrap();
    let mbox = source(&res, &path);
    let all = acquire_from(&res, &mbox, None, SourcePosition::START);
    assert_eq!(all.len(), 3);
    assert!(matches!(
        all[1].message().flags.read,
        ImportReadState::FromHeaders { seen: false }
    ));

    let resumed = acquire_from(&res, &mbox, None, all[0].next_position());
    let identity = |m: &AcquiredMessage| {
        (
            *m.message().object.id(),
            m.message().offset,
            m.next_position(),
        )
    };
    assert_eq!(
        resumed.iter().map(identity).collect::<Vec<_>>(),
        all[1..].iter().map(identity).collect::<Vec<_>>()
    );
    let done = SourcePosition::new(bytes.len() as u64, 3);
    assert!(acquire_from(&res, &mbox, None, done).is_empty());

    // Positions that cannot come from this source are refused at open.
    let second = all[0].next_position().offset();
    let length = bytes.len() as u64;
    for (offset, messages) in [
        (0, 1),
        (second, 0),
        (length, 2),
        (second, 3),
        (length + 1, 3),
        (length, 4),
    ] {
        let error = res
            .open_import_acquisition(&mbox, None, SourcePosition::new(offset, messages))
            .unwrap_err();
        assert!(
            matches!(error, StoreError::InvalidInput { .. }),
            "{offset}/{messages}: {error:?}"
        );
    }
    // An offset inside a message is not a separator: corrupt, not skipped.
    let mut inside = res
        .open_import_acquisition(&mbox, None, SourcePosition::new(second + 1, 1))
        .unwrap();
    let error = res.acquire_import_message(&mut inside).unwrap_err();
    assert!(matches!(error, StoreError::Corrupt { .. }), "{error:?}");
    assert_eq!(inside.position(), SourcePosition::new(second + 1, 1));

    let eml_path = scratch.path().join("one.eml");
    fs::write(&eml_path, RICH).unwrap();
    let eml = source(&res, &eml_path);
    let error = res
        .open_import_acquisition(&eml, None, SourcePosition::new(0, 1))
        .unwrap_err();
    assert!(
        matches!(error, StoreError::InvalidInput { .. }),
        "{error:?}"
    );
    let done = SourcePosition::new(RICH.len() as u64, 1);
    assert!(acquire_from(&res, &eml, None, done).is_empty());
}

#[test]
fn inventories_that_disagree_with_their_bytes_are_corrupt() {
    let (scratch, res) = resources("disagree");
    let path = scratch.path().join("Three.mbox");
    fs::write(&path, three_message_mbox()).unwrap();
    let mbox = source(&res, &path);
    let recount = |messages: u64| {
        let mut json: serde_json::Value = serde_json::from_str(&mbox.to_json()).unwrap();
        json["messages"] = messages.into();
        SourceInventory::from_json(&json.to_string()).unwrap()
    };

    // Too few: the first counted message would end before the source.
    let short = recount(1);
    let mut acquisition = res
        .open_import_acquisition(&short, None, SourcePosition::START)
        .unwrap();
    let error = res.acquire_import_message(&mut acquisition).unwrap_err();
    assert!(matches!(error, StoreError::Corrupt { .. }), "{error:?}");

    // Too many: the bytes end before the count does.
    let long = recount(4);
    let mut acquisition = res
        .open_import_acquisition(&long, None, SourcePosition::START)
        .unwrap();
    for _ in 0..2 {
        res.acquire_import_message(&mut acquisition)
            .unwrap()
            .unwrap();
    }
    let error = res.acquire_import_message(&mut acquisition).unwrap_err();
    assert!(matches!(error, StoreError::Corrupt { .. }), "{error:?}");
    assert_eq!(acquisition.position().messages(), 2);
}

#[test]
fn a_missing_snapshot_stops_acquisition() {
    let (scratch, res) = resources("missing");
    let path = scratch.path().join("one.eml");
    fs::write(&path, RICH).unwrap();
    let eml = source(&res, &path);
    let snapshot = scratch
        .path()
        .join("store/recovery")
        .join(format!("import-source-{}", eml.snapshot().token()));
    fs::remove_file(snapshot).unwrap();
    // The original is still there; it is never substituted (spec/09).
    let error = res
        .open_import_acquisition(&eml, None, SourcePosition::START)
        .unwrap_err();
    assert!(matches!(error, StoreError::NotFound { .. }), "{error:?}");
}

/// A snapshot whose reads fail from `fail_at` on, and once with
/// `Interrupted` first when `interrupt` is set.
struct Failing {
    bytes: Cursor<Vec<u8>>,
    fail_at: u64,
    interrupt: bool,
}

impl Read for Failing {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if std::mem::take(&mut self.interrupt) {
            return Err(io::ErrorKind::Interrupted.into());
        }
        let room = self.fail_at.saturating_sub(self.bytes.position());
        if room == 0 {
            return Err(io::Error::other("unreadable sector"));
        }
        let len = buf.len().min(usize::try_from(room).unwrap_or(usize::MAX));
        self.bytes.read(&mut buf[..len])
    }
}

impl Seek for Failing {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.bytes.seek(to)
    }
}

fn failing(kind: SourceKind, bytes: Vec<u8>, total: u64, fail_at: u64) -> SourceAcquisition {
    SourceAcquisition {
        length: bytes.len() as u64,
        file: Box::new(Failing {
            bytes: Cursor::new(bytes),
            fail_at,
            interrupt: true,
        }),
        kind,
        variant: MboxVariant::Mboxrd,
        path: "/synthetic/source".to_owned(),
        sha256: "0".repeat(64),
        total,
        position: SourcePosition::START,
    }
}

#[test]
fn unreadable_regions_name_their_offset_and_store_nothing() {
    let (_scratch, res) = resources("unreadable");
    let bytes = three_message_mbox();
    let third = bytes.len() - "third\n".len();
    let mut mbox = failing(SourceKind::Mbox, bytes, 3, third as u64);
    for _ in 0..2 {
        next(&res.store, &mut mbox, &Limits::default())
            .unwrap()
            .unwrap();
    }
    let before = mbox.position();
    let error = next(&res.store, &mut mbox, &Limits::default()).unwrap_err();
    let StoreError::Unavailable { reason, .. } = &error else {
        panic!("{error:?}");
    };
    assert!(reason.contains(&format!("byte {third}")), "{reason}");
    assert!(reason.contains("/synthetic/source"), "{reason}");
    assert_eq!(mbox.position(), before);

    // An EML read failure surfaces from publication with offset 0; the
    // interrupted first read was retried rather than reported.
    let mut eml = failing(SourceKind::Eml, RICH.to_vec(), 1, 40);
    let error = next(&res.store, &mut eml, &Limits::default()).unwrap_err();
    let StoreError::Unavailable { reason, .. } = &error else {
        panic!("{error:?}");
    };
    assert!(
        reason.contains("byte 0") && reason.contains("unreadable sector"),
        "{reason}"
    );
    assert_eq!(eml.position(), SourcePosition::START);
}

#[test]
fn verified_bundle_manifest_refuses_unverified_ingest() {
    let (scratch, res) = resources("bundle-acquire-guard");
    let path = scratch.path().join("message.eml");
    let manifest = scratch.path().join("manifest.eml");
    fs::write(&path, RICH).unwrap();
    fs::write(&manifest, b"synthetic manifest").unwrap();
    let plain = source(&res, &path);
    let captured_manifest = source(&res, &manifest);
    let mut json: serde_json::Value = serde_json::from_str(&plain.to_json()).unwrap();
    let manifest_json: serde_json::Value =
        serde_json::from_str(&captured_manifest.to_json()).unwrap();
    json["v"] = 2.into();
    json["bundle"] = serde_json::json!({"manifest": manifest_json["snapshot"], "format": "eml"});
    let bundle = SourceInventory::from_json(&json.to_string()).unwrap();
    let error = res
        .open_import_acquisition(&bundle, None, SourcePosition::START)
        .unwrap_err();
    assert!(matches!(error, StoreError::Unavailable { .. }));
    assert!(error.to_string().contains("M1.7d2"));
}

#[test]
fn bundle_preparation_distinguishes_cancellation() {
    let (scratch, res) = resources("bundle-prepare-cancel");
    let message = scratch.path().join("message.eml");
    let manifest = scratch.path().join("manifest.eml");
    fs::write(&message, b"body").unwrap();
    fs::write(&manifest, b"synthetic manifest").unwrap();
    let source_json = source(&res, &message).to_json();
    let manifest_json = source(&res, &manifest).to_json();
    let mut json: serde_json::Value = serde_json::from_str(&source_json).unwrap();
    let manifest_json: serde_json::Value = serde_json::from_str(&manifest_json).unwrap();
    json["v"] = serde_json::json!(2);
    json["bundle"] = serde_json::json!({"manifest": manifest_json["snapshot"], "format": "eml", "file_ordinal": 0});
    let bundle = SourceInventory::from_json(&json.to_string()).unwrap();
    assert!(matches!(
        prepare_bundle(&res.store, &bundle, SourcePosition::START, &mut || {
            ControlFlow::Break(())
        }),
        Ok(sb_store::Scoped::Stopped)
    ));
    let mut calls = 0;
    let mut stop_in_parser = || {
        calls += 1;
        if calls >= 3 {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    assert!(matches!(
        prepare_bundle(
            &res.store,
            &bundle,
            SourcePosition::START,
            &mut stop_in_parser
        ),
        Ok(sb_store::Scoped::Stopped)
    ));
    assert!(matches!(
        prepare_bundle(&res.store, &bundle, SourcePosition::START, &mut || {
            ControlFlow::Continue(())
        }),
        Err(StoreError::Corrupt { .. })
    ));
    res.remove_import_sources(&[bundle.bundle().unwrap().manifest().clone()])
        .unwrap();
    assert!(
        prepare_bundle(&res.store, &bundle, SourcePosition::START, &mut || {
            ControlFlow::Continue(())
        })
        .is_err()
    );
}

#[test]
fn bundle_cursor_selects_interleaved_descriptors_and_resumes() {
    use serde_json::json;
    use sha2::{Digest, Sha256};

    let (scratch, res) = resources("bundle-cursor");
    let path = scratch.path().join("alias.mbox");
    let contents = format!("{ENVELOPE}a\n{ENVELOPE}b\n");
    fs::write(&path, &contents).unwrap();
    let plain = source(&res, &path);
    let offset = (ENVELOPE.len() + 2) as u64;
    let entry = |id: i64, file: Option<&str>, start: u64| {
        let mut value = json!({"v":1,"occurrence_id":id,"message_id":id,
            "account":{"id":"a","name":"a"},"mailbox":{"id":1,"name":"m"},
            "occurrence_state":"present","flags":{"seen":false,"flagged":false,"answered":false,"draft":false,"deleted":false},
            "keywords":[],"acquisition":{"id":id,"sha256":"a".repeat(64),"bytes":2},
            "body_availability":"acquired","tags":[],"note":null,"file":file,
            "record_offset":start,"encoded_bytes":2,"original_bytes":2,"framing_lf_added":false});
        if file.is_none() {
            value["acquisition"] = serde_json::Value::Null;
            value["body_availability"] = json!("not_acquired");
            for key in [
                "record_offset",
                "encoded_bytes",
                "original_bytes",
                "framing_lf_added",
            ] {
                value.as_object_mut().unwrap().remove(key);
            }
        }
        value
    };
    let mut messages = vec![
        entry(1, Some("target.mbox"), 0),
        entry(2, Some("other.mbox"), 0),
        entry(3, None, 0),
        entry(4, Some("target.mbox"), offset),
    ];
    messages[3]["encoded_bytes"] = json!(3);
    messages[3]["framing_lf_added"] = json!(true);
    let mut digest = Sha256::new();
    for id in 1_i64..=4 {
        digest.update(id.to_be_bytes());
    }
    let hash = digest
        .finalize()
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect::<String>();
    let mut manifest = json!({"v":1,"kind":"export-manifest","job":"j","format":"mboxrd","destination":"/d",
        "scope":{"kind":"mailbox","id":1,"generation":1},"generation":1,"entries":4,"ids_sha256":hash,
        "files":[{"name":"other.mbox","bytes":1,"records":1},{"name":"target.mbox","bytes":contents.len(),"records":2}],"messages":messages});
    // Force multiple parser reads inside a single unbounded JSON string.
    const LONG_NOTE_BYTES: usize = 32 * 1024;
    const STOP_DURING_STRING_READ: usize = 5;
    manifest["messages"][0]["note"] = json!("x".repeat(LONG_NOTE_BYTES));
    let manifest_path = scratch.path().join("manifest.eml");
    fs::write(&manifest_path, manifest.to_string()).unwrap();
    let manifest_source = source(&res, &manifest_path);
    let mut metadata: serde_json::Value = serde_json::from_str(&plain.to_json()).unwrap();
    let manifest_metadata: serde_json::Value =
        serde_json::from_str(&manifest_source.to_json()).unwrap();
    metadata["v"] = json!(2);
    metadata["bundle"] =
        json!({"manifest":manifest_metadata["snapshot"],"format":"mboxrd","file_ordinal":1});
    let bundle = SourceInventory::from_json(&metadata.to_string()).unwrap();
    let mut reads = 0;
    let mut stop_in_string = || {
        reads += 1;
        if reads == STOP_DURING_STRING_READ {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    };
    assert!(matches!(
        prepare_bundle(
            &res.store,
            &bundle,
            SourcePosition::START,
            &mut stop_in_string
        ),
        Ok(sb_store::Scoped::Stopped)
    ));
    assert_eq!(reads, STOP_DURING_STRING_READ);
    let mut stop = || ControlFlow::Continue(());
    let sb_store::Scoped::Done(mut cursor) = prepare_bundle(
        &res.store,
        &bundle,
        SourcePosition::new(offset, 1),
        &mut stop,
    )
    .unwrap() else {
        panic!("cancelled")
    };
    let sb_store::Scoped::Done(Some(descriptor)) = cursor.next().unwrap() else {
        panic!("missing descriptor")
    };
    assert_eq!(descriptor.sha256, "a".repeat(64));
    assert_eq!(descriptor.bytes, 2);
    let framing = descriptor.framing.unwrap();
    assert_eq!(framing.offset, offset);
    assert_eq!(framing.encoded_bytes, 3);
    assert!(framing.framing_lf_added);
    assert!(matches!(
        cursor.next().unwrap(),
        sb_store::Scoped::Done(None)
    ));
    drop(cursor);
    assert!(
        prepare_bundle(
            &res.store,
            &bundle,
            SourcePosition::new(offset + 1, 1),
            &mut stop
        )
        .is_err()
    );
    assert!(
        res.open_import_acquisition(&bundle, None, SourcePosition::START)
            .is_err()
    );
    let sb_store::Scoped::Done(mut finished) = prepare_bundle(
        &res.store,
        &bundle,
        SourcePosition::new(contents.len() as u64, 2),
        &mut stop,
    )
    .unwrap() else {
        panic!("cancelled")
    };
    assert!(matches!(
        finished.next().unwrap(),
        sb_store::Scoped::Done(None)
    ));
    drop(finished);
    metadata["bundle"]["file_ordinal"] = json!(2);
    let out_of_range = SourceInventory::from_json(&metadata.to_string()).unwrap();
    assert!(prepare_bundle(&res.store, &out_of_range, SourcePosition::START, &mut stop).is_err());
    metadata["bundle"]
        .as_object_mut()
        .unwrap()
        .remove("file_ordinal");
    let legacy = SourceInventory::from_json(&metadata.to_string()).unwrap();
    assert!(prepare_bundle(&res.store, &legacy, SourcePosition::START, &mut stop).is_err());

    // Tampering with the retained manifest invalidates its snapshot proof.
    let snapshot = bundle.bundle().unwrap().manifest();
    let copy = scratch
        .path()
        .join("store/recovery")
        .join(format!("import-source-{}", snapshot.token()));
    let mut changed = fs::read(&copy).unwrap();
    changed[0] ^= 1;
    fs::write(copy, changed).unwrap();
    assert!(matches!(
        prepare_bundle(&res.store, &bundle, SourcePosition::START, &mut stop),
        Err(StoreError::Corrupt { .. })
    ));
}
