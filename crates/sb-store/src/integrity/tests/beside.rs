//! M1.8h5b4: a latched store verifies a backup beside its frozen root.
//! The scratch directory, its owner row and the object ledger live in
//! `<data root>.verifier`, under the ownership protocol a sound store
//! follows in its own root, so a restore can start while latched.

use std::ops::ControlFlow;
use std::panic::{AssertUnwindSafe, catch_unwind};

use super::frozen::tree;
use super::*;
use crate::BackupImageInspection;
use crate::backups::BackupDestination;
use crate::{ObjectStore, StorePaths};

const STRANGER: &str = "00000000-0000-4000-8000-0000000000e1";
const STRANGER_TOKEN: &str = "00000000-0000-4000-8000-0000000000e2";

/// `<data root>.verifier`.
fn beside(paths: &StorePaths) -> PathBuf {
    let mut name = paths.data_root().file_name().unwrap().to_os_string();
    name.push(".verifier");
    paths.data_root().with_file_name(name)
}

/// The scratch directories and markers in the sibling.
fn scratch_entries(paths: &StorePaths) -> Vec<String> {
    let mut found: Vec<String> = std::fs::read_dir(beside(paths))
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .filter(|name| !name.starts_with("ledger.sqlite"))
        .collect();
    found.sort();
    found
}

