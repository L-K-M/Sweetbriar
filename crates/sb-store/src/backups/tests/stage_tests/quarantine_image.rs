//! Native execution, failure and SIGKILL proof for the private image kernel.
use super::super::super::{
    quarantine,
    stage_quarantine::{self, ImagePhase, QuarantinedImage},
};
use super::*;
use sb_types::fs::{FileIdentity, ReadWriteFile};
use std::io;
use std::sync::atomic::AtomicUsize;

const PHASES: [ImagePhase; 7] = [
    ImagePhase::BytesVerified,
    ImagePhase::ReadOnlyInspected,
    ImagePhase::WriterInspected,
    ImagePhase::Quarantined,
    ImagePhase::Checkpointed,
    ImagePhase::Closed,
    ImagePhase::Flushed,
];

fn source_uuid(source: &StagingSource) -> String {
    reading(&source.store, |tx| {
        tx.query_row(
            "SELECT value FROM store_meta WHERE key='store_uuid'",
            [],
            |row| row.get(0),
        )
        .map_err(bug)
    })
}

fn candidate(source: &StagingSource) -> crate::PreparedRestoreStage {
    prepare(
        &source.store,
        source.select_with(&source.store),
        source.receipt.location(),
        source.metadata,
    )
}

fn execute(
    stage: &crate::PreparedRestoreStage,
    uuid: &str,
    cancel: &Arc<AtomicBool>,
    progress: impl FnMut(ImagePhase),
) -> Result<QuarantinedImage, StoreError> {
    let input = quarantine::Quarantine {
        new_incarnation: stage.incarnation(),
        generation: stage.generation_id(),
        source_location: stage.source_location().to_str().unwrap(),
        current_root: &stage.data_root,
        preserved_root: stage.preserved_root(),
        manifest_sha256: stage.manifest_sha256(),
        object_count: stage.metadata.object_count,
        total_bytes: stage.metadata.total_bytes,
        completed_at: stage.metadata.completed_at,
        cancel,
        max_work_calls: quarantine::work_allowance(stage.files.database_bytes),
    };
    let leases = Arc::strong_count(&stage.files.database);
    let result =
        stage_quarantine::run(&stage.files, &test_seams(real_fs()), &input, uuid, progress);
    assert_eq!(
        Arc::strong_count(&stage.files.database),
        leases,
        "all SQLite callbacks must release the retained main file"
    );
    result
}

