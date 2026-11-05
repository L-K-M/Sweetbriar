//! M1.7d2c2a native proof: manifest-verified EML acquisition over the
//! frozen registry, through the real Resources facade and the real
//! object publisher. Synthetic isolated roots only; no owner data.

use std::fs;
use std::io::{self, Cursor, Read, Seek, SeekFrom};
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::Arc;

use sb_store::imports::{
    ImportBatch, ImportBatchStats, ImportJobState, ImportMessage, ImportSourceKind,
    advance_import_job, commit_import_batch, create_import_job, record_import_source,
};
use sb_store::{ObjectId, StoreError, WritePriority, read_error_as_store_error};
use sb_test_support::{FaultFs, FaultKind, FsOp, ScratchDir, block_on, real_fs};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

use super::*;
use crate::{ImportInventoryOutcome, ManifestProofRegistry, Resources, Scoped, SourceInventory};

const MANIFEST: &str = "sweetbriar-metadata.json";
const JOB: &str = "00000000-0000-4000-8000-0000000000c5";
const MESSAGE: &[u8] = b"From: sender@example.test\r\nSubject: reserved\r\n\r\nreserved bytes\r\n";

fn resources(label: &str) -> (ScratchDir, Resources) {
    let scratch = ScratchDir::new(label);
    let resources = Resources::open(&scratch.path().join("store")).unwrap();
    (scratch, resources)
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
}

fn sha_hex(bytes: &[u8]) -> String {
    hex_of(&Sha256::digest(bytes))
}

fn rows(res: &Resources, table: &str) -> i64 {
    res.read(|tx| {
        Ok(tx
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
                row.get::<_, i64>(0)
            })
            .unwrap())
    })
    .unwrap()
}

fn reservations(res: &Resources) -> i64 {
    res.read(|tx| {
        Ok(tx
            .query_row(
                "SELECT count(*) FROM object_pins WHERE pin_kind = 'reservation'",
                [],
                |row| row.get::<_, i64>(0),
            )
            .unwrap())
    })
    .unwrap()
}

/// Write a synthetic bundle directory: `.eml` children named for their
/// occurrence ids, plus a manifest whose file table is sorted by name
/// (the inventory's merge order) while its message order may differ.
fn write_bundle(dir: &Path, bodies: &[Vec<u8>], message_order: &[usize]) {
    assert_eq!(message_order.len(), bodies.len());
    fs::create_dir_all(dir).unwrap();
    let mut manifest_order: Vec<usize> = (0..bodies.len()).collect();
    manifest_order.sort_by_key(|&index| format!("{index}.eml"));
    for (index, body) in bodies.iter().enumerate() {
        fs::write(dir.join(format!("{index}.eml")), body).unwrap();
    }
    let mut ids = Sha256::new();
    for &index in message_order {
        ids.update((index as i64).to_be_bytes());
    }
    let messages: Vec<Value> = message_order
        .iter()
        .map(|&index| {
            let body = &bodies[index];
            json!({
                "v": 1,
                "occurrence_id": index as i64,
                "message_id": index as i64 + 1,
                "account": {"id": "acct", "name": "Account"},
                "mailbox": {"id": 1, "name": "Inbox"},
                "occurrence_state": "present",
                "flags": {"seen": false, "flagged": false, "answered": false,
                          "draft": false, "deleted": false},
                "keywords": [],
                "acquisition": {"id": index as i64 + 1, "sha256": sha_hex(body),
                                "bytes": body.len()},
                "body_availability": "acquired",
                "tags": [],
                "note": null,
                "file": format!("{index}.eml"),
            })
        })
        .collect();
    let files: Vec<Value> = manifest_order
        .iter()
        .map(|&index| {
            json!({"name": format!("{index}.eml"), "bytes": bodies[index].len(),
                   "records": 1})
        })
        .collect();
    let manifest = json!({
        "v": 1,
        "kind": "export-manifest",
        "job": "bundle-job",
        "format": "eml",
        "destination": dir.display().to_string(),
        "scope": {"kind": "mailbox", "id": 1, "generation": 1},
        "generation": 1,
        "entries": bodies.len(),
        "ids_sha256": hex_of(&ids.finalize()),
        "files": files,
        "messages": messages,
    });
    fs::write(dir.join(MANIFEST), manifest.to_string()).unwrap();
}

