//! Native proof for staged evidence re-authentication (b6b3a): a real
//! prepared stage re-reads its manifest and objects through its retained
//! root; any changed byte, length, name binding or declared fact is
//! refused, cancellation and fired I/O faults surface with their cause,
//! and nothing in the stage, source or live store is touched.
use super::super::super::stage_evidence::{self, EvidencePhase, Expected, StagedEvidence};
use super::*;
use std::io;
use std::path::PathBuf;

fn candidate(source: &StagingSource) -> crate::PreparedRestoreStage {
    prepare(
        &source.store,
        source.select_with(&source.store),
        source.receipt.location(),
        source.metadata,
    )
}

fn expected(stage: &crate::PreparedRestoreStage) -> Expected<'_> {
    Expected {
        manifest_sha256: hex32(stage.manifest_sha256()),
        manifest_bytes: stage.manifest_bytes,
        generation_id: stage.generation_id(),
        object_count: u64::try_from(stage.metadata.object_count).unwrap(),
        total_bytes: u64::try_from(stage.metadata.total_bytes).unwrap(),
    }
}

fn reauthenticate(
    stage: &crate::PreparedRestoreStage,
    expected: &Expected<'_>,
    cancel: &AtomicBool,
    progress: impl FnMut(EvidencePhase),
) -> Result<StagedEvidence, StoreError> {
    stage_evidence::reauthenticate(&stage.files, expected, cancel, progress)
}

fn quietly(stage: &crate::PreparedRestoreStage) -> Result<StagedEvidence, StoreError> {
    reauthenticate(stage, &expected(stage), &AtomicBool::new(false), |_| {})
}

fn assert_corrupt(result: Result<StagedEvidence, StoreError>, case: &str) {
    match result {
        Err(StoreError::Corrupt { .. }) => {}
        other => panic!("{case}: expected Corrupt, got {other:?}"),
    }
}

/// Every staged object file, sorted.
fn staged_objects(stage: &crate::PreparedRestoreStage) -> Vec<PathBuf> {
    let mut found = Vec::new();
    let mut pending = vec![stage.path().join("objects/sha256")];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                found.push(path);
            }
        }
    }
    found.sort();
    found
}

/// Rewrite `path` in place for one case, then put the original back.
fn with_bytes(path: &Path, bytes: &[u8], check: impl FnOnce()) {
    let original = std::fs::read(path).unwrap();
    std::fs::write(path, bytes).unwrap();
    check();
    std::fs::write(path, original).unwrap();
}

fn flipped(mut bytes: Vec<u8>, at: usize) -> Vec<u8> {
    bytes[at] ^= 0x20;
    bytes
}

#[test]
fn a_prepared_stage_reauthenticates_to_its_snapshot_identity() {
    let source = staging_source("evidence-happy");
    let stage = candidate(&source);
    let store_uuid: String = reading(&source.store, |tx| {
        tx.query_row(
            "SELECT value FROM store_meta WHERE key='store_uuid'",
            [],
            |row| row.get(0),
        )
        .map_err(bug)
    });
    let before = source.live_incarnation();

    let mut seen = Vec::new();
    let evidence = reauthenticate(
        &stage,
        &expected(&stage),
        &AtomicBool::new(false),
        |phase| seen.push(phase),
    )
    .unwrap();

    assert_eq!(evidence.store_uuid, store_uuid);
    assert!(evidence.schema_version > 0);
    assert_eq!(
        seen,
        [
            EvidencePhase::ManifestAuthenticated,
            EvidencePhase::ObjectsAuthenticated
        ]
    );
    assert_eq!(
        staged_objects(&stage).len(),
        4,
        "the fixture's four objects"
    );
    // Read-only: a second pass agrees, and nothing else moved.
    assert_eq!(quietly(&stage).unwrap(), evidence);
    assert_eq!(source.live_incarnation(), before);
    source.assert_untouched();
}

