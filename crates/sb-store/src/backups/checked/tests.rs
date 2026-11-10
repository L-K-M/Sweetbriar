//! M1.8h4b: each retained generation checked in place against its
//! manifest, the verdict recorded on its row as bookkeeping.

use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};
use std::sync::{Arc, Barrier};

use sb_test_support::{SeededRandom, real_fs};
use sb_types::time::Timestamp;

use super::{GenerationCheck, GenerationVerdict};
use crate::backups::{BackupDestination, Pauses};
use crate::changes::CONTENT_CHANGES;
use crate::objects::{ObjectId, ObjectStore, ReservationRelease, release_reservation};
use crate::schema::bug;
use crate::testing::{FIXED_NOW_MS, reading, scratch_paths, writing};
use crate::{Seams, Store, StoreError, StorePaths, WritePriority};

const PAYLOAD: &[u8] = b"Subject: keep me\r\n\r\nthe archived bytes\r\n";
const HOUR_MS: i64 = 60 * 60 * 1000;
const WEEK_MS: i64 = 7 * 24 * HOUR_MS;

/// A clock the tests move by weeks.
struct MovableClock(AtomicI64);

impl crate::Clock for MovableClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0.load(Ordering::SeqCst))
    }
}

struct Fixture {
    _scratch: sb_test_support::ScratchDir,
    paths: StorePaths,
    clock: Arc<MovableClock>,
    store: Store,
}

fn open(label: &str) -> Fixture {
    let (scratch, paths) = scratch_paths(label);
    let clock = Arc::new(MovableClock(FIXED_NOW_MS.into()));
    let seams = Seams::new(real_fs(), clock.clone(), Arc::new(SeededRandom::new(7)));
    let store = crate::open(&paths, &seams).unwrap();
    Fixture {
        _scratch: scratch,
        paths,
        clock,
        store,
    }
}

/// One published, referenced object.
fn referenced_object(store: &Store) -> ObjectId {
    let publication = store.objects().publish(PAYLOAD).unwrap();
    let id = *publication.id();
    let statement = format!(
        "INSERT OR IGNORE INTO acquisitions
         (content_sha256, byte_length, object_state, first_acquired_at)
         VALUES ('{}', {}, 'published', 0)",
        id.sha256_hex(),
        id.byte_length()
    );
    writing(store, move |tx| {
        tx.execute_batch(&statement).map_err(bug)?;
        release_reservation(tx, &publication, ReservationRelease::Required)
    });
    id
}

/// Where a case finds the file it changes in a generation.
type FileOf = fn(&Path, &ObjectId) -> PathBuf;

/// One complete generation holding one object: its id and directory.
fn generation(store: &Store, destination: BackupDestination) -> (String, PathBuf, ObjectId) {
    let id = referenced_object(store);
    let snapshot = store.backup_snapshot(destination).unwrap();
    let location = snapshot.location().to_path_buf();
    let receipt = store.complete_backup(snapshot).unwrap();
    (receipt.generation_id().to_owned(), location, id)
}

fn object_copy(location: &Path, id: &ObjectId) -> PathBuf {
    let sha = id.sha256_hex();
    location
        .join("objects/sha256")
        .join(&sha[..2])
        .join(&sha[2..4])
        .join(&sha)
}

fn check(store: &Store, generation: &str) -> GenerationCheck {
    store
        .verify_generation(generation, &AtomicBool::new(false))
        .unwrap()
}