fn inventory_sources(res: &Resources, dir: &Path) -> Vec<SourceInventory> {
    let outcome = res.inventory_import_sources(&[dir.to_path_buf()], |_| ControlFlow::Continue(()));
    let ImportInventoryOutcome::Complete(inventory) = outcome else {
        panic!("the inventory was cancelled");
    };
    assert!(
        inventory.problems().is_empty(),
        "{:?}",
        inventory.problems()
    );
    inventory.sources().to_vec()
}

fn body_index(source: &SourceInventory) -> usize {
    source
        .path()
        .file_stem()
        .unwrap()
        .to_str()
        .unwrap()
        .parse()
        .unwrap()
}

fn acquire_one(
    res: &Resources,
    registry: &ManifestProofRegistry,
    source: &SourceInventory,
) -> AcquiredMessage {
    let mut acquisition =
        match res.open_bundle_acquisition(registry, source, SourcePosition::START, |_| {
            ControlFlow::Continue(())
        }) {
            Ok(Scoped::Done(acquisition)) => acquisition,
            other => panic!("the bundle session did not open: {other:?}"),
        };
    let message = res
        .acquire_import_message(&mut acquisition)
        .unwrap()
        .unwrap();
    assert!(
        res.acquire_import_message(&mut acquisition)
            .unwrap()
            .is_none()
    );
    message
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

fn tmp_is_empty(scratch: &ScratchDir) -> bool {
    let tmp = scratch.path().join("store/objects/tmp");
    fs::read_dir(tmp).unwrap().count() == 0
}

/// A reader that fails once `fail_at` bytes remain, optionally after one
/// `Interrupted` that the publisher's prefix tee must retry.
struct FailAt {
    bytes: Cursor<Vec<u8>>,
    fail_at: u64,
    interrupt: bool,
}

impl Read for FailAt {
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

impl Seek for FailAt {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.bytes.seek(to)
    }
}

fn tampered(source: &SourceInventory, change: impl FnOnce(&mut Value)) -> SourceInventory {
    let mut json: Value = serde_json::from_str(&source.to_json()).unwrap();
    change(&mut json);
    SourceInventory::from_json(&json.to_string()).unwrap()
}

fn running_job(res: &Resources, sources: &[SourceInventory]) -> i64 {
    let now = res.now();
    let jsons: Vec<String> = sources.iter().map(SourceInventory::to_json).collect();
    block_on(res.write(WritePriority::Sync, move |tx| {
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
    .unwrap()
}

fn commit(
    res: &Resources,
    expected_revision: i64,
    source_ordinal: u64,
    mailbox: String,
    messages: Vec<ImportMessage>,
) -> Result<ImportBatchStats, StoreError> {
    let now = res.now();
    block_on(res.write(WritePriority::Sync, move |tx| {
        commit_import_batch(
            tx,
            &ImportBatch {
                job_id: JOB,
                expected_revision,
                source_ordinal,
                mailbox_name: &mailbox,
                messages: &messages,
            },
            now,
            |_| Ok(("{\"v\":1}".to_owned(), "{\"v\":1}".to_owned())),
        )
    }))
}

#[test]
fn verified_read_checks_length_and_hash_at_eof() {
    let body = b"exact bytes";
    let expected = VerifiedMessage {
        sha256: Sha256::digest(body).into(),
        original_bytes: body.len() as u64,
    };
    let mut reader = VerifiedRead::new(Cursor::new(body.to_vec()), expected);
    let mut out = Vec::new();
    reader.read_to_end(&mut out).unwrap();
    assert_eq!(out, body);

    // Growth: one extra byte past the expected length.
    let mut grown = body.to_vec();
    grown.push(b'!');
    let error = VerifiedRead::new(Cursor::new(grown), expected)
        .read_to_end(&mut Vec::new())
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);
    assert!(read_error_as_store_error(&error).is_some());

    // Truncation: fewer bytes than expected.
    let error = VerifiedRead::new(Cursor::new(body[..4].to_vec()), expected)
        .read_to_end(&mut Vec::new())
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);

    // Hash mismatch: right length, wrong bytes.
    let wrong = b"wrong bytes";
    assert_eq!(wrong.len(), body.len());
    let error = VerifiedRead::new(Cursor::new(wrong.to_vec()), expected)
        .read_to_end(&mut Vec::new())
        .unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::InvalidData);

    // An empty message verifies its empty hash.
    let empty = VerifiedMessage {
        sha256: Sha256::digest([]).into(),
        original_bytes: 0,
    };
    let mut reader = VerifiedRead::new(Cursor::new(Vec::new()), empty);
    assert_eq!(reader.read_to_end(&mut Vec::new()).unwrap(), 0);

    // An interrupted read keeps its native kind; it is not cancellation.
    struct Interrupting;
    impl Read for Interrupting {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::ErrorKind::Interrupted.into())
        }
    }
    let mut reader = VerifiedRead::new(Interrupting, expected);
    let mut buf = [0u8; 8];
    let error = reader.read(&mut buf).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
}