#[test]
fn a_changed_staged_manifest_is_refused() {
    let source = staging_source("evidence-manifest");
    let stage = candidate(&source);
    let path = stage.path().join("manifest.json");
    let original = std::fs::read(&path).unwrap();
    let middle = original.len() / 2;

    with_bytes(&path, &flipped(original.clone(), middle), || {
        assert_corrupt(quietly(&stage), "a flipped byte")
    });
    let mut grown = original.clone();
    grown.push(b' ');
    with_bytes(&path, &grown, || {
        assert_corrupt(quietly(&stage), "one byte of growth")
    });
    let mut padded = original.clone();
    padded.extend(std::iter::repeat_n(b' ', 1 << 20));
    with_bytes(&path, &padded, || {
        assert_corrupt(quietly(&stage), "a megabyte of growth")
    });
    with_bytes(&path, &original[..original.len() - 1], || {
        assert_corrupt(quietly(&stage), "one byte short")
    });
    with_bytes(&path, b"", || assert_corrupt(quietly(&stage), "emptied"));

    // A same-bytes replacement is a new file the digest still accepts:
    // names are authenticated by content here, by identity in b6b3b.
    std::fs::rename(&path, stage.path().join("manifest.old")).unwrap();
    std::fs::write(&path, &original).unwrap();
    quietly(&stage).unwrap();
    std::fs::remove_file(&path).unwrap();
    std::fs::rename(stage.path().join("manifest.old"), &path).unwrap();

    std::fs::remove_file(&path).unwrap();
    assert_eq!(
        quietly(&stage).unwrap_err().io_kind(),
        Some(io::ErrorKind::NotFound)
    );
    std::fs::write(&path, &original).unwrap();
    quietly(&stage).unwrap();
    source.assert_untouched();
}

#[test]
fn staged_object_damage_is_refused() {
    let source = staging_source("evidence-objects");
    let stage = candidate(&source);
    for object in staged_objects(&stage) {
        let original = std::fs::read(&object).unwrap();
        with_bytes(&object, &flipped(original.clone(), 0), || {
            assert_corrupt(quietly(&stage), "a flipped object byte")
        });
        let mut grown = original.clone();
        grown.push(0);
        with_bytes(&object, &grown, || {
            assert_corrupt(quietly(&stage), "a grown object")
        });
        if !original.is_empty() {
            with_bytes(&object, &original[..original.len() - 1], || {
                assert_corrupt(quietly(&stage), "a truncated object")
            });
        }
    }

    let object = staged_objects(&stage).remove(0);
    let aside = stage.path().join("object.aside");
    std::fs::rename(&object, &aside).unwrap();
    assert_eq!(
        quietly(&stage).unwrap_err().io_kind(),
        Some(io::ErrorKind::NotFound),
        "a removed object"
    );
    // A link to identical bytes is still not the staged object.
    std::os::unix::fs::symlink(&aside, &object).unwrap();
    assert!(quietly(&stage).is_err(), "a symlinked object");
    std::fs::remove_file(&object).unwrap();
    std::fs::rename(&aside, &object).unwrap();

    // Nor is a linked hash directory holding identical bytes.
    let leaf = object.parent().unwrap().to_path_buf();
    let moved = stage.path().join("leaf.aside");
    std::fs::rename(&leaf, &moved).unwrap();
    std::os::unix::fs::symlink(&moved, &leaf).unwrap();
    assert!(quietly(&stage).is_err(), "a symlinked hash directory");
    std::fs::remove_file(&leaf).unwrap();
    std::fs::rename(&moved, &leaf).unwrap();

    quietly(&stage).unwrap();
    source.assert_untouched();
}

