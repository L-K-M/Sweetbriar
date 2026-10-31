//! Acceptance tests for the object store and the publication protocol
//! (plan M0.5). The protocol tests run against the `ObjectStore`
//! trait, over the real `Fs`, `FaultFs`, `SimFs` and — through the
//! child-process harness — real crash cuts.

use std::env;
use std::fs;
use std::io::{Cursor, Read};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Barrier, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use sb_test_support::{
    CutFs, FaultFs, FaultKind, FsOp, ScratchDir, SeededRandom, SimFs, real_fs, run_crash_cuts,
};
use sb_types::fs::Fs;

use super::*;
use crate::StorePaths;
use crate::store::create_layout;
use crate::testing::scratch_paths;
use sha2::{Digest, Sha256};

/// An object store over `fs` and a layout at `paths`, with
/// deterministic randomness. The isolated file-protocol harness
/// commits reservations to a no-op pin sink (spec/03); the real
/// writer path is covered by the store-level reservation tests.
fn object_store(fs: Arc<dyn Fs>, paths: &StorePaths, seed: u64) -> FileObjectStore {
    FileObjectStore::new_for_test(
        fs,
        Arc::new(SeededRandom::new(seed)),
        paths.objects().to_path_buf(),
        paths.recovery().to_path_buf(),
        Box::new(|_, _| Ok(())),
    )
}

/// A scratch store layout on the real filesystem.
fn real_layout(label: &str) -> (ScratchDir, StorePaths) {
    let (scratch, paths) = scratch_paths(label);
    create_layout(real_fs().as_ref(), &paths).unwrap();
    (scratch, paths)
}

/// A scratch store layout inside `SimFs`, its ancestors bootstrapped.
fn sim_layout(label: &str) -> (ScratchDir, StorePaths, Arc<SimFs>) {
    let (scratch, paths) = scratch_paths(label);
    let sim = Arc::new(SimFs::new());
    sim.create_dir_all(scratch.path()).unwrap();
    create_layout(sim.as_ref(), &paths).unwrap();
    (scratch, paths, sim)
}

/// Publish through the trait (spec/03: the publication fault harness
/// runs against the `ObjectStore` trait, so a BLOB implementation
/// could replace files), keeping only the identity.
fn publish<S: ObjectStore + ?Sized>(store: &S, payload: &[u8]) -> Result<ObjectId, StoreError> {
    ObjectStore::publish(store, payload).map(|published| *published.id())
}

/// Where the layout puts an object: `objects/sha256/ab/cd/<64 hex>`.
fn object_at(paths: &StorePaths, id: &ObjectId) -> PathBuf {
    let hex = id.sha256_hex();
    paths
        .objects()
        .join("sha256")
        .join(&hex[..2])
        .join(&hex[2..4])
        .join(&hex)
}

/// The SHA-256 of `bytes`, as the store computes it.
fn sha256_of(bytes: &[u8]) -> [u8; 32] {
    let mut hasher = Sha256::new();
    hasher.update(bytes);
    hasher.finalize().into()
}

/// Everything in a directory, sorted (the empty vec when it is
/// missing), for assertions about leftovers.
fn listing(dir: &Path) -> Vec<PathBuf> {
    let mut entries = fs::read_dir(dir)
        .unwrap_or_else(|e| panic!("cannot list {}: {e}", dir.display()))
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    entries.sort();
    entries
}

// ---------------------------------------------------------------------------
// Publishing and reading (acceptance 1).
//

/// Publish, open and read back identical bytes — over the real `Fs`
/// and over `SimFs` — including empty input; `exists` and `list` see
/// the object; the id's hex is lowercase and matches the payload's
/// hash.
#[test]
fn publish_open_round_trips_over_real_and_sim_fs() {
    fn round_trip(fs: Arc<dyn Fs>, paths: &StorePaths) {
        let store = object_store(fs, paths, 5);
        let payloads: [&[u8]; 2] = [b"the bytes that round trip", b""];
        for payload in payloads {
            let id = publish(&store, payload).unwrap();
            assert_eq!(id.sha256(), sha256_of(payload));
            assert_eq!(id.byte_length(), payload.len() as u64);
            let hex = id.sha256_hex();
            assert_eq!(hex.len(), 64);
            assert!(
                hex.bytes()
                    .all(|b| b.is_ascii_lowercase() || b.is_ascii_digit()),
                "the hex id must be lowercase: {hex}"
            );

            let mut read = Vec::new();
            store.open(&id).unwrap().read_to_end(&mut read).unwrap();
            assert_eq!(read, payload);
            assert!(store.exists(&id).unwrap());
            assert!(
                store.list().unwrap().any(|listed| listed.unwrap() == id),
                "list does not see {hex}"
            );
        }
    }

    let (_scratch, paths) = real_layout("objects-rt-real");
    round_trip(real_fs(), &paths);

    let (_scratch, paths, _sim) = sim_layout("objects-rt-sim");
    round_trip(_sim.clone(), &paths);
}

/// `list` skips anything under `sha256/` that an object is not: wrong
/// name lengths, uppercase, a hash in the wrong chain, stray files.
#[test]
fn list_skips_names_that_are_not_objects() {
    let (_scratch, paths) = real_layout("objects-list-junk");
    let store = object_store(real_fs(), &paths, 5);
    let id = publish(&store, &b"one real object"[..]).unwrap();

    let object = object_at(&paths, &id);
    let cd = object.parent().unwrap();
    let ab = cd.parent().unwrap();
    fs::write(cd.join("not-a-hash"), b"junk").unwrap();
    fs::write(cd.join("tooshort"), b"junk").unwrap();
    fs::write(
        cd.join(format!("{}{}", "A".repeat(60), "b".repeat(4))),
        b"junk",
    )
    .unwrap();
    // A valid name in the wrong chain.
    fs::write(cd.join("f".repeat(64)), b"junk").unwrap();
    fs::write(ab.join("stray-at-chain-level"), b"junk").unwrap();
    fs::write(paths.objects_sha256().join("stray"), b"junk").unwrap();

    let listed = store
        .list()
        .unwrap()
        .map(|listed| listed.unwrap())
        .collect::<Vec<_>>();
    assert_eq!(listed, vec![id]);
}