/// The row's `(verified_at, verification)`.
fn recorded(store: &Store, generation: &str) -> (Option<i64>, Option<String>) {
    let generation = generation.to_owned();
    reading(store, move |tx| {
        tx.query_row(
            "SELECT verified_at, verification FROM backup_generations
             WHERE generation_id = ?1",
            [generation],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(bug)
    })
}

fn content_changes(store: &Store) -> u64 {
    reading(store, |tx| crate::changes::read(tx, CONTENT_CHANGES))
}

#[test]
fn an_intact_generation_is_recorded_and_not_due_again_until_next_week() {
    let f = open("checked-intact");
    let (generation, _, _) = generation(&f.store, BackupDestination::DataRoot);
    assert_eq!(
        f.store.generations_to_verify().unwrap(),
        std::slice::from_ref(&generation)
    );
    let before = content_changes(&f.store);

    assert_eq!(
        check(&f.store, &generation),
        GenerationCheck::Recorded(GenerationVerdict::Intact)
    );
    assert_eq!(
        recorded(&f.store, &generation),
        (Some(FIXED_NOW_MS), Some("intact".to_owned()))
    );
    // Bookkeeping: the store doesn't become due for a backup.
    assert_eq!(content_changes(&f.store), before);
    assert!(f.store.generations_to_verify().unwrap().is_empty());

    f.clock.0.store(FIXED_NOW_MS + WEEK_MS, Ordering::SeqCst);
    assert_eq!(f.store.generations_to_verify().unwrap(), [generation]);
}

/// Never-checked generations come first, then the longest unchecked;
/// in-progress ones never.
#[test]
fn the_order_is_never_checked_then_longest_unchecked() {
    let f = open("checked-order");
    // An hour apart, so hourly retention keeps all three.
    let mut generations = Vec::new();
    for hour in 0..3 {
        f.clock
            .0
            .store(FIXED_NOW_MS + hour * HOUR_MS, Ordering::SeqCst);
        generations.push(generation(&f.store, BackupDestination::DataRoot).0);
    }
    let [first, second, third] = <[String; 3]>::try_from(generations).unwrap();
    check(&f.store, &second);
    f.clock
        .0
        .store(FIXED_NOW_MS + 3 * HOUR_MS, Ordering::SeqCst);
    check(&f.store, &first);
    let _in_progress = f
        .store
        .backup_snapshot(BackupDestination::DataRoot)
        .unwrap();

    f.clock.0.store(FIXED_NOW_MS + WEEK_MS, Ordering::SeqCst);
    assert_eq!(
        f.store.generations_to_verify().unwrap(),
        [third, second, first]
    );
}

#[test]
fn damaged_files_are_damaged() {
    let cases: [(&str, FileOf); 3] = [
        ("object", object_copy),
        ("manifest", |location, _| location.join("manifest.json")),
        ("database", |location, _| location.join("state.sqlite")),
    ];
    for (name, file) in cases {
        for how in ["appended", "rewritten", "truncated"] {
            let f = open("checked-damaged");
            let (generation, location, id) = generation(&f.store, BackupDestination::DataRoot);
            let path = file(&location, &id);
            let mut bytes = std::fs::read(&path).unwrap();
            match how {
                "appended" => bytes.push(b' '),
                "rewritten" => *bytes.last_mut().unwrap() ^= 1,
                _ => bytes.truncate(bytes.len() / 2),
            }
            std::fs::write(&path, bytes).unwrap();

            assert_eq!(
                check(&f.store, &generation),
                GenerationCheck::Recorded(GenerationVerdict::Damaged),
                "{name} {how}"
            );
            assert_eq!(
                recorded(&f.store, &generation).1.as_deref(),
                Some("damaged")
            );
        }
    }
}

#[test]
fn absent_files_are_missing() {
    let cases: [FileOf; 4] = [
        object_copy,
        |location, _| location.join("manifest.json"),
        |location, _| location.join("state.sqlite"),
        |location, _| location.to_path_buf(),
    ];
    for file in cases {
        let f = open("checked-missing");
        let (generation, location, id) = generation(&f.store, BackupDestination::DataRoot);
        let path = file(&location, &id);
        if path.is_dir() {
            std::fs::remove_dir_all(&path).unwrap();
        } else {
            std::fs::remove_file(&path).unwrap();
        }

        assert_eq!(
            check(&f.store, &generation),
            GenerationCheck::Recorded(GenerationVerdict::Missing),
            "{}",
            path.display()
        );
        assert_eq!(
            recorded(&f.store, &generation).1.as_deref(),
            Some("missing")
        );
    }
}

/// A destination that isn't mounted records nothing: the next run
/// tries again, and an intact generation is intact once it's back.
#[test]
fn an_absent_destination_records_nothing() {
    let f = open("checked-unmounted");
    let destination = f
        .paths
        .data_root()
        .parent()
        .unwrap()
        .join("external")
        .join("mount");
    let (generation, _, _) =
        generation(&f.store, BackupDestination::Directory(destination.clone()));
    let aside = destination.with_file_name("unmounted");
    std::fs::rename(&destination, &aside).unwrap();

    assert_eq!(
        check(&f.store, &generation),
        GenerationCheck::DestinationAbsent
    );
    assert_eq!(recorded(&f.store, &generation), (None, None));
    assert_eq!(
        f.store.generations_to_verify().unwrap(),
        std::slice::from_ref(&generation)
    );

    std::fs::rename(&aside, &destination).unwrap();
    assert_eq!(
        check(&f.store, &generation),
        GenerationCheck::Recorded(GenerationVerdict::Intact)
    );
}

/// A check waits for no backup: while one runs it's a `Conflict`.
/// An abandoned `in_progress` row, or an unknown id, is gone.
#[test]
fn unknown_and_unfinished_generations_are_gone() {
    let f = open("checked-gone");
    let (complete, _, _) = generation(&f.store, BackupDestination::DataRoot);
    let snapshot = f
        .store
        .backup_snapshot(BackupDestination::DataRoot)
        .unwrap();
    let unfinished = snapshot.generation_id().to_owned();
    let error = f
        .store
        .verify_generation(&complete, &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(error, StoreError::Conflict { .. }), "{error:?}");
    drop(snapshot);

    assert_eq!(check(&f.store, &unfinished), GenerationCheck::Gone);
    assert_eq!(
        check(&f.store, "00000000-0000-4000-8000-00000000dead"),
        GenerationCheck::Gone
    );
    assert_eq!(recorded(&f.store, &unfinished), (None, None));
    assert_eq!(recorded(&f.store, &complete), (None, None));
}

#[test]
fn a_stopped_check_records_nothing() {
    let f = open("checked-stopped");
    let (generation, _, _) = generation(&f.store, BackupDestination::DataRoot);
    let stop = AtomicBool::new(true);
    assert_eq!(
        f.store.verify_generation(&generation, &stop).unwrap(),
        GenerationCheck::Stopped
    );
    assert_eq!(recorded(&f.store, &generation), (None, None));
}

/// A later check replaces the verdict: a restored file reads intact.
#[test]
fn a_later_check_replaces_the_verdict() {
    let f = open("checked-again");
    let (generation, location, id) = generation(&f.store, BackupDestination::DataRoot);
    let path = object_copy(&location, &id);
    let aside = path.with_extension("aside");
    std::fs::rename(&path, &aside).unwrap();
    check(&f.store, &generation);
    assert_eq!(
        recorded(&f.store, &generation).1.as_deref(),
        Some("missing")
    );

    std::fs::rename(&aside, &path).unwrap();
    f.clock.0.store(FIXED_NOW_MS + WEEK_MS, Ordering::SeqCst);
    check(&f.store, &generation);
    assert_eq!(
        recorded(&f.store, &generation),
        (Some(FIXED_NOW_MS + WEEK_MS), Some("intact".to_owned()))
    );
}

#[test]
fn the_schema_refuses_a_half_or_unknown_verdict() {
    let f = open("checked-schema");
    let (generation, _, _) = generation(&f.store, BackupDestination::DataRoot);
    for set in [
        "verified_at = 1",
        "verification = 'intact'",
        "verified_at = 1, verification = 'fine'",
    ] {
        let statement =
            format!("UPDATE backup_generations SET {set} WHERE generation_id = '{generation}'");
        let result = sb_test_support::block_on(f.store.write(WritePriority::User, move |tx| {
            tx.execute_batch(&statement).map_err(bug)
        }));
        assert!(result.is_err(), "{set}");
    }
}

/// Recording awaits the writer: on the writer thread it's refused, never
/// a thread parked on its own queue.
#[test]
fn the_writer_thread_cannot_check_a_generation() {
    let f = open("checked-own-writer");
    let (generation, _, _) = generation(&f.store, BackupDestination::DataRoot);
    let store = Arc::new(f.store);
    let on_writer = Arc::clone(&store);
    let error = sb_test_support::block_on(store.write(WritePriority::User, move |_tx| {
        Ok(on_writer
            .verify_generation(&generation, &AtomicBool::new(false))
            .unwrap_err())
    }))
    .unwrap();
    assert!(matches!(error, StoreError::Internal { .. }), "{error:?}");
    Arc::into_inner(store).unwrap().close().unwrap();
}

/// A directory where publication left a regular file, or a file where
/// it left a directory, is damaged: publication creates only those two.
#[test]
fn another_kind_of_entry_is_damaged() {
    let cases: [(&str, FileOf); 4] = [
        ("object", object_copy),
        ("manifest", |location, _| location.join("manifest.json")),
        ("database", |location, _| location.join("state.sqlite")),
        ("generation", |location, _| location.to_path_buf()),
    ];
    for (name, file) in cases {
        let f = open("checked-entry-kind");
        let (generation, location, id) = generation(&f.store, BackupDestination::DataRoot);
        let path = file(&location, &id);
        let aside = path.with_extension("aside");
        std::fs::rename(&path, &aside).unwrap();
        if aside.is_dir() {
            std::fs::write(&path, b"").unwrap();
        } else {
            std::fs::create_dir(&path).unwrap();
        }

        assert_eq!(
            check(&f.store, &generation),
            GenerationCheck::Recorded(GenerationVerdict::Damaged),
            "{name}"
        );
    }
}

/// A symlink is refused by the confined open with an error std doesn't
/// name yet: an I/O error, nothing recorded, tried again next run.
#[test]
fn a_symlinked_entry_is_an_error_and_records_nothing() {
    let f = open("checked-symlink");
    let (generation, location, _) = generation(&f.store, BackupDestination::DataRoot);
    let path = location.join("manifest.json");
    let aside = path.with_extension("aside");
    std::fs::rename(&path, &aside).unwrap();
    std::os::unix::fs::symlink(&aside, &path).unwrap();

    let error = f
        .store
        .verify_generation(&generation, &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(error, StoreError::Unavailable { .. }), "{error:?}");
    assert_eq!(recorded(&f.store, &generation), (None, None));
}

/// Copy `from` into a new directory `to`, recursively.
fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir(to).unwrap();
    for entry in std::fs::read_dir(from).unwrap() {
        let entry = entry.unwrap();
        let target = to.join(entry.file_name());
        if entry.file_type().unwrap().is_dir() {
            copy_tree(&entry.path(), &target);
        } else {
            std::fs::copy(entry.path(), &target).unwrap();
        }
    }
}

