use std::io::Read;
use std::sync::atomic::{AtomicU8, Ordering};
use std::{env, fs};

use sb_test_support::{
    CutFs, FaultFs, FaultKind, FsOp, ScratchDir, SeededRandom, SimFs, real_fs, run_crash_cuts,
};

use super::*;

#[derive(Default)]
struct Tokens(AtomicU8);
impl Random for Tokens {
    fn fill(&self, dest: &mut [u8]) -> io::Result<()> {
        dest.fill(self.0.fetch_add(1, Ordering::SeqCst));
        Ok(())
    }
}

fn cache(fs: Arc<dyn Fs>) -> RenditionCache {
    RenditionCache::open_on(Path::new("/cache"), fs, Arc::new(Tokens::default())).unwrap()
}

fn sim() -> Arc<SimFs> {
    let fs = Arc::new(SimFs::new());
    fs.create_dir_all(Path::new("/cache")).unwrap();
    fs
}

fn contents(fs: &dyn Fs, path: &Path) -> Vec<u8> {
    let mut bytes = Vec::new();
    fs.open(path).unwrap().read_to_end(&mut bytes).unwrap();
    bytes
}

/// Half a lease: the size the budget tests reserve in.
const FILE_BYTES: u64 = LEASE_BYTES / 2;
#[test]
fn publication_is_durable_generated_and_exclusive() {
    let fs = sim();
    let cache = cache(fs.clone());
    let mut files = cache.reserve(&[7, 0]).unwrap();
    let path = files
        .publish(0, b"../evil", RenditionProtection::Internal)
        .unwrap();
    assert_eq!(path.parent(), Some(files.directory.as_path()));
    assert_eq!(path.file_name().unwrap(), "file-0");
    assert_eq!(files.token().len(), 64);
    assert!(files.token().bytes().all(|b| b.is_ascii_hexdigit()));
    assert!(
        files
            .publish(0, b"../evil", RenditionProtection::Internal)
            .is_err()
    );
    assert!(
        files
            .publish(usize::MAX, b"", RenditionProtection::Internal)
            .is_err()
    );
    assert!(
        files
            .publish(1, b"wrong size", RenditionProtection::Internal)
            .is_err()
    );
    let empty = files
        .publish(1, b"", RenditionProtection::Internal)
        .unwrap();
    fs.power_cut();
    assert_eq!(contents(fs.as_ref(), &path), b"../evil");
    assert_eq!(contents(fs.as_ref(), &empty), b"");
    assert_eq!(fs.read_dir(&files.directory).unwrap().len(), 2);
    files.remove().unwrap();
    files.remove().unwrap();
    fs.power_cut();
    assert!(fs.metadata(&files.directory).unwrap().is_none());
}

#[test]
fn budgets_are_reserved_before_writes_and_never_evict() {
    let fs = sim();
    let fault = Arc::new(FaultFs::new(fs, FsOp::Write, 1, FaultKind::Eio));
    let cache = cache(fault.clone());
    for sizes in [vec![LEASE_BYTES + 1], vec![FILE_BYTES, FILE_BYTES, 1]] {
        assert!(matches!(
            cache.reserve(&sizes),
            Err(RenditionError::Quota(_))
        ));
    }
    assert_eq!(fault.calls(FsOp::CreatePrivateDir), 1);
    let mut leases: Vec<_> = (0..5)
        .map(|_| cache.reserve(&[FILE_BYTES, FILE_BYTES]).unwrap())
        .collect();
    leases.push(cache.reserve(&[12_000_000]).unwrap());
    assert!(matches!(cache.reserve(&[1]), Err(RenditionError::Quota(_))));
    assert_eq!(fault.calls(FsOp::Write), 0);
    assert_eq!(cache.0.reservations.lock().unwrap().bytes, LIVE_BYTES);
    leases[0].remove().unwrap();
    assert!(cache.reserve(&[FILE_BYTES]).is_ok());
    drop(leases);
    let leases: Vec<_> = (0..LIVE_LEASES)
        .map(|_| cache.reserve(&[]).unwrap())
        .collect();
    assert!(matches!(cache.reserve(&[]), Err(RenditionError::Quota(_))));
    assert_eq!(leases.len(), LIVE_LEASES);
}