// ---------------------------------------------------------------------------
// Corruption and quarantine (acceptance 2).
//

/// A wrong length is `Corrupt` and the file moves under `recovery/`;
/// an absent object is `NotFound`; a wrong hash at the right length
/// fails the streaming verification, and a second quarantine of the
/// same id never overwrites the first copy.
#[test]
fn wrong_length_or_hash_is_corrupt_and_moved_to_recovery() {
    let (_scratch, paths) = real_layout("objects-quarantine");
    let store = object_store(real_fs(), &paths, 5);
    let payload: &[u8] = b"these bytes will be damaged";
    let id = publish(&store, payload).unwrap();
    let object = object_at(&paths, &id);
    assert_eq!(listing(paths.recovery()), Vec::<PathBuf>::new());

    // Wrong length: truncate.
    fs::write(&object, &payload[..payload.len() - 4]).unwrap();
    match store.open(&id) {
        Err(StoreError::Corrupt { detail }) => {
            assert!(detail.contains("bytes"), "{detail}");
        }
        other => panic!("expected Corrupt for a truncated object, got {other:?}"),
    }
    assert!(!object.exists(), "the damaged file stayed at its path");
    let first = listing(paths.recovery());
    assert_eq!(first.len(), 1, "nothing was quarantined");
    assert!(
        first[0]
            .file_name()
            .unwrap()
            .to_str()
            .unwrap()
            .starts_with(&format!("object-{}", id.sha256_hex())),
        "the quarantine copy is named after the object: {}",
        first[0].display()
    );

    // Absent: `NotFound`.
    let absent = ObjectId::from_parts([7; 32], 0);
    match store.open(&absent) {
        Err(StoreError::NotFound { what }) => assert!(what.contains("object"), "{what}"),
        other => panic!("expected NotFound, got {other:?}"),
    }

    // Wrong hash, same length: caught while streaming.
    let mut flipped = payload.to_vec();
    flipped[0] ^= 0xff;
    fs::write(&object, &flipped).unwrap();
    let mut read = Vec::new();
    let err = store.open(&id).unwrap().read_to_end(&mut read).unwrap_err();
    assert_eq!(err.kind(), std::io::ErrorKind::InvalidData, "{err}");
    match read_error_as_store_error(&err) {
        Some(StoreError::Corrupt { .. }) => {}
        other => panic!("the read error did not carry Corrupt: {other:?}"),
    }
    assert!(!object.exists(), "the mismatched file stayed at its path");

    // The second quarantine is a second copy; the first is preserved
    // with exactly the bytes it saved. (Their random suffixes sort in
    // any order, so find them by path, not position.)
    let quarantined = listing(paths.recovery());
    assert_eq!(
        quarantined.len(),
        2,
        "the second quarantine overwrote the first"
    );
    assert!(
        quarantined.contains(&first[0]),
        "the first quarantine copy was overwritten"
    );
    assert_eq!(
        fs::read(&first[0]).unwrap(),
        &payload[..payload.len() - 4],
        "the first quarantine copy changed"
    );
    let second_copy = quarantined
        .iter()
        .find(|copy| **copy != first[0])
        .expect("two distinct copies");
    assert_eq!(fs::read(second_copy).unwrap(), flipped);
}

// ---------------------------------------------------------------------------
// Idempotence (acceptance 3).
//

/// Publishing the same bytes twice returns the same id and leaves one
/// file and an empty `objects/tmp/`.
#[test]
fn publishing_twice_keeps_one_file_and_empties_tmp() {
    let (_scratch, paths) = real_layout("objects-twice");
    let store = object_store(real_fs(), &paths, 5);
    let payload: &[u8] = b"published but once";

    let first = publish(&store, payload).unwrap();
    let second = publish(&store, payload).unwrap();
    assert_eq!(first, second);

    let object = object_at(&paths, &first);
    assert_eq!(listing(object.parent().unwrap()), vec![object]);
    assert_eq!(listing(paths.objects_tmp()), Vec::<PathBuf>::new());
}

// ---------------------------------------------------------------------------
// Layout durability (acceptance 4).
//

/// A new store's directories and the data-root entry are flushed: a
/// power cut right after `create_layout` leaves every layout
/// directory in place.
#[test]
fn a_new_stores_layout_survives_a_power_cut() {
    let (_scratch, paths, sim) = sim_layout("objects-layout-cut");
    sim.power_cut();
    for dir in paths.layout_dirs() {
        assert!(
            matches!(
                sim.metadata(dir).unwrap(),
                Some(md) if md.kind() == sb_types::fs::FileKind::Directory
            ),
            "{} did not survive the power cut",
            dir.display()
        );
    }
}

// ---------------------------------------------------------------------------
// Injected failures (acceptance 5).
//

