//! `export_files` through Resources (spec/09 "Export file output"):
//! exact bytes, the version-1 manifest, unavailable bodies, refusals,
//! cancellation, resume and durability cuts.

use std::collections::BTreeSet;
use std::ops::ControlFlow;
use std::os::unix::fs::MetadataExt;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};
use std::thread;

use sb_resources::{
    ExportCaptureRequest, ExportFormat, ExportJob, ExportJobState, ExportScope, ExportSearchSort,
    ExportSelection, Query, Resources, Scoped, SearchScope, StoreError, Transaction, WritePriority,
    advance_export_job, append_export_chunks, create_export_job, export_job, list_generation,
    search_generation,
};
use sb_test_support::{ScratchDir, block_on};
use serde_json::{Value, json};
use sha2::{Digest, Sha256};

const BODY_A: &[u8] = b"From: a@example.test\nSubject: Alpha\n\nalpha body\n";
const BODY_B: &[u8] = b"From: b@example.test\nSubject: Beta\n\n>From inside\nFrom edge";
const BODY_C: &[u8] = b"";
const BODY_B_MBOX: &[u8] = b"From: b@example.test\nSubject: Beta\n\n>>From inside\n>From edge\n";
const ENVELOPE: &[u8] = b"From MAILER-DAEMON Thu Jan  1 00:00:00 1970\n";
const MISSING_SHA: &str = "aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa";
const QUARANTINED_SHA: &str = "bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb";
const EMPTY_HASH: &str = "e3b0c44298fc1c149afbf4c8996fb92427ae41e4649b934ca495991b7852b855";
const CLAIM: &str = "sweetbriar-export.claim";
const MANIFEST: &str = "sweetbriar-metadata.json";

fn write<T: Send + 'static>(
    res: &Resources,
    f: impl FnOnce(&Transaction) -> Result<T, StoreError> + Send + 'static,
) -> T {
    block_on(res.write(WritePriority::User, f)).unwrap()
}