#[test]
fn quota_refusal_keeps_published_bytes_and_other_leases_independent() {
    let fs = sim();
    let cache = cache(fs.clone());
    let mut first = cache.reserve(&[1]).unwrap();
    let path = first
        .publish(0, b"x", RenditionProtection::Internal)
        .unwrap();
    let mut second = cache.reserve(&[1]).unwrap();
    let second_path = second
        .publish(0, b"y", RenditionProtection::Internal)
        .unwrap();
    assert_ne!(first.token(), second.token());
    assert!(cache.reserve(&[LEASE_BYTES + 1]).is_err());
    first.remove().unwrap();
    assert_eq!(contents(fs.as_ref(), &second_path), b"y");
    assert!(fs.metadata(&path).unwrap().is_none());
}

#[test]
fn token_collisions_do_not_overwrite_and_released_tokens_are_not_reused() {
    let fs = sim();
    let cache = cache(fs.clone());
    let collision = cache.0.namespace.join("00".repeat(32));
    fs.create_dir(&collision).unwrap();
    let marker = collision.join("keep");
    fs.create(&marker).unwrap().write_all(b"keep").unwrap();
    let mut files = cache.reserve(&[0]).unwrap();
    assert_eq!(files.token(), "01".repeat(32));
    assert_eq!(contents(fs.as_ref(), &marker), b"keep");
    files.remove().unwrap();
    // The cache's RNG wraps after 256 calls. All issued tokens remain guarded.
    for _ in 0..254 {
        cache.reserve(&[]).unwrap().remove().unwrap();
    }
    assert!(
        matches!(cache.reserve(&[]), Err(RenditionError::Io(e)) if e.kind() == io::ErrorKind::AlreadyExists)
    );
}

#[test]
fn every_publication_io_fault_refuses_a_path_and_drop_reclaims_it() {
    for (op, nth) in [
        (FsOp::Create, 1),
        (FsOp::Write, 1),
        (FsOp::SyncFile, 1),
        (FsOp::Rename, 1),
        (FsOp::SyncDir, 4),
        (FsOp::Quarantine, 1),
    ] {
        for kind in [FaultKind::NoSpace, FaultKind::Eio, FaultKind::ShortWrite] {
            let fs = sim();
            let fault = Arc::new(FaultFs::new(fs.clone(), op, nth, kind));
            let cache = cache(fault.clone());
            let mut files = cache.reserve(&[4]).unwrap();
            let dir = files.directory.clone();
            assert!(
                files
                    .publish(0, b"test", RenditionProtection::Quarantine)
                    .is_err(),
                "{op:?} {kind:?}"
            );
            assert_eq!(fault.calls(op), nth);
            if op == FsOp::Quarantine {
                assert_eq!(fault.calls(FsOp::Rename), 0);
                assert!(fs.metadata(&dir.join("file-0")).unwrap().is_none());
            }
            assert!(
                files
                    .publish(0, b"test", RenditionProtection::Internal)
                    .is_err()
            );
            drop(files);
            assert!(fs.metadata(&dir).unwrap().is_none());
            assert_eq!(cache.0.reservations.lock().unwrap().bytes, 0);
        }
    }
}

#[test]
fn cleanup_failure_keeps_the_budget_until_retry_and_missing_is_success() {
    for (op, nth) in [(FsOp::RemoveDirAll, 1), (FsOp::SyncDir, 4)] {
        let fs = sim();
        let fault = Arc::new(FaultFs::new(fs.clone(), op, nth, FaultKind::Eio));
        let cache = cache(fault);
        let mut files = cache.reserve(&[FILE_BYTES]).unwrap();
        assert!(files.remove().is_err());
        assert_eq!(cache.0.reservations.lock().unwrap().bytes, FILE_BYTES);
        files.remove().unwrap();
        files.remove().unwrap();
        assert_eq!(cache.0.reservations.lock().unwrap().bytes, 0);
        let mut files = cache.reserve(&[1]).unwrap();
        fs.remove_dir_all(&files.directory).unwrap();
        files.remove().unwrap();
    }
}

#[test]
fn startup_creates_missing_ancestors_and_sweeps_only_its_namespace() {
    let fs = Arc::new(SimFs::new());
    let root = Path::new("/missing/ancestors/cache");
    let cache = RenditionCache::open_on(root, fs.clone(), Arc::new(Tokens::default())).unwrap();
    let outside = root.join("previews");
    fs.create_dir(&outside).unwrap();
    let stale = cache.0.namespace.join("stale");
    fs.create_dir(&stale).unwrap();
    fs.create(&stale.join("partial.tmp")).unwrap();
    drop(cache);
    let reopened = RenditionCache::open_on(root, fs.clone(), Arc::new(Tokens::default())).unwrap();
    assert!(fs.read_dir(&reopened.0.namespace).unwrap().is_empty());
    assert!(fs.metadata(&outside).unwrap().is_some());
    fs.power_cut();
    assert!(fs.metadata(root).unwrap().is_some());
    assert!(fs.metadata(&stale).unwrap().is_none());
}

