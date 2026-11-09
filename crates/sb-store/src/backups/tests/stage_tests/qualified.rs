//! Native proof for the qualified binding handoff (b6b3b): a real
//! prepared stage qualifies to its quarantined image; another store's
//! capability, a replaced source, live or stage name (before the
//! quarantine or after its flushes), a changed live incarnation and
//! damaged staged evidence are refused; cancellation at every phase and
//! fired I/O faults hand off nothing; and a SIGKILL anywhere in the
//! qualification leaves the live store, the source and the unqualified
//! stage as they were, with no preserved root.
use super::super::super::qualify::{QualifiedStage, QualifyPhase};
use super::super::super::stage_quarantine::ImagePhase;
use super::*;
use std::io;
use std::sync::Mutex;

const PHASES: [QualifyPhase; 10] = [
    QualifyPhase::BindingsChecked,
    QualifyPhase::EvidenceChecked,
    QualifyPhase::Image(ImagePhase::BytesVerified),
    QualifyPhase::Image(ImagePhase::ReadOnlyInspected),
    QualifyPhase::Image(ImagePhase::WriterInspected),
    QualifyPhase::Image(ImagePhase::Quarantined),
    QualifyPhase::Image(ImagePhase::Checkpointed),
    QualifyPhase::Image(ImagePhase::Closed),
    QualifyPhase::Image(ImagePhase::Flushed),
    QualifyPhase::Rechecked,
];

fn candidate(source: &StagingSource) -> crate::PreparedRestoreStage {
    prepare(
        &source.store,
        source.select_with(&source.store),
        source.receipt.location(),
        source.metadata,
    )
}

fn qualify(
    source: &StagingSource,
    stage: crate::PreparedRestoreStage,
    progress: impl FnMut(QualifyPhase),
) -> Result<QualifiedStage, StoreError> {
    let cancel = Arc::new(AtomicBool::new(false));
    source.store.backups.qualify_with(stage, &cancel, progress)
}

/// The paths a refused qualification must leave as they were.
struct Evidence {
    stage: PathBuf,
    preserved: PathBuf,
    incarnation: String,
}

impl Evidence {
    fn of(source: &StagingSource, stage: &crate::PreparedRestoreStage) -> Self {
        Self {
            stage: stage.path().to_path_buf(),
            preserved: stage.preserved_root().to_path_buf(),
            incarnation: source.live_incarnation(),
        }
    }

    /// The unqualified stage stays behind, nothing was preserved or
    /// swapped, and the live store keeps its incarnation.
    fn assert_kept(&self, source: &StagingSource) {
        assert!(
            self.stage.join("state.sqlite").is_file(),
            "stage evidence kept"
        );
        assert!(!self.preserved.exists(), "no preserved root");
        assert_eq!(source.live_incarnation(), self.incarnation);
    }
}

fn assert_conflict<T: std::fmt::Debug>(result: Result<T, StoreError>, case: &str) {
    match result {
        Err(StoreError::Conflict { .. }) => {}
        other => panic!("{case}: expected Conflict, got {other:?}"),
    }
}

/// Swap `path` for a byte-identical copy under the same name: a new
/// inode (and, for a directory, new children) with the same content.
fn replace_with_copy(path: &Path) {
    let aside = path.with_extension("aside");
    std::fs::rename(path, &aside).unwrap();
    copy_tree(&aside, path);
}

fn copy_tree(from: &Path, to: &Path) {
    if from.is_dir() {
        std::fs::create_dir(to).unwrap();
        for entry in std::fs::read_dir(from).unwrap() {
            let entry = entry.unwrap();
            copy_tree(&entry.path(), &to.join(entry.file_name()));
        }
    } else {
        std::fs::copy(from, to).unwrap();
    }
}

