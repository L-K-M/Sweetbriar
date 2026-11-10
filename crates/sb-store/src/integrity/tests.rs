//! M1.8h5a: the integrity check, the read-only latch it sets, the
//! evidence copy, and opening a latched store.

use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Barrier};

use rusqlite::Connection;
use sb_test_support::{ScratchDir, block_on, real_fs};
use sha2::{Digest, Sha256};

use super::*;
use crate::schema::bug;
use crate::testing::{extra_migration, reading, scratch_paths, test_seams, writing};
use crate::{IntegrityCheck, WritePriority};

/// A store holding a small indexed fixture table.
fn open_with_probe(label: &str) -> (ScratchDir, StorePaths, Store) {
    let (scratch, paths) = scratch_paths(label);
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    writing(&store, |tx| {
        tx.execute_batch(
            "CREATE TABLE probe (seq INTEGER PRIMARY KEY, pad TEXT NOT NULL);
             CREATE INDEX probe_pad ON probe(pad);
             INSERT INTO probe (seq, pad)
             WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x + 1 FROM c WHERE x < 200)
             SELECT x, printf('pad-%04d', x) FROM c;",
        )
        .map_err(bug)
    });
    (scratch, paths, store)
}

/// Damage the probe's index the way `quick_check` doesn't see: with
/// the store closed, its definition is rewritten to another column,
/// so every entry the b-tree holds is one the table doesn't have.
fn damage_the_index(paths: &StorePaths, keep_clean_marker: bool) {
    let conn = Connection::open(paths.state_db()).unwrap();
    conn.execute_batch(
        "PRAGMA writable_schema = ON;
         UPDATE sqlite_schema SET sql = 'CREATE INDEX probe_pad ON probe(seq)'
         WHERE name = 'probe_pad';
         PRAGMA writable_schema = OFF;",
    )
    .unwrap();
    if !keep_clean_marker {
        conn.execute("DELETE FROM store_meta WHERE key = 'clean_shutdown_at'", [])
            .unwrap();
    }
    drop(conn);
    // `quick_check` passes it: the startup check finds nothing.
    let conn = Connection::open(paths.state_db()).unwrap();
    let quick: String = conn
        .query_row("PRAGMA quick_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(quick, "ok");
}

fn reopen(paths: &StorePaths) -> Store {
    crate::open(paths, &test_seams(real_fs())).unwrap()
}

fn check(store: &Store) -> IntegrityCheck {
    store.check_integrity(&AtomicBool::new(false)).unwrap()
}

fn latch_path(paths: &StorePaths) -> PathBuf {
    paths.data_root().join(LATCH_FILE)
}

/// The published integrity copies under `recovery/`.
fn copies(paths: &StorePaths) -> Vec<PathBuf> {
    let mut found: Vec<PathBuf> = std::fs::read_dir(paths.recovery())
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .filter(|path| {
            path.file_name()
                .and_then(|name| name.to_str())
                .is_some_and(|name| name.starts_with("integrity-"))
        })
        .collect();
    found.sort();
    found
}

fn sha256(path: &Path) -> String {
    hex(&Sha256::digest(std::fs::read(path).unwrap()))
}

/// A user write, its error if refused.
fn try_write(store: &Store) -> Result<(), StoreError> {
    block_on(store.write(WritePriority::User, |tx| {
        tx.execute("INSERT INTO probe (pad) VALUES ('later')", [])
            .map_err(bug)?;
        Ok(())
    }))
}

fn probe_rows(store: &Store) -> i64 {
    reading(store, |tx| {
        tx.query_row("SELECT count(*) FROM probe", [], |row| row.get(0))
            .map_err(bug)
    })
}

#[test]
fn a_clean_store_is_clean_and_stays_writable() {
    let (_scratch, paths, store) = open_with_probe("integrity-clean");
    assert!(matches!(check(&store), IntegrityCheck::Clean));
    assert!(!latch_path(&paths).exists());
    assert!(copies(&paths).is_empty());
    assert_eq!(store.integrity_latch(), None);
    try_write(&store).unwrap();
    store.close().unwrap();
}

/// The finding: the store latches read-only, now and after a reopen,
/// and its database files are copied to `recovery/` as they were.
#[test]
fn a_finding_latches_the_store_and_copies_its_files() {
    for keep_clean_marker in [true, false] {
        let (_scratch, paths, store) = open_with_probe("integrity-finding");
        store.close().unwrap();
        damage_the_index(&paths, keep_clean_marker);
        let store = reopen(&paths);
        assert!(!store.writer.is_read_only());
        try_write(&store).unwrap();

        let IntegrityCheck::Finding(finding) = check(&store) else {
            panic!("the damaged index went unnoticed");
        };
        assert!(finding.detail.contains("probe_pad"), "{}", finding.detail);
        finding.latch.unwrap();
        let copy = finding.copy.unwrap();
        assert_eq!(copies(&paths), std::slice::from_ref(&copy));
        assert!(latch_path(&paths).is_file());

        // Every write is refused naming the finding; reads go on.
        match try_write(&store) {
            Err(StoreError::Corrupt { detail }) => assert!(detail.contains("probe_pad")),
            other => panic!("expected Corrupt, got {other:?}"),
        }
        assert_eq!(probe_rows(&store), 201);
        assert_eq!(store.integrity_latch().as_deref(), Some(&*finding.detail));
        assert!(matches!(
            store.check_integrity(&AtomicBool::new(false)),
            Err(StoreError::Corrupt { .. })
        ));

        // The copy holds the files as they are, and its manifest says so.
        let manifest: serde_json::Value =
            serde_json::from_slice(&std::fs::read(copy.join("manifest.json")).unwrap()).unwrap();
        assert_eq!(manifest["detail"], finding.detail.as_str());
        let files = manifest["files"].as_array().unwrap();
        assert_eq!(files[0]["name"], "state.sqlite");
        for file in files {
            let name = file["name"].as_str().unwrap();
            let copied = copy.join(name);
            assert_eq!(file["sha256"], sha256(&copied).as_str(), "{name}");
            assert_eq!(
                file["bytes"],
                std::fs::metadata(&copied).unwrap().len(),
                "{name}"
            );
        }
        assert_eq!(sha256(&copy.join("state.sqlite")), sha256(paths.state_db()));

        // Closing writes nothing: no clean-shutdown marker, no checkpoint.
        let wal = paths.data_root().join("state.sqlite-wal");
        let before = (sha256(paths.state_db()), wal.exists().then(|| sha256(&wal)));
        store.close().unwrap();
        let after = (sha256(paths.state_db()), wal.exists().then(|| sha256(&wal)));
        assert_eq!(before, after);

        // A reopen is latched too, on a connection that can't write,
        // and makes no second copy; it, its reads and its close leave
        // the database and its WAL as they were.
        let store = reopen(&paths);
        assert_eq!(store.integrity_latch().as_deref(), Some(&*finding.detail));
        assert!(store.writer.is_read_only());
        assert!(matches!(try_write(&store), Err(StoreError::Corrupt { .. })));
        assert_eq!(probe_rows(&store), 201);
        assert_eq!(copies(&paths), [copy]);
        store.close().unwrap();
        let reopened = (sha256(paths.state_db()), wal.exists().then(|| sha256(&wal)));
        assert_eq!(reopened, after);
    }
}

/// A write queued when the store latches fails; the one running
/// finishes before the latch returns, so the copy sees its commit.
#[test]
fn the_latch_fails_queued_writes_and_waits_for_the_running_one() {
    let (_scratch, _paths, store) = open_with_probe("integrity-queued");
    let store = Arc::new(store);
    let started = Arc::new(Barrier::new(2));
    let release = Arc::new(Barrier::new(2));
    let running = {
        let (started, release) = (Arc::clone(&started), Arc::clone(&release));
        store.write(WritePriority::User, move |tx| {
            started.wait();
            release.wait();
            tx.execute("INSERT INTO probe (pad) VALUES ('running')", [])
                .map_err(bug)?;
            Ok(())
        })
    };
    started.wait();
    let queued = store.write(WritePriority::Index, |tx| {
        tx.execute("INSERT INTO probe (pad) VALUES ('queued')", [])
            .map_err(bug)?;
        Ok(())
    });
    let latching = {
        let store = Arc::clone(&store);
        std::thread::spawn(move || store.writer.latch("a test finding"))
    };
    // The queued write fails while the running one still runs.
    match block_on(queued) {
        Err(StoreError::Corrupt { detail }) => assert!(detail.contains("a test finding")),
        other => panic!("expected Corrupt, got {other:?}"),
    }
    assert!(!latching.is_finished());
    release.wait();
    block_on(running).unwrap();
    latching.join().unwrap().unwrap();

    assert_eq!(probe_rows(&store), 201);
    assert!(matches!(try_write(&store), Err(StoreError::Corrupt { .. })));
    Arc::into_inner(store).unwrap().close().unwrap();
}

#[test]
fn a_stop_ends_the_check_and_records_nothing() {
    let (_scratch, paths, store) = open_with_probe("integrity-stop");
    // Raised before the check starts.
    assert!(matches!(
        store.check_integrity(&AtomicBool::new(true)).unwrap(),
        IntegrityCheck::Stopped
    ));
    // Raised under the running pragma: the progress handler ends it.
    let conn = open_connection(paths.state_db(), Access::ReadOnly, READER_PRAGMAS).unwrap();
    assert!(matches!(
        run_on(&conn, &AtomicBool::new(true)).unwrap(),
        Ran::Stopped
    ));
    assert!(matches!(
        run_on(&conn, &AtomicBool::new(false)).unwrap(),
        Ran::Clean
    ));
    drop(conn);
    assert!(!latch_path(&paths).exists());
    assert_eq!(store.integrity_latch(), None);
    try_write(&store).unwrap();
    store.close().unwrap();
}

/// A latch that can't be read is still a latch.
#[test]
fn an_unreadable_latch_still_opens_read_only() {
    let (_scratch, paths, store) = open_with_probe("integrity-unreadable");
    store.close().unwrap();
    for bytes in [
        &b"not json"[..],
        br#"{"format":2,"detected_at":1,"detail":"d","copy":"integrity-1-00000000"}"#,
        br#"{"format":1,"detected_at":1,"detail":"d","copy":"../outside"}"#,
    ] {
        std::fs::write(latch_path(&paths), bytes).unwrap();
        let store = reopen(&paths);
        let latch = store.integrity_latch().unwrap();
        assert!(latch.contains("can't be read"), "{latch}");
        assert!(matches!(try_write(&store), Err(StoreError::Corrupt { .. })));
        store.close().unwrap();
        assert!(copies(&paths).is_empty());
    }
}

/// A latch whose copy was never published (a crash, or no space then)
/// makes it at the next open, over any unfinished attempt.
#[test]
fn a_latched_open_makes_a_missing_copy() {
    let (_scratch, paths, store) = open_with_probe("integrity-recopy");
    store.close().unwrap();
    damage_the_index(&paths, true);
    let store = reopen(&paths);
    let IntegrityCheck::Finding(finding) = check(&store) else {
        panic!("no finding");
    };
    let copy = finding.copy.unwrap();
    store.close().unwrap();
    // As if the copy never finished: unpublished, half-written.
    let unfinished = paths.recovery().join(format!(
        ".{}.tmp",
        copy.file_name().unwrap().to_str().unwrap()
    ));
    std::fs::rename(&copy, &unfinished).unwrap();
    std::fs::remove_file(unfinished.join("manifest.json")).unwrap();

    let store = reopen(&paths);
    assert_eq!(copies(&paths), std::slice::from_ref(&copy));
    assert!(copy.join("manifest.json").is_file());
    assert!(!unfinished.exists());
    store.close().unwrap();
}

/// Migrating writes: a latched store an older version left is `Corrupt`.
#[test]
fn a_latched_store_that_needs_migrating_does_not_open() {
    let (_scratch, paths, store) = open_with_probe("integrity-migrate");
    store.close().unwrap();
    std::fs::write(
        latch_path(&paths),
        br#"{"format":1,"detected_at":1,"detail":"an old finding","copy":"integrity-1-00000000"}"#,
    )
    .unwrap();
    let migrations = extra_migration(|_, _| Ok(()));
    let opened = crate::store::open_with(
        &paths,
        &test_seams(real_fs()),
        crate::store::WRITER_PRAGMAS,
        READER_PRAGMAS,
        &crate::store::PINNED_SQLITE,
        &migrations,
    );
    match opened {
        Err(StoreError::Corrupt { detail }) => {
            assert!(detail.contains("needs migrating"), "{detail}");
            assert!(detail.contains("an old finding"), "{detail}");
        }
        other => panic!("expected Corrupt, got {other:?}"),
    }
}

/// A latch that can't be written leaves the store read-only in this
/// process, says so, and still copies the evidence.
#[test]
fn a_latch_that_cannot_be_written_is_reported() {
    let (_scratch, paths, store) = open_with_probe("integrity-unwritable");
    store.close().unwrap();
    damage_the_index(&paths, true);
    let store = reopen(&paths);
    // A directory where the latch goes: the rename onto it fails.
    std::fs::create_dir(latch_path(&paths)).unwrap();
    std::fs::write(latch_path(&paths).join("occupied"), b"").unwrap();

    let IntegrityCheck::Finding(finding) = check(&store) else {
        panic!("no finding");
    };
    assert!(finding.latch.is_err(), "{:?}", finding.latch);
    finding.copy.unwrap();
    assert!(matches!(try_write(&store), Err(StoreError::Corrupt { .. })));
    // No temporary latch is left behind.
    let leftovers: Vec<_> = std::fs::read_dir(paths.data_root())
        .unwrap()
        .filter_map(|entry| entry.unwrap().file_name().into_string().ok())
        .filter(|name| name.starts_with(".integrity-latch-"))
        .collect();
    assert!(leftovers.is_empty(), "{leftovers:?}");
    store.close().unwrap();
}

#[test]
fn the_writer_thread_cannot_check_integrity() {
    let (_scratch, _paths, store) = open_with_probe("integrity-own-writer");
    let store = Arc::new(store);
    let on_writer = Arc::clone(&store);
    let error = block_on(store.write(WritePriority::User, move |_tx| {
        Ok(on_writer
            .check_integrity(&AtomicBool::new(false))
            .unwrap_err())
    }))
    .unwrap();
    assert!(matches!(error, StoreError::Internal { .. }), "{error:?}");
    Arc::into_inner(store).unwrap().close().unwrap();
}

#[test]
fn copy_names_are_only_the_ones_the_check_makes() {
    assert!(valid_copy_name("integrity-1735689600000-0a1b2c3d"));
    for name in [
        "integrity-1-0A1B2C3D",
        "integrity-1-0a1b2c3",
        "integrity-+1-0a1b2c3d",
        "integrity-x-0a1b2c3d",
        "../integrity-1-0a1b2c3d",
        "integrity-1-0a1b2c3d/..",
        "object-1-0a1b2c3d",
    ] {
        assert!(!valid_copy_name(name), "{name}");
    }
}

/// A latched root whose database is gone opens nothing: the writable
/// connection would create a fresh database over the evidence.
#[test]
fn a_latched_store_without_its_database_does_not_open() {
    let (_scratch, paths, store) = open_with_probe("integrity-no-database");
    store.close().unwrap();
    std::fs::write(
        latch_path(&paths),
        br#"{"format":1,"detected_at":1,"detail":"an old finding","copy":"integrity-1-00000000"}"#,
    )
    .unwrap();
    for name in ["state.sqlite", "state.sqlite-wal", "state.sqlite-shm"] {
        let _ = std::fs::remove_file(paths.state_db().with_file_name(name));
    }
    match crate::open(&paths, &test_seams(real_fs())) {
        Err(StoreError::Corrupt { detail }) => {
            assert!(detail.contains("database is missing or empty"), "{detail}");
            assert!(detail.contains("an old finding"), "{detail}");
        }
        other => panic!("expected Corrupt, got {:?}", other.map(|_| ())),
    }
    assert!(!paths.state_db().exists());
}

/// Damage that keeps the check's own connection from opening (a broken
/// header) is a finding, latched and copied like one the check reports.
#[test]
fn a_header_the_check_cannot_open_is_a_finding() {
    let (_scratch, paths, store) = open_with_probe("integrity-header");
    // Everything into the database file, so a new connection reads the
    // header from it, not from the WAL.
    let raw = Connection::open(paths.state_db()).unwrap();
    raw.busy_timeout(std::time::Duration::from_secs(5)).unwrap();
    let checkpointed = (0..100).any(|_| {
        let busy: i64 = raw
            .query_row("PRAGMA wal_checkpoint(TRUNCATE)", [], |row| row.get(0))
            .unwrap();
        busy == 0 || {
            std::thread::sleep(std::time::Duration::from_millis(20));
            false
        }
    });
    assert!(checkpointed, "the WAL stayed busy");
    drop(raw);
    let mut bytes = std::fs::read(paths.state_db()).unwrap();
    bytes[..16].copy_from_slice(b"not a database!\0");
    std::fs::write(paths.state_db(), &bytes).unwrap();

    let finding = match store.check_integrity(&AtomicBool::new(false)).unwrap() {
        IntegrityCheck::Finding(finding) => finding,
        other => panic!("expected a finding, got {other:?}"),
    };
    assert!(
        finding.detail.contains("not a readable database"),
        "{}",
        finding.detail
    );
    finding.latch.unwrap();
    finding.copy.unwrap();
    assert!(latch_path(&paths).exists());
    assert!(matches!(try_write(&store), Err(StoreError::Corrupt { .. })));
    store.close().unwrap();
}

/// Randomness that runs out once `.0` is raised.
struct Exhaustible(Arc<AtomicBool>);

impl crate::Random for Exhaustible {
    fn fill(&self, dest: &mut [u8]) -> io::Result<()> {
        if self.0.load(Ordering::SeqCst) {
            return Err(io::Error::other("no entropy"));
        }
        dest.fill(7);
        Ok(())
    }
}

/// Randomness running out after the writer latched still records the
/// finding: the copy takes the fixed suffix, and the latch file,
/// whose temporary name needs randomness, is reported unwritten.
#[test]
fn a_finding_without_randomness_still_latches_and_copies() {
    let (_scratch, paths, store) = open_with_probe("integrity-no-entropy");
    store.close().unwrap();
    damage_the_index(&paths, true);
    let exhausted = Arc::new(AtomicBool::new(false));
    let base = test_seams(real_fs());
    let seams = crate::Seams::new(
        Arc::clone(base.fs_arc()),
        Arc::clone(base.clock_arc()),
        Arc::new(Exhaustible(Arc::clone(&exhausted))),
    );
    let store = crate::open(&paths, &seams).unwrap();
    exhausted.store(true, Ordering::SeqCst);

    let IntegrityCheck::Finding(finding) = check(&store) else {
        panic!("no finding");
    };
    assert!(finding.latch.is_err(), "{:?}", finding.latch);
    let copy = finding.copy.unwrap();
    let name = copy.file_name().unwrap().to_str().unwrap();
    assert!(name.ends_with(&format!("-{FALLBACK_NONCE}")), "{name}");
    assert!(valid_copy_name(name), "{name}");
    assert!(matches!(try_write(&store), Err(StoreError::Corrupt { .. })));
    store.close().unwrap();
}