#[test]
fn ownership_lock_prevents_sweep_until_every_owner_has_gone() {
    let fs = sim();
    let cache = cache(fs.clone());
    let files = cache.reserve(&[]).unwrap();
    let root = Path::new("/cache");
    drop(cache);
    assert!(matches!(
        RenditionCache::open_on(root, fs.clone(), Arc::new(Tokens::default())),
        Err(RenditionError::Locked)
    ));
    assert!(fs.metadata(&files.directory).unwrap().is_some());
    drop(files);
    assert!(RenditionCache::open_on(root, fs, Arc::new(Tokens::default())).is_ok());
}

#[test]
fn startup_and_reservation_failures_are_errors_without_live_reservations() {
    for op in [
        FsOp::CreatePrivateDir,
        FsOp::ReadDir,
        FsOp::SyncDir,
        FsOp::RestrictDir,
        FsOp::RemoveDirAll,
    ] {
        let fs = sim();
        let namespace = Path::new("/cache/renditions");
        if matches!(op, FsOp::RestrictDir | FsOp::RemoveDirAll) {
            fs.create_dir(namespace).unwrap();
            fs.create_dir(&namespace.join("stale")).unwrap();
        }
        let fault = Arc::new(FaultFs::new(fs, op, 1, FaultKind::Eio));
        assert!(
            RenditionCache::open_on(Path::new("/cache"), fault, Arc::new(Tokens::default()))
                .is_err(),
            "{op:?}"
        );
    }
    for (op, nth) in [(FsOp::CreatePrivateDir, 2), (FsOp::SyncDir, 3)] {
        let fs = sim();
        let fault = Arc::new(FaultFs::new(fs.clone(), op, nth, FaultKind::NoSpace));
        let cache = cache(fault);
        assert!(cache.reserve(&[FILE_BYTES]).is_err());
        assert_eq!(cache.0.reservations.lock().unwrap().bytes, 0);
        assert!(fs.read_dir(&cache.0.namespace).unwrap().is_empty());
    }
}

#[test]
fn real_namespace_is_private_and_a_symlink_namespace_is_refused() {
    use std::os::unix::fs::{PermissionsExt, symlink};
    let scratch = sb_test_support::ScratchDir::new("renditions");
    let root = scratch.path().join("cache");
    let cache = RenditionCache::open(&root).unwrap();
    let files = cache.reserve(&[0]).unwrap();
    for dir in [&cache.0.namespace, &files.directory] {
        assert_eq!(
            std::fs::metadata(dir).unwrap().permissions().mode() & 0o777,
            0o700
        );
    }
    drop(files);
    drop(cache);
    let debris = root.join("renditions/.DS_Store");
    std::fs::write(&debris, b"debris").unwrap();
    drop(RenditionCache::open(&root).unwrap());
    assert!(!debris.exists());
    let alias_root = scratch.path().join("alias");
    std::fs::create_dir(&alias_root).unwrap();
    symlink(root.join("renditions"), alias_root.join("renditions")).unwrap();
    assert!(RenditionCache::open(&alias_root).is_err());
}

#[test]
fn concurrent_reservations_share_one_aggregate_budget() {
    let cache = cache(sim());
    let leases = std::thread::scope(|scope| {
        let tasks: Vec<_> = (0..16)
            .map(|_| {
                let cache = &cache;
                scope.spawn(move || cache.reserve(&[FILE_BYTES, FILE_BYTES]))
            })
            .collect();
        tasks
            .into_iter()
            .map(|t| t.join().unwrap())
            .collect::<Vec<_>>()
    });
    assert_eq!(leases.iter().filter(|r| r.is_ok()).count(), 5);
    assert!(
        leases
            .iter()
            .filter(|r| r.is_err())
            .all(|r| matches!(r, Err(RenditionError::Quota(_))))
    );
    assert_eq!(cache.0.reservations.lock().unwrap().bytes, 500_000_000);
    drop(leases);
    assert_eq!(cache.0.reservations.lock().unwrap().bytes, 0);
}