/// Every injected failure makes `publish` return an error (never a
/// reported success): a torn write, `ENOSPC`, `EIO` on the file sync,
/// a `create_dir` failure, a rename failure, and a directory-sync
/// failure at each chain directory. Before the rename, no object
/// exists at its path; after it, a complete hash-valid orphan may
/// remain. A retry succeeds.
#[test]
fn every_injected_publish_failure_errors_and_leaves_no_reference() {
    let payload: &[u8] = b"0123456789abcdef";
    let cases: [(FsOp, u64, FaultKind, &str); 9] = [
        (FsOp::Write, 1, FaultKind::ShortWrite, "a torn write"),
        (FsOp::Write, 1, FaultKind::NoSpace, "no space"),
        (FsOp::SyncFile, 1, FaultKind::Eio, "the file sync"),
        (FsOp::CreateDir, 1, FaultKind::Eio, "creating the chain"),
        (FsOp::Rename, 1, FaultKind::Eio, "the rename"),
        (FsOp::SyncDir, 1, FaultKind::Eio, "flushing sha256/ab"),
        (FsOp::SyncDir, 2, FaultKind::Eio, "flushing sha256/"),
        (FsOp::SyncDir, 3, FaultKind::Eio, "flushing objects/"),
        (
            FsOp::SyncDir,
            4,
            FaultKind::Eio,
            "flushing the object's directory",
        ),
    ];
    for (op, nth, kind, label) in cases {
        let (_scratch, paths) = real_layout("objects-faults");
        // The layout is built untouched; the wrapper sees only the
        // publication.
        let fs = Arc::new(FaultFs::new(real_fs(), op, nth, kind));
        let store = object_store(fs, &paths, 5);

        let failed = publish(&store, payload).unwrap_err();
        assert!(
            matches!(failed, StoreError::Unavailable { .. }),
            "{label}: expected Unavailable, got {failed:?}"
        );
        assert_eq!(
            listing(paths.objects_tmp()),
            Vec::<PathBuf>::new(),
            "{label}: the temp file was left behind"
        );

        let id = ObjectId::from_parts(sha256_of(payload), payload.len() as u64);
        let object = object_at(&paths, &id);
        // The last case fails after the rename: a complete orphan may
        // remain. Everything earlier fails before it: no object.
        if nth == 4 && matches!(op, FsOp::SyncDir) {
            if let Ok(mut reader) = store.open(&id) {
                let mut read = Vec::new();
                reader.read_to_end(&mut read).unwrap();
                assert_eq!(read, payload, "{label}: the orphan is not intact");
            }
        } else {
            assert!(!object.exists(), "{label}: an object exists at its path");
        }

        // The fault is spent; a retry publishes and reads back.
        let retried = publish(&store, payload).unwrap();
        assert_eq!(retried, id);
        let mut read = Vec::new();
        store
            .open(&retried)
            .unwrap()
            .read_to_end(&mut read)
            .unwrap();
        assert_eq!(read, payload, "{label}: the retry did not publish");
    }
}

/// `remove` of an absent object is `NotFound`; of a published one it
/// clears the path and flushes its directory.
#[test]
fn remove_is_not_found_when_absent_and_flushes_when_present() {
    let (_scratch, paths) = real_layout("objects-remove");
    let store = object_store(real_fs(), &paths, 5);
    let payload: &[u8] = b"removable";
    let id = publish(&store, payload).unwrap();
    let object = object_at(&paths, &id);

    let absent = ObjectId::from_parts([9; 32], 3);
    match store.remove(&absent) {
        Err(StoreError::NotFound { .. }) => {}
        other => panic!("expected NotFound, got {other:?}"),
    }

    store.remove(&id).unwrap();
    assert!(!object.exists());
    assert!(!store.exists(&id).unwrap());
}

// ---------------------------------------------------------------------------
// Crash cuts (acceptance 6).
//

/// The env vars the cut child reads: the cut point and a fresh data
/// root.
const CUT_N_ENV: &str = "SB_OBJECTS_TEST_CUT_N";
const CUT_ROOT_ENV: &str = "SB_OBJECTS_TEST_CUT_ROOT";

/// What the cut child publishes; big enough for several writes.
fn cut_payload() -> Vec<u8> {
    (0..100_000u32).map(|i| (i % 251) as u8).collect()
}

/// Not a test: the crash-cut harness re-executes the test binary to
/// run this. It builds the layout uncut, then publishes through a
/// `CutFs` that kills the process before the Nth filesystem
/// operation.
#[test]
#[ignore = "spawned by the crash-cut tests"]
fn publish_cut_child() {
    let n: u64 = env::var(CUT_N_ENV)
        .expect("cut point env")
        .parse()
        .expect("a number");
    let root = env::var(CUT_ROOT_ENV).expect("data root env");
    let paths = StorePaths::new(&root).expect("an absolute data root");
    create_layout(real_fs().as_ref(), &paths).expect("the layout");
    let fs = Arc::new(CutFs::new(real_fs(), n));
    let store = FileObjectStore::new_for_test(
        fs,
        Arc::new(SeededRandom::new(7)),
        paths.objects().to_path_buf(),
        paths.recovery().to_path_buf(),
        Box::new(|_, _| Ok(())),
    );
    store
        .publish(Cursor::new(cut_payload()))
        .expect("an uncut publish");
}

/// Cut at every filesystem operation of a publish. After each cut the
/// object is absent or opens and reads back complete, and a new
/// publish of the same bytes then succeeds.
#[test]
fn crash_cuts_leave_no_object_or_a_complete_one() {
    let payload = cut_payload();
    let expected = ObjectId::from_parts(sha256_of(&payload), payload.len() as u64);
    let scratch = ScratchDir::new("objects-crash-cuts");
    let mut run = 0u64;
    let cuts = run_crash_cuts(
        CUT_N_ENV,
        CUT_ROOT_ENV,
        "objects::tests::publish_cut_child",
        &mut |_| {
            run += 1;
            let dir = scratch.path().join(format!("cut-{run}"));
            fs::create_dir(&dir).unwrap();
            dir
        },
        &mut |dir| {
            let paths = StorePaths::new(dir).unwrap();
            let store = object_store(real_fs(), &paths, 99);
            match store.open(&expected) {
                Err(StoreError::NotFound { .. }) => {}
                Ok(mut reader) => {
                    let mut read = Vec::new();
                    reader.read_to_end(&mut read).unwrap();
                    assert_eq!(read, payload, "a cut left a damaged object");
                }
                Err(other) => panic!("a cut left an unusable object: {other:?}"),
            }
            let id = publish(&store, payload.as_slice()).unwrap();
            assert_eq!(id, expected);
            let mut read = Vec::new();
            store.open(&id).unwrap().read_to_end(&mut read).unwrap();
            assert_eq!(read, payload);
        },
    );
    // One cut per operation a publish performs, no fewer: an
    // operation that stopped being a cut point would go unswept.
    assert_eq!(
        cuts,
        PUBLISH_OPERATIONS.len() as u64,
        "the sweep didn't cut at every publish operation"
    );
}

// ---------------------------------------------------------------------------
// Power-cut acknowledgement (acceptance 7).
//