/// The sibling's rows: owners, then ledger entries.
fn rows(paths: &StorePaths) -> (i64, i64) {
    let conn = Connection::open(beside(paths).join("ledger.sqlite")).unwrap();
    let count = |table: &str| {
        conn.query_row(&format!("SELECT count(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
    };
    (count("verifier_scratch"), count("verifier_objects"))
}

/// A store holding one object and a complete generation of it, then
/// damaged and latched by the full check. Returns the generation's id
/// and directory.
fn latched(label: &str) -> (ScratchDir, StorePaths, Store, String, PathBuf) {
    let (scratch, paths, store) = open_with_probe(label);
    store.objects().publish(&b"a backed-up object"[..]).unwrap();
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let complete = store.complete_backup(snapshot).unwrap();
    let generation = complete.generation_id().to_owned();
    let location = complete.location().to_path_buf();
    store.close().unwrap();
    damage_the_index(&paths, true);
    let store = reopen(&paths);
    let IntegrityCheck::Finding(finding) = check(&store) else {
        panic!("no finding");
    };
    finding.latch.unwrap();
    (scratch, paths, store, generation, location)
}

fn verify(store: &Store, generation: &str) -> Result<BackupImageInspection, StoreError> {
    store.verify_recorded_backup(generation, &AtomicBool::new(false))
}

fn assert_verified(store: &Store, generation: &str) {
    let verified = verify(store, generation);
    assert!(
        matches!(verified, Ok(BackupImageInspection::Inspected { .. })),
        "{verified:?}"
    );
}

/// The verification runs, the data root stays as it was, file for
/// file, and nothing is left beside it but the empty ledger: at the
/// finding, and again after a latched reopen.
#[test]
fn a_latched_store_verifies_a_backup_beside_its_root() {
    let (_scratch, paths, store, generation, _) = latched("integrity-beside");
    assert!(!beside(&paths).exists());

    let before = tree(&paths);
    assert_verified(&store, &generation);
    assert_eq!(tree(&paths), before, "verifying touched the data root");
    assert_eq!(scratch_entries(&paths), Vec::<String>::new());
    assert_eq!(rows(&paths), (0, 0));
    store.close().unwrap();

    // The sibling and its ledger are there already.
    let store = reopen(&paths);
    let before = tree(&paths);
    assert_verified(&store, &generation);
    assert_eq!(tree(&paths), before, "verifying touched the data root");
    assert_eq!(scratch_entries(&paths), Vec::<String>::new());
    assert_eq!(rows(&paths), (0, 0));
    store.close().unwrap();
}

/// A verification cut short leaves its scratch directory and its owner
/// row in the sibling; the next one proves the directory its own,
/// removes both and goes on.
#[test]
fn the_next_verification_cleans_up_one_cut_short_beside_the_root() {
    let (_scratch, paths, store, generation, location) = latched("integrity-beside-leftover");
    let image = location.join("state.sqlite");
    let bytes = std::fs::read(&image).unwrap();
    let digest: [u8; 32] = Sha256::digest(&bytes).into();

    // The copy is under way when its progress callback panics.
    let cut = catch_unwind(AssertUnwindSafe(|| {
        store.inspect_backup_image(
            &location,
            digest,
            bytes.len() as u64,
            &AtomicBool::new(false),
            |copied| -> ControlFlow<()> {
                assert_eq!(copied, 0, "cut short");
                ControlFlow::Continue(())
            },
        )
    }));
    assert!(cut.is_err(), "the first progress report after 0 panics");
    let left = scratch_entries(&paths);
    assert_eq!(left.len(), 2, "{left:?}");
    assert!(left[0].starts_with(".verifier-") && left[0].ends_with(".owner"));
    assert!(left[1].starts_with("verifier-"));
    assert_eq!(rows(&paths).0, 1);
    // As a cut later in the verification leaves it: the ledger holds
    // an object of the dead scratch.
    let id = left[1].trim_start_matches("verifier-").to_owned();
    Connection::open(beside(&paths).join("ledger.sqlite"))
        .unwrap()
        .execute(
            "INSERT INTO verifier_objects (scratch_id, sha256, bytes) VALUES (?1, ?2, ?3)",
            rusqlite::params![id, [7u8; 32], 9u64.to_be_bytes()],
        )
        .unwrap();
    assert_eq!(rows(&paths), (1, 1));

    let before = tree(&paths);
    assert_verified(&store, &generation);
    assert_eq!(tree(&paths), before, "the cleanup touched the data root");
    assert_eq!(scratch_entries(&paths), Vec::<String>::new());
    assert_eq!(rows(&paths), (0, 0));
    store.close().unwrap();
}

/// A row recorded under another incarnation proves nothing about this
/// store's scratch: its directory and the row stay, and verification
/// goes on beside them.
#[test]
fn a_row_of_another_incarnation_authorizes_no_cleanup_beside_the_root() {
    let (_scratch, paths, store, generation, _) = latched("integrity-beside-stranger");
    assert_verified(&store, &generation);
    let directory = beside(&paths).join(format!("verifier-{STRANGER}"));
    std::fs::create_dir(&directory).unwrap();
    Connection::open(beside(&paths).join("ledger.sqlite"))
        .unwrap()
        .execute(
            "INSERT INTO verifier_scratch VALUES (?1, ?2, 'another incarnation')",
            [STRANGER, STRANGER_TOKEN],
        )
        .unwrap();

    assert_verified(&store, &generation);

    assert!(directory.is_dir());
    assert_eq!(rows(&paths), (1, 0));
    store.close().unwrap();
}

/// An empty ledger file, which is what a process that died creating
/// it leaves (SQLite commits the first page or rolls it back), is made
/// again, so a cut there never keeps a latched store from its restore.
/// Bytes that are no database are refused under the ledger's name and
/// stay.
#[test]
fn an_empty_ledger_is_made_again_and_one_that_is_no_database_refused() {
    let (_scratch, paths, store, generation, _) = latched("integrity-beside-torn");
    let ledger = beside(&paths).join("ledger.sqlite");
    std::fs::create_dir_all(beside(&paths)).unwrap();
    std::fs::write(&ledger, b"").unwrap();
    assert_verified(&store, &generation);
    assert_eq!(rows(&paths), (0, 0));

    for foreign in [b"SQLite fo".to_vec(), vec![0x5a; 4096]] {
        std::fs::remove_dir_all(beside(&paths)).unwrap();
        std::fs::create_dir_all(beside(&paths)).unwrap();
        std::fs::write(&ledger, &foreign).unwrap();
        match verify(&store, &generation) {
            Err(StoreError::Corrupt { detail }) => {
                assert!(detail.contains("ledger.sqlite"), "{detail}");
                assert!(detail.contains("is not a verifier ledger"), "{detail}");
            }
            other => panic!("expected Corrupt, got {other:?}"),
        }
        assert_eq!(std::fs::read(&ledger).unwrap(), foreign);
    }
    store.close().unwrap();
}

/// The ledger decides whether a backup reconciles, so a database that
/// holds anything but the two tables is refused, as is a sibling that
/// is no directory. Neither is removed or repaired.
#[test]
fn a_sibling_that_is_no_verifier_home_is_refused() {
    let (_scratch, paths, store, generation, _) = latched("integrity-beside-foreign");
    assert_verified(&store, &generation);
    let ledger = beside(&paths).join("ledger.sqlite");
    Connection::open(&ledger)
        .unwrap()
        .execute_batch(
            "CREATE TRIGGER mark AFTER INSERT ON verifier_objects BEGIN
                 UPDATE verifier_objects SET referenced = 1;
             END;",
        )
        .unwrap();

    match verify(&store, &generation) {
        Err(StoreError::Corrupt { detail }) => {
            assert!(detail.contains("is not a verifier ledger"), "{detail}");
        }
        other => panic!("expected Corrupt, got {other:?}"),
    }
    assert!(ledger.is_file());

    std::fs::remove_dir_all(beside(&paths)).unwrap();
    std::fs::write(beside(&paths), b"not a directory").unwrap();
    match verify(&store, &generation) {
        Err(StoreError::Corrupt { detail }) => {
            assert!(detail.contains("is not a directory"), "{detail}");
        }
        other => panic!("expected Corrupt, got {other:?}"),
    }
    assert_eq!(std::fs::read(beside(&paths)).unwrap(), b"not a directory");

    // A symlink is no sibling of this store's, wherever it leads: the
    // ledger is never opened through it, into the frozen root least of
    // all.
    std::fs::remove_file(beside(&paths)).unwrap();
    let elsewhere = paths.data_root().join("recovery");
    let before = tree(&paths);
    std::os::unix::fs::symlink(&elsewhere, beside(&paths)).unwrap();
    match verify(&store, &generation) {
        Err(StoreError::Corrupt { detail }) => {
            assert!(detail.contains("is not a directory"), "{detail}");
        }
        other => panic!("expected Corrupt, got {other:?}"),
    }
    assert_eq!(tree(&paths), before, "the link was followed into the root");
    assert!(beside(&paths).is_symlink());
    store.close().unwrap();
}
