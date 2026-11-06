//! Verified bundle ingest through the real engine (M1.7d2c3, spec/09
//! "Export bundles"): a real export's destination directory re-imports
//! as manifest-verified sources: byte-identical acquisitions, exact
//! replay, durable checkpoints and resume, with failure evidence kept
//! and no plain-path fallback. One lazily created proof registry serves
//! every bundle source in the turn, mixed with plain sources.

use std::env;
use std::sync::Arc;

use sb_test_support::{CutFs, FaultFs, FaultKind, FsOp, real_fs, run_crash_cut_points};

use super::acceptance::{
    assert_intact, cut_points, finish_after_cut, outcome, states_before_recovery,
};
use super::exports::{complete_export, hashes, import_inbox, mailbox, published};
use super::*;

/// The fixture bodies' SHA-256s, in index order, and the
/// `ids_sha256` of each message order the tests write: the digest
/// commits to the occurrence ids in manifest order.
const BODIES: [(&[u8], &str); 3] = [
    (
        b"From: alice@example.test\nSubject: first note\n\nfirst body\n",
        "7768c23d5eb3a21eda791485939156869b86850cf56e3eabe460491182177c5e",
    ),
    (
        b"From: bob@example.test\nSubject: second note\n\nsecond body\n",
        "22dc0e5d22d025995c05193bb1742f8233d6e09bfe0186785946baabd683164e",
    ),
    (
        b"From: carol@example.test\nSubject: third note\n\nthird body\n",
        "360a94f2f7af62d239502b297b502cf5c3e04389dab15a39f4048c2f18932780",
    ),
];
const IDS_01: &str = "7c3ccd10bb7ec37b46d37926ae6274267f007a34aeaf15c882a715a7f3300529";
const IDS_012: &str = "5dfadd0e50910f561636c47335ecf8316251cbd85964eadb5c00103502edf177";
const IDS_210: &str = "a62ef3c0384d4e006e13a897fda6edcbb44be2764f822cfb984ac242e2efba54";

/// A tiny synthetic EML bundle in `dir`: `bodies.len()` `<index>.eml`
/// children plus a manifest whose `files`/`messages` arrays follow
/// `file_order`/`message_order` (permutations of `0..n`); `ids_sha256`
/// must match `message_order`'s digest or the manifest is invalid.
fn write_bundle(
    dir: &Path,
    bodies: &[(&[u8], &str)],
    file_order: &[usize],
    message_order: &[usize],
    ids_sha256: &str,
) {
    assert!(file_order.len() == bodies.len() && message_order.len() == bodies.len());
    fs::create_dir_all(dir).unwrap();
    for (index, (body, _)) in bodies.iter().enumerate() {
        fs::write(dir.join(format!("{index}.eml")), body).unwrap();
    }
    let message = |index: usize| {
        let (body, sha256) = bodies[index];
        serde_json::json!({
            "v":1, "occurrence_id":index as i64, "message_id":index as i64,
            "account":{"id":"a","name":"a"}, "mailbox":{"id":1,"name":"Inbox"},
            "occurrence_state":"present",
            "flags":{"seen":false,"flagged":false,"answered":false,"draft":false,"deleted":false},
            "keywords":[], "body_availability":"acquired", "tags":[], "note":null,
            "acquisition":{"id":index as i64,"sha256":sha256,"bytes":body.len()},
            "file":format!("{index}.eml")
        })
    };
    let manifest = serde_json::json!({
        "v":1, "kind":"export-manifest", "job":"j", "format":"eml",
        "destination":"/d", "scope":{"kind":"mailbox","id":1,"generation":1},
        "generation":1, "entries":bodies.len(), "ids_sha256":ids_sha256,
        "files":file_order.iter().map(|&index| serde_json::json!({
            "name":format!("{index}.eml"),"bytes":bodies[index].0.len(),"records":1
        })).collect::<Vec<_>>(),
        "messages":message_order.iter().map(|&index| message(index)).collect::<Vec<_>>(),
    });
    fs::write(dir.join("sweetbriar-metadata.json"), manifest.to_string()).unwrap();
}

