//! M1.8h2: a generation clones its objects where the filesystem can
//! (spec/03 step 4, APFS `clonefile`) and copies them where it can't,
//! and a completion stops at the next object when cancelled, leaving
//! the generation for the next run's recovery.

use super::*;
use crate::BackupReceipt;

/// `count` distinct referenced objects.
fn objects(store: &Store, count: usize) -> Vec<String> {
    (0..count)
        .map(|n| {
            let (id, sha) = object(
                store,
                format!("Subject: {n}\r\n\r\nbody {n}\r\n").as_bytes(),
            );
            reference(store, &id, "published");
            sha
        })
        .collect()
}

/// A generation through `fs` (wrapped around the real filesystem),
/// verified afterwards as any recorded generation is.
fn back_up_through(store: &Store, paths: &StorePaths, fs: &Arc<FaultFs>) -> BackupReceipt {
    let backups = faulty_backups(store, paths, fs.clone());
    let snapshot = backups.snapshot(BackupDestination::DataRoot).unwrap();
    let receipt = backups.complete(snapshot, &AtomicBool::new(false)).unwrap();
    let inspection = store
        .verify_recorded_backup(receipt.generation_id(), &AtomicBool::new(false))
        .unwrap();
    assert!(
        !matches!(inspection, crate::BackupImageInspection::Cancelled),
        "{inspection:?}"
    );
    receipt
}

/// The bytes the generation holds for `sha`.
fn generation_object(receipt: &BackupReceipt, sha: &str) -> Vec<u8> {
    std::fs::read(
        receipt
            .location()
            .join("objects/sha256")
            .join(&sha[..2])
            .join(&sha[2..4])
            .join(sha),
    )
    .unwrap()
}

/// Files a generation creates besides its object copies: the
/// ownership marker, the snapshot and the manifest.
const OTHER_CREATES: u64 = 3;

/// A fault plan that never fires: the wrapper only counts.
fn counting() -> Arc<FaultFs> {
    Arc::new(FaultFs::new(
        real_fs(),
        FsOp::CloneFile,
        u64::MAX,
        FaultKind::Eio,
    ))
}

#[test]
fn objects_are_cloned_where_the_filesystem_can_and_copied_where_not() {
    let (_scratch, paths, store) = open("backup-clone-real");
    let shas = objects(&store, 3);
    let fs = counting();
    let receipt = back_up_through(&store, &paths, &fs);

    if cfg!(target_os = "macos") {
        // APFS clones every object: none is created as a copy.
        assert_eq!(fs.calls(FsOp::CloneFile), 3);
        assert_eq!(fs.calls(FsOp::Create), OTHER_CREATES);
    } else {
        // The first refusal means none will clone: one attempt, three
        // copies.
        assert_eq!(fs.calls(FsOp::CloneFile), 1);
        assert_eq!(fs.calls(FsOp::Create), OTHER_CREATES + 3);
    }
    for (n, sha) in shas.iter().enumerate() {
        assert_eq!(
            generation_object(&receipt, sha),
            format!("Subject: {n}\r\n\r\nbody {n}\r\n").as_bytes()
        );
    }
}

#[test]
fn a_filesystem_that_cannot_clone_is_asked_once_and_every_object_copied() {
    let (_scratch, paths, store) = open("backup-clone-unsupported");
    let shas = objects(&store, 3);
    let fs = Arc::new(FaultFs::new(
        real_fs(),
        FsOp::CloneFile,
        1,
        FaultKind::Kind(std::io::ErrorKind::Unsupported),
    ));
    let receipt = back_up_through(&store, &paths, &fs);

    assert_eq!(fs.calls(FsOp::CloneFile), 1);
    assert_eq!(fs.calls(FsOp::Create), OTHER_CREATES + 3, "three copies");
    for sha in &shas {
        assert!(!generation_object(&receipt, sha).is_empty());
    }
}

#[test]
fn a_failed_clone_copies_that_object_and_keeps_cloning() {
    let (_scratch, paths, store) = open("backup-clone-failed");
    let shas = objects(&store, 2);
    let fs = Arc::new(FaultFs::new(real_fs(), FsOp::CloneFile, 1, FaultKind::Eio));
    let receipt = back_up_through(&store, &paths, &fs);

    // The second object is still offered to the clone (where the real
    // filesystem can't clone, that is its refusal).
    assert_eq!(fs.calls(FsOp::CloneFile), 2);
    for sha in &shas {
        assert!(!generation_object(&receipt, sha).is_empty());
    }
}

#[test]
fn a_damaged_source_never_completes_and_is_quarantined() {
    let (_scratch, paths, store) = open("backup-clone-damaged");
    let shas = objects(&store, 1);
    let id = ObjectId::from_hex(&shas[0], 0).unwrap();
    let source = store.objects().object_path(&id);
    let mut bytes = std::fs::read(&source).unwrap();
    bytes[0] ^= 0x20;
    std::fs::write(&source, &bytes).unwrap();

    let fs = counting();
    let backups = faulty_backups(&store, &paths, fs.clone());
    let snapshot = backups.snapshot(BackupDestination::DataRoot).unwrap();
    let generation = snapshot.generation_id().to_owned();
    let failed = backups.complete(snapshot, &AtomicBool::new(false));
    assert!(
        matches!(failed, Err(StoreError::Corrupt { .. })),
        "{failed:?}"
    );
    assert_eq!(
        generation_row(&store, &generation).unwrap().0,
        "in_progress"
    );
    // Whichever path read it, the damaged source left the store for
    // `recovery/`.
    assert!(!source.exists(), "the damaged object was quarantined");
}