fn readonly(stage: &crate::PreparedRestoreStage) -> Connection {
    Connection::open_with_flags(
        stage.path().join("state.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
    )
    .unwrap()
}

#[test]
fn a_fresh_target_preserves_large_rows_and_quarantines_real_backup_state() {
    let mut source = staging_source("quarantine-real-state");
    sql(&source.store, "UPDATE draft_revisions SET body_source=hex(zeroblob(600000)); UPDATE send_attempts SET state='queued'; INSERT INTO import_jobs VALUES ('q-job','running','{}','{}',1,0,0); INSERT INTO intents (intent_id,account_id,config_generation,op,target_json,args_json,preconditions_json,state,idempotency_key,origin,created_at,updated_at) SELECT 'q-intent',account_id,1,'move','{}','{}','{}','pending','q-key','user',0,0 FROM accounts WHERE provider='generic_imap'; INSERT INTO verifier_scratch VALUES ('q-owner','q-token','old');".into());
    source.receipt = source
        .store
        .complete_backup(
            source
                .store
                .backup_snapshot(BackupDestination::DataRoot)
                .unwrap(),
        )
        .unwrap();
    source.metadata = restore_metadata(&source.store, source.receipt.generation_id());
    let (_target_scratch, target_paths) = scratch_paths("quarantine-fresh-target");
    let target = crate::open(
        &target_paths,
        &crate::Seams::new(
            real_fs(),
            Arc::new(FixedClock::new(Timestamp::from_millis(FIXED_NOW_MS))),
            Arc::new(SeededRandom::new(43)),
        ),
    )
    .unwrap();
    let uuid = source_uuid(&source);
    let target_uuid: String = reading(&target, |tx| {
        tx.query_row(
            "SELECT value FROM store_meta WHERE key='store_uuid'",
            [],
            |row| row.get(0),
        )
        .map_err(bug)
    });
    assert_ne!(target_uuid, uuid, "fresh target has its own store identity");
    let stage = prepare(
        &target,
        source.select_with(&target),
        source.receipt.location(),
        source.metadata,
    );
    // Selection verification may update its own live scratch bookkeeping.
    // The image executor starts after that preparation has completed.
    let live_before = std::fs::read(target_paths.state_db()).unwrap();
    let mut phases = Vec::new();
    let image = execute(&stage, &uuid, &Arc::new(AtomicBool::new(false)), |phase| {
        phases.push(phase)
    })
    .unwrap();
    assert_eq!(phases, PHASES);
    let connection = readonly(&stage);
    for (sql, expected) in [
        (
            "SELECT state FROM import_jobs WHERE job_id='q-job'",
            "failed",
        ),
        (
            "SELECT state FROM intents WHERE intent_id='q-intent'",
            "quarantined",
        ),
        ("SELECT state FROM send_attempts LIMIT 1", "outcome_unknown"),
    ] {
        assert_eq!(
            connection
                .query_row(sql, [], |row| row.get::<_, String>(0))
                .unwrap(),
            expected
        );
    }
    assert_eq!(
        connection
            .query_row(
                "SELECT octet_length(body_source) FROM draft_revisions LIMIT 1",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1_200_000
    );
    // Existence, not vacuity: the fixture account must still be there,
    // paused — a delete would satisfy a bare `state<>'paused'` count.
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM accounts WHERE provider='generic_imap' AND state='paused'",
                [],
                |row| row.get::<_, i64>(0)
            )
            .unwrap(),
        1
    );
    for sql in [
        "SELECT count(*) FROM accounts WHERE state<>'paused'",
        "SELECT count(*) FROM verifier_scratch",
        "SELECT count(*) FROM object_pins WHERE pin_kind IN ('reservation','backup_in_progress')",
        "SELECT count(*) FROM backup_cleanup_pending",
    ] {
        assert_eq!(
            connection
                .query_row(sql, [], |row| row.get::<_, i64>(0))
                .unwrap(),
            0
        );
    }
    let row: (String, String, String, i64, i64, i64) = connection.query_row("SELECT state,location,manifest_sha256,object_count,total_bytes,completed_at FROM backup_generations WHERE generation_id=?1", [stage.generation_id()], |r| Ok((r.get(0)?,r.get(1)?,r.get(2)?,r.get(3)?,r.get(4)?,r.get(5)?))).unwrap();
    assert_eq!(
        row,
        (
            "complete".into(),
            stage.source_location().to_str().unwrap().into(),
            stage.manifest_sha256().into(),
            stage.metadata.object_count,
            stage.metadata.total_bytes,
            stage.metadata.completed_at
        )
    );
    assert_eq!(
        connection
            .query_row(
                "SELECT count(*) FROM backup_ownership WHERE generation_id=?1",
                [stage.generation_id()],
                |r| r.get::<_, i64>(0)
            )
            .unwrap(),
        0
    );
    connection.close().unwrap();
    let bytes = std::fs::read(stage.path().join("state.sqlite")).unwrap();
    assert_eq!(image.bytes, bytes.len() as u64);
    assert_eq!(image.sha256, <[u8; 32]>::from(Sha256::digest(&bytes)));
    assert_eq!(image.database_identity, stage.files.database_identity);
    assert_eq!(image.root_identity, stage.files.root_identity);
    assert_eq!(std::fs::read(target_paths.state_db()).unwrap(), live_before);
    source.assert_untouched();
}

#[test]
fn every_phase_observes_cancellation_without_an_image_handoff() {
    for point in PHASES {
        let source = staging_source("quarantine-phase-cancel");
        let before = source.live_incarnation();
        let stage = candidate(&source);
        let cancel = Arc::new(AtomicBool::new(false));
        let mut seen = Vec::new();
        let result = execute(&stage, &source_uuid(&source), &cancel, |phase| {
            seen.push(phase);
            if phase == point {
                cancel.store(true, Ordering::Release);
            }
        });
        assert!(
            matches!(result, Err(StoreError::Unavailable { .. })),
            "cancel at {point:?}: {result:?}"
        );
        assert_eq!(seen.last(), Some(&point));
        assert!(stage.path().is_dir());
        assert!(!stage.preserved_root().exists());
        assert_eq!(source.live_incarnation(), before);
        source.assert_untouched();
    }
}

#[test]
fn original_bytes_length_and_source_uuid_are_required() {
    for change in 0..4 {
        let source = staging_source("quarantine-original-check");
        let stage = candidate(&source);
        let mut uuid = source_uuid(&source);
        match change {
            0 => {
                stage.files.database.write_at(b"!", 0).unwrap();
            }
            1 => stage
                .files
                .database
                .set_len(stage.files.database_bytes - 1)
                .unwrap(),
            2 => stage
                .files
                .database
                .set_len(stage.files.database_bytes + 1)
                .unwrap(),
            _ => uuid = "00000000-0000-4000-8000-000000000099".into(),
        }
        let mut seen = Vec::new();
        assert!(
            execute(&stage, &uuid, &Arc::new(AtomicBool::new(false)), |p| seen
                .push(p))
            .is_err()
        );
        assert!(!seen.contains(&ImagePhase::WriterInspected));
        source.assert_untouched();
    }
}