/// Inventory, confirm and settle a bundle-dir import to `state`.
fn import_bundle(engine: &Engine, dir: &Path, state: ImportJobState) -> ImportJobStatus {
    let id = start(engine, std::slice::from_ref(&dir.to_path_buf()));
    settle(engine, &id, ImportJobState::AwaitingConfirmation);
    confirm(engine, &id, vec![]).unwrap();
    settle(engine, &id, state)
}

#[test]
fn registry_cleanup_failure_cannot_report_completed_ingest() {
    fn run(fault: u64) -> (u64, ImportJobStatus, usize) {
        let scratch = ScratchDir::new("bundle-registry-cleanup");
        let bundle = scratch.path().join("bundle");
        write_bundle(&bundle, &BODIES[..2], &[0, 1], &[0, 1], IDS_01);
        let root = scratch.path().join("store");
        let fs = Arc::new(FaultFs::new(
            real_fs(),
            FsOp::RemoveDirAll,
            fault,
            FaultKind::Eio,
        ));
        let engine = block_on(Engine::open_with_fs(&root, fs.clone())).unwrap();
        let id = start(&engine, &[bundle]);
        settle(&engine, &id, ImportJobState::AwaitingConfirmation);
        confirm(&engine, &id, vec![]).unwrap();
        let deadline = std::time::Instant::now() + PATIENCE;
        let terminal = loop {
            let job = block_on(engine.import_job(&id)).unwrap();
            if matches!(
                job.state(),
                ImportJobState::Completed | ImportJobState::Failed
            ) {
                break job;
            }
            assert!(std::time::Instant::now() < deadline, "{job:?}");
            std::thread::sleep(Duration::from_millis(10));
        };
        // Drain accepted work even if its completed state became visible too early.
        block_on(engine.shutdown()).unwrap();
        (fs.calls(FsOp::RemoveDirAll), terminal, snapshots(&root))
    }
    let (cleanup, complete, _) = run(u64::MAX);
    assert_eq!(complete.state(), ImportJobState::Completed);
    assert!(cleanup > 0);
    // The last directory removal is the turn registry's explicit finish.
    let (_, failed, evidence) = run(cleanup);
    assert_eq!(failed.state(), ImportJobState::Failed);
    assert!(
        failed
            .checkpoint()
            .to_json()
            .contains("manifest proof registry cleanup failed")
    );
    assert!(
        evidence > 0,
        "failed cleanup must retain captured source evidence"
    );
}

#[test]
fn an_exported_eml_bundle_reimports_byte_identical_acquisitions() {
    let scratch = ScratchDir::new("bundle-eml-roundtrip");
    let engine = open(&scratch.path().join("store"));
    import_inbox(&engine);
    let before = hashes(&engine);
    let (mailbox_id, _) = mailbox(&engine);
    let dest = scratch.path().join("out");
    complete_export(&engine, mailbox_id, &dest, ExportFormat::Eml);
    assert!(dest.join("sweetbriar-metadata.json").is_file());
    block_on(engine.shutdown()).unwrap();

    let mirror = open(&scratch.path().join("mirror"));
    let done = import_bundle(&mirror, &dest, ImportJobState::Completed);
    let totals = done.checkpoint().totals();
    assert_eq!((totals.processed, totals.imported), (2, 2));
    assert_eq!(hashes(&mirror), before, "EML bundle re-imports identical");
    assert_eq!(count(&mirror, "SELECT count(*) FROM occurrences"), 2);
    assert_eq!(snapshots(&scratch.path().join("mirror")), 0);

    // Re-importing the same bundle replays by source identity: nothing new.
    let again = import_bundle(&mirror, &dest, ImportJobState::Completed);
    assert_eq!(again.checkpoint().totals().imported, 0);
    assert_eq!(hashes(&mirror), before);
    block_on(mirror.shutdown()).unwrap();
}

#[test]
fn an_exported_mboxrd_bundle_reimports_byte_identical_acquisitions() {
    let scratch = ScratchDir::new("bundle-mboxrd-roundtrip");
    let engine = open(&scratch.path().join("store"));
    import_inbox(&engine);
    let before = hashes(&engine);
    let (mailbox_id, _) = mailbox(&engine);
    let dest = scratch.path().join("out");
    complete_export(&engine, mailbox_id, &dest, ExportFormat::Mboxrd);
    block_on(engine.shutdown()).unwrap();

    let mirror = open(&scratch.path().join("mirror"));
    let done = import_bundle(&mirror, &dest, ImportJobState::Completed);
    let totals = done.checkpoint().totals();
    assert_eq!((totals.processed, totals.imported), (2, 2));
    assert_eq!(
        hashes(&mirror),
        before,
        "mboxrd bundle re-imports identical"
    );
    assert_eq!(count(&mirror, "SELECT count(*) FROM occurrences"), 2);

    let again = import_bundle(&mirror, &dest, ImportJobState::Completed);
    assert_eq!(again.checkpoint().totals().imported, 0);
    assert_eq!(hashes(&mirror), before);
    block_on(mirror.shutdown()).unwrap();
}