#[test]
fn bundle_eml_round_trips_exact_bytes() {
    let (scratch, res) = resources("bundle-eml-roundtrip");
    let dir = scratch.path().join("bundle");
    let bodies = vec![
        b"From: one@example.test\r\nSubject: one\r\n\r\nfirst\r\n".to_vec(),
        b"From: two@example.test\r\nSubject: two\r\n\r\nunterminated".to_vec(),
        Vec::new(),
    ];
    write_bundle(&dir, &bodies, &[2, 0, 1]);
    let sources = inventory_sources(&res, &dir);
    assert_eq!(sources.len(), bodies.len());
    let registry = res.manifest_proof_registry().unwrap();
    for source in &sources {
        let index = body_index(source);
        let message = acquire_one(&res, &registry, source);
        assert_eq!(published(&res, &message), bodies[index]);
        assert!(matches!(
            message.message().source_kind,
            ImportSourceKind::Eml
        ));
        assert_eq!(
            message.message().source_path,
            source.path().to_str().unwrap()
        );
        assert_eq!(
            message.message().source_sha256,
            source.snapshot().sha256_hex()
        );
        assert_eq!(message.message().offset, 0);
        assert_eq!(
            message.next_position(),
            SourcePosition::new(bodies[index].len() as u64, 1)
        );
        assert_eq!(
            message.flag_extraction(),
            if index == 2 {
                FlagExtraction::Incomplete
            } else {
                FlagExtraction::Complete
            }
        );
        let subject = message.message().summary.subject.as_deref();
        assert_eq!(
            subject,
            match index {
                0 => Some("one"),
                1 => Some("two"),
                _ => None,
            }
        );
    }
    registry.finish().unwrap();
    res.close().unwrap();
}

#[test]
fn shuffled_message_order_keeps_source_order_without_manifest_reads() {
    let (scratch, res) = resources("bundle-shuffled");
    let dir = scratch.path().join("bundle");
    let bodies: Vec<Vec<u8>> = (0..64)
        .map(|index| {
            format!("From: {index}@example.test\nSubject: {index}\n\nbody {index}\n").into_bytes()
        })
        .collect();
    let message_order: Vec<usize> = (0..bodies.len()).rev().collect();
    write_bundle(&dir, &bodies, &message_order);
    let sources = inventory_sources(&res, &dir);
    assert_eq!(sources.len(), bodies.len());
    let registry = res.manifest_proof_registry().unwrap();
    let first = acquire_one(&res, &registry, &sources[0]);
    assert_eq!(published(&res, &first), bodies[body_index(&sources[0])]);

    // The frozen proof is enough: the manifest snapshot is gone and the
    // remaining sources still resolve their own descriptors.
    let token = sources[0].bundle().unwrap().manifest().token().to_owned();
    let snapshot = scratch
        .path()
        .join("store/recovery")
        .join(format!("import-source-{token}"));
    fs::remove_file(snapshot).unwrap();
    for source in &sources[1..] {
        let message = acquire_one(&res, &registry, source);
        assert_eq!(published(&res, &message), bodies[body_index(source)]);
    }
    registry.finish().unwrap();
    res.close().unwrap();
}