#[test]
fn a_fresh_writer_checks_its_own_catalog_before_any_quarantine() {
    let source = staging_source("quarantine-fresh-catalog");
    let stage = candidate(&source);
    let mut seen = Vec::new();
    let result = execute(
        &stage,
        &source_uuid(&source),
        &Arc::new(AtomicBool::new(false)),
        |phase| {
            seen.push(phase);
            if phase == ImagePhase::ReadOnlyInspected {
                // An adversarial actor changes the image between independent opens.
                let connection = Connection::open_with_flags(
                    stage.path().join("state.sqlite"),
                    rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
                )
                .unwrap();
                connection.execute_batch("PRAGMA journal_mode=DELETE; CREATE TRIGGER reject_quarantine BEFORE UPDATE ON accounts BEGIN SELECT RAISE(ABORT,'must never execute'); END; PRAGMA journal_mode=WAL; PRAGMA wal_checkpoint(TRUNCATE);").unwrap();
                connection.close().unwrap();
            }
        },
    );
    assert!(
        matches!(result, Err(StoreError::Corrupt { .. })),
        "{result:?}"
    );
    assert_eq!(seen.last(), Some(&ImagePhase::ReadOnlyInspected));
    source.assert_untouched();
}

#[test]
fn checkpoint_close_and_flush_mutations_cannot_change_the_sealed_image() {
    for point in [
        ImagePhase::WriterInspected,
        ImagePhase::Checkpointed,
        ImagePhase::Closed,
        ImagePhase::Flushed,
    ] {
        let source = staging_source("quarantine-sealed-mutation");
        let stage = candidate(&source);
        let mut mutated = false;
        let result = execute(
            &stage,
            &source_uuid(&source),
            &Arc::new(AtomicBool::new(false)),
            |phase| {
                if phase == point {
                    // application_id is structurally valid SQLite metadata, outside
                    // the catalog, so the post-checkpoint digest must also protect it.
                    let mut byte = [0];
                    stage.files.database.read_at(&mut byte, 68).unwrap();
                    byte[0] ^= 1;
                    stage.files.database.write_at(&byte, 68).unwrap();
                    mutated = true;
                }
            },
        );
        assert!(mutated);
        assert!(
            matches!(result, Err(StoreError::Corrupt { .. })),
            "{point:?}: {result:?}"
        );
        source.assert_untouched();
    }
}

#[test]
fn fired_native_sidecar_and_directory_faults_keep_their_cause() {
    for operation in [FsOp::Write, FsOp::Truncate, FsOp::SyncFile, FsOp::SyncDir] {
        let source = staging_source("quarantine-sidecar-fault");
        let mut stage = candidate(&source);
        let fault = Arc::new(FaultFs::new(
            real_fs(),
            operation,
            1,
            FaultKind::Kind(io::ErrorKind::PermissionDenied),
        ));
        stage.files.root = fault.open_confined_dir(stage.path()).unwrap();
        let result = execute(
            &stage,
            &source_uuid(&source),
            &Arc::new(AtomicBool::new(false)),
            |_| {},
        );
        assert!(fault.calls(operation) >= 1, "{operation:?} must fire");
        assert_eq!(
            result.unwrap_err().io_kind(),
            Some(io::ErrorKind::PermissionDenied),
            "{operation:?}"
        );
        source.assert_untouched();
    }
}

struct ArmedSync {
    inner: Arc<dyn ReadWriteFile>,
    armed: Arc<AtomicBool>,
    fired: Arc<AtomicUsize>,
}
impl ReadWriteFile for ArmedSync {
    fn read_at(&self, b: &mut [u8], p: u64) -> io::Result<usize> {
        self.inner.read_at(b, p)
    }
    fn write_at(&self, b: &[u8], p: u64) -> io::Result<usize> {
        self.inner.write_at(b, p)
    }
    fn set_len(&self, n: u64) -> io::Result<()> {
        self.inner.set_len(n)
    }
    fn len(&self) -> io::Result<u64> {
        self.inner.len()
    }
    fn identity(&self) -> io::Result<FileIdentity> {
        self.inner.identity()
    }
    fn sync_all(&self) -> io::Result<()> {
        if self.armed.load(Ordering::Acquire) {
            self.fired.fetch_add(1, Ordering::SeqCst);
            Err(io::ErrorKind::PermissionDenied.into())
        } else {
            self.inner.sync_all()
        }
    }
}

