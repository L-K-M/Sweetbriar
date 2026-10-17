//! Acceptance tests for import source snapshots (plan M1.2c1,
//! spec/03 "Import source snapshots"): exact bytes through capture
//! and verified reopen, changing originals, metadata reconstruction
//! and token validation, cancellation, name collisions, injected I/O
//! and flush faults, and the SimFs power-loss proof.

use std::io::{self, Cursor, Read, Seek, SeekFrom, Write};
use std::ops::ControlFlow;
use std::path::{Path, PathBuf};
use std::sync::Arc;

use sb_test_support::{FaultFs, FaultKind, FsOp, ScratchDir, SeededRandom, SimFs, real_fs};
use sb_types::fs::Fs;
use sb_types::random::Random;
use sha2::{Digest, Sha256};

use super::*;

/// A capturer over `fs`, copies landing in `recovery`, drawing names
/// from `seed`.
fn capturer(fs: Arc<dyn Fs>, recovery: &Path, seed: u64) -> ImportSourceSnapshots {
    ImportSourceSnapshots::new(
        fs,
        Arc::new(SeededRandom::new(seed)),
        recovery.to_path_buf(),
    )
}

/// Scratch `recovery/` and `sources/` directories on the real fs.
fn real_dirs(label: &str) -> (ScratchDir, PathBuf, PathBuf) {
    let scratch = ScratchDir::new(label);
    let recovery = scratch.path().join("recovery");
    let sources = scratch.path().join("sources");
    std::fs::create_dir(&recovery).unwrap();
    std::fs::create_dir(&sources).unwrap();
    (scratch, recovery, sources)
}