#[test]
fn duplicate_bundle_content_keeps_multiplicity_and_replays_by_source_identity() {
    let (scratch, res) = resources("bundle-duplicates");
    let dir = scratch.path().join("bundle");
    let bodies = vec![MESSAGE.to_vec(), MESSAGE.to_vec()];
    write_bundle(&dir, &bodies, &[0, 1]);
    let sources = inventory_sources(&res, &dir);
    let registry = res.manifest_proof_registry().unwrap();
    let first = acquire_one(&res, &registry, &sources[0]);
    let second = acquire_one(&res, &registry, &sources[1]);
    assert_eq!(
        first.message().object.id().sha256(),
        second.message().object.id().sha256()
    );
    assert_eq!(
        first.message().object.id().byte_length(),
        second.message().object.id().byte_length()
    );
    assert_ne!(first.message().source_path, second.message().source_path);
    assert_eq!(
        reservations(&res),
        2,
        "same bytes publish distinct lifetimes"
    );

    let revision = running_job(&res, &sources);
    let mailbox = sources[0].mailbox().to_owned();
    commit(
        &res,
        revision,
        0,
        mailbox.clone(),
        vec![first.into_message()],
    )
    .unwrap();
    assert_eq!(reservations(&res), 1);

    // Re-acquiring the first source is a replay: its commit confirms the
    // existing reference and releases only the new token.
    let replay = acquire_one(&res, &registry, &sources[0]);
    assert_eq!(reservations(&res), 2);
    commit(
        &res,
        revision + 1,
        0,
        mailbox.clone(),
        vec![replay.into_message()],
    )
    .unwrap();
    assert_eq!(reservations(&res), 1);

    // The second source is a distinct identity and commits separately.
    commit(&res, revision + 2, 1, mailbox, vec![second.into_message()]).unwrap();
    assert_eq!(reservations(&res), 0);
    assert_eq!(rows(&res, "acquisitions"), 1);
    assert_eq!(rows(&res, "acquisition_sources"), 2);
    registry.finish().unwrap();
    res.close().unwrap();
}

#[test]
fn changed_originals_never_replace_the_private_snapshots() {
    let (scratch, res) = resources("bundle-changed-originals");
    let dir = scratch.path().join("bundle");
    let bodies = vec![
        b"Subject: zero\n\nzero\n".to_vec(),
        b"Subject: one\n\none\n".to_vec(),
        b"Subject: two\n\ntwo\n".to_vec(),
    ];
    write_bundle(&dir, &bodies, &[0, 1, 2]);
    let sources = inventory_sources(&res, &dir);
    let registry = res.manifest_proof_registry().unwrap();

    // Change, truncate and remove the originals after inventory.
    fs::write(dir.join("0.eml"), b"changed\n").unwrap();
    fs::write(dir.join("1.eml"), b"").unwrap();
    fs::remove_file(dir.join("2.eml")).unwrap();
    for source in &sources {
        let message = acquire_one(&res, &registry, source);
        assert_eq!(published(&res, &message), bodies[body_index(source)]);
    }
    registry.finish().unwrap();
    res.close().unwrap();
}

