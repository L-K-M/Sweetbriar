//! Tests for deterministic import discovery and the durable
//! per-source inventory (M1.2c2a): synthetic trees on the real
//! filesystem through `ScratchDir`, plus the `fixtures/import`
//! corpus. No owner data.

use std::fs;
use std::io::{self, Read};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};

use sb_test_support::ScratchDir;

use super::*;
use crate::Resources;

const MBOX_TWO: &[u8] = b"From a@example.test Thu Jan  1 00:00:00 1970\n\nfirst\nFrom b@example.test Thu Jan  1 00:00:00 1970\n\nsecond\n";
const MBOX_ONE: &[u8] = b"From a@example.test Thu Jan  1 00:00:00 1970\n\nonly\n";
const EML: &[u8] = b"From: a@example.test\nSubject: s\n\nbody\n";

fn resources(label: &str) -> (ScratchDir, Resources) {
    let scratch = ScratchDir::new(label);
    let resources = Resources::open(&scratch.path().join("store")).unwrap();
    (scratch, resources)
}

/// A completed inventory; a cancelled run fails the test here.
fn run(res: &Resources, roots: &[PathBuf]) -> ImportInventory {
    match res.inventory_import_sources(roots, |_| ControlFlow::Continue(())) {
        ImportInventoryOutcome::Complete(inventory) => inventory,
        ImportInventoryOutcome::Cancelled(_) => panic!("the inventory was cancelled"),
    }
}

/// The private copies `recovery/` currently holds.
fn snapshot_names(scratch: &ScratchDir) -> Vec<String> {
    fs::read_dir(scratch.path().join("store/recovery"))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().to_string_lossy().into_owned())
        .filter(|name| name.starts_with("import-source-"))
        .collect()
}

#[test]
fn mbox_package_contributes_only_its_mbox_child() {
    let (scratch, res) = resources("pkg");
    let package = scratch.path().join("Export.mbox");
    fs::create_dir(&package).unwrap();
    fs::write(package.join("mbox"), MBOX_TWO).unwrap();
    fs::write(package.join("Info.plist"), b"plist").unwrap();
    // Even a mail-looking sidecar is ignored.
    fs::write(package.join("note.eml"), EML).unwrap();

    let inventory = run(&res, std::slice::from_ref(&package));
    assert!(inventory.problems().is_empty());
    let [source] = inventory.sources() else {
        panic!("one source expected: {:?}", inventory.sources())
    };
    assert_eq!(source.kind(), SourceKind::Mbox);
    assert_eq!(source.mailbox(), "Export");
    assert_eq!(source.path(), package.join("mbox").canonicalize().unwrap());
    assert_eq!(source.message_count(), 2);
    assert_eq!(source.quoting(), MboxQuoting::Equivalent);
}

#[test]
fn walk_visits_sorted_entries_and_names_mailboxes() {
    let (scratch, res) = resources("walk");
    let dir = scratch.path().join("mail");
    fs::create_dir(&dir).unwrap();
    fs::create_dir(dir.join("sub")).unwrap();
    // A nested package contributes its child under its own stem.
    fs::create_dir(dir.join("Inner.mbox")).unwrap();
    fs::write(dir.join("a.eml"), EML).unwrap();
    fs::write(dir.join("b.mbox"), MBOX_ONE).unwrap();
    fs::write(dir.join("notes.txt"), b"not mail").unwrap();
    fs::write(dir.join("Inner.mbox/mbox"), MBOX_ONE).unwrap();
    fs::write(dir.join("Inner.mbox/Info.plist"), b"plist").unwrap();
    fs::write(dir.join("sub/c.eml"), EML).unwrap();
    fs::write(dir.join("sub/z.eml"), EML).unwrap();

    let inventory = run(&res, &[dir]);
    assert!(inventory.problems().is_empty());
    let found: Vec<(&str, SourceKind, u64)> = inventory
        .sources()
        .iter()
        .map(|s| (s.mailbox(), s.kind(), s.message_count()))
        .collect();
    // Sorted entries (uppercase first): Inner.mbox, a.eml, b.mbox,
    // notes.txt, sub; the subdirectory walks at its own position.
    assert_eq!(
        found,
        [
            ("Inner", SourceKind::Mbox, 1),
            ("mail", SourceKind::Eml, 1),
            ("b", SourceKind::Mbox, 1),
            ("sub", SourceKind::Eml, 1),
            ("sub", SourceKind::Eml, 1),
        ]
    );
}