#[test]
fn failed_drop_cleanup_remains_charged_and_is_swept_on_restart() {
    let fs = sim();
    let fault = Arc::new(FaultFs::new(
        fs.clone(),
        FsOp::RemoveDirAll,
        1,
        FaultKind::Eio,
    ));
    let cache = cache(fault);
    let files = cache.reserve(&[FILE_BYTES]).unwrap();
    let dir = files.directory.clone();
    drop(files);
    assert_eq!(cache.0.reservations.lock().unwrap().bytes, FILE_BYTES);
    assert!(fs.metadata(&dir).unwrap().is_some());
    drop(cache);
    let reopened =
        RenditionCache::open_on(Path::new("/cache"), fs.clone(), Arc::new(Tokens::default()))
            .unwrap();
    assert!(fs.metadata(&dir).unwrap().is_none());
    assert_eq!(reopened.0.reservations.lock().unwrap().bytes, 0);
}

#[test]
fn failing_randomness_does_not_create_a_lease() {
    struct RefuseRandom;
    impl Random for RefuseRandom {
        fn fill(&self, _: &mut [u8]) -> io::Result<()> {
            Err(io::Error::other("randomness unavailable"))
        }
    }
    let fs = sim();
    let cache =
        RenditionCache::open_on(Path::new("/cache"), fs.clone(), Arc::new(RefuseRandom)).unwrap();
    assert!(cache.reserve(&[1]).is_err());
    assert_eq!(cache.0.reservations.lock().unwrap().leases, 0);
    assert!(fs.read_dir(&cache.0.namespace).unwrap().is_empty());
}

#[test]
fn startup_sweeps_stray_files_as_well_as_stale_directories() {
    let fs = sim();
    let cache = cache(fs.clone());
    fs.create(&cache.0.namespace.join(".DS_Store")).unwrap();
    drop(cache);
    let reopened =
        RenditionCache::open_on(Path::new("/cache"), fs.clone(), Arc::new(Tokens::default()))
            .unwrap();
    assert!(fs.read_dir(&reopened.0.namespace).unwrap().is_empty());
}

#[test]
fn startup_reports_errors_removing_stray_files_and_can_retry() {
    let fs = sim();
    let cache = cache(fs.clone());
    let debris = cache.0.namespace.join(".DS_Store");
    fs.create(&debris).unwrap();
    drop(cache);
    let fault = Arc::new(FaultFs::new(fs.clone(), FsOp::Remove, 1, FaultKind::Eio));
    let root = Path::new("/cache");
    assert!(RenditionCache::open_on(root, fault.clone(), Arc::new(Tokens::default())).is_err());
    assert_eq!(fault.calls(FsOp::Remove), 1);
    assert!(fs.metadata(&debris).unwrap().is_some());
    let reopened = RenditionCache::open_on(root, fault, Arc::new(Tokens::default())).unwrap();
    assert!(fs.read_dir(&reopened.0.namespace).unwrap().is_empty());
}

#[test]
fn named_publication_rejects_paths_and_duplicate_names() {
    let fs = sim();
    let cache = cache(fs.clone());
    let mut files = cache.reserve(&[1, 1]).unwrap();
    for name in ["", ".", "..", "../outside", "/absolute", "a/b", "a\\b"] {
        assert!(
            files
                .publish_named(0, name, b"x", RenditionProtection::Internal)
                .is_err()
        );
    }
    let path = files
        .publish_named(0, "manifest.json", b"x", RenditionProtection::Internal)
        .unwrap();
    assert_eq!(contents(fs.as_ref(), &path), b"x");
    assert!(
        files
            .publish_named(1, "manifest.json", b"y", RenditionProtection::Internal)
            .is_err()
    );
    assert_eq!(contents(fs.as_ref(), &path), b"x");
}

// ---------------------------------------------------------------------------
// Crash cuts (spec/11: every durable transition).
//

/// The env vars the cut child reads: the cut point and a fresh cache
/// root.
const CUT_N_ENV: &str = "SB_RENDITIONS_TEST_CUT_N";
const CUT_ROOT_ENV: &str = "SB_RENDITIONS_TEST_CUT_ROOT";

/// What the cut child publishes: a rendition, then a quarantined source.
const CUT_FILES: [&[u8]; 2] = [b"<p>sanitized</p>", b"%PDF-1.7 attachment"];

/// What a previous core left behind: one lease and Finder debris.
const STALE_LEASE: &str = "stale-lease";
const STRAY_FILE: &str = ".DS_Store";

/// Every operation of the child's lifecycle, in order: open and sweep,
/// reserve, publish both files, remove.
const LIFECYCLE_OPERATIONS: [&str; 28] = [
    "metadata",
    "canonicalize",
    "lock_exclusive",
    "create_private_dir",
    "restrict_dir",
    "sync_dir",
    "read_dir",
    "remove_dir_all",
    "remove_dir_all",
    "remove",
    "sync_dir",
    "create_private_dir",
    "sync_dir",
    "metadata",
    "create",
    "write",
    "sync_all",
    "rename",
    "sync_dir",
    "metadata",
    "create",
    "write",
    "quarantine",
    "sync_all",
    "rename",
    "sync_dir",
    "remove_dir_all",
    "sync_dir",
];