/// A directory-sync failure during the first publish, a retry that
/// succeeds, then a power cut: the acknowledged object opens and
/// reads back, because the retry flushed the whole chain.
#[test]
fn a_failed_chain_flush_then_retry_survives_a_power_cut() {
    let (_scratch, paths, sim) = sim_layout("objects-sim-retry");
    let fs: Arc<dyn Fs> = Arc::new(FaultFs::new(sim.clone(), FsOp::SyncDir, 1, FaultKind::Eio));
    let store = object_store(fs, &paths, 11);
    let payload: &[u8] = b"acknowledge me";

    assert!(
        matches!(
            publish(&store, payload),
            Err(StoreError::Unavailable { .. })
        ),
        "the injected chain-flush failure was not reported"
    );
    let id = publish(&store, payload).unwrap();

    sim.power_cut();
    let mut read = Vec::new();
    store.open(&id).unwrap().read_to_end(&mut read).unwrap();
    assert_eq!(read, payload, "the acknowledged object was lost");
}

/// Two payloads whose hashes share their first two bytes (found by
/// counting), published concurrently into one new chain directory.
/// Both are acknowledged, and both survive the power cut.
#[test]
fn concurrent_publications_into_one_new_chain_survive_a_power_cut() {
    let (first, second) = colliding_payloads();
    assert_eq!(
        sha256_of(&first)[..2],
        sha256_of(&second)[..2],
        "the payloads must hash into one chain"
    );
    let (_scratch, paths, sim) = sim_layout("objects-sim-concurrent");
    let store = object_store(sim.clone(), &paths, 13);
    let barrier = Barrier::new(2);

    let ids = thread::scope(|scope| {
        let payloads = [first.clone(), second.clone()];
        let handles = payloads.map(|payload| {
            let store = &store;
            let barrier = &barrier;
            scope.spawn(move || {
                barrier.wait();
                publish(store, &payload)
            })
        });
        let ids = handles.map(|handle| {
            handle
                .join()
                .expect("a panicked publisher")
                .expect("a concurrent publication into a new chain was not acknowledged")
        });
        assert_ne!(ids[0], ids[1]);
        ids
    });

    sim.power_cut();
    for (id, payload) in ids.iter().zip([&first, &second]) {
        let mut read = Vec::new();
        store.open(id).unwrap().read_to_end(&mut read).unwrap();
        assert_eq!(&read, payload, "an acknowledged object was lost");
    }
}

/// Two short inputs hashing to the same first two bytes. Fixing the
/// first input's prefix and counting up meets a mate after about 2^16
/// tries.
fn colliding_payloads() -> (Vec<u8>, Vec<u8>) {
    let first = b"Sweetbriar collides 0".to_vec();
    let prefix = sha256_of(&first)[..2].to_vec();
    let mut counter = 1u64;
    loop {
        let candidate = format!("Sweetbriar collides {counter}").into_bytes();
        if sha256_of(&candidate)[..2] == prefix[..] {
            return (first, candidate);
        }
        counter += 1;
    }
}

// ---------------------------------------------------------------------------
// The store hands its object store out (acceptance 8).
//

/// `Store::objects` publishes into the opened store's `objects/`, at
/// exactly the path the layout promises.
#[test]
fn an_open_stores_objects_publish_into_its_objects_dir() {
    let (_scratch, paths) = scratch_paths("objects-store");
    let seams = crate::testing::test_seams(real_fs());
    let store = crate::open(&paths, &seams).unwrap();

    let payload: &[u8] = b"through the store";
    let id = *store.objects().publish(payload).unwrap().id();
    let hex = id.sha256_hex();
    let expected = paths
        .objects()
        .join("sha256")
        .join(&hex[..2])
        .join(&hex[2..4])
        .join(&hex);
    assert_eq!(fs::read(&expected).unwrap(), payload);

    let mut read = Vec::new();
    store
        .objects()
        .open(&id)
        .unwrap()
        .read_to_end(&mut read)
        .unwrap();
    assert_eq!(read, payload);
}

// ---------------------------------------------------------------------------
// Review regressions: reads, races, durability of moves.
//

/// An empty buffer reads nothing without meaning end of file: the
/// object stays published and reads back whole.
#[test]
fn an_empty_read_is_not_end_of_file() {
    let (_scratch, paths) = real_layout("objects-empty-read");
    let store = object_store(real_fs(), &paths, 5);
    let payload: &[u8] = b"not quarantined by an empty read";
    let id = publish(&store, payload).unwrap();

    let mut reader = store.open(&id).unwrap();
    assert_eq!(reader.read(&mut []).unwrap(), 0);
    let mut read = Vec::new();
    reader.read_to_end(&mut read).unwrap();
    assert_eq!(read, payload);
    assert!(store.exists(&id).unwrap());
    assert_eq!(listing(paths.recovery()), Vec::<PathBuf>::new());
}

/// A reader that finishes after the object was quarantined and
/// republished reports its own corruption but leaves the new copy
/// alone.
#[test]
fn a_late_reader_never_quarantines_a_republished_copy() {
    let (_scratch, paths) = real_layout("objects-late-reader");
    let store = object_store(real_fs(), &paths, 5);
    let payload: &[u8] = b"damaged, then restored";
    let id = publish(&store, payload).unwrap();
    let mut flipped = payload.to_vec();
    flipped[0] ^= 0xff;
    fs::write(object_at(&paths, &id), &flipped).unwrap();

    let mut early = store.open(&id).unwrap();
    let mut late = store.open(&id).unwrap();
    let err = early.read_to_end(&mut Vec::new()).unwrap_err();
    assert!(matches!(
        read_error_as_store_error(&err),
        Some(StoreError::Corrupt { .. })
    ));
    assert_eq!(publish(&store, payload).unwrap(), id);

    let err = late.read_to_end(&mut Vec::new()).unwrap_err();
    assert!(matches!(
        read_error_as_store_error(&err),
        Some(StoreError::Corrupt { .. })
    ));
    let mut read = Vec::new();
    store.open(&id).unwrap().read_to_end(&mut read).unwrap();
    assert_eq!(read, payload, "the republished copy was moved away");
    assert_eq!(listing(paths.recovery()).len(), 1);
}

/// An `Fs` that, after each look at one path, holds the caller until
/// two threads have looked (or a short wait passes), so both act on
/// what they saw before either moved on; and that counts renames onto
/// that path (a second one replaces the first's file).
struct RaceFs {
    inner: Arc<dyn Fs>,
    watched: PathBuf,
    arrived: Mutex<usize>,
    both: Condvar,
    renames_onto_watched: AtomicUsize,
}