#[test]
fn real_close_and_final_database_flush_failures_prevent_handoff() {
    for point in [ImagePhase::Checkpointed, ImagePhase::Closed] {
        let source = staging_source("quarantine-final-sync-fault");
        let mut stage = candidate(&source);
        let armed = Arc::new(AtomicBool::new(false));
        let fired = Arc::new(AtomicUsize::new(0));
        stage.files.database = Arc::new(ArmedSync {
            inner: stage.files.database.clone(),
            armed: armed.clone(),
            fired: fired.clone(),
        });
        let mut seen = Vec::new();
        let result = execute(
            &stage,
            &source_uuid(&source),
            &Arc::new(AtomicBool::new(false)),
            |phase| {
                seen.push(phase);
                if phase == point {
                    armed.store(true, Ordering::Release);
                }
            },
        );
        assert!(
            fired.load(Ordering::SeqCst) > 0,
            "actual retained sync must fire"
        );
        assert_eq!(
            result.unwrap_err().io_kind(),
            Some(io::ErrorKind::PermissionDenied)
        );
        assert_eq!(
            seen.last(),
            Some(&point),
            "the failed boundary cannot advance"
        );
        source.assert_untouched();
    }
}

const IMAGE_CUT_N: &str = "SB_QUARANTINE_IMAGE_CUT_N";
const IMAGE_CUT_ROOT: &str = "SB_QUARANTINE_IMAGE_CUT_ROOT";
const IMAGE_CUT_CHILD: &str = "backups::tests::stage_tests::quarantine_image::image_cut_child";

#[test]
#[ignore = "spawned by image_process_cuts_preserve_live_source_and_unqualified_evidence"]
fn image_cut_child() {
    let nth = env::var(IMAGE_CUT_N).unwrap().parse().unwrap();
    let report_root = PathBuf::from(env::var(IMAGE_CUT_ROOT).unwrap());
    let source = staging_source("quarantine-image-cut-child");
    let mut stage = candidate(&source);
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
    let report = serde_json::json!({
        "scratch": source._scratch.path(), "live": source.paths.data_root(),
        "stage": stage.path(), "preserved": stage.preserved_root(),
        "incarnation": source.live_incarnation(), "live_id": format!("{live_id:?}"),
        "hashes": hashes,
    });
    std::fs::write(
        report_root.join("expected.json"),
        serde_json::to_vec(&report).unwrap(),
    )
    .unwrap();
    let cuts = Arc::new(CutFs::new(real_fs(), nth));
    stage.files.root = cuts.open_confined_dir(stage.path()).unwrap();
    let mut counts = Vec::new();
    execute(
        &stage,
        &source_uuid(&source),
        &Arc::new(AtomicBool::new(false)),
        |phase| {
            counts.push(cuts.operations());
            std::fs::write(report_root.join("phase.txt"), format!("{phase:?}")).unwrap();
        },
    )
    .unwrap();
    std::fs::write(
        report_root.join("counts.json"),
        serde_json::to_vec(&counts).unwrap(),
    )
    .unwrap();
}

#[test]
fn image_process_cuts_preserve_live_source_and_unqualified_evidence() {
    let scratch = ScratchDir::new("quarantine-image-process-cuts");
    let baseline = scratch.path().join("baseline");
    let calibration = run_crash_cut_points(
        IMAGE_CUT_N,
        IMAGE_CUT_ROOT,
        IMAGE_CUT_CHILD,
        &[u64::MAX],
        &mut |_| {
            std::fs::create_dir(&baseline).unwrap();
            baseline.clone()
        },
        &mut |_| panic!("calibration must survive"),
    );
    assert_eq!(calibration.survived, [u64::MAX]);
    let counts: Vec<u64> =
        serde_json::from_slice(&std::fs::read(baseline.join("counts.json")).unwrap()).unwrap();
    assert_eq!(counts.len(), PHASES.len());
    assert!(
        counts[3] > counts[2],
        "real sidecar I/O must occur inside quarantine"
    );
    // First actual confined I/O after each boundary: inside the transaction,
    // during checkpoint, during close, after close and after final flush.
    let points: Vec<_> = counts[2..].iter().map(|count| count + 1).collect();
    assert!(points.windows(2).all(|pair| pair[0] < pair[1]));
    let mut phases = Vec::new();
    let result = run_crash_cut_points(
        IMAGE_CUT_N,
        IMAGE_CUT_ROOT,
        IMAGE_CUT_CHILD,
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
            let phase = std::fs::read_to_string(root.join("phase.txt")).unwrap();
            phases.push(phase);
            for pair in report["hashes"].as_array().unwrap() {
                let path = pair[0].as_str().unwrap();
                assert_eq!(
                    sha256_hex(&std::fs::read(path).unwrap()),
                    pair[1].as_str().unwrap()
                );
            }
            let live = Path::new(report["live"].as_str().unwrap());
            assert!(live.is_dir());
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
            // This directory was created by our killed fixture child. Its
            // ownership is recorded before the first possible kernel cut.
            let child_scratch = Path::new(report["scratch"].as_str().unwrap());
            assert!(
                child_scratch
                    .file_name()
                    .unwrap()
                    .to_str()
                    .unwrap()
                    .contains("quarantine-image-cut-child")
            );
            assert!(
                child_scratch.starts_with(std::fs::canonicalize(std::env::temp_dir()).unwrap())
            );
            std::fs::remove_dir_all(child_scratch).unwrap();
        },
    );
    assert_eq!(
        result.killed, points,
        "every selected point must SIGKILL, not survive"
    );
    assert!(result.survived.is_empty());
    assert_eq!(
        phases,
        [
            "WriterInspected",
            "Quarantined",
            "Checkpointed",
            "Closed",
            "Flushed"
        ]
    );
}