#[test]
fn roots_run_in_request_order_and_extensions_are_case_insensitive() {
    let (scratch, res) = resources("order");
    fs::write(scratch.path().join("B.MBox"), MBOX_ONE).unwrap();
    fs::write(scratch.path().join("a.eml"), EML).unwrap();
    fs::write(scratch.path().join("C.EML"), EML).unwrap();
    fs::create_dir(scratch.path().join("D.MBOX")).unwrap();
    fs::write(scratch.path().join("D.MBOX/mbox"), MBOX_ONE).unwrap();

    let inventory = run(
        &res,
        &[
            scratch.path().join("B.MBox"),
            scratch.path().join("a.eml"),
            scratch.path().join("C.EML"),
            scratch.path().join("D.MBOX"),
        ],
    );
    let order: Vec<(&str, SourceKind)> = inventory
        .sources()
        .iter()
        .map(|s| (s.mailbox(), s.kind()))
        .collect();
    assert_eq!(
        order,
        [
            ("B", SourceKind::Mbox),
            ("a", SourceKind::Eml),
            ("C", SourceKind::Eml),
            ("D", SourceKind::Mbox),
        ]
    );
}

#[test]
fn explicit_regular_files_are_mbox_candidates_and_bad_bytes_report() {
    let (scratch, res) = resources("explicit");
    fs::write(scratch.path().join("blob.data"), MBOX_ONE).unwrap();
    fs::write(scratch.path().join("junk.txt"), b"not an mbox at all\n").unwrap();
    fs::write(scratch.path().join("message.eml"), EML).unwrap();

    let inventory = run(
        &res,
        &[
            scratch.path().join("blob.data"),
            scratch.path().join("junk.txt"),
            scratch.path().join("message.eml"),
        ],
    );
    let [blob, message] = inventory.sources() else {
        panic!("two sources expected: {:?}", inventory.sources())
    };
    assert_eq!((blob.mailbox(), blob.kind()), ("blob", SourceKind::Mbox));
    assert_eq!(blob.message_count(), 1);
    assert_eq!(
        (message.mailbox(), message.kind()),
        ("message", SourceKind::Eml)
    );
    let [problem] = inventory.problems() else {
        panic!("one problem expected: {:?}", inventory.problems())
    };
    assert_eq!(problem.kind(), ImportProblemKind::Failed);
    assert!(problem.detail().contains("byte 0"), "{}", problem.detail());
}

#[cfg(unix)]
#[test]
fn aliases_cycles_and_overlapping_roots_visit_once() {
    use std::os::unix::fs::symlink;

    let (scratch, res) = resources("aliases");
    let real = scratch.path().join("real");
    fs::create_dir(&real).unwrap();
    fs::write(real.join("f.eml"), EML).unwrap();
    fs::write(real.join("m.mbox"), MBOX_ONE).unwrap();
    symlink(&real, real.join("cycle")).unwrap();
    symlink(&real, scratch.path().join("alias")).unwrap();

    let inventory = run(
        &res,
        &[
            scratch.path().join("alias"),     // another spelling of real/
            real.join("cycle"),               // a symlink back into real/
            real.clone(),                     // an overlapping root
            real.join("cycle").join("f.eml"), // the same file by alias
            real.join("f.eml"),
        ],
    );
    assert!(inventory.problems().is_empty());
    let found: Vec<(&str, SourceKind)> = inventory
        .sources()
        .iter()
        .map(|s| (s.mailbox(), s.kind()))
        .collect();
    // The walk sees f.eml under the folder's name first; explicit
    // re-selection is the same canonical file and dedupes.
    assert_eq!(found, [("real", SourceKind::Eml), ("m", SourceKind::Mbox)]);
}