/// How long `RaceFs` waits for the second thread: without the store's
/// install lock both arrive at once; with it the second can't arrive
/// until the first is done, so the first gives up after this.
const RACE_WAIT: Duration = Duration::from_millis(200);

impl Fs for RaceFs {
    fn metadata(&self, path: &Path) -> std::io::Result<Option<sb_types::fs::Metadata>> {
        let seen = self.inner.metadata(path);
        if path == self.watched {
            let mut arrived = self.arrived.lock().unwrap();
            *arrived += 1;
            self.both.notify_all();
            let _ = self
                .both
                .wait_timeout_while(arrived, RACE_WAIT, |n| *n < 2)
                .unwrap();
        }
        seen
    }

    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        if to == self.watched {
            self.renames_onto_watched.fetch_add(1, Ordering::SeqCst);
        }
        self.inner.rename(from, to)
    }

    fn create(&self, path: &Path) -> std::io::Result<Box<dyn sb_types::fs::WriteFile>> {
        self.inner.create(path)
    }
    fn open(&self, path: &Path) -> std::io::Result<Box<dyn sb_types::fs::ReadFile>> {
        self.inner.open(path)
    }
    fn open_regular(
        &self,
        path: &Path,
    ) -> std::io::Result<(Box<dyn sb_types::fs::ReadFile>, sb_types::fs::Metadata)> {
        self.inner.open_regular(path)
    }
    fn read_dir(&self, path: &Path) -> std::io::Result<Vec<PathBuf>> {
        self.inner.read_dir(path)
    }
    fn create_dir(&self, path: &Path) -> std::io::Result<()> {
        self.inner.create_dir(path)
    }
    fn sync_dir(&self, path: &Path) -> std::io::Result<()> {
        self.inner.sync_dir(path)
    }
    fn remove(&self, path: &Path) -> std::io::Result<()> {
        self.inner.remove(path)
    }
    fn canonicalize(&self, path: &Path) -> std::io::Result<PathBuf> {
        self.inner.canonicalize(path)
    }
    fn lock_exclusive(
        &self,
        path: &Path,
    ) -> Result<Box<dyn sb_types::fs::Lease>, sb_types::fs::LockError> {
        self.inner.lock_exclusive(path)
    }
    fn location(&self, path: &Path) -> std::io::Result<sb_types::fs::Location> {
        self.inner.location(path)
    }
}

/// Two concurrent publications of the same bytes install one file: the
/// second sees the first's and drops its temp copy rather than
/// renaming over a published object.
#[test]
fn concurrent_identical_publications_never_replace_a_published_file() {
    let (_scratch, paths) = real_layout("objects-identical-race");
    let payload: &[u8] = b"published twice at once";
    let id = ObjectId::from_parts(sha256_of(payload), payload.len() as u64);
    let race = Arc::new(RaceFs {
        inner: real_fs(),
        watched: object_at(&paths, &id),
        arrived: Mutex::new(0),
        both: Condvar::new(),
        renames_onto_watched: AtomicUsize::new(0),
    });
    let store = object_store(race.clone(), &paths, 5);
    let barrier = Barrier::new(2);

    thread::scope(|scope| {
        let publishers = [(), ()].map(|()| {
            scope.spawn(|| {
                barrier.wait();
                publish(&store, payload).unwrap()
            })
        });
        for publisher in publishers {
            assert_eq!(publisher.join().unwrap(), id);
        }
    });
    assert_eq!(
        race.renames_onto_watched.load(Ordering::SeqCst),
        1,
        "a second publisher renamed over the published file"
    );
    assert_eq!(listing(&paths.objects().join("tmp")), Vec::<PathBuf>::new());
}

/// A failed directory flush at any of a publish's four `sync_dir`
/// calls (the chain's three, then the final directory's), a retry that
/// succeeds, then a power cut: the acknowledged object survives. The
/// fourth leaves a renamed but unflushed file, so the retry must flush
/// the final directory although the file already exists.
#[test]
fn every_failed_directory_flush_then_retry_survives_a_power_cut() {
    for call in 1..=4 {
        let (_scratch, paths, sim) = sim_layout("objects-sim-retry-each");
        let fs: Arc<dyn Fs> = Arc::new(FaultFs::new(
            sim.clone(),
            FsOp::SyncDir,
            call,
            FaultKind::Eio,
        ));
        let store = object_store(fs, &paths, 11);
        let payload: &[u8] = b"acknowledged after a retry";

        assert!(
            matches!(
                publish(&store, payload),
                Err(StoreError::Unavailable { .. })
            ),
            "sync_dir call {call}: the failure was not reported"
        );
        let id = publish(&store, payload).unwrap();
        sim.power_cut();
        let mut read = Vec::new();
        store.open(&id).unwrap().read_to_end(&mut read).unwrap();
        assert_eq!(read, payload, "sync_dir call {call}: the object was lost");
    }
}

/// Replace the file at `path` in `fs` with `bytes`, durably: a damaged
/// object as a disk would hold it.
fn damage(fs: &dyn Fs, path: &Path, bytes: &[u8]) {
    fs.remove(path).unwrap();
    let mut file = fs.create(path).unwrap();
    std::io::Write::write_all(&mut file, bytes).unwrap();
    file.sync_all().unwrap();
    drop(file);
    fs.sync_dir(path.parent().unwrap()).unwrap();
}

/// A quarantine is durable: after `Corrupt` and a power cut, the
/// damaged bytes are in `recovery/` and the object's path is empty.
/// So is a removal: a removed object doesn't come back.
#[test]
fn quarantine_and_removal_survive_a_power_cut() {
    let (_scratch, paths, sim) = sim_layout("objects-sim-moves");
    let store = object_store(sim.clone(), &paths, 11);
    let payload: &[u8] = b"to be damaged";
    let id = publish(&store, payload).unwrap();
    damage(sim.as_ref(), &object_at(&paths, &id), b"short");

    assert!(matches!(store.open(&id), Err(StoreError::Corrupt { .. })));
    sim.power_cut();
    assert!(!store.exists(&id).unwrap(), "the damaged file came back");
    let quarantined = sim.read_dir(paths.recovery()).unwrap();
    assert_eq!(quarantined.len(), 1, "the quarantine copy was lost");
    let mut saved = Vec::new();
    sim.open(&quarantined[0])
        .unwrap()
        .read_to_end(&mut saved)
        .unwrap();
    assert_eq!(saved, b"short");

    let kept = publish(&store, b"to be removed").unwrap();
    store.remove(&kept).unwrap();
    sim.power_cut();
    assert!(
        !store.exists(&kept).unwrap(),
        "the removed object came back"
    );
}