struct CountReads {
    inner: Arc<dyn ReadWriteFile>,
    armed: Arc<AtomicBool>,
    bytes: Arc<std::sync::atomic::AtomicU64>,
}
impl ReadWriteFile for CountReads {
    fn read_at(&self, b: &mut [u8], p: u64) -> io::Result<usize> {
        let read = self.inner.read_at(b, p)?;
        if self.armed.load(Ordering::Acquire) {
            self.bytes.fetch_add(read as u64, Ordering::SeqCst);
        }
        Ok(read)
    }
    fn write_at(&self, b: &[u8], p: u64) -> io::Result<usize> {
        self.inner.write_at(b, p)
    }
    fn set_len(&self, n: u64) -> io::Result<()> {
        self.inner.set_len(n)
    }
    fn len(&self) -> io::Result<u64> {
        self.inner.len()
    }
    fn identity(&self) -> io::Result<FileIdentity> {
        self.inner.identity()
    }
    fn sync_all(&self) -> io::Result<()> {
        self.inner.sync_all()
    }
}

#[test]
fn the_first_schema_load_has_a_total_physical_input_bound() {
    let source = staging_source("quarantine-eager-schema");
    let mut stage = candidate(&source);
    let connection = Connection::open_with_flags(
        stage.path().join("state.sqlite"),
        rusqlite::OpenFlags::SQLITE_OPEN_READ_WRITE,
    )
    .unwrap();
    connection
        .execute_batch("PRAGMA journal_mode=DELETE;")
        .unwrap();
    let literal = "x".repeat(8000);
    for table in 0..300 {
        connection
            .execute_batch(&format!(
                "CREATE TABLE extra_{table}(v TEXT CHECK(v<>'{literal}')) STRICT;"
            ))
            .unwrap();
    }
    connection
        .execute_batch("PRAGMA journal_mode=WAL; PRAGMA wal_checkpoint(TRUNCATE);")
        .unwrap();
    connection.close().unwrap();
    // Model authenticated hostile bytes at this private image boundary. The
    // public qualified handoff still must authenticate its separate manifest.
    let bytes = std::fs::read(stage.path().join("state.sqlite")).unwrap();
    stage.files.database_bytes = bytes.len() as u64;
    stage.files.database_sha256 = Sha256::digest(&bytes).into();
    let armed = Arc::new(AtomicBool::new(false));
    let reads = Arc::new(std::sync::atomic::AtomicU64::new(0));
    stage.files.database = Arc::new(CountReads {
        inner: stage.files.database.clone(),
        armed: armed.clone(),
        bytes: reads.clone(),
    });
    let result = execute(
        &stage,
        &source_uuid(&source),
        &Arc::new(AtomicBool::new(false)),
        |phase| {
            if phase == ImagePhase::BytesVerified {
                armed.store(true, Ordering::Release);
            }
        },
    );
    // The physical input budget itself must reject the image, not some
    // incidental failure a short-circuit could also produce.
    assert!(
        matches!(result, Err(StoreError::Corrupt { .. })),
        "{result:?}"
    );
    let actual = reads.load(Ordering::SeqCst);
    assert!(actual > 0, "the actual SQLite schema read path must run");
    assert!(
        actual <= 2 * 1024 * 1024,
        "eager schema load consumed {actual} bytes"
    );
    source.assert_untouched();
}
