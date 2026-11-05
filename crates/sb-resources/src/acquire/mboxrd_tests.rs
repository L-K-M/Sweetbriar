//! M1.7d2c2b native proof: manifest-verified mboxrd acquisition over the
//! frozen registry, through the real Resources facade and object store.
//! Synthetic isolated roots only; no owner data.

use std::fs;
use std::io::Read;
use std::ops::ControlFlow;
use std::path::Path;

use sb_mime::{MboxVariant, write_mboxrd_record};
use sb_store::imports::{
    ImportBatch, ImportJobState, ImportMessage, advance_import_job, commit_import_batch,
    create_import_job, record_import_source,
};
use sb_store::{StoreError, WritePriority};
use sb_test_support::{ScratchDir, block_on};
use sha2::{Digest, Sha256};

use super::*;
use crate::{
    ImportInventoryOutcome, ManifestDescriptor, ManifestProofRegistry, Scoped, SourceInventory,
};

const JOB: &str = "00000000-0000-4000-8000-0000000000c7";

fn resources(label: &str) -> (ScratchDir, Resources) {
    let scratch = ScratchDir::new(label);
    let resources = Resources::open(&scratch.path().join("store")).unwrap();
    (scratch, resources)
}

fn hex_of(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
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

fn tmp_is_empty(scratch: &ScratchDir) -> bool {
    let tmp = scratch.path().join("store/objects/tmp");
    // The registry scratch may remain as a directory; a publication temp
    // is a file and must be gone.
    fs::read_dir(tmp)
        .unwrap()
        .all(|entry| entry.unwrap().path().is_dir())
}

#[derive(Clone)]
struct TestMbox {
    name: String,
    bytes: Vec<u8>,
    descriptors: Vec<ManifestDescriptor>,
}

/// Encode bodies with the exporter's own mboxrd transport writer.
fn encode_mbox(name: &str, bodies: &[Vec<u8>]) -> TestMbox {
    let mut bytes = Vec::new();
    let mut descriptors = Vec::new();
    for body in bodies {
        let offset = bytes.len() as u64;
        let info = write_mboxrd_record(body.as_slice(), &mut bytes).unwrap();
        descriptors.push(ManifestDescriptor {
            sha256: Sha256::digest(body).into(),
            original_bytes: body.len() as u64,
            record_offset: offset,
            encoded_bytes: info.encoded_bytes(),
            framing_lf_added: info.framing_lf_added(),
        });
    }
    TestMbox {
        name: name.to_owned(),
        bytes,
        descriptors,
    }
}

/// Write a synthetic mboxrd bundle. `file_order` is the manifest's file
/// table order; `group_order` is the message-group order. The override
/// runs before each descriptor is written, for malformed-proof cases.
fn write_mboxrd_bundle<F: FnMut(usize, usize, &mut ManifestDescriptor) + ?Sized>(
    dir: &Path,
    files: &[TestMbox],
    file_order: &[usize],
    group_order: &[usize],
    override_descriptor: &mut F,
) {
    fs::create_dir_all(dir).unwrap();
    for file in files {
        fs::write(dir.join(&file.name), &file.bytes).unwrap();
    }
    let mut ids = Sha256::new();
    let mut messages = Vec::new();
    let mut occurrence = 0i64;
    for &file_index in group_order {
        let file = &files[file_index];
        for (record_index, descriptor) in file.descriptors.iter().copied().enumerate() {
            let mut descriptor = descriptor;
            override_descriptor(file_index, record_index, &mut descriptor);
            ids.update(occurrence.to_be_bytes());
            messages.push(format!(
                r#"{{"v":1,"occurrence_id":{occurrence},"message_id":{},"account":{{"id":"acct","name":"Account"}},"mailbox":{{"id":1,"name":"Inbox"}},"occurrence_state":"present","flags":{{"seen":false,"flagged":false,"answered":false,"draft":false,"deleted":false}},"keywords":[],"tags":[],"note":null,"acquisition":{{"id":{},"sha256":"{}","bytes":{}}},"body_availability":"acquired","file":"{}","record_offset":{},"encoded_bytes":{},"original_bytes":{},"framing_lf_added":{}}}"#,
                occurrence + 1,
                occurrence + 1,
                hex_of(&descriptor.sha256),
                descriptor.original_bytes,
                file.name,
                descriptor.record_offset,
                descriptor.encoded_bytes,
                descriptor.original_bytes,
                descriptor.framing_lf_added
            ));
            occurrence += 1;
        }
    }
    let files_json: Vec<String> = file_order
        .iter()
        .map(|&index| {
            let file = &files[index];
            format!(
                r#"{{"name":"{}","bytes":{},"records":{}}}"#,
                file.name,
                file.bytes.len(),
                file.descriptors.len()
            )
        })
        .collect();
    let manifest = format!(
        r#"{{"v":1,"kind":"export-manifest","job":"bundle-job","format":"mboxrd","destination":"{}","scope":{{"kind":"mailbox","id":1,"generation":1}},"generation":1,"entries":{},"ids_sha256":"{}","files":[{}],"messages":[{}]}}"#,
        dir.display(),
        messages.len(),
        hex_of(&ids.finalize()),
        files_json.join(","),
        messages.join(",")
    );
    fs::write(dir.join("sweetbriar-metadata.json"), manifest).unwrap();
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

fn open_session(
    res: &Resources,
    registry: &ManifestProofRegistry,
    source: &SourceInventory,
    position: SourcePosition,
) -> SourceAcquisition {
    match res.open_bundle_acquisition(registry, source, position, |_| ControlFlow::Continue(())) {
        Ok(Scoped::Done(session)) => session,
        other => panic!("the bundle session did not open: {other:?}"),
    }
}

fn acquire_all(
    res: &Resources,
    registry: &ManifestProofRegistry,
    source: &SourceInventory,
) -> Vec<AcquiredMessage> {
    let mut session = open_session(res, registry, source, SourcePosition::START);
    let mut messages = Vec::new();
    while let Some(message) = res.acquire_import_message(&mut session).unwrap() {
        messages.push(message);
    }
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
) -> Result<sb_store::imports::ImportBatchStats, StoreError> {
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

fn body_for(index: usize) -> Vec<u8> {
    format!("From: {index}@example.test\nSubject: {index}\n\nbody {index}\n").into_bytes()
}

#[test]
fn mboxrd_round_trips_exact_bytes_and_plain_import_keeps_physical_lf() {
    let (scratch, res) = resources("mboxrd-roundtrip");
    let dir = scratch.path().join("bundle");
    let bodies = vec![
        b"From: one@example.test\r\nSubject: one\r\n\r\nfirst\r\n".to_vec(),
        b"Subject: two\n\nunterminated".to_vec(),
        Vec::new(),
        b"Subject: quotes\n\nFrom inside\n>From inside\n>>From inside\n".to_vec(),
        format!("Subject: long\n\n{}>\n", "From ".repeat(2000)).into_bytes(),
        b"Subject: crlf\n\r\nbody\r\n".to_vec(),
    ];
    let file = encode_mbox("0.mbox", &bodies);
    write_mboxrd_bundle(&dir, &[file], &[0], &[0], &mut |_, _, _| {});
    let sources = inventory_sources(&res, &dir);
    let registry = res.manifest_proof_registry().unwrap();
    let messages = acquire_all(&res, &registry, &sources[0]);
    assert_eq!(messages.len(), bodies.len());
    for (index, message) in messages.iter().enumerate() {
        assert_eq!(published(&res, message), bodies[index]);
    }
    let finished = SourcePosition::new(
        messages.last().unwrap().next_position().offset(),
        messages.len() as u64,
    );
    let mut session = open_session(&res, &registry, &sources[0], finished);
    assert!(res.acquire_import_message(&mut session).unwrap().is_none());
    registry.finish().unwrap();

    let plain_dir = scratch.path().join("plain");
    fs::create_dir_all(&plain_dir).unwrap();
    fs::write(
        plain_dir.join("0.mbox"),
        fs::read(dir.join("0.mbox")).unwrap(),
    )
    .unwrap();
    let plain = inventory_sources(&res, &plain_dir).remove(0);
    let mut session = res
        .open_import_acquisition(&plain, Some(MboxVariant::Mboxrd), SourcePosition::START)
        .unwrap();
    for (index, body) in bodies.iter().enumerate() {
        let message = res.acquire_import_message(&mut session).unwrap().unwrap();
        let mut expected = body.clone();
        if !expected.is_empty() && !expected.ends_with(b"\n") {
            expected.push(b'\n');
        }
        assert_eq!(published(&res, &message), expected, "record {index}");
        assert!(message.message().bundle_framing.is_none());
    }
    assert!(res.acquire_import_message(&mut session).unwrap().is_none());
    res.close().unwrap();
}

fn bump_offset(_: usize, _: usize, d: &mut ManifestDescriptor) {
    d.record_offset += 1;
}
fn grow_encoded(_: usize, _: usize, d: &mut ManifestDescriptor) {
    d.encoded_bytes += 1;
}
fn bump_original(_: usize, _: usize, d: &mut ManifestDescriptor) {
    d.original_bytes += 1;
}
fn wrong_hash(_: usize, _: usize, d: &mut ManifestDescriptor) {
    d.sha256 = [0x5a; 32];
}
fn wrong_padding(_: usize, _: usize, d: &mut ManifestDescriptor) {
    d.original_bytes = 5;
    d.sha256 = Sha256::digest(b"body\n").into();
    d.framing_lf_added = true;
}
fn missing_lf(_: usize, _: usize, d: &mut ManifestDescriptor) {
    d.original_bytes = 4;
    d.sha256 = Sha256::digest(b"body").into();
    d.framing_lf_added = false;
}

type Override = fn(usize, usize, &mut ManifestDescriptor);

#[test]
fn malformed_mboxrd_records_publish_no_final_object() {
    let (scratch, res) = resources("mboxrd-malformed");
    let bodies = vec![b"body one\n".to_vec(), b"body two".to_vec()];
    let valid = encode_mbox("0.mbox", &bodies);
    let mut damaged = valid.clone();
    damaged.bytes[10] = b'X';
    let cases: &[(&str, TestMbox, Override)] = &[
        ("record offset", valid.clone(), bump_offset),
        (
            "encoded length past next record",
            valid.clone(),
            grow_encoded,
        ),
        ("original length", valid.clone(), bump_original),
        ("original hash", valid.clone(), wrong_hash),
        ("wrong padding", valid.clone(), wrong_padding),
        ("padding without LF", valid.clone(), missing_lf),
        ("damaged envelope", damaged, |_, _, _| {}),
    ];
    for (label, file, override_descriptor) in cases {
        let dir = scratch.path().join(label.replace(' ', "-"));
        let mut override_descriptor = *override_descriptor;
        write_mboxrd_bundle(
            &dir,
            std::slice::from_ref(file),
            &[0],
            &[0],
            &mut override_descriptor,
        );
        let sources = inventory_sources(&res, &dir);
        let registry = res.manifest_proof_registry().unwrap();
        let error =
            match res.open_bundle_acquisition(&registry, &sources[0], SourcePosition::START, |_| {
                ControlFlow::Continue(())
            }) {
                Ok(Scoped::Done(mut session)) => {
                    let error = res.acquire_import_message(&mut session).unwrap_err();
                    assert_eq!(session.position(), SourcePosition::START, "{label}");
                    error
                }
                Ok(Scoped::Stopped) => panic!("{label}: the session was stopped"),
                Err(error) => error,
            };
        assert!(
            matches!(
                error,
                StoreError::Corrupt { .. } | StoreError::InvalidInput { .. }
            ),
            "{label}: {error:?}"
        );
        assert_eq!(rows(&res, "acquisitions"), 0, "{label}");
        assert_eq!(reservations(&res), 0, "{label}");
        registry.finish().unwrap();
        assert!(tmp_is_empty(&scratch), "{label}");
    }
}

#[test]
fn mboxrd_checkpoint_resume_reopen_and_zero_message_files() {
    let (scratch, res) = resources("mboxrd-checkpoint");
    let dir = scratch.path().join("bundle");
    let bodies = vec![body_for(0), body_for(1), body_for(2)];
    let file = encode_mbox("0.mbox", &bodies);
    write_mboxrd_bundle(&dir, &[file], &[0], &[0], &mut |_, _, _| {});
    let sources = inventory_sources(&res, &dir);
    let registry = res.manifest_proof_registry().unwrap();
    let source = &sources[0];
    let length = source.snapshot().byte_length();
    let total = source.message_count();

    let checkpoint = {
        let mut session = open_session(&res, &registry, source, SourcePosition::START);
        let first = res.acquire_import_message(&mut session).unwrap().unwrap();
        assert_eq!(published(&res, &first), bodies[0]);
        first.next_position()
    };

    let mut resumed = open_session(&res, &registry, source, checkpoint);
    let second = res.acquire_import_message(&mut resumed).unwrap().unwrap();
    let third = res.acquire_import_message(&mut resumed).unwrap().unwrap();
    assert_eq!(published(&res, &second), bodies[1]);
    assert_eq!(published(&res, &third), bodies[2]);
    assert!(res.acquire_import_message(&mut resumed).unwrap().is_none());
    drop(resumed);

    let mut done = open_session(&res, &registry, source, SourcePosition::new(length, total));
    assert!(res.acquire_import_message(&mut done).unwrap().is_none());

    let misaligned = SourcePosition::new(checkpoint.offset() + 1, checkpoint.messages());
    let error = res
        .open_bundle_acquisition(&registry, source, misaligned, |_| ControlFlow::Continue(()))
        .unwrap_err();
    assert!(
        matches!(error, StoreError::InvalidInput { .. }),
        "{error:?}"
    );

    registry.finish().unwrap();
    let registry = res.manifest_proof_registry().unwrap();
    let mut reopened = open_session(&res, &registry, source, SourcePosition::START);
    let again = res.acquire_import_message(&mut reopened).unwrap().unwrap();
    assert_eq!(published(&res, &again), bodies[0]);
    registry.finish().unwrap();

    let zero_dir = scratch.path().join("zero");
    let zero = encode_mbox("0.mbox", &[]);
    write_mboxrd_bundle(&zero_dir, &[zero], &[0], &[0], &mut |_, _, _| {});
    let zero_sources = inventory_sources(&res, &zero_dir);
    let registry = res.manifest_proof_registry().unwrap();
    let mut zero_session = open_session(&res, &registry, &zero_sources[0], SourcePosition::START);
    assert!(
        res.acquire_import_message(&mut zero_session)
            .unwrap()
            .is_none()
    );
    let error = res
        .open_bundle_acquisition(
            &registry,
            &zero_sources[0],
            SourcePosition::new(0, 1),
            |_| ControlFlow::Continue(()),
        )
        .unwrap_err();
    assert!(
        matches!(error, StoreError::InvalidInput { .. }),
        "{error:?}"
    );
    registry.finish().unwrap();
    res.close().unwrap();
}

#[test]
fn duplicate_mboxrd_bytes_keep_separate_occurrences_and_replay() {
    let (scratch, res) = resources("mboxrd-duplicates");
    let dir = scratch.path().join("bundle");
    let message = b"From: sender@example.test\r\nSubject: duplicate\r\n\r\nsame\r\n";
    let bodies = vec![message.to_vec(), message.to_vec()];
    let file = encode_mbox("0.mbox", &bodies);
    write_mboxrd_bundle(&dir, &[file], &[0], &[0], &mut |_, _, _| {});
    let sources = inventory_sources(&res, &dir);
    let registry = res.manifest_proof_registry().unwrap();
    let messages = acquire_all(&res, &registry, &sources[0]);
    assert_eq!(messages.len(), 2);
    assert_eq!(
        messages[0].message().object.id().sha256(),
        messages[1].message().object.id().sha256()
    );
    assert_ne!(messages[0].message().offset, messages[1].message().offset);
    let mut messages = messages;
    let second = messages.pop().unwrap().into_message();
    let first = messages.pop().unwrap().into_message();

    let revision = running_job(&res, &sources);
    let mailbox = sources[0].mailbox().to_owned();
    commit(&res, revision, 0, mailbox.clone(), vec![first]).unwrap();
    assert_eq!(reservations(&res), 1);
    commit(&res, revision + 1, 0, mailbox.clone(), vec![second]).unwrap();
    assert_eq!(reservations(&res), 0);
    assert_eq!(rows(&res, "acquisitions"), 1);
    assert_eq!(rows(&res, "acquisition_sources"), 2);
    assert_eq!(rows(&res, "occurrences"), 2);

    let mut replay = acquire_all(&res, &registry, &sources[0]);
    assert_eq!(published(&res, &replay[0]), message);
    assert_eq!(reservations(&res), 2);
    let replay = replay.pop().unwrap().into_message();
    commit(&res, revision + 2, 0, mailbox, vec![replay]).unwrap();
    assert_eq!(reservations(&res), 1);
    assert_eq!(rows(&res, "acquisition_sources"), 2);
    assert_eq!(rows(&res, "occurrences"), 2);
    registry.finish().unwrap();
    res.close().unwrap();
}

#[test]
fn two_valid_framing_interpretations_are_distinct_sources() {
    let (scratch, res) = resources("mboxrd-framing-intent");
    let dir = scratch.path().join("bundle");
    let body = b"body\n".to_vec();
    let file = encode_mbox("0.mbox", &[body]);
    write_mboxrd_bundle(
        &dir,
        std::slice::from_ref(&file),
        &[0],
        &[0],
        &mut |_, _, _| {},
    );
    let first_sources = inventory_sources(&res, &dir);
    let registry = res.manifest_proof_registry().unwrap();
    let first = acquire_all(&res, &registry, &first_sources[0]).remove(0);
    assert_eq!(published(&res, &first), b"body\n");
    assert_eq!(
        first.message().bundle_framing.map(|f| f.framing_lf_added),
        Some(false)
    );

    write_mboxrd_bundle(&dir, &[file], &[0], &[0], &mut |_, _, d| {
        d.original_bytes = 4;
        d.sha256 = Sha256::digest(b"body").into();
        d.framing_lf_added = true;
    });
    let second_sources = inventory_sources(&res, &dir);
    assert_eq!(second_sources[0].path(), first_sources[0].path());
    let second = acquire_all(&res, &registry, &second_sources[0]).remove(0);
    assert_eq!(published(&res, &second), b"body");
    assert_eq!(
        second.message().bundle_framing.map(|f| f.framing_lf_added),
        Some(true)
    );

    let revision = running_job(&res, &[first_sources[0].clone(), second_sources[0].clone()]);
    commit(
        &res,
        revision,
        0,
        first_sources[0].mailbox().to_owned(),
        vec![first.into_message()],
    )
    .unwrap();
    commit(
        &res,
        revision + 1,
        1,
        second_sources[0].mailbox().to_owned(),
        vec![second.into_message()],
    )
    .unwrap();
    assert_eq!(rows(&res, "acquisition_sources"), 2);
    assert_eq!(rows(&res, "occurrences"), 2);
    registry.finish().unwrap();
    res.close().unwrap();
}

#[test]
fn mboxrd_cancellation_stops_before_the_session() {
    let (scratch, res) = resources("mboxrd-cancel");
    let dir = scratch.path().join("bundle");
    let file = encode_mbox("0.mbox", &[body_for(0)]);
    write_mboxrd_bundle(
        &dir,
        std::slice::from_ref(&file),
        &[0],
        &[0],
        &mut |_, _, _| {},
    );
    let sources = inventory_sources(&res, &dir);
    let registry = res.manifest_proof_registry().unwrap();
    let mut calls = 0;
    let stopped = res
        .open_bundle_acquisition(&registry, &sources[0], SourcePosition::START, |_| {
            calls += 1;
            ControlFlow::Break(())
        })
        .unwrap();
    assert!(matches!(stopped, Scoped::Stopped) && calls == 1);
    let message = acquire_all(&res, &registry, &sources[0]).remove(0);
    assert_eq!(published(&res, &message), body_for(0));
    registry.finish().unwrap();
    res.close().unwrap();
}

#[cfg(feature = "test-seams")]
#[test]
fn shuffled_mboxrd_sources_acquire_with_linear_reads_and_no_manifest() {
    use sb_test_support::{FaultFs, FaultKind, FsOp, real_fs};

    let scratch = ScratchDir::new("mboxrd-shuffled");
    let root = scratch.path().join("store");
    let dir = scratch.path().join("bundle");
    let mut files = Vec::new();
    let mut expected = Vec::new();
    for file_index in 0..8 {
        let bodies: Vec<Vec<u8>> = (0..16)
            .map(|record| {
                format!(
                    "From: {file_index}-{record}@example.test\nSubject: s\n\n{}\n",
                    "x".repeat((file_index * 16 + record) % 4096)
                )
                .into_bytes()
            })
            .collect();
        expected.extend(bodies.clone());
        files.push(encode_mbox(&format!("{file_index}.mbox"), &bodies));
    }
    let order = [0, 1, 2, 3, 4, 5, 6, 7];
    write_mboxrd_bundle(&dir, &files, &order, &order, &mut |_, _, _| {});

    let probe = std::sync::Arc::new(FaultFs::new(
        real_fs(),
        FsOp::Read,
        u64::MAX,
        FaultKind::Eio,
    ));
    let res = Resources::open_with_fs(&root, probe.clone()).unwrap();
    let sources = inventory_sources(&res, &dir);
    let registry = res.manifest_proof_registry().unwrap();
    assert_eq!(acquire_all(&res, &registry, &sources[0]).len(), 16);
    let token = sources[0].bundle().unwrap().manifest().token().to_owned();
    fs::remove_file(root.join("recovery").join(format!("import-source-{token}"))).unwrap();

    let reads = probe.calls(FsOp::Read);
    let seeks = probe.calls(FsOp::Seek);
    let mut acquired = 0;
    for source in sources[1..].iter().rev() {
        let file_index: usize = source
            .path()
            .file_stem()
            .unwrap()
            .to_str()
            .unwrap()
            .parse()
            .unwrap();
        for (record, message) in acquire_all(&res, &registry, source).iter().enumerate() {
            assert_eq!(published(&res, message), expected[file_index * 16 + record]);
            acquired += 1;
        }
    }
    assert_eq!(acquired, 7 * 16);
    let reads = probe.calls(FsOp::Read) - reads;
    let seeks = probe.calls(FsOp::Seek) - seeks;
    assert!(
        reads <= 32 * acquired + 64,
        "reads grew nonlinearly: {reads}"
    );
    assert!(
        seeks <= 16 * acquired + 64,
        "seeks grew nonlinearly: {seeks}"
    );
    registry.finish().unwrap();
    res.close().unwrap();
}

#[cfg(feature = "test-seams")]
#[test]
fn mboxrd_faults_leave_no_final_object() {
    use sb_store::ObjectId;
    use sb_test_support::{FaultFs, FaultKind, FsOp, real_fs};

    for (op, kind) in [
        (FsOp::Read, FaultKind::Eio),
        (FsOp::Write, FaultKind::Eio),
        (FsOp::SyncFile, FaultKind::Eio),
        (FsOp::Rename, FaultKind::NoSpace),
    ] {
        let scratch = ScratchDir::new("mboxrd-fault");
        let root = scratch.path().join("store");
        let dir = scratch.path().join("bundle");
        let bodies = vec![b"From: a@example.test\nSubject: A\n\nbody\n".to_vec()];
        let file = encode_mbox("0.mbox", &bodies);
        write_mboxrd_bundle(
            &dir,
            std::slice::from_ref(&file),
            &[0],
            &[0],
            &mut |_, _, _| {},
        );

        let counting = std::sync::Arc::new(FaultFs::new(real_fs(), op, u64::MAX, FaultKind::Eio));
        let res = Resources::open_with_fs(&root, counting.clone()).unwrap();
        let sources = inventory_sources(&res, &dir);
        let registry = res.manifest_proof_registry().unwrap();
        let session = open_session(&res, &registry, &sources[0], SourcePosition::START);
        let baseline = counting.calls(op);
        drop(session);
        drop(registry);
        drop(res);

        let fs = std::sync::Arc::new(FaultFs::new(real_fs(), op, baseline + 1, kind));
        let res = Resources::open_with_fs(&root, fs.clone()).unwrap();
        let sources = inventory_sources(&res, &dir);
        let registry = res.manifest_proof_registry().unwrap();
        let mut session = open_session(&res, &registry, &sources[0], SourcePosition::START);
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
        assert_eq!(
            reservations(&res),
            if op == FsOp::Rename { 1 } else { 0 },
            "{op:?} reservations"
        );
        assert_eq!(rows(&res, "acquisitions"), 0, "{op:?} rows");
        let id = ObjectId::from_hex(&hex_of(&Sha256::digest(&bodies[0])), bodies[0].len() as u64)
            .unwrap();
        assert!(!res.store.objects().exists(&id).unwrap());
        drop(session);
        res.close().unwrap();
    }
}