/// Every step of a quarantine reports its failure: the rename, the
/// flush of `recovery/`, the flush of the directory it left.
#[test]
fn every_quarantine_failure_is_unavailable() {
    // After one publish of a new chain: 1 rename, 4 directory syncs.
    for (op, nth) in [(FsOp::Rename, 2), (FsOp::SyncDir, 5), (FsOp::SyncDir, 6)] {
        let (_scratch, paths, sim) = sim_layout("objects-sim-quarantine-fault");
        let fs: Arc<dyn Fs> = Arc::new(FaultFs::new(sim.clone(), op, nth, FaultKind::Eio));
        let store = object_store(fs, &paths, 11);
        let id = publish(&store, b"to be damaged").unwrap();
        damage(sim.as_ref(), &object_at(&paths, &id), b"short");
        assert!(
            matches!(store.open(&id), Err(StoreError::Unavailable { .. })),
            "{op:?} call {nth}: the quarantine failure was not reported"
        );
    }
}

/// The `Fs` operations a publish of `cut_payload()` into a new chain
/// performs, in order (two writes: the payload spans two stream
/// chunks). The crash sweep must cut at each of them.
const PUBLISH_OPERATIONS: [&str; 12] = [
    "create",
    "write",
    "write",
    "sync_all",
    "create_dir",
    "create_dir",
    "sync_dir",
    "sync_dir",
    "sync_dir",
    "metadata",
    "rename",
    "sync_dir",
];

/// An `Fs` that records the name of every operation, its write
/// handles' included: an enumeration independent of `CutFs`.
struct RecordFs {
    inner: Arc<dyn Fs>,
    log: Arc<Mutex<Vec<&'static str>>>,
}

impl RecordFs {
    fn note(&self, op: &'static str) {
        self.log.lock().unwrap().push(op);
    }
}

/// A write handle that records into its `RecordFs`'s log.
struct RecordWrite {
    inner: Box<dyn sb_types::fs::WriteFile>,
    log: Arc<Mutex<Vec<&'static str>>>,
}

impl std::io::Write for RecordWrite {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.log.lock().unwrap().push("write");
        self.inner.write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl sb_types::fs::WriteFile for RecordWrite {
    fn sync_all(&mut self) -> std::io::Result<()> {
        self.log.lock().unwrap().push("sync_all");
        self.inner.sync_all()
    }
}

impl Fs for RecordFs {
    fn create(&self, path: &Path) -> std::io::Result<Box<dyn sb_types::fs::WriteFile>> {
        self.note("create");
        Ok(Box::new(RecordWrite {
            inner: self.inner.create(path)?,
            log: Arc::clone(&self.log),
        }))
    }
    fn open(&self, path: &Path) -> std::io::Result<Box<dyn sb_types::fs::ReadFile>> {
        self.note("open");
        self.inner.open(path)
    }
    fn open_regular(
        &self,
        path: &Path,
    ) -> std::io::Result<(Box<dyn sb_types::fs::ReadFile>, sb_types::fs::Metadata)> {
        self.note("open_regular");
        self.inner.open_regular(path)
    }
    fn metadata(&self, path: &Path) -> std::io::Result<Option<sb_types::fs::Metadata>> {
        self.note("metadata");
        self.inner.metadata(path)
    }
    fn read_dir(&self, path: &Path) -> std::io::Result<Vec<PathBuf>> {
        self.note("read_dir");
        self.inner.read_dir(path)
    }
    fn create_dir(&self, path: &Path) -> std::io::Result<()> {
        self.note("create_dir");
        self.inner.create_dir(path)
    }
    fn sync_dir(&self, path: &Path) -> std::io::Result<()> {
        self.note("sync_dir");
        self.inner.sync_dir(path)
    }
    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        self.note("rename");
        self.inner.rename(from, to)
    }
    fn remove(&self, path: &Path) -> std::io::Result<()> {
        self.note("remove");
        self.inner.remove(path)
    }
    fn canonicalize(&self, path: &Path) -> std::io::Result<PathBuf> {
        self.note("canonicalize");
        self.inner.canonicalize(path)
    }
    fn lock_exclusive(
        &self,
        path: &Path,
    ) -> Result<Box<dyn sb_types::fs::Lease>, sb_types::fs::LockError> {
        self.note("lock_exclusive");
        self.inner.lock_exclusive(path)
    }
    fn location(&self, path: &Path) -> std::io::Result<sb_types::fs::Location> {
        self.note("location");
        self.inner.location(path)
    }
}

/// `PUBLISH_OPERATIONS` is what a publish really does, so the crash
/// sweep's cut count can be held to it. The pin sink's "reserve" must
/// sit between the chain flush and the install's existence check — the
/// reservation commits before the file installs (spec/03).
#[test]
fn a_publish_performs_exactly_the_listed_operations() {
    let (_scratch, paths) = real_layout("objects-op-sequence");
    let log = Arc::new(Mutex::new(Vec::new()));
    let fs = Arc::new(RecordFs {
        inner: real_fs(),
        log: Arc::clone(&log),
    });
    let pins = Arc::clone(&log);
    let store = FileObjectStore::new_for_test(
        fs,
        Arc::new(SeededRandom::new(7)),
        paths.objects().to_path_buf(),
        paths.recovery().to_path_buf(),
        Box::new(move |_, _| {
            pins.lock().unwrap().push("reserve");
            Ok(())
        }),
    );
    store.publish(Cursor::new(cut_payload())).unwrap();
    let mut expected = PUBLISH_OPERATIONS.to_vec();
    // The install step's first op is its existence `metadata`.
    expected.insert(PUBLISH_OPERATIONS.len() - 3, "reserve");
    assert_eq!(*log.lock().unwrap(), expected);
}