#[cfg(unix)]
#[test]
fn a_package_alias_without_an_extension_still_finds_its_source() {
    let (scratch, res) = resources("package-alias");
    let package = scratch.path().join("Original.mbox");
    fs::create_dir(&package).unwrap();
    fs::write(package.join("mbox"), MBOX_ONE).unwrap();
    let alias = scratch.path().join("Chosen");
    std::os::unix::fs::symlink(&package, &alias).unwrap();
    let inventory = run(&res, &[alias]);
    assert!(inventory.problems().is_empty());
    assert_eq!(inventory.sources().len(), 1);
    assert_eq!(inventory.sources()[0].message_count(), 1);
}

#[cfg(unix)]
#[test]
fn missing_nonregular_and_non_utf8_paths_are_reported() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let (scratch, res) = resources("problems");
    let dir = scratch.path().join("d");
    fs::create_dir(&dir).unwrap();
    fs::write(dir.join("ok.eml"), EML).unwrap();
    // APFS rejects these names; Linux also covers an actual listed entry.
    #[cfg(target_os = "linux")]
    fs::write(dir.join(OsStr::from_bytes(b"bad\xff.eml")), EML).unwrap();
    let fifo = scratch.path().join("pipe");
    assert!(
        std::process::Command::new("mkfifo")
            .arg(&fifo)
            .status()
            .unwrap()
            .success()
    );
    let missing = scratch.path().join("missing.mbox");
    let nasty = scratch.path().join(OsStr::from_bytes(b"nasty\xffdir"));

    let inventory = run(&res, &[missing, fifo, nasty, dir]);
    let kinds: Vec<ImportProblemKind> = inventory.problems().iter().map(|p| p.kind()).collect();
    // Invalid roots need no filesystem entry; good siblings still inventory.
    assert_eq!(
        kinds,
        [
            ImportProblemKind::Inaccessible,
            ImportProblemKind::Unsupported,
            ImportProblemKind::NonUtf8,
            #[cfg(target_os = "linux")]
            ImportProblemKind::NonUtf8,
        ]
    );
    let [source] = inventory.sources() else {
        panic!("one source expected: {:?}", inventory.sources())
    };
    assert_eq!(source.mailbox(), "d");
}

#[cfg(unix)]
#[test]
fn non_utf8_roots_are_rejected_before_filesystem_lookup() {
    use std::ffi::OsStr;
    use std::os::unix::ffi::OsStrExt;

    let (scratch, res) = resources("invalid-path-input");
    let path = scratch.path().join(OsStr::from_bytes(b"absent\xff.eml"));
    let inventory = run(&res, &[path]);
    assert!(inventory.sources().is_empty());
    assert_eq!(inventory.problems().len(), 1);
    assert_eq!(inventory.problems()[0].kind(), ImportProblemKind::NonUtf8);
}

#[test]
fn a_package_without_an_mbox_child_reports_a_problem() {
    let (scratch, res) = resources("empty-pkg");
    let package = scratch.path().join("Empty.mbox");
    fs::create_dir(&package).unwrap();
    fs::write(package.join("Info.plist"), b"plist").unwrap();

    let inventory = run(&res, &[package]);
    assert!(inventory.sources().is_empty());
    let [problem] = inventory.problems() else {
        panic!("one problem expected: {:?}", inventory.problems())
    };
    assert_eq!(problem.kind(), ImportProblemKind::Inaccessible);
}