/// The SHA-256 the way the capture computes it.
fn sha256_of(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

/// Where the private copy sits — tests may look inside `recovery/`
/// even though the API never exposes the path.
fn copy_path(recovery: &Path, snapshot: &ImportSourceSnapshot) -> PathBuf {
    recovery.join(format!("import-source-{}", snapshot.token()))
}

/// The entries currently in `recovery/` (only captures write there).
fn copies(fs: &dyn Fs, recovery: &Path) -> Vec<PathBuf> {
    fs.read_dir(recovery).unwrap()
}

/// A capture expected to succeed; panics otherwise.
fn captured(snaps: &ImportSourceSnapshots, source: &Path) -> ImportSourceSnapshot {
    match snaps.capture(source, |_| ControlFlow::Continue(())) {
        Ok(ImportSourceCapture::Captured { snapshot, .. }) => snapshot,
        other => panic!("the capture did not succeed: {other:?}"),
    }
}

/// Read a snapshot fully through `open`.
fn read_all(snaps: &ImportSourceSnapshots, snapshot: &ImportSourceSnapshot) -> Vec<u8> {
    let mut read = Vec::new();
    snaps
        .open(snapshot)
        .unwrap()
        .read_to_end(&mut read)
        .unwrap();
    read
}

/// The error `open` must produce; a successfully opened file is a
/// failed test (kept Debug-free because `Box<dyn ReadFile>` is not).
fn open_err(snaps: &ImportSourceSnapshots, snapshot: &ImportSourceSnapshot) -> StoreError {
    match snaps.open(snapshot) {
        Err(e) => e,
        Ok(_) => panic!("the snapshot opened unexpectedly"),
    }
}

/// Capture streams exact binary bytes — a multi-chunk payload with
/// NULs and non-UTF8 — into `recovery/` under the token's name,
/// stamped with the real hash and length; `open` returns the verified
/// copy rewound and seekable; progress saw each chunk's total. The
/// captured copy stands alone: the original may change or go.
#[test]
fn capture_copies_exact_bytes_and_reopen_is_seekable() {
    let (_scratch, recovery, sources) = real_dirs("snap-round-trip");
    let bytes: Vec<u8> = (0..70_000u32).map(|i| (i % 251) as u8).collect();
    let source = sources.join("original.bin");
    std::fs::write(&source, &bytes).unwrap();
    let snaps = capturer(real_fs(), &recovery, 3);

    let mut seen = Vec::new();
    let result = snaps.capture(&source, |done| {
        seen.push(done);
        ControlFlow::Continue(())
    });
    let (source_path, snapshot) = match result.unwrap() {
        ImportSourceCapture::Captured {
            source_path,
            snapshot,
        } => (source_path, snapshot),
        other => panic!("expected Captured, got {other:?}"),
    };
    assert_eq!(source_path, source.canonicalize().unwrap());
    assert_eq!(snapshot.byte_length(), bytes.len() as u64);
    assert_eq!(snapshot.sha256(), sha256_of(&bytes));
    assert_eq!(snapshot.sha256_hex().len(), 64);
    assert_eq!(seen, vec![0, 65_536, 70_000]);
    let token = snapshot.token();
    assert!(is_token(token), "the token is not lowercase hex: {token}");
    assert_eq!(
        std::fs::read(copy_path(&recovery, &snapshot)).unwrap(),
        bytes
    );

    // `open` verifies then rewinds; the file it returns seeks.
    let mut file = snaps.open(&snapshot).unwrap();
    let mut read = Vec::new();
    file.read_to_end(&mut read).unwrap();
    assert_eq!(read, bytes);
    file.seek(SeekFrom::Start(7)).unwrap();
    let mut tail = Vec::new();
    file.read_to_end(&mut tail).unwrap();
    assert_eq!(tail, bytes[7..]);
    file.seek(SeekFrom::Start(0)).unwrap();
    read.clear();
    file.read_to_end(&mut read).unwrap();
    assert_eq!(read, bytes);

    // A changed original, then a deleted one, cannot move the copy.
    std::fs::write(&source, b"replaced entirely").unwrap();
    assert_eq!(read_all(&snaps, &snapshot), bytes);
    std::fs::remove_file(&source).unwrap();
    assert_eq!(read_all(&snaps, &snapshot), bytes);
}

#[test]
fn store_reopens_its_private_snapshot_after_shutdown() {
    use crate::ObjectStore;
    let (_scratch, paths) = crate::testing::scratch_paths("snapshot-store");
    let source = paths.data_root().parent().unwrap().join("source.eml");
    std::fs::write(&source, b"Subject: captured\n\noriginal bytes\n").unwrap();
    let store = crate::open(&paths, &crate::testing::test_seams(real_fs())).unwrap();
    let snapshot = match store
        .capture_import_source(&source, |_| ControlFlow::Continue(()))
        .unwrap()
    {
        ImportSourceCapture::Captured { snapshot, .. } => snapshot,
        ImportSourceCapture::Cancelled => panic!("capture cancelled"),
    };
    assert_eq!(store.objects().list().unwrap().count(), 0);
    store.close().unwrap();
    std::fs::remove_file(&source).unwrap();
    let store = crate::open(&paths, &crate::testing::test_seams(real_fs())).unwrap();
    let mut bytes = Vec::new();
    store
        .open_import_source(&snapshot)
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(bytes, b"Subject: captured\n\noriginal bytes\n");
    store.close().unwrap();
}

/// A checkpoint's stored token, hash and length rebuild an openable
/// snapshot; anything that is not 32 lowercase hex digits — a path,
/// separators, uppercase, a wrong length — is refused.
#[test]
fn from_metadata_rebuilds_and_rejects_bad_tokens() {
    let (_scratch, recovery, sources) = real_dirs("snap-metadata");
    let source = sources.join("m.eml");
    std::fs::write(&source, b"meta").unwrap();
    let snaps = capturer(real_fs(), &recovery, 3);
    let snapshot = captured(&snaps, &source);
    let rebuilt = ImportSourceSnapshot::from_metadata(
        snapshot.token(),
        snapshot.sha256(),
        snapshot.byte_length(),
    )
    .unwrap();
    assert_eq!(rebuilt, snapshot);
    assert_eq!(read_all(&snaps, &rebuilt), b"meta");

    for bad in [
        String::new(),
        "0".repeat(31),
        "a".repeat(33),
        "A".repeat(32),
        "g".repeat(32),
        "../escape/../../../../x".to_owned(),
        format!("../{}", "0".repeat(29)),
        "0/".repeat(16),
    ] {
        assert!(
            matches!(
                ImportSourceSnapshot::from_metadata(&bad, [0; 32], 0),
                Err(StoreError::InvalidInput { .. })
            ),
            "bad token accepted: {bad:?}"
        );
    }
}

/// An empty source captures an empty snapshot; a directory is refused
/// as input; a missing source is `NotFound`.
#[test]
fn empty_missing_and_non_file_sources() {
    let (_scratch, recovery, sources) = real_dirs("snap-kinds");
    let snaps = capturer(real_fs(), &recovery, 3);
    let empty = sources.join("empty.mbox");
    std::fs::write(&empty, b"").unwrap();
    let snapshot = captured(&snaps, &empty);
    assert_eq!(snapshot.byte_length(), 0);
    assert_eq!(snapshot.sha256(), sha256_of(b""));
    assert!(read_all(&snaps, &snapshot).is_empty());
    assert!(matches!(
        snaps.capture(&sources, |_| ControlFlow::Continue(())),
        Err(StoreError::InvalidInput { .. })
    ));
    assert!(matches!(
        snaps.capture(&sources.join("missing"), |_| ControlFlow::Continue(())),
        Err(StoreError::NotFound { .. })
    ));
}

/// `open` discriminates: missing is `NotFound`; a wrong kind, length
/// or hash is `Corrupt`, and the evidence stays in `recovery/`. The
/// original's path is never substituted, even while it still exists.
#[test]
fn open_marks_missing_corrupt_and_keeps_evidence() {
    let (_scratch, recovery, sources) = real_dirs("snap-open-errors");
    let bytes: &[u8] = b"corrupt me in every way";
    let source = sources.join("orig");
    std::fs::write(&source, bytes).unwrap();
    let snaps = capturer(real_fs(), &recovery, 3);
    let snapshot = captured(&snaps, &source);
    let copy = copy_path(&recovery, &snapshot);

    // Missing: remove the copy; the living original is no substitute.
    std::fs::remove_file(&copy).unwrap();
    assert!(source.exists());
    assert!(matches!(
        open_err(&snaps, &snapshot),
        StoreError::NotFound { .. }
    ));

    // Wrong kind: a directory under the token's name.
    std::fs::create_dir(&copy).unwrap();
    assert!(matches!(
        open_err(&snaps, &snapshot),
        StoreError::Corrupt { .. }
    ));
    assert!(copy.is_dir(), "the evidence directory was removed");
    std::fs::remove_dir(&copy).unwrap();

    // Wrong length, shorter and longer.
    for damaged in [
        bytes[..bytes.len() - 3].to_vec(),
        [bytes, b"extra"].concat(),
    ] {
        std::fs::write(&copy, &damaged).unwrap();
        match open_err(&snaps, &snapshot) {
            StoreError::Corrupt { detail } => assert!(detail.contains("bytes"), "{detail}"),
            other => panic!("expected Corrupt for a wrong length, got {other:?}"),
        }
        assert_eq!(std::fs::read(&copy).unwrap(), damaged, "evidence removed");
    }

    // Same length, flipped byte: fails the hash, stays put.
    let mut flipped = bytes.to_vec();
    flipped[0] ^= 0xff;
    std::fs::write(&copy, &flipped).unwrap();
    match open_err(&snaps, &snapshot) {
        StoreError::Corrupt { detail } => assert!(detail.contains("hash"), "{detail}"),
        other => panic!("expected Corrupt for a damaged copy, got {other:?}"),
    }
    assert_eq!(std::fs::read(&copy).unwrap(), flipped, "evidence removed");
}

/// Breaking `progress` before or during the copy cancels: no
/// descriptor, and this attempt's private file — the only file a
/// cancel may remove — is gone. The original is untouched.
#[test]
fn cancellation_removes_only_the_attempts_file() {
    let (_scratch, recovery, sources) = real_dirs("snap-cancel");
    let source = sources.join("big.bin");
    std::fs::write(&source, vec![7u8; 70_000]).unwrap();
    let snaps = capturer(real_fs(), &recovery, 3);

    let result = snaps.capture(&source, |done| {
        assert_eq!(done, 0, "cancelled before any byte was copied");
        ControlFlow::Break(())
    });
    assert!(matches!(result, Ok(ImportSourceCapture::Cancelled)));
    assert!(copies(real_fs().as_ref(), &recovery).is_empty());

    let mut calls = Vec::new();
    let result = snaps.capture(&source, |done| {
        calls.push(done);
        match calls.len() {
            2 => ControlFlow::Break(()),
            _ => ControlFlow::Continue(()),
        }
    });
    assert!(matches!(result, Ok(ImportSourceCapture::Cancelled)));
    assert_eq!(calls, vec![0, 65_536], "cancelled after the first chunk");
    assert!(copies(real_fs().as_ref(), &recovery).is_empty());
    assert_eq!(std::fs::read(&source).unwrap(), vec![7u8; 70_000]);
}

/// A private name already taken is left alone: the capture draws a
/// new token and the earlier occupant keeps its bytes.
#[test]
fn an_occupied_private_name_is_preserved() {
    let (_scratch, recovery, sources) = real_dirs("snap-collision");
    let source = sources.join("s");
    std::fs::write(&source, b"payload").unwrap();

    // Occupy the first name seed 9 draws; the capture must move on.
    let mut drawn = [0u8; 16];
    SeededRandom::new(9).fill(&mut drawn).unwrap();
    let occupied = recovery.join(format!("import-source-{}", hex(&drawn)));
    std::fs::write(&occupied, b"an earlier attempt's file").unwrap();

    let snaps = capturer(real_fs(), &recovery, 9);
    let snapshot = captured(&snaps, &source);
    assert_ne!(
        snapshot.token(),
        hex(&drawn),
        "the occupied name was reused"
    );
    assert_eq!(
        std::fs::read(&occupied).unwrap(),
        b"an earlier attempt's file"
    );
    assert_eq!(read_all(&snaps, &snapshot), b"payload");
}

#[test]
fn a_create_failure_does_not_own_an_occupied_name() {
    for kind in [FaultKind::Eio, FaultKind::NoSpace] {
        let (_scratch, recovery, sources) = real_dirs("snap-failed-create-collision");
        let source = sources.join("original");
        std::fs::write(&source, b"source bytes").unwrap();
        let mut drawn = [0u8; TOKEN_BYTES];
        SeededRandom::new(9).fill(&mut drawn).unwrap();
        let occupied = recovery.join(format!("import-source-{}", hex(&drawn)));
        std::fs::write(&occupied, b"another job's captured source").unwrap();
        let fs = Arc::new(FaultFs::new(real_fs(), FsOp::Create, 1, kind));
        let snaps = capturer(fs, &recovery, 9);
        assert!(matches!(
            snaps.capture(&source, |_| ControlFlow::Continue(())),
            Err(StoreError::Unavailable { .. })
        ));
        assert_eq!(
            std::fs::read(&occupied).expect("failed create removed an unowned file"),
            b"another job's captured source"
        );
        assert_eq!(std::fs::read(&source).unwrap(), b"source bytes");
    }
}

/// Write, flush and create faults each fail the capture — never a
/// reported success — and remove this attempt's private file; the
/// source is untouched and a retry over the spent fault succeeds.
#[test]
fn injected_io_and_flush_faults_fail_and_clean_up() {
    let cases: [(FsOp, FaultKind, &str); 5] = [
        (FsOp::Write, FaultKind::ShortWrite, "a torn write"),
        (FsOp::Write, FaultKind::NoSpace, "no space"),
        (FsOp::SyncFile, FaultKind::Eio, "the file flush"),
        (FsOp::SyncDir, FaultKind::Eio, "the directory flush"),
        (FsOp::Create, FaultKind::Eio, "the create"),
    ];
    for (op, kind, label) in cases {
        let (_scratch, recovery, sources) = real_dirs("snap-faults");
        let source = sources.join("s");
        std::fs::write(&source, b"some bytes").unwrap();
        let fs: Arc<dyn Fs> = Arc::new(FaultFs::new(real_fs(), op, 1, kind));
        let snaps = capturer(fs.clone(), &recovery, 5);

        assert!(
            matches!(
                snaps.capture(&source, |_| ControlFlow::Continue(())),
                Err(StoreError::Unavailable { .. })
            ),
            "{label}: expected Unavailable"
        );
        assert!(
            copies(fs.as_ref(), &recovery).is_empty(),
            "{label}: the attempt's file stayed"
        );
        assert_eq!(std::fs::read(&source).unwrap(), b"some bytes", "{label}");
        // The fault was spent (nth = 1): a retry over the same fs works.
        assert_eq!(captured(&snaps, &source).byte_length(), 10, "{label}");
    }
}

/// A `Read` over `inner` that fails `Interrupted` `interrupts` times,
/// serves `ok_reads` real reads, then fails once with `error` if set.
struct FlakyRead {
    inner: Cursor<Vec<u8>>,
    interrupts: usize,
    ok_reads: usize,
    error: Option<io::ErrorKind>,
}

impl Read for FlakyRead {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.interrupts > 0 {
            self.interrupts -= 1;
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        if self.ok_reads > 0 {
            self.ok_reads -= 1;
            return self.inner.read(buf);
        }
        match self.error.take() {
            Some(kind) => Err(io::Error::from(kind)),
            None => self.inner.read(buf),
        }
    }
}

/// Never ends: every read fills the buffer. The copy must stop at
/// expected + 1 byte.
struct EndlessRead;

impl Read for EndlessRead {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        buf.fill(0xAB);
        Ok(buf.len())
    }
}