/// The `StoreError` a failed `ObjectReader` read carries.
fn read_error(reader: &mut ObjectReader) -> StoreError {
    let err = reader.read_to_end(&mut Vec::new()).unwrap_err();
    match read_error_as_store_error(&err) {
        // Clone the whole error: the recovery must keep the typed
        // `io_kind` a quarantine failure carried, not just the reason.
        Some(error) => error.clone(),
        None => panic!("unexpected read error {err}"),
    }
}

/// A quarantine that moved the file but couldn't flush `recovery/`
/// isn't `Corrupt` yet: the first reader gets `Unavailable`; a second
/// reader, finding the file already moved, flushes before reporting
/// `Corrupt`, so after a power cut the damaged bytes are in
/// `recovery/` and not back at the object's path.
#[test]
fn a_streaming_quarantine_reports_corrupt_only_once_durable() {
    let (_scratch, paths, sim) = sim_layout("objects-sim-stream-quarantine");
    // One publish of a new chain makes four directory syncs; the fifth
    // is the quarantine's flush of `recovery/`.
    let fs: Arc<dyn Fs> = Arc::new(FaultFs::new(sim.clone(), FsOp::SyncDir, 5, FaultKind::Eio));
    let store = object_store(fs, &paths, 11);
    let payload: &[u8] = b"damaged in place";
    let id = publish(&store, payload).unwrap();
    let mut flipped = payload.to_vec();
    flipped[0] ^= 0xff;
    damage(sim.as_ref(), &object_at(&paths, &id), &flipped);

    let mut first = store.open(&id).unwrap();
    let mut second = store.open(&id).unwrap();
    assert!(matches!(
        read_error(&mut first),
        StoreError::Unavailable { .. }
    ));
    assert!(matches!(
        read_error(&mut second),
        StoreError::Corrupt { .. }
    ));

    sim.power_cut();
    assert!(!store.exists(&id).unwrap(), "the damaged file came back");
    assert_eq!(sim.read_dir(paths.recovery()).unwrap().len(), 1);
}

/// A quarantine whose rename failed reports `Unavailable`, and the
/// same reader's next read tries again rather than claiming `Corrupt`
/// with the damaged file still published.
#[test]
fn a_failed_streaming_quarantine_is_retried() {
    let (_scratch, paths, sim) = sim_layout("objects-sim-stream-rename");
    // The publish makes rename 1; the quarantine's is rename 2.
    let fs: Arc<dyn Fs> = Arc::new(FaultFs::new(sim.clone(), FsOp::Rename, 2, FaultKind::Eio));
    let store = object_store(fs, &paths, 11);
    let payload: &[u8] = b"damaged in place";
    let id = publish(&store, payload).unwrap();
    let mut flipped = payload.to_vec();
    flipped[0] ^= 0xff;
    damage(sim.as_ref(), &object_at(&paths, &id), &flipped);

    let mut reader = store.open(&id).unwrap();
    assert!(matches!(
        read_error(&mut reader),
        StoreError::Unavailable { .. }
    ));
    assert!(store.exists(&id).unwrap(), "nothing moved yet");
    assert!(matches!(
        read_error(&mut reader),
        StoreError::Corrupt { .. }
    ));
    assert!(
        !store.exists(&id).unwrap(),
        "Corrupt with the file still published"
    );
    assert_eq!(sim.read_dir(paths.recovery()).unwrap().len(), 1);
}

/// An `Fs` that removes one file inside `open`, after `metadata` saw
/// it: the object vanishing between the look and the open.
struct VanishFs {
    inner: Arc<dyn Fs>,
    vanishing: PathBuf,
}

impl Fs for VanishFs {
    fn open(&self, path: &Path) -> std::io::Result<Box<dyn sb_types::fs::ReadFile>> {
        if path == self.vanishing {
            self.inner.remove(path)?;
        }
        self.inner.open(path)
    }

    fn open_regular(
        &self,
        path: &Path,
    ) -> std::io::Result<(Box<dyn sb_types::fs::ReadFile>, sb_types::fs::Metadata)> {
        if path == self.vanishing {
            self.inner.remove(path)?;
        }
        self.inner.open_regular(path)
    }

    fn create(&self, path: &Path) -> std::io::Result<Box<dyn sb_types::fs::WriteFile>> {
        self.inner.create(path)
    }
    fn metadata(&self, path: &Path) -> std::io::Result<Option<sb_types::fs::Metadata>> {
        self.inner.metadata(path)
    }
    fn read_dir(&self, path: &Path) -> std::io::Result<Vec<PathBuf>> {
        self.inner.read_dir(path)
    }
    fn create_dir(&self, path: &Path) -> std::io::Result<()> {
        self.inner.create_dir(path)
    }
    fn sync_dir(&self, path: &Path) -> std::io::Result<()> {
        self.inner.sync_dir(path)
    }
    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        self.inner.rename(from, to)
    }
    fn remove(&self, path: &Path) -> std::io::Result<()> {
        self.inner.remove(path)
    }
    fn canonicalize(&self, path: &Path) -> std::io::Result<PathBuf> {
        self.inner.canonicalize(path)
    }
    fn lock_exclusive(
        &self,
        path: &Path,
    ) -> Result<Box<dyn sb_types::fs::Lease>, sb_types::fs::LockError> {
        self.inner.lock_exclusive(path)
    }
    fn location(&self, path: &Path) -> std::io::Result<sb_types::fs::Location> {
        self.inner.location(path)
    }
}

/// An object that vanishes between `open`'s look and its open is
/// `NotFound`, as if it had been absent all along.
#[test]
fn an_object_vanishing_during_open_is_not_found() {
    let (_scratch, paths) = real_layout("objects-vanish");
    let payload: &[u8] = b"here, then gone";
    let id = publish(&object_store(real_fs(), &paths, 5), payload).unwrap();
    let fs = Arc::new(VanishFs {
        inner: real_fs(),
        vanishing: object_at(&paths, &id),
    });
    let store = object_store(fs, &paths, 5);
    assert!(matches!(store.open(&id), Err(StoreError::NotFound { .. })));
}