#[cfg(unix)]
#[test]
fn a_socket_is_an_unsupported_source_not_a_failed_open() {
    let (scratch, res) = resources("socket");
    let socket = scratch.path().join("socket");
    let _listener = std::os::unix::net::UnixListener::bind(&socket).unwrap();
    let eml = scratch.path().join("good.eml");
    fs::write(&eml, EML).unwrap();
    let inventory = run(&res, &[socket, eml]);
    assert_eq!(inventory.sources().len(), 1);
    assert_eq!(inventory.problems().len(), 1);
    assert_eq!(
        inventory.problems()[0].kind(),
        ImportProblemKind::Unsupported
    );
}

#[test]
fn empty_sources_are_valid() {
    let (scratch, res) = resources("empty");
    fs::write(scratch.path().join("empty.eml"), b"").unwrap();
    fs::write(scratch.path().join("empty.mbox"), b"").unwrap();

    let inventory = run(
        &res,
        &[
            scratch.path().join("empty.eml"),
            scratch.path().join("empty.mbox"),
        ],
    );
    let [eml, mbox] = inventory.sources() else {
        panic!("two sources expected: {:?}", inventory.sources())
    };
    assert_eq!(eml.message_count(), 1);
    assert_eq!(
        (mbox.message_count(), mbox.quoting()),
        (0, MboxQuoting::Equivalent)
    );
    for source in inventory.sources() {
        assert_eq!(
            SourceInventory::from_json(&source.to_json())
                .unwrap()
                .to_json(),
            source.to_json()
        );
    }
}

#[test]
fn captured_inventory_ignores_later_changes_to_the_original() {
    let (scratch, res) = resources("durable");
    let mbox = scratch.path().join("In.mbox");
    fs::write(&mbox, MBOX_TWO).unwrap();

    let inventory = run(&res, std::slice::from_ref(&mbox));
    let [source] = inventory.sources() else {
        panic!("one source expected")
    };
    let snapshot = source.snapshot().clone();
    let json = source.to_json();

    // Grow, shrink and delete: the captured stream is unaffected.
    fs::write(&mbox, b"From x@y.test\n\ndifferent bytes now\n").unwrap();
    let copy = scratch
        .path()
        .join(format!("store/recovery/import-source-{}", snapshot.token()));
    assert_eq!(fs::read(&copy).unwrap(), MBOX_TWO);
    let again = run(&res, std::slice::from_ref(&mbox));
    assert_ne!(again.sources()[0].snapshot().sha256(), snapshot.sha256());

    fs::remove_file(&mbox).unwrap();
    assert_eq!(fs::read(&copy).unwrap(), MBOX_TWO);
    // The recorded metadata still reconstructs.
    assert_eq!(
        SourceInventory::from_json(&json).unwrap().snapshot(),
        &snapshot
    );
}

#[test]
fn a_snapshot_that_grows_during_scan_is_not_a_complete_inventory() {
    use std::io::Write;

    let (scratch, res) = resources("changed-snapshot");
    let source = scratch.path().join("source.mbox");
    fs::write(&source, MBOX_ONE).unwrap();
    let mut changed = false;
    let outcome = res.inventory_import_sources(&[source], |step| {
        if matches!(step, ImportProgress::Scanning { .. }) && !changed {
            let name = snapshot_names(&scratch).pop().unwrap();
            fs::OpenOptions::new()
                .append(true)
                .open(scratch.path().join("store/recovery").join(name))
                .unwrap()
                .write_all(MBOX_ONE)
                .unwrap();
            changed = true;
        }
        ControlFlow::Continue(())
    });
    let inventory = outcome.inventory();
    assert!(changed);
    assert!(inventory.sources().is_empty(), "changed copy was accepted");
    assert_eq!(inventory.problems().len(), 1);
    assert_eq!(snapshot_names(&scratch).len(), 1, "evidence was removed");
}