fn sha_hex(bytes: &[u8]) -> String {
    let digest: [u8; 32] = Sha256::digest(bytes).into();
    digest.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn repair_relationship_proof(manifest: &mut Value) {
    let messages = manifest["messages"].as_array().unwrap();
    let mut digest = Sha256::new();
    for message in messages {
        digest.update(message["occurrence_id"].as_i64().unwrap().to_be_bytes());
    }
    let entries = messages.len();
    let references = messages
        .iter()
        .filter_map(|message| {
            Some((
                message["file"].as_str()?.to_owned(),
                message["acquisition"]["bytes"].as_u64()?,
            ))
        })
        .collect::<Vec<_>>();
    manifest["entries"] = json!(entries);
    manifest["ids_sha256"] = json!(hex(digest.finalize().as_slice()));
    let eml = manifest["format"] == "eml";
    let files = manifest["files"].as_array_mut().unwrap();
    if eml {
        files.retain(|file| references.iter().any(|(name, _)| file["name"] == *name));
    }
    for file in files {
        let matches = references
            .iter()
            .filter(|(name, _)| file["name"] == *name)
            .collect::<Vec<_>>();
        file["records"] = json!(matches.len());
        if eml && let Some((_, bytes)) = matches.first() {
            file["bytes"] = json!(bytes);
        }
    }
}

fn object_path(root: &Path, bytes: &[u8]) -> PathBuf {
    let hex = sha_hex(bytes);
    root.join("objects/sha256")
        .join(&hex[..2])
        .join(&hex[2..4])
        .join(&hex)
}

/// Drop `bytes` at the content-addressed path a publication would own
/// (spec/03): the fixture stands in for an earlier acquire.
fn place_object(root: &Path, bytes: &[u8]) -> String {
    let path = object_path(root, bytes);
    std::fs::create_dir_all(path.parent().unwrap()).unwrap();
    std::fs::write(&path, bytes).unwrap();
    sha_hex(bytes)
}

/// Three mailboxes, seven messages and every availability state:
/// acquired bodies A/B/C (C empty), recorded `missing` and
/// `quarantined` acquisitions, a never-acquired message, message 7
/// sharing message 1's body so mboxrd writes it twice, and mailbox 3
/// holding no occurrence at all. Crash-cut children re-seed, so the
/// whole fixture is idempotent.
fn seed(res: &Resources, root: &Path) {
    let (seeded, account): (bool, String) = res
        .read(|tx| {
            Ok(tx
                .query_row(
                    "SELECT EXISTS(SELECT 1 FROM occurrences), (SELECT account_id FROM accounts WHERE provider='local_archive')",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap())
        })
        .unwrap();
    if seeded {
        return;
    }
    let a = place_object(root, BODY_A);
    let b = place_object(root, BODY_B);
    let c = place_object(root, BODY_C);
    let (la, lb) = (BODY_A.len(), BODY_B.len());
    write(res, move |tx| {
        tx.execute_batch(&format!(
            r#"INSERT INTO mailboxes (mailbox_id,account_id,remote_name,kind,selectable,sync_state) VALUES (1,'{account}','Inbox','folder',1,'synced'),(2,'{account}','Archive Böx','folder',1,'synced'),(3,'{account}','Empty','folder',1,'synced');
INSERT INTO acquisitions (acquisition_id,content_sha256,byte_length,object_state,first_acquired_at) VALUES (1,'{a}',{la},'published',1),(2,'{b}',{lb},'published',1),(3,'{c}',0,'published',1),(4,'{MISSING_SHA}',9,'missing',1),(5,'{QUARANTINED_SHA}',11,'quarantined',1);
INSERT INTO logical_messages (message_id,account_id,acquisition_id,created_at) VALUES (1,'{account}',1,1),(2,'{account}',2,1),(3,'{account}',3,1),(4,'{account}',4,1),(5,'{account}',5,1),(6,'{account}',NULL,1),(7,'{account}',1,1);
INSERT INTO occurrences (occurrence_id,account_id,mailbox_id,message_id,internal_date,keywords,state,first_seen_at,last_seen_at) VALUES (1,'{account}',1,1,40,'[]','present',1,1),(2,'{account}',1,2,30,'[]','present',1,1),(3,'{account}',1,3,20,'[]','present',1,1),(4,'{account}',1,4,10,'[]','present',1,1),(5,'{account}',1,5,5,'[]','present',1,1),(6,'{account}',1,6,1,'[]','present',1,1),(7,'{account}',2,1,60,'[]','present',1,1),(8,'{account}',2,7,50,'[]','present',1,1);
UPDATE occurrences SET f_flagged=1 WHERE occurrence_id=1; UPDATE occurrences SET keywords='["$Forwarded","alpha"]' WHERE occurrence_id=2; INSERT INTO tags (tag_id,name,color_srgb) VALUES ('t','keep','#0a1b2c'); INSERT INTO message_tags (message_id,tag_id,added_at) VALUES (1,'t',1); INSERT INTO notes (note_id,message_id,body_source,created_at,updated_at) VALUES ('n',1,'frozen note',1,1);"#
        ))
        .unwrap();
        sb_store::refresh_list_rows(tx, &(1..=8).collect::<BTreeSet<_>>()).unwrap();
        Ok(())
    });
}

fn fixture() -> (ScratchDir, Resources, PathBuf) {
    let scratch = ScratchDir::new("export-files");
    let root = scratch.path().join("store");
    let res = Resources::open(&root).unwrap();
    seed(&res, &root);
    (scratch, res, root)
}

fn mailbox_scope(res: &Resources, mailbox: i64) -> ExportScope {
    ExportScope::Mailbox {
        mailbox_id: mailbox,
        expected_generation: res.read(list_generation).unwrap(),
    }
}

fn account_scope(res: &Resources) -> ExportScope {
    let account: String = res
        .read(|tx| {
            Ok(tx
                .query_row(
                    "SELECT account_id FROM accounts WHERE provider='local_archive'",
                    [],
                    |row| row.get(0),
                )
                .unwrap())
        })
        .unwrap();
    ExportScope::Account {
        account_id: account,
        expected_generation: res.read(list_generation).unwrap(),
    }
}

fn selection_scope(res: &Resources, ids: &[i64]) -> ExportScope {
    let generation = res.read(list_generation).unwrap();
    ExportScope::Selection(
        ExportSelection::new("sel".into(), 1, generation, Arc::from(ids)).unwrap(),
    )
}

fn capture(res: &Resources, id: &str, format: ExportFormat, dest: &Path, scope: ExportScope) {
    let request = ExportCaptureRequest::new(format, dest.display().to_string(), scope).unwrap();
    match res.capture_export(id, &request, |_| ControlFlow::Continue(())) {
        Ok(Scoped::Done(_)) => {}
        other => panic!("capture failed: {other:?}"),
    }
}

fn export(res: &Resources, id: &str) -> Vec<u64> {
    let mut progress = Vec::new();
    let done = res.export_files(id, |files| {
        progress.push(files);
        ControlFlow::Continue(())
    });
    assert!(
        matches!(done, Ok(Scoped::Done(_))),
        "export failed: {done:?}"
    );
    progress
}

fn stop_at(
    res: &Resources,
    id: &str,
    mut stop: impl FnMut(u64) -> bool,
) -> Result<Scoped<ExportJob>, StoreError> {
    res.export_files(id, |files| {
        if stop(files) {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    })
}

fn export_err(res: &Resources, id: &str) -> StoreError {
    match res.export_files(id, |_| ControlFlow::Continue(())) {
        Err(error) => error,
        Ok(done) => panic!("export unexpectedly succeeded: {done:?}"),
    }
}

fn assert_err(res: &Resources, id: &str, pred: impl Fn(&StoreError) -> bool) {
    let error = export_err(res, id);
    assert!(pred(&error), "{id}: {error:?}");
}

fn assert_stopped(result: Result<Scoped<ExportJob>, StoreError>) {
    assert!(matches!(result, Ok(Scoped::Stopped)), "{result:?}");
}

/// Nothing reached the destination but the claim and the lease's lock:
/// no final file, no partial, no manifest.
fn assert_only_claim(dest: &Path) {
    let mut names: Vec<String> = std::fs::read_dir(dest)
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .collect();
    names.sort();
    assert_eq!(names, ["sweetbriar-export.claim", "sweetbriar-export.lock"]);
}

fn manifest(dest: &Path) -> Value {
    serde_json::from_str(&std::fs::read_to_string(dest.join(MANIFEST)).unwrap()).unwrap()
}

fn job_state(res: &Resources, id: &str) -> ExportJobState {
    res.read(|tx| Ok(export_job(tx, id)?.state())).unwrap()
}

fn checkpoint(res: &Resources, id: &str) -> Value {
    res.read(|tx| Ok(serde_json::from_str(export_job(tx, id)?.checkpoint_json()).unwrap()))
        .unwrap()
}

fn ids_of(messages: &[Value]) -> Vec<i64> {
    messages
        .iter()
        .map(|m| m["occurrence_id"].as_i64().unwrap())
        .collect()
}

#[test]
fn eml_output_is_byte_exact_and_the_manifest_lists_every_occurrence() {
    let (scratch, res, _root) = fixture();
    let dest = scratch.path().join("eml-out");
    let generation = res.read(list_generation).unwrap();
    capture(
        &res,
        "eml",
        ExportFormat::Eml,
        &dest,
        ExportScope::Mailbox {
            mailbox_id: 1,
            expected_generation: generation,
        },
    );
    let progress = export(&res, "eml");

    for (occurrence, body) in [(1, BODY_A), (2, BODY_B), (3, BODY_C)] {
        assert_eq!(
            std::fs::read(dest.join(format!("{occurrence}.eml"))).unwrap(),
            body
        );
    }
    for unavailable in [4, 5, 6] {
        assert!(
            !dest.join(format!("{unavailable}.eml")).exists(),
            "{unavailable}"
        );
    }
    assert_eq!(std::fs::read(dest.join(CLAIM)).unwrap(), b"eml");
    assert_eq!(job_state(&res, "eml"), ExportJobState::Completed);
    let checkpoint = checkpoint(&res, "eml");
    assert_eq!(checkpoint["files"], 3);
    assert_eq!(
        checkpoint["manifest_bytes"].as_u64().unwrap(),
        std::fs::metadata(dest.join(MANIFEST)).unwrap().len()
    );

    let manifest = manifest(&dest);
    for (key, want) in [
        ("v", json!(1)),
        ("kind", json!("export-manifest")),
        ("job", json!("eml")),
        ("format", json!("eml")),
        ("destination", json!(dest.display().to_string())),
        ("entries", json!(6)),
        ("generation", json!(generation)),
    ] {
        assert_eq!(manifest[key], want, "{key}");
    }
    assert_eq!(
        manifest["scope"],
        json!({"kind":"mailbox","id":1,"generation":generation})
    );
    assert_eq!(
        manifest["files"],
        json!([
            {"name":"1.eml","bytes":BODY_A.len(),"records":1},
            {"name":"2.eml","bytes":BODY_B.len(),"records":1},
            {"name":"3.eml","bytes":0,"records":1},
        ])
    );

    let messages = manifest["messages"].as_array().unwrap();
    assert_eq!(ids_of(messages), [1, 2, 3, 4, 5, 6]);
    for (i, (file, availability)) in [
        (Some("1.eml"), "acquired"),
        (Some("2.eml"), "acquired"),
        (Some("3.eml"), "acquired"),
        (None, "missing"),
        (None, "quarantined"),
        (None, "not_acquired"),
    ]
    .into_iter()
    .enumerate()
    {
        assert_eq!(
            messages[i]["file"],
            file.map_or(Value::Null, Value::from),
            "{i}"
        );
        assert_eq!(messages[i]["body_availability"], availability, "{i}");
        // The record-span fields belong to mboxrd entries only.
        assert_eq!(messages[i].get("record_offset"), None, "{i}");
    }
    assert_eq!(messages[0]["flags"]["flagged"], true);
    assert_eq!(messages[0]["note"], "frozen note");
    assert_eq!(
        messages[0]["tags"][0],
        json!({"id":"t","name":"keep","color_srgb":"#0a1b2c"})
    );
    assert_eq!(messages[1]["keywords"], json!(["$Forwarded", "alpha"]));
    assert_eq!(messages[0]["acquisition"]["sha256"], sha_hex(BODY_A));
    assert_eq!(progress.last(), Some(&3));
}

#[test]
fn mboxrd_quotes_frames_and_writes_one_file_per_mailbox() {
    let (scratch, res, _root) = fixture();
    let dest = scratch.path().join("mbox-out");
    capture(
        &res,
        "mbox",
        ExportFormat::Mboxrd,
        &dest,
        selection_scope(&res, &[4, 1, 3, 6, 2, 5, 8, 7]),
    );
    export(&res, "mbox");

    let record_a = [ENVELOPE, BODY_A].concat();
    let record_b = [ENVELOPE, BODY_B_MBOX].concat();
    // The framed record lands last: its added LF ends the file.
    let inbox = [record_a.as_slice(), ENVELOPE, record_b.as_slice()].concat();
    let archive = [&record_a, &record_a].map(Vec::as_slice).concat();
    assert_eq!(std::fs::read(dest.join("1-Inbox.mbox")).unwrap(), inbox);
    // "Archive Böx" folds per byte: space and both 'ö' bytes to `_`.
    assert_eq!(
        std::fs::read(dest.join("2-Archive_B__x.mbox")).unwrap(),
        archive
    );

    let sidecar = manifest(&dest);
    assert_eq!(sidecar["entries"], 8);
    let messages = sidecar["messages"].as_array().unwrap();
    assert_eq!(ids_of(messages), [4, 1, 3, 6, 2, 5, 8, 7]);
    let by_id = |id: i64| messages.iter().find(|m| m["occurrence_id"] == id).unwrap();
    for id in [4, 5, 6] {
        assert_eq!(by_id(id)["file"], Value::Null, "{id}");
        assert_eq!(by_id(id).get("record_offset"), None, "{id}");
    }
    let span_a = record_a.len() as u64;
    let span_b = record_b.len() as u64;
    let span_c = ENVELOPE.len() as u64;
    for (id, want) in [
        (1, ("1-Inbox.mbox", 0, span_a, BODY_A.len() as u64, false)),
        (3, ("1-Inbox.mbox", span_a, span_c, 0, false)),
        (
            2,
            (
                "1-Inbox.mbox",
                span_a + span_c,
                span_b,
                BODY_B.len() as u64,
                true,
            ),
        ),
        (
            8,
            ("2-Archive_B__x.mbox", 0, span_a, BODY_A.len() as u64, false),
        ),
        (
            7,
            (
                "2-Archive_B__x.mbox",
                span_a,
                span_a,
                BODY_A.len() as u64,
                false,
            ),
        ),
    ] {
        let m = by_id(id);
        let got = (
            m["file"].as_str().unwrap(),
            m["record_offset"].as_u64().unwrap(),
            m["encoded_bytes"].as_u64().unwrap(),
            m["original_bytes"].as_u64().unwrap(),
            m["framing_lf_added"].as_bool().unwrap(),
        );
        assert_eq!(got, want, "{id}");
    }
    assert_eq!(
        sidecar["files"],
        json!([
            {"name":"1-Inbox.mbox","bytes":inbox.len(),"records":3},
            {"name":"2-Archive_B__x.mbox","bytes":archive.len(),"records":2},
        ])
    );

    // A mailbox whose records are all unavailable still gets its file.
    let empty_dest = scratch.path().join("mbox-empty-out");
    capture(
        &res,
        "mbox-empty",
        ExportFormat::Mboxrd,
        &empty_dest,
        selection_scope(&res, &[4, 5, 6]),
    );
    export(&res, "mbox-empty");
    assert_eq!(std::fs::read(empty_dest.join("1-Inbox.mbox")).unwrap(), b"");
    assert_eq!(
        manifest(&empty_dest)["files"],
        json!([{"name":"1-Inbox.mbox","bytes":0,"records":0}])
    );
}

/// Scope-declared mailboxes freeze their file identity with membership
/// (spec/09): an explicitly scoped mailbox has no record to name it, so
/// the capture's mailbox set still plans its empty file. Account and
/// mailbox scopes declare theirs; member records land on the
/// pre-planned files.
#[test]
fn mboxrd_plans_one_file_per_scoped_mailbox() {
    let (scratch, res, _root) = fixture();
    let empty_dest = scratch.path().join("scoped-empty-out");
    capture(
        &res,
        "scoped-empty",
        ExportFormat::Mboxrd,
        &empty_dest,
        mailbox_scope(&res, 3),
    );
    let progress = export(&res, "scoped-empty");
    assert_eq!(std::fs::read(empty_dest.join("3-Empty.mbox")).unwrap(), b"");
    let sidecar = manifest(&empty_dest);
    assert_eq!(sidecar["entries"], 0);
    assert_eq!(sidecar["messages"], json!([]));
    assert_eq!(
        sidecar["files"],
        json!([{"name":"3-Empty.mbox","bytes":0,"records":0}])
    );
    assert_eq!(checkpoint(&res, "scoped-empty")["files"], 1);
    assert_eq!(progress.last(), Some(&1));

    // Account scope freezes every declared mailbox, including the
    // empty one; unavailable members still produce no records.
    let account_dest = scratch.path().join("scoped-account-out");
    capture(
        &res,
        "scoped-account",
        ExportFormat::Mboxrd,
        &account_dest,
        account_scope(&res),
    );
    export(&res, "scoped-account");
    let record_a = [ENVELOPE, BODY_A].concat();
    let record_b = [ENVELOPE, BODY_B_MBOX].concat();
    // Account order is mailbox id, then each mailbox's date order:
    // members A, framed B and empty C inside Inbox, A twice in Archive.
    let inbox = [record_a.as_slice(), record_b.as_slice(), ENVELOPE].concat();
    assert_eq!(
        std::fs::read(account_dest.join("1-Inbox.mbox")).unwrap(),
        inbox
    );
    assert_eq!(
        std::fs::read(account_dest.join("2-Archive_B__x.mbox")).unwrap(),
        [&record_a, &record_a].map(Vec::as_slice).concat()
    );
    assert_eq!(
        std::fs::read(account_dest.join("3-Empty.mbox")).unwrap(),
        b""
    );
    let sidecar = manifest(&account_dest);
    assert_eq!(
        sidecar["files"],
        json!([
            {"name":"1-Inbox.mbox","bytes":inbox.len(),"records":3},
            {"name":"2-Archive_B__x.mbox","bytes":record_a.len()*2,"records":2},
            {"name":"3-Empty.mbox","bytes":0,"records":0},
        ])
    );
    assert_eq!(
        ids_of(sidecar["messages"].as_array().unwrap()),
        [1, 2, 3, 4, 5, 6, 7, 8]
    );
}

/// A remote name folds per byte then truncates at the 128-byte cap
/// (spec/09): folding yields ASCII only, so the cut cannot split a
/// character. Uncapped, a 200-character CJK name folds to 600 `_`
/// bytes and the publish fails NAME_MAX on every run.
#[test]
fn mboxrd_caps_a_long_mailbox_name() {
    let (scratch, res, _root) = fixture();
    let dest = scratch.path().join("long-name-out");
    // 200 three-byte characters: 600 folded bytes before the cap.
    let remote = "\u{4ef6}".repeat(200);
    write(&res, move |tx| {
        tx.execute_batch(&format!(
            "INSERT INTO mailboxes (mailbox_id,account_id,remote_name,kind,selectable,sync_state)
             SELECT 4, account_id, '{remote}', 'folder', 1, 'synced' FROM accounts WHERE provider='local_archive';
             INSERT INTO logical_messages (message_id,account_id,acquisition_id,created_at)
             SELECT 9, account_id, 1, 1 FROM accounts WHERE provider='local_archive';
             INSERT INTO occurrences (occurrence_id,account_id,mailbox_id,message_id,internal_date,keywords,state,first_seen_at,last_seen_at)
             SELECT 9, account_id, 4, 9, 70, '[]', 'present', 1, 1 FROM accounts WHERE provider='local_archive';"
        ))
        .unwrap();
        sb_store::refresh_list_rows(tx, &BTreeSet::from([9])).unwrap();
        Ok(())
    });
    capture(
        &res,
        "long-name",
        ExportFormat::Mboxrd,
        &dest,
        mailbox_scope(&res, 4),
    );
    export(&res, "long-name");

    let name = format!("4-{}.mbox", "_".repeat(128));
    assert!(
        name.len() <= "4-".len() + 128 + ".mbox".len(),
        "{name} exceeds the folded-name budget"
    );
    let record_a = [ENVELOPE, BODY_A].concat();
    assert_eq!(std::fs::read(dest.join(&name)).unwrap(), record_a);
    let sidecar = manifest(&dest);
    assert_eq!(
        sidecar["files"],
        json!([{"name": name, "bytes": record_a.len(), "records": 1}])
    );
    let message = &sidecar["messages"][0];
    assert_eq!(message["file"], name);
    assert_eq!(message["record_offset"], 0);
    assert_eq!(message["encoded_bytes"], record_a.len() as u64);
}

#[test]
fn wrong_states_and_foreign_destinations_are_refused() {
    let (scratch, res, _root) = fixture();
    assert!(matches!(
        export_err(&res, "missing-job"),
        StoreError::NotFound { .. }
    ));

    // A still-capturing job cannot produce output; ended states refuse.
    let dest = scratch.path().join("refused-out");
    let request = ExportCaptureRequest::new(
        ExportFormat::Eml,
        dest.display().to_string(),
        mailbox_scope(&res, 1),
    )
    .unwrap();
    for (id, end) in [
        ("stopped", ExportJobState::Cancelled),
        ("failed", ExportJobState::Failed),
    ] {
        assert_stopped(res.capture_export(id, &request, |_| ControlFlow::Break(())));
        assert_err(&res, id, |e| matches!(e, StoreError::Conflict { .. }));
        let job = res.read(|tx| export_job(tx, id)).unwrap();
        let now = res.now();
        write(&res, move |tx| {
            advance_export_job(tx, id, job.revision(), end, "{\"v\":1}", now)
        });
        assert_err(&res, id, |e| matches!(e, StoreError::Conflict { .. }));
    }

    // A completed replay touches no output, even with the destination gone.
    let done_dest = scratch.path().join("done-out");
    capture(
        &res,
        "done",
        ExportFormat::Eml,
        &done_dest,
        mailbox_scope(&res, 1),
    );
    export(&res, "done");
    std::fs::remove_dir_all(&done_dest).unwrap();
    assert!(matches!(
        res.export_files("done", |_| ControlFlow::Continue(())),
        Ok(Scoped::Done(_))
    ));
    assert!(!done_dest.exists());

    // Claim violations refuse rather than overwrite foreign files.
    type DestSetup = fn(&Path);
    let cases: [(&str, DestSetup); 3] = [
        ("foreign", |d| {
            std::fs::create_dir(d).unwrap();
            std::fs::write(d.join("theirs"), b"x").unwrap();
        }),
        ("claimed", |d| {
            std::fs::create_dir(d).unwrap();
            std::fs::write(d.join(CLAIM), b"other-job").unwrap();
        }),
        ("file-dest", |d| std::fs::write(d, b"x").unwrap()),
    ];
    for (name, setup) in cases {
        let dest = scratch.path().join(format!("{name}-out"));
        setup(&dest);
        capture(&res, name, ExportFormat::Eml, &dest, mailbox_scope(&res, 1));
        assert_err(&res, name, |e| matches!(e, StoreError::InvalidInput { .. }));
        assert_eq!(job_state(&res, name), ExportJobState::Ready, "{name}");
    }
    assert_eq!(
        std::fs::read(scratch.path().join("foreign-out/theirs")).unwrap(),
        b"x"
    );
    assert_eq!(
        std::fs::read(scratch.path().join("claimed-out").join(CLAIM)).unwrap(),
        b"other-job"
    );

    let lost_dest = scratch.path().join("lost-claim-out");
    capture(
        &res,
        "lost",
        ExportFormat::Eml,
        &lost_dest,
        mailbox_scope(&res, 1),
    );
    assert_stopped(stop_at(&res, "lost", |files| files == 0));
    std::fs::remove_file(lost_dest.join(CLAIM)).unwrap();
    assert_err(&res, "lost", |e| {
        matches!(e, StoreError::Unavailable { .. })
    });
    assert!(!lost_dest.join("1.eml").exists());

    // The writer thread refuses before it can wait on its own queue.
    let thread_dest = scratch.path().join("thread-out");
    capture(
        &res,
        "threaded",
        ExportFormat::Eml,
        &thread_dest,
        mailbox_scope(&res, 1),
    );
    let res = Arc::new(res);
    let inside = Arc::clone(&res);
    write(&res, move |_tx| {
        assert!(matches!(
            inside.export_files("threaded", |_| ControlFlow::Continue(())),
            Err(StoreError::Internal { .. })
        ));
        Ok(())
    });
}

#[test]
fn missing_and_corrupt_objects_fail_before_any_manifest() {
    let (scratch, res, root) = fixture();
    let dest = scratch.path().join("broken-out");
    capture(
        &res,
        "broken",
        ExportFormat::Eml,
        &dest,
        selection_scope(&res, &[1, 2]),
    );
    std::fs::remove_file(object_path(&root, BODY_A)).unwrap();
    assert_err(&res, "broken", |e| matches!(e, StoreError::NotFound { .. }));
    assert_eq!(job_state(&res, "broken"), ExportJobState::Running);
    assert!(!dest.join(MANIFEST).exists());

    // Damaged bytes fail their verification pass and get quarantined.
    let corrupt_dest = scratch.path().join("corrupt-out");
    capture(
        &res,
        "corrupt",
        ExportFormat::Eml,
        &corrupt_dest,
        selection_scope(&res, &[2]),
    );
    let damaged = object_path(&root, BODY_B);
    std::fs::write(&damaged, b"corrupted!").unwrap();
    assert_err(&res, "corrupt", |e| matches!(e, StoreError::Corrupt { .. }));
    assert!(!damaged.exists());
    assert_eq!(job_state(&res, "corrupt"), ExportJobState::Running);
    assert!(!corrupt_dest.join("2.eml").exists());
    assert!(!corrupt_dest.join("2.eml.partial").exists());
}

/// The same object failures inside an mboxrd mailbox file: the typed
/// error stands, the partial is removed, and the committed-file
/// checkpoint and manifest never move (spec/09).
#[test]
fn mboxrd_object_errors_fail_before_any_manifest() {
    let (scratch, res, root) = fixture();

    // The second member's missing object unwrites the open partial:
    // file one's first record already streamed when A's open fails.
    let dest = scratch.path().join("broken-mbox-out");
    capture(
        &res,
        "broken-mbox",
        ExportFormat::Mboxrd,
        &dest,
        selection_scope(&res, &[2, 1]),
    );
    std::fs::remove_file(object_path(&root, BODY_A)).unwrap();
    assert_err(&res, "broken-mbox", |e| {
        matches!(e, StoreError::NotFound { .. })
    });
    assert_eq!(job_state(&res, "broken-mbox"), ExportJobState::Running);
    assert_eq!(
        checkpoint(&res, "broken-mbox"),
        json!({"v": 1, "output": 1, "files": 0})
    );
    assert_only_claim(&dest);

    // Damaged bytes fail their verification pass and get quarantined.
    let corrupt_dest = scratch.path().join("corrupt-mbox-out");
    capture(
        &res,
        "corrupt-mbox",
        ExportFormat::Mboxrd,
        &corrupt_dest,
        selection_scope(&res, &[2]),
    );
    let damaged = object_path(&root, BODY_B);
    std::fs::write(&damaged, b"corrupted!").unwrap();
    assert_err(&res, "corrupt-mbox", |e| {
        matches!(e, StoreError::Corrupt { .. })
    });
    assert!(!damaged.exists());
    assert_eq!(job_state(&res, "corrupt-mbox"), ExportJobState::Running);
    assert_eq!(checkpoint(&res, "corrupt-mbox")["files"], 0);
    assert_only_claim(&corrupt_dest);
}

#[test]
fn a_changed_capture_is_corrupt_and_never_rescoped() {
    let (scratch, res, _root) = fixture();
    let dest = scratch.path().join("tampered-out");
    let now = res.now();
    write(&res, move |tx| {
        let request = format!(r#"{{"v":1,"capture":1,"sha256":"{EMPTY_HASH}"}}"#);
        let job = create_export_job(tx, "tamper", &request, now)?;
        let mut stream = json!({
            "v": 1,
            "kind": "capture",
            "intent": {"v": 1, "format": "eml", "destination": "/elsewhere/out", "scope": {"kind": "mailbox", "id": 1, "generation": 0}},
            "generation": 0,
            "entries": 0,
            "ids_sha256": EMPTY_HASH,
        })
        .to_string()
        .into_bytes();
        stream.push(b'\n');
        let job = append_export_chunks(tx, "tamper", job.revision(), 0, &[stream], now)?;
        advance_export_job(
            tx,
            "tamper",
            job.revision(),
            ExportJobState::Ready,
            "{\"v\":1,\"capture\":1,\"entries\":0,\"chunks\":1}",
            now,
        )?;
        Ok(())
    });
    match export_err(&res, "tamper") {
        StoreError::Corrupt { detail } => assert!(detail.contains("changed"), "{detail}"),
        other => panic!("a changed capture was not corrupt: {other:?}"),
    }
    assert!(!dest.exists());
    assert_eq!(job_state(&res, "tamper"), ExportJobState::Ready);
}

/// A sealed ledger that names one occurrence twice would plan two
/// `<occurrence>.eml` files whose later publish overwrites the first
/// while the manifest lists both; the membership proof hashes repeats
/// too, so the plan itself must refuse the duplicate (spec/09 "Export
/// file output").
#[test]
fn a_repeated_occurrence_is_corrupt() {
    let (scratch, res, _root) = fixture();
    let dest = scratch.path().join("repeated-out");
    let intent = json!({
        "v": 1,
        "format": "eml",
        "destination": dest.display().to_string(),
        "scope": {"kind": "mailbox", "id": 1, "generation": 0},
    });
    let record = json!({
        "occurrence_id": 1,
        "body_availability": "acquired",
        "acquisition": {"sha256": sha_hex(BODY_A), "bytes": BODY_A.len()},
    })
    .to_string();
    let mut ids = Sha256::new();
    ids.update(1i64.to_be_bytes());
    ids.update(1i64.to_be_bytes());
    let header = json!({
        "v": 1,
        "kind": "capture",
        "intent": intent,
        "generation": 0,
        "entries": 2,
        "ids_sha256": hex(ids.finalize().as_slice()),
    })
    .to_string();
    let request = format!(
        r#"{{"v":1,"capture":1,"sha256":"{}"}}"#,
        sha_hex(&serde_json::to_vec(&intent).unwrap())
    );
    let stream = format!("{header}\n{record}\n{record}\n").into_bytes();
    let now = res.now();
    write(&res, move |tx| {
        let job = create_export_job(tx, "repeated", &request, now)?;
        let job = append_export_chunks(tx, "repeated", job.revision(), 0, &[stream], now)?;
        advance_export_job(
            tx,
            "repeated",
            job.revision(),
            ExportJobState::Ready,
            "{\"v\":1,\"capture\":1,\"entries\":2,\"chunks\":1}",
            now,
        )?;
        Ok(())
    });
    assert_err(
        &res,
        "repeated",
        |e| matches!(e, StoreError::Corrupt { detail } if detail.contains("repeat")),
    );
    assert_eq!(job_state(&res, "repeated"), ExportJobState::Ready);
    assert!(!dest.exists());
}

fn seed_out_of_scope_capture(
    res: &Resources,
    job_id: &str,
    destination: &Path,
    scope: Value,
    mailboxes: Option<Value>,
) {
    let intent = json!({
        "v": 1,
        "format": "mboxrd",
        "destination": destination.display().to_string(),
        "scope": scope,
    });
    let record = json!({
        "occurrence_id": 1,
        "mailbox": {"id": 2, "name": "Archive"},
        "body_availability": "acquired",
        "acquisition": {"sha256": sha_hex(BODY_A), "bytes": BODY_A.len()},
    })
    .to_string();
    let mut ids = Sha256::new();
    ids.update(1i64.to_be_bytes());
    let mut header = json!({
        "v": 1,
        "kind": "capture",
        "intent": intent,
        "generation": 0,
        "entries": 1,
        "ids_sha256": hex(ids.finalize().as_slice()),
    });
    if let Some(mailboxes) = mailboxes {
        header["mailboxes"] = mailboxes;
    }
    let request = format!(
        r#"{{"v":1,"capture":1,"sha256":"{}"}}"#,
        sha_hex(&serde_json::to_vec(&intent).unwrap())
    );
    let stream = format!("{}\n{record}\n", header).into_bytes();
    let now = res.now();
    let job = job_id.to_owned();
    write(res, move |tx| {
        let created = create_export_job(tx, &job, &request, now)?;
        let captured = append_export_chunks(tx, &job, created.revision(), 0, &[stream], now)?;
        advance_export_job(
            tx,
            &job,
            captured.revision(),
            ExportJobState::Ready,
            "{\"v\":1,\"capture\":1,\"entries\":1,\"chunks\":1}",
            now,
        )?;
        Ok(())
    });
}

/// A scope-declared capture bounds its file set: a member record
/// naming a mailbox the header did not declare is `Corrupt`, never a
/// silently grown export (spec/09).
#[test]
fn a_record_outside_the_declared_scope_is_corrupt() {
    let (scratch, res, _root) = fixture();
    let cases = [
        (
            "undeclared-nonempty-mailbox",
            json!({"kind": "mailbox", "id": 1, "generation": 0}),
            json!([{"id": 1, "name": "Inbox"}]),
        ),
        (
            "undeclared-empty-account",
            json!({"kind": "account", "id": "empty", "generation": 0}),
            json!([]),
        ),
        (
            "undeclared-empty-mailbox",
            json!({"kind": "mailbox", "id": 1, "generation": 0}),
            json!([]),
        ),
    ];

    for (job_id, scope, mailboxes) in cases {
        let dest = scratch.path().join(job_id);
        seed_out_of_scope_capture(&res, job_id, &dest, scope, Some(mailboxes));
        assert_err(
            &res,
            job_id,
            |e| matches!(e, StoreError::Corrupt { detail } if detail.contains("declared scope")),
        );
        assert_eq!(job_state(&res, job_id), ExportJobState::Ready);
        assert!(!dest.exists());
    }
}

/// Captures sealed before `mailboxes` existed derive their files from
/// member records. Field absence is the compatibility marker; an empty
/// declared account or mailbox set is not equivalent (spec/09).
#[test]
fn a_legacy_capture_without_mailboxes_uses_record_files() {
    let (scratch, res, _root) = fixture();
    let dest = scratch.path().join("legacy-no-mailboxes");
    seed_out_of_scope_capture(
        &res,
        "legacy-no-mailboxes",
        &dest,
        json!({"kind": "account", "id": "legacy", "generation": 0}),
        None,
    );

    export(&res, "legacy-no-mailboxes");

    assert_eq!(
        std::fs::read(dest.join("2-Archive.mbox")).unwrap(),
        [ENVELOPE, BODY_A].concat()
    );
}

/// A record line that is valid JSON but not an object cannot carry a
/// `file` field: the manifest pass reports `Corrupt`, never an indexing
/// panic (spec/09 "Export file output"). The plan pass already proved
/// every record an object, so only a changed sealed chunk reaches this;
/// the test drops the append-only trigger and edits the payload after
/// the single file commits, before the manifest re-reads it.
#[test]
fn a_nonobject_record_is_corrupt_not_a_panic() {
    let (scratch, res, _root) = fixture();
    let dest = scratch.path().join("nonobject-out");
    capture(
        &res,
        "nonobject",
        ExportFormat::Eml,
        &dest,
        selection_scope(&res, &[1]),
    );
    let result = res.export_files("nonobject", |files| {
        if files == 1 {
            write(&res, |tx| {
                tx.execute_batch("DROP TRIGGER export_chunks_immutable")
                    .unwrap();
                tx.execute(
                    "UPDATE export_job_chunks SET payload = ?1
                     WHERE job_id = 'nonobject' AND ordinal = 0",
                    (b"{}\n[]\n".as_slice(),),
                )
                .unwrap();
                Ok(())
            });
        }
        ControlFlow::Continue(())
    });
    match result {
        Err(StoreError::Corrupt { detail }) => {
            assert!(detail.contains("not an object"), "{detail}")
        }
        other => panic!("a nonobject record was not corrupt: {other:?}"),
    }
    assert_eq!(job_state(&res, "nonobject"), ExportJobState::Running);
    assert!(!dest.join(MANIFEST).exists());
}

#[test]
fn cancellation_and_resume_reproduce_the_same_output() {
    let (scratch, res, _root) = fixture();
    let dest = scratch.path().join("resume-out");
    capture(
        &res,
        "resume",
        ExportFormat::Eml,
        &dest,
        mailbox_scope(&res, 1),
    );

    assert_stopped(stop_at(&res, "resume", |files| files == 0));
    assert_eq!(std::fs::read(dest.join(CLAIM)).unwrap(), b"resume");
    assert!(!dest.join("1.eml").exists());
    assert_eq!(job_state(&res, "resume"), ExportJobState::Running);
    assert_eq!(checkpoint(&res, "resume")["files"], 0);

    assert_stopped(stop_at(&res, "resume", |files| files == 1));
    assert_eq!(std::fs::read(dest.join("1.eml")).unwrap(), BODY_A);
    assert!(!dest.join("2.eml").exists());
    assert_eq!(checkpoint(&res, "resume")["files"], 1);

    // Stop mid-file (on resume, the third report is inside file two):
    let mut calls = 0u32;
    assert_stopped(stop_at(&res, "resume", |_| {
        calls += 1;
        calls == 3
    }));
    assert!(!dest.join("2.eml").exists());
    assert!(!dest.join("2.eml.partial").exists());
    assert_eq!(checkpoint(&res, "resume")["files"], 1);

    std::fs::write(dest.join("1.eml"), b"different").unwrap();
    let progress = export(&res, "resume");
    for (occurrence, body) in [(1, BODY_A), (2, BODY_B), (3, BODY_C)] {
        assert_eq!(
            std::fs::read(dest.join(format!("{occurrence}.eml"))).unwrap(),
            body
        );
    }
    assert_eq!(manifest(&dest)["messages"].as_array().unwrap().len(), 6);
    assert_eq!(progress.last(), Some(&3));
    assert_eq!(job_state(&res, "resume"), ExportJobState::Completed);

    let mbox_dest = scratch.path().join("resume-mbox-out");
    capture(
        &res,
        "resume-mbox",
        ExportFormat::Mboxrd,
        &mbox_dest,
        selection_scope(&res, &[1, 3, 2, 7, 8]),
    );
    assert_stopped(stop_at(&res, "resume-mbox", |files| files == 2));
    let record_a = [ENVELOPE, BODY_A].concat();
    let record_b = [ENVELOPE, BODY_B_MBOX].concat();
    let inbox = [record_a.as_slice(), ENVELOPE, record_b.as_slice()].concat();
    let archive = [&record_a, &record_a].map(Vec::as_slice).concat();
    // The intact committed file verifies and is kept; the missing one
    // is republished identically.
    let kept = mbox_dest.join("1-Inbox.mbox");
    let alias = scratch.path().join("kept-inbox.mbox");
    std::fs::hard_link(&kept, &alias).unwrap();
    std::fs::remove_file(mbox_dest.join("2-Archive_B__x.mbox")).unwrap();
    export(&res, "resume-mbox");
    assert_eq!(
        std::fs::metadata(&alias).unwrap().ino(),
        std::fs::metadata(&kept).unwrap().ino(),
        "an intact committed mboxrd file was rewritten instead of kept"
    );
    assert_eq!(
        std::fs::read(mbox_dest.join("1-Inbox.mbox")).unwrap(),
        inbox
    );
    assert_eq!(
        std::fs::read(mbox_dest.join("2-Archive_B__x.mbox")).unwrap(),
        archive
    );
    let sidecar = manifest(&mbox_dest);
    assert_eq!(
        ids_of(sidecar["messages"].as_array().unwrap()),
        [1, 3, 2, 7, 8]
    );
    assert_eq!(job_state(&res, "resume-mbox"), ExportJobState::Completed);
}

/// A cancellation inside a mailbox file, not at a file boundary: the
/// open partial is removed, the committed-file checkpoint keeps its
/// count, and resume republishes the identical bytes (spec/09).
#[test]
fn mboxrd_cancellation_inside_a_file_leaves_no_output() {
    let (scratch, res, _root) = fixture();
    let dest = scratch.path().join("mbox-mid-stop-out");
    capture(
        &res,
        "mbox-mid-stop",
        ExportFormat::Mboxrd,
        &dest,
        selection_scope(&res, &[1, 3, 2, 7, 8]),
    );

    // Reports run once before output, once per encoded record inside
    // the open file, and once per commit. The third report lands
    // inside file one's encode, after its second record.
    let mut calls = 0u32;
    assert_stopped(stop_at(&res, "mbox-mid-stop", |_| {
        calls += 1;
        calls == 3
    }));
    assert_eq!(job_state(&res, "mbox-mid-stop"), ExportJobState::Running);
    assert_eq!(
        checkpoint(&res, "mbox-mid-stop"),
        json!({"v": 1, "output": 1, "files": 0})
    );
    assert_only_claim(&dest);

    export(&res, "mbox-mid-stop");
    let record_a = [ENVELOPE, BODY_A].concat();
    let record_b = [ENVELOPE, BODY_B_MBOX].concat();
    let inbox = [record_a.as_slice(), ENVELOPE, record_b.as_slice()].concat();
    assert_eq!(std::fs::read(dest.join("1-Inbox.mbox")).unwrap(), inbox);
    assert_eq!(
        std::fs::read(dest.join("2-Archive_B__x.mbox")).unwrap(),
        [&record_a, &record_a].map(Vec::as_slice).concat()
    );
    assert_eq!(job_state(&res, "mbox-mid-stop"), ExportJobState::Completed);
}

/// A second call for the same running job cannot enter the
/// destination: the lease rides on a lock file publication never
/// replaces, so the claim marker's own publish-rename cannot strand
/// the held lock on an unlinked inode (spec/09 ownership).
#[test]
fn overlap_with_the_running_job_is_refused() {
    let (scratch, res, _root) = fixture();
    let dest = scratch.path().join("overlap-out");
    capture(
        &res,
        "overlap",
        ExportFormat::Eml,
        &dest,
        mailbox_scope(&res, 1),
    );

    // Park the first writer inside its first progress report: the
    // claim is published and the lease is held while a second call
    // for the same job tries to enter.
    let res = Arc::new(res);
    let entered = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let first = thread::spawn({
        let res = res.clone();
        let (entered, release) = (entered.clone(), release.clone());
        move || {
            let mut parked = false;
            res.export_files("overlap", |_| {
                if !std::mem::replace(&mut parked, true) {
                    entered.wait();
                    release.wait();
                }
                ControlFlow::Continue(())
            })
        }
    });
    entered.wait();
    let second = res.export_files("overlap", |_| ControlFlow::Continue(()));
    // The published claim itself reads held for the write's whole
    // duration on a real filesystem.
    let relock = sb_test_support::real_fs().lock_exclusive(&dest.join(CLAIM));
    release.wait();
    let first = first.join().unwrap();
    assert!(
        matches!(relock, Err(sb_platform::LockError::Held)),
        "the published claim relocked mid-write"
    );

    match second {
        Err(StoreError::Unavailable { reason, .. }) => {
            assert!(reason.contains("held"), "{reason}")
        }
        other => panic!("the overlapping call entered: {other:?}"),
    }
    assert!(matches!(first, Ok(Scoped::Done(_))), "first: {first:?}");
    assert_eq!(std::fs::read(dest.join("1.eml")).unwrap(), BODY_A);
    assert_eq!(manifest(&dest)["entries"], 6);
}

/// A committed file must reproduce the encoder's framing, not just
/// its decoded bytes (spec/09): a `From ` separator whose bytes differ
/// from the fixed envelope, drifted record bytes and a lost framing
/// LF all mean republish identically.
#[test]
fn resume_republishes_files_failing_mboxrd_verification() {
    let (scratch, res, _) = fixture();
    type Damage = fn(&mut Vec<u8>);
    let damages: [(&str, Damage); 4] = [
        // MAILER-DAEMON -> XAILER-DAEMON: still splits, wrong bytes.
        ("envelope", |bytes| bytes[5] = b'X'),
        ("body", |bytes| bytes[ENVELOPE.len()] ^= 0xff),
        ("truncated", |bytes| {
            bytes.truncate(bytes.len() - 1);
        }),
        ("short-envelope", |bytes| bytes.truncate(10)),
    ];
    for (name, damage) in damages {
        let dest = scratch.path().join(format!("{name}-out"));
        capture(
            &res,
            name,
            ExportFormat::Mboxrd,
            &dest,
            selection_scope(&res, &[1]),
        );
        assert_stopped(stop_at(&res, name, |files| files == 1));
        let path = dest.join("1-Inbox.mbox");
        let expected = std::fs::read(&path).unwrap();
        let mut damaged = expected.clone();
        damage(&mut damaged);
        std::fs::write(&path, &damaged).unwrap();
        export(&res, name);
        assert_eq!(
            std::fs::read(&path).unwrap(),
            expected,
            "{name}: a failing verification kept damaged output"
        );
    }
}

#[test]
fn manifest_rejects_invalid_global_relationships() {
    let (scratch, res, _) = fixture();
    let mut accepted = Vec::new();
    for (job, format) in [
        ("relationships-eml", ExportFormat::Eml),
        ("relationships-mbox", ExportFormat::Mboxrd),
    ] {
        let dest = scratch.path().join(job);
        capture(&res, job, format, &dest, mailbox_scope(&res, 1));
        export(&res, job);
        let original = manifest(&dest);
        let mut cases = Vec::new();

        let mut changed = original.clone();
        changed["entries"] = json!(original["entries"].as_u64().unwrap() + 1);
        cases.push(("entries", changed));
        let mut changed = original.clone();
        changed["ids_sha256"] = json!("0".repeat(64));
        cases.push(("digest", changed));
        let mut changed = original.clone();
        changed["files"][0]["records"] =
            json!(original["files"][0]["records"].as_u64().unwrap() + 1);
        cases.push(("records", changed));
        let mut changed = original.clone();
        changed["messages"][0]["file"] = json!("unresolved");
        cases.push(("unresolved", changed));

        for (label, changed) in cases {
            std::fs::write(dest.join(MANIFEST), changed.to_string()).unwrap();
            let outcome = res.inventory_import_sources(std::slice::from_ref(&dest), |_| {
                ControlFlow::Continue(())
            });
            if outcome.inventory().problems()[0].kind() != sb_resources::ImportProblemKind::Failed {
                accepted.push(format!("{job}: {label}"));
            }
        }
    }
    assert!(accepted.is_empty(), "accepted: {accepted:?}");
}

#[test]
fn manifest_schema_rejects_invalid_writer_manifests() {
    let (scratch, res, _) = fixture();
    for (job, format) in [
        ("schema-eml", ExportFormat::Eml),
        ("schema-mbox", ExportFormat::Mboxrd),
    ] {
        let dest = scratch.path().join(job);
        capture(&res, job, format, &dest, mailbox_scope(&res, 1));
        export(&res, job);
        let original = manifest(&dest);
        let mut cases: Vec<(String, String)> = Vec::new();

        let spans = [
            "record_offset",
            "encoded_bytes",
            "original_bytes",
            "framing_lf_added",
        ];
        for availability in ["acquired", "not_acquired", "missing", "quarantined"] {
            for acquisition in [false, true] {
                for file in [false, true] {
                    for mask in 0..16 {
                        let mut changed = original.clone();
                        let message = changed["messages"][0].as_object_mut().unwrap();
                        message.insert("body_availability".into(), json!(availability));
                        if !acquisition {
                            message.insert("acquisition".into(), Value::Null);
                        }
                        if !file {
                            message.insert("file".into(), Value::Null);
                        }
                        for (index, span) in spans.iter().enumerate() {
                            message.remove(*span);
                            if mask & (1 << index) != 0 {
                                let value = match *span {
                                    "original_bytes" => {
                                        original["messages"][0]["acquisition"]["bytes"].clone()
                                    }
                                    "framing_lf_added" => json!(false),
                                    _ => json!(0),
                                };
                                message.insert((*span).into(), value);
                            }
                        }
                        let valid_pair = match availability {
                            "acquired" => acquisition && file,
                            "not_acquired" => !acquisition && !file,
                            _ => acquisition && !file,
                        };
                        let required = if availability == "acquired"
                            && matches!(format, ExportFormat::Mboxrd)
                        {
                            15
                        } else {
                            0
                        };
                        let valid_schema = valid_pair && mask == required;
                        if valid_schema {
                            repair_relationship_proof(&mut changed);
                        }
                        std::fs::write(dest.join(MANIFEST), changed.to_string()).unwrap();
                        let outcome = res
                            .inventory_import_sources(std::slice::from_ref(&dest), |_| {
                                ControlFlow::Continue(())
                            });
                        assert!(outcome.inventory().sources().is_empty());
                        let problem = &outcome.inventory().problems()[0];
                        assert_eq!(
                            problem.detail().contains("invalid manifest JSON"),
                            !valid_schema,
                            "{job}: messages[0] availability={availability} acquisition={acquisition} file={file} spans={mask:04b}: {problem:?}"
                        );
                    }
                }
            }
        }

        for state in [
            "present",
            "vanish_pending",
            "vanished",
            "epoch_stale",
            "superseded",
        ] {
            let mut changed = original.clone();
            changed["messages"][0]["occurrence_state"] = json!(state);
            changed["messages"][0]["occurrence_id"] = json!(i64::MIN);
            changed["messages"][0]["message_id"] = json!(i64::MAX);
            changed["messages"][0]["mailbox"]["id"] = json!(i64::MIN);
            changed["messages"][0]["acquisition"]["id"] = json!(i64::MAX);
            changed["messages"][0]["acquisition"]["bytes"] = json!(u64::MAX);
            if matches!(format, ExportFormat::Mboxrd) {
                changed["messages"][0]["record_offset"] = json!(u64::MAX);
                changed["messages"][0]["encoded_bytes"] = json!(u64::MAX);
                changed["messages"][0]["original_bytes"] = json!(u64::MAX);
            }
            if state == "present" {
                changed["messages"][0]["account"]["name"] = json!("x".repeat(100_000));
                let mut keywords = (0..300).map(|n| format!("{n:04}")).collect::<Vec<_>>();
                keywords.extend([
                    format!("{}a", "z".repeat(100_000)),
                    format!("{}b", "z".repeat(100_000)),
                ]);
                changed["messages"][0]["keywords"] = json!(keywords);
                let mut tags = (0..70).map(|n| json!({"id":format!("{n:04}"),"name":format!("{n:04}"),"color_srgb":null})).collect::<Vec<_>>();
                for suffix in ["a", "b"] {
                    tags.push(json!({"id":format!("{}{suffix}", "z".repeat(10_000)), "name":format!("{}{suffix}", "z".repeat(10_000)), "color_srgb":"#0123ab"}));
                }
                changed["messages"][0]["tags"] = json!(tags);
            }
            repair_relationship_proof(&mut changed);
            std::fs::write(dest.join(MANIFEST), changed.to_string()).unwrap();
            let outcome = res.inventory_import_sources(std::slice::from_ref(&dest), |_| {
                ControlFlow::Continue(())
            });
            assert!(outcome.inventory().sources().is_empty());
            assert!(
                !outcome.inventory().problems()[0]
                    .detail()
                    .contains("invalid manifest JSON"),
                "{job}: messages[0].occurrence_state {state}"
            );
        }

        for span in spans {
            for value in [json!(-1), json!(1.5), json!("bad")] {
                let mut changed = original.clone();
                changed["messages"][0][span] = value;
                cases.push((
                    format!("messages[0].{span}: invalid span"),
                    changed.to_string(),
                ));
            }
        }
        if matches!(format, ExportFormat::Mboxrd) {
            let mut changed = original.clone();
            changed["messages"][0]["original_bytes"] = json!(0);
            assert_ne!(
                changed["messages"][0]["original_bytes"],
                original["messages"][0]["acquisition"]["bytes"]
            );
            cases.push((
                "messages[0].original_bytes mismatch".into(),
                changed.to_string(),
            ));
        }

        for key in ["id", "name", "color_srgb"] {
            let tag = json!({"id":"a","name":"a","color_srgb":null});
            let mut missing = tag.clone();
            missing.as_object_mut().unwrap().remove(key);
            let mut changed = original.clone();
            changed["messages"][0]["tags"] = json!([missing]);
            cases.push((
                format!("messages[0].tags[0].{key}: missing"),
                changed.to_string(),
            ));
            changed["messages"][0]["tags"] = json!([tag.clone()]);
            let raw = tag.to_string();
            let escaped = format!("\\u{:04x}{}", key.as_bytes()[0], &key[1..]);
            let duplicate = raw.replacen('{', &format!("{{\"{escaped}\":{},", tag[key]), 1);
            cases.push((
                format!("messages[0].tags[0].{key}: escaped duplicate"),
                changed.to_string().replacen(&raw, &duplicate, 1),
            ));
        }

        for (field, value) in [
            ("v", json!(2)),
            ("occurrence_id", json!("1")),
            ("message_id", json!("1")),
            ("account", json!({})),
            ("mailbox", json!({})),
            ("occurrence_state", json!("unknown")),
            ("flags", json!({})),
            ("keywords", json!(["z", "a"])),
            ("acquisition", json!({})),
            ("body_availability", json!("unknown")),
            ("tags", json!([{}])),
            ("note", json!(false)),
            ("file", json!(false)),
        ] {
            let mut changed = original.clone();
            changed["messages"][0][field] = value;
            cases.push((
                format!("messages[0].{field}: invalid value"),
                changed.to_string(),
            ));

            let mut missing = original.clone();
            missing["messages"][0]
                .as_object_mut()
                .unwrap()
                .remove(field);
            cases.push((format!("messages[0].{field}: missing"), missing.to_string()));
        }

        for (field, value) in [
            ("account", json!({"id": 1, "name": "a"})),
            ("mailbox", json!({"id": "1", "name": "m"})),
            (
                "flags",
                json!({"seen": true, "flagged": false, "answered": false, "draft": false, "deleted": 0}),
            ),
            (
                "acquisition",
                json!({"id": 1, "sha256": "A".repeat(64), "bytes": 0}),
            ),
            (
                "tags",
                json!([{"id": "a", "name": "x", "color_srgb": "#ABCDEF"}]),
            ),
            (
                "tags",
                json!([{"id": "a", "name": "x", "color_srgb": null}, {"id": "b", "name": "x", "color_srgb": null}]),
            ),
            (
                "tags",
                json!([{"id": "a", "name": "x", "color_srgb": null}, {"id": "a", "name": "y", "color_srgb": null}]),
            ),
        ] {
            let mut changed = original.clone();
            changed["messages"][0][field] = value;
            cases.push((
                format!("messages[0].{field}: invalid value"),
                changed.to_string(),
            ));
        }

        for (object, field) in [
            ("account", "id"),
            ("account", "name"),
            ("mailbox", "id"),
            ("mailbox", "name"),
            ("flags", "seen"),
            ("flags", "flagged"),
            ("flags", "answered"),
            ("flags", "draft"),
            ("flags", "deleted"),
            ("acquisition", "id"),
            ("acquisition", "sha256"),
            ("acquisition", "bytes"),
        ] {
            let mut missing = original.clone();
            missing["messages"][0][object]
                .as_object_mut()
                .unwrap()
                .remove(field);
            cases.push((
                format!("messages[0].{object}.{field}: missing"),
                missing.to_string(),
            ));

            let mut unknown = original.clone();
            unknown["messages"][0][object]["unexpected"] = json!(true);
            cases.push((
                format!("messages[0].{object}: unknown key near {field}"),
                unknown.to_string(),
            ));
        }

        for availability in ["not_acquired", "missing", "quarantined"] {
            let mut changed = original.clone();
            changed["messages"][0]["body_availability"] = json!(availability);
            cases.push((
                format!("messages[0].body_availability: incompatible {availability}"),
                changed.to_string(),
            ));
        }

        for (field, value) in [
            ("occurrence_id", json!(1.5)),
            ("message_id", json!(1.5)),
            ("keywords", json!(["a", "a"])),
            ("keywords", json!(["a", 1])),
            (
                "tags",
                json!([{"id":"a","name":"z","color_srgb":null},{"id":"b","name":"a","color_srgb":null}]),
            ),
            (
                "tags",
                json!([{"id":"a","name":"a","color_srgb":null},{"id":"b","name":"b","color_srgb":null},{"id":"a","name":"c","color_srgb":null}]),
            ),
            (
                "tags",
                json!([{"id":"a","name":"a","color_srgb":null},{"id":"b","name":"b","color_srgb":null},{"id":"c","name":"a","color_srgb":null}]),
            ),
            (
                "tags",
                json!([{"id":"a","name":"a","color_srgb":"#12345g"}]),
            ),
            ("tags", json!([{"id":"a","name":"a"}])),
            (
                "tags",
                json!([{"id":"a","name":"a","color_srgb":null,"extra":0}]),
            ),
            (
                "acquisition",
                json!({"id":0,"sha256":"a".repeat(64),"bytes":-1}),
            ),
            (
                "acquisition",
                json!({"id":0,"sha256":"a".repeat(64),"bytes":1.5}),
            ),
        ] {
            let mut changed = original.clone();
            changed["messages"][0][field] = value;
            cases.push((
                format!("messages[0].{field}: invalid value"),
                changed.to_string(),
            ));
        }

        for (field, raw) in [
            ("occurrence_id", "-9223372036854775809"),
            ("message_id", "9223372036854775808"),
        ] {
            let old = original["messages"][0][field].to_string();
            let document = original.to_string().replacen(
                &format!("\"{field}\":{old}"),
                &format!("\"{field}\":{raw}"),
                1,
            );
            assert_ne!(document, original.to_string());
            cases.push((format!("messages[0].{field}: overflow"), document));
        }

        for (field, replacement) in [("v", "18446744073709551616"), ("v", "1.5")] {
            let raw = original["messages"][0].to_string();
            let changed = raw.replacen(
                &format!("\"{field}\":{}", original["messages"][0][field]),
                &format!("\"{field}\":{replacement}"),
                1,
            );
            assert_ne!(raw, changed);
            cases.push((
                format!("messages[0].{field}: {replacement}"),
                original.to_string().replacen(&raw, &changed, 1),
            ));
        }
        if matches!(format, ExportFormat::Mboxrd) {
            for field in ["record_offset", "encoded_bytes", "original_bytes"] {
                let raw = original["messages"][0].to_string();
                let changed = raw.replacen(
                    &format!("\"{field}\":{}", original["messages"][0][field]),
                    &format!("\"{field}\":18446744073709551616"),
                    1,
                );
                assert_ne!(raw, changed);
                cases.push((
                    format!("messages[0].{field}: overflow"),
                    original.to_string().replacen(&raw, &changed, 1),
                ));
            }
        }

        for (object, field, raw_value) in [
            ("mailbox", "id", "9223372036854775808"),
            ("mailbox", "id", "1.5"),
            ("acquisition", "id", "-9223372036854775809"),
            ("acquisition", "id", "1.5"),
            ("acquisition", "bytes", "18446744073709551616"),
        ] {
            let nested = &original["messages"][0][object];
            let raw = nested.to_string();
            let changed = raw.replacen(
                &format!("\"{field}\":{}", nested[field]),
                &format!("\"{field}\":{raw_value}"),
                1,
            );
            assert_ne!(raw, changed);
            cases.push((
                format!("messages[0].{object}.{field}: {raw_value}"),
                original.to_string().replacen(&raw, &changed, 1),
            ));
        }

        for (object, key) in [
            ("account", "id"),
            ("account", "name"),
            ("mailbox", "id"),
            ("mailbox", "name"),
            ("flags", "seen"),
            ("flags", "flagged"),
            ("flags", "answered"),
            ("flags", "draft"),
            ("flags", "deleted"),
            ("acquisition", "id"),
            ("acquisition", "sha256"),
            ("acquisition", "bytes"),
        ] {
            let nested = &original["messages"][0][object];
            let raw = nested.to_string();
            let escaped = format!("\\u{:04x}{}", key.as_bytes()[0], &key[1..]);
            let duplicate = raw.replacen('{', &format!("{{\"{escaped}\":{},", nested[key]), 1);
            let document = original.to_string().replacen(&raw, &duplicate, 1);
            assert_ne!(document, original.to_string());
            cases.push((
                format!("messages[0].{object}.{key}: escaped duplicate"),
                document,
            ));
        }

        let raw = original["messages"][0].to_string();
        let duplicate = raw.replacen('{', r#"{"\u0076":1,"#, 1);
        cases.push((
            "messages[0].v: escaped duplicate".into(),
            original.to_string().replacen(&raw, &duplicate, 1),
        ));

        let mut escaped = original.clone();
        escaped["messages"][0]["keywords"] = json!(["a", "b"]);
        cases.push((
            "messages[0].keywords: escaped duplicate".into(),
            escaped
                .to_string()
                .replacen(r#"["a","b"]"#, r#"["a","\u0061"]"#, 1),
        ));

        let mut escaped = original.clone();
        escaped["messages"][0]["tags"] = json!([
            {"id":"a","name":"a","color_srgb":null},
            {"id":"b","name":"b","color_srgb":null},
            {"id":"c","name":"c","color_srgb":null}
        ]);
        cases.push((
            "messages[0].tags: escaped duplicate id".into(),
            escaped
                .to_string()
                .replacen(r#""id":"c""#, r#""id":"\u0061""#, 1),
        ));
        cases.push((
            "messages[0].tags: escaped duplicate name".into(),
            escaped
                .to_string()
                .replacen(r#""name":"c""#, r#""name":"\u0061""#, 1),
        ));

        let id = original["messages"][0]["occurrence_id"].to_string();
        let overflow = original.to_string().replacen(
            &format!(r#""occurrence_id":{id}"#),
            r#""occurrence_id":9223372036854775808"#,
            1,
        );
        assert_ne!(overflow, original.to_string());
        cases.push(("messages[0].occurrence_id: overflow".into(), overflow));

        let mut unknown = original.clone();
        unknown["messages"][0]["unexpected"] = json!(true);
        cases.push(("messages[0]: unknown key".into(), unknown.to_string()));

        let mut invalid_hash = original.clone();
        invalid_hash["messages"][0]["acquisition"]["sha256"] = json!("x".repeat(64));
        cases.push((
            "messages[0].acquisition.sha256: invalid hash".into(),
            invalid_hash.to_string(),
        ));

        let mut invalid_availability = original.clone();
        invalid_availability["messages"][0]["body_availability"] = json!("unknown");
        cases.push((
            "messages[0].body_availability: unknown".into(),
            invalid_availability.to_string(),
        ));

        let mut no_file = original.clone();
        no_file["messages"][0]["file"] = Value::Null;
        cases.push((
            "messages[0].file: missing for acquired".into(),
            no_file.to_string(),
        ));

        if matches!(format, ExportFormat::Mboxrd) {
            let mut missing_span = original.clone();
            missing_span["messages"][0]
                .as_object_mut()
                .unwrap()
                .remove("record_offset");
            cases.push((
                "messages[0].record_offset: missing".into(),
                missing_span.to_string(),
            ));
        }

        for (label, document) in cases {
            std::fs::write(dest.join(MANIFEST), document).unwrap();
            let inventory = res.inventory_import_sources(std::slice::from_ref(&dest), |_| {
                ControlFlow::Continue(())
            });
            let inventory = inventory.inventory();
            assert!(inventory.sources().is_empty(), "{job}: {label}");
            assert_eq!(inventory.problems().len(), 1, "{job}: {label}");
            assert_eq!(
                inventory.problems()[0].kind(),
                sb_resources::ImportProblemKind::Failed,
                "{job}: {label}"
            );
        }
    }
}

/// Destination claiming under the injected filesystem (spec/09): the
/// exclusive lease serializes first claims so exactly one job owns the
/// destination, and a retried claim repeats every durability barrier.
#[cfg(feature = "test-seams")]
mod ownership {
    use std::io;
    use std::sync::atomic::{AtomicBool, AtomicU64, Ordering};
    use std::sync::{Barrier, Condvar, Mutex};
    use std::thread;

    use sb_platform::{Fs, Lease, Location, LockError, Metadata, ReadFile, WriteFile};
    use sb_test_support::{SimFs, real_fs};

    use super::*;

    /// Routes the store to the real fs and the destination subtree
    /// (`parent` and everything under it) to a `SimFs`, so power cuts
    /// and injected faults land on export output only.
    struct SimDestFs {
        real: Arc<dyn Fs>,
        sim: Arc<SimFs>,
        parent: PathBuf,
        /// Fail the next `sync_dir` of `parent` exactly once.
        fail_parent: AtomicBool,
        race: Option<Arc<ClaimRace>>,
    }

    impl SimDestFs {
        fn at(&self, path: &Path) -> &dyn Fs {
            if path.starts_with(&self.parent) {
                self.sim.as_ref()
            } else {
                self.real.as_ref()
            }
        }
    }

    impl Fs for SimDestFs {
        fn create(&self, p: &Path) -> io::Result<Box<dyn WriteFile>> {
            self.at(p).create(p)
        }
        fn open(&self, p: &Path) -> io::Result<Box<dyn ReadFile>> {
            self.at(p).open(p)
        }
        fn open_regular(&self, p: &Path) -> io::Result<(Box<dyn ReadFile>, Metadata)> {
            self.at(p).open_regular(p)
        }
        fn metadata(&self, p: &Path) -> io::Result<Option<Metadata>> {
            self.at(p).metadata(p)
        }
        fn read_dir(&self, p: &Path) -> io::Result<Vec<PathBuf>> {
            let result = self.at(p).read_dir(p);
            if let Some(race) = &self.race {
                race.observe(p == self.parent.join("out"));
            }
            result
        }
        fn create_dir(&self, p: &Path) -> io::Result<()> {
            self.at(p).create_dir(p)
        }
        fn create_private_dir(&self, p: &Path) -> io::Result<()> {
            self.at(p).create_private_dir(p)
        }
        fn restrict_dir(&self, p: &Path) -> io::Result<()> {
            self.at(p).restrict_dir(p)
        }
        fn remove_dir_all(&self, p: &Path) -> io::Result<()> {
            self.at(p).remove_dir_all(p)
        }
        fn quarantine(&self, p: &Path) -> io::Result<()> {
            self.at(p).quarantine(p)
        }
        fn sync_dir(&self, p: &Path) -> io::Result<()> {
            if p == self.parent && self.fail_parent.swap(false, Ordering::SeqCst) {
                return Err(io::Error::from_raw_os_error(5)); // EIO
            }
            self.at(p).sync_dir(p)
        }
        fn rename(&self, from: &Path, to: &Path) -> io::Result<()> {
            self.at(from).rename(from, to)
        }
        fn remove(&self, p: &Path) -> io::Result<()> {
            self.at(p).remove(p)
        }
        fn canonicalize(&self, p: &Path) -> io::Result<PathBuf> {
            self.at(p).canonicalize(p)
        }
        fn lock_exclusive(&self, p: &Path) -> Result<Box<dyn Lease>, LockError> {
            self.at(p).lock_exclusive(p)
        }
        fn location(&self, p: &Path) -> io::Result<Location> {
            self.at(p).location(p)
        }
    }

    /// A two-party barrier at the two claiming jobs' first listing of
    /// the destination: both observe the pre-claim state, then the
    /// second thread waits for the first export to finish before its
    /// stale result returns. That is the check-then-publish race that
    /// decides which single claim may stand.
    struct ClaimRace {
        checked_empty: Barrier,
        first_done: (Mutex<bool>, Condvar),
        listings: AtomicU64,
    }

    impl ClaimRace {
        fn new() -> Self {
            Self {
                checked_empty: Barrier::new(2),
                first_done: (Mutex::new(false), Condvar::new()),
                listings: AtomicU64::new(0),
            }
        }

        fn finish_first(&self) {
            *self.first_done.0.lock().unwrap() = true;
            self.first_done.1.notify_all();
        }

        fn observe(&self, destination_listing: bool) {
            if !destination_listing {
                return;
            }
            // Only each job's probe listing joins the barrier; the
            // under-lease re-listing must not block on a spent one.
            if self.listings.fetch_add(1, Ordering::SeqCst) >= 2 {
                return;
            }
            self.checked_empty.wait();
            if thread::current().name() == Some("second-export") {
                let mut done = self.first_done.0.lock().unwrap();
                while !*done {
                    done = self.first_done.1.wait(done).unwrap();
                }
            }
        }
    }

    /// A retry after a failed destination-parent sync must re-run the
    /// claim's durability barriers: completing without them loses the
    /// whole destination in a power cut.
    #[test]
    fn claim_retry_repeats_the_durability_barriers() {
        let scratch = ScratchDir::new("export-claim-durability");
        let root = scratch.path().join("store");
        let parent = scratch.path().join("sim-export-parent");
        let sim = Arc::new(SimFs::new());
        sim.create_dir_all(&parent).unwrap();
        let fs = Arc::new(SimDestFs {
            real: real_fs(),
            sim: sim.clone(),
            parent: parent.clone(),
            fail_parent: AtomicBool::new(true),
            race: None,
        });
        let res = Resources::open_with_fs(&root, fs).unwrap();
        seed(&res, &root);
        let dest = parent.join("out");
        capture(
            &res,
            "durable",
            ExportFormat::Eml,
            &dest,
            selection_scope(&res, &[1]),
        );
        let first = export_err(&res, "durable");
        assert!(matches!(first, StoreError::Unavailable { .. }), "{first:?}");
        assert_eq!(job_state(&res, "durable"), ExportJobState::Ready);
        export(&res, "durable");
        assert_eq!(job_state(&res, "durable"), ExportJobState::Completed);
        assert!(sim.metadata(&dest.join(MANIFEST)).unwrap().is_some());
        sim.power_cut();
        assert!(
            sim.metadata(&dest.join(MANIFEST)).unwrap().is_some(),
            "a completed job lost its destination: the retried claim never flushed the parent"
        );
    }

    /// Two exports racing one empty destination admit exactly one
    /// claim: the loser is refused and the winner's output stands.
    #[test]
    fn concurrent_first_claims_admit_exactly_one_job() {
        let scratch = ScratchDir::new("export-claim-race");
        let root = scratch.path().join("store");
        let parent = scratch.path().join("sim-export-parent");
        let dest = parent.join("out");
        let sim = Arc::new(SimFs::new());
        sim.create_dir_all(&dest).unwrap();
        let race = Arc::new(ClaimRace::new());
        let fs = Arc::new(SimDestFs {
            real: real_fs(),
            sim: sim.clone(),
            parent,
            fail_parent: AtomicBool::new(false),
            race: Some(race.clone()),
        });
        let res = Arc::new(Resources::open_with_fs(&root, fs).unwrap());
        seed(&res, &root);
        capture(
            &res,
            "first-job",
            ExportFormat::Eml,
            &dest,
            selection_scope(&res, &[1]),
        );
        // The second capture freezes different acquisition bytes for
        // the same occurrence, so a stolen destination is observable.
        write(&res, |tx| {
            tx.execute(
                "UPDATE logical_messages SET acquisition_id=2 WHERE message_id=1",
                [],
            )
            .unwrap();
            Ok(())
        });
        capture(
            &res,
            "second-job",
            ExportFormat::Eml,
            &dest,
            selection_scope(&res, &[1]),
        );
        let first_res = res.clone();
        let first_race = race.clone();
        let first = thread::Builder::new()
            .name("first-export".into())
            .spawn(move || {
                let result = first_res.export_files("first-job", |_| ControlFlow::Continue(()));
                first_race.finish_first();
                result
            })
            .unwrap();
        let second_res = res.clone();
        let second = thread::Builder::new()
            .name("second-export".into())
            .spawn(move || second_res.export_files("second-job", |_| ControlFlow::Continue(())))
            .unwrap();
        let first = first.join().unwrap();
        let second = second.join().unwrap();
        assert!(matches!(first, Ok(Scoped::Done(_))), "first: {first:?}");
        assert!(second.is_err(), "both claims stood: {second:?}");
        let mut output = Vec::new();
        io::Read::read_to_end(&mut *sim.open(&dest.join("1.eml")).unwrap(), &mut output).unwrap();
        assert_eq!(
            output, BODY_A,
            "the loser's claim replaced the winner's output"
        );
    }
}

#[cfg(feature = "test-seams")]
mod crash_cuts {
    use std::env;

    use super::*;
    use sb_resources::Fs;
    use sb_test_support::{CutFs, real_fs, run_crash_cut_points};

    const CUT_N_ENV: &str = "SB_EXPORT_FILES_CUT_N";
    const CUT_ROOT_ENV: &str = "SB_EXPORT_FILES_CUT_ROOT";
    const CUT_POINTS: usize = 50;

    fn drive(root: &Path, fs: Arc<dyn Fs>) -> ExportJob {
        let res = Resources::open_with_fs(root, fs).unwrap();
        seed(&res, root);
        capture(
            &res,
            "cut",
            ExportFormat::Mboxrd,
            &root.join("out"),
            selection_scope(&res, &[4, 1, 3, 6, 2, 5, 8, 7]),
        );
        match res.export_files("cut", |_| ControlFlow::Continue(())) {
            Ok(Scoped::Done(job)) => job,
            other => panic!("the cut run failed: {other:?}"),
        }
    }

    /// Not a test: the harness re-runs this as a child killed at the Nth fs op.
    #[test]
    #[ignore = "spawned by export_crash_cuts_resume_to_identical_output"]
    fn export_cut_child() {
        let n: u64 = env::var(CUT_N_ENV).unwrap().parse().unwrap();
        let root = PathBuf::from(env::var(CUT_ROOT_ENV).unwrap());
        drive(&root, Arc::new(CutFs::new(real_fs(), n)));
    }

    fn normalized(dest: &Path) -> (Vec<(String, Vec<u8>)>, Value) {
        let mut files: Vec<(String, Vec<u8>)> = std::fs::read_dir(dest)
            .unwrap()
            .map(|entry| {
                let path = entry.unwrap().path();
                let name = path.file_name().unwrap().to_string_lossy().into_owned();
                (name, std::fs::read(&path).unwrap())
            })
            .filter(|(name, _)| name != MANIFEST)
            .collect();
        files.sort();
        let mut manifest = manifest(dest);
        manifest["destination"] = Value::Null;
        for message in manifest["messages"].as_array_mut().unwrap() {
            message["account"]["id"] = Value::Null;
        }
        (files, manifest)
    }

    /// Every sampled cut lands; each killed run resumes to identical output.
    #[test]
    fn export_crash_cuts_resume_to_identical_output() {
        let scratch = ScratchDir::new("export-files-crash-cuts");
        let reference_root = scratch.path().join("reference");
        std::fs::create_dir_all(&reference_root).unwrap();
        let uncut = Arc::new(CutFs::new(real_fs(), u64::MAX));
        drive(&reference_root, uncut.clone());
        let expected = normalized(&reference_root.join("out"));
        let last = uncut.operations();
        // Evenly spaced, deterministic cut points across the whole run.
        let step = (last / CUT_POINTS as u64).max(1) as usize;
        let points: Vec<u64> = (1..=last).step_by(step).take(CUT_POINTS).collect();

        let cuts = run_crash_cut_points(
            CUT_N_ENV,
            CUT_ROOT_ENV,
            "crash_cuts::export_cut_child",
            &points,
            &mut |n| {
                let root = scratch.path().join(format!("cut-{n}"));
                std::fs::create_dir_all(&root).unwrap();
                root
            },
            &mut |root| {
                // A killed run never completed without its manifest.
                let job = drive(root, real_fs());
                assert_eq!(job.state(), ExportJobState::Completed);
                for entry in std::fs::read_dir(root.join("out")).unwrap().flatten() {
                    assert!(
                        !entry.file_name().to_string_lossy().ends_with(".partial"),
                        "a completed export left a partial: {entry:?}"
                    );
                }
                assert_eq!(normalized(&root.join("out")), expected);
            },
        );
        assert!(
            cuts.survived.is_empty(),
            "children outlived cut points {:?}",
            cuts.survived
        );
        assert_eq!(cuts.killed, points);
    }
}

#[test]
fn bundle_inventory_reports_child_disagreements() {
    let (scratch, res, _) = fixture();
    for (job, format) in [
        ("children-eml", ExportFormat::Eml),
        ("children-mbox", ExportFormat::Mboxrd),
    ] {
        let dest = scratch.path().join(job);
        capture(&res, job, format, &dest, account_scope(&res));
        export(&res, job);
        let metadata = manifest(&dest);
        let files = metadata["files"].as_array().unwrap();
        let missing = dest.join(files[0]["name"].as_str().unwrap());
        std::fs::remove_file(&missing).unwrap();
        let extra = dest.join("unlisted.eml");
        std::fs::write(&extra, BODY_A).unwrap();
        let mut captured = Vec::new();
        let outcome = res.inventory_import_sources(std::slice::from_ref(&dest), |event| {
            if let sb_resources::ImportProgress::Capturing { path, .. } = event {
                captured.push(path.to_path_buf());
            }
            ControlFlow::Continue(())
        });
        let inventory = outcome.inventory();
        assert!(inventory.sources().is_empty());
        assert!(inventory.problems().iter().any(|p| p.path() == missing));
        assert!(inventory.problems().iter().any(|p| p.path() == extra));
        if files.len() > 1 {
            let sibling = dest.join(files[1]["name"].as_str().unwrap());
            assert!(captured.contains(&sibling), "{job}: sibling not checked");
        }
    }
}

#[test]
fn bundle_inventory_checks_child_kind_length_and_count() {
    let (scratch, res, _) = fixture();
    for (job, format) in [
        ("agreement-eml", ExportFormat::Eml),
        ("agreement-mbox", ExportFormat::Mboxrd),
    ] {
        let dest = scratch.path().join(job);
        capture(&res, job, format, &dest, mailbox_scope(&res, 1));
        export(&res, job);
        let original = manifest(&dest);
        let name = original["files"][0]["name"].as_str().unwrap();
        let path = dest.join(name);
        let bytes = std::fs::read(&path).unwrap();
        let check = |property: &str| {
            let outcome = res.inventory_import_sources(std::slice::from_ref(&dest), |_| {
                ControlFlow::Continue(())
            });
            assert!(outcome.inventory().sources().is_empty());
            assert!(
                outcome.inventory().problems().iter().any(|problem| {
                    problem.path() == path && problem.detail().contains(property)
                }),
                "{job}: {property}: {:?}",
                outcome.inventory().problems()
            );
        };

        let mut longer = bytes.clone();
        longer.push(b'x');
        std::fs::write(&path, longer).unwrap();
        check("length");
        if matches!(format, ExportFormat::Mboxrd) {
            let mut same_length = bytes.clone();
            let separator = same_length
                .windows(6)
                .position(|window| window == b"\nFrom ")
                .unwrap();
            same_length[separator + 1] = b'X';
            std::fs::write(&path, same_length).unwrap();
            check("record count");
        }
        std::fs::remove_file(&path).unwrap();
        std::fs::create_dir(&path).unwrap();
        check("regular");
        std::fs::remove_dir(&path).unwrap();
        std::fs::write(&path, &bytes).unwrap();

        let wrong_name = "wrong.txt";
        let mut wrong = original.clone();
        wrong["files"][0]["name"] = json!(wrong_name);
        for message in wrong["messages"].as_array_mut().unwrap() {
            if message["file"] == name {
                message["file"] = json!(wrong_name);
            }
        }
        let wrong_path = dest.join(wrong_name);
        std::fs::rename(&path, &wrong_path).unwrap();
        std::fs::write(dest.join(MANIFEST), wrong.to_string()).unwrap();
        let outcome = res
            .inventory_import_sources(std::slice::from_ref(&dest), |_| ControlFlow::Continue(()));
        assert!(outcome.inventory().sources().is_empty());
        assert!(
            outcome
                .inventory()
                .problems()
                .iter()
                .any(|p| p.path() == wrong_path && p.detail().contains("format"))
        );
    }
}

#[test]
fn bundle_and_direct_mbox_roots_do_not_suppress_each_other() {
    let (scratch, res, _) = fixture();
    let dest = scratch.path().join("bundle");
    capture(
        &res,
        "alias-roots",
        ExportFormat::Mboxrd,
        &dest,
        mailbox_scope(&res, 1),
    );
    export(&res, "alias-roots");
    let child = dest.join(manifest(&dest)["files"][0]["name"].as_str().unwrap());
    let alias = scratch.path().join("alias.mbox");
    std::os::unix::fs::symlink(&child, &alias).unwrap();
    for roots in [
        vec![dest.clone(), alias.clone()],
        vec![alias.clone(), dest.clone()],
    ] {
        let outcome = res.inventory_import_sources(&roots, |_| ControlFlow::Continue(()));
        assert_eq!(outcome.inventory().sources().len(), 1);
        assert!(outcome.inventory().sources()[0].bundle().is_none());
        assert_eq!(outcome.inventory().problems().len(), 1);
        assert_eq!(
            outcome.inventory().problems()[0].kind(),
            sb_resources::ImportProblemKind::Unsupported
        );
    }
}

#[test]
fn bundle_checks_each_declared_alias() {
    let (scratch, res, _) = fixture();
    let dest = scratch.path().join("aliases");
    capture(
        &res,
        "declared-aliases",
        ExportFormat::Eml,
        &dest,
        mailbox_scope(&res, 1),
    );
    export(&res, "declared-aliases");
    let mut metadata = manifest(&dest);
    let first = metadata["files"][0]["name"].as_str().unwrap().to_owned();
    let second = metadata["files"][1]["name"].as_str().unwrap().to_owned();
    let first_path = dest.join(&first);
    let second_path = dest.join(&second);
    let bytes = std::fs::metadata(&first_path).unwrap().len();
    std::fs::remove_file(&second_path).unwrap();
    std::os::unix::fs::symlink(&first_path, &second_path).unwrap();
    metadata["files"][1]["bytes"] = json!(bytes);
    for message in metadata["messages"].as_array_mut().unwrap() {
        if message["file"] == second {
            message["acquisition"]["bytes"] = json!(bytes);
        }
    }
    std::fs::write(dest.join(MANIFEST), metadata.to_string()).unwrap();
    let mut captured = BTreeSet::new();
    let outcome = res.inventory_import_sources(std::slice::from_ref(&dest), |event| {
        if let sb_resources::ImportProgress::Capturing { path, .. } = event {
            captured.insert(path.to_path_buf());
        }
        ControlFlow::Continue(())
    });
    assert!(captured.contains(&first_path));
    assert!(captured.contains(&second_path));
    assert!(outcome.inventory().sources().is_empty());
    assert_eq!(outcome.inventory().problems().len(), 1);
    assert_eq!(
        outcome.inventory().problems()[0].kind(),
        sb_resources::ImportProblemKind::Unsupported
    );
}

#[test]
fn bundle_children_follow_sorted_names_and_ignore_nested_files() {
    let (scratch, res, _) = fixture();
    let dest = scratch.path().join("sorted");
    capture(
        &res,
        "sorted-children",
        ExportFormat::Eml,
        &dest,
        account_scope(&res),
    );
    export(&res, "sorted-children");
    let mut metadata = manifest(&dest);
    let lower = metadata["files"][0]["name"].as_str().unwrap().to_owned();
    let upper = format!("{}.EML", lower.strip_suffix(".eml").unwrap());
    std::fs::rename(dest.join(&lower), dest.join(&upper)).unwrap();
    metadata["files"][0]["name"] = json!(upper);
    for message in metadata["messages"].as_array_mut().unwrap() {
        if message["file"] == lower {
            message["file"] = json!(upper);
        }
    }
    let files = metadata["files"].as_array_mut().unwrap();
    files.reverse();
    let name = files[0]["name"].as_str().unwrap().to_owned();
    let first = name.chars().next().unwrap();
    let escaped = format!("\\u{:04x}", first as u32);
    let document = metadata
        .to_string()
        .replacen(&name, &name.replacen(first, &escaped, 1), 1);
    assert!(document.contains(&escaped));
    std::fs::write(dest.join(MANIFEST), document).unwrap();
    std::fs::write(dest.join("sidecar.txt"), b"sidecar").unwrap();
    let nested = dest.join("nested");
    std::fs::create_dir(&nested).unwrap();
    std::fs::write(nested.join("nested.eml"), BODY_A).unwrap();
    let mut captured = Vec::new();
    let outcome = res.inventory_import_sources(std::slice::from_ref(&dest), |event| {
        if let sb_resources::ImportProgress::Capturing { path, .. } = event
            && path != dest.join(MANIFEST)
            && captured.last().is_none_or(|last| last != path)
        {
            captured.push(path.to_path_buf());
        }
        ControlFlow::Continue(())
    });
    let mut sorted = captured.clone();
    sorted.sort();
    assert!(captured.len() > 1);
    assert_eq!(captured, sorted);
    assert!(outcome.inventory().sources().is_empty());
    assert_eq!(outcome.inventory().problems().len(), 1);
    assert_eq!(
        outcome.inventory().problems()[0].kind(),
        sb_resources::ImportProblemKind::Unsupported
    );
}

#[test]
fn bundle_missing_name_diagnostic_is_bounded() {
    let (scratch, res, _) = fixture();
    let dest = scratch.path().join("long-name");
    capture(
        &res,
        "long-name",
        ExportFormat::Eml,
        &dest,
        mailbox_scope(&res, 1),
    );
    export(&res, "long-name");
    let mut metadata = manifest(&dest);
    let old = metadata["files"][0]["name"].as_str().unwrap().to_owned();
    let long = format!("{}😀.eml", "x".repeat(100_000));
    metadata["files"][0]["name"] = json!(long);
    for message in metadata["messages"].as_array_mut().unwrap() {
        if message["file"] == old {
            message["file"] = json!(long);
        }
    }
    std::fs::write(dest.join(MANIFEST), metadata.to_string()).unwrap();
    let outcome =
        res.inventory_import_sources(std::slice::from_ref(&dest), |_| ControlFlow::Continue(()));
    let problem = outcome
        .inventory()
        .problems()
        .iter()
        .find(|p| p.path() == dest)
        .unwrap();
    assert!(problem.detail().contains("truncated"));
    assert!(problem.detail().contains(&long.len().to_string()));
    assert!(problem.detail().contains(&sha_hex(long.as_bytes())));
    assert!(problem.detail().len() < 600);
    assert!(outcome.inventory().sources().is_empty());
}

#[test]
fn bundle_child_cancellation_discards_only_its_snapshots() {
    let (scratch, res, root) = fixture();
    let other = scratch.path().join("other.eml");
    std::fs::write(&other, BODY_A).unwrap();
    let other_inventory =
        res.inventory_import_sources(std::slice::from_ref(&other), |_| ControlFlow::Continue(()));
    assert_eq!(other_inventory.inventory().sources().len(), 1);
    let snapshots = || {
        std::fs::read_dir(root.join("recovery"))
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .filter(|name| name.to_string_lossy().starts_with("import-source-"))
            .collect::<BTreeSet<_>>()
    };
    let before = snapshots();
    let dest = scratch.path().join("cancellation");
    capture(
        &res,
        "cancel-children",
        ExportFormat::Mboxrd,
        &dest,
        mailbox_scope(&res, 1),
    );
    export(&res, "cancel-children");
    let mut metadata = manifest(&dest);
    let child = dest.join(metadata["files"][0]["name"].as_str().unwrap());
    let mut content = std::fs::read(&child).unwrap();
    content.extend(std::iter::repeat_n(b'x', 128 * 1024));
    std::fs::write(&child, &content).unwrap();
    metadata["files"][0]["bytes"] = json!(content.len());
    std::fs::write(dest.join(MANIFEST), metadata.to_string()).unwrap();
    for cancel_scan in [false, true] {
        let mut seen_scan = 0;
        let outcome =
            res.inventory_import_sources(std::slice::from_ref(&dest), |event| match event {
                sb_resources::ImportProgress::Capturing { path, .. }
                    if path == child && !cancel_scan =>
                {
                    ControlFlow::Break(())
                }
                sb_resources::ImportProgress::Scanning { path, .. }
                    if path == child && cancel_scan =>
                {
                    seen_scan += 1;
                    if seen_scan == 4 {
                        ControlFlow::Break(())
                    } else {
                        ControlFlow::Continue(())
                    }
                }
                _ => ControlFlow::Continue(()),
            });
        assert!(matches!(
            outcome,
            sb_resources::ImportInventoryOutcome::Cancelled(_)
        ));
        assert_eq!(snapshots(), before);
    }
    let outcome =
        res.inventory_import_sources(std::slice::from_ref(&dest), |_| ControlFlow::Continue(()));
    assert_eq!(
        outcome.inventory().problems()[0].kind(),
        sb_resources::ImportProblemKind::Unsupported
    );
    assert_eq!(snapshots(), before);
    std::fs::write(&child, b"bad").unwrap();
    let outcome =
        res.inventory_import_sources(std::slice::from_ref(&dest), |_| ControlFlow::Continue(()));
    assert_eq!(
        outcome.inventory().problems()[0].kind(),
        sb_resources::ImportProblemKind::Failed
    );
    assert_eq!(snapshots(), before);
}

#[test]
fn emitted_bundles_are_captured_and_refused_without_plain_import() {
    let (scratch, res, _) = fixture();
    for (job, format, scope) in [
        ("eml-import", ExportFormat::Eml, mailbox_scope(&res, 1)),
        ("mbox-import", ExportFormat::Mboxrd, account_scope(&res)),
        (
            "mbox-mailbox-import",
            ExportFormat::Mboxrd,
            mailbox_scope(&res, 1),
        ),
        ("eml-account-import", ExportFormat::Eml, account_scope(&res)),
        (
            "selection-import",
            ExportFormat::Eml,
            selection_scope(&res, &[1, 2]),
        ),
        (
            "mbox-selection-import",
            ExportFormat::Mboxrd,
            selection_scope(&res, &[1, 2]),
        ),
        (
            "search-import",
            ExportFormat::Eml,
            ExportScope::Search {
                query: Query::parse_json(r#"{"v":1,"q":{"flag":"flagged"}}"#).unwrap(),
                scope: SearchScope::Mailbox(1),
                sort: ExportSearchSort::Date,
                expected_generation: res.read(|tx| search_generation(tx)).unwrap() as u64,
            },
        ),
        (
            "mbox-search-import",
            ExportFormat::Mboxrd,
            ExportScope::Search {
                query: Query::parse_json(r#"{"v":1,"q":{"flag":"flagged"}}"#).unwrap(),
                scope: SearchScope::Mailbox(1),
                sort: ExportSearchSort::Date,
                expected_generation: res.read(|tx| search_generation(tx)).unwrap() as u64,
            },
        ),
    ] {
        let dest = scratch.path().join(format!("{job}.mbox"));
        capture(&res, job, format, &dest, scope);
        export(&res, job);
        let mut captured = false;
        let outcome = res.inventory_import_sources(std::slice::from_ref(&dest), |event| {
            if matches!(event, sb_resources::ImportProgress::Capturing { .. }) {
                captured = true;
            }
            ControlFlow::Continue(())
        });
        let inventory = outcome.inventory();
        assert!(captured);
        assert!(inventory.sources().is_empty());
        assert_eq!(inventory.problems().len(), 1);
        assert_eq!(
            inventory.problems()[0].kind(),
            sb_resources::ImportProblemKind::Unsupported
        );
        assert!(
            inventory.problems()[0]
                .detail()
                .contains("not yet supported")
        );
    }
}

#[test]
fn manifest_envelope_rejects_missing_scope() {
    let (scratch, res, _) = fixture();
    for (job, format) in [
        ("schema-eml", ExportFormat::Eml),
        ("schema-mbox", ExportFormat::Mboxrd),
    ] {
        let dest = scratch.path().join(job);
        capture(&res, job, format, &dest, mailbox_scope(&res, 1));
        export(&res, job);
        let original = manifest(&dest);
        let mut cases = Vec::new();

        let mut missing = original.clone();
        missing.as_object_mut().unwrap().remove("scope");
        cases.push(("missing scope", missing.to_string()));

        let duplicate_key = original.to_string().replace(
            r#""kind":"export-manifest""#,
            r#""kind":"export-manifest","\u006bind":"export-manifest""#,
        );
        assert_ne!(duplicate_key, original.to_string());
        cases.push(("escaped duplicate key", duplicate_key));

        for (label, document) in cases {
            std::fs::write(dest.join(MANIFEST), document).unwrap();
            let inventory = res.inventory_import_sources(std::slice::from_ref(&dest), |_| {
                ControlFlow::Continue(())
            });
            let inventory = inventory.inventory();
            assert!(inventory.sources().is_empty(), "{job}: {label}");
            assert_eq!(inventory.problems().len(), 1, "{job}: {label}");
            assert_eq!(
                inventory.problems()[0].kind(),
                sb_resources::ImportProblemKind::Failed,
                "{job}: {label}"
            );
        }
    }
}

#[test]
fn manifest_file_table_rejects_invalid_writer_entries() {
    let (scratch, res, _) = fixture();
    for (job, format) in [
        ("file-schema-eml", ExportFormat::Eml),
        ("file-schema-mbox", ExportFormat::Mboxrd),
    ] {
        let dest = scratch.path().join(job);
        capture(&res, job, format, &dest, mailbox_scope(&res, 1));
        export(&res, job);
        let original = manifest(&dest);
        for files in [
            r#"{}"#,
            r#"[null]"#,
            r#"[{"bytes":0,"records":1}]"#,
            r#"[{"name":"a","records":1}]"#,
            r#"[{"name":"a","bytes":0}]"#,
            r#"[{"name":"a","bytes":0,"records":1,"extra":0}]"#,
            r#"[{"name":"a","bytes":0,"\u0062ytes":0,"records":1}]"#,
            r#"[{"name":1,"bytes":0,"records":1}]"#,
            r#"[{"name":"a","bytes":"0","records":1}]"#,
            r#"[{"name":"a","bytes":-1,"records":1}]"#,
            r#"[{"name":"a","bytes":18446744073709551616,"records":1}]"#,
            r#"[{"name":"a","bytes":0,"records":-1}]"#,
            r#"[{"name":"a","bytes":0,"records":18446744073709551616}]"#,
            r#"[{"name":"","bytes":0,"records":1}]"#,
            r#"[{"name":".","bytes":0,"records":1}]"#,
            r#"[{"name":"\u002e.","bytes":0,"records":1}]"#,
            r#"[{"name":"/a","bytes":0,"records":1}]"#,
            r#"[{"name":"a\/b","bytes":0,"records":1}]"#,
            r#"[{"name":"a\\b","bytes":0,"records":1}]"#,
            r#"[{"name":"a\u0000b","bytes":0,"records":1}]"#,
            r#"[{"name":"a","bytes":0,"records":1},{"name":"\u0061","bytes":0,"records":1}]"#,
            r#"[{"name":"😀","bytes":0,"records":1},{"name":"\ud83d\ude00","bytes":0,"records":1}]"#,
        ] {
            let document = original.to_string().replace(
                &format!("\"files\":{}", original["files"]),
                &format!("\"files\":{files}"),
            );
            assert_ne!(document, original.to_string());
            std::fs::write(dest.join(MANIFEST), document).unwrap();
            let outcome = res.inventory_import_sources(std::slice::from_ref(&dest), |_| {
                ControlFlow::Continue(())
            });
            let inventory = outcome.inventory();
            assert!(inventory.sources().is_empty(), "{job}: {files}");
            assert_eq!(inventory.problems().len(), 1, "{job}: {files}");
            assert_eq!(
                inventory.problems()[0].kind(),
                sb_resources::ImportProblemKind::Failed,
                "{job}: {files}"
            );
        }
    }
}