#[test]
fn declared_facts_must_match_the_staged_manifest() {
    let source = staging_source("evidence-facts");
    let mut stage = candidate(&source);
    let cancel = AtomicBool::new(false);
    let base = expected(&stage);
    let generation = "00000000-0000-4000-8000-000000000000".to_owned();
    let mut digest = base.manifest_sha256;
    digest[31] ^= 1;
    let cases = [
        (
            "object count",
            Expected {
                object_count: base.object_count + 1,
                ..base
            },
        ),
        (
            "byte total",
            Expected {
                total_bytes: base.total_bytes - 1,
                ..base
            },
        ),
        (
            "generation",
            Expected {
                generation_id: &generation,
                ..base
            },
        ),
        (
            "manifest digest",
            Expected {
                manifest_sha256: digest,
                ..base
            },
        ),
        (
            "manifest length",
            Expected {
                manifest_bytes: base.manifest_bytes + 1,
                ..base
            },
        ),
        (
            "short manifest length",
            Expected {
                manifest_bytes: base.manifest_bytes - 1,
                ..base
            },
        ),
        (
            "overflowing length",
            Expected {
                manifest_bytes: u64::MAX,
                ..base
            },
        ),
    ];
    for (case, wrong) in &cases {
        assert_corrupt(reauthenticate(&stage, wrong, &cancel, |_| {}), case);
    }

    // The manifest must name the database staging retained.
    stage.files.database_sha256[0] ^= 1;
    assert_corrupt(quietly(&stage), "a different database digest");
    stage.files.database_sha256[0] ^= 1;
    stage.files.database_bytes += 1;
    assert_corrupt(quietly(&stage), "a different database length");
    stage.files.database_bytes -= 1;
    quietly(&stage).unwrap();
    source.assert_untouched();
}

#[test]
fn cancellation_is_observed_before_and_after_each_pass() {
    let source = staging_source("evidence-cancel");
    let stage = candidate(&source);
    let armed = AtomicBool::new(true);
    assert!(matches!(
        reauthenticate(&stage, &expected(&stage), &armed, |_| {}),
        Err(StoreError::Unavailable { .. })
    ));
    for point in [
        EvidencePhase::ManifestAuthenticated,
        EvidencePhase::ObjectsAuthenticated,
    ] {
        let cancel = AtomicBool::new(false);
        let mut seen = Vec::new();
        let result = reauthenticate(&stage, &expected(&stage), &cancel, |phase| {
            seen.push(phase);
            if phase == point {
                cancel.store(true, Ordering::Release);
            }
        });
        assert!(
            matches!(result, Err(StoreError::Unavailable { io_kind: None, .. })),
            "{point:?}: {result:?}"
        );
        assert_eq!(seen.last(), Some(&point));
    }
    quietly(&stage).unwrap();
    source.assert_untouched();
}

#[test]
fn fired_open_and_read_faults_keep_their_native_cause() {
    let source = staging_source("evidence-faults");
    let mut stage = candidate(&source);
    for operation in [FsOp::Open, FsOp::Read] {
        // Calibrate: count this operation over one whole clean pass.
        let counting = Arc::new(FaultFs::new(
            real_fs(),
            operation,
            u64::MAX,
            FaultKind::Kind(io::ErrorKind::PermissionDenied),
        ));
        stage.files.root = counting.open_confined_dir(stage.path()).unwrap();
        let opened = counting.calls(operation);
        quietly(&stage).unwrap();
        let total = counting.calls(operation) - opened;
        assert!(total > 0, "{operation:?} must occur");
        for nth in 1..=total {
            let fault = Arc::new(FaultFs::new(
                real_fs(),
                operation,
                opened + nth,
                FaultKind::Kind(io::ErrorKind::PermissionDenied),
            ));
            stage.files.root = fault.open_confined_dir(stage.path()).unwrap();
            let result = quietly(&stage);
            assert_eq!(
                result.unwrap_err().io_kind(),
                Some(io::ErrorKind::PermissionDenied),
                "{operation:?} #{nth} of {total}"
            );
        }
    }
    source.assert_untouched();
}