#[test]
fn fixtures_count_bytes_and_report_ambiguity() {
    let (_scratch, res) = resources("fixtures");
    let fixtures = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/import");
    let inventory = run(
        &res,
        &[fixtures.join("Inbox.mbox"), fixtures.join("quoting.mbox")],
    );
    assert!(inventory.problems().is_empty());
    let [inbox, quoting] = inventory.sources() else {
        panic!("two sources expected: {:?}", inventory.sources())
    };
    assert_eq!(
        (inbox.mailbox(), inbox.message_count(), inbox.quoting()),
        ("Inbox", 2, MboxQuoting::Equivalent)
    );
    assert_eq!(inbox.snapshot().byte_length(), 268);
    assert_eq!(
        inbox.snapshot().sha256_hex(),
        "3a4e94e39a4c23bdaeb5df6092bc4ad83bec3e915c16562740edd4495013bb2f"
    );
    assert_eq!(
        (
            quoting.mailbox(),
            quoting.message_count(),
            quoting.quoting()
        ),
        ("quoting", 1, MboxQuoting::Ambiguous)
    );
    assert_eq!(quoting.snapshot().byte_length(), 289);
    assert_eq!(
        quoting.snapshot().sha256_hex(),
        "724fe2fa49eac578cfb0eaba45497015979618d5c19d1ddf4f7e39d5aba0bacc"
    );
    assert_eq!(
        SourceInventory::from_json(&quoting.to_json())
            .unwrap()
            .to_json(),
        quoting.to_json()
    );
}

#[test]
fn source_json_round_trips_through_v1() {
    let (scratch, res) = resources("json");
    let eml = scratch.path().join("solo.eml");
    fs::write(&eml, EML).unwrap();
    let inventory = run(&res, &[eml]);
    let source = &inventory.sources()[0];

    let json = source.to_json();
    let rebuilt = SourceInventory::from_json(&json).unwrap();
    assert_eq!(rebuilt.to_json(), json);
    assert_eq!(rebuilt.path(), source.path());
    assert_eq!(rebuilt.mailbox(), "solo");
    assert_eq!(rebuilt.kind(), SourceKind::Eml);
    assert_eq!(rebuilt.message_count(), 1);
    assert_eq!(rebuilt.snapshot(), source.snapshot());
}

#[test]
fn source_json_validation_rejects_malformed_metadata() {
    use serde_json::{Value, json};

    let valid = json!({
        "v": 1, "path": "/x/y.eml", "mailbox": "y", "kind": "eml",
        "snapshot": {"token": "0".repeat(32), "sha256": "a".repeat(64), "byte_length": 3},
        "messages": 1, "quoting": "equivalent",
    });
    assert!(SourceInventory::from_json(&valid.to_string()).is_ok());
    let reject = |json: &str| {
        assert!(SourceInventory::from_json(json).is_err(), "accepted {json}");
    };
    reject("not json");
    reject("[1]");
    for (pointer, replacement) in [
        ("/v", json!(2)),
        ("/v", json!("1")),
        ("/path", json!("relative/x.eml")),
        ("/path", json!("")),
        ("/path", json!("/x/\0")),
        ("/mailbox", json!("")),
        ("/mailbox", json!("x\0")),
        ("/kind", json!("zip")),
        ("/kind", json!("mbox")),
        ("/messages", json!(2)),
        ("/messages", json!(-1)),
        ("/messages", json!("1")),
        ("/quoting", json!("vague")),
        ("/quoting", json!("ambiguous")),
        ("/snapshot", Value::Null),
        ("/snapshot/byte_length", json!(-1)),
        ("/snapshot/token", json!("F".repeat(32))),
        ("/snapshot/token", json!(".".repeat(32))),
        ("/snapshot/sha256", json!("A".repeat(64))),
        ("/snapshot/sha256", json!("g".repeat(64))),
        ("/snapshot/sha256", json!("a".repeat(63))),
    ] {
        let mut broken = valid.clone();
        *broken.pointer_mut(pointer).unwrap() = replacement;
        reject(&broken.to_string());
    }
    for field in [
        "v", "path", "mailbox", "kind", "snapshot", "messages", "quoting",
    ] {
        let mut broken = valid.clone();
        broken.as_object_mut().unwrap().remove(field);
        reject(&broken.to_string());
    }
    for field in ["token", "sha256", "byte_length"] {
        let mut broken = valid.clone();
        broken["snapshot"].as_object_mut().unwrap().remove(field);
        reject(&broken.to_string());
    }
    let mut mbox = valid.clone();
    mbox["kind"] = json!("mbox");
    mbox["messages"] = json!(2);
    mbox["snapshot"]["byte_length"] = json!(11);
    assert!(SourceInventory::from_json(&mbox.to_string()).is_ok());
    mbox["messages"] = json!(0);
    mbox["snapshot"]["byte_length"] = json!(0);
    assert!(SourceInventory::from_json(&mbox.to_string()).is_ok());
    mbox["quoting"] = json!("ambiguous");
    reject(&mbox.to_string());
    mbox["quoting"] = json!("equivalent");
    mbox["messages"] = json!(u64::MAX);
    mbox["snapshot"]["byte_length"] = json!(u64::MAX);
    reject(&mbox.to_string());
    // Refused before parsing, over the import write budget.
    reject(&format!(
        "{{\"v\":1,\"pad\":\"{}\"}}",
        "x".repeat(MAX_IMPORT_CHANGES_BYTES)
    ));
}