/// Leave a stale lease and a stray file for the sweep to remove.
fn leave_debris(root: &Path) {
    let lease = root.join("renditions").join(STALE_LEASE);
    fs::create_dir_all(&lease).unwrap();
    fs::write(lease.join("file-0"), b"stale").unwrap();
    fs::write(root.join("renditions").join(STRAY_FILE), b"debris").unwrap();
}

/// Open, reserve, publish both files and remove, through `fs`.
fn lifecycle(root: &Path, fs: Arc<dyn Fs>) {
    let cache = RenditionCache::open_on(root, fs, Arc::new(SeededRandom::new(7))).unwrap();
    let mut files = cache
        .reserve(&CUT_FILES.map(|bytes| bytes.len() as u64))
        .unwrap();
    files
        .publish(0, CUT_FILES[0], RenditionProtection::Internal)
        .unwrap();
    files
        .publish(1, CUT_FILES[1], RenditionProtection::Quarantine)
        .unwrap();
    files.remove().unwrap();
}

/// Not a test: the crash-cut harness re-executes the test binary to
/// run this. It leaves debris uncut, then runs the lifecycle through a
/// `CutFs` that kills the process before the Nth operation.
#[test]
#[ignore = "spawned by the crash-cut tests"]
fn lifecycle_cut_child() {
    let n: u64 = env::var(CUT_N_ENV)
        .expect("cut point env")
        .parse()
        .expect("a number");
    let root = PathBuf::from(env::var(CUT_ROOT_ENV).expect("cache root env"));
    leave_debris(&root);
    lifecycle(&root, Arc::new(CutFs::new(real_fs(), n)));
}

/// Published names of the lease directories under `namespace`, with
/// their bytes. Temporary files are not published.
fn published_files(namespace: &Path) -> Vec<(String, Vec<u8>)> {
    let mut found = Vec::new();
    for lease in fs::read_dir(namespace).unwrap() {
        let lease = lease.unwrap().path();
        if !lease.is_dir() || lease.ends_with(STALE_LEASE) {
            continue;
        }
        for file in fs::read_dir(&lease).unwrap() {
            let file = file.unwrap().path();
            let name = file.file_name().unwrap().to_string_lossy().into_owned();
            if name.ends_with(".tmp") {
                continue;
            }
            found.push((name, fs::read(&file).unwrap()));
        }
    }
    found
}

/// Cut at every operation of the lifecycle. After each cut a published
/// file is complete, the next open sweeps everything, and a new lease
/// works.
#[test]
fn crash_cuts_leave_complete_files_and_the_next_open_sweeps_them() {
    let scratch = ScratchDir::new("renditions-crash-cuts");
    let mut run = 0u64;
    let cuts = run_crash_cuts(
        CUT_N_ENV,
        CUT_ROOT_ENV,
        "renditions::tests::lifecycle_cut_child",
        &mut |_| {
            run += 1;
            let dir = scratch.path().join(format!("cut-{run}"));
            fs::create_dir(&dir).unwrap();
            dir
        },
        &mut |dir| {
            let namespace = dir.join("renditions");
            for (name, bytes) in published_files(&namespace) {
                let index: usize = name.strip_prefix("file-").unwrap().parse().unwrap();
                assert_eq!(bytes, CUT_FILES[index], "a cut published a partial {name}");
            }

            let cache =
                RenditionCache::open_on(dir, real_fs(), Arc::new(SeededRandom::new(99))).unwrap();
            assert!(
                fs::read_dir(&namespace).unwrap().next().is_none(),
                "the next open left stale entries"
            );

            let mut files = cache.reserve(&[CUT_FILES[0].len() as u64]).unwrap();
            let path = files
                .publish(0, CUT_FILES[0], RenditionProtection::Internal)
                .unwrap();
            assert_eq!(fs::read(path).unwrap(), CUT_FILES[0]);
            files.remove().unwrap();
            assert!(fs::read_dir(&namespace).unwrap().next().is_none());
        },
    );
    // One cut per lifecycle operation, no fewer: an operation that
    // stopped being a cut point would go unswept.
    assert_eq!(cuts, LIFECYCLE_OPERATIONS.len() as u64);
}