#[test]
fn wrong_binding_format_and_checkpoint_are_rejected() {
    let (scratch, res) = resources("bundle-wrong-binding");
    let dir = scratch.path().join("bundle");
    let bodies = vec![
        b"Subject: zero\n\nzero\n".to_vec(),
        b"Subject: one\n\none\n".to_vec(),
    ];
    write_bundle(&dir, &bodies, &[0, 1]);
    let sources = inventory_sources(&res, &dir);
    let registry = res.manifest_proof_registry().unwrap();
    let length = sources[0].snapshot().byte_length();
    let wrong_sha = "00".repeat(32);

    let cases: Vec<(&str, SourceInventory)> = vec![
        (
            "manifest hash",
            tampered(&sources[0], |json| {
                json["bundle"]["manifest"]["sha256"] = wrong_sha.into();
            }),
        ),
        (
            "manifest length",
            tampered(&sources[0], |json| {
                json["bundle"]["manifest"]["byte_length"] = 1.into();
            }),
        ),
        (
            "source length",
            tampered(&sources[0], |json| {
                json["snapshot"]["byte_length"] = (length + 1).into();
            }),
        ),
        (
            "ordinal out of range",
            tampered(&sources[0], |json| {
                json["bundle"]["file_ordinal"] = 999.into();
            }),
        ),
        (
            "ordinal of another file",
            tampered(&sources[0], |json| {
                json["bundle"]["file_ordinal"] = 1.into();
            }),
        ),
        (
            "legacy ordinal",
            tampered(&sources[0], |json| {
                json["bundle"]
                    .as_object_mut()
                    .unwrap()
                    .remove("file_ordinal");
            }),
        ),
    ];
    for (label, source) in cases {
        let error = res
            .open_bundle_acquisition(&registry, &source, SourcePosition::START, |_| {
                ControlFlow::Continue(())
            })
            .unwrap_err();
        assert!(
            matches!(error, StoreError::Corrupt { .. }),
            "{label}: {error:?}"
        );
    }

    // The EML count is fixed at one by the inventory parser; the
    // registry's slot count is checked again by `visit_source`.
    let mut json: Value = serde_json::from_str(&sources[0].to_json()).unwrap();
    json["messages"] = 2.into();
    assert!(SourceInventory::from_json(&json.to_string()).is_err());

    // A plain source is not a bundle source.
    let plain = crate::SourceInventory::from_json(
        &serde_json::json!({
            "v": 1,
            "path": dir.join("0.eml").display().to_string(),
            "mailbox": "bundle",
            "kind": "eml",
            "snapshot": {
                "token": sources[0].snapshot().token(),
                "sha256": sources[0].snapshot().sha256_hex(),
                "byte_length": length,
            },
            "messages": 1,
            "quoting": "equivalent",
        })
        .to_string(),
    )
    .unwrap();
    let error = res
        .open_bundle_acquisition(&registry, &plain, SourcePosition::START, |_| {
            ControlFlow::Continue(())
        })
        .unwrap_err();
    assert!(
        matches!(error, StoreError::InvalidInput { .. }),
        "{error:?}"
    );

    // A format that disagrees with the frozen manifest is corrupt,
    // never a fallback to the plain path.
    let mboxrd = tampered(&sources[0], |json| {
        json["kind"] = "mbox".into();
        json["bundle"]["format"] = "mboxrd".into();
    });
    let error = res
        .open_bundle_acquisition(&registry, &mboxrd, SourcePosition::START, |_| {
            ControlFlow::Continue(())
        })
        .unwrap_err();
    assert!(matches!(error, StoreError::Corrupt { .. }), "{error:?}");

    // An invented midpoint is not a checkpoint.
    for (offset, messages) in [(length / 2, 0), (0, 1), (length, 0)] {
        let error = res
            .open_bundle_acquisition(
                &registry,
                &sources[0],
                SourcePosition::new(offset, messages),
                |_| ControlFlow::Continue(()),
            )
            .unwrap_err();
        assert!(
            matches!(error, StoreError::InvalidInput { .. }),
            "{offset}/{messages}: {error:?}"
        );
    }

    // The done checkpoint is accepted and yields no message.
    let Scoped::Done(mut acquisition) = res
        .open_bundle_acquisition(
            &registry,
            &sources[0],
            SourcePosition::new(length, 1),
            |_| ControlFlow::Continue(()),
        )
        .unwrap()
    else {
        panic!("the done checkpoint was stopped");
    };
    assert!(
        res.acquire_import_message(&mut acquisition)
            .unwrap()
            .is_none()
    );
    registry.finish().unwrap();
    res.close().unwrap();
}

#[test]
fn bundle_read_faults_keep_the_native_kind_and_publish_nothing() {
    let (scratch, res) = resources("bundle-read-fault");
    for interrupt in [false, true] {
        let mut acquisition = SourceAcquisition {
            file: Box::new(FailAt {
                bytes: Cursor::new(b"partial".to_vec()),
                fail_at: 3,
                interrupt,
            }),
            kind: SourceKind::Eml,
            variant: MboxVariant::Mboxrd,
            path: "/synthetic/source".to_owned(),
            sha256: "0".repeat(64),
            length: 7,
            total: 1,
            position: SourcePosition::START,
            verification: Some(VerifiedMessage {
                sha256: [0; 32],
                original_bytes: 7,
            }),
            descriptors: None,
            next_offset: 0,
        };
        let error = next(&res.store, &mut acquisition, &Limits::default()).unwrap_err();
        let StoreError::Unavailable { io_kind, reason } = &error else {
            panic!("{error:?}");
        };
        assert_eq!(*io_kind, Some(io::ErrorKind::Other), "{error:?}");
        assert!(reason.contains("byte 0"), "{reason}");
        assert_eq!(acquisition.position(), SourcePosition::START);
        assert_eq!(reservations(&res), 0);
        assert!(tmp_is_empty(&scratch));
    }
    res.close().unwrap();
}