#[test]
fn cancellation_during_discovery_is_an_explicit_outcome() {
    let (scratch, res) = resources("cancel-walk");
    let dir = scratch.path().join("d");
    fs::create_dir(&dir).unwrap();
    for name in ["a.eml", "b.eml", "c.eml", "e.eml"] {
        fs::write(dir.join(name), EML).unwrap();
    }
    let mut checks = 0;
    let outcome = res.inventory_import_sources(std::slice::from_ref(&dir), |step| {
        if matches!(step, ImportProgress::Checking { .. }) {
            checks += 1;
        }
        if checks >= 3 {
            ControlFlow::Break(())
        } else {
            ControlFlow::Continue(())
        }
    });
    let inventory = match outcome {
        ImportInventoryOutcome::Cancelled(inventory) => inventory,
        ImportInventoryOutcome::Complete(_) => panic!("cancellation was not reported"),
    };
    // The check order is root, a.eml, b.eml: only `a.eml` finished.
    let [only] = inventory.sources() else {
        panic!("one source expected: {:?}", inventory.sources())
    };
    assert_eq!(only.mailbox(), "d");
}

#[test]
fn cancellation_during_capture_is_an_explicit_outcome() {
    let (scratch, res) = resources("cancel-copy");
    let mbox = scratch.path().join("Big.mbox");
    let mut bytes = b"From s@t.test\n\n".to_vec();
    bytes.extend(std::iter::repeat_n(b'x', 200_000));
    fs::write(&mbox, &bytes).unwrap();

    let outcome = res.inventory_import_sources(std::slice::from_ref(&mbox), |step| match step {
        ImportProgress::Capturing { bytes, .. } if bytes > 0 => ControlFlow::Break(()),
        _ => ControlFlow::Continue(()),
    });
    let inventory = match outcome {
        ImportInventoryOutcome::Cancelled(inventory) => inventory,
        ImportInventoryOutcome::Complete(_) => panic!("cancellation was not reported"),
    };
    assert!(inventory.sources().is_empty());
    // A cancelled capture returns no completed snapshot and removes
    // only the file it made (spec/03).
    assert!(snapshot_names(&scratch).is_empty());
}