/// A removal counts as a move: a reader opened before it, finishing
/// after the object was republished, leaves the new copy alone.
#[test]
fn a_late_reader_never_quarantines_a_copy_republished_after_removal() {
    let (_scratch, paths) = real_layout("objects-late-reader-remove");
    let store = object_store(real_fs(), &paths, 5);
    let payload: &[u8] = b"removed, then restored";
    let id = publish(&store, payload).unwrap();
    let mut flipped = payload.to_vec();
    flipped[0] ^= 0xff;
    fs::write(object_at(&paths, &id), &flipped).unwrap();

    let mut late = store.open(&id).unwrap();
    store.remove(&id).unwrap();
    assert_eq!(publish(&store, payload).unwrap(), id);
    assert!(matches!(read_error(&mut late), StoreError::Corrupt { .. }));
    let mut read = Vec::new();
    store.open(&id).unwrap().read_to_end(&mut read).unwrap();
    assert_eq!(read, payload, "the republished copy was moved away");
}

/// Both steps of a removal report their failure: the unlink and the
/// directory flush.
#[test]
fn every_removal_failure_is_unavailable() {
    // After one publish of a new chain: 4 directory syncs, no removal.
    for (op, nth) in [(FsOp::Remove, 1), (FsOp::SyncDir, 5)] {
        let (_scratch, paths, sim) = sim_layout("objects-sim-remove-fault");
        let fs: Arc<dyn Fs> = Arc::new(FaultFs::new(sim.clone(), op, nth, FaultKind::Eio));
        let store = object_store(fs, &paths, 11);
        let id = publish(&store, b"to be removed").unwrap();
        assert!(
            matches!(store.remove(&id), Err(StoreError::Unavailable { .. })),
            "{op:?} call {nth}: the removal failure was not reported"
        );
    }
}

/// Randomness that always hands out the same bytes: every temp name
/// repeats.
struct RepeatingRandom;

impl sb_types::random::Random for RepeatingRandom {
    fn fill(&self, dest: &mut [u8]) -> std::io::Result<()> {
        dest.fill(0x5a);
        Ok(())
    }
}

/// A publish whose temp name is taken fails without removing the file
/// there: it may be another publisher's live temp file.
#[test]
fn a_taken_temp_name_fails_without_removing_its_file() {
    let (_scratch, paths) = real_layout("objects-taken-temp");
    let store = FileObjectStore::new_for_test(
        real_fs(),
        Arc::new(RepeatingRandom),
        paths.objects().to_path_buf(),
        paths.recovery().to_path_buf(),
        Box::new(|_, _| Ok(())),
    );
    let mut name = [0u8; 16];
    RepeatingRandom.fill(&mut name).unwrap();
    let taken = paths.objects().join("tmp").join(hex(&name));
    fs::write(&taken, b"someone else's temp file").unwrap();

    assert!(matches!(
        publish(&store, b"bytes"),
        Err(StoreError::Unavailable { .. })
    ));
    assert_eq!(fs::read(&taken).unwrap(), b"someone else's temp file");
}

/// `open`'s I/O failures keep their native kinds in `Unavailable`'s
/// `io_kind`, and a file gone by `open` still decides `NotFound`
/// (spec/03 "Search writes").
#[test]
fn open_failures_keep_their_native_kinds() {
    for kind in [
        std::io::ErrorKind::PermissionDenied,
        std::io::ErrorKind::StorageFull,
    ] {
        let (_scratch, paths, sim) = sim_layout("objects-sim-open-kind");
        let fs: Arc<dyn Fs> = Arc::new(FaultFs::new(
            sim.clone(),
            FsOp::Open,
            1,
            FaultKind::Kind(kind),
        ));
        let store = object_store(fs, &paths, 11);
        let id = publish(&store, b"payload").unwrap();
        let Err(error) = store.open(&id) else {
            panic!("the injected open fault did not fire")
        };
        assert!(matches!(error, StoreError::Unavailable { .. }), "{error:?}");
        assert_eq!(error.io_kind(), Some(kind));
    }
    // A file gone by `open` is still `NotFound`: a native kind never
    // changes the missing-object decision.
    let (_scratch, paths, sim) = sim_layout("objects-sim-open-gone");
    let fs: Arc<dyn Fs> = Arc::new(FaultFs::new(
        sim.clone(),
        FsOp::Open,
        1,
        FaultKind::Kind(std::io::ErrorKind::NotFound),
    ));
    let store = object_store(fs, &paths, 11);
    let id = publish(&store, b"payload").unwrap();
    assert!(matches!(store.open(&id), Err(StoreError::NotFound { .. })));
}

/// A quarantine failure's native kind survives being wrapped in the
/// reader's `io::Error` and recovered; the `Corrupt` a successful
/// retry reports carries none (spec/03 "Search writes").
#[test]
fn a_failed_quarantine_keeps_its_io_kind() {
    let (_scratch, paths, sim) = sim_layout("objects-sim-quarantine-kind");
    // The publish makes rename 1; the quarantine's is rename 2.
    let fs: Arc<dyn Fs> = Arc::new(FaultFs::new(
        sim.clone(),
        FsOp::Rename,
        2,
        FaultKind::Kind(std::io::ErrorKind::ResourceBusy),
    ));
    let store = object_store(fs, &paths, 11);
    let payload: &[u8] = b"damaged in place";
    let id = publish(&store, payload).unwrap();
    let mut flipped = payload.to_vec();
    flipped[0] ^= 0xff;
    damage(sim.as_ref(), &object_at(&paths, &id), &flipped);

    let mut reader = store.open(&id).unwrap();
    let error = read_error(&mut reader);
    let StoreError::Unavailable { reason, .. } = &error else {
        panic!("expected Unavailable, got {error:?}")
    };
    assert!(reason.contains("cannot quarantine the object"), "{reason}");
    assert_eq!(error.io_kind(), Some(std::io::ErrorKind::ResourceBusy));
    let retried = read_error(&mut reader);
    assert!(matches!(retried, StoreError::Corrupt { .. }), "{retried:?}");
    assert_eq!(retried.io_kind(), None);
}