#[test]
fn missing_manifest_snapshot_is_typed_not_found() {
    let (scratch, res) = resources("bundle-missing-manifest");
    let dir = scratch.path().join("bundle");
    let bodies = vec![b"Subject: zero\n\nzero\n".to_vec()];
    write_bundle(&dir, &bodies, &[0]);
    let sources = inventory_sources(&res, &dir);
    let registry = res.manifest_proof_registry().unwrap();
    let token = sources[0].bundle().unwrap().manifest().token().to_owned();
    let snapshot = scratch
        .path()
        .join("store/recovery")
        .join(format!("import-source-{token}"));
    fs::remove_file(snapshot).unwrap();
    let error = res
        .open_bundle_acquisition(&registry, &sources[0], SourcePosition::START, |_| {
            ControlFlow::Continue(())
        })
        .unwrap_err();
    assert!(matches!(error, StoreError::NotFound { .. }), "{error:?}");
    drop(registry);
    res.close().unwrap();
}

#[test]
fn deterministic_cancellation_stops_before_the_session() {
    let (scratch, res) = resources("bundle-cancel");
    let dir = scratch.path().join("bundle");
    let bodies = vec![b"Subject: zero\n\nzero\n".to_vec()];
    write_bundle(&dir, &bodies, &[0]);
    let sources = inventory_sources(&res, &dir);
    let registry = res.manifest_proof_registry().unwrap();

    let mut calls = 0;
    let stopped = res
        .open_bundle_acquisition(&registry, &sources[0], SourcePosition::START, |_| {
            calls += 1;
            ControlFlow::Break(())
        })
        .unwrap();
    assert!(matches!(stopped, Scoped::Stopped));
    assert_eq!(calls, 1, "the first progress check stops production");

    // The next attempt with a continuing callback succeeds.
    let message = acquire_one(&res, &registry, &sources[0]);
    assert_eq!(published(&res, &message), bodies[0]);
    registry.finish().unwrap();
    res.close().unwrap();
}

#[test]
fn digest_mismatch_aborts_before_install_or_reservation() {
    let (scratch, res) = resources("bundle-digest-mismatch");
    let actual = b"changed bytes!".to_vec();
    let expected = VerifiedMessage {
        sha256: Sha256::digest(b"original bytes").into(),
        original_bytes: actual.len() as u64,
    };
    let mut acquisition = SourceAcquisition {
        file: Box::new(Cursor::new(actual.clone())),
        kind: SourceKind::Eml,
        variant: MboxVariant::Mboxrd,
        path: "/synthetic/source".to_owned(),
        sha256: "0".repeat(64),
        length: actual.len() as u64,
        total: 1,
        position: SourcePosition::START,
        verification: Some(expected),
        descriptors: None,
        next_offset: 0,
    };
    let error = next(&res.store, &mut acquisition, &Limits::default()).unwrap_err();
    assert!(matches!(error, StoreError::Corrupt { .. }), "{error:?}");
    assert_eq!(acquisition.position(), SourcePosition::START);
    let id = ObjectId::from_hex(&sha_hex(&actual), actual.len() as u64).unwrap();
    assert!(!res.store.objects().exists(&id).unwrap());
    assert_eq!(reservations(&res), 0);
    assert_eq!(rows(&res, "acquisitions"), 0);
    assert!(tmp_is_empty(&scratch));
    res.close().unwrap();
}