#[test]
fn cancellation_inside_a_long_scan_line_keeps_the_snapshot() {
    let (scratch, res) = resources("cancel-scan");
    let mbox = scratch.path().join("Long.mbox");
    let mut bytes = b"From s@t.test\n\n".to_vec();
    bytes.extend(std::iter::repeat_n(b'x', 200_000)); // one line, no newline
    fs::write(&mbox, &bytes).unwrap();

    let outcome = res.inventory_import_sources(std::slice::from_ref(&mbox), |step| match step {
        ImportProgress::Scanning { bytes, .. } if bytes > 50_000 => ControlFlow::Break(()),
        _ => ControlFlow::Continue(()),
    });
    let inventory = match outcome {
        ImportInventoryOutcome::Cancelled(inventory) => inventory,
        ImportInventoryOutcome::Complete(_) => panic!("cancellation was not reported"),
    };
    assert!(inventory.sources().is_empty());
    // The completed private copy stays as recovery evidence until the
    // engine's terminal-job cleanup (spec/03).
    assert_eq!(snapshot_names(&scratch).len(), 1);
}

/// Yields at most `left` bytes of `inner`, then fails.
struct FailAfter<R> {
    inner: R,
    left: usize,
}

impl<R: Read> Read for FailAfter<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.left == 0 {
            return Err(io::Error::other("the disk is gone"));
        }
        let allowed = self.left.min(buf.len());
        let n = self.inner.read(&mut buf[..allowed])?;
        self.left -= n;
        Ok(n)
    }
}

#[test]
fn scan_failure_preserves_the_source_offset() {
    let mut bytes = b"From s@t.test\n\n".to_vec();
    bytes.extend_from_slice(b"aaaaaaaaaa");
    let mut progress = |_: ImportProgress<'_>| ControlFlow::Continue(());
    let reader = FailAfter {
        inner: io::Cursor::new(bytes),
        left: 20,
    };
    match scan_snapshot(Path::new("/t"), reader, &mut progress) {
        Scan::Failed(MboxError::Io { offset, .. }) => assert_eq!(offset, 20),
        _ => panic!("expected a scan I/O failure at byte 20"),
    }
}

const DEEP_ROOT: &str = "SB_IMPORT_DEEP_ROOT";
const CHILD_PROOF: &str = "child-ran";
const WALK_STACK_BYTES: usize = 512 * 1024;
// Keep the canonical path below each host's path-length limit.
#[cfg(target_os = "linux")]
const WALK_DEPTH: usize = 1_000;
#[cfg(target_os = "macos")]
const WALK_DEPTH: usize = 300;

#[test]
#[ignore = "spawned by deeply_nested_sources_do_not_consume_the_thread_stack"]
fn deep_inventory_child() {
    let root = PathBuf::from(std::env::var_os(DEEP_ROOT).unwrap());
    let res = Resources::open(&root.join("store")).unwrap();
    let input = root.join("input");
    fs::create_dir(&input).unwrap();
    let mut deepest = input.clone();
    for _ in 0..WALK_DEPTH {
        deepest.push("d");
        fs::create_dir(&deepest).unwrap();
    }
    fs::write(deepest.join("leaf.eml"), EML).unwrap();
    std::thread::Builder::new()
        .stack_size(WALK_STACK_BYTES)
        .spawn(move || {
            let inventory = run(&res, &[input]);
            assert!(inventory.problems().is_empty());
            assert_eq!(inventory.sources().len(), 1);
            res.close().unwrap();
            fs::write(root.join(CHILD_PROOF), b"ok").unwrap();
        })
        .unwrap()
        .join()
        .unwrap();
}

#[test]
fn deeply_nested_sources_do_not_consume_the_thread_stack() {
    use std::time::{Duration, Instant};

    let scratch = ScratchDir::new("deep-inventory");
    let mut child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--exact",
            "import::tests::deep_inventory_child",
            "--ignored",
            "--nocapture",
        ])
        .env(DEEP_ROOT, scratch.path())
        .spawn()
        .unwrap();
    let deadline = Instant::now() + Duration::from_secs(120);
    loop {
        if let Some(status) = child.try_wait().unwrap() {
            assert!(status.success(), "deep inventory failed: {status}");
            assert_eq!(
                fs::read(scratch.path().join(CHILD_PROOF))
                    .expect("the child filter matched no test"),
                b"ok"
            );
            break;
        }
        if Instant::now() >= deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("deep inventory did not finish before its deadline");
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