#[test]
fn a_cancelled_completion_stops_and_the_next_backup_recovers_it() {
    let (_scratch, _paths, store) = open("backup-cancelled");
    objects(&store, 2);
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let generation = snapshot.generation_id().to_owned();
    let location = snapshot.location().to_path_buf();

    let cancelled = store.complete_backup_with(snapshot, &AtomicBool::new(true));
    match cancelled {
        Err(StoreError::Unavailable { reason, .. }) => assert!(reason.contains("cancelled")),
        other => panic!("expected a cancelled completion, got {other:?}"),
    }
    // Nothing was copied; the generation waits, still gating GC.
    assert_eq!(
        generation_row(&store, &generation).unwrap().0,
        "in_progress"
    );
    assert_eq!(
        std::fs::read_dir(location.join("objects/sha256"))
            .unwrap()
            .count(),
        0
    );
    let gc = store.collect_garbage(GC_GRACE_PERIOD).unwrap();
    assert_eq!(gc.blocked, Some(GcGate::BackupInProgress), "{gc:?}");
    assert!(store.backup_due().unwrap());

    // The next generation marks it failed and removes its directory.
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    store.complete_backup(snapshot).unwrap();
    assert_eq!(generation_row(&store, &generation).unwrap().0, "failed");
    assert!(!location.exists());
    assert!(!store.backup_due().unwrap());
    let gc = store.collect_garbage(GC_GRACE_PERIOD).unwrap();
    assert_eq!(gc.blocked, None, "{gc:?}");
}

/// The real filesystem, except that each clone leaves its target
/// behind and then fails, as a clone whose flush failed would.
struct LeftoverCloneFs(Arc<dyn Fs>);

impl Fs for LeftoverCloneFs {
    fn clone_file(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        let mut input = self.0.open(from)?;
        let mut output = self.0.create(to)?;
        std::io::copy(&mut input, &mut output)?;
        Err(std::io::Error::other(
            "injected flush failure after the clone",
        ))
    }
    fn open_confined_dir(
        &self,
        path: &Path,
    ) -> std::io::Result<Box<dyn sb_types::fs::ConfinedDir>> {
        self.0.open_confined_dir(path)
    }
    fn create(&self, path: &Path) -> std::io::Result<Box<dyn sb_types::fs::WriteFile>> {
        self.0.create(path)
    }
    fn open(&self, path: &Path) -> std::io::Result<Box<dyn sb_types::fs::ReadFile>> {
        self.0.open(path)
    }
    fn open_no_follow(&self, path: &Path) -> std::io::Result<Box<dyn sb_types::fs::ReadFile>> {
        self.0.open_no_follow(path)
    }
    fn open_regular(
        &self,
        path: &Path,
    ) -> std::io::Result<(Box<dyn sb_types::fs::ReadFile>, sb_types::fs::Metadata)> {
        self.0.open_regular(path)
    }
    fn metadata(&self, path: &Path) -> std::io::Result<Option<sb_types::fs::Metadata>> {
        self.0.metadata(path)
    }
    fn read_dir(&self, path: &Path) -> std::io::Result<Vec<PathBuf>> {
        self.0.read_dir(path)
    }
    fn create_dir(&self, path: &Path) -> std::io::Result<()> {
        self.0.create_dir(path)
    }
    fn create_private_dir(&self, path: &Path) -> std::io::Result<()> {
        self.0.create_private_dir(path)
    }
    fn restrict_dir(&self, path: &Path) -> std::io::Result<()> {
        self.0.restrict_dir(path)
    }
    fn remove_dir_all(&self, path: &Path) -> std::io::Result<()> {
        self.0.remove_dir_all(path)
    }
    fn quarantine(&self, path: &Path) -> std::io::Result<()> {
        self.0.quarantine(path)
    }
    fn sync_dir(&self, path: &Path) -> std::io::Result<()> {
        self.0.sync_dir(path)
    }
    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        self.0.rename(from, to)
    }
    fn remove(&self, path: &Path) -> std::io::Result<()> {
        self.0.remove(path)
    }
    fn canonicalize(&self, path: &Path) -> std::io::Result<PathBuf> {
        self.0.canonicalize(path)
    }
    fn lock_exclusive(
        &self,
        path: &Path,
    ) -> Result<Box<dyn sb_types::fs::Lease>, sb_types::fs::LockError> {
        self.0.lock_exclusive(path)
    }
    fn location(&self, path: &Path) -> std::io::Result<sb_types::fs::Location> {
        self.0.location(path)
    }
}

/// A clone that fails after creating its target leaves the name to the
/// copy: the generation completes with the copied object.
#[test]
fn a_clone_that_fails_after_creating_its_target_is_copied_over() {
    let (_scratch, paths, store) = open("backup-clone-leftover");
    let shas = objects(&store, 2);
    let backups = faulty_backups(&store, &paths, Arc::new(LeftoverCloneFs(real_fs())));
    let snapshot = backups.snapshot(BackupDestination::DataRoot).unwrap();
    let receipt = backups.complete(snapshot, &AtomicBool::new(false)).unwrap();

    store
        .verify_recorded_backup(receipt.generation_id(), &AtomicBool::new(false))
        .unwrap();
    for (n, sha) in shas.iter().enumerate() {
        assert_eq!(
            generation_object(&receipt, sha),
            format!("Subject: {n}\r\n\r\nbody {n}\r\n").as_bytes()
        );
    }
}