#[test]
fn shuffled_manifest_order_keeps_inventory_source_order() {
    let scratch = ScratchDir::new("bundle-shuffled");
    let dir = scratch.path().join("bundle");
    // `files` and `messages` both run 2,1,0: the inventory still lists
    // the directory's own order, and every source resolves through its
    // manifest ordinal.
    write_bundle(&dir, &BODIES, &[2, 1, 0], &[2, 1, 0], IDS_210);
    let engine = open(&scratch.path().join("store"));
    let done = import_bundle(&engine, &dir, ImportJobState::Completed);

    let names: Vec<String> = done
        .sources()
        .iter()
        .map(|source| {
            source
                .source()
                .path()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string()
        })
        .collect();
    assert_eq!(names, ["0.eml", "1.eml", "2.eml"]);
    assert_eq!(done.checkpoint().totals().imported, 3);
    for position in done.sources().iter().map(ImportSourceStatus::position) {
        assert_eq!(position.messages(), 1);
    }
    let mut expected: Vec<String> = BODIES.iter().map(|(_, sha)| (*sha).to_owned()).collect();
    expected.sort();
    assert_eq!(hashes(&engine), expected);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn mixed_plain_and_bundle_sources_ingest_in_order() {
    let scratch = ScratchDir::new("bundle-mixed");
    let dir = scratch.path().join("bundle");
    write_bundle(&dir, &BODIES[..2], &[0, 1], &[0, 1], IDS_01);
    let loose = scratch.path().join("Loose.mbox");
    fs::write(&loose, mbox(2)).unwrap();
    let engine = open(&scratch.path().join("store"));
    let id = start(&engine, &[dir, loose]);
    let waiting = settle(&engine, &id, ImportJobState::AwaitingConfirmation);
    assert_eq!(waiting.sources().len(), 3);
    confirm(&engine, &id, vec![]).unwrap();
    let done = settle(&engine, &id, ImportJobState::Completed);
    let names: Vec<String> = done
        .sources()
        .iter()
        .map(|source| {
            source
                .source()
                .path()
                .file_name()
                .unwrap()
                .to_string_lossy()
                .to_string()
        })
        .collect();
    assert_eq!(names, ["0.eml", "1.eml", "Loose.mbox"]);
    assert_eq!(done.checkpoint().totals().imported, 4);
    assert_eq!(count(&engine, "SELECT count(*) FROM occurrences"), 4);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_wrong_manifest_hash_fails_with_snapshots_kept_and_no_plain_fallback() {
    let scratch = ScratchDir::new("bundle-wrong-hash");
    let dir = scratch.path().join("bundle");
    // Structurally valid, but one declared SHA-256 is wrong: the verified
    // open refuses before any message publishes.
    write_bundle(&dir, &BODIES[..2], &[0, 1], &[0, 1], IDS_01);
    let manifest = dir.join("sweetbriar-metadata.json");
    let text = fs::read_to_string(&manifest).unwrap();
    fs::write(&manifest, text.replacen(BODIES[0].1, &"f".repeat(64), 1)).unwrap();
    let root = scratch.path().join("store");
    let engine = open(&root);
    let failed = import_bundle(&engine, &dir, ImportJobState::Failed);

    let failure = failed.checkpoint().failure().unwrap();
    assert!(
        failure.contains("does not match its captured bytes"),
        "{failure}"
    );
    assert_eq!(failed.checkpoint().totals().processed, 0);
    assert!(
        failed
            .sources()
            .iter()
            .all(|s| s.position() == SourcePosition::START)
    );
    assert_eq!(count(&engine, "SELECT count(*) FROM occurrences"), 0);
    // Failure evidence: the member snapshots and the shared manifest stay.
    assert_eq!(snapshots(&root), 3);
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn a_corrupt_bundle_record_fails_mid_ingest_keeping_evidence() {
    let scratch = ScratchDir::new("bundle-corrupt-record");
    let engine = open(&scratch.path().join("store"));
    import_inbox(&engine);
    let (mailbox_id, _) = mailbox(&engine);
    let dest = scratch.path().join("out");
    complete_export(&engine, mailbox_id, &dest, ExportFormat::Mboxrd);
    // Corrupt inside the first record's encoded range: the verified
    // session fails the record instead of publishing it. A byte outside
    // the declared ranges is legitimately never read.
    let member = published(&dest)
        .into_iter()
        .find(|name| name.ends_with(".mbox"))
        .map(|name| dest.join(name))
        .unwrap();
    let manifest: serde_json::Value =
        serde_json::from_str(&fs::read_to_string(dest.join("sweetbriar-metadata.json")).unwrap())
            .unwrap();
    let record = &manifest["messages"][0];
    let offset = record["record_offset"].as_u64().unwrap() as usize;
    let encoded = record["encoded_bytes"].as_u64().unwrap() as usize;
    let mut bytes = fs::read(&member).unwrap();
    assert!(offset + encoded <= bytes.len());
    bytes[offset + encoded / 2] ^= 0xFF;
    fs::write(&member, &bytes).unwrap();
    block_on(engine.shutdown()).unwrap();

    let root = scratch.path().join("mirror");
    let mirror = open(&root);
    let failed = import_bundle(&mirror, &dest, ImportJobState::Failed);
    assert!(failed.checkpoint().failure().is_some());
    assert_eq!(count(&mirror, "SELECT count(*) FROM occurrences"), 0);
    assert!(snapshots(&root) >= 2, "a failed job keeps its snapshots");
    block_on(mirror.shutdown()).unwrap();
}

#[test]
fn a_running_bundle_job_resumes_across_shutdown_without_duplicates() {
    let scratch = ScratchDir::new("bundle-resume");
    let dir = scratch.path().join("bundle");
    write_bundle(&dir, &BODIES, &[0, 1, 2], &[0, 1, 2], IDS_012);
    let root = scratch.path().join("store");
    let engine = open(&root);
    let id = plant_bundle_running(&engine, &dir, 1);

    // Shutdown with the worker parked at task entry: the planted commit
    // stands, the rest is owed.
    let (entered, reached) = mpsc::channel();
    *engine
        .imports()
        .before_task
        .lock()
        .expect("a panicking thread held the import job registry") =
        Some(Arc::new(move |shutting_down| {
            entered.send(()).unwrap();
            while !shutting_down() {
                std::thread::yield_now();
            }
        }));
    engine.recover_imports().unwrap();
    reached.recv_timeout(PATIENCE).unwrap();
    assert_eq!(status(&engine, &id).checkpoint().totals().processed, 1);
    block_on(engine.shutdown()).unwrap();

    let engine = open(&root);
    let done = settle(&engine, &id, ImportJobState::Completed);
    let totals = done.checkpoint().totals();
    assert_eq!((totals.processed, totals.imported), (3, 3));
    assert_eq!(count(&engine, "SELECT count(*) FROM occurrences"), 3);
    assert_eq!(
        count(&engine, "SELECT count(*) FROM acquisition_sources"),
        3
    );
    block_on(engine.shutdown()).unwrap();
}

/// Plant a running bundle job with `committed` messages of source 0
/// already committed through the real verified session.
fn plant_bundle_running(engine: &Engine, dir: &Path, committed: u64) -> String {
    let ImportInventoryOutcome::Complete(inventory) = engine
        .resources()
        .inventory_import_sources(&[dir.to_path_buf()], |_| ControlFlow::Continue(()))
    else {
        panic!("inventory cancelled");
    };
    let sources = inventory.sources().to_vec();
    let registry = engine.resources().manifest_proof_registry().unwrap();
    let mut acquired = Vec::new();
    {
        let mut acquisition = match engine.resources().open_bundle_acquisition(
            &registry,
            &sources[0],
            SourcePosition::START,
            |_| ControlFlow::Continue(()),
        ) {
            Ok(Scoped::Done(acquisition)) => acquisition,
            other => panic!("the bundle session did not open: {other:?}"),
        };
        for _ in 0..committed {
            acquired.push(
                engine
                    .resources()
                    .acquire_import_message(&mut acquisition)
                    .unwrap()
                    .unwrap(),
            );
        }
    }
    registry.finish().unwrap();
    let position = acquired.last().unwrap().next_position();
    let id = engine.resources().new_uuid_string();
    let job = id.clone();
    let now = engine.resources().now();
    let paths = vec![dir.to_path_buf()];
    block_on(engine.resources().write(WritePriority::Sync, move |tx| {
        create_import_job(tx, &job, &paths, now)?;
        record_import_sources(tx, &job, 0, &sources)?;
        let checkpoint = ImportJobCheckpoint::inventoried(&sources, &[]);
        let record = advance_import_job(
            tx,
            &job,
            1,
            ImportJobState::AwaitingConfirmation,
            &checkpoint,
            now,
        )?;
        let record = advance_import_job(
            tx,
            &job,
            record.revision(),
            ImportJobState::Running,
            record.checkpoint(),
            now,
        )?;
        let messages: Vec<_> = acquired
            .into_iter()
            .map(AcquiredMessage::into_message)
            .collect();
        commit_import_job_batch(
            tx,
            &ImportJobBatch {
                job_id: &job,
                revision: record.revision(),
                ordinal: 0,
                mailbox: sources[0].mailbox(),
                messages: &messages,
                position,
                flags_incomplete: 0,
            },
            now,
        )
        .map(drop)
    }))
    .unwrap();
    id
}

const CUT_N_ENV: &str = "SB_BUNDLE_CUT_N";
const CUT_ROOT_ENV: &str = "SB_BUNDLE_CUT_ROOT";

#[test]
fn bundle_imports_reach_the_uncut_result_after_crash_cuts() {
    let scratch = ScratchDir::new("bundle-crash-cuts");
    let bundle = scratch.path().join("bundle");
    write_bundle(&bundle, &BODIES, &[0, 1, 2], &[0, 1, 2], IDS_012);

    // An uncut run measures the ingest's operations, after the engine's
    // own startup, and what the finished job and the store hold.
    let uncut = Arc::new(CutFs::new(real_fs(), u64::MAX));
    let root = scratch.path().join("uncut");
    let engine = block_on(Engine::open_with_fs(&root, uncut.clone())).unwrap();
    let first = uncut.operations() + 1;
    let done = import_bundle(&engine, &bundle, ImportJobState::Completed);
    let expected = (hashes(&engine), outcome(&done));
    block_on(engine.shutdown()).unwrap();
    let points = cut_points(first, uncut.operations());

    let cuts = run_crash_cut_points(
        CUT_N_ENV,
        CUT_ROOT_ENV,
        "import::tests::bundles::bundle_crash_child",
        &points,
        &mut |n| scratch.path().join(format!("cut-{n}")).join("store"),
        &mut |root| {
            let before = states_before_recovery(root);
            let engine = open(root);
            let done = finish_after_cut(&engine, &before, std::slice::from_ref(&bundle));
            let found = (hashes(&engine), outcome(&done));
            assert_eq!(found, expected, "after a cut in {}", root.display());
            assert!(
                done.report_path().is_some(),
                "a completed cut-run publishes its report"
            );
            assert_intact(&engine, root);
            block_on(engine.shutdown()).unwrap();
        },
    );
    // Operation counts are deterministic, so every point must land.
    assert!(cuts.survived.is_empty(), "children outlived {cuts:?}");
    assert_eq!(cuts.killed, points);
}

/// The bundle crash-cut child: import the fixture bundle on `CutFs`,
/// killed at its counted operation. The bundle sits beside the store.
#[test]
#[ignore = "run by the crash-cut suite as its child"]
fn bundle_crash_child() {
    let n: u64 = env::var(CUT_N_ENV).unwrap().parse().unwrap();
    let root = PathBuf::from(env::var(CUT_ROOT_ENV).unwrap());
    let bundle = root.parent().unwrap().parent().unwrap().join("bundle");
    let engine = block_on(Engine::open_with_fs(
        &root,
        Arc::new(CutFs::new(real_fs(), n)),
    ))
    .unwrap();
    import_bundle(&engine, &bundle, ImportJobState::Completed);
    block_on(engine.shutdown()).unwrap();
}