/// `copy_bounded` against scripted sources: `Interrupted` retries, a
/// real error reports the consumed offset, and a stream that no
/// longer matches the observed size — shorter, longer, never-ending —
/// fails with the offset, the bound cutting an endless source at
/// expected + 1 byte rather than streaming forever.
#[test]
fn copy_bounded_bounds_retries_and_reports_offsets() {
    let copy = |source: &mut dyn Read, expected: u64| {
        copy_bounded(
            source,
            &mut Vec::new(),
            expected,
            &mut |_| ControlFlow::Continue(()),
            Path::new("/s"),
        )
    };

    // Interrupted twice, then the bytes land.
    let mut source = FlakyRead {
        inner: Cursor::new(b"interrupted then fine".to_vec()),
        interrupts: 2,
        ok_reads: 0,
        error: None,
    };
    let mut dest = Vec::new();
    let copied = copy_bounded(
        &mut source,
        &mut dest,
        21,
        &mut |_| ControlFlow::Continue(()),
        Path::new("/s"),
    )
    .unwrap();
    assert!(matches!(copied, Copied::Done(_)));
    assert_eq!(dest, b"interrupted then fine");

    // A real read error mid-stream reports the consumed offset.
    let mut source = FlakyRead {
        inner: Cursor::new(vec![1u8; 100]),
        interrupts: 0,
        ok_reads: 1,
        error: Some(io::ErrorKind::ConnectionReset),
    };
    match copy(&mut source, 100).unwrap_err() {
        StoreError::Unavailable { reason } => assert!(reason.contains("100"), "{reason}"),
        other => panic!("expected Unavailable, got {other:?}"),
    }

    // Shorter than observed: ends early, offset named.
    match copy(&mut Cursor::new(b"abcde".to_vec()), 10).unwrap_err() {
        StoreError::Unavailable { reason } => {
            assert!(reason.contains("5") && reason.contains("10"), "{reason}");
        }
        other => panic!("expected Unavailable, got {other:?}"),
    }

    // Longer than observed: caught one byte past the size.
    match copy(&mut Cursor::new(b"0123456789X".to_vec()), 10).unwrap_err() {
        StoreError::Unavailable { reason } => {
            assert!(reason.contains("offset 10"), "{reason}");
        }
        other => panic!("expected Unavailable, got {other:?}"),
    }

    // Never ends: still stops at the bound.
    assert!(matches!(
        copy(&mut EndlessRead, 5).unwrap_err(),
        StoreError::Unavailable { .. }
    ));
}

/// After a successful capture — file and directory flushed — a power
/// cut keeps the copy; `open` still verifies and serves it. The
/// original was left unflushed, so the proof cannot lean on it.
#[test]
fn a_captured_copy_survives_power_loss() {
    let sim = Arc::new(SimFs::new());
    sim.create_dir_all(Path::new("/data/recovery")).unwrap();
    sim.create_dir_all(Path::new("/data/sources")).unwrap();
    let source = Path::new("/data/sources/orig.mbox");
    let mut w = sim.create(source).unwrap();
    w.write_all(b"durable by capture alone").unwrap();
    drop(w);

    let recovery = Path::new("/data/recovery");
    let snaps = capturer(sim.clone(), recovery, 4);
    let snapshot = captured(&snaps, source);
    sim.power_cut();

    assert!(
        sim.metadata(source).unwrap().is_none(),
        "the unflushed original survived; the test proves nothing"
    );
    assert!(
        sim.metadata(&copy_path(recovery, &snapshot))
            .unwrap()
            .is_some(),
        "the captured copy did not survive the cut"
    );
    assert_eq!(read_all(&snaps, &snapshot), b"durable by capture alone");
}