#[test]
fn drop_and_reopen_rebuilds_the_frozen_proof() {
    let (scratch, res) = resources("bundle-reopen");
    let dir = scratch.path().join("bundle");
    let bodies = vec![
        b"Subject: zero\n\nzero\n".to_vec(),
        b"Subject: one\n\none\n".to_vec(),
    ];
    write_bundle(&dir, &bodies, &[1, 0]);
    let sources = inventory_sources(&res, &dir);
    let source = sources[0].clone();

    // A registry dropped without finish cleans its root lease; a new
    // registry reproduces the proof from the manifest snapshot.
    {
        let registry = res.manifest_proof_registry().unwrap();
        let _session =
            match res.open_bundle_acquisition(&registry, &source, SourcePosition::START, |_| {
                ControlFlow::Continue(())
            }) {
                Ok(Scoped::Done(session)) => session,
                other => panic!("the bundle session did not open: {other:?}"),
            };
    }
    let registry = res.manifest_proof_registry().unwrap();
    let message = acquire_one(&res, &registry, &source);
    assert_eq!(published(&res, &message), bodies[body_index(&source)]);
    registry.finish().unwrap();
    res.close().unwrap();
}

#[cfg(feature = "test-seams")]
#[test]
fn publisher_faults_leave_no_object_or_reservation() {
    for (op, kind) in [
        (FsOp::Write, FaultKind::Eio),
        (FsOp::Write, FaultKind::ShortWrite),
        (FsOp::SyncFile, FaultKind::Eio),
        (FsOp::Rename, FaultKind::NoSpace),
    ] {
        let scratch = ScratchDir::new("bundle-publisher-fault");
        let root = scratch.path().join("store");
        let dir = scratch.path().join("bundle");
        let bodies = vec![b"From: a@example.test\nSubject: A\n\nbody\n".to_vec()];
        write_bundle(&dir, &bodies, &[0]);

        // Measure the operations the setup performs, so the fault lands
        // on the publish rather than the proof production.
        let counting = Arc::new(FaultFs::new(
            real_fs(),
            FsOp::Write,
            u64::MAX,
            FaultKind::Eio,
        ));
        let res = Resources::open_with_fs(&root, counting.clone()).unwrap();
        let sources = inventory_sources(&res, &dir);
        let registry = res.manifest_proof_registry().unwrap();
        let session =
            match res.open_bundle_acquisition(&registry, &sources[0], SourcePosition::START, |_| {
                ControlFlow::Continue(())
            }) {
                Ok(Scoped::Done(session)) => session,
                other => panic!("the bundle session did not open: {other:?}"),
            };
        let baseline = counting.calls(op);
        drop(session);
        drop(registry);
        drop(res);

        let fs = Arc::new(FaultFs::new(real_fs(), op, baseline + 1, kind));
        let res = Resources::open_with_fs(&root, fs.clone()).unwrap();
        let sources = inventory_sources(&res, &dir);
        let registry = res.manifest_proof_registry().unwrap();
        let mut session =
            match res.open_bundle_acquisition(&registry, &sources[0], SourcePosition::START, |_| {
                ControlFlow::Continue(())
            }) {
                Ok(Scoped::Done(session)) => session,
                other => panic!("the bundle session did not open: {other:?}"),
            };
        let error = res.acquire_import_message(&mut session).unwrap_err();
        assert!(
            matches!(error, StoreError::Unavailable { .. }),
            "{op:?} {kind:?}: {error:?}"
        );
        assert_eq!(
            fs.calls(op),
            baseline + 1,
            "{op:?} fault landed on the publish"
        );
        // A rename fault lands after the reservation commits; the other
        // faults abort before it. Either way no final object is installed.
        assert_eq!(
            reservations(&res),
            if op == FsOp::Rename { 1 } else { 0 },
            "{op:?} reservations"
        );
        assert_eq!(rows(&res, "acquisitions"), 0, "{op:?} rows");
        let id = ObjectId::from_hex(&sha_hex(&bodies[0]), bodies[0].len() as u64).unwrap();
        assert!(!res.store.objects().exists(&id).unwrap());
        drop(session);
        res.close().unwrap();
    }
}

#[test]
fn bundle_session_is_send() {
    fn assert_send<T: Send>() {}
    assert_send::<SourceAcquisition>();
    assert_send::<ManifestProofRegistry>();
}