fn staged_database_value(path: &Path, key: &str) -> String {
    let connection =
        Connection::open_with_flags(path, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let value = connection
        .query_row(
            "SELECT value FROM store_meta WHERE key = ?1",
            [key],
            |row| row.get(0),
        )
        .unwrap();
    connection.close().unwrap();
    value
}

#[test]
fn a_prepared_stage_qualifies_to_its_quarantined_image() {
    let source = staging_source("qualified-happy");
    let stage = candidate(&source);
    let (path, incarnation, generation) = (
        stage.path().to_path_buf(),
        stage.incarnation().to_owned(),
        stage.generation_id().to_owned(),
    );
    let (preserved, manifest) = (
        stage.preserved_root().to_path_buf(),
        stage.manifest_sha256().to_owned(),
    );
    let live = source.live_incarnation();
    let store_uuid = staged_database_value(&source.receipt.state_db(), "store_uuid");

    let mut seen = Vec::new();
    let qualified = qualify(&source, stage, |phase| seen.push(phase)).unwrap();

    assert_eq!(seen, PHASES);
    assert_eq!(qualified.path(), path);
    assert_eq!(qualified.incarnation(), incarnation);
    assert_eq!(qualified.generation_id(), generation);
    assert_eq!(qualified.manifest_sha256(), manifest);
    assert_eq!(qualified.preserved_root(), preserved);
    assert_eq!(qualified.source_location(), source.receipt.location());
    assert_eq!(qualified.live_incarnation, live);
    // The handed-off fingerprint is the quarantined file on disk, which
    // carries the new incarnation and the snapshot's own store UUID.
    let image = std::fs::read(path.join("state.sqlite")).unwrap();
    assert_eq!(qualified.image_sha256(), sha256_hex(&image));
    assert_eq!(qualified.image_bytes(), image.len() as u64);
    let staged = path.join("state.sqlite");
    assert_eq!(staged_database_value(&staged, "incarnation"), incarnation);
    assert_eq!(staged_database_value(&staged, "store_uuid"), store_uuid);
    // The live store and the source are untouched, nothing is preserved,
    // and the operation lease stays held by the qualified stage.
    assert_eq!(source.live_incarnation(), live);
    assert!(!preserved.exists());
    source.assert_untouched();
    assert!(
        source
            .store
            .backup_snapshot(BackupDestination::DataRoot)
            .is_err(),
        "the qualified stage holds the backup operation lease"
    );
    drop(qualified);
    assert!(path.is_dir(), "dropping a qualified stage removes nothing");
}

#[test]
fn a_stage_from_another_store_never_qualifies() {
    let source = staging_source("qualified-foreign");
    let other = staging_source("qualified-foreign-other");
    let stage = candidate(&source);
    let evidence = Evidence::of(&source, &stage);

    let cancel = Arc::new(AtomicBool::new(false));
    assert_conflict(
        other.store.backups.qualify_with(stage, &cancel, |_| {}),
        "another store's coordinator",
    );
    evidence.assert_kept(&source);
    source.assert_untouched();
}

/// Where a bound name lives, given the source and the stage path.
type BoundName = fn(&StagingSource, &Path) -> PathBuf;

fn source_generation(source: &StagingSource, _: &Path) -> PathBuf {
    source.receipt.location().to_path_buf()
}

fn live_database(source: &StagingSource, _: &Path) -> PathBuf {
    source.paths.state_db().to_path_buf()
}

fn stage_directory(_: &StagingSource, stage: &Path) -> PathBuf {
    stage.to_path_buf()
}

fn staged_database(_: &StagingSource, stage: &Path) -> PathBuf {
    stage.join("state.sqlite")
}

/// Every name a qualified stage is bound to.
const BOUND_NAMES: [(&str, BoundName); 4] = [
    ("the source generation", source_generation),
    ("the live database", live_database),
    ("the stage directory", stage_directory),
    ("the staged database", staged_database),
];

/// Each name a qualified stage is bound to, swapped for a same-bytes
/// copy before qualification starts.
#[test]
fn a_replaced_name_before_the_handoff_is_refused() {
    for (case, target) in BOUND_NAMES {
        let source = staging_source("qualified-replaced-before");
        let stage = candidate(&source);
        let evidence = Evidence::of(&source, &stage);
        replace_with_copy(&target(&source, stage.path()));

        let result = qualify(&source, stage, |_| {});

        assert!(
            matches!(
                result,
                Err(StoreError::Conflict { .. } | StoreError::Corrupt { .. })
            ),
            "{case}: {result:?}"
        );
        assert!(!evidence.preserved.exists(), "{case}");
    }
}

/// The same substitutions after the quarantine's flushes: the post
/// check refuses, so no qualified stage exists for a moved name.
#[test]
fn a_replaced_name_after_the_flushes_is_refused() {
    for (case, target) in BOUND_NAMES {
        let source = staging_source("qualified-replaced-after");
        let stage = candidate(&source);
        let evidence = Evidence::of(&source, &stage);
        let replaced = target(&source, &evidence.stage);
        let mut seen = Vec::new();

        let result = qualify(&source, stage, |phase| {
            seen.push(phase);
            if phase == QualifyPhase::Image(ImagePhase::Flushed) {
                replace_with_copy(&replaced);
            }
        });

        assert!(
            matches!(
                result,
                Err(StoreError::Conflict { .. } | StoreError::Corrupt { .. })
            ),
            "{case}: {result:?}"
        );
        assert_eq!(
            seen.last(),
            Some(&QualifyPhase::Image(ImagePhase::Flushed)),
            "{case}: never rechecked"
        );
        assert!(!evidence.preserved.exists(), "{case}");
    }
}

#[test]
fn a_changed_live_incarnation_is_refused() {
    for after_flush in [false, true] {
        let source = staging_source("qualified-live-incarnation");
        let stage = candidate(&source);
        let preserved = stage.preserved_root().to_path_buf();
        let fresh = "00000000-0000-4000-8000-0000000000e1";
        let change = || {
            sql(
                &source.store,
                format!("UPDATE store_meta SET value='{fresh}' WHERE key='incarnation'"),
            )
        };
        if !after_flush {
            change();
        }

        let result = qualify(&source, stage, |phase| {
            if after_flush && phase == QualifyPhase::Image(ImagePhase::Flushed) {
                change();
            }
        });

        assert_conflict(result, &format!("after flush: {after_flush}"));
        assert!(!preserved.exists());
    }
}

#[test]
fn damaged_staged_evidence_is_refused_before_and_after_quarantine() {
    for after_flush in [false, true] {
        let source = staging_source("qualified-evidence");
        let stage = candidate(&source);
        let evidence = Evidence::of(&source, &stage);
        let manifest = evidence.stage.join("manifest.json");
        let damage = || {
            let mut bytes = std::fs::read(&manifest).unwrap();
            let middle = bytes.len() / 2;
            bytes[middle] ^= 0x20;
            std::fs::write(&manifest, bytes).unwrap();
        };
        if !after_flush {
            damage();
        }
        let mut seen = Vec::new();

        let result = qualify(&source, stage, |phase| {
            seen.push(phase);
            if after_flush && phase == QualifyPhase::Image(ImagePhase::Flushed) {
                damage();
            }
        });

        assert!(
            matches!(result, Err(StoreError::Corrupt { .. })),
            "after flush {after_flush}: {result:?}"
        );
        if !after_flush {
            assert_eq!(seen, [QualifyPhase::BindingsChecked], "no quarantine ran");
        }
        evidence.assert_kept(&source);
    }
}

#[test]
fn cancellation_at_every_phase_hands_off_nothing() {
    for point in PHASES {
        let source = staging_source("qualified-cancel");
        let stage = candidate(&source);
        let evidence = Evidence::of(&source, &stage);
        let cancel = Arc::new(AtomicBool::new(false));
        let mut seen = Vec::new();

        let result = source.store.backups.qualify_with(stage, &cancel, |phase| {
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
        evidence.assert_kept(&source);
    }

    // A flag raised before the call stops it before any check.
    let source = staging_source("qualified-cancel-armed");
    let stage = candidate(&source);
    let evidence = Evidence::of(&source, &stage);
    let armed = Arc::new(AtomicBool::new(true));
    let mut seen = Vec::new();
    let result = source
        .store
        .backups
        .qualify_with(stage, &armed, |phase| seen.push(phase));
    assert!(matches!(result, Err(StoreError::Unavailable { .. })));
    assert!(seen.is_empty());
    evidence.assert_kept(&source);
}

/// Every open the qualifier makes through the coordinator's file system
/// (source, live and stage names, and the staged evidence), failed in
/// turn with a native cause that must survive.
#[test]
fn fired_open_faults_keep_their_native_cause() {
    const INCARNATION: &str = "00000000-0000-4000-8000-0000000000e2";
    let stage_and_qualify = |nth: u64| {
        let source = staging_source("qualified-open-fault");
        let fault = Arc::new(FaultFs::new(
            real_fs(),
            FsOp::Open,
            nth,
            FaultKind::Kind(io::ErrorKind::PermissionDenied),
        ));
        let backups = faulty_backups_seeded(
            &source.store,
            &source.paths,
            Arc::clone(&fault) as Arc<dyn Fs>,
            9,
        );
        let selection = source.select_with(&source.store);
        let stage = backups
            .stage_into(
                &selection,
                source.receipt.location(),
                source.metadata,
                INCARNATION,
                &AtomicBool::new(false),
                |_| {},
            )
            .expect("staging opens come before the fault");
        let staged = fault.calls(FsOp::Open);
        let preserved = stage.preserved_root().to_path_buf();
        let result = backups.qualify_with(stage, &Arc::new(AtomicBool::new(false)), |_| {});
        (result, staged, fault.calls(FsOp::Open), preserved, source)
    };

    // Calibrate: count staging's opens, then the qualifier's.
    let (clean, staged, total, _, _) = stage_and_qualify(u64::MAX);
    clean.expect("a clean qualification");
    assert!(
        total > staged,
        "the qualifier opens through the coordinator"
    );
    for nth in staged + 1..=total {
        let (result, _, _, preserved, source) = stage_and_qualify(nth);
        assert_eq!(
            result.unwrap_err().io_kind(),
            Some(io::ErrorKind::PermissionDenied),
            "open #{nth} of {total}"
        );
        assert!(!preserved.exists());
        source.assert_untouched();
    }
}

const QUALIFY_CUT_N: &str = "SB_QUALIFY_CUT_N";
const QUALIFY_CUT_ROOT: &str = "SB_QUALIFY_CUT_ROOT";
const QUALIFY_CUT_CHILD: &str = "backups::tests::stage_tests::qualified::qualify_cut_child";
const CUT_INCARNATION: &str = "00000000-0000-4000-8000-0000000000e3";

#[test]
#[ignore = "spawned by qualification_cuts_leave_live_source_and_stage_evidence"]
fn qualify_cut_child() {
    let nth = env::var(QUALIFY_CUT_N).unwrap().parse().unwrap();
    let report_root = PathBuf::from(env::var(QUALIFY_CUT_ROOT).unwrap());
    let source = staging_source("qualify-cut-child");
    let cuts = Arc::new(CutFs::new(real_fs(), nth));
    let backups = faulty_backups_seeded(
        &source.store,
        &source.paths,
        Arc::clone(&cuts) as Arc<dyn Fs>,
        11,
    );
    let selection = source.select_with(&source.store);
    let live_root = real_fs()
        .open_confined_dir(source.paths.data_root())
        .unwrap();
    let (_, _, live_id) = live_root
        .open_regular_identified(Path::new("state.sqlite"))
        .unwrap();
    let mut immutable = vec![
        source.receipt.location().join("manifest.json"),
        source.receipt.state_db().to_path_buf(),
    ];
    for (sha, _) in &source.payloads {
        immutable.push(source.receipt.location().join(format!(
            "objects/sha256/{}/{}/{}",
            &sha[..2],
            &sha[2..4],
            sha
        )));
    }
    let hashes: Vec<_> = immutable
        .into_iter()
        .map(|path| {
            let hash = sha256_hex(&std::fs::read(&path).unwrap());
            (path, hash)
        })
        .collect();
    let stage = backups
        .stage_into(
            &selection,
            source.receipt.location(),
            source.metadata,
            CUT_INCARNATION,
            &AtomicBool::new(false),
            |_| {},
        )
        .unwrap();
    let report = serde_json::json!({
        "scratch": source._scratch.path(), "live": source.paths.data_root(),
        "stage": stage.path(), "preserved": stage.preserved_root(),
        "incarnation": source.live_incarnation(), "live_id": format!("{live_id:?}"),
        "hashes": hashes, "staged": cuts.operations(),
    });
    std::fs::write(
        report_root.join("expected.json"),
        serde_json::to_vec(&report).unwrap(),
    )
    .unwrap();
    let counts = Mutex::new(Vec::new());
    backups
        .qualify_with(stage, &Arc::new(AtomicBool::new(false)), |phase| {
            counts.lock().unwrap().push(cuts.operations());
            std::fs::write(report_root.join("phase.txt"), format!("{phase:?}")).unwrap();
        })
        .unwrap();
    counts.lock().unwrap().push(cuts.operations());
    std::fs::write(
        report_root.join("counts.json"),
        serde_json::to_vec(&*counts.lock().unwrap()).unwrap(),
    )
    .unwrap();
}

#[test]
fn qualification_cuts_leave_live_source_and_stage_evidence() {
    let scratch = ScratchDir::new("qualify-process-cuts");
    let baseline = scratch.path().join("baseline");
    let calibration = run_crash_cut_points(
        QUALIFY_CUT_N,
        QUALIFY_CUT_ROOT,
        QUALIFY_CUT_CHILD,
        &[u64::MAX],
        &mut |_| {
            std::fs::create_dir(&baseline).unwrap();
            baseline.clone()
        },
        &mut |_| panic!("calibration must survive"),
    );
    assert_eq!(calibration.survived, [u64::MAX]);
    let report: serde_json::Value =
        serde_json::from_slice(&std::fs::read(baseline.join("expected.json")).unwrap()).unwrap();
    let staged = report["staged"].as_u64().unwrap();
    // One count per phase, then the count at handoff.
    let counts: Vec<u64> =
        serde_json::from_slice(&std::fs::read(baseline.join("counts.json")).unwrap()).unwrap();
    assert_eq!(counts.len(), PHASES.len() + 1);
    let flushed = counts[PHASES.len() - 2];
    let rechecked = counts[PHASES.len() - 1];
    assert!(counts[0] > staged, "the pre-checks do real I/O");
    assert!(rechecked > flushed, "the post-flush recheck does real I/O");
    // The first operation of the pre-checks, of the evidence pass, of the
    // quarantine, and the first, middle and last of the post-flush
    // recheck: the last one is the final I/O before the handoff.
    let mut points = vec![
        staged + 1,
        counts[0] + 1,
        counts[1] + 1,
        flushed + 1,
        flushed + (rechecked - flushed).div_ceil(2),
        rechecked,
    ];
    points.dedup();
    assert!(points.windows(2).all(|pair| pair[0] < pair[1]));
    let mut phases = Vec::new();
    let result = run_crash_cut_points(
        QUALIFY_CUT_N,
        QUALIFY_CUT_ROOT,
        QUALIFY_CUT_CHILD,
        &points,
        &mut |n| {
            let path = scratch.path().join(format!("cut-{n}"));
            std::fs::create_dir(&path).unwrap();
            path
        },
        &mut |root| {
            let report: serde_json::Value =
                serde_json::from_slice(&std::fs::read(root.join("expected.json")).unwrap())
                    .unwrap();
            phases.push(std::fs::read_to_string(root.join("phase.txt")).unwrap_or_default());
            for pair in report["hashes"].as_array().unwrap() {
                let path = pair[0].as_str().unwrap();
                assert_eq!(
                    sha256_hex(&std::fs::read(path).unwrap()),
                    pair[1].as_str().unwrap()
                );
            }
            let live = Path::new(report["live"].as_str().unwrap());
            let connection = Connection::open_with_flags(
                live.join("state.sqlite"),
                rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
            )
            .unwrap();
            let incarnation: String = connection
                .query_row(
                    "SELECT value FROM store_meta WHERE key='incarnation'",
                    [],
                    |r| r.get(0),
                )
                .unwrap();
            assert_eq!(incarnation, report["incarnation"].as_str().unwrap());
            connection.close().unwrap();
            let directory = real_fs().open_confined_dir(live).unwrap();
            let (_, _, id) = directory
                .open_regular_identified(Path::new("state.sqlite"))
                .unwrap();
            assert_eq!(format!("{id:?}"), report["live_id"].as_str().unwrap());
            assert!(Path::new(report["stage"].as_str().unwrap()).is_dir());
            assert!(!Path::new(report["preserved"].as_str().unwrap()).exists());
            drop(directory);
            let child_scratch = Path::new(report["scratch"].as_str().unwrap());
            assert!(
                child_scratch
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .contains("qualify-cut-child")
            );
            assert!(
                child_scratch.starts_with(std::fs::canonicalize(std::env::temp_dir()).unwrap())
            );
            std::fs::remove_dir_all(child_scratch).unwrap();
        },
    );
    assert_eq!(result.killed, points, "every cut point killed the child");
    assert!(
        phases.iter().any(|phase| phase.contains("Flushed")),
        "a cut landed after the quarantine's flushes: {phases:?}"
    );
}