/// Park a check before its identity re-check, run `between`, then let
/// it finish.
fn parked_check(
    label: &str,
    between: impl FnOnce(&Path, &AtomicBool),
) -> (GenerationCheck, Option<String>) {
    let Fixture {
        _scratch, store, ..
    } = open(label);
    let (generation, location, _) = generation(&store, BackupDestination::DataRoot);
    let store = Arc::new(store);
    let stop = Arc::new(AtomicBool::new(false));
    let gate = Arc::new(Barrier::new(2));
    store.backups.set_pauses(Pauses {
        before_identity_recheck: Some(gate.clone()),
        ..Pauses::default()
    });
    let background = {
        let (store, stop, generation) = (Arc::clone(&store), Arc::clone(&stop), generation.clone());
        std::thread::spawn(move || store.verify_generation(&generation, &stop).unwrap())
    };
    gate.wait();
    between(&location, &stop);
    gate.wait();
    let result = background.join().unwrap();
    (result, recorded(&store, &generation).1)
}

/// The same bytes in a new directory at the generation's name, swapped
/// in while it was read: not the directory the check read.
#[test]
fn a_directory_replaced_while_it_is_read_is_damaged() {
    let (result, recorded) = parked_check("checked-swapped-dir", |location, _| {
        let aside = location.with_extension("aside");
        std::fs::rename(location, &aside).unwrap();
        copy_tree(&aside, location);
    });
    assert_eq!(
        result,
        GenerationCheck::Recorded(GenerationVerdict::Damaged)
    );
    assert_eq!(recorded.as_deref(), Some("damaged"));
}

/// A stop raised after the last file still records nothing.
#[test]
fn a_stop_after_the_last_file_records_nothing() {
    let (result, recorded) = parked_check("checked-late-stop", |_, stop| {
        stop.store(true, Ordering::Release);
    });
    assert_eq!(result, GenerationCheck::Stopped);
    assert_eq!(recorded, None);
}
