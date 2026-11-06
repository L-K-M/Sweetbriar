//! Backup snapshot proof (spec/03 "Backup generations" steps 1–3):
//! the `in_progress` row that freezes GC, the consistent point-in-time
//! copy while writes continue, pins for exactly the objects the
//! snapshot references, and the durability errors that must surface —
//! never a silent partial, never a `complete` generation. Everything
//! runs through `Store::backup_snapshot` or the real GC path.

use std::env;
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Barrier};

use rusqlite::Connection;
use sha2::{Digest, Sha256};

use sb_test_support::{
    CutFs, FaultFs, FaultKind, FixedClock, FsOp, ScratchDir, SeededRandom, block_on, real_fs,
    run_crash_cut_points, run_crash_cuts,
};
use sb_types::fs::Fs;
use sb_types::time::Timestamp;

use super::{BackupDestination, BackupReceipt, Backups, Pauses};
use crate::objects::{ObjectId, ObjectStore, ReservationRelease, release_reservation};
use crate::schema::bug;
use crate::testing::{FIXED_NOW_MS, reading, scratch_paths, test_seams, writing};
use crate::{GC_GRACE_PERIOD, GcGate, GcReport, Store, StoreError, StorePaths, Uuid};

// One account, identity, draft and revision: the schema seeds its
// own `local_archive` account and forbids a second.
const ACCOUNT: &str = "00000000-0000-4000-8000-0000000000a1";
const IDENTITY: &str = "00000000-0000-4000-8000-0000000000a2";
const DRAFT: &str = "00000000-0000-4000-8000-0000000000a3";
const REVISION: &str = "00000000-0000-4000-8000-0000000000a4";
const PAYLOAD: &[u8] = b"Subject: keep me\r\n\r\nthe archived bytes\r\n";

/// The common prelude: a scratch store over the real filesystem.
pub(super) fn open(label: &str) -> (ScratchDir, StorePaths, Store) {
    let (scratch, paths) = scratch_paths(label);
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    (scratch, paths, store)
}

// --- the fixture rows spec/03 names, written through the writer ---

/// One batch in one writer commit; values are synthetic literals.
pub(super) fn sql(store: &Store, statement: String) {
    writing(store, move |tx| tx.execute_batch(&statement).map_err(bug));
}

/// The account, identity and one draft revision every fixture hangs
/// on (the schema seeds its own `local_archive` account).
pub(super) fn draft_rows(store: &Store) {
    let s = format!(
        "INSERT OR IGNORE INTO accounts
         (account_id, kind, provider, display_name, state, sort_order, created_at)
         VALUES ('{ACCOUNT}','imap','generic_imap','Fixture','active',0,0);
         INSERT OR IGNORE INTO identities
         (identity_id, account_id, email, display_name, is_default, sendable)
         VALUES ('{IDENTITY}','{ACCOUNT}','me@example.com','Me',1,'validated');
         INSERT OR IGNORE INTO drafts (draft_id, state, created_at, updated_at)
         VALUES ('{DRAFT}','editing',0,0);
         INSERT OR IGNORE INTO draft_revisions
         (revision_id, draft_id, seq, account_id, identity_id, compose_format,
          subject, to_json, cc_json, bcc_json, body_source, attachments_json, created_at)
         VALUES ('{REVISION}','{DRAFT}',1,'{ACCOUNT}','{IDENTITY}','plain',
                 's','[]','[]','[]','','[]',0)"
    );
    sql(store, s);
}

/// The one draft revision's attachments: `attached_sha` or empty, in
/// the `attachments_json` shape `save_draft` writes (spec/02).
fn draft(store: &Store, attached_sha: Option<&str>) {
    draft_rows(store);
    let json = attached_sha
        .map(|sha| {
            format!(
                "[{{\"object_sha256\":\"{sha}\",\"filename\":\"a.txt\",\
                 \"media_type\":\"text/plain\",\"size\":4}}]"
            )
        })
        .unwrap_or_else(|| "[]".to_owned());
    sql(
        store,
        format!("UPDATE draft_revisions SET attachments_json = '{json}'"),
    );
}

/// An `accepted` send attempt whose frozen MIME is the object.
fn send_attempt(store: &Store, sha: &str, tag: &str) {
    draft_rows(store);
    let s = format!(
        "INSERT OR IGNORE INTO send_attempts
         (attempt_id, draft_id, revision_id, account_id, config_generation,
          identity_id, mime_sha256, message_id_header, envelope_from,
          state, sent_copy, created_at, updated_at)
         VALUES ('attempt-{tag}','{DRAFT}','{REVISION}','{ACCOUNT}',1,
                 '{IDENTITY}','{sha}','<m@x>','me@example.com',
                 'accepted','server_saves',0,0)"
    );
    sql(store, s);
}

/// A bare `acquisitions` row: the reference the import commit leaves.
fn reference(store: &Store, id: &ObjectId, state: &str) {
    let s = format!(
        "INSERT OR REPLACE INTO acquisitions
         (content_sha256, byte_length, object_state, first_acquired_at)
         VALUES ('{}', {}, '{state}', 0)",
        id.sha256_hex(),
        id.byte_length()
    );
    sql(store, s);
}

/// A `pin_kind` pin on `sha` stamped `at` — an aged grace clock's form.
fn pin(store: &Store, sha: &str, kind: &str, at: i64) {
    sql(
        store,
        format!("INSERT OR IGNORE INTO object_pins VALUES ('{sha}','{kind}','gc',{at})"),
    );
}

/// Publish `payload` and free its reservation: a bare object.
fn object(store: &Store, payload: &[u8]) -> (ObjectId, String) {
    let publication = store.objects().publish(payload).unwrap();
    let (id, sha) = (*publication.id(), publication.id().sha256_hex());
    writing(store, move |tx| {
        release_reservation(tx, &publication, ReservationRelease::Required)
    });
    (id, sha)
}

/// `count` rows appended to `seqs` in one commit — the live-copy
/// test's measurable growth. Each row carries ~2 KiB so the snapshot
/// takes many bounded page steps.
fn seed_rows(store: &Store, start: i64, count: i64) {
    let s = format!(
        "CREATE TABLE IF NOT EXISTS seqs (seq INTEGER PRIMARY KEY, pad TEXT);
         INSERT INTO seqs (seq, pad)
         WITH RECURSIVE c(x) AS
           (SELECT {start} UNION ALL SELECT x+1 FROM c WHERE x < {start} + {count} - 1)
         SELECT x, hex(zeroblob(1024)) FROM c"
    );
    sql(store, s);
}

/// Every `backup_in_progress` pin's object hash, sorted.
fn backup_pins(store: &Store) -> Vec<String> {
    reading(store, |tx| {
        let mut stmt = tx
            .prepare(
                "SELECT object_sha256 FROM object_pins
                 WHERE pin_kind = 'backup_in_progress' ORDER BY 1",
            )
            .map_err(bug)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(bug)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(bug)
    })
}

/// The generation row's `(state, location, started_at)` for `id`.
fn generation_row(store: &Store, id: &str) -> Option<(String, String, i64)> {
    reading(store, |tx| {
        Ok(tx
            .query_row(
                "SELECT state, location, started_at FROM backup_generations
                 WHERE generation_id = ?1",
                [id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .ok())
    })
}

/// The `seqs` table's `(count, max(seq))` — the live-copy proof's
/// contiguous-prefix check.
fn seqs_shape(conn: &Connection) -> (i64, i64) {
    conn.query_row(
        "SELECT count(*), coalesce(max(seq), 0) FROM seqs",
        [],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .unwrap()
}

// --- steps 1–3, through the public API ---

/// The snapshot is a consistent point-in-time copy while writes
/// continue (acceptance: "rows committed after the snapshot point are
/// absent, rows before are present"). Writes land through the real
/// writer while the copy runs.
#[test]
fn the_snapshot_is_a_consistent_copy_while_writes_continue() {
    let (_scratch, paths, store) = open("backup-live-copy");
    let (object_id, sha) = object(&store, PAYLOAD);
    reference(&store, &object_id, "published");
    seed_rows(&store, 1, 2_048);
    let store = Arc::new(store);

    // Park the copy after its first page batch, commit rows mid-copy,
    // release, then commit more after the copy finished.
    let gate = Arc::new(Barrier::new(2));
    store.backups.set_pauses(Pauses {
        after_first_step: Some(gate.clone()),
        ..Pauses::default()
    });
    let background = {
        let store = Arc::clone(&store);
        std::thread::spawn(move || store.backup_snapshot(BackupDestination::DataRoot))
    };
    gate.wait();
    seed_rows(&store, 2_049, 2_048);
    gate.wait();
    let snapshot = background.join().unwrap().unwrap();
    seed_rows(&store, 4_097, 2_048);

    // One consistent prefix: the pre-copy commit and the mid-copy
    // commit are whole, the post-copy commit absent.
    let conn = Connection::open(snapshot.state_db()).unwrap();
    assert_eq!(seqs_shape(&conn), (4_096, 4_096));
    let check: String = conn
        .query_row("PRAGMA integrity_check", [], |row| row.get(0))
        .unwrap();
    assert_eq!(check, "ok");
    drop(conn);

    // The row: `in_progress`, its location inside `backups/`, the
    // seam's clock value — and `state_db` names the file itself.
    let (state, location, started_at) = generation_row(&store, snapshot.generation_id()).unwrap();
    assert_eq!(state, "in_progress");
    assert_eq!(started_at, FIXED_NOW_MS);
    snapshot
        .generation_id()
        .parse::<Uuid>()
        .expect("the generation id is a UUID");
    let expected = real_fs()
        .canonicalize(paths.backups())
        .unwrap()
        .join(snapshot.generation_id());
    assert_eq!(location, expected.display().to_string());
    assert_eq!(snapshot.location(), expected.as_path());
    assert_eq!(snapshot.state_db(), expected.join("state.sqlite"));
    assert_eq!(snapshot.referenced_objects(), &[sha][..]);
    // Only the snapshot is present: no WAL leftovers stay behind.
    let mut entries: Vec<_> = std::fs::read_dir(&expected)
        .unwrap()
        .map(|entry| entry.unwrap().file_name())
        .collect();
    entries.sort();
    assert_eq!(entries, ["state.sqlite"]);
}

/// Every object the snapshot references is pinned — acquisitions,
/// send-attempt MIME and draft attachments alike — and nothing else:
/// an unreferenced object, and one referenced only after the snapshot
/// point, get no pin (acceptance: "every object the snapshot
/// references is pinned, and none that it doesn't reference"). The
/// parked read proves pins come from the snapshot, not the live
/// database.
#[test]
fn pins_cover_exactly_the_snapshot_references() {
    let (_scratch, _paths, store) = open("backup-pins");
    let (id_a, sha_a) = object(&store, b"a");
    reference(&store, &id_a, "published");
    let (_id_b, sha_b) = object(&store, b"b");
    send_attempt(&store, &sha_b, "b");
    let (_id_c, sha_c) = object(&store, b"c");
    draft(&store, Some(&sha_c));
    let (_id_o, _sha_o) = object(&store, b"o");

    // Land a live-only reference while the pin read is parked, so the
    // snapshot can't have seen it.
    let gate = Arc::new(Barrier::new(2));
    let store = Arc::new(store);
    store.backups.set_pauses(Pauses {
        before_referenced_read: Some(gate.clone()),
        ..Pauses::default()
    });
    let background = {
        let store = Arc::clone(&store);
        std::thread::spawn(move || store.backup_snapshot(BackupDestination::DataRoot))
    };
    gate.wait();
    let (id_late, _sha_late) = object(&store, b"late");
    reference(&store, &id_late, "published");
    gate.wait();
    let snapshot = background.join().unwrap().unwrap();

    let mut want = [sha_a, sha_b, sha_c];
    want.sort();
    assert_eq!(backup_pins(&store), want);
    assert_eq!(snapshot.referenced_objects(), &want[..]);
    // The pins are keyed by this generation and stamped by the seam.
    let pinned: Vec<(String, i64)> = reading(&store, |tx| {
        let mut stmt = tx
            .prepare(
                "SELECT pin_ref, created_at FROM object_pins
                 WHERE pin_kind = 'backup_in_progress' ORDER BY 1",
            )
            .map_err(bug)?;
        let rows = stmt
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .map_err(bug)?;
        rows.collect::<Result<Vec<(String, i64)>, _>>().map_err(bug)
    });
    assert_eq!(
        pinned,
        vec![(snapshot.generation_id().to_owned(), FIXED_NOW_MS); want.len()]
    );
}

/// The `in_progress` row freezes the whole pass — M1.8a2's real GC
/// gate (acceptance: "while the generation is `in_progress`, GC
/// removes nothing"). The same expired orphan falls when unblocked,
/// so the gate is the only thing keeping it.
#[test]
fn gc_removes_nothing_while_a_generation_is_in_progress() {
    let (_scratch, _paths, store) = open("backup-freezes-gc");
    let (id, sha) = object(&store, PAYLOAD);
    pin(&store, &sha, "gc_grace", 0);
    store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let frozen = store.collect_garbage(GC_GRACE_PERIOD).unwrap();
    assert_eq!(
        frozen,
        GcReport {
            blocked: Some(GcGate::BackupInProgress),
            scanned: 1,
            ..GcReport::default()
        }
    );
    assert!(store.objects().exists(&id).unwrap());

    // The same fixture without the generation is a real removal.
    let (_scratch2, _paths2, other) = open("backup-freezes-gc-2");
    let (id2, sha2) = object(&other, PAYLOAD);
    pin(&other, &sha2, "gc_grace", 0);
    let report = other.collect_garbage(GC_GRACE_PERIOD).unwrap();
    assert_eq!(
        report.removed, 1,
        "the orphan is a real candidate: {report:?}"
    );
    assert!(!other.objects().exists(&id2).unwrap());
}

/// One live generation at a time: a second call while the first is
/// `in_progress` is `Conflict`, and the first is untouched.
#[test]
fn a_second_generation_while_one_is_in_progress_is_a_conflict() {
    let (_scratch, _paths, store) = open("backup-one-at-a-time");
    let first = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let err = store
        .backup_snapshot(BackupDestination::DataRoot)
        .unwrap_err();
    assert!(
        matches!(err, StoreError::Conflict { ref detail } if detail.contains("already in progress")),
        "{err:?}"
    );
    assert!(first.state_db().is_file());
}

/// A user-chosen destination is created with flushed ancestors, holds
/// the generation directory, and records the canonical location.
#[test]
fn a_chosen_destination_is_created_and_holds_the_generation() {
    let (_scratch, paths, store) = open("backup-external");
    let (id, _sha) = object(&store, PAYLOAD);
    reference(&store, &id, "published");
    let destination = paths
        .data_root()
        .parent()
        .unwrap()
        .join("external")
        .join("mount");
    let snapshot = store
        .backup_snapshot(BackupDestination::Directory(destination.clone()))
        .unwrap();

    let canonical = real_fs().canonicalize(&destination).unwrap();
    assert_eq!(
        snapshot.location(),
        canonical.join(snapshot.generation_id())
    );
    assert!(snapshot.state_db().is_file());
    let (state, location, _) = generation_row(&store, snapshot.generation_id()).unwrap();
    assert_eq!(state, "in_progress");
    assert_eq!(location, snapshot.location().display().to_string());
    // The snapshot opened alone carries the live rows.
    let conn = Connection::open(snapshot.state_db()).unwrap();
    let referenced: i64 = conn
        .query_row("SELECT count(*) FROM acquisitions", [], |row| row.get(0))
        .unwrap();
    assert_eq!(referenced, 1);
}

/// The destination argument's validation: a relative path or a file
/// is `InvalidInput`, creating nothing.
#[test]
fn an_unusable_destination_is_invalid_input() {
    let (_scratch, paths, store) = open("backup-destination");
    let relative = store
        .backup_snapshot(BackupDestination::Directory(PathBuf::from("relative/dir")))
        .unwrap_err();
    assert!(
        matches!(relative, StoreError::InvalidInput { .. }),
        "{relative:?}"
    );
    let file = paths.data_root().parent().unwrap().join("file");
    std::fs::write(&file, b"not a dir").unwrap();
    let not_dir = store
        .backup_snapshot(BackupDestination::Directory(file))
        .unwrap_err();
    assert!(
        matches!(not_dir, StoreError::InvalidInput { .. }),
        "{not_dir:?}"
    );
    let rows: i64 = reading(&store, |tx| {
        tx.query_row("SELECT count(*) FROM backup_generations", [], |row| {
            row.get(0)
        })
        .map_err(bug)
    });
    assert_eq!(rows, 0, "a refused destination starts no generation");
}

// --- failure honesty: every dropped flush or failed write is an error ---

/// A `Backups` over a fault-injecting `fs`: the store itself is real —
/// only the snapshot path's operations can fail, so fault points are
/// exact.
fn faulty_backups(store: &Store, paths: &StorePaths, fs: Arc<dyn Fs>) -> Backups {
    faulty_backups_seeded(store, paths, fs, 7)
}

/// `faulty_backups` with a chosen random seed, so two stages can draw
/// different incarnations (or the same one, deliberately).
fn faulty_backups_seeded(store: &Store, paths: &StorePaths, fs: Arc<dyn Fs>, seed: u64) -> Backups {
    // Staging opens confined directories, which never follow a symlinked
    // ancestor (macOS's /var); resolve the root the way `open` does.
    let resolved = std::fs::canonicalize(paths.data_root()).expect("the scratch store root");
    let paths = StorePaths::new(resolved).unwrap();
    let objects = Arc::new(crate::objects::FileObjectStore::new(
        Arc::clone(&fs),
        Arc::new(SeededRandom::new(7)),
        paths.objects().to_path_buf(),
        paths.recovery().to_path_buf(),
        Arc::clone(&store.writer),
        Arc::new(FixedClock::new(Timestamp::from_millis(FIXED_NOW_MS))),
    ));
    Backups::new(
        fs,
        Arc::new(SeededRandom::new(seed)),
        Arc::new(FixedClock::new(Timestamp::from_millis(FIXED_NOW_MS))),
        Arc::clone(&store.writer),
        paths.state_db().to_path_buf(),
        paths.backups().to_path_buf(),
        objects,
    )
}

/// One fault point's aftermath: `Unavailable`, the row stays
/// `in_progress`, and the next run recovers the leftover.
fn assert_failed_snapshot<T: std::fmt::Debug>(store: &Store, result: Result<T, StoreError>) {
    let err = result.unwrap_err();
    assert!(
        matches!(err, StoreError::Unavailable { .. }),
        "a dropped flush must be Unavailable, not {err:?}"
    );
    let rows: Vec<String> = reading(store, |tx| {
        let mut stmt = tx
            .prepare("SELECT state FROM backup_generations")
            .map_err(bug)?;
        let rows = stmt
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(bug)?;
        rows.collect::<Result<Vec<_>, _>>().map_err(bug)
    });
    assert_eq!(rows, ["in_progress"], "the row stays for recovery");
    let retry = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    assert_eq!(
        generation_row(store, retry.generation_id()).unwrap().0,
        "in_progress"
    );
}

/// The snapshot file's create, its end-of-copy fsync and both
/// directory flushes failing each surface `Unavailable` — the
/// publication protocol's flush points (acceptance: "a dropped flush
/// ... is an error, never a silent partial").
#[test]
fn a_dropped_flush_or_failed_create_is_an_error() {
    for (op, nth) in [
        (FsOp::CreateDir, 1), // the generation's directory
        (FsOp::Create, 1),    // the snapshot file
        (FsOp::SyncDir, 1),   // the destination-root flush
        (FsOp::SyncDir, 2),   // the generation-directory flush
        (FsOp::SyncFile, 1),  // the snapshot file's fsync
    ] {
        let (_scratch, paths, store) = open("backup-fault");
        let fs = FaultFs::new(real_fs(), op, nth, FaultKind::Eio);
        let result =
            faulty_backups(&store, &paths, Arc::new(fs)).snapshot(BackupDestination::DataRoot);
        assert_failed_snapshot(&store, result);
    }
}

/// A destination directory that can't be prepared is `Unavailable`
/// and starts no generation.
#[test]
fn a_failed_destination_prepare_is_an_error_before_the_row() {
    let (_scratch, paths, store) = open("backup-dest-fault");
    let fs = FaultFs::new(real_fs(), FsOp::CreateDir, 1, FaultKind::NoSpace);
    let destination = paths.data_root().parent().unwrap().join("mount");
    let err = faulty_backups(&store, &paths, Arc::new(fs))
        .snapshot(BackupDestination::Directory(destination))
        .unwrap_err();
    assert!(matches!(err, StoreError::Unavailable { .. }), "{err:?}");
    let rows: i64 = reading(&store, |tx| {
        tx.query_row("SELECT count(*) FROM backup_generations", [], |row| {
            row.get(0)
        })
        .map_err(bug)
    });
    assert_eq!(rows, 0);
}

/// The online-backup write itself failing — the snapshot path
/// unwritable when SQLite opens it — is an error (acceptance: "a
/// failed snapshot write is an error"). The parked point between the
/// seam's file create and SQLite's open makes the failure exact.
#[test]
fn a_failed_snapshot_write_is_an_error_not_a_partial() {
    use std::os::unix::fs::PermissionsExt;
    let (_scratch, _paths, store) = open("backup-write-fails");
    let store = Arc::new(store);
    let gate = Arc::new(Barrier::new(2));
    store.backups.set_pauses(Pauses {
        after_snapshot_file: Some(gate.clone()),
        ..Pauses::default()
    });
    let background = {
        let store = Arc::clone(&store);
        std::thread::spawn(move || store.backup_snapshot(BackupDestination::DataRoot))
    };
    gate.wait();
    // The seam created the file; make it unwritable before SQLite's
    // copy starts.
    let location: String = reading(&store, |tx| {
        tx.query_row("SELECT location FROM backup_generations", [], |row| {
            row.get(0)
        })
        .map_err(bug)
    });
    let file = Path::new(&location).join("state.sqlite");
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o444)).unwrap();
    gate.wait();
    let result = background.join().unwrap();
    store.backups.set_pauses(Pauses::default());
    std::fs::set_permissions(&file, std::fs::Permissions::from_mode(0o644)).unwrap();
    assert_failed_snapshot(&store, result);
}

/// The writer thread can't await its own queue — the same refusal GC
/// gets (spec/03 "Write discipline").
#[test]
fn the_writer_thread_cannot_back_up_through_its_own_queue() {
    let (_scratch, _paths, store) = open("backup-on-writer");
    let store = Arc::new(store);
    let inside = {
        let store = Arc::clone(&store);
        move |_tx: &rusqlite::Transaction<'_>| {
            let result = store.backup_snapshot(BackupDestination::DataRoot);
            assert!(
                matches!(result, Err(StoreError::Internal { .. })),
                "{result:?}"
            );
            Ok(())
        }
    };
    block_on(store.write(crate::WritePriority::User, inside)).unwrap();
}

// --- crash cuts: a cut can never produce `complete` ---

const CUT_N: &str = "SB_BACKUP_CUT_N";
const CUT_ROOT: &str = "SB_BACKUP_CUT_ROOT";

/// Not a test: `run_crash_cuts` re-executes it per cut point. The
/// store and fixtures run over the real filesystem; only the
/// snapshot's `Backups` coordinator sees the `CutFs`, so every cut
/// point is one of this procedure's own operations.
#[test]
#[ignore = "spawned by run_crash_cuts"]
fn snapshot_cut_child() {
    let nth = env::var(CUT_N).unwrap().parse().unwrap();
    let root = env::var(CUT_ROOT).unwrap();
    let paths = StorePaths::new(Path::new(&root)).unwrap();
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    let (id_a, _a) = object(&store, b"one");
    reference(&store, &id_a, "published");
    let (_id_o, _o) = object(&store, b"two");
    let backups = faulty_backups(&store, &paths, Arc::new(CutFs::new(real_fs(), nth)));
    backups.snapshot(BackupDestination::DataRoot).unwrap();
}

/// At every cut point: the store reopens, every generation is
/// `in_progress` — `complete` and `failed` are unreachable — a
/// `backup_in_progress` pin only ever belongs to a live generation,
/// and the next run recovers the leftover and backs up cleanly.
#[test]
fn crash_cuts_leave_the_generation_in_progress_never_complete() {
    let scratch = ScratchDir::new("backup-cuts");
    let cuts = run_crash_cuts(
        CUT_N,
        CUT_ROOT,
        "backups::tests::snapshot_cut_child",
        &mut |n| {
            let root = scratch.path().join(format!("cut-{n}"));
            std::fs::create_dir(&root).unwrap();
            root
        },
        &mut |root| {
            let paths = StorePaths::new(root).unwrap();
            let store = crate::open(&paths, &test_seams(real_fs()))
                .expect("the store must reopen after every cut");
            let (open_rows, other_rows): (i64, i64) = reading(&store, |tx| {
                tx.query_row(
                    "SELECT (SELECT count(*) FROM backup_generations WHERE state = 'in_progress'),
                            (SELECT count(*) FROM backup_generations WHERE state <> 'in_progress')",
                    [],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .map_err(bug)
            });
            assert_eq!(other_rows, 0, "no state but in_progress may appear");
            assert!(open_rows <= 1, "generations stay one-at-a-time");
            let stray: i64 = reading(&store, |tx| {
                tx.query_row(
                    "SELECT count(*) FROM object_pins p
                     WHERE p.pin_kind = 'backup_in_progress' AND NOT EXISTS
                       (SELECT 1 FROM backup_generations g
                        WHERE g.generation_id = p.pin_ref AND g.state = 'in_progress')",
                    [],
                    |row| row.get(0),
                )
                .map_err(bug)
            });
            assert_eq!(stray, 0, "a backup pin only belongs to a live generation");
            store
                .backup_snapshot(BackupDestination::DataRoot)
                .expect("recover the cut generation");
        },
    );
    assert!(cuts > 0, "the sweep must have killed children");
}

#[test]
fn publication_verifies_objects_and_commits_a_manifest() {
    let (_scratch, _paths, store) = open("backup-publish");
    let (id, sha) = object(&store, PAYLOAD);
    reference(&store, &id, "published");
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let receipt = store.complete_backup(snapshot.clone()).unwrap();
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(snapshot.location().join("manifest.json")).unwrap())
            .unwrap();
    let db = Connection::open(snapshot.state_db()).unwrap();
    let store_uuid: String = db
        .query_row(
            "SELECT value FROM store_meta WHERE key='store_uuid'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    let schema_version: i64 = db
        .query_row("PRAGMA user_version", [], |r| r.get(0))
        .unwrap();
    assert_eq!(manifest.as_object().unwrap().len(), 8);
    assert_eq!(manifest["format"], 1);
    assert_eq!(manifest["store_uuid"], store_uuid);
    assert_eq!(manifest["app_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(manifest["schema_version"], schema_version);
    assert_eq!(manifest["created_at"], FIXED_NOW_MS);
    assert_eq!(manifest["generation_id"], snapshot.generation_id());
    assert_eq!(manifest["database"]["file"], "state.sqlite");
    assert_eq!(manifest["objects"][0]["sha256"], sha);
    assert_eq!(manifest["objects"][0]["bytes"], PAYLOAD.len());
    assert_eq!(receipt.generation_id(), snapshot.generation_id());
    assert_eq!(
        generation_row(&store, snapshot.generation_id()).unwrap().0,
        "complete"
    );
    assert!(backup_pins(&store).is_empty());
}

#[test]
fn publication_failures_leave_the_generation_in_progress() {
    // Sweep all calls until the first successful run, including the
    // snapshot's and publication's flushes and both streamed writes.
    for op in [
        FsOp::SyncFile,
        FsOp::SyncDir,
        FsOp::CreateDir,
        FsOp::Create,
        FsOp::Read,
        FsOp::Write,
    ] {
        let mut failures = 0;
        let mut finished = false;
        for nth in 1..40 {
            let (_scratch, paths, store) = open("backup-publication-fault");
            let (id, _) = object(&store, PAYLOAD);
            reference(&store, &id, "published");
            let fs = Arc::new(FaultFs::new(real_fs(), op, nth, FaultKind::Eio));
            let backups = faulty_backups(&store, &paths, fs);
            let result = backups
                .snapshot(BackupDestination::DataRoot)
                .and_then(|s| backups.complete(s));
            if result.is_ok() {
                finished = true;
                break;
            }
            failures += 1;
            assert_failed_snapshot(&store, result);
        }
        assert!(failures > 0, "{op:?} must be exercised");
        assert!(finished, "{op:?} sweep must reach the end");
    }
}

#[test]
fn damaged_or_missing_source_never_completes() {
    for missing in [false, true] {
        let (_scratch, _paths, store) = open("backup-damaged");
        let (id, _) = object(&store, PAYLOAD);
        reference(&store, &id, "published");
        let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
        let source = store.objects().object_path(&id);
        if missing {
            std::fs::remove_file(&source).unwrap();
        } else {
            std::fs::write(&source, b"damaged").unwrap();
        }
        let err = store.complete_backup(snapshot.clone()).unwrap_err();
        if missing {
            assert!(matches!(err, StoreError::NotFound { .. }), "{err:?}");
        } else {
            assert!(matches!(err, StoreError::Corrupt { .. }), "{err:?}");
            assert!(!source.exists(), "damaged source must be quarantined");
        }
        assert_eq!(
            generation_row(&store, snapshot.generation_id()).unwrap().0,
            "in_progress"
        );
        assert!(!snapshot.location().join("manifest.json").exists());
        assert_eq!(backup_pins(&store).len(), 1);
    }
}

#[test]
fn gc_stays_blocked_until_publication_commits() {
    let (_scratch, _paths, store) = open("backup-publish-gc");
    let (id, sha) = object(&store, PAYLOAD);
    reference(&store, &id, "published");
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    sql(&store, "DELETE FROM acquisitions".to_owned());
    pin(&store, &sha, "gc_grace", 0);
    let report = store.collect_garbage(GC_GRACE_PERIOD).unwrap();
    assert_eq!(report.blocked, Some(GcGate::BackupInProgress));
    assert!(store.objects().exists(&id).unwrap());
    store.complete_backup(snapshot).unwrap();
    let report = store.collect_garbage(GC_GRACE_PERIOD).unwrap();
    assert_eq!(report.blocked, None);
    assert_eq!(report.removed, 1);
}

const PUBLICATION_CUT_N: &str = "SB_BACKUP_PUBLICATION_CUT_N";
const PUBLICATION_CUT_ROOT: &str = "SB_BACKUP_PUBLICATION_CUT_ROOT";

#[test]
#[ignore = "spawned by run_crash_cuts"]
fn publication_cut_child() {
    let nth = env::var(PUBLICATION_CUT_N).unwrap().parse().unwrap();
    let root = env::var(PUBLICATION_CUT_ROOT).unwrap();
    let paths = StorePaths::new(Path::new(&root)).unwrap();
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    let (id, _) = object(&store, PAYLOAD);
    reference(&store, &id, "published");
    let backups = faulty_backups(&store, &paths, Arc::new(CutFs::new(real_fs(), nth)));
    let snapshot = backups.snapshot(BackupDestination::DataRoot).unwrap();
    backups.complete(snapshot).unwrap();
}

#[test]
fn publication_crash_cuts_never_complete() {
    let scratch = ScratchDir::new("backup-publication-cuts");
    let cuts = run_crash_cuts(
        PUBLICATION_CUT_N,
        PUBLICATION_CUT_ROOT,
        "backups::tests::publication_cut_child",
        &mut |n| {
            let root = scratch.path().join(format!("cut-{n}"));
            std::fs::create_dir(&root).unwrap();
            root
        },
        &mut |root| {
            let paths = StorePaths::new(root).unwrap();
            let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
            let complete: i64 = reading(&store, |tx| {
                tx.query_row(
                    "SELECT count(*) FROM backup_generations WHERE state='complete'",
                    [],
                    |r| r.get(0),
                )
                .map_err(bug)
            });
            assert_eq!(complete, 0);
        },
    );
    assert!(cuts > 0);
}

#[test]
fn completion_receipt_survives_a_power_cut() {
    use sb_test_support::SimFs;
    use sha2::{Digest, Sha256};
    use std::io::{Read, Write};
    let hash = |bytes: &[u8]| {
        Sha256::digest(bytes)
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>()
    };

    let (_scratch, paths, store) = open("backup-power-cut");
    let (id, sha) = object(&store, PAYLOAD);
    reference(&store, &id, "published");
    let mut snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let sim = Arc::new(SimFs::new());
    let destination = snapshot.location().parent().unwrap();
    sim.create_dir_all(destination).unwrap();
    sim.create_dir(snapshot.location()).unwrap();
    let mut state = sim.create(&snapshot.state_db()).unwrap();
    state
        .write_all(&std::fs::read(snapshot.state_db()).unwrap())
        .unwrap();
    snapshot.snapshot_handle = Arc::new(std::sync::Mutex::new(state));
    let source = store.objects().object_path(&id);
    sim.create_dir_all(source.parent().unwrap()).unwrap();
    let mut source_file = sim.create(&source).unwrap();
    source_file.write_all(PAYLOAD).unwrap();
    source_file.sync_all().unwrap();
    sim.sync_dir(source.parent().unwrap()).unwrap();
    let backups = faulty_backups(&store, &paths, sim.clone());
    *backups.active.lock().unwrap() = Some(Arc::downgrade(&snapshot.active));
    let receipt = backups.complete(snapshot).unwrap();
    sim.power_cut();
    let mut data = Vec::new();
    sim.open(&receipt.location().join("manifest.json"))
        .unwrap()
        .read_to_end(&mut data)
        .unwrap();
    let manifest: serde_json::Value = serde_json::from_slice(&data).unwrap();
    let mut database = Vec::new();
    sim.open(&receipt.state_db())
        .unwrap()
        .read_to_end(&mut database)
        .unwrap();
    assert_eq!(manifest["database"]["sha256"], hash(&database));
    assert_eq!(manifest["database"]["bytes"], database.len());
    let copy = receipt
        .location()
        .join("objects/sha256")
        .join(&sha[..2])
        .join(&sha[2..4])
        .join(&sha);
    let mut bytes = Vec::new();
    sim.open(&copy).unwrap().read_to_end(&mut bytes).unwrap();
    assert_eq!(bytes, PAYLOAD);
    assert_eq!(manifest["objects"][0]["sha256"], hash(&bytes));
    assert_eq!(
        generation_row(&store, receipt.generation_id()).unwrap().0,
        "complete"
    );
    let persisted_hash: String = reading(&store, |tx| {
        tx.query_row(
            "SELECT manifest_sha256 FROM backup_generations WHERE generation_id=?1",
            [receipt.generation_id()],
            |r| r.get(0),
        )
        .map_err(bug)
    });
    assert_eq!(persisted_hash, hash(&data));
    assert_eq!(receipt.manifest_sha256(), persisted_hash);
    let id = receipt.generation_id().to_owned();
    drop(backups);
    drop(store);
    let reopened = crate::open(&paths, &test_seams(real_fs())).unwrap();
    assert_eq!(generation_row(&reopened, &id).unwrap().0, "complete");
}

#[test]
fn failed_pin_release_keeps_gc_gated() {
    let (_scratch, _paths, store) = open("backup-pin-release-failure");
    let (id, sha) = object(&store, PAYLOAD);
    reference(&store, &id, "published");
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    sql(&store, "CREATE TRIGGER fail_backup_unpin BEFORE DELETE ON object_pins WHEN OLD.pin_kind='backup_in_progress' BEGIN SELECT RAISE(ABORT, 'test unpin failure'); END;".to_owned());
    assert!(store.complete_backup(snapshot.clone()).is_err());
    assert_eq!(
        generation_row(&store, snapshot.generation_id()).unwrap().0,
        "in_progress"
    );
    assert_eq!(backup_pins(&store), vec![sha]);
    assert_eq!(
        store.collect_garbage(GC_GRACE_PERIOD).unwrap().blocked,
        Some(GcGate::BackupInProgress)
    );
    assert!(store.objects().exists(&id).unwrap());
}

#[test]
fn interruption_between_pin_batches_keeps_gc_gated() {
    let (_scratch, _paths, store) = open("backup-partial-pin-release");
    let (id, sha) = object(&store, PAYLOAD);
    reference(&store, &id, "published");
    let mut snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let mut pins: Vec<String> = (1..=200).map(|n| format!("{n:064x}")).collect();
    assert!(pins.iter().all(|pin| pin < &sha));
    let generation = snapshot.generation_id().to_owned();
    let seeded = pins.clone();
    writing(&store, move |tx| {
        for pin_sha in seeded {
            tx.execute(
                "INSERT INTO object_pins VALUES (?1, 'backup_in_progress', ?2, 0)",
                [&pin_sha, &generation],
            )
            .map_err(bug)?;
        }
        Ok(())
    });
    pins.push(sha.clone());
    snapshot.referenced = pins.into();
    sql(
        &store,
        format!(
            "CREATE TRIGGER fail_last_backup_unpin BEFORE DELETE ON object_pins WHEN OLD.pin_kind='backup_in_progress' AND OLD.object_sha256='{sha}' BEGIN SELECT RAISE(ABORT, 'test second batch failure'); END;"
        ),
    );
    assert!(store.backups.release_pins(&snapshot).is_err());
    assert_eq!(backup_pins(&store), vec![sha]);
    assert_eq!(
        generation_row(&store, snapshot.generation_id()).unwrap().0,
        "in_progress"
    );
    assert_eq!(
        store.collect_garbage(GC_GRACE_PERIOD).unwrap().blocked,
        Some(GcGate::BackupInProgress)
    );
    assert!(store.objects().exists(&id).unwrap());
}

#[test]
fn completion_commit_failure_keeps_gc_gated_after_pins_are_released() {
    let (_scratch, _paths, store) = open("backup-completion-failure");
    let (id, _) = object(&store, PAYLOAD);
    reference(&store, &id, "published");
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    sql(&store, "CREATE TRIGGER fail_backup_complete BEFORE UPDATE ON backup_generations WHEN NEW.state='complete' BEGIN SELECT RAISE(ABORT, 'test completion failure'); END;".to_owned());
    assert!(store.complete_backup(snapshot.clone()).is_err());
    assert_eq!(
        generation_row(&store, snapshot.generation_id()).unwrap().0,
        "in_progress"
    );
    assert!(backup_pins(&store).is_empty());
    assert_eq!(
        store.collect_garbage(GC_GRACE_PERIOD).unwrap().blocked,
        Some(GcGate::BackupInProgress)
    );
    assert!(store.objects().exists(&id).unwrap());
}

#[test]
fn empty_generation_completes_and_store_closes_cleanly() {
    let (_scratch, paths, store) = open("backup-empty");
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let receipt = store.complete_backup(snapshot).unwrap();
    let manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(receipt.location().join("manifest.json")).unwrap())
            .unwrap();
    assert_eq!(manifest["objects"], serde_json::json!([]));
    store.close().unwrap();
    let reopened = crate::open(&paths, &test_seams(real_fs())).unwrap();
    assert_eq!(
        generation_row(&reopened, receipt.generation_id())
            .unwrap()
            .0,
        "complete"
    );
}

#[test]
fn backup_reader_does_not_quarantine_a_repaired_object() {
    let (_scratch, _paths, store) = open("backup-repair-race");
    let (id, _) = object(&store, PAYLOAD);
    reference(&store, &id, "published");
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    std::fs::write(store.objects().object_path(&id), b"bad").unwrap();
    let gate = Arc::new(Barrier::new(2));
    store.objects().set_pauses(crate::objects::Pauses {
        backup_opened: Some(gate.clone()),
        ..Default::default()
    });
    let store = Arc::new(store);
    let backup = {
        let store = store.clone();
        std::thread::spawn(move || store.complete_backup(snapshot))
    };
    gate.wait();
    assert!(matches!(
        store.objects().open(&id),
        Err(StoreError::Corrupt { .. })
    ));
    let replacement = store.objects().publish(PAYLOAD).unwrap();
    gate.wait();
    let _ = backup.join().unwrap();
    let mut bytes = Vec::new();
    use std::io::Read;
    store
        .objects()
        .open(replacement.id())
        .unwrap()
        .read_to_end(&mut bytes)
        .unwrap();
    assert_eq!(bytes, PAYLOAD);
}

#[test]
fn remaining_pin_fence_uses_a_leading_generation_index() {
    let (_scratch, _paths, store) = open("backup-pin-index");
    let plan: String = reading(&store, |tx| {
        tx.query_row(
        "EXPLAIN QUERY PLAN SELECT 1 FROM object_pins WHERE pin_kind='backup_in_progress' AND pin_ref='gen' LIMIT 1", [], |r| r.get(3)).map_err(bug)
    });
    assert!(
        plan.contains("SEARCH") && plan.contains("pin_kind=? AND pin_ref=?"),
        "{plan}"
    );
}

const TX_CUT: &str = "SB_BACKUP_TX_CUT";
const TX_CUT_ROOT: &str = "SB_BACKUP_TX_CUT_ROOT";

#[test]
#[ignore = "spawned by transaction_cuts_preserve_backup_state"]
fn transaction_cut_child() {
    let point = env::var(TX_CUT).unwrap();
    let root = env::var(TX_CUT_ROOT).unwrap();
    let paths = StorePaths::new(root).unwrap();
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    let (id, _) = object(&store, PAYLOAD);
    reference(&store, &id, "published");
    let mut snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    if point.contains("batch") {
        let mut pins: Vec<String> = (1..=200).map(|n| format!("{n:064x}")).collect();
        let generation = snapshot.generation_id().to_owned();
        let seeded = pins.clone();
        writing(&store, move |tx| {
            for sha in seeded {
                tx.execute(
                    "INSERT INTO object_pins VALUES (?1, 'backup_in_progress', ?2, 0)",
                    [&sha, &generation],
                )
                .map_err(bug)?;
            }
            Ok(())
        });
        pins.push(id.sha256_hex());
        pins.sort();
        snapshot.referenced = pins.into();
        store.backups.release_pins(&snapshot).unwrap();
    } else {
        store.complete_backup(snapshot).unwrap();
    }
}

#[test]
fn transaction_cuts_preserve_backup_state() {
    use std::os::unix::process::ExitStatusExt;
    let scratch = ScratchDir::new("backup-tx-cuts");
    for point in [
        "before-batch-0",
        "inside-batch-0",
        "after-batch-0",
        "before-batch-1",
        "inside-batch-1",
        "after-batch-1",
        "before-completion-commit",
        "after-completion-commit",
    ] {
        let root = scratch.path().join(point);
        std::fs::create_dir(&root).unwrap();
        let status = std::process::Command::new(env::current_exe().unwrap())
            .args([
                "--exact",
                "backups::tests::transaction_cut_child",
                "--ignored",
            ])
            .env(TX_CUT, point)
            .env(TX_CUT_ROOT, &root)
            .status()
            .unwrap();
        assert_eq!(
            status.signal(),
            Some(9),
            "cut did not fire at {point}: {status}"
        );
        let paths = StorePaths::new(root).unwrap();
        let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
        let (state, location, hash): (String, String, Option<String>) = reading(&store, |tx| {
            tx.query_row(
                "SELECT state, location, manifest_sha256 FROM backup_generations",
                [],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .map_err(bug)
        });
        if point == "after-completion-commit" {
            use sha2::{Digest, Sha256};
            let data = std::fs::read(Path::new(&location).join("manifest.json")).unwrap();
            let actual = Sha256::digest(&data)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>();
            assert_eq!(state, "complete");
            assert_eq!(hash.as_deref(), Some(actual.as_str()));
            let manifest: serde_json::Value = serde_json::from_slice(&data).unwrap();
            let database = std::fs::read(Path::new(&location).join("state.sqlite")).unwrap();
            let database_sha = Sha256::digest(&database)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>();
            assert_eq!(manifest["database"]["sha256"], database_sha);
            assert_eq!(manifest["database"]["bytes"], database.len());
            let sha = manifest["objects"][0]["sha256"].as_str().unwrap();
            let bytes = std::fs::read(
                Path::new(&location)
                    .join("objects/sha256")
                    .join(&sha[..2])
                    .join(&sha[2..4])
                    .join(sha),
            )
            .unwrap();
            assert_eq!(bytes, PAYLOAD);
            let object_sha = Sha256::digest(&bytes)
                .iter()
                .map(|b| format!("{b:02x}"))
                .collect::<String>();
            assert_eq!(sha, object_sha);
        } else {
            assert_eq!(state, "in_progress", "{point}");
            assert_eq!(
                store.collect_garbage(GC_GRACE_PERIOD).unwrap().blocked,
                Some(GcGate::BackupInProgress)
            );
            let count = backup_pins(&store).len();
            let expected = match point {
                "before-batch-0" | "inside-batch-0" => 201,
                "after-batch-0" | "before-batch-1" | "inside-batch-1" => 1,
                _ => 0,
            };
            assert_eq!(count, expected, "{point}");
        }
    }
}

#[test]
fn user_writes_progress_during_large_pin_validation() {
    let (_scratch, _paths, store) = open("backup-validation-progress");
    let (id, _) = object(&store, PAYLOAD);
    reference(&store, &id, "published");
    let mut snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let mut pins: Vec<String> = (1..=400).map(|n| format!("{n:064x}")).collect();
    let seeded = pins.clone();
    let generation = snapshot.generation_id().to_owned();
    writing(&store, move |tx| {
        for sha in seeded {
            tx.execute(
                "INSERT INTO object_pins VALUES (?1, 'backup_in_progress', ?2, 0)",
                [&sha, &generation],
            )
            .map_err(bug)?;
        }
        Ok(())
    });
    pins.push(id.sha256_hex());
    pins.sort();
    snapshot.referenced = pins.into();
    let gate = Arc::new(Barrier::new(2));
    store.backups.set_pauses(Pauses {
        during_pin_validation: Some(gate.clone()),
        ..Default::default()
    });
    let store = Arc::new(store);
    let worker = {
        let store = store.clone();
        std::thread::spawn(move || store.complete_backup(snapshot))
    };
    gate.wait();
    sql(
        &store,
        "CREATE TABLE user_write_during_validation (id INTEGER) STRICT".to_owned(),
    );
    gate.wait();
    assert!(worker.join().unwrap().is_err()); // synthetic source hashes do not exist
    assert_eq!(
        store.collect_garbage(GC_GRACE_PERIOD).unwrap().blocked,
        Some(GcGate::BackupInProgress)
    );
}

#[test]
fn a_new_pin_after_validation_prevents_completion() {
    let (_scratch, _paths, store) = open("backup-pin-fence");
    let (id, _) = object(&store, PAYLOAD);
    reference(&store, &id, "published");
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let generation = snapshot.generation_id().to_owned();
    let gate = Arc::new(Barrier::new(2));
    store.backups.set_pauses(Pauses {
        before_pin_release: Some(gate.clone()),
        ..Default::default()
    });
    let store = Arc::new(store);
    let worker = {
        let store = store.clone();
        std::thread::spawn(move || store.complete_backup(snapshot))
    };
    gate.wait();
    writing(&store, move |tx| {
        tx.execute(
            "INSERT INTO object_pins VALUES ('late', 'backup_in_progress', ?1, 0)",
            [generation],
        )
        .map_err(bug)?;
        Ok(())
    });
    gate.wait();
    assert!(worker.join().unwrap().is_err());
    assert_eq!(backup_pins(&store), vec!["late"]);
    assert_eq!(
        store.collect_garbage(GC_GRACE_PERIOD).unwrap().blocked,
        Some(GcGate::BackupInProgress)
    );
}

#[test]
fn next_backup_recovers_an_abandoned_generation() {
    let (_scratch, _paths, store) = open("backup-recover-abandoned");
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let id = snapshot.generation_id().to_owned();
    let location = snapshot.location().to_owned();
    drop(snapshot);
    let next = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    assert_ne!(next.generation_id(), id);
    assert_eq!(generation_row(&store, &id).unwrap().0, "failed");
    assert!(!location.exists());
}

#[test]
fn another_store_rejects_an_active_snapshot() {
    let (_scratch_a, _paths_a, store_a) = open("backup-token-store-a");
    let (_scratch_b, _paths_b, store_b) = open("backup-token-store-b");
    let a = store_a
        .backup_snapshot(BackupDestination::DataRoot)
        .unwrap();
    let b = store_b
        .backup_snapshot(BackupDestination::DataRoot)
        .unwrap();

    assert!(matches!(
        store_b.complete_backup(a.clone()),
        Err(StoreError::Conflict { .. })
    ));
    assert_eq!(
        generation_row(&store_a, a.generation_id()).unwrap().0,
        "in_progress"
    );
    assert_eq!(
        generation_row(&store_b, b.generation_id()).unwrap().0,
        "in_progress"
    );
    assert!(!a.location().join("manifest.json").exists());
    assert!(!b.location().join("manifest.json").exists());
    store_a.complete_backup(a).unwrap();
    store_b.complete_backup(b).unwrap();
}

#[test]
fn cloned_snapshot_alone_prevents_recovery_until_dropped() {
    let (_scratch, _paths, store) = open("backup-clone-ownership");
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let id = snapshot.generation_id().to_owned();
    let location = snapshot.location().to_owned();
    let clone = snapshot.clone();
    drop(snapshot);

    assert!(matches!(
        store.backup_snapshot(BackupDestination::DataRoot),
        Err(StoreError::Conflict { .. })
    ));
    assert_eq!(generation_row(&store, &id).unwrap().0, "in_progress");
    assert!(location.join("state.sqlite").is_file());
    drop(clone);
    let next = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    assert_ne!(next.generation_id(), id);
    assert_eq!(generation_row(&store, &id).unwrap().0, "failed");
    assert!(!location.exists());
}

fn seed_generation(
    store: &Store,
    root: &Path,
    n: u64,
    time: i64,
    state: &str,
) -> (String, PathBuf) {
    let id = format!("00000000-0000-4000-8000-{n:012x}");
    let path = root.join(&id);
    sql(
        store,
        format!(
            "INSERT INTO backup_generations (generation_id, started_at, state, location) VALUES ('{id}', {time}, '{state}', '{}')",
            path.display()
        ),
    );
    (id, path)
}

#[test]
fn recovery_retries_failed_cleanup_without_touching_inherited_failed_rows() {
    for (op, kind) in [
        (FsOp::RemoveDirAll, std::io::ErrorKind::PermissionDenied),
        (FsOp::RemoveDirAll, std::io::ErrorKind::Other),
        (FsOp::SyncDir, std::io::ErrorKind::StorageFull),
        (FsOp::SyncDir, std::io::ErrorKind::PermissionDenied),
    ] {
        let (_scratch, paths, store) = open("backup-recovery-fault");
        let external = paths.data_root().parent().unwrap().join("external");
        std::fs::create_dir(&external).unwrap();
        let (id, path) = seed_generation(&store, &external, 1, 0, "in_progress");
        std::fs::create_dir(&path).unwrap();
        own_fixture(&store, &path);
        let (_, inherited) = seed_generation(&store, &external, 2, 0, "failed");
        std::fs::create_dir(&inherited).unwrap();
        sql(
            &store,
            format!(
                "INSERT INTO object_pins VALUES ('one','backup_in_progress','{id}',0); INSERT INTO object_pins VALUES ('two','reservation','{id}',0)"
            ),
        );
        let fault = Arc::new(FaultFs::new(real_fs(), op, 1, FaultKind::Kind(kind)));
        let backups = faulty_backups(&store, &paths, fault);
        let err = backups.recover().unwrap_err();
        assert_eq!(err.io_kind(), Some(kind));
        assert_eq!(generation_row(&store, &id).unwrap().0, "failed");
        assert_eq!(backup_pins(&store), ["one"]);
        assert!(inherited.exists());
        drop(backups);
        let backups = faulty_backups(&store, &paths, real_fs());
        backups.recover().unwrap();
        assert!(!path.exists());
        assert!(inherited.exists());
        assert!(backup_pins(&store).is_empty());
        let other: i64 = reading(&store, |tx| {
            tx.query_row(
                "SELECT count(*) FROM object_pins WHERE pin_kind='reservation'",
                [],
                |r| r.get(0),
            )
            .map_err(bug)
        });
        assert_eq!(other, 1);
        assert_eq!(generation_row(&store, &id).unwrap().0, "failed");
    }
}

#[test]
fn recovery_handles_missing_directories_and_rejects_bad_locations() {
    let (_scratch, paths, store) = open("backup-recovery-locations");
    let (id, _) = seed_generation(&store, paths.backups(), 3, 0, "in_progress");
    store.backups.recover().unwrap();
    assert_eq!(generation_row(&store, &id).unwrap().0, "failed");
    let (bad, path) = seed_generation(&store, paths.backups(), 4, 1, "in_progress");
    std::fs::create_dir(&path).unwrap();
    sql(
        &store,
        format!(
            "UPDATE backup_generations SET location='{}' WHERE generation_id='{bad}'",
            paths.backups().display()
        ),
    );
    assert!(matches!(
        store.backups.recover(),
        Err(StoreError::Corrupt { .. })
    ));
    assert!(path.exists());
    assert_eq!(generation_row(&store, &bad).unwrap().0, "in_progress");
}

fn own_fixture(store: &Store, path: &Path) {
    let id = path.file_name().unwrap().to_str().unwrap();
    let token = "9f17c0aa-0000-4000-8000-000000000001";
    sql(
        store,
        format!("INSERT INTO backup_ownership VALUES ('{id}','{token}')"),
    );
    std::fs::write(super::owner_path(path), token).unwrap();
    std::fs::write(path.join("payload"), b"generation").unwrap();
}

#[test]
fn completion_receipt_survives_retention_until_the_next_pass() {
    for offset in [1, 0] {
        let (_scratch, paths, store) = open("backup-receipt-retention");
        let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
        let id = "ffffffff-ffff-4fff-bfff-ffffffffffff";
        assert!(snapshot.generation_id() < id);
        let path = paths.backups().join(id);
        sql(
            &store,
            format!(
                "INSERT INTO backup_generations (generation_id, started_at, state, location) VALUES ('{id}', {}, 'complete', '{}')",
                FIXED_NOW_MS + offset,
                path.display()
            ),
        );
        std::fs::create_dir(&path).unwrap();
        own_fixture(&store, &path);

        let receipt = store.complete_backup(snapshot).unwrap();
        let row = generation_row(&store, receipt.generation_id()).unwrap();
        assert_eq!(row.0, "complete");
        assert_eq!(Path::new(&row.1), receipt.location());
        assert!(receipt.location().join("manifest.json").is_file());

        store.backups.retain().unwrap();
        assert!(generation_row(&store, receipt.generation_id()).is_none());
        assert!(!receipt.location().exists());
        assert!(path.exists());
    }
}

#[test]
fn retention_keeps_bucket_union_and_deletes_oldest_first() {
    let (_scratch, paths, store) = open("backup-retention");
    const HOUR: i64 = 3_600_000;
    for n in 0..30 {
        let (_, path) = seed_generation(&store, paths.backups(), n, n as i64 * HOUR, "complete");
        std::fs::create_dir(&path).unwrap();
        own_fixture(&store, &path);
    }
    let fault = Arc::new(FaultFs::new(
        real_fs(),
        FsOp::RemoveDirAll,
        2,
        FaultKind::Eio,
    ));
    let backups = faulty_backups(&store, &paths, fault);
    assert!(backups.retain().is_err());
    assert!(generation_row(&store, "00000000-0000-4000-8000-000000000000").is_none());
    assert!(generation_row(&store, "00000000-0000-4000-8000-000000000001").is_some());
    store.backups.retain().unwrap();
    for n in 0..30 {
        let id = format!("00000000-0000-4000-8000-{n:012x}");
        assert_eq!(generation_row(&store, &id).is_some(), n >= 6, "{n}");
    }
}

#[test]
#[ignore = "spawned by recovery_crash_boundaries"]
fn recovery_cut_child() {
    let root = env::var("SB_BACKUP_RECOVERY_ROOT").unwrap();
    let paths = StorePaths::new(Path::new(&root)).unwrap();
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    let (id, path) = seed_generation(&store, paths.backups(), 9, 0, "in_progress");
    std::fs::create_dir(&path).unwrap();
    own_fixture(&store, &path);
    sql(
        &store,
        format!(
            "INSERT INTO object_pins SELECT printf('%064d', x), 'backup_in_progress', '{id}', 0 FROM (WITH RECURSIVE c(x) AS (SELECT 1 UNION ALL SELECT x+1 FROM c WHERE x<201) SELECT x FROM c)"
        ),
    );
    store.backups.recover().unwrap();
}

#[test]
fn recovery_crash_boundaries() {
    use std::os::unix::process::ExitStatusExt;
    let scratch = ScratchDir::new("backup-recovery-cuts");
    for point in [
        "before-mark",
        "inside-mark",
        "marked",
        "unlinked",
        "directory-synced",
        "marker-unlinked",
        "marker-synced",
        "removed",
        "unpin",
        "before-finish",
        "finished",
    ] {
        let root = scratch.path().join(point);
        std::fs::create_dir(&root).unwrap();
        let status = std::process::Command::new(env::current_exe().unwrap())
            .args(["--exact", "backups::tests::recovery_cut_child", "--ignored"])
            .env("SB_BACKUP_RECOVERY_CUT", point)
            .env("SB_BACKUP_RECOVERY_ROOT", &root)
            .status()
            .unwrap();
        assert_eq!(status.signal(), Some(9), "{point}");
        let paths = StorePaths::new(root).unwrap();
        let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
        store.backups.recover().unwrap();
        let id = "00000000-0000-4000-8000-000000000009";
        assert_eq!(generation_row(&store, id).unwrap().0, "failed");
        assert!(!paths.backups().join(id).exists());
        assert!(backup_pins(&store).is_empty());
        assert_ne!(
            store.collect_garbage(GC_GRACE_PERIOD).unwrap().blocked,
            Some(GcGate::BackupInProgress)
        );
    }
}

#[test]
fn retention_preserves_sparse_daily_and_weekly_buckets() {
    let (_scratch, paths, store) = open("backup-retention-sparse");
    const HOUR: i64 = 3_600_000;
    const DAY: i64 = 24 * HOUR;
    // Eight old weekly buckets, then thirty hourly buckets crossing midnight.
    for n in 0..8 {
        let (_, path) = seed_generation(
            &store,
            paths.backups(),
            n,
            (45 + n as i64 * 7) * DAY,
            "complete",
        );
        std::fs::create_dir(&path).unwrap();
        own_fixture(&store, &path);
    }
    for n in 0..30 {
        let (_, path) = seed_generation(
            &store,
            paths.backups(),
            100 + n,
            100 * DAY + n as i64 * HOUR,
            "complete",
        );
        std::fs::create_dir(&path).unwrap();
        own_fixture(&store, &path);
    }
    store.backups.retain().unwrap();
    for n in 0..8 {
        let id = format!("00000000-0000-4000-8000-{n:012x}");
        assert_eq!(generation_row(&store, &id).is_some(), n >= 3, "old {n}");
    }
    for n in 0..30 {
        let id = format!("00000000-0000-4000-8000-{:012x}", 100 + n);
        assert_eq!(generation_row(&store, &id).is_some(), n >= 6, "hour {n}");
    }
}

#[test]
fn retention_keeps_weekly_buckets_across_monday_boundary() {
    let (_scratch, paths, store) = open("backup-retention-week-boundary");
    const DAY: i64 = 86_400_000;
    const HOUR: i64 = 3_600_000;
    // Day 74 is Monday. These straddle its UTC boundary and are too
    // old for the hourly and daily selections.
    for (id, time) in [(1, 74 * DAY - 1), (2, 74 * DAY)] {
        let (_, path) = seed_generation(&store, paths.backups(), id, time, "complete");
        std::fs::create_dir(&path).unwrap();
        own_fixture(&store, &path);
    }
    for n in 0..6 {
        let (_, path) = seed_generation(
            &store,
            paths.backups(),
            10 + n,
            (90 + n as i64) * DAY,
            "complete",
        );
        std::fs::create_dir(&path).unwrap();
        own_fixture(&store, &path);
    }
    for n in 0..30 {
        let (_, path) = seed_generation(
            &store,
            paths.backups(),
            100 + n,
            100 * DAY + n as i64 * HOUR,
            "complete",
        );
        std::fs::create_dir(&path).unwrap();
        own_fixture(&store, &path);
    }
    store.backups.retain().unwrap();
    for id in [1, 2] {
        let id = format!("00000000-0000-4000-8000-{id:012x}");
        assert!(generation_row(&store, &id).is_some());
    }
}

#[test]
fn concurrent_snapshot_cannot_recover_a_live_generation() {
    let (_scratch, _paths, store) = open("backup-recovery-concurrent");
    let store = Arc::new(store);
    let gate = Arc::new(Barrier::new(2));
    store.backups.set_pauses(Pauses {
        after_snapshot_file: Some(gate.clone()),
        ..Pauses::default()
    });
    let worker = {
        let store = Arc::clone(&store);
        std::thread::spawn(move || store.backup_snapshot(BackupDestination::DataRoot))
    };
    gate.wait();
    let result = store.backup_snapshot(BackupDestination::DataRoot);
    assert!(matches!(result, Err(StoreError::Conflict { .. })));
    gate.wait();
    let snapshot = worker.join().unwrap().unwrap();
    assert_eq!(
        generation_row(&store, snapshot.generation_id()).unwrap().0,
        "in_progress"
    );
    assert!(snapshot.state_db().is_file());
}

#[test]
fn abandoned_backup_recovery_releases_real_gc_gate() {
    let (_scratch, _paths, store) = open("backup-recovery-gc");
    let (id, sha) = object(&store, PAYLOAD);
    pin(&store, &sha, "gc_grace", 0);
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    assert_eq!(
        store.collect_garbage(GC_GRACE_PERIOD).unwrap().blocked,
        Some(GcGate::BackupInProgress)
    );
    drop(snapshot);
    store.backups.recover().unwrap();
    let report = store.collect_garbage(GC_GRACE_PERIOD).unwrap();
    assert_eq!(report.blocked, None);
    assert_eq!(report.removed, 1);
    assert!(!store.objects().exists(&id).unwrap());
}

#[test]
fn retention_breaks_equal_timestamp_ties_by_generation_id() {
    let (_scratch, paths, store) = open("backup-retention-ties");
    for id in [1, 2] {
        let (_, path) = seed_generation(&store, paths.backups(), id, 0, "complete");
        std::fs::create_dir(&path).unwrap();
        own_fixture(&store, &path);
    }
    for n in 0..30 {
        let (_, path) = seed_generation(
            &store,
            paths.backups(),
            100 + n,
            864_000_000 + n as i64 * 3_600_000,
            "complete",
        );
        std::fs::create_dir(&path).unwrap();
        own_fixture(&store, &path);
    }
    store.backups.retain().unwrap();
    assert!(generation_row(&store, "00000000-0000-4000-8000-000000000001").is_none());
    assert!(generation_row(&store, "00000000-0000-4000-8000-000000000002").is_some());
}

#[test]
fn recovery_refuses_a_marker_on_a_complete_generation() {
    let (_scratch, paths, store) = open("backup-recovery-corrupt-marker");
    let (id, path) = seed_generation(&store, paths.backups(), 5, 0, "complete");
    std::fs::create_dir(&path).unwrap();
    sql(
        &store,
        format!("INSERT INTO backup_cleanup_pending VALUES ('{id}')"),
    );
    assert!(matches!(
        store.backups.recover(),
        Err(StoreError::Corrupt { .. })
    ));
    assert!(path.exists());
    assert_eq!(generation_row(&store, &id).unwrap().0, "complete");
}

#[test]
fn unrelated_external_uuid_directory_is_never_removed() {
    let (_scratch, paths, store) = open("backup-unrelated-directory");
    let external = paths.data_root().parent().unwrap().join("external-owner");
    std::fs::create_dir(&external).unwrap();
    let (id, path) = seed_generation(&store, &external, 17, 0, "in_progress");
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("important"), b"unrelated").unwrap();
    sql(
        &store,
        format!("INSERT INTO object_pins VALUES ('one','backup_in_progress','{id}',0)"),
    );
    assert!(matches!(
        store.backup_snapshot(BackupDestination::DataRoot),
        Err(StoreError::Corrupt { .. })
    ));
    assert_eq!(std::fs::read(path.join("important")).unwrap(), b"unrelated");
    assert_eq!(backup_pins(&store), ["one"]);
    assert!(generation_row(&store, &id).is_some());
}

#[test]
#[ignore = "spawned by marker_creation_cuts_recover"]
fn marker_creation_cut_child() {
    let root = env::var("SB_BACKUP_RECOVERY_ROOT").unwrap();
    let paths = StorePaths::new(Path::new(&root)).unwrap();
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    store.backup_snapshot(BackupDestination::DataRoot).unwrap();
}

#[test]
fn marker_creation_cuts_recover() {
    use std::os::unix::process::ExitStatusExt;
    let scratch = ScratchDir::new("backup-owner-cuts");
    for point in [
        "owner-before-create",
        "owner-dir",
        "owner-create",
        "owner-write",
        "owner-sync",
        "owner-dir-sync",
    ] {
        let root = scratch.path().join(point);
        std::fs::create_dir(&root).unwrap();
        let status = std::process::Command::new(env::current_exe().unwrap())
            .args([
                "--exact",
                "backups::tests::marker_creation_cut_child",
                "--ignored",
            ])
            .env("SB_BACKUP_RECOVERY_CUT", point)
            .env("SB_BACKUP_RECOVERY_ROOT", &root)
            .status()
            .unwrap();
        assert_eq!(status.signal(), Some(9), "{point}");
        let paths = StorePaths::new(root).unwrap();
        let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
        store.backups.recover().unwrap();
        let (state, location): (String, String) = reading(&store, |tx| {
            tx.query_row("SELECT state, location FROM backup_generations", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .map_err(bug)
        });
        assert_eq!(state, "failed");
        assert!(!Path::new(&location).exists());
    }
}

#[test]
fn ownership_marker_failures_report_io_and_recover() {
    for op in [FsOp::Create, FsOp::Write, FsOp::SyncFile, FsOp::SyncDir] {
        let (_scratch, paths, store) = open("backup-owner-failure");
        let fault = Arc::new(FaultFs::new(
            real_fs(),
            op,
            1,
            FaultKind::Kind(std::io::ErrorKind::PermissionDenied),
        ));
        let backups = faulty_backups(&store, &paths, fault);
        let err = backups.snapshot(BackupDestination::DataRoot).unwrap_err();
        assert_eq!(err.io_kind(), Some(std::io::ErrorKind::PermissionDenied));
        let state: String = reading(&store, |tx| {
            tx.query_row("SELECT state FROM backup_generations", [], |r| r.get(0))
                .map_err(bug)
        });
        assert_eq!(state, "in_progress");
        drop(backups);
        store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    }
}

#[test]
fn missing_or_wrong_owner_marker_preserves_nonempty_directory_and_pins() {
    for wrong in [false, true] {
        let (_scratch, paths, store) = open("backup-owner-mismatch");
        let (id, path) = seed_generation(&store, paths.backups(), 21, 0, "in_progress");
        std::fs::create_dir(&path).unwrap();
        own_fixture(&store, &path);
        if wrong {
            std::fs::write(super::owner_path(&path), b"wrong").unwrap();
        } else {
            std::fs::remove_file(super::owner_path(&path)).unwrap();
        }
        sql(
            &store,
            format!("INSERT INTO object_pins VALUES ('one','backup_in_progress','{id}',0)"),
        );
        assert!(matches!(
            store.backup_snapshot(BackupDestination::DataRoot),
            Err(StoreError::Corrupt { .. })
        ));
        assert!(path.join("payload").exists());
        assert_eq!(backup_pins(&store), ["one"]);
        assert_eq!(generation_row(&store, &id).unwrap().0, "in_progress");
    }
}

#[test]
fn partial_owner_marker_is_recoverable_only_without_other_content() {
    let (_scratch, paths, store) = open("backup-partial-owner");
    let (id, path) = seed_generation(&store, paths.backups(), 22, 0, "in_progress");
    std::fs::create_dir(&path).unwrap();
    own_fixture(&store, &path);
    std::fs::write(super::owner_path(&path), b"9f17c0aa").unwrap();
    assert!(matches!(
        store.backups.recover(),
        Err(StoreError::Corrupt { .. })
    ));
    std::fs::remove_dir_all(&path).unwrap();
    store.backups.recover().unwrap();
    assert!(!path.exists());
    assert_eq!(generation_row(&store, &id).unwrap().0, "failed");
}

#[test]
fn changed_parent_symlink_cannot_redirect_recovery() {
    use std::os::unix::fs::symlink;
    let (_scratch, paths, store) = open("backup-owner-parent");
    let root = paths.data_root().parent().unwrap();
    let external = root.join("external");
    let replacement = root.join("replacement");
    std::fs::create_dir(&external).unwrap();
    std::fs::create_dir(&replacement).unwrap();
    let (id, path) = seed_generation(&store, &external, 23, 0, "in_progress");
    std::fs::create_dir(&path).unwrap();
    own_fixture(&store, &path);
    let moved = root.join("moved");
    std::fs::rename(&external, &moved).unwrap();
    std::fs::create_dir(replacement.join(&id)).unwrap();
    std::fs::write(replacement.join(&id).join("important"), b"keep").unwrap();
    symlink(&replacement, &external).unwrap();
    assert!(matches!(
        store.backups.recover(),
        Err(StoreError::Corrupt { .. })
    ));
    assert!(replacement.join(&id).join("important").exists());
    assert!(moved.join(&id).join("payload").exists());
}

#[test]
fn retention_sync_failure_retries_missing_generation_before_row_removal() {
    let (_scratch, paths, store) = open("backup-retention-sync");
    for n in 0..26 {
        let (_, path) =
            seed_generation(&store, paths.backups(), n, n as i64 * 3_600_000, "complete");
        std::fs::create_dir(&path).unwrap();
        own_fixture(&store, &path);
    }
    let fault = Arc::new(FaultFs::new(real_fs(), FsOp::SyncDir, 1, FaultKind::Eio));
    let backups = faulty_backups(&store, &paths, fault);
    assert!(matches!(
        backups.retain(),
        Err(StoreError::Unavailable { .. })
    ));
    let oldest = "00000000-0000-4000-8000-000000000000";
    assert!(!paths.backups().join(oldest).exists());
    assert!(generation_row(&store, oldest).is_some());
    store.backups.retain().unwrap();
    assert!(generation_row(&store, oldest).is_none());
}

#[test]
fn retention_refuses_legacy_nonempty_directory_without_proof() {
    let (_scratch, paths, store) = open("backup-legacy-retention");
    for n in 0..26 {
        let (_, path) =
            seed_generation(&store, paths.backups(), n, n as i64 * 3_600_000, "complete");
        std::fs::create_dir(&path).unwrap();
        if n == 0 {
            std::fs::write(path.join("legacy"), b"keep").unwrap();
        } else {
            own_fixture(&store, &path);
        }
    }
    assert!(matches!(
        store.backups.retain(),
        Err(StoreError::Corrupt { .. })
    ));
    let oldest = "00000000-0000-4000-8000-000000000000";
    assert_eq!(
        std::fs::read(paths.backups().join(oldest).join("legacy")).unwrap(),
        b"keep"
    );
    assert!(generation_row(&store, oldest).is_some());
}

#[test]
#[ignore = "spawned by retention_crash_retry"]
fn retention_cut_child() {
    let root = env::var("SB_BACKUP_RECOVERY_ROOT").unwrap();
    let paths = StorePaths::new(Path::new(&root)).unwrap();
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    for n in 0..26 {
        let (_, path) =
            seed_generation(&store, paths.backups(), n, n as i64 * 3_600_000, "complete");
        std::fs::create_dir(&path).unwrap();
        own_fixture(&store, &path);
    }
    store.backups.retain().unwrap();
}

#[test]
fn retention_crash_retry() {
    use std::os::unix::process::ExitStatusExt;
    let scratch = ScratchDir::new("backup-retention-cuts");
    for point in [
        "unlinked",
        "directory-synced",
        "marker-unlinked",
        "marker-synced",
        "retention-removed",
        "retention-finished",
    ] {
        let root = scratch.path().join(point);
        std::fs::create_dir(&root).unwrap();
        let status = std::process::Command::new(env::current_exe().unwrap())
            .args([
                "--exact",
                "backups::tests::retention_cut_child",
                "--ignored",
            ])
            .env("SB_BACKUP_RECOVERY_CUT", point)
            .env("SB_BACKUP_RECOVERY_ROOT", &root)
            .status()
            .unwrap();
        assert_eq!(status.signal(), Some(9), "{point}");
        let paths = StorePaths::new(root).unwrap();
        let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
        store.backups.retain().unwrap();
        let oldest = "00000000-0000-4000-8000-000000000000";
        assert!(!paths.backups().join(oldest).exists());
        assert!(generation_row(&store, oldest).is_none());
    }
}

#[test]
fn external_owned_generation_can_be_retained_and_parent_survives() {
    let (_scratch, paths, store) = open("backup-external-retention");
    let external = paths.data_root().parent().unwrap().join("external");
    std::fs::create_dir(&external).unwrap();
    for n in 0..26 {
        let (_, path) = seed_generation(&store, &external, n, n as i64 * 3_600_000, "complete");
        std::fs::create_dir(&path).unwrap();
        own_fixture(&store, &path);
    }
    store.backups.retain().unwrap();
    assert!(external.is_dir());
    assert!(
        !external
            .join("00000000-0000-4000-8000-000000000000")
            .exists()
    );
    assert!(
        external
            .join("00000000-0000-4000-8000-000000000019")
            .join("payload")
            .exists()
    );
}

#[test]
fn migration_22_keeps_legacy_generations_without_forging_ownership() {
    let (_scratch, paths) = scratch_paths("backup-owner-upgrade");
    let seams = test_seams(real_fs());
    let store = crate::store::open_with(
        &paths,
        &seams,
        crate::store::WRITER_PRAGMAS,
        crate::store::READER_PRAGMAS,
        &crate::store::PINNED_SQLITE,
        &crate::schema::MIGRATIONS[..21],
    )
    .unwrap();
    let (id, path) = seed_generation(&store, paths.backups(), 24, 0, "in_progress");
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("legacy"), b"keep").unwrap();
    store.close().unwrap();
    let store = crate::open(&paths, &seams).unwrap();
    assert!(matches!(
        store.backups.recover(),
        Err(StoreError::Corrupt { .. })
    ));
    assert!(path.join("legacy").exists());
    assert_eq!(generation_row(&store, &id).unwrap().0, "in_progress");
}

#[test]
fn torn_marker_write_is_retryable() {
    let (_scratch, paths, store) = open("backup-torn-owner");
    let fault = Arc::new(FaultFs::new(
        real_fs(),
        FsOp::Write,
        1,
        FaultKind::ShortWrite,
    ));
    let backups = faulty_backups(&store, &paths, fault);
    assert!(matches!(
        backups.snapshot(BackupDestination::DataRoot),
        Err(StoreError::Unavailable { .. })
    ));
    let (id, location): (String, String) = reading(&store, |tx| {
        tx.query_row(
            "SELECT generation_id, location FROM backup_generations",
            [],
            |r| Ok((r.get(0)?, r.get(1)?)),
        )
        .map_err(bug)
    });
    let marker = super::owner_path(Path::new(&location));
    assert_eq!(std::fs::read(&marker).unwrap().len(), 18);
    drop(backups);
    store.backups.recover().unwrap();
    assert!(!Path::new(&location).exists());
    assert_eq!(generation_row(&store, &id).unwrap().0, "failed");
}

#[test]
fn interrupted_recursive_removal_can_be_retried() {
    let (_scratch, paths, store) = open("backup-partial-remove");
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let id = snapshot.generation_id().to_owned();
    let path = snapshot.location().to_owned();
    std::fs::write(path.join("payload"), b"keep until retry").unwrap();
    sql(
        &store,
        format!("INSERT INTO object_pins VALUES ('one','backup_in_progress','{id}',0)"),
    );
    drop(snapshot);
    let fault = Arc::new(FaultFs::new(
        real_fs(),
        FsOp::RemoveDirAll,
        1,
        FaultKind::PartialRemove,
    ));
    let backups = faulty_backups(&store, &paths, fault);
    assert!(matches!(
        backups.recover(),
        Err(StoreError::Unavailable { .. })
    ));
    assert!(path.join("payload").exists());
    assert!(super::owner_path(&path).is_file());
    assert_eq!(backup_pins(&store), ["one"]);
    assert_eq!(generation_row(&store, &id).unwrap().0, "failed");
    drop(backups);
    store.backups.recover().unwrap();
    assert!(!path.exists());
    assert!(backup_pins(&store).is_empty());
}

#[test]
fn retention_retries_a_partial_recursive_removal() {
    let (_scratch, paths, store) = open("backup-retention-partial");
    for n in 0..26 {
        let (_, path) =
            seed_generation(&store, paths.backups(), n, n as i64 * 3_600_000, "complete");
        std::fs::create_dir(&path).unwrap();
        own_fixture(&store, &path);
        std::fs::write(path.join("state.sqlite"), b"snapshot").unwrap();
    }
    let fault = Arc::new(FaultFs::new(
        real_fs(),
        FsOp::RemoveDirAll,
        1,
        FaultKind::PartialRemove,
    ));
    let backups = faulty_backups(&store, &paths, fault);
    assert!(matches!(
        backups.retain(),
        Err(StoreError::Unavailable { .. })
    ));
    let id = "00000000-0000-4000-8000-000000000000";
    let path = paths.backups().join(id);
    assert!(path.join("payload").exists());
    assert!(!path.join("state.sqlite").exists());
    assert!(super::owner_path(&path).is_file());
    assert!(generation_row(&store, id).is_some());
    store.backups.retain().unwrap();
    assert!(!path.exists());
    assert!(!super::owner_path(&path).exists());
    assert!(generation_row(&store, id).is_none());
}

#[test]
fn preexisting_directory_is_not_granted_ownership() {
    let (_scratch, paths, store) = open("backup-existing-directory");
    let path = paths.backups().join("00000000-0000-4000-8000-000000000001");
    std::fs::create_dir(&path).unwrap();
    std::fs::write(path.join("important"), b"keep").unwrap();
    let error = store
        .backups
        .write_snapshot(&path, "9f17c0aa-0000-4000-8000-000000000001")
        .err()
        .unwrap();
    assert!(matches!(error, StoreError::Conflict { .. }));
    assert!(!super::owner_path(&path).exists());
    assert!(path.join("important").exists());
}

#[test]
fn startup_retries_proved_verifier_scratch() {
    let (_temp, paths, store) = open("scratch-startup");
    let id = "00000000-0000-4000-8000-000000000041";
    let token = "00000000-0000-4000-8000-000000000042";
    let root = real_fs().open_confined_dir(paths.recovery()).unwrap();
    let scratch = super::scratch::prepare(root.as_ref(), id, token).unwrap();
    let mut file = scratch.create(Path::new("state.sqlite")).unwrap();
    std::io::Write::write_all(&mut file, b"owned").unwrap();
    file.sync_all().unwrap();
    drop(file);
    scratch.sync().unwrap();
    sql(
        &store,
        format!(
            "INSERT INTO verifier_scratch SELECT '{id}', '{token}', value FROM store_meta WHERE key='incarnation'"
        ),
    );
    for chunk in (0u16..401).collect::<Vec<_>>().chunks(200) {
        super::ledger::insert(
            &store.backups.writer,
            id.to_owned(),
            chunk
                .iter()
                .map(|n| {
                    let mut hash = [0; 32];
                    hash[..2].copy_from_slice(&n.to_be_bytes());
                    super::manifest::Entry {
                        sha256: hash,
                        bytes: u64::MAX,
                    }
                })
                .collect(),
        )
        .unwrap();
    }
    drop(scratch);
    store.close().unwrap();
    let reopened = crate::open(&paths, &test_seams(real_fs())).unwrap();
    let count: i64 = reading(&reopened, |tx| {
        tx.query_row("SELECT count(*) FROM verifier_scratch", [], |r| r.get(0))
            .map_err(bug)
    });
    assert_eq!(count, 0);
    let count: i64 = reading(&reopened, |tx| {
        tx.query_row("SELECT count(*) FROM verifier_objects", [], |r| r.get(0))
            .map_err(bug)
    });
    assert_eq!(count, 0);
    assert!(!paths.recovery().join(format!("verifier-{id}")).exists());
}

#[test]
fn verifier_ledger_batches_reconcile_and_reject_duplicates() {
    let (_temp, paths, store) = open("verifier-ledger");
    let id = "owner";
    sql(
        &store,
        "INSERT INTO verifier_scratch VALUES ('owner','token','current')".into(),
    );
    let entry = |n: u16| {
        let mut sha256 = [0; 32];
        sha256[..2].copy_from_slice(&n.to_be_bytes());
        super::manifest::Entry {
            sha256,
            bytes: u64::MAX - u64::from(n),
        }
    };
    for start in [0, 200, 400] {
        super::ledger::insert(
            &store.backups.writer,
            id.into(),
            (start..(start + 200).min(401)).map(entry).collect(),
        )
        .unwrap();
    }
    assert!(
        super::ledger::insert(&store.backups.writer, id.into(), vec![entry(500), entry(1)])
            .is_err()
    );
    assert!(
        super::ledger::insert(
            &store.backups.writer,
            id.into(),
            (500..701).map(entry).collect()
        )
        .is_err()
    );
    let conn = Connection::open(paths.state_db()).unwrap();
    assert!(
        super::ledger::reference(
            &store.backups.writer,
            id.into(),
            (0..201).map(|n| entry(n).sha256).collect()
        )
        .is_err()
    );
    assert!(
        super::ledger::page(&conn, id, None)
            .unwrap()
            .iter()
            .all(|row| !row.referenced)
    );
    let hash = entry(1).sha256;
    assert!(
        super::ledger::reference(
            &store.backups.writer,
            id.into(),
            vec![hash, entry(500).sha256]
        )
        .is_err()
    );
    assert!(!super::ledger::page(&conn, id, None).unwrap()[1].referenced);
    super::ledger::reference(&store.backups.writer, id.into(), vec![hash, hash]).unwrap();
    let first = super::ledger::page(&conn, id, None).unwrap();
    assert_eq!(first.len(), 200);
    assert_eq!(first[0].entry.bytes, u64::MAX);
    assert!(first[1].referenced);
    let second = super::ledger::page(&conn, id, Some(first.last().unwrap().entry.sha256)).unwrap();
    assert_eq!(second.len(), 200);
    let third = super::ledger::page(&conn, id, Some(second.last().unwrap().entry.sha256)).unwrap();
    assert_eq!(third.len(), 1);
    assert_eq!(third[0].entry.sha256, entry(400).sha256);
    super::ledger::purge(&store.backups.writer, id).unwrap();
    assert!(super::ledger::page(&conn, id, None).unwrap().is_empty());
}

#[test]
fn recorded_verification_checks_real_empty_backup() {
    let (_scratch, _paths, store) = open("recorded-verification-empty");
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let receipt = store.complete_backup(snapshot).unwrap();
    let cancelled = std::sync::atomic::AtomicBool::new(false);
    assert!(matches!(
        store.verify_recorded_backup(receipt.generation_id(), &cancelled),
        Ok(crate::BackupImageInspection::Inspected { .. })
    ));
}

#[test]
fn external_restore_verifies_into_a_fresh_store() {
    use std::sync::atomic::AtomicBool;
    let (_source_scratch, _source_paths, source) = open("external-restore-source");
    let receipt = source
        .complete_backup(source.backup_snapshot(BackupDestination::DataRoot).unwrap())
        .unwrap();
    let (_fresh_scratch, _fresh_paths, fresh) = open("external-restore-fresh");
    let cancel = AtomicBool::new(false);
    let selection = fresh
        .verify_external_backup(
            receipt.location().parent().unwrap(),
            receipt.generation_id(),
            &cancel,
        )
        .unwrap()
        .unwrap();
    assert_eq!(selection.generation_id(), receipt.generation_id());
    let moved = receipt.location().with_extension("moved");
    std::fs::rename(receipt.location(), &moved).unwrap();
    assert!(
        selection
            .source()
            .open_regular(Path::new("manifest.json"))
            .is_ok()
    );
    std::fs::rename(&moved, receipt.location()).unwrap();
    let manifest = receipt.location().join("manifest.json");
    let original = std::fs::read(&manifest).unwrap();
    std::fs::write(&manifest, b"{}").unwrap();
    assert!(
        fresh
            .verify_external_backup(
                receipt.location().parent().unwrap(),
                receipt.generation_id(),
                &cancel
            )
            .is_err()
    );
    std::fs::write(&manifest, original).unwrap();
    let saved = manifest.with_extension("saved");
    assert!(
        fresh
            .backups
            .verify_external_after_prepare(
                receipt.location().parent().unwrap(),
                receipt.generation_id(),
                &cancel,
                || {
                    std::fs::rename(&manifest, &saved).unwrap();
                    std::fs::write(&manifest, b"{}").unwrap();
                }
            )
            .is_err()
    );
    std::fs::remove_file(&manifest).unwrap();
    std::fs::rename(&saved, &manifest).unwrap();
    assert!(
        fresh
            .backups
            .verify_external_after_prepare(
                receipt.location().parent().unwrap(),
                receipt.generation_id(),
                &cancel,
                || {
                    std::fs::write(&manifest, b"{}").unwrap();
                }
            )
            .is_err()
    );
    assert!(
        fresh
            .verify_external_backup(
                receipt.location().parent().unwrap(),
                receipt.generation_id(),
                &AtomicBool::new(true)
            )
            .unwrap()
            .is_none()
    );
}

#[test]
fn external_discovery_streams_complete_generations_and_checks_objects() {
    use std::ops::ControlFlow;
    use std::sync::atomic::AtomicBool;
    let (_scratch, _paths, source) = open("external-discovery-source");
    let mut hashes = Vec::new();
    for n in 0..201 {
        let (id, hash) = object(&source, format!("external {n}").as_bytes());
        reference(&source, &id, "published");
        hashes.push(hash);
    }
    let receipt = source
        .complete_backup(source.backup_snapshot(BackupDestination::DataRoot).unwrap())
        .unwrap();
    let parent = receipt.location().parent().unwrap();
    let (_other_scratch, _other_paths, other) = open("external-discovery-other");
    let other_receipt = other
        .complete_backup(
            other
                .backup_snapshot(BackupDestination::Directory(parent.to_path_buf()))
                .unwrap(),
        )
        .unwrap();
    let incomplete = parent.join("00000000-0000-4000-8000-000000000099");
    std::fs::create_dir(&incomplete).unwrap();
    let (_fresh_scratch, _fresh_paths, fresh) = open("external-discovery-fresh");
    let cancel = AtomicBool::new(false);
    let mut found = Vec::new();
    assert!(
        fresh
            .discover_external_backups(parent, &cancel, |selection| {
                found.push(selection.generation_id().to_owned());
                ControlFlow::Continue(())
            })
            .unwrap()
            .is_continue()
    );
    found.sort();
    let mut expected = vec![
        receipt.generation_id().to_owned(),
        other_receipt.generation_id().to_owned(),
    ];
    expected.sort();
    assert_eq!(found, expected);
    let hash = &hashes[0];
    let object = receipt.location().join(format!(
        "objects/sha256/{}/{}/{}",
        &hash[..2],
        &hash[2..4],
        hash
    ));
    std::fs::write(object, b"substituted").unwrap();
    assert!(
        fresh
            .verify_external_backup(parent, receipt.generation_id(), &cancel)
            .is_err()
    );
    let mut count = 0;
    let _ = fresh
        .discover_external_backups(parent, &cancel, |_| {
            count += 1;
            ControlFlow::Continue(())
        })
        .unwrap();
    assert_eq!(count, 1);
    std::fs::write(other_receipt.location().join("state.sqlite"), b"damaged").unwrap();
    let mut remaining = 0;
    assert!(
        fresh
            .discover_external_backups(parent, &cancel, |_| {
                remaining += 1;
                ControlFlow::Continue(())
            })
            .unwrap()
            .is_continue()
    );
    assert_eq!(remaining, 0);
}

#[test]
fn external_restore_propagates_io_and_cleanup_errors() {
    use std::ops::ControlFlow;
    use std::sync::atomic::AtomicBool;
    let (_scratch, _paths, source) = open("external-errors-source");
    let (id, _) = object(&source, PAYLOAD);
    reference(&source, &id, "published");
    let receipt = source
        .complete_backup(source.backup_snapshot(BackupDestination::DataRoot).unwrap())
        .unwrap();
    let (_fresh_scratch, fresh_paths, fresh) = open("external-errors-fresh");
    let parent = receipt.location().parent().unwrap();
    let cancel = AtomicBool::new(false);
    for op in [FsOp::Read, FsOp::RemoveDirAll] {
        let fs = Arc::new(FaultFs::new(real_fs(), op, 1, FaultKind::Eio));
        let failing = faulty_backups(&fresh, &fresh_paths, fs);
        assert!(
            failing
                .verify_external(parent, receipt.generation_id(), &cancel)
                .is_err()
        );
        assert!(
            fresh
                .verify_external_backup(parent, receipt.generation_id(), &cancel)
                .unwrap()
                .is_some()
        );
    }
    let fs = Arc::new(FaultFs::new(real_fs(), FsOp::ReadDir, 1, FaultKind::Eio));
    let failing = faulty_backups(&fresh, &fresh_paths, fs);
    assert!(
        failing
            .discover_external(parent, &cancel, |_| ControlFlow::Continue(()))
            .is_err()
    );
    sql(&fresh, "CREATE TRIGGER fail_external BEFORE INSERT ON verifier_objects BEGIN SELECT RAISE(ABORT, 'injected'); END".into());
    assert!(
        fresh
            .verify_external_backup(parent, receipt.generation_id(), &cancel)
            .is_err()
    );
    assert!(
        fresh
            .discover_external_backups(parent, &cancel, |_| ControlFlow::Continue(()))
            .is_err()
    );
    sql(&fresh, "DROP TRIGGER fail_external".into());
    assert!(
        fresh
            .verify_external_backup(parent, receipt.generation_id(), &cancel)
            .unwrap()
            .is_some()
    );
}

#[test]
fn external_discovery_propagates_not_found_cleanup_failure() {
    use std::ops::ControlFlow;
    use std::sync::atomic::AtomicBool;
    let (_scratch, _paths, source) = open("external-cleanup-provenance-source");
    let receipt = source
        .complete_backup(source.backup_snapshot(BackupDestination::DataRoot).unwrap())
        .unwrap();
    let (_fresh_scratch, fresh_paths, fresh) = open("external-cleanup-provenance-fresh");
    let fs = Arc::new(FaultFs::new(
        real_fs(),
        FsOp::RemoveDirAll,
        1,
        FaultKind::Kind(std::io::ErrorKind::NotFound),
    ));
    let failing = faulty_backups(&fresh, &fresh_paths, fs);
    let result = failing.discover_external(
        receipt.location().parent().unwrap(),
        &AtomicBool::new(false),
        |_| ControlFlow::Continue(()),
    );
    assert!(result.is_err());
}

#[test]
fn external_verification_and_discovery_conflict_with_active_backup() {
    use std::ops::ControlFlow;
    use std::sync::atomic::AtomicBool;
    let (_scratch, _paths, store) = open("external-active-backup");
    let receipt = store
        .complete_backup(store.backup_snapshot(BackupDestination::DataRoot).unwrap())
        .unwrap();
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let parent = receipt.location().parent().unwrap();
    let cancel = AtomicBool::new(false);
    assert!(matches!(
        store.verify_external_backup(parent, receipt.generation_id(), &cancel),
        Err(StoreError::Conflict { .. })
    ));
    assert!(matches!(
        store.discover_external_backups(parent, &cancel, |_| ControlFlow::Continue(())),
        Err(StoreError::Conflict { .. })
    ));
    drop(snapshot);
    let mut visited = false;
    assert!(
        store
            .discover_external_backups(parent, &cancel, |_| {
                visited = true;
                assert!(matches!(
                    store.backup_snapshot(BackupDestination::DataRoot),
                    Err(StoreError::Conflict { .. })
                ));
                ControlFlow::Break(())
            })
            .unwrap()
            .is_break()
    );
    assert!(visited);
}

#[test]
fn recorded_verification_pages_real_objects_and_rejects_corruption() {
    use std::sync::atomic::AtomicBool;
    let (_scratch, _paths, store) = open("recorded-verification-objects");
    let mut hashes = Vec::new();
    for n in 0..201 {
        let (id, hash) = object(&store, format!("object {n}").as_bytes());
        reference(&store, &id, "published");
        hashes.push(hash);
    }
    let receipt = store
        .complete_backup(store.backup_snapshot(BackupDestination::DataRoot).unwrap())
        .unwrap();
    let cancel = AtomicBool::new(false);
    let verify = || store.verify_recorded_backup(receipt.generation_id(), &cancel);
    assert!(matches!(
        verify(),
        Ok(crate::BackupImageInspection::Inspected { .. })
    ));
    let hash = &hashes[0];
    let path = receipt.location().join(format!(
        "objects/sha256/{}/{}/{}",
        &hash[..2],
        &hash[2..4],
        hash
    ));
    let original = std::fs::read(&path).unwrap();
    for bytes in [
        Vec::new(),
        vec![b'x'; original.len()],
        [original.as_slice(), b"x"].concat(),
    ] {
        std::fs::write(&path, bytes).unwrap();
        assert!(verify().is_err());
    }
    std::fs::remove_file(&path).unwrap();
    assert!(verify().is_err());
    std::fs::write(&path, original).unwrap();
    let manifest = receipt.location().join("manifest.json");
    let original = std::fs::read(&manifest).unwrap();
    std::fs::write(&manifest, b"{}").unwrap();
    assert!(verify().is_err());
    std::fs::write(manifest, original).unwrap();
    assert!(matches!(
        verify(),
        Ok(crate::BackupImageInspection::Inspected { .. })
    ));
}

#[test]
fn recorded_verification_cancellation_and_cleanup_failure() {
    use std::sync::atomic::AtomicBool;
    let (_scratch, paths, store) = open("recorded-verification-errors");
    let (id, _) = object(&store, PAYLOAD);
    reference(&store, &id, "published");
    let receipt = store
        .complete_backup(store.backup_snapshot(BackupDestination::DataRoot).unwrap())
        .unwrap();
    assert!(matches!(
        store.verify_recorded_backup(receipt.generation_id(), &AtomicBool::new(true)),
        Ok(crate::BackupImageInspection::Cancelled)
    ));
    let fs = Arc::new(FaultFs::new(
        real_fs(),
        FsOp::RemoveDirAll,
        1,
        FaultKind::Eio,
    ));
    let failing = faulty_backups(&store, &paths, fs);
    assert!(
        failing
            .verify_recorded(receipt.generation_id(), &AtomicBool::new(false))
            .is_err()
    );
    assert!(matches!(
        store.verify_recorded_backup(receipt.generation_id(), &AtomicBool::new(false)),
        Ok(crate::BackupImageInspection::Inspected { .. })
    ));
}

#[test]
fn recorded_verification_rejects_record_and_symlink_substitution() {
    use std::sync::atomic::AtomicBool;
    let (_scratch, paths, store) = open("recorded-verification-substitution");
    let (id, hash) = object(&store, PAYLOAD);
    reference(&store, &id, "published");
    let receipt = store
        .complete_backup(store.backup_snapshot(BackupDestination::DataRoot).unwrap())
        .unwrap();
    let cancel = AtomicBool::new(false);
    for (change, restore) in [
        ("state='failed'", "state='complete'"),
        ("object_count=2", "object_count=1"),
        ("started_at=started_at+1", "started_at=started_at-1"),
        ("total_bytes=total_bytes+1", "total_bytes=total_bytes-1"),
    ] {
        sql(&store, format!("UPDATE backup_generations SET {change}"));
        assert!(
            store
                .verify_recorded_backup(receipt.generation_id(), &cancel)
                .is_err()
        );
        sql(&store, format!("UPDATE backup_generations SET {restore}"));
    }
    let path = receipt.location().join(format!(
        "objects/sha256/{}/{}/{}",
        &hash[..2],
        &hash[2..4],
        hash
    ));
    let renamed = path.with_extension("held");
    std::fs::rename(&path, &renamed).unwrap();
    std::os::unix::fs::symlink(&renamed, &path).unwrap();
    assert!(
        store
            .verify_recorded_backup(receipt.generation_id(), &cancel)
            .is_err()
    );
    std::fs::remove_file(&path).unwrap();
    std::fs::rename(&renamed, &path).unwrap();
    let fs = Arc::new(FaultFs::new(real_fs(), FsOp::Read, 1, FaultKind::Eio));
    let failing = faulty_backups(&store, &paths, fs);
    assert!(
        failing
            .verify_recorded(receipt.generation_id(), &cancel)
            .is_err()
    );
    sql(&store, "CREATE TRIGGER fail_verifier_insert BEFORE INSERT ON verifier_objects BEGIN SELECT RAISE(ABORT, 'injected'); END".into());
    assert!(
        store
            .verify_recorded_backup(receipt.generation_id(), &cancel)
            .is_err()
    );
    sql(&store, "DROP TRIGGER fail_verifier_insert".into());
    assert!(matches!(
        store.verify_recorded_backup(receipt.generation_id(), &cancel),
        Ok(crate::BackupImageInspection::Inspected { .. })
    ));
}

#[test]
fn recorded_verification_stream_cancellation_and_mutation() {
    use super::recorded::StreamPhase;
    use std::sync::atomic::{AtomicBool, Ordering};
    let (_scratch, _paths, store) = open("recorded-verification-streams");
    let payload = vec![b'a'; 131072];
    let (id, hash) = object(&store, &payload);
    reference(&store, &id, "published");
    let receipt = store
        .complete_backup(store.backup_snapshot(BackupDestination::DataRoot).unwrap())
        .unwrap();
    let path = receipt.location().join(format!(
        "objects/sha256/{}/{}/{}",
        &hash[..2],
        &hash[2..4],
        hash
    ));
    for target in [StreamPhase::Manifest, StreamPhase::Object] {
        let cancel = AtomicBool::new(false);
        let mut reached = false;
        let result =
            store
                .backups
                .verify_recorded_with(receipt.generation_id(), &cancel, |phase| {
                    if phase == target {
                        reached = true;
                        cancel.store(true, Ordering::Release);
                    }
                });
        assert!(reached);
        assert!(matches!(
            result,
            Ok(crate::BackupImageInspection::Cancelled)
        ));
    }
    let mut changed = false;
    let cancel = AtomicBool::new(false);
    let result = store
        .backups
        .verify_recorded_with(receipt.generation_id(), &cancel, |phase| {
            if phase == StreamPhase::Object && !changed {
                use std::io::{Seek, SeekFrom, Write};
                let mut file = std::fs::OpenOptions::new().write(true).open(&path).unwrap();
                file.seek(SeekFrom::Start(100000)).unwrap();
                file.write_all(b"changed").unwrap();
                changed = true;
            }
        });
    assert!(changed);
    assert!(result.is_err());
    std::fs::write(&path, payload).unwrap();
    assert!(matches!(
        store.verify_recorded_backup(receipt.generation_id(), &cancel),
        Ok(crate::BackupImageInspection::Inspected { .. })
    ));
}

#[test]
fn recorded_verification_rejects_generation_root_symlink() {
    use super::recorded::StreamPhase;
    use std::sync::atomic::AtomicBool;
    let (_scratch, _paths, store) = open("recorded-verification-root");
    let receipt = store
        .complete_backup(store.backup_snapshot(BackupDestination::DataRoot).unwrap())
        .unwrap();
    let root = receipt.location();
    let held = root.with_extension("held");
    std::fs::rename(root, &held).unwrap();
    std::os::unix::fs::symlink(&held, root).unwrap();
    assert!(
        store
            .verify_recorded_backup(receipt.generation_id(), &AtomicBool::new(false))
            .is_err()
    );
    std::fs::remove_file(root).unwrap();
    std::fs::rename(held, root).unwrap();
    let held = root.with_extension("held");
    let mut replaced = false;
    let result = store.backups.verify_recorded_with(
        receipt.generation_id(),
        &AtomicBool::new(false),
        |phase| {
            if phase == StreamPhase::Manifest && !replaced {
                std::fs::rename(root, &held).unwrap();
                std::os::unix::fs::symlink(&held, root).unwrap();
                replaced = true;
            }
        },
    );
    assert!(replaced);
    assert!(result.is_err());
    std::fs::remove_file(root).unwrap();
    std::fs::rename(held, root).unwrap();
    assert!(matches!(
        store.verify_recorded_backup(receipt.generation_id(), &AtomicBool::new(false)),
        Ok(crate::BackupImageInspection::Inspected { .. })
    ));
}

#[test]
fn reconciliation_accepts_real_empty_backup() {
    use sha2::{Digest, Sha256};
    use std::ops::ControlFlow;
    use std::sync::atomic::AtomicBool;

    let (_scratch, _paths, store) = open("reconciliation-empty");
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let receipt = store.complete_backup(snapshot).unwrap();
    let database = std::fs::read(receipt.state_db()).unwrap();
    let manifest = std::fs::read(receipt.location().join("manifest.json")).unwrap();
    let digest = Sha256::digest(&database).into();
    let cancelled = AtomicBool::new(false);
    let result = store.backups.reconcile_image(
        receipt.location(),
        manifest.as_slice(),
        super::reconciliation::Expected {
            database: super::manifest::Entry {
                sha256: digest,
                bytes: database.len() as u64,
            },
            object_count: 0,
            total_bytes: database.len() as u64,
        },
        &cancelled,
        |_| ControlFlow::Continue(()),
    );
    assert!(matches!(
        result,
        Ok(crate::BackupImageInspection::Inspected { .. })
    ));
}

#[test]
fn reconciliation_accepts_real_backup_and_retains_evidence_on_cleanup_failure() {
    use sha2::{Digest, Sha256};
    use std::ops::ControlFlow;
    use std::sync::atomic::AtomicBool;

    let (_scratch, paths, store) = open("reconciliation-real");
    let (object, _) = object(&store, PAYLOAD);
    reference(&store, &object, "published");
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let receipt = store.complete_backup(snapshot).unwrap();
    let database = std::fs::read(receipt.state_db()).unwrap();
    let manifest = std::fs::read(receipt.location().join("manifest.json")).unwrap();
    let digest = Sha256::digest(&database).into();
    let cancelled = AtomicBool::new(false);
    let verify = |verifier: &Backups| {
        verifier.reconcile_image(
            receipt.location(),
            manifest.as_slice(),
            super::reconciliation::Expected {
                database: super::manifest::Entry {
                    sha256: digest,
                    bytes: database.len() as u64,
                },
                object_count: 1,
                total_bytes: database.len() as u64 + PAYLOAD.len() as u64,
            },
            &cancelled,
            |_| ControlFlow::Continue(()),
        )
    };
    assert!(matches!(
        verify(&store.backups),
        Ok(crate::BackupImageInspection::Inspected { .. })
    ));
    let fs = Arc::new(FaultFs::new(
        real_fs(),
        FsOp::RemoveDirAll,
        1,
        FaultKind::Eio,
    ));
    let failing = faulty_backups(&store, &paths, fs);
    assert!(verify(&failing).is_err());
    let counts = reading(&store, |tx| {
        tx.query_row("SELECT (SELECT count(*) FROM verifier_scratch), (SELECT count(*) FROM verifier_objects)", [], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).map_err(bug)
    });
    assert_eq!(counts, (1, 1));
    assert!(matches!(
        verify(&store.backups),
        Ok(crate::BackupImageInspection::Inspected { .. })
    ));
    let counts = reading(&store, |tx| {
        tx.query_row("SELECT (SELECT count(*) FROM verifier_scratch), (SELECT count(*) FROM verifier_objects)", [], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).map_err(bug)
    });
    assert_eq!(counts, (0, 0));
}

struct ReconciliationFixture {
    _scratch: ScratchDir,
    store: Store,
    image: Connection,
}

impl ReconciliationFixture {
    fn new() -> Self {
        let (scratch, _paths, store) = open("reconciliation-fixture");
        sql(
            &store,
            "INSERT INTO verifier_scratch VALUES ('owner','token','current')".into(),
        );
        let image = Connection::open_in_memory().unwrap();
        image
            .execute_batch(include_str!("../schema/v1.sql"))
            .unwrap();
        image.execute_batch("PRAGMA foreign_keys=OFF; PRAGMA user_version=1; INSERT INTO store_meta VALUES ('store_uuid','00000000-0000-4000-8000-000000000041'); INSERT INTO backup_generations(generation_id,started_at,state,location) VALUES ('00000000-0000-4000-8000-000000000042',7,'in_progress','fixture');").unwrap();
        Self {
            _scratch: scratch,
            store,
            image,
        }
    }

    fn run(
        &self,
        doc: &[u8],
        count: u64,
        total: u64,
        cancelled: &std::sync::atomic::AtomicBool,
    ) -> Result<std::ops::ControlFlow<()>, StoreError> {
        super::reconciliation::run(
            &self.image,
            &self.store.backups,
            "owner",
            doc,
            super::reconciliation::Expected {
                database: super::manifest::Entry {
                    sha256: [0; 32],
                    bytes: 5,
                },
                object_count: count,
                total_bytes: total,
            },
            cancelled,
        )
    }
}

fn reconciliation_manifest(objects: &[(&str, u64)]) -> Vec<u8> {
    let objects: Vec<_> = objects
        .iter()
        .map(|(hash, size)| serde_json::json!({"sha256": hash, "bytes": size}))
        .collect();
    serde_json::to_vec(&serde_json::json!({"format":1,"store_uuid":"00000000-0000-4000-8000-000000000041","generation_id":"00000000-0000-4000-8000-000000000042","app_version":"test","schema_version":1,"created_at":7,"database":{"file":"state.sqlite","sha256":"00".repeat(32),"bytes":5},"objects":objects})).unwrap()
}

#[test]
fn reconciliation_checks_every_occurrence_and_manifest_entry() {
    use std::ops::ControlFlow;
    use std::sync::atomic::AtomicBool;
    let hash = "ab".repeat(32);
    let other = "cd".repeat(32);
    for mode in [
        "matching",
        "size",
        "missing",
        "extra",
        "duplicate",
        "hash",
        "count",
        "total",
        "database",
        "uuid",
        "generation",
        "time",
        "schema",
        "decoder",
    ] {
        let f = ReconciliationFixture::new();
        let size = if mode == "size" { 4 } else { 3 };
        f.image.execute("INSERT INTO acquisitions(content_sha256,byte_length,object_state,first_acquired_at) VALUES (?1,3,'published',0)", [&hash]).unwrap();
        f.image.execute("INSERT INTO send_attempts(attempt_id,draft_id,revision_id,account_id,config_generation,identity_id,mime_sha256,message_id_header,envelope_from,state,sent_copy,created_at,updated_at) VALUES ('s','d','r','a',1,'i',?1,'m','e','cancelled','filed',0,0)", [&hash]).unwrap();
        let attachment = if mode == "decoder" {
            format!(
                r#"[{{"object_sha256":"{hash}","filename":"f","media_type":"x","size":{size}}},{{}}]"#
            )
        } else {
            format!(
                r#"[{{"object_sha256":"{hash}","filename":"f","media_type":"x","size":{size}}}]"#
            )
        };
        f.image.execute("INSERT INTO draft_revisions(revision_id,draft_id,seq,compose_format,subject,to_json,cc_json,bcc_json,body_source,attachments_json,created_at) VALUES ('r','d',1,'plain','','[]','[]','[]','',?1,0)", [&attachment]).unwrap();
        let objects = match mode {
            "missing" => vec![],
            "extra" => vec![(hash.as_str(), 3), (other.as_str(), 1)],
            "duplicate" => vec![(hash.as_str(), 3), (hash.as_str(), 3)],
            "hash" => vec![(other.as_str(), 3)],
            _ => vec![(hash.as_str(), 3)],
        };
        let mut doc: serde_json::Value =
            serde_json::from_slice(&reconciliation_manifest(&objects)).unwrap();
        match mode {
            "database" => doc["database"]["bytes"] = 6.into(),
            "uuid" => doc["store_uuid"] = "00000000-0000-4000-8000-000000000043".into(),
            "generation" => doc["generation_id"] = "00000000-0000-4000-8000-000000000043".into(),
            "time" => doc["created_at"] = 8.into(),
            "schema" => doc["schema_version"] = 2.into(),
            _ => {}
        }
        let count = objects.len() as u64 + u64::from(mode == "count");
        let total = 5 + objects.iter().map(|(_, n)| n).sum::<u64>() + u64::from(mode == "total");
        let outcome = f.run(
            &serde_json::to_vec(&doc).unwrap(),
            count,
            total,
            &AtomicBool::new(false),
        );
        if mode == "matching" {
            assert_eq!(outcome.unwrap(), ControlFlow::Continue(()));
        } else {
            assert!(
                matches!(outcome, Err(StoreError::Corrupt { .. })),
                "{mode}: {outcome:?}"
            );
        }
    }
}

#[test]
fn reconciliation_pages_large_unique_and_duplicate_sets_without_a_total_cap() {
    use std::ops::ControlFlow;
    use std::sync::atomic::AtomicBool;
    let f = ReconciliationFixture::new();
    f.image.execute_batch("BEGIN").unwrap();
    let mut hashes = Vec::new();
    for n in 0..20_001u64 {
        let hash = format!("{n:064x}");
        f.image.execute("INSERT INTO acquisitions(content_sha256,byte_length,object_state,first_acquired_at) VALUES (?1,3,'published',0)", [&hash]).unwrap();
        hashes.push(hash);
    }
    for n in 0..401 {
        f.image.execute("INSERT INTO backup_generations(generation_id,started_at,state,location) VALUES (?1,0,'failed','x')", [format!("history-{n}")]).unwrap();
    }
    let item = format!(
        r#"{{"object_sha256":"{}","filename":"f","media_type":"x","size":3}}"#,
        hashes[0]
    );
    let attachment = format!("[{}]", vec![item; 201].join(","));
    f.image.execute("INSERT INTO draft_revisions(revision_id,draft_id,seq,compose_format,subject,to_json,cc_json,bcc_json,body_source,attachments_json,created_at) VALUES ('r','d',1,'plain','','[]','[]','[]','',?1,0)", [&attachment]).unwrap();
    f.image.execute_batch("COMMIT").unwrap();
    let items: Vec<_> = hashes.iter().map(|hash| (hash.as_str(), 3)).collect();
    let doc = reconciliation_manifest(&items);
    assert_eq!(
        f.run(&doc, 20_001, 60_008, &AtomicBool::new(false))
            .unwrap(),
        ControlFlow::Continue(())
    );
}

#[test]
fn reconciliation_rejects_hostile_schema_and_propagates_failures() {
    use std::io::{self, Read};
    use std::ops::ControlFlow;
    use std::sync::atomic::AtomicBool;
    let doc = reconciliation_manifest(&[]);
    let cancelled = AtomicBool::new(true);
    let f = ReconciliationFixture::new();
    assert_eq!(
        f.run(&doc, 0, 5, &cancelled).unwrap(),
        ControlFlow::Break(())
    );
    let f = ReconciliationFixture::new();
    f.image
        .execute_batch(
            "DROP TABLE send_attempts; CREATE VIEW send_attempts AS SELECT 'x' AS mime_sha256;",
        )
        .unwrap();
    assert!(matches!(
        f.run(&doc, 0, 5, &AtomicBool::new(false)),
        Err(StoreError::Corrupt { .. })
    ));
    for schema in [
        "DROP TABLE backup_generations; CREATE VIEW backup_generations AS SELECT 'x' generation_id, 7 started_at, 'in_progress' state;",
        "DROP TABLE backup_generations; CREATE TABLE backup_generations(rowid INTEGER, generation_id TEXT, started_at INTEGER, state TEXT);",
        "DROP TABLE backup_generations; CREATE TABLE backup_generations(generation_id TEXT, started_at INTEGER, state TEXT); INSERT INTO backup_generations VALUES ('00000000-0000-4000-8000-000000000042',7,'in_progress'),('00000000-0000-4000-8000-000000000042',7,'in_progress');",
    ] {
        let f = ReconciliationFixture::new();
        f.image.execute_batch(schema).unwrap();
        assert!(
            matches!(
                f.run(&doc, 0, 5, &AtomicBool::new(false)),
                Err(StoreError::Corrupt { .. })
            ),
            "{schema}"
        );
    }
    struct Broken;
    impl Read for Broken {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::InvalidData, "native read"))
        }
    }
    let f = ReconciliationFixture::new();
    let result = super::reconciliation::run(
        &f.image,
        &f.store.backups,
        "owner",
        Broken,
        super::reconciliation::Expected {
            database: super::manifest::Entry {
                sha256: [0; 32],
                bytes: 5,
            },
            object_count: 0,
            total_bytes: 5,
        },
        &AtomicBool::new(false),
    );
    assert_eq!(
        result.unwrap_err().io_kind(),
        Some(io::ErrorKind::InvalidData)
    );
    let f = ReconciliationFixture::new();
    sql(&f.store, "CREATE TRIGGER ledger_insert_fault BEFORE INSERT ON verifier_objects BEGIN SELECT RAISE(ABORT,'fault'); END;".into());
    let hash = "ab".repeat(32);
    assert!(
        f.run(
            &reconciliation_manifest(&[(&hash, 3)]),
            1,
            8,
            &AtomicBool::new(false)
        )
        .is_err()
    );
    let f = ReconciliationFixture::new();
    f.image.execute("INSERT INTO acquisitions(content_sha256,byte_length,object_state,first_acquired_at) VALUES (?1,3,'published',0)", [&hash]).unwrap();
    sql(&f.store, "CREATE TRIGGER ledger_update_fault BEFORE UPDATE ON verifier_objects BEGIN SELECT RAISE(ABORT,'fault'); END;".into());
    assert!(
        f.run(
            &reconciliation_manifest(&[(&hash, 3)]),
            1,
            8,
            &AtomicBool::new(false)
        )
        .is_err()
    );

    struct CancelRead<'a> {
        data: &'a [u8],
        cancelled: &'a AtomicBool,
    }
    impl Read for CancelRead<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = self.data.read(buf)?;
            self.cancelled
                .store(true, std::sync::atomic::Ordering::Release);
            Ok(n)
        }
    }
    let f = ReconciliationFixture::new();
    let cancelled = AtomicBool::new(false);
    let result = super::reconciliation::run(
        &f.image,
        &f.store.backups,
        "owner",
        CancelRead {
            data: &doc,
            cancelled: &cancelled,
        },
        super::reconciliation::Expected {
            database: super::manifest::Entry {
                sha256: [0; 32],
                bytes: 5,
            },
            object_count: 0,
            total_bytes: 5,
        },
        &cancelled,
    );
    assert_eq!(result.unwrap(), ControlFlow::Break(()));
}

#[test]
fn startup_ledger_purge_failure_retains_owner_for_retry() {
    let (_temp, paths, store) = open("verifier-ledger-retry");
    let id = "00000000-0000-4000-8000-000000000041";
    sql(
        &store,
        format!(
            "INSERT INTO verifier_scratch SELECT '{id}', '00000000-0000-4000-8000-000000000042', value FROM store_meta WHERE key='incarnation'"
        ),
    );
    for start in [0u16, 200] {
        super::ledger::insert(
            &store.backups.writer,
            id.into(),
            (start..start + 200)
                .map(|n| {
                    let mut sha256 = [0; 32];
                    sha256[..2].copy_from_slice(&n.to_be_bytes());
                    super::manifest::Entry { sha256, bytes: 1 }
                })
                .collect(),
        )
        .unwrap();
    }
    sql(&store, "CREATE TRIGGER ledger_fault BEFORE DELETE ON verifier_objects WHEN (SELECT count(*) FROM verifier_objects) <= 200 BEGIN SELECT RAISE(ABORT, 'fault'); END;".into());
    store.close().unwrap();
    assert!(crate::open(&paths, &test_seams(real_fs())).is_err());
    let conn = Connection::open(paths.state_db()).unwrap();
    assert_eq!(
        conn.query_row("SELECT count(*) FROM verifier_scratch", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        1
    );
    assert_eq!(
        conn.query_row("SELECT count(*) FROM verifier_objects", [], |r| r
            .get::<_, i64>(0))
            .unwrap(),
        200
    );
    conn.execute_batch("DROP TRIGGER ledger_fault").unwrap();
    drop(conn);
    let reopened = crate::open(&paths, &test_seams(real_fs())).unwrap();
    let counts = reading(&reopened, |tx| {
        tx.query_row("SELECT (SELECT count(*) FROM verifier_scratch), (SELECT count(*) FROM verifier_objects)", [], |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?))).map_err(bug)
    });
    assert_eq!(counts, (0, 0));
}

#[test]
fn retained_image_callback_outcomes_clean_owned_evidence() {
    use sha2::{Digest, Sha256};
    use std::ops::ControlFlow;
    use std::sync::atomic::{AtomicBool, Ordering};

    let (_scratch, paths, store) = open("image-callback-outcomes");
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let receipt = store.complete_backup(snapshot).unwrap();
    let data = std::fs::read(receipt.state_db()).unwrap();
    let digest = Sha256::digest(&data).into();
    for mode in ["success", "error", "before", "during", "break"] {
        let cancelled = AtomicBool::new(mode == "before");
        let mut called = false;
        let closes_before = crate::image_vfs::closes_on_this_thread();
        let result = store.backups.inspect_image_with(
            receipt.location(),
            digest,
            data.len() as u64,
            &cancelled,
            |_| ControlFlow::Continue(()),
            |conn, _| {
                called = true;
                assert_eq!(std::fs::read_dir(paths.recovery()).unwrap().count(), 2);
                let _: i64 = conn
                    .query_row("PRAGMA user_version", [], |r| r.get(0))
                    .unwrap();
                let id: String = reading(&store, |tx| {
                    tx.query_row("SELECT id FROM verifier_scratch", [], |r| r.get(0))
                        .map_err(bug)
                });
                super::ledger::insert(
                    &store.writer,
                    id,
                    vec![super::manifest::Entry {
                        sha256: [1; 32],
                        bytes: 1,
                    }],
                )?;
                match mode {
                    "error" => Err(StoreError::corrupt("callback error")),
                    "during" => {
                        cancelled.store(true, Ordering::Release);
                        Ok(ControlFlow::Continue(()))
                    }
                    "break" => Ok(ControlFlow::Break(())),
                    _ => Ok(ControlFlow::Continue(())),
                }
            },
        );
        assert_eq!(called, mode != "before");
        assert_eq!(
            crate::image_vfs::closes_on_this_thread(),
            closes_before + usize::from(called),
            "connection close after {mode}"
        );
        match mode {
            "success" => assert!(matches!(
                result,
                Ok(crate::BackupImageInspection::Inspected { .. })
            )),
            "error" => assert!(matches!(result, Err(StoreError::Corrupt { .. }))),
            _ => assert!(matches!(
                result,
                Ok(crate::BackupImageInspection::Cancelled)
            )),
        }
        let counts = reading(&store, |tx| {
            tx.query_row(
            "SELECT (SELECT count(*) FROM verifier_scratch), (SELECT count(*) FROM verifier_objects)", [],
            |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
        ).map_err(bug)
        });
        assert_eq!(counts, (0, 0), "{mode}");
        assert_eq!(std::fs::read_dir(paths.recovery()).unwrap().count(), 0);
    }
}

#[test]
fn retained_image_callback_uses_pinned_connection_after_path_replacement() {
    use sha2::{Digest, Sha256};
    use std::ops::ControlFlow;
    use std::sync::atomic::AtomicBool;
    let (_scratch, paths, store) = open("image-callback-pinned");
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let receipt = store.complete_backup(snapshot).unwrap();
    let data = std::fs::read(receipt.state_db()).unwrap();
    let digest = Sha256::digest(&data).into();
    let result = store.backups.inspect_image_with(
        receipt.location(),
        digest,
        data.len() as u64,
        &AtomicBool::new(false),
        |_| ControlFlow::Continue(()),
        |conn, _| {
            let dir = std::fs::read_dir(paths.recovery())
                .unwrap()
                .map(|e| e.unwrap().path())
                .find(|p| {
                    p.file_name()
                        .unwrap()
                        .to_string_lossy()
                        .starts_with("verifier-")
                })
                .unwrap();
            let image = dir.join("state.sqlite");
            let old = paths.data_root().join("old-image");
            std::fs::rename(&image, &old).unwrap();
            std::fs::write(&image, b"replacement").unwrap();
            let version: i64 = conn
                .query_row("PRAGMA user_version", [], |r| r.get(0))
                .unwrap();
            assert!(version > 0);
            let escape = paths.data_root().join("sb-image-escape");
            assert!(
                conn.execute_batch(&format!("ATTACH DATABASE '{}' AS other", escape.display()))
                    .is_err()
            );
            std::fs::remove_file(old).unwrap();
            Ok(ControlFlow::Continue(()))
        },
    );
    assert!(matches!(
        result,
        Ok(crate::BackupImageInspection::Inspected { .. })
    ));
    assert_eq!(std::fs::read_dir(paths.recovery()).unwrap().count(), 0);
}

#[test]
fn retained_image_callback_cleanup_failure_retains_evidence_for_retry() {
    use sha2::{Digest, Sha256};
    use std::ops::ControlFlow;
    use std::sync::atomic::AtomicBool;
    let (_scratch, paths, store) = open("image-callback-cleanup");
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let receipt = store.complete_backup(snapshot).unwrap();
    let data = std::fs::read(receipt.state_db()).unwrap();
    let digest = Sha256::digest(&data).into();
    let fs = Arc::new(FaultFs::new(
        real_fs(),
        FsOp::RemoveDirAll,
        1,
        FaultKind::Eio,
    ));
    let verifier = faulty_backups(&store, &paths, fs);
    let result = verifier.inspect_image_with(
        receipt.location(),
        digest,
        data.len() as u64,
        &AtomicBool::new(false),
        |_| ControlFlow::Continue(()),
        |_, _| {
            let id: String = reading(&store, |tx| {
                tx.query_row("SELECT id FROM verifier_scratch", [], |r| r.get(0))
                    .map_err(bug)
            });
            super::ledger::insert(
                &store.writer,
                id,
                vec![super::manifest::Entry {
                    sha256: [2; 32],
                    bytes: 2,
                }],
            )?;
            Err(StoreError::corrupt("callback error"))
        },
    );
    assert!(result.is_err());
    let counts = reading(&store, |tx| {
        tx.query_row(
        "SELECT (SELECT count(*) FROM verifier_scratch), (SELECT count(*) FROM verifier_objects)", [],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
    ).map_err(bug)
    });
    assert_eq!(counts, (1, 1));
    assert!(matches!(
        store.inspect_backup_image(
            receipt.location(),
            digest,
            data.len() as u64,
            &AtomicBool::new(false),
            |_| ControlFlow::Continue(())
        ),
        Ok(crate::BackupImageInspection::Inspected { .. })
    ));
    let counts = reading(&store, |tx| {
        tx.query_row(
        "SELECT (SELECT count(*) FROM verifier_scratch), (SELECT count(*) FROM verifier_objects)", [],
        |r| Ok((r.get::<_, i64>(0)?, r.get::<_, i64>(1)?)),
    ).map_err(bug)
    });
    assert_eq!(counts, (0, 0));
}

#[test]
fn verifier_image_reads_only_authenticated_copied_database() {
    use sha2::{Digest, Sha256};
    let (_scratch, _paths, store) = open("image-copy");
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let receipt = store.complete_backup(snapshot).unwrap();
    let bytes = std::fs::read(receipt.state_db()).unwrap();
    let digest: [u8; 32] = Sha256::digest(&bytes).into();
    let result = store
        .inspect_backup_image(
            receipt.location(),
            digest,
            bytes.len() as u64,
            &std::sync::atomic::AtomicBool::new(false),
            |_| std::ops::ControlFlow::Continue(()),
        )
        .unwrap();
    assert!(matches!(
        result,
        crate::BackupImageInspection::Inspected { .. }
    ));
    let wrong = store.inspect_backup_image(
        receipt.location(),
        [0; 32],
        bytes.len() as u64,
        &std::sync::atomic::AtomicBool::new(false),
        |_| std::ops::ControlFlow::Continue(()),
    );
    assert!(matches!(wrong, Err(StoreError::Corrupt { .. })));
    let cancelled = store
        .inspect_backup_image(
            receipt.location(),
            digest,
            bytes.len() as u64,
            &std::sync::atomic::AtomicBool::new(false),
            |_| std::ops::ControlFlow::Break(()),
        )
        .unwrap();
    assert!(matches!(cancelled, crate::BackupImageInspection::Cancelled));
}

#[test]
fn verifier_image_lock_recovers_after_progress_panic() {
    use sha2::{Digest, Sha256};
    use std::panic::{AssertUnwindSafe, catch_unwind};
    let (_scratch, _paths, store) = open("image-lock-panic");
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let receipt = store.complete_backup(snapshot).unwrap();
    let data = std::fs::read(receipt.state_db()).unwrap();
    let digest: [u8; 32] = Sha256::digest(&data).into();
    let panicked = catch_unwind(AssertUnwindSafe(|| {
        let _ = store.inspect_backup_image(
            receipt.location(),
            digest,
            data.len() as u64,
            &std::sync::atomic::AtomicBool::new(false),
            |_| panic!("progress callback panicked"),
        );
    }));
    assert!(panicked.is_err());
    let retry = store
        .inspect_backup_image(
            receipt.location(),
            digest,
            data.len() as u64,
            &std::sync::atomic::AtomicBool::new(false),
            |_| std::ops::ControlFlow::Continue(()),
        )
        .unwrap();
    assert!(matches!(
        retry,
        crate::BackupImageInspection::Inspected { .. }
    ));
}

#[test]
fn verifier_image_survives_source_change_after_copy_and_ignores_sidecars() {
    use sha2::{Digest, Sha256};
    let (_scratch, _paths, store) = open("image-source-race");
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let receipt = store.complete_backup(snapshot).unwrap();
    let path = receipt.state_db();
    let data = std::fs::read(&path).unwrap();
    let digest = Sha256::digest(&data).into();
    let wal = receipt.location().join("state.sqlite-wal");
    std::fs::write(&wal, b"untrusted sidecar").unwrap();
    let mut changed = false;
    let report = store
        .inspect_backup_image(
            receipt.location(),
            digest,
            data.len() as u64,
            &std::sync::atomic::AtomicBool::new(false),
            |n| {
                if n == data.len() as u64 && !changed {
                    std::fs::write(&path, b"replaced after copy").unwrap();
                    changed = true;
                }
                std::ops::ControlFlow::Continue(())
            },
        )
        .unwrap();
    assert!(matches!(
        report,
        crate::BackupImageInspection::Inspected { .. }
    ));
    assert!(changed);
    assert_eq!(std::fs::read(&wal).unwrap(), b"untrusted sidecar");
    assert!(!receipt.location().join("state.sqlite-shm").exists());
}

#[test]
fn verifier_image_sql_failure_never_claims_success() {
    use sha2::{Digest, Sha256};
    for operation in ["INSERT", "DELETE"] {
        let (_scratch, paths, store) = open("image-sql-fault");
        let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
        let receipt = store.complete_backup(snapshot).unwrap();
        let data = std::fs::read(receipt.state_db()).unwrap();
        let digest = Sha256::digest(&data).into();
        sql(
            &store,
            format!(
                "CREATE TRIGGER fail_image BEFORE {operation} ON verifier_scratch BEGIN SELECT RAISE(ABORT, 'fault'); END;"
            ),
        );
        assert!(
            store
                .inspect_backup_image(
                    receipt.location(),
                    digest,
                    data.len() as u64,
                    &std::sync::atomic::AtomicBool::new(false),
                    |_| { std::ops::ControlFlow::Continue(()) }
                )
                .is_err()
        );
        sql(&store, "DROP TRIGGER fail_image".into());
        store.close().unwrap();
        let reopened = crate::open(&paths, &test_seams(real_fs())).unwrap();
        let count: i64 = reading(&reopened, |tx| {
            tx.query_row("SELECT count(*) FROM verifier_scratch", [], |r| r.get(0))
                .map_err(bug)
        });
        assert_eq!(count, 0);
    }
}

#[test]
fn verifier_image_remains_pinned_when_scratch_parent_is_replaced() {
    use sha2::{Digest, Sha256};
    let (_scratch, paths, store) = open("image-parent-swap");
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let receipt = store.complete_backup(snapshot).unwrap();
    let data = std::fs::read(receipt.state_db()).unwrap();
    let digest = Sha256::digest(&data).into();
    let old = paths.data_root().join("old-recovery");
    let mut swapped = false;
    let result = store
        .inspect_backup_image(
            receipt.location(),
            digest,
            data.len() as u64,
            &std::sync::atomic::AtomicBool::new(false),
            |n| {
                if n == data.len() as u64 && !swapped {
                    std::fs::rename(paths.recovery(), &old).unwrap();
                    std::fs::create_dir(paths.recovery()).unwrap();
                    std::fs::write(paths.recovery().join("untouched"), b"replacement").unwrap();
                    swapped = true;
                }
                std::ops::ControlFlow::Continue(())
            },
        )
        .unwrap();
    assert!(matches!(
        result,
        crate::BackupImageInspection::Inspected { .. }
    ));
    assert_eq!(
        std::fs::read(paths.recovery().join("untouched")).unwrap(),
        b"replacement"
    );
    assert_eq!(std::fs::read_dir(&old).unwrap().count(), 0);
}

#[test]
fn verifier_image_accepts_read_only_generation_without_creating_sidecars() {
    use sha2::{Digest, Sha256};
    use std::os::unix::fs::PermissionsExt;
    let (_scratch, _paths, store) = open("image-readonly");
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let receipt = store.complete_backup(snapshot).unwrap();
    let path = receipt.state_db();
    let data = std::fs::read(&path).unwrap();
    let digest = Sha256::digest(&data).into();
    let before: Vec<_> = std::fs::read_dir(receipt.location())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    std::fs::set_permissions(&path, std::fs::Permissions::from_mode(0o444)).unwrap();
    std::fs::set_permissions(receipt.location(), std::fs::Permissions::from_mode(0o555)).unwrap();
    let result = store.inspect_backup_image(
        receipt.location(),
        digest,
        data.len() as u64,
        &std::sync::atomic::AtomicBool::new(false),
        |_| std::ops::ControlFlow::Continue(()),
    );
    std::fs::set_permissions(receipt.location(), std::fs::Permissions::from_mode(0o755)).unwrap();
    assert!(matches!(
        result.unwrap(),
        crate::BackupImageInspection::Inspected { .. }
    ));
    let after: Vec<_> = std::fs::read_dir(receipt.location())
        .unwrap()
        .map(|e| e.unwrap().file_name())
        .collect();
    assert_eq!(before, after);
}

#[test]
fn verifier_image_cleanup_preserves_a_substituted_user_directory() {
    use sha2::{Digest, Sha256};
    let (_scratch, paths, store) = open("image-dir-swap");
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let receipt = store.complete_backup(snapshot).unwrap();
    let data = std::fs::read(receipt.state_db()).unwrap();
    let digest = Sha256::digest(&data).into();
    let old = paths.data_root().join("owned-image-moved");
    let mut scratch_path = None;
    let result = store.inspect_backup_image(
        receipt.location(),
        digest,
        data.len() as u64,
        &std::sync::atomic::AtomicBool::new(false),
        |n| {
            if n == data.len() as u64 && scratch_path.is_none() {
                let path = std::fs::read_dir(paths.recovery())
                    .unwrap()
                    .map(|e| e.unwrap().path())
                    .find(|p| {
                        p.file_name()
                            .unwrap()
                            .to_string_lossy()
                            .starts_with("verifier-")
                    })
                    .unwrap();
                std::fs::rename(&path, &old).unwrap();
                std::fs::create_dir(&path).unwrap();
                std::fs::write(path.join("state.sqlite"), b"user data").unwrap();
                scratch_path = Some(path);
            }
            std::ops::ControlFlow::Continue(())
        },
    );
    assert!(result.is_err());
    let path = scratch_path.unwrap();
    assert_eq!(
        std::fs::read(path.join("state.sqlite")).unwrap(),
        b"user data"
    );
    std::fs::remove_file(path.join("state.sqlite")).unwrap();
    std::fs::remove_dir(&path).unwrap();
    std::fs::rename(old, path).unwrap();
    store.close().unwrap();
    crate::open(&paths, &test_seams(real_fs())).unwrap();
}

#[test]
#[ignore = "spawned by verifier_image_transaction_cuts"]
fn verifier_image_transaction_cut_child() {
    use sha2::{Digest, Sha256};
    let root = env::var("SB_VERIFIER_TX_ROOT").unwrap();
    let paths = StorePaths::new(root).unwrap();
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    if env::var_os("SB_RECORDED_CUT").is_some() {
        let (id, _) = object(&store, PAYLOAD);
        reference(&store, &id, "published");
    }
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let receipt = store.complete_backup(snapshot).unwrap();
    let data = std::fs::read(receipt.state_db()).unwrap();
    let digest = Sha256::digest(&data).into();
    std::fs::write(paths.recovery().join("user-sentinel"), b"untouched").unwrap();
    if env::var_os("SB_RECORDED_CUT").is_some() {
        store
            .verify_recorded_backup(
                receipt.generation_id(),
                &std::sync::atomic::AtomicBool::new(false),
            )
            .unwrap();
        return;
    }
    store
        .inspect_backup_image(
            receipt.location(),
            digest,
            data.len() as u64,
            &std::sync::atomic::AtomicBool::new(false),
            |_| std::ops::ControlFlow::Continue(()),
        )
        .unwrap();
}

#[test]
fn verifier_image_transaction_cuts_recover_owned_scratch() {
    use std::os::unix::process::ExitStatusExt;
    let scratch = ScratchDir::new("verifier-tx-cuts");
    for point in [
        "insert-before-commit",
        "insert-after-commit",
        "before-cleanup",
        "after-cleanup",
        "delete-before-commit",
        "delete-after-commit",
    ] {
        let root = scratch.path().join(point);
        std::fs::create_dir(&root).unwrap();
        let status = std::process::Command::new(env::current_exe().unwrap())
            .args([
                "--exact",
                "backups::tests::verifier_image_transaction_cut_child",
                "--ignored",
            ])
            .env("SB_VERIFIER_TX_CUT", point)
            .env("SB_VERIFIER_TX_ROOT", &root)
            .status()
            .unwrap();
        assert_eq!(status.signal(), Some(9), "{point}: {status}");
        let paths = StorePaths::new(root).unwrap();
        let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
        let count: i64 = reading(&store, |tx| {
            tx.query_row("SELECT count(*) FROM verifier_scratch", [], |r| r.get(0))
                .map_err(bug)
        });
        assert_eq!(count, 0, "{point}");
        assert_eq!(
            std::fs::read(paths.recovery().join("user-sentinel")).unwrap(),
            b"untouched"
        );
    }
}

#[test]
fn recorded_verification_crash_recovers_ledger_and_manifest() {
    use std::os::unix::process::ExitStatusExt;
    let scratch = ScratchDir::new("recorded-verification-cuts");
    for point in ["before-cleanup", "after-cleanup", "delete-before-commit"] {
        let root = scratch.path().join(point);
        std::fs::create_dir(&root).unwrap();
        let status = std::process::Command::new(env::current_exe().unwrap())
            .args([
                "--exact",
                "backups::tests::verifier_image_transaction_cut_child",
                "--ignored",
            ])
            .env("SB_VERIFIER_TX_CUT", point)
            .env("SB_VERIFIER_TX_ROOT", &root)
            .env("SB_RECORDED_CUT", "1")
            .status()
            .unwrap();
        assert_eq!(status.signal(), Some(9));
        let paths = StorePaths::new(root).unwrap();
        let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
        let counts: (i64, i64) = reading(&store, |tx| {
            tx.query_row("SELECT (SELECT count(*) FROM verifier_scratch), (SELECT count(*) FROM verifier_objects)", [], |r| Ok((r.get(0)?, r.get(1)?))).map_err(bug)
        });
        assert_eq!(counts, (0, 0));
        assert_eq!(
            std::fs::read(paths.recovery().join("user-sentinel")).unwrap(),
            b"untouched"
        );
    }
}

#[test]
fn verifier_image_cleanup_failure_is_retryable_without_restart() {
    use sha2::{Digest, Sha256};
    let (_scratch, paths, store) = open("image-retry");
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let receipt = store.complete_backup(snapshot).unwrap();
    let data = std::fs::read(receipt.state_db()).unwrap();
    let digest = Sha256::digest(&data).into();
    let fs = Arc::new(FaultFs::new(
        real_fs(),
        FsOp::RemoveDirAll,
        1,
        FaultKind::Eio,
    ));
    let verifier = faulty_backups(&store, &paths, fs);
    assert!(
        verifier
            .inspect_image(
                receipt.location(),
                digest,
                data.len() as u64,
                &std::sync::atomic::AtomicBool::new(false),
                |_| { std::ops::ControlFlow::Continue(()) }
            )
            .is_err()
    );
    drop(verifier);
    assert!(matches!(
        store
            .inspect_backup_image(
                receipt.location(),
                digest,
                data.len() as u64,
                &std::sync::atomic::AtomicBool::new(false),
                |_| { std::ops::ControlFlow::Continue(()) }
            )
            .unwrap(),
        crate::BackupImageInspection::Inspected { .. }
    ));
    let count: i64 = reading(&store, |tx| {
        tx.query_row("SELECT count(*) FROM verifier_scratch", [], |r| r.get(0))
            .map_err(bug)
    });
    assert_eq!(count, 0);
}

#[test]
fn verifier_image_rejects_modified_private_copy() {
    use sha2::{Digest, Sha256};
    let (_scratch, paths, store) = open("image-copy-tamper");
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let receipt = store.complete_backup(snapshot).unwrap();
    let data = std::fs::read(receipt.state_db()).unwrap();
    let digest = Sha256::digest(&data).into();
    let mut changed = false;
    let result = store.inspect_backup_image(
        receipt.location(),
        digest,
        data.len() as u64,
        &std::sync::atomic::AtomicBool::new(false),
        |n| {
            if n == data.len() as u64 && !changed {
                let scratch = std::fs::read_dir(paths.recovery())
                    .unwrap()
                    .map(|e| e.unwrap().path())
                    .find(|p| {
                        p.file_name()
                            .unwrap()
                            .to_string_lossy()
                            .starts_with("verifier-")
                    })
                    .unwrap();
                std::fs::write(scratch.join("state.sqlite"), b"tampered").unwrap();
                changed = true;
            }
            std::ops::ControlFlow::Continue(())
        },
    );
    assert!(changed);
    assert!(matches!(result, Err(StoreError::Corrupt { .. })));
}

#[test]
fn verifier_image_atomic_cancellation_cleans_scratch() {
    use sha2::{Digest, Sha256};
    use std::sync::atomic::{AtomicBool, Ordering};
    let (_scratch, paths, store) = open("image-atomic-cancel");
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let receipt = store.complete_backup(snapshot).unwrap();
    let data = std::fs::read(receipt.state_db()).unwrap();
    let digest = Sha256::digest(&data).into();
    let cancelled = AtomicBool::new(false);
    let result = store
        .inspect_backup_image(
            receipt.location(),
            digest,
            data.len() as u64,
            &cancelled,
            |n| {
                if n > 0 {
                    cancelled.store(true, Ordering::Relaxed);
                }
                std::ops::ControlFlow::Continue(())
            },
        )
        .unwrap();
    assert!(matches!(result, crate::BackupImageInspection::Cancelled));
    let count: i64 = reading(&store, |tx| {
        tx.query_row("SELECT count(*) FROM verifier_scratch", [], |r| r.get(0))
            .map_err(bug)
    });
    assert_eq!(count, 0);
    assert_eq!(std::fs::read_dir(paths.recovery()).unwrap().count(), 0);
}

const IMAGE_CUT_N: &str = "SB_IMAGE_CUT_N";
const IMAGE_CUT_ROOT: &str = "SB_IMAGE_CUT_ROOT";

#[test]
#[ignore = "spawned by run_crash_cuts"]
fn verifier_image_cut_child() {
    use sha2::{Digest, Sha256};
    let nth = env::var(IMAGE_CUT_N).unwrap().parse().unwrap();
    let root = env::var(IMAGE_CUT_ROOT).unwrap();
    let paths = StorePaths::new(Path::new(&root)).unwrap();
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let receipt = store.complete_backup(snapshot).unwrap();
    let data = std::fs::read(receipt.state_db()).unwrap();
    let digest = Sha256::digest(&data).into();
    std::fs::write(paths.recovery().join("user-sentinel"), b"untouched").unwrap();
    let verifier = faulty_backups(&store, &paths, Arc::new(CutFs::new(real_fs(), nth)));
    verifier
        .inspect_image(
            receipt.location(),
            digest,
            data.len() as u64,
            &std::sync::atomic::AtomicBool::new(false),
            |_| std::ops::ControlFlow::Continue(()),
        )
        .unwrap();
}

#[test]
fn verifier_image_crash_cuts_recover_owned_scratch() {
    let scratch = ScratchDir::new("verifier-cuts");
    let cuts = run_crash_cuts(
        IMAGE_CUT_N,
        IMAGE_CUT_ROOT,
        "backups::tests::verifier_image_cut_child",
        &mut |n| {
            let root = scratch.path().join(format!("cut-{n}"));
            std::fs::create_dir(&root).unwrap();
            root
        },
        &mut |root| {
            let paths = StorePaths::new(root).unwrap();
            match crate::open(&paths, &test_seams(real_fs())) {
                Ok(store) => {
                    let count: i64 = reading(&store, |tx| {
                        tx.query_row("SELECT count(*) FROM verifier_scratch", [], |r| r.get(0))
                            .map_err(bug)
                    });
                    assert_eq!(count, 0);
                }
                Err(_) => {
                    // A cut before ownership proof deliberately retains evidence.
                    let conn = Connection::open(paths.state_db()).unwrap();
                    let count: i64 = conn
                        .query_row("SELECT count(*) FROM verifier_scratch", [], |r| r.get(0))
                        .unwrap();
                    assert_eq!(count, 1);
                }
            }
            assert_eq!(
                std::fs::read(paths.recovery().join("user-sentinel")).unwrap(),
                b"untouched"
            );
        },
    );
    assert!(cuts > 0);
}

// --- M1.8e3b b1: verified private stage copy ---

fn sha256_hex(bytes: &[u8]) -> String {
    Sha256::digest(bytes)
        .iter()
        .map(|b| format!("{b:02x}"))
        .collect()
}

fn to_hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

fn hex32(text: &str) -> [u8; 32] {
    let mut out = [0u8; 32];
    for (index, pair) in text.as_bytes().chunks(2).enumerate() {
        out[index] = u8::from_str_radix(std::str::from_utf8(pair).unwrap(), 16).unwrap();
    }
    out
}

fn restore_metadata(store: &Store, generation: &str) -> crate::RestoreMetadata {
    reading(store, |tx| {
        let row = tx
            .query_row(
                "SELECT manifest_sha256, object_count, total_bytes, completed_at
                 FROM backup_generations WHERE generation_id=?1",
                [generation],
                |r| {
                    Ok((
                        r.get::<_, String>(0)?,
                        r.get::<_, i64>(1)?,
                        r.get::<_, i64>(2)?,
                        r.get::<_, i64>(3)?,
                    ))
                },
            )
            .map_err(bug)?;
        Ok(crate::RestoreMetadata {
            manifest_sha256: hex32(&row.0),
            object_count: row.1,
            total_bytes: row.2,
            completed_at: row.3,
        })
    })
}

/// A source store with a published backup whose manifest, database and
/// object hashes are real (spec/03). The first payload is referenced by
/// both an acquisition and a send attempt, so the manifest holds one
/// record for a hash referenced twice.
struct StagingSource {
    _scratch: ScratchDir,
    paths: StorePaths,
    store: Store,
    receipt: BackupReceipt,
    payloads: Vec<(String, Vec<u8>)>,
    metadata: crate::RestoreMetadata,
}

fn staging_source(label: &str) -> StagingSource {
    staging_source_inner(label, None)
}

fn staging_source_inner(label: &str, incarnation: Option<&str>) -> StagingSource {
    let (scratch, paths, store) = open(label);
    draft_rows(&store);
    let mut payloads = Vec::new();
    for (index, payload) in [
        PAYLOAD.to_vec(),
        vec![b'a'; 70_000],
        b"third staging payload".to_vec(),
        vec![0x5a; 100_000],
    ]
    .into_iter()
    .enumerate()
    {
        let (id, sha) = object(&store, &payload);
        if index == 0 {
            reference(&store, &id, "published");
            send_attempt(&store, &sha, "staging");
        } else if index < 3 {
            reference(&store, &id, "published");
        }
        payloads.push((sha, payload));
    }
    let attached = payloads[3].0.clone();
    let attached_len = payloads[3].1.len();
    sql(
        &store,
        format!(
            "UPDATE draft_revisions SET attachments_json = '[{{\"object_sha256\":\"{attached}\",\
             \"filename\":\"only-attachment.bin\",\"media_type\":\"application/octet-stream\",\
             \"size\":{attached_len}}}]'"
        ),
    );

    let attempt_sha = payloads[0].0.clone();
    writing(&store, move |tx| {
        for (id, state) in [
            ("job-inventory", "inventory"),
            ("job-awaiting", "awaiting_confirmation"),
            ("job-running", "running"),
            ("job-completed", "completed"),
            ("job-cancelled", "cancelled"),
            ("job-failed", "failed"),
        ] {
            tx.execute(
                "INSERT INTO import_jobs
                 (job_id,state,request_json,checkpoint_json,revision,created_at,updated_at)
                 VALUES (?1,?2,?3,?4,1,0,0)",
                rusqlite::params![
                    id,
                    state,
                    format!("request-{id}"),
                    format!("checkpoint-{id}")
                ],
            )
            .map_err(bug)?;
            tx.execute(
                "INSERT INTO import_job_sources (job_id,ordinal,source_json,checkpoint_json)
                 VALUES (?1,0,?2,?3)",
                rusqlite::params![id, format!("source-{id}"), format!("position-{id}")],
            )
            .map_err(bug)?;
        }
        for (id, state) in [
            ("intent-pending", "pending"),
            ("intent-inflight", "in_flight"),
            ("intent-parked", "parked"),
            ("intent-done", "done"),
            ("intent-quarantined", "quarantined"),
            ("intent-cancelled", "cancelled"),
        ] {
            tx.execute(
                "INSERT INTO intents
                 (intent_id,account_id,config_generation,op,target_json,args_json,
                  preconditions_json,state,idempotency_key,origin,created_at,updated_at)
                 VALUES (?1,?2,1,'move','{}','{}','{}',?3,?4,'user',0,0)",
                rusqlite::params![id, ACCOUNT, state, format!("key-{id}")],
            )
            .map_err(bug)?;
        }
        for (seq, state) in [
            (2i64, "held"),
            (3, "queued"),
            (4, "submitting"),
            (5, "accepted"),
            (6, "failed"),
            (7, "outcome_unknown"),
            (8, "cancelled"),
        ] {
            let revision = format!("rev-{state}");
            tx.execute(
                "INSERT INTO draft_revisions
                 (revision_id,draft_id,seq,account_id,identity_id,compose_format,subject,
                  to_json,cc_json,bcc_json,body_source,attachments_json,created_at)
                 VALUES (?1,?2,?3,?4,?5,'plain','','[]','[]','[]','','[]',0)",
                rusqlite::params![revision, DRAFT, seq, ACCOUNT, IDENTITY],
            )
            .map_err(bug)?;
            let resolution: Option<&str> = (state == "outcome_unknown").then_some("marked_sent");
            let resolved_at: Option<i64> = (state == "outcome_unknown").then_some(0);
            tx.execute(
                "INSERT INTO send_attempts
                 (attempt_id,draft_id,revision_id,account_id,config_generation,identity_id,
                  mime_sha256,message_id_header,envelope_from,state,sent_copy,
                  resolution,resolved_at,created_at,updated_at)
                 VALUES (?1,?2,?3,?4,1,?5,?9,'<m@x>','me@example.com',?6,'pending',?7,?8,0,0)",
                rusqlite::params![
                    format!("att-{state}"),
                    DRAFT,
                    revision,
                    ACCOUNT,
                    IDENTITY,
                    state,
                    resolution,
                    resolved_at,
                    attempt_sha
                ],
            )
            .map_err(bug)?;
        }
        Ok(())
    });

    let inside = paths
        .data_root()
        .join("backups/11111111-1111-4111-8111-111111111111");
    let inside_text = inside.to_str().unwrap().to_owned();
    writing(&store, move |tx| {
        tx.execute(
            "INSERT INTO backup_generations
             (generation_id,started_at,state,location,manifest_sha256,object_count,total_bytes,completed_at)
             VALUES ('11111111-1111-4111-8111-111111111111',?2,'complete',?1,
                     'inside-manifest',1,2,3)",
            rusqlite::params![inside_text, FIXED_NOW_MS - 3_600_000i64],
        )
        .map_err(bug)?;
        tx.execute(
            "INSERT INTO backup_generations
             (generation_id,started_at,state,location,manifest_sha256,object_count,total_bytes,completed_at)
             VALUES ('22222222-2222-4222-8222-222222222222',?1,'complete',
                     '/external/22222222-2222-4222-8222-222222222222',
                     'outside-manifest',1,2,3)",
            rusqlite::params![FIXED_NOW_MS - 10_800_000i64],
        )
        .map_err(bug)?;
        tx.execute(
            "INSERT INTO backup_ownership VALUES
             ('11111111-1111-4111-8111-111111111111','inside-token')",
            [],
        )
        .map_err(bug)?;
        tx.execute(
            "INSERT INTO backup_ownership VALUES
             ('22222222-2222-4222-8222-222222222222','outside-token')",
            [],
        )
        .map_err(bug)?;
        tx.execute(
            "INSERT INTO verifier_scratch VALUES ('v1','token','inc')",
            [],
        )
        .map_err(bug)?;
        tx.execute(
            "INSERT INTO verifier_objects VALUES ('v1',zeroblob(32),zeroblob(8),0)",
            [],
        )
        .map_err(bug)?;
        tx.execute(
            "INSERT INTO object_pins VALUES ('keep','gc_grace','gc',0)",
            [],
        )
        .map_err(bug)?;
        Ok(())
    });
    if let Some(incarnation) = incarnation {
        sql(
            &store,
            format!("UPDATE store_meta SET value='{incarnation}' WHERE key='incarnation'"),
        );
    }
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let receipt = store.complete_backup(snapshot).unwrap();
    let metadata = restore_metadata(&store, receipt.generation_id());
    StagingSource {
        _scratch: scratch,
        paths,
        store,
        receipt,
        payloads,
        metadata,
    }
}

impl StagingSource {
    fn select_with(&self, store: &Store) -> super::VerifiedExternalBackup {
        let cancel = AtomicBool::new(false);
        store
            .verify_external_backup(
                self.receipt.location().parent().unwrap(),
                self.receipt.generation_id(),
                &cancel,
            )
            .unwrap()
            .unwrap()
    }

    fn live_incarnation(&self) -> String {
        reading(&self.store, |tx| {
            tx.query_row(
                "SELECT value FROM store_meta WHERE key='incarnation'",
                [],
                |r| r.get(0),
            )
            .map_err(bug)
        })
    }

    /// The generation directory still holds exactly the bytes its
    /// manifest declares, and the live row was not rewritten.
    fn assert_untouched(&self) {
        let manifest = std::fs::read(self.receipt.location().join("manifest.json")).unwrap();
        assert_eq!(
            Sha256::digest(&manifest).as_slice(),
            self.metadata.manifest_sha256
        );
        let value: serde_json::Value = serde_json::from_slice(&manifest).unwrap();
        let db_sha = value["database"]["sha256"].as_str().unwrap();
        let db_bytes = value["database"]["bytes"].as_u64().unwrap();
        let db = std::fs::read(self.receipt.state_db()).unwrap();
        assert_eq!(db.len() as u64, db_bytes);
        assert_eq!(sha256_hex(&db), db_sha);
        for (sha, payload) in &self.payloads {
            let path = self.receipt.location().join(format!(
                "objects/sha256/{}/{}/{}",
                &sha[..2],
                &sha[2..4],
                sha
            ));
            assert_eq!(&std::fs::read(path).unwrap(), payload);
        }
        let (state, location, _) =
            generation_row(&self.store, self.receipt.generation_id()).unwrap();
        assert_eq!(state, "complete");
        assert_eq!(location, self.receipt.location().to_str().unwrap());
    }
}

fn assert_no_stage_dir(paths: &StorePaths) {
    for entry in std::fs::read_dir(paths.data_root().parent().unwrap()).unwrap() {
        let name = entry.unwrap().file_name().to_string_lossy().into_owned();
        assert!(!name.contains(".restoring-"), "unexpected stage {name}");
    }
}

/// Byte-exact objects and the layout a swapped root will need.
fn assert_stage_objects(stage_path: &Path, manifest_sha256: &str, payloads: &[(String, Vec<u8>)]) {
    let manifest = std::fs::read(stage_path.join("manifest.json")).unwrap();
    assert_eq!(sha256_hex(&manifest), manifest_sha256);
    let value: serde_json::Value = serde_json::from_slice(&manifest).unwrap();
    let objects = value["objects"].as_array().unwrap();
    assert_eq!(objects.len(), payloads.len());
    for entry in objects {
        let sha = entry["sha256"].as_str().unwrap();
        let bytes = entry["bytes"].as_u64().unwrap();
        let (_, payload) = payloads.iter().find(|(hash, _)| hash == sha).unwrap();
        assert_eq!(payload.len() as u64, bytes);
        let path = stage_path.join(format!(
            "objects/sha256/{}/{}/{}",
            &sha[..2],
            &sha[2..4],
            sha
        ));
        assert_eq!(&std::fs::read(path).unwrap(), payload);
    }
    for dir in [
        "objects",
        "objects/tmp",
        "objects/sha256",
        "backups",
        "recovery",
    ] {
        assert!(std::fs::metadata(stage_path.join(dir)).unwrap().is_dir());
    }
}

/// A clean stage through the public entry point.
fn prepare(
    store: &Store,
    selection: super::VerifiedExternalBackup,
    location: &Path,
    metadata: crate::RestoreMetadata,
) -> crate::PreparedRestoreStage {
    store
        .stage_restore(selection, location, metadata, &AtomicBool::new(false))
        .unwrap()
}

/// How many generation rows the live store holds.
fn generation_count(store: &Store) -> i64 {
    reading(store, |tx| {
        tx.query_row("SELECT count(*) FROM backup_generations", [], |r| r.get(0))
            .map_err(bug)
    })
}

fn assert_stage_err(
    source: &StagingSource,
    selection: super::VerifiedExternalBackup,
    location: &Path,
    metadata: crate::RestoreMetadata,
    cancel: &AtomicBool,
) {
    assert!(
        source
            .store
            .stage_restore(selection, location, metadata, cancel)
            .is_err()
    );
}

/// Stage while cancelling at `phase`, for the cancellation checks.
fn stage_cancel_at(
    source: &StagingSource,
    phase: super::staging::StagePhase,
) -> Result<crate::PreparedRestoreStage, StoreError> {
    let incarnation = match phase {
        super::staging::StagePhase::Located => "00000000-0000-4000-8000-0000000000d1",
        super::staging::StagePhase::DatabaseCopied => "00000000-0000-4000-8000-0000000000d2",
        super::staging::StagePhase::ManifestCopied => "00000000-0000-4000-8000-0000000000d3",
        // Quarantined fires only inside qualification, never staging.
        super::staging::StagePhase::Quarantined => "00000000-0000-4000-8000-0000000000d4",
    };
    let selection = source.select_with(&source.store);
    let cancelled = AtomicBool::new(false);
    source.store.backups.stage_into(
        &selection,
        source.receipt.location(),
        source.metadata,
        incarnation,
        &cancelled,
        |observed| {
            if observed == phase {
                cancelled.store(true, Ordering::Release);
            }
        },
    )
}

#[test]
fn staging_copies_internal_and_external_generations() {
    let source = staging_source("staging-both");
    let internal = prepare(
        &source.store,
        source.select_with(&source.store),
        source.receipt.location(),
        source.metadata,
    );
    assert_eq!(
        internal.path().file_name().unwrap().to_str().unwrap(),
        format!("store.restoring-{}", internal.incarnation())
    );
    assert_eq!(
        std::fs::canonicalize(internal.path().parent().unwrap()).unwrap(),
        std::fs::canonicalize(source.paths.data_root().parent().unwrap()).unwrap()
    );
    assert_eq!(
        internal
            .preserved_root()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap(),
        format!("store.preserved-{}", source.live_incarnation())
    );
    assert_eq!(internal.source_location(), source.receipt.location());
    assert_eq!(internal.generation_id(), source.receipt.generation_id());
    assert_eq!(
        internal.manifest_sha256(),
        to_hex(&source.metadata.manifest_sha256)
    );
    assert_stage_objects(
        internal.path(),
        internal.manifest_sha256(),
        &source.payloads,
    );
    let database = std::fs::read(internal.path().join("state.sqlite")).unwrap();
    assert_eq!(
        &database,
        &std::fs::read(source.receipt.state_db()).unwrap()
    );
    assert_eq!(generation_count(&source.store), 3);

    // A fresh store with a live incarnation the source never had: the
    // preserved suffix provably comes from the live store.
    let (_scratch, _paths, fresh) = open("staging-both-fresh");
    sql(
        &fresh,
        "UPDATE store_meta SET value='00000000-0000-4000-8000-0000000000f1'
         WHERE key='incarnation'"
            .into(),
    );
    let external = prepare(
        &fresh,
        source.select_with(&fresh),
        source.receipt.location(),
        source.metadata,
    );
    let preserved = external
        .preserved_root()
        .file_name()
        .unwrap()
        .to_str()
        .unwrap();
    assert_eq!(
        preserved,
        "store.preserved-00000000-0000-4000-8000-0000000000f1"
    );
    assert_ne!(
        preserved,
        format!("store.preserved-{}", source.live_incarnation()),
        "the preserved suffix comes from the live store, not the source"
    );
    assert_stage_objects(
        external.path(),
        external.manifest_sha256(),
        &source.payloads,
    );
    assert_eq!(generation_count(&fresh), 0);
    source.assert_untouched();
}
#[test]
fn staging_rejects_metadata_locator_and_source_changes() {
    let source = staging_source("staging-rejects");
    let cancel = AtomicBool::new(false);
    let metadata = source.metadata;

    // A declared digest, count, total or negative value fails before
    // anything is created.
    for bad in [
        crate::RestoreMetadata {
            manifest_sha256: [0u8; 32],
            ..metadata
        },
        crate::RestoreMetadata {
            object_count: metadata.object_count + 1,
            ..metadata
        },
        crate::RestoreMetadata {
            total_bytes: metadata.total_bytes + 1,
            ..metadata
        },
        crate::RestoreMetadata {
            object_count: -1,
            ..metadata
        },
    ] {
        assert_stage_err(
            &source,
            source.select_with(&source.store),
            source.receipt.location(),
            bad,
            &cancel,
        );
        assert_no_stage_dir(&source.paths);
    }

    // A locator that does not name the selected generation directory.
    let bad_leaf = source
        .receipt
        .location()
        .parent()
        .unwrap()
        .join("not-a-generation");
    assert_stage_err(
        &source,
        source.select_with(&source.store),
        &bad_leaf,
        metadata,
        &cancel,
    );

    // A same-named directory with a different identity is not the source.
    let impostor = source.paths.data_root().parent().unwrap().join("impostor");
    std::fs::create_dir_all(impostor.join(source.receipt.generation_id())).unwrap();
    assert_stage_err(
        &source,
        source.select_with(&source.store),
        &impostor.join(source.receipt.generation_id()),
        metadata,
        &cancel,
    );
    assert_no_stage_dir(&source.paths);

    // A stale locator after the generation moved away cannot reopen it.
    let selection = source.select_with(&source.store);
    let moved = source.receipt.location().with_extension("moved");
    std::fs::rename(source.receipt.location(), &moved).unwrap();
    assert_stage_err(
        &source,
        selection,
        source.receipt.location(),
        metadata,
        &cancel,
    );
    std::fs::rename(&moved, source.receipt.location()).unwrap();

    // Changed manifest, database or object bytes, and a longer object,
    // fail against the exact selected bytes.
    let manifest_path = source.receipt.location().join("manifest.json");
    let db_path = source.receipt.state_db();
    let object_path = source.receipt.location().join(format!(
        "objects/sha256/{}/{}/{}",
        &source.payloads[0].0[..2],
        &source.payloads[0].0[2..4],
        source.payloads[0].0
    ));
    let original_object = std::fs::read(&object_path).unwrap();
    let original_manifest = std::fs::read(&manifest_path).unwrap();
    let original_db = std::fs::read(&db_path).unwrap();
    let mut longer = original_object.clone();
    longer.extend_from_slice(b"trailing");
    for (path, original, replacement) in [
        (manifest_path, original_manifest, b"{}".to_vec()),
        (db_path, original_db, b"damaged".to_vec()),
        (
            object_path.clone(),
            original_object.clone(),
            b"substituted".to_vec(),
        ),
        (object_path.clone(), original_object.clone(), longer),
    ] {
        let selection = source.select_with(&source.store);
        std::fs::write(&path, &replacement).unwrap();
        assert_stage_err(
            &source,
            selection,
            source.receipt.location(),
            metadata,
            &cancel,
        );
        std::fs::write(&path, &original).unwrap();
    }
    #[cfg(unix)]
    {
        use std::os::unix::fs::symlink;
        // The selection was verified against the real object; replacing
        // it with a symlink cannot redirect the confined read.
        let selection = source.select_with(&source.store);
        std::fs::remove_file(&object_path).unwrap();
        symlink("/etc/hosts", &object_path).unwrap();
        assert_stage_err(
            &source,
            selection,
            source.receipt.location(),
            metadata,
            &cancel,
        );
        std::fs::remove_file(&object_path).unwrap();
        std::fs::write(&object_path, &original_object).unwrap();
    }

    // The source is byte-identical again, and a clean stage succeeds.
    source.assert_untouched();
    let stage = source
        .store
        .stage_restore(
            source.select_with(&source.store),
            source.receipt.location(),
            metadata,
            &cancel,
        )
        .unwrap();
    assert_stage_objects(stage.path(), stage.manifest_sha256(), &source.payloads);
}
#[test]
fn staging_pre_and_mid_cancellation_never_prepare() {
    let source = staging_source("staging-cancel");

    // Pre-cancelled: nothing is created at all.
    let cancelled = AtomicBool::new(true);
    assert_stage_err(
        &source,
        source.select_with(&source.store),
        source.receipt.location(),
        source.metadata,
        &cancelled,
    );
    assert_no_stage_dir(&source.paths);

    // Cancelled after the locator, after the database copy and after the
    // manifest copy: each returns no prepared capability.
    assert!(stage_cancel_at(&source, super::staging::StagePhase::Located).is_err());
    assert_no_stage_dir(&source.paths);
    assert!(stage_cancel_at(&source, super::staging::StagePhase::DatabaseCopied).is_err());
    source.assert_untouched();
    assert!(stage_cancel_at(&source, super::staging::StagePhase::ManifestCopied).is_err());
    source.assert_untouched();
}

#[test]
fn staging_refuses_existing_and_stale_stage_paths() {
    let source = staging_source("staging-stale");
    let (_scratch, fresh_paths, fresh) = open("staging-stale-fresh");
    let parent = std::fs::canonicalize(fresh_paths.data_root().parent().unwrap()).unwrap();
    let existing = "00000000-0000-4000-8000-0000000000b1";
    let stage_dir = parent.join(format!("store.restoring-{existing}"));
    std::fs::create_dir_all(&stage_dir).unwrap();
    let selection = source.select_with(&fresh);
    let stage_with = |incarnation: &str| {
        fresh.backups.stage_into(
            &selection,
            source.receipt.location(),
            source.metadata,
            incarnation,
            &AtomicBool::new(false),
            |_| {},
        )
    };
    let error = stage_with(existing).unwrap_err();
    assert!(matches!(error, StoreError::Conflict { .. }), "{error:?}");
    assert_eq!(std::fs::read_dir(&stage_dir).unwrap().count(), 0);

    let stale = "00000000-0000-4000-8000-0000000000b2";
    let first = stage_with(stale).unwrap();
    let first_path = first.path().to_path_buf();
    let first_manifest = std::fs::read(first.path().join("manifest.json")).unwrap();
    assert_stage_objects(first.path(), first.manifest_sha256(), &source.payloads);
    drop(first);
    let second = prepare(
        &fresh,
        source.select_with(&fresh),
        source.receipt.location(),
        source.metadata,
    );
    assert_ne!(second.incarnation(), stale);
    assert!(first_path.is_dir());
    assert_eq!(
        std::fs::read(first_path.join("manifest.json")).unwrap(),
        first_manifest
    );
    assert_stage_objects(second.path(), second.manifest_sha256(), &source.payloads);
}

#[test]
fn staging_rejects_bad_incarnations() {
    let source = staging_source("staging-bad-incarnations");
    let reused = "00000000-0000-4000-8000-0000000000c1";
    sql(
        &source.store,
        format!("UPDATE store_meta SET value='{reused}' WHERE key='incarnation'"),
    );
    let selection = source.select_with(&source.store);
    let stage_with = |incarnation: &str| {
        source.store.backups.stage_into(
            &selection,
            source.receipt.location(),
            source.metadata,
            incarnation,
            &AtomicBool::new(false),
            |_| {},
        )
    };
    let error = stage_with(reused).unwrap_err();
    assert!(matches!(error, StoreError::Conflict { .. }), "{error:?}");
    assert_no_stage_dir(&source.paths);

    for invalid in ["not-a-uuid", "00000000-0000-4000-8000-0000000000A1"] {
        let error = stage_with(invalid).unwrap_err();
        assert!(
            matches!(error, StoreError::Corrupt { .. }),
            "{invalid}: {error:?}"
        );
    }
    assert_no_stage_dir(&source.paths);
}

#[test]
fn staging_io_failures_never_prepare() {
    let source = staging_source("staging-io-faults");
    let (_scratch, fresh_paths, fresh) = open("staging-io-faults-fresh");
    let cancel = AtomicBool::new(false);
    for (op, nth, kind) in [
        (FsOp::CreatePrivateDir, 1, FaultKind::Eio),
        (FsOp::Create, 1, FaultKind::Eio),
        (FsOp::Read, 1, FaultKind::Eio),
        (FsOp::Write, 1, FaultKind::ShortWrite),
        (FsOp::SyncFile, 1, FaultKind::Eio),
        (FsOp::SyncDir, 1, FaultKind::Eio),
    ] {
        let selection = source.select_with(&fresh);
        let fs: Arc<dyn Fs> = Arc::new(FaultFs::new(real_fs(), op, nth, kind));
        let backups = faulty_backups(&fresh, &fresh_paths, fs);
        assert!(
            backups
                .stage_restore(
                    selection,
                    source.receipt.location(),
                    source.metadata,
                    &cancel
                )
                .is_err()
        );
        source.assert_untouched();
    }
}

#[test]
fn prepared_stage_holds_the_operation_lease_until_drop() {
    let source = staging_source("staging-lease");
    let selection = source.select_with(&source.store);
    let stage = prepare(
        &source.store,
        selection,
        source.receipt.location(),
        source.metadata,
    );
    assert_eq!(stage.data_root, source.paths.data_root());
    assert_eq!(stage.live_incarnation, source.live_incarnation());
    assert!(Arc::ptr_eq(
        &stage.coordinator,
        &source.store.backups.coordinator
    ));
    // A normal backup retention pass cannot start while the handoff is held.
    assert!(matches!(
        source.store.backup_snapshot(BackupDestination::DataRoot),
        Err(StoreError::Conflict { .. })
    ));
    drop(stage);
    drop(
        source
            .store
            .backup_snapshot(BackupDestination::DataRoot)
            .unwrap(),
    );
}

const STAGE_CUT_N: &str = "SB_STAGE_CUT_N";
const STAGE_CUT_ROOT: &str = "SB_STAGE_CUT_ROOT";

#[test]
#[ignore = "spawned by staging_crash_cuts_leave_store_and_source_untouched"]
fn staging_cut_child() {
    let nth: u64 = env::var(STAGE_CUT_N).unwrap().parse().unwrap();
    let root = PathBuf::from(env::var(STAGE_CUT_ROOT).unwrap());
    let source_paths = StorePaths::new(root.join("source/store")).unwrap();
    let source = crate::open(&source_paths, &test_seams(real_fs())).unwrap();
    let (id, sha) = object(&source, PAYLOAD);
    reference(&source, &id, "published");
    let receipt = source
        .complete_backup(source.backup_snapshot(BackupDestination::DataRoot).unwrap())
        .unwrap();
    let manifest = std::fs::read(receipt.location().join("manifest.json")).unwrap();
    let database = std::fs::read(receipt.state_db()).unwrap();
    let object_path = receipt.location().join(format!(
        "objects/sha256/{}/{}/{}",
        &sha[..2],
        &sha[2..4],
        sha
    ));
    let object = std::fs::read(&object_path).unwrap();
    let expected = [
        (receipt.location().join("manifest.json"), &manifest),
        (receipt.state_db(), &database),
        (object_path, &object),
    ]
    .map(|(path, bytes)| format!("{} {}\n", path.display(), sha256_hex(bytes)))
    .concat();
    std::fs::write(root.join("expected.txt"), expected).unwrap();

    let fresh_paths = StorePaths::new(root.join("fresh/store")).unwrap();
    let fresh = crate::open(&fresh_paths, &test_seams(real_fs())).unwrap();
    let cancel = AtomicBool::new(false);
    let selection = fresh
        .verify_external_backup(
            receipt.location().parent().unwrap(),
            receipt.generation_id(),
            &cancel,
        )
        .unwrap()
        .unwrap();
    let metadata = restore_metadata(&source, receipt.generation_id());
    let cut = Arc::new(CutFs::new(real_fs(), nth));
    let backups = faulty_backups_seeded(&fresh, &fresh_paths, cut, 9);
    let location = receipt.location();
    let stage = backups
        .stage_restore(selection, location, metadata, &cancel)
        .unwrap();
    // A cut point past staging lands inside qualification: the
    // inspection, quarantine, checkpoint, close or final flushes.
    backups.qualify_restore_stage(stage, &cancel).unwrap();
}

#[test]
fn staging_crash_cuts_leave_store_and_source_untouched() {
    let scratch = ScratchDir::new("staging-cuts");
    // A seeded spread over the seam's cut points: the small ones bracket
    // locator validation and stage creation, the large ones the copy and
    // flush boundaries. Points past the run's last operation survive and
    // are recorded by the harness.
    let points = [1, 2, 5, 13, 21, 34, 55, 89];
    let cuts = run_crash_cut_points(
        STAGE_CUT_N,
        STAGE_CUT_ROOT,
        "backups::tests::staging_cut_child",
        &points,
        &mut |n| {
            let root = scratch.path().join(format!("cut-{n}"));
            std::fs::create_dir(&root).unwrap();
            root
        },
        &mut |root| {
            let root = PathBuf::from(root);
            // The source generation's bytes are unchanged.
            let expected = std::fs::read_to_string(root.join("expected.txt")).unwrap();
            for line in expected.lines() {
                let (path, hash) = line.split_once(' ').unwrap();
                assert_eq!(sha256_hex(&std::fs::read(path).unwrap()), hash, "{path}");
            }
            // The live fresh store was not renamed or replaced.
            let fresh_paths = StorePaths::new(root.join("fresh/store")).unwrap();
            assert!(fresh_paths.state_db().is_file());
        },
    );
    assert!(!cuts.killed.is_empty(), "no cut point killed the child");
    assert!(
        cuts.survived.iter().any(|point| *point > 1),
        "the highest cut points should survive a complete run"
    );
}

// --- M1.8e3b2a: bounded staged-schema inspection ---

#[test]
fn stage_schema_inspection_accepts_a_valid_copy() {
    let source = staging_source("stage-schema-valid");
    let stage = prepare(
        &source.store,
        source.select_with(&source.store),
        source.receipt.location(),
        source.metadata,
    );
    let entries = || {
        let mut names: Vec<_> = std::fs::read_dir(stage.path())
            .unwrap()
            .map(|entry| entry.unwrap().file_name())
            .collect();
        names.sort();
        names
    };
    let before = entries();
    let inspection = stage.inspect_stage_schema(&AtomicBool::new(false)).unwrap();
    assert_eq!(
        inspection.schema_version,
        i64::from(crate::schema::LATEST_SCHEMA_VERSION)
    );
    // The inspection is read-only: no -shm/-wal or other side file is
    // left in the stage, and its exact bytes are unchanged.
    assert_eq!(before, entries());
    assert_stage_objects(stage.path(), stage.manifest_sha256(), &source.payloads);
    source.assert_untouched();
}

#[test]
fn stage_schema_inspection_ignores_large_unrelated_archive_data() {
    let (_scratch, _paths, store) = open("stage-schema-large-archive");
    draft_rows(&store);
    let request = "r".repeat(3 * 1024 * 1024);
    let stored_request = request.clone();
    writing(&store, move |tx| {
        tx.execute(
            "INSERT INTO import_jobs VALUES ('large','running',?1,'checkpoint',1,0,0)",
            rusqlite::params![stored_request],
        )
        .map_err(bug)
    });
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let receipt = store.complete_backup(snapshot).unwrap();
    let metadata = restore_metadata(&store, receipt.generation_id());
    let selection = store
        .verify_external_backup(
            receipt.location().parent().unwrap(),
            receipt.generation_id(),
            &AtomicBool::new(false),
        )
        .unwrap()
        .unwrap();
    let stage = store
        .stage_restore(
            selection,
            receipt.location(),
            metadata,
            &AtomicBool::new(false),
        )
        .unwrap();
    let inspection = stage.inspect_stage_schema(&AtomicBool::new(false)).unwrap();
    assert_eq!(
        inspection.schema_version,
        i64::from(crate::schema::LATEST_SCHEMA_VERSION)
    );
    // The >1 MiB request is untouched and still readable on a fresh
    // connection: the inspection's bound never leaked to archive data.
    let staged_db = stage.path().join("state.sqlite");
    let conn = Connection::open_with_flags(&staged_db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY)
        .unwrap();
    let saved: String = conn
        .query_row(
            "SELECT request_json FROM import_jobs WHERE job_id='large'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(saved, request);
    let default_limit = Connection::open_in_memory()
        .unwrap()
        .limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH)
        .unwrap();
    assert_eq!(
        conn.limit(rusqlite::limits::Limit::SQLITE_LIMIT_LENGTH)
            .unwrap(),
        default_limit
    );
}

#[test]
fn stage_schema_inspection_rejects_historical_schema_22() {
    let source = staging_source("stage-schema-22");
    let stage = prepare(
        &source.store,
        source.select_with(&source.store),
        source.receipt.location(),
        source.metadata,
    );
    let db = stage.path().join("state.sqlite");
    {
        let conn = Connection::open(&db).unwrap();
        conn.pragma_update(None, "user_version", 22).unwrap();
    }
    let error = stage
        .inspect_stage_schema(&AtomicBool::new(false))
        .unwrap_err();
    let StoreError::Corrupt { detail } = error else {
        panic!("historical schema must fail closed: {error:?}");
    };
    assert!(detail.contains("22"), "{detail}");
    assert!(detail.contains("verifier 23/24"), "{detail}");
    assert!(detail.contains("not migrated"), "{detail}");
}

#[test]
fn stage_schema_inspection_rejects_a_view_substituted_for_a_table() {
    let source = staging_source("stage-schema-view");
    let stage = prepare(
        &source.store,
        source.select_with(&source.store),
        source.receipt.location(),
        source.metadata,
    );
    let db = stage.path().join("state.sqlite");
    {
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "DROP TABLE backup_cleanup_pending;
             CREATE VIEW backup_cleanup_pending AS SELECT 'x' AS generation_id;",
        )
        .unwrap();
    }
    let error = stage
        .inspect_stage_schema(&AtomicBool::new(false))
        .unwrap_err();
    let StoreError::Corrupt { detail } = error else {
        panic!("a view must fail closed: {error:?}");
    };
    assert!(detail.contains("backup_cleanup_pending"), "{detail}");
    assert!(detail.contains("view"), "{detail}");
}

#[test]
fn stage_schema_inspection_rejects_a_virtual_table_substituted_for_a_table() {
    let source = staging_source("stage-schema-virtual");
    let stage = prepare(
        &source.store,
        source.select_with(&source.store),
        source.receipt.location(),
        source.metadata,
    );
    let db = stage.path().join("state.sqlite");
    {
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "PRAGMA foreign_keys=OFF;
             DROP TABLE accounts;
             CREATE VIRTUAL TABLE accounts USING fts5(state);",
        )
        .unwrap();
    }
    let error = stage
        .inspect_stage_schema(&AtomicBool::new(false))
        .unwrap_err();
    let StoreError::Corrupt { detail } = error else {
        panic!("a virtual table must fail closed: {error:?}");
    };
    assert!(detail.contains("accounts"), "{detail}");
    assert!(detail.contains("virtual"), "{detail}");
}

#[test]
fn stage_schema_inspection_rejects_wrong_rowid_shapes() {
    // backup_generations must stay a rowid table for NOT INDEXED.
    let source = staging_source("stage-schema-bg-shape");
    let stage = prepare(
        &source.store,
        source.select_with(&source.store),
        source.receipt.location(),
        source.metadata,
    );
    let db = stage.path().join("state.sqlite");
    {
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "PRAGMA foreign_keys=OFF;
             DROP TABLE backup_generations;
             CREATE TABLE backup_generations (
               generation_id TEXT PRIMARY KEY,
               started_at INTEGER NOT NULL,
               completed_at INTEGER,
               state TEXT NOT NULL,
               location TEXT NOT NULL,
               manifest_sha256 TEXT,
               object_count INTEGER,
               total_bytes INTEGER
             ) STRICT, WITHOUT ROWID;",
        )
        .unwrap();
    }
    let error = stage
        .inspect_stage_schema(&AtomicBool::new(false))
        .unwrap_err();
    let StoreError::Corrupt { detail } = error else {
        panic!("a WITHOUT ROWID backup_generations must fail closed: {error:?}");
    };
    assert!(detail.contains("backup_generations"), "{detail}");
    assert!(detail.contains("rowid"), "{detail}");
}

#[test]
fn stage_schema_inspection_rejects_a_rowid_verifier_objects() {
    let source = staging_source("stage-schema-verifier-shape");
    let stage = prepare(
        &source.store,
        source.select_with(&source.store),
        source.receipt.location(),
        source.metadata,
    );
    let db = stage.path().join("state.sqlite");
    {
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(
            "DROP TABLE verifier_objects;
             CREATE TABLE verifier_objects (
               scratch_id TEXT NOT NULL,
               sha256 BLOB NOT NULL,
               bytes BLOB NOT NULL,
               referenced INTEGER NOT NULL DEFAULT 0,
               PRIMARY KEY(scratch_id, sha256)
             ) STRICT;",
        )
        .unwrap();
    }
    let error = stage
        .inspect_stage_schema(&AtomicBool::new(false))
        .unwrap_err();
    let StoreError::Corrupt { detail } = error else {
        panic!("a rowid verifier_objects must fail closed: {error:?}");
    };
    assert!(detail.contains("verifier_objects"), "{detail}");
    assert!(detail.contains("rowid"), "{detail}");
}

#[test]
fn stage_schema_inspection_rejects_missing_and_renamed_columns() {
    let source = staging_source("stage-schema-missing-table");
    let stage = prepare(
        &source.store,
        source.select_with(&source.store),
        source.receipt.location(),
        source.metadata,
    );
    let db = stage.path().join("state.sqlite");
    {
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch("DROP TABLE backup_cleanup_pending;")
            .unwrap();
    }
    let error = stage
        .inspect_stage_schema(&AtomicBool::new(false))
        .unwrap_err();
    let StoreError::Corrupt { detail } = error else {
        panic!("a missing table must fail closed: {error:?}");
    };
    assert!(detail.contains("missing"), "{detail}");

    let source = staging_source("stage-schema-missing-column");
    let stage = prepare(
        &source.store,
        source.select_with(&source.store),
        source.receipt.location(),
        source.metadata,
    );
    let db = stage.path().join("state.sqlite");
    {
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch("ALTER TABLE accounts RENAME COLUMN state TO status;")
            .unwrap();
    }
    let error = stage
        .inspect_stage_schema(&AtomicBool::new(false))
        .unwrap_err();
    let StoreError::Corrupt { detail } = error else {
        panic!("a renamed column must fail closed: {error:?}");
    };
    assert!(detail.contains("accounts"), "{detail}");
    assert!(detail.contains("column"), "{detail}");
}

#[test]
fn stage_schema_inspection_honours_cancellation() {
    let source = staging_source("stage-schema-cancel");
    let stage = prepare(
        &source.store,
        source.select_with(&source.store),
        source.receipt.location(),
        source.metadata,
    );
    let error = stage
        .inspect_stage_schema(&AtomicBool::new(true))
        .unwrap_err();
    assert!(
        matches!(error, StoreError::Unavailable { .. }),
        "cancellation must be refused: {error:?}"
    );
}

#[test]
fn stage_schema_inherited_programs_do_not_run_during_inspection() {
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "backups::tests::stage_schema_inherited_program_child",
            "--nocapture",
        ])
        .env("SB_STAGE_SCHEMA_PROGRAM_CHILD", "1")
        .output()
        .unwrap();
    assert!(
        child.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
}

#[test]
#[ignore = "spawned by stage_schema_inherited_programs_do_not_run_during_inspection"]
fn stage_schema_inherited_program_child() {
    assert_eq!(
        std::env::var("SB_STAGE_SCHEMA_PROGRAM_CHILD").as_deref(),
        Ok("1")
    );
    let source = staging_source("stage-schema-program");
    let stage = prepare(
        &source.store,
        source.select_with(&source.store),
        source.receipt.location(),
        source.metadata,
    );
    let db = stage.path().join("state.sqlite");
    let huge = "x".repeat(16 * 1024 * 1024);
    {
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(&format!(
            "PRAGMA foreign_keys=OFF;
             DROP TABLE accounts;
             CREATE VIRTUAL TABLE accounts USING fts5(state);
             DROP TABLE accounts_config;
             CREATE TABLE marker_payload(k TEXT PRIMARY KEY, v TEXT) STRICT;
             CREATE VIEW accounts_config AS SELECT k, v FROM marker_payload;
             INSERT INTO marker_payload(k, v) VALUES('k', '{huge}');"
        ))
        .unwrap();
    }
    let _ = crate::image_vfs::largest_allocation(true).unwrap();
    let result = stage.inspect_stage_schema(&AtomicBool::new(false));
    let largest = crate::image_vfs::largest_allocation(false).unwrap();
    assert!(
        largest < i64::from(crate::backups::staging::MAX_SCHEMA_VALUE_BYTES),
        "an inherited program ran during inspection: {largest}"
    );
    assert!(
        matches!(result, Err(StoreError::Corrupt { .. })),
        "an inherited view or virtual table must fail closed: {result:?}"
    );
}

#[test]
fn stage_schema_metadata_allocation_is_bounded() {
    // SQLite's allocation high-water mark is global to a process.
    let child = std::process::Command::new(std::env::current_exe().unwrap())
        .args([
            "--ignored",
            "--exact",
            "backups::tests::stage_schema_allocation_child",
            "--nocapture",
        ])
        .env("SB_STAGE_SCHEMA_ALLOC_CHILD", "1")
        .output()
        .unwrap();
    assert!(
        child.status.success(),
        "{}\n{}",
        String::from_utf8_lossy(&child.stdout),
        String::from_utf8_lossy(&child.stderr)
    );
}

#[test]
#[ignore = "spawned by stage_schema_metadata_allocation_is_bounded"]
fn stage_schema_allocation_child() {
    assert_eq!(
        std::env::var("SB_STAGE_SCHEMA_ALLOC_CHILD").as_deref(),
        Ok("1")
    );
    let source = staging_source("stage-schema-alloc");
    let stage = prepare(
        &source.store,
        source.select_with(&source.store),
        source.receipt.location(),
        source.metadata,
    );
    let db = stage.path().join("state.sqlite");
    let huge = "x".repeat(16 * 1024 * 1024);
    {
        let conn = Connection::open(&db).unwrap();
        conn.execute_batch(&format!("ALTER TABLE accounts ADD COLUMN \"{huge}\" TEXT"))
            .unwrap();
    }
    // The legacy unbounded read reproduces the huge allocation.
    let legacy =
        Connection::open_with_flags(&db, rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY).unwrap();
    let _ = crate::image_vfs::largest_allocation(true).unwrap();
    let legacy_result = (|| -> rusqlite::Result<Vec<(String, String)>> {
        let mut stmt = legacy.prepare("PRAGMA table_info(accounts)")?;
        let rows = stmt.query_map([], |row| {
            Ok((row.get::<_, String>(1)?, row.get::<_, String>(2)?))
        })?;
        rows.collect()
    })();
    let legacy_largest = crate::image_vfs::largest_allocation(false).unwrap();
    assert!(
        legacy_result.is_ok(),
        "the legacy unbounded read should materialize the huge column"
    );
    assert!(
        legacy_largest >= 16 * 1024 * 1024,
        "legacy unbounded allocation not observed: {legacy_largest}"
    );
    drop(legacy);
    // The bounded inspection fails closed without a comparable allocation.
    let _ = crate::image_vfs::largest_allocation(true).unwrap();
    let result = stage.inspect_stage_schema(&AtomicBool::new(false));
    let bounded_largest = crate::image_vfs::largest_allocation(false).unwrap();
    assert!(
        bounded_largest < i64::from(crate::backups::staging::MAX_SCHEMA_VALUE_BYTES),
        "bounded schema allocation: {bounded_largest}"
    );
    assert!(
        matches!(result, Err(StoreError::Corrupt { .. })),
        "oversized schema metadata must fail closed"
    );
}

/// Every after-restore row state of a qualified root (spec/03 step 3).
/// Takes paths and ids rather than the stage so a swapped-in live root
/// verifies the same way.
fn assert_quarantined_stage(
    root: &Path,
    incarnation: &str,
    manifest_sha256: &str,
    generation_id: &str,
    metadata: &crate::RestoreMetadata,
    expected_location: &Path,
    expected_inherited_inside: &Path,
) {
    assert_eq!(manifest_sha256, to_hex(&metadata.manifest_sha256));
    let conn = Connection::open(root.join("state.sqlite")).unwrap();
    conn.execute_batch("PRAGMA query_only=ON").unwrap();
    let live_incarnation: String = conn
        .query_row(
            "SELECT value FROM store_meta WHERE key='incarnation'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(live_incarnation, incarnation);
    let active: i64 = conn
        .query_row(
            "SELECT count(*) FROM accounts WHERE state<>'paused'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(active, 0);
    assert!(
        conn.query_row::<i64, _, _>("SELECT count(*) FROM accounts", [], |r| r.get(0))
            .unwrap()
            >= 2
    );
    for (id, expected) in [
        ("job-inventory", "failed"),
        ("job-awaiting", "failed"),
        ("job-running", "failed"),
        ("job-completed", "completed"),
        ("job-cancelled", "cancelled"),
        ("job-failed", "failed"),
    ] {
        let row: (String, String, String) = conn
            .query_row(
                "SELECT state,request_json,checkpoint_json FROM import_jobs WHERE job_id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .unwrap();
        assert_eq!(
            row,
            (
                expected.to_owned(),
                format!("request-{id}"),
                format!("checkpoint-{id}")
            )
        );
    }
    for id in ["job-inventory", "job-running", "job-completed"] {
        let row: (String, String) = conn
            .query_row(
                "SELECT source_json,checkpoint_json FROM import_job_sources WHERE job_id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(row, (format!("source-{id}"), format!("position-{id}")));
    }
    for (id, expected_state, expected_reason) in [
        ("intent-pending", "quarantined", Some("restore")),
        ("intent-inflight", "quarantined", Some("restore")),
        ("intent-parked", "quarantined", Some("restore")),
        ("intent-done", "done", None),
        ("intent-quarantined", "quarantined", None),
        ("intent-cancelled", "cancelled", None),
    ] {
        let row: (String, Option<String>) = conn
            .query_row(
                "SELECT state,quarantine_reason FROM intents WHERE intent_id=?1",
                [id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .unwrap();
        assert_eq!(row.0, expected_state);
        assert_eq!(row.1, expected_reason.map(str::to_owned));
    }
    for (id, expected) in [
        ("att-held", "outcome_unknown"),
        ("att-queued", "outcome_unknown"),
        ("att-submitting", "outcome_unknown"),
        ("att-accepted", "accepted"),
        ("att-failed", "failed"),
        ("att-outcome_unknown", "outcome_unknown"),
        ("att-cancelled", "cancelled"),
    ] {
        let actual: String = conn
            .query_row(
                "SELECT state FROM send_attempts WHERE attempt_id=?1",
                [id],
                |r| r.get(0),
            )
            .unwrap();
        assert_eq!(actual, expected);
    }
    let row: (String, String, String, i64, i64, i64) = conn
        .query_row(
            "SELECT state,location,manifest_sha256,object_count,total_bytes,completed_at
             FROM backup_generations WHERE generation_id=?1",
            [generation_id],
            |r| {
                Ok((
                    r.get(0)?,
                    r.get(1)?,
                    r.get(2)?,
                    r.get(3)?,
                    r.get(4)?,
                    r.get(5)?,
                ))
            },
        )
        .unwrap();
    assert_eq!(
        row,
        (
            "complete".to_owned(),
            expected_location.to_str().unwrap().to_owned(),
            to_hex(&metadata.manifest_sha256),
            metadata.object_count,
            metadata.total_bytes,
            metadata.completed_at
        )
    );
    let inside: String = conn
        .query_row(
            "SELECT location FROM backup_generations WHERE generation_id=
             '11111111-1111-4111-8111-111111111111'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(inside, expected_inherited_inside.to_str().unwrap());
    let outside: String = conn
        .query_row(
            "SELECT location FROM backup_generations WHERE generation_id=
             '22222222-2222-4222-8222-222222222222'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(outside, "/external/22222222-2222-4222-8222-222222222222");
    let owners: Vec<String> = conn
        .prepare("SELECT generation_id FROM backup_ownership ORDER BY generation_id")
        .unwrap()
        .query_map([], |r| r.get(0))
        .unwrap()
        .map(Result::unwrap)
        .collect();
    assert_eq!(
        owners,
        vec![
            "11111111-1111-4111-8111-111111111111".to_owned(),
            "22222222-2222-4222-8222-222222222222".to_owned()
        ]
    );
    for table in [
        "backup_cleanup_pending",
        "verifier_scratch",
        "verifier_objects",
    ] {
        let count: i64 = conn
            .query_row(&format!("SELECT count(*) FROM {table}"), [], |r| r.get(0))
            .unwrap();
        assert_eq!(count, 0, "{table}");
    }
    let pins: i64 = conn
        .query_row(
            "SELECT count(*) FROM object_pins WHERE pin_kind='backup_in_progress'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(pins, 0);
    let kept: String = conn
        .query_row(
            "SELECT pin_kind FROM object_pins WHERE object_sha256='keep'",
            [],
            |r| r.get(0),
        )
        .unwrap();
    assert_eq!(kept, "gc_grace");
}

/// A clean qualify through the public entry point.
fn qualify(store: &Store, stage: crate::PreparedRestoreStage) -> crate::RestoreStage {
    store
        .qualify_restore_stage(stage, &AtomicBool::new(false))
        .unwrap()
}

#[test]
fn staging_qualifies_an_internal_generation() {
    let source = staging_source("staging-qualify-internal");
    let prepared = prepare(
        &source.store,
        source.select_with(&source.store),
        source.receipt.location(),
        source.metadata,
    );
    let stage = qualify(&source.store, prepared);
    let expected_location = stage
        .preserved_root()
        .join("backups")
        .join(source.receipt.generation_id());
    let expected_inherited_inside = stage
        .preserved_root()
        .join("backups/11111111-1111-4111-8111-111111111111");
    assert_eq!(
        stage
            .preserved_root()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap(),
        format!("store.preserved-{}", source.live_incarnation())
    );
    assert_quarantined_stage(
        stage.path(),
        stage.incarnation(),
        stage.manifest_sha256(),
        stage.generation_id(),
        &source.metadata,
        &expected_location,
        &expected_inherited_inside,
    );
    assert_stage_objects(stage.path(), stage.manifest_sha256(), &source.payloads);
    // The staged database was quarantined in place: it no longer equals
    // the source bytes.
    assert_ne!(
        std::fs::read(stage.path().join("state.sqlite")).unwrap(),
        std::fs::read(source.receipt.state_db()).unwrap()
    );
    // The live store keeps its own rows and account states.
    assert_eq!(generation_count(&source.store), 3);
    let paused: i64 = reading(&source.store, |tx| {
        tx.query_row(
            "SELECT count(*) FROM accounts WHERE state<>'paused'",
            [],
            |r| r.get(0),
        )
        .map_err(bug)
    });
    assert_eq!(paused, 2);
    source.assert_untouched();
}

#[test]
fn qualifying_an_external_generation_into_a_fresh_store() {
    let source = staging_source("qualify-external-source");
    let (_fresh_scratch, _fresh_paths, fresh) = open("qualify-external-fresh");
    // Give the live store an incarnation the source never had, so the
    // preserved suffix provably comes from the live store.
    sql(
        &fresh,
        "UPDATE store_meta SET value='00000000-0000-4000-8000-0000000000f1'
         WHERE key='incarnation'"
            .into(),
    );
    let prepared = prepare(
        &fresh,
        source.select_with(&fresh),
        source.receipt.location(),
        source.metadata,
    );
    let stage = qualify(&fresh, prepared);
    assert_eq!(
        stage
            .preserved_root()
            .file_name()
            .unwrap()
            .to_str()
            .unwrap(),
        "store.preserved-00000000-0000-4000-8000-0000000000f1"
    );
    // An external locator stays where it is after the swap; an inherited
    // location inside the *source* root stays external to this store.
    let inherited_inside = source
        .paths
        .data_root()
        .join("backups/11111111-1111-4111-8111-111111111111");
    assert_quarantined_stage(
        stage.path(),
        stage.incarnation(),
        stage.manifest_sha256(),
        stage.generation_id(),
        &source.metadata,
        source.receipt.location(),
        &inherited_inside,
    );
    assert_stage_objects(stage.path(), stage.manifest_sha256(), &source.payloads);
    assert_eq!(generation_count(&fresh), 0);
    source.assert_untouched();
}

#[test]
fn qualification_rejects_a_foreign_coordinator() {
    let source = staging_source("qualify-foreign");
    let stage = prepare(
        &source.store,
        source.select_with(&source.store),
        source.receipt.location(),
        source.metadata,
    );
    // A second Backups for the same store carries another coordinator
    // token; the stage's binding refuses it.
    let foreign = faulty_backups(&source.store, &source.paths, real_fs());
    let error = foreign
        .qualify_restore_stage(stage, &AtomicBool::new(false))
        .unwrap_err();
    assert!(
        matches!(error, StoreError::InvalidInput { .. }),
        "{error:?}"
    );
    // The unqualified stage stays on disk as evidence and its lease is
    // released.
    assert!(
        source
            .paths
            .data_root()
            .parent()
            .unwrap()
            .read_dir()
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".restoring-"))
    );
    source
        .store
        .backup_snapshot(BackupDestination::DataRoot)
        .unwrap();
}

#[test]
fn qualification_rejects_a_moved_incarnation() {
    let source = staging_source("qualify-moved-incarnation");
    let stage = prepare(
        &source.store,
        source.select_with(&source.store),
        source.receipt.location(),
        source.metadata,
    );
    // A restore or rebuild between staging and qualification moves the
    // live incarnation; the stage's preserved-root suffix is stale.
    sql(
        &source.store,
        "UPDATE store_meta SET value='00000000-0000-4000-8000-0000000000e1'
         WHERE key='incarnation'"
            .into(),
    );
    let error = source
        .store
        .qualify_restore_stage(stage, &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(error, StoreError::Conflict { .. }), "{error:?}");
    source.assert_untouched();
}

#[test]
fn qualification_rejects_forged_binding_fields() {
    let source = staging_source("qualify-forged");
    let cancel = AtomicBool::new(false);

    // A forged data root is refused.
    let stage = prepare(
        &source.store,
        source.select_with(&source.store),
        source.receipt.location(),
        source.metadata,
    );
    let mut forged = stage;
    forged.data_root = PathBuf::from("/elsewhere/store");
    let error = source
        .store
        .qualify_restore_stage(forged, &cancel)
        .unwrap_err();
    assert!(matches!(error, StoreError::Conflict { .. }), "{error:?}");

    // A forged live incarnation is refused.
    let stage = prepare(
        &source.store,
        source.select_with(&source.store),
        source.receipt.location(),
        source.metadata,
    );
    let mut forged = stage;
    forged.live_incarnation = "00000000-0000-4000-8000-0000000000e2".into();
    let error = source
        .store
        .qualify_restore_stage(forged, &cancel)
        .unwrap_err();
    assert!(matches!(error, StoreError::Conflict { .. }), "{error:?}");

    // A forged preserved root is refused.
    let stage = prepare(
        &source.store,
        source.select_with(&source.store),
        source.receipt.location(),
        source.metadata,
    );
    let mut forged = stage;
    forged.preserved_root = PathBuf::from("/elsewhere/store.preserved-x");
    let error = source
        .store
        .qualify_restore_stage(forged, &cancel)
        .unwrap_err();
    assert!(matches!(error, StoreError::Corrupt { .. }), "{error:?}");

    // A forged coordinator token is refused first.
    let stage = prepare(
        &source.store,
        source.select_with(&source.store),
        source.receipt.location(),
        source.metadata,
    );
    let mut forged = stage;
    forged.coordinator = Arc::new(());
    let error = source
        .store
        .qualify_restore_stage(forged, &cancel)
        .unwrap_err();
    assert!(
        matches!(error, StoreError::InvalidInput { .. }),
        "{error:?}"
    );
}

#[test]
fn qualification_pre_and_post_inspect_cancellation() {
    let source = staging_source("qualify-cancel");

    // Pre-cancelled: nothing is touched, the stage stays unqualified.
    let stage = prepare(
        &source.store,
        source.select_with(&source.store),
        source.receipt.location(),
        source.metadata,
    );
    let stage_path = stage.path().to_path_buf();
    let cancelled = AtomicBool::new(true);
    assert!(
        source
            .store
            .qualify_restore_stage(stage, &cancelled)
            .is_err()
    );
    assert!(stage_path.is_dir(), "the unqualified stage is evidence");
    source.assert_untouched();

    // A cancel observed after the copy qualifies nothing but releases
    // the lease; a fresh operation proceeds.
    let stage = prepare(
        &source.store,
        source.select_with(&source.store),
        source.receipt.location(),
        source.metadata,
    );
    source
        .store
        .qualify_restore_stage(stage, &AtomicBool::new(true))
        .unwrap_err();
    source
        .store
        .backup_snapshot(BackupDestination::DataRoot)
        .unwrap();

    // A cancel that lands only after the committed quarantine cannot
    // uncommit it: qualification completes.
    let stage = prepare(
        &source.store,
        source.select_with(&source.store),
        source.receipt.location(),
        source.metadata,
    );
    let cancelled = AtomicBool::new(false);
    let flag = &cancelled;
    let stage = source
        .store
        .backups
        .qualify_restore_stage_with(stage, &cancelled, |phase| {
            if phase == super::staging::StagePhase::Quarantined {
                flag.store(true, Ordering::Release);
            }
        })
        .unwrap();
    assert_quarantined_stage(
        stage.path(),
        stage.incarnation(),
        stage.manifest_sha256(),
        stage.generation_id(),
        &source.metadata,
        &stage
            .preserved_root()
            .join("backups")
            .join(source.receipt.generation_id()),
        &stage
            .preserved_root()
            .join("backups/11111111-1111-4111-8111-111111111111"),
    );
    source.assert_untouched();
}

#[test]
fn a_held_stage_excludes_other_backup_operations() {
    let source = staging_source("qualify-lease");
    let stage = prepare(
        &source.store,
        source.select_with(&source.store),
        source.receipt.location(),
        source.metadata,
    );
    // The prepared stage holds the operation lease.
    let error = source
        .store
        .backup_snapshot(BackupDestination::DataRoot)
        .unwrap_err();
    assert!(matches!(error, StoreError::Conflict { .. }), "{error:?}");
    let qualified = qualify(&source.store, stage);
    let error = source
        .store
        .backup_snapshot(BackupDestination::DataRoot)
        .unwrap_err();
    assert!(matches!(error, StoreError::Conflict { .. }), "{error:?}");
    // Dropping the qualified stage releases the moved lease.
    drop(qualified);
    source
        .store
        .backup_snapshot(BackupDestination::DataRoot)
        .unwrap();
}

#[test]
fn qualifying_rolls_back_an_inherited_trigger() {
    let (_scratch, paths, store) = open("qualify-trigger");
    draft_rows(&store);
    sql(
        &store,
        "CREATE TRIGGER fail_staging BEFORE UPDATE ON accounts
         BEGIN SELECT RAISE(ABORT, 'injected'); END;"
            .to_owned(),
    );
    let receipt = store
        .complete_backup(store.backup_snapshot(BackupDestination::DataRoot).unwrap())
        .unwrap();
    let (_fresh_scratch, fresh_paths, fresh) = open("qualify-trigger-fresh");
    let cancel = AtomicBool::new(false);
    let selection = fresh
        .verify_external_backup(
            receipt.location().parent().unwrap(),
            receipt.generation_id(),
            &cancel,
        )
        .unwrap()
        .unwrap();
    let metadata = restore_metadata(&store, receipt.generation_id());
    let stage = fresh
        .stage_restore(selection, receipt.location(), metadata, &cancel)
        .unwrap();
    let error = fresh.qualify_restore_stage(stage, &cancel).unwrap_err();
    assert!(
        matches!(
            error,
            StoreError::Internal { .. } | StoreError::Corrupt { .. }
        ),
        "an inherited trigger must fail the quarantine, not {error:?}"
    );
    // The unqualified stage remains as evidence; it is never removed.
    assert!(
        fresh_paths
            .data_root()
            .parent()
            .unwrap()
            .read_dir()
            .unwrap()
            .any(|entry| entry
                .unwrap()
                .file_name()
                .to_string_lossy()
                .contains(".restoring-"))
    );
    let _ = paths;
}

#[test]
fn qualification_io_failures_never_qualify() {
    let source = staging_source("qualify-io-faults");
    let (_scratch, fresh_paths, fresh) = open("qualify-io-faults-fresh");
    let cancel = AtomicBool::new(false);

    // A clean run counts the staging-parent SyncDir calls; the last two
    // belong to qualification (stage dir + parent). Failing either must
    // return no qualified stage.
    let counter = Arc::new(FaultFs::new(
        real_fs(),
        FsOp::SyncDir,
        u64::MAX,
        FaultKind::Eio,
    ));
    let count = Arc::clone(&counter);
    let backups = faulty_backups_seeded(&fresh, &fresh_paths, counter, 31);
    let stage = backups
        .stage_restore(
            source.select_with(&fresh),
            source.receipt.location(),
            source.metadata,
            &cancel,
        )
        .unwrap();
    let qualified = backups.qualify_restore_stage(stage, &cancel).unwrap();
    let total = count.calls(FsOp::SyncDir);
    drop(qualified);

    // Failing the stage-dir flush (second to last SyncDir).
    let fs: Arc<dyn Fs> = Arc::new(FaultFs::new(
        real_fs(),
        FsOp::SyncDir,
        total - 1,
        FaultKind::Eio,
    ));
    let backups = faulty_backups_seeded(&fresh, &fresh_paths, fs, 32);
    let stage = backups
        .stage_restore(
            source.select_with(&fresh),
            source.receipt.location(),
            source.metadata,
            &cancel,
        )
        .unwrap();
    assert!(backups.qualify_restore_stage(stage, &cancel).is_err());

    // Failing the parent flush (last SyncDir).
    let fs: Arc<dyn Fs> = Arc::new(FaultFs::new(
        real_fs(),
        FsOp::SyncDir,
        total,
        FaultKind::Eio,
    ));
    let backups = faulty_backups_seeded(&fresh, &fresh_paths, fs, 33);
    let stage = backups
        .stage_restore(
            source.select_with(&fresh),
            source.receipt.location(),
            source.metadata,
            &cancel,
        )
        .unwrap();
    assert!(backups.qualify_restore_stage(stage, &cancel).is_err());
    source.assert_untouched();
}

// --- M1.8f: durable swap + startup recovery ---

/// A `<root>.restore-pending` marker written the way the swap writes it.
fn write_restore_marker(
    paths: &StorePaths,
    staging: &str,
    preserved: &str,
    incarnation: &str,
) -> PathBuf {
    let mut marker = paths.data_root().as_os_str().to_os_string();
    marker.push(".restore-pending");
    let marker = PathBuf::from(marker);
    std::fs::write(
        &marker,
        serde_json::json!({
            "v": 1,
            "staging": staging,
            "preserved": preserved,
            "generation_id": "00000000-0000-4000-8000-0000000000d4",
            "incarnation": incarnation,
        })
        .to_string(),
    )
    .unwrap();
    marker
}

fn live_incarnation(store: &Store) -> String {
    reading(store, |tx| {
        tx.query_row(
            "SELECT value FROM store_meta WHERE key='incarnation'",
            [],
            |r| r.get(0),
        )
        .map_err(bug)
    })
}

fn store_incarnation(store: &Store) -> String {
    live_incarnation(store)
}

#[test]
fn restore_swap_places_the_qualified_stage_and_preserves_the_old_root() {
    let source = staging_source("swap-source");
    let (_scratch, paths, store) = open("swap-live");
    draft_rows(&store);
    let live = live_incarnation(&store);
    let stage = qualify(
        &store,
        prepare(
            &store,
            source.select_with(&store),
            source.receipt.location(),
            source.metadata,
        ),
    );
    let staged_incarnation = stage.incarnation().to_owned();
    let generation = stage.generation_id().to_owned();
    let manifest_sha = stage.manifest_sha256().to_owned();
    let preserved = stage.preserved_root().to_path_buf();

    let lease = store.close_for_restore().unwrap();
    lease.swap_restore(stage, &AtomicBool::new(false)).unwrap();
    drop(lease);

    // The marker is gone and the staging name no longer exists.
    let mut marker = paths.data_root().as_os_str().to_os_string();
    marker.push(".restore-pending");
    assert!(!PathBuf::from(&marker).exists());
    assert!(
        !paths
            .data_root()
            .parent()
            .unwrap()
            .join(format!("store.restoring-{staged_incarnation}"))
            .exists()
    );

    // The live root now holds the quarantined image. The generation
    // rows point inside the *source* root — external to this store —
    // so their locations are unchanged and still resolve.
    let expected_location = source.receipt.location().to_path_buf();
    let expected_inherited_inside = source
        .paths
        .data_root()
        .join("backups/11111111-1111-4111-8111-111111111111");
    assert_quarantined_stage(
        paths.data_root(),
        &staged_incarnation,
        &manifest_sha,
        &generation,
        &source.metadata,
        &expected_location,
        &expected_inherited_inside,
    );
    assert_stage_objects(paths.data_root(), &manifest_sha, &source.payloads);
    assert!(preserved.join("state.sqlite").is_file());

    // The preserved root is a working store with the old incarnation
    // and unpaused accounts — never deleted, still openable.
    let preserved_paths = StorePaths::new(&preserved).unwrap();
    let preserved_store = crate::open(&preserved_paths, &test_seams(real_fs())).unwrap();
    assert_eq!(store_incarnation(&preserved_store), live);
    let active: i64 = reading(&preserved_store, |tx| {
        tx.query_row(
            "SELECT count(*) FROM accounts WHERE state<>'paused'",
            [],
            |r| r.get(0),
        )
        .map_err(bug)
    });
    assert!(active >= 2, "the preserved store keeps its account states");
    preserved_store.close().unwrap();

    // The restored store opens normally and reports the new incarnation.
    let restored = crate::open(&paths, &test_seams(real_fs())).unwrap();
    assert_eq!(store_incarnation(&restored), staged_incarnation);
    restored.close().unwrap();
    source.assert_untouched();
}

#[test]
fn restore_swap_keeps_internal_generations_resolvable() {
    // The source is also the live store: its generation location is
    // inside the live root, so the quarantine rewrites it into the
    // future preserved root — the whole point of the rewrite is that
    // it still resolves after the swap renamed the root.
    let source = staging_source("swap-internal");
    let live = live_incarnation(&source.store);
    let stage = qualify(
        &source.store,
        prepare(
            &source.store,
            source.select_with(&source.store),
            source.receipt.location(),
            source.metadata,
        ),
    );
    let staged_incarnation = stage.incarnation().to_owned();
    let generation = stage.generation_id().to_owned();
    let preserved = stage.preserved_root().to_path_buf();
    let lease = source.store.close_for_restore().unwrap();
    lease.swap_restore(stage, &AtomicBool::new(false)).unwrap();
    drop(lease);

    let restored = crate::open(&source.paths, &test_seams(real_fs())).unwrap();
    assert_eq!(store_incarnation(&restored), staged_incarnation);
    // The rewritten location resolves against the preserved root: the
    // old root carried the generation's bytes through the rename.
    let relocated = preserved.join("backups").join(&generation);
    assert!(relocated.join("manifest.json").is_file());
    assert!(relocated.join("state.sqlite").is_file());
    let location: String = reading(&restored, |tx| {
        tx.query_row(
            "SELECT location FROM backup_generations WHERE generation_id=?1",
            [generation.as_str()],
            |r| r.get(0),
        )
        .map_err(bug)
    });
    assert_eq!(location, relocated.to_str().unwrap());
    assert!(live != staged_incarnation);
    // Restart and back up again (spec/03): G stays `complete`, intact
    // and restorable, and the new generation verifies.
    let snapshot = restored
        .backup_snapshot(BackupDestination::DataRoot)
        .unwrap();
    let second = restored.complete_backup(snapshot).unwrap();
    let (state, still, _) = generation_row(&restored, &generation).unwrap();
    assert_eq!(state, "complete");
    assert_eq!(still, relocated.to_str().unwrap());
    let cancel = AtomicBool::new(false);
    assert!(
        restored
            .verify_external_backup(
                second.location().parent().unwrap(),
                second.generation_id(),
                &cancel
            )
            .unwrap()
            .is_some()
    );
    restored.close().unwrap();
}

#[test]
fn a_held_restore_lease_blocks_a_second_open() {
    let source = staging_source("swap-lease-source");
    let (_scratch, paths, store) = open("swap-lease-live");
    let stage = qualify(
        &store,
        prepare(
            &store,
            source.select_with(&store),
            source.receipt.location(),
            source.metadata,
        ),
    );
    let lease = store.close_for_restore().unwrap();
    let err = crate::open(&paths, &test_seams(real_fs())).unwrap_err();
    assert!(
        matches!(err, StoreError::Locked),
        "a second open while the swap lease is held must be Locked, not {err:?}"
    );
    drop(lease);
    // The lease release leaves a still-valid old store to swap or open.
    let _ = stage;
}

#[test]
fn a_cancelled_swap_never_starts() {
    let source = staging_source("swap-cancel-source");
    let (_scratch, paths, store) = open("swap-cancel-live");
    let live = live_incarnation(&store);
    let stage = qualify(
        &store,
        prepare(
            &store,
            source.select_with(&store),
            source.receipt.location(),
            source.metadata,
        ),
    );
    let stage_path = stage.path().to_path_buf();
    let lease = store.close_for_restore().unwrap();
    let cancelled = AtomicBool::new(true);
    let err = lease.swap_restore(stage, &cancelled).unwrap_err();
    assert!(matches!(err, StoreError::Unavailable { .. }), "{err:?}");
    drop(lease);
    // No marker, the live root untouched, the stage still on disk.
    let mut marker = paths.data_root().as_os_str().to_os_string();
    marker.push(".restore-pending");
    assert!(!PathBuf::from(&marker).exists());
    assert!(stage_path.join("state.sqlite").is_file());
    let reopened = crate::open(&paths, &test_seams(real_fs())).unwrap();
    assert_eq!(store_incarnation(&reopened), live);
    reopened.close().unwrap();
}

#[test]
fn a_foreign_stage_cannot_swap() {
    let source = staging_source("swap-foreign-source");
    let (_scratch, paths, store) = open("swap-foreign-live");
    let other = faulty_backups(&store, &paths, real_fs());
    let stage = other
        .qualify_restore_stage(
            other
                .stage_restore(
                    source.select_with(&store),
                    source.receipt.location(),
                    source.metadata,
                    &AtomicBool::new(false),
                )
                .unwrap(),
            &AtomicBool::new(false),
        )
        .unwrap();
    let lease = store.close_for_restore().unwrap();
    let err = lease
        .swap_restore(stage, &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(err, StoreError::InvalidInput { .. }), "{err:?}");
    let mut marker = paths.data_root().as_os_str().to_os_string();
    marker.push(".restore-pending");
    assert!(!PathBuf::from(&marker).exists());
}

#[test]
fn an_existing_preserved_root_refuses_the_swap() {
    let source = staging_source("swap-clash-source");
    let (_scratch, paths, store) = open("swap-clash-live");
    let live = live_incarnation(&store);
    let stage = qualify(
        &store,
        prepare(
            &store,
            source.select_with(&store),
            source.receipt.location(),
            source.metadata,
        ),
    );
    let preserved = stage.preserved_root().to_path_buf();
    std::fs::create_dir(&preserved).unwrap();
    let lease = store.close_for_restore().unwrap();
    let err = lease
        .swap_restore(stage, &AtomicBool::new(false))
        .unwrap_err();
    assert!(matches!(err, StoreError::Conflict { .. }), "{err:?}");
    drop(lease);
    // The refusal left the live root and the pre-existing preserved
    // dir in place; the store opens with its own incarnation.
    assert!(preserved.is_dir());
    let reopened = crate::open(&paths, &test_seams(real_fs())).unwrap();
    assert_eq!(store_incarnation(&reopened), live);
    reopened.close().unwrap();
}

// The three marker states at open (spec/03): a finished swap is
// completed, an unstarted one is dropped, a finished one's marker is
// removed — and an inconsistent marker fails closed.

const RESTORED_INC: &str = "00000000-0000-4000-8000-0000000000e1";
const OLD_INC: &str = "00000000-0000-4000-8000-0000000000e2";

/// A real second store used as the staged image for the (false, true)
/// recovery state.
fn alternate_store(scratch: &ScratchDir, incarnation: &str) -> PathBuf {
    let alt = StorePaths::new(scratch.path().join("alternate/store")).unwrap();
    let store = crate::open(&alt, &test_seams(real_fs())).unwrap();
    sql(
        &store,
        format!("UPDATE store_meta SET value='{incarnation}' WHERE key='incarnation'"),
    );
    store.close().unwrap();
    alt.data_root().to_path_buf()
}

#[test]
fn swap_recovery_finishes_an_interrupted_swap() {
    let (scratch, paths, store) = open("swap-recover-finish");
    store.close().unwrap();
    let root = paths.data_root().to_path_buf();
    let parent = root.parent().unwrap().to_path_buf();
    let preserved = parent.join(format!("store.preserved-{OLD_INC}"));
    let staging = parent.join(format!("store.restoring-{RESTORED_INC}"));
    std::fs::rename(&root, &preserved).unwrap();
    std::fs::rename(alternate_store(&scratch, RESTORED_INC), &staging).unwrap();
    let marker = write_restore_marker(
        &paths,
        &format!("store.restoring-{RESTORED_INC}"),
        &format!("store.preserved-{OLD_INC}"),
        RESTORED_INC,
    );

    let reopened = crate::open(&paths, &test_seams(real_fs())).unwrap();
    assert_eq!(store_incarnation(&reopened), RESTORED_INC);
    reopened.close().unwrap();
    assert!(!marker.exists());
    assert!(!staging.exists());
    assert!(preserved.join("state.sqlite").is_file());
}

#[test]
fn swap_recovery_discards_an_unstarted_swap() {
    let (_scratch, paths, store) = open("swap-recover-unstarted");
    let live = live_incarnation(&store);
    store.close().unwrap();
    let staging = paths
        .data_root()
        .parent()
        .unwrap()
        .join(format!("store.restoring-{RESTORED_INC}"));
    std::fs::create_dir(&staging).unwrap();
    let marker = write_restore_marker(
        &paths,
        &format!("store.restoring-{RESTORED_INC}"),
        &format!("store.preserved-{OLD_INC}"),
        RESTORED_INC,
    );

    let reopened = crate::open(&paths, &test_seams(real_fs())).unwrap();
    assert_eq!(store_incarnation(&reopened), live);
    reopened.close().unwrap();
    assert!(!marker.exists());
    // The unstarted stage stays on disk as evidence.
    assert!(staging.is_dir());
}

#[test]
fn swap_recovery_removes_a_completed_marker() {
    let (_scratch, paths, store) = open("swap-recover-complete");
    let live = live_incarnation(&store);
    store.close().unwrap();
    let marker = write_restore_marker(
        &paths,
        &format!("store.restoring-{RESTORED_INC}"),
        &format!("store.preserved-{OLD_INC}"),
        RESTORED_INC,
    );

    let reopened = crate::open(&paths, &test_seams(real_fs())).unwrap();
    assert_eq!(store_incarnation(&reopened), live);
    reopened.close().unwrap();
    assert!(!marker.exists());
}

#[test]
fn swap_recovery_refuses_a_marker_over_nothing() {
    let (_scratch, paths, store) = open("swap-recover-empty");
    store.close().unwrap();
    let hidden = paths.data_root().with_file_name("store.hidden");
    std::fs::rename(paths.data_root(), &hidden).unwrap();
    write_restore_marker(
        &paths,
        &format!("store.restoring-{RESTORED_INC}"),
        &format!("store.preserved-{OLD_INC}"),
        RESTORED_INC,
    );
    let err = crate::open(&paths, &test_seams(real_fs())).unwrap_err();
    assert!(
        matches!(err, StoreError::Corrupt { .. }),
        "a marker naming nothing recoverable must fail closed, not {err:?}"
    );
}

#[test]
fn a_corrupt_restore_marker_fails_closed() {
    let (_scratch, paths, store) = open("swap-marker-corrupt");
    store.close().unwrap();
    let mut marker = paths.data_root().as_os_str().to_os_string();
    marker.push(".restore-pending");
    std::fs::write(&marker, b"not a marker").unwrap();
    let err = crate::open(&paths, &test_seams(real_fs())).unwrap_err();
    assert!(matches!(err, StoreError::Corrupt { .. }), "{err:?}");
}

#[test]
fn a_marker_naming_a_foreign_stage_fails_closed() {
    let (_scratch, paths, store) = open("swap-marker-foreign");
    store.close().unwrap();
    // The staging name does not carry the recorded incarnation.
    write_restore_marker(
        &paths,
        "store.restoring-something-else",
        &format!("store.preserved-{OLD_INC}"),
        RESTORED_INC,
    );
    let err = crate::open(&paths, &test_seams(real_fs())).unwrap_err();
    assert!(matches!(err, StoreError::Corrupt { .. }), "{err:?}");
}

const SWAP_CUT_N: &str = "SB_SWAP_CUT_N";
const SWAP_CUT_ROOT: &str = "SB_SWAP_CUT_ROOT";

#[test]
#[ignore = "spawned by restore_swap_cuts_leave_a_recoverable_store"]
fn swap_cut_child() {
    let nth: u64 = env::var(SWAP_CUT_N).unwrap().parse().unwrap();
    let root = PathBuf::from(env::var(SWAP_CUT_ROOT).unwrap());
    let source_paths = StorePaths::new(root.join("source/store")).unwrap();
    let source = crate::open(&source_paths, &test_seams(real_fs())).unwrap();
    let (id, sha) = object(&source, PAYLOAD);
    reference(&source, &id, "published");
    let receipt = source
        .complete_backup(source.backup_snapshot(BackupDestination::DataRoot).unwrap())
        .unwrap();
    let manifest = std::fs::read(receipt.location().join("manifest.json")).unwrap();
    let database = std::fs::read(receipt.state_db()).unwrap();
    let object_path = receipt.location().join(format!(
        "objects/sha256/{}/{}/{}",
        &sha[..2],
        &sha[2..4],
        sha
    ));
    let object = std::fs::read(&object_path).unwrap();
    let expected = [
        (receipt.location().join("manifest.json"), &manifest),
        (receipt.state_db(), &database),
        (object_path, &object),
    ]
    .map(|(path, bytes)| format!("{} {}\n", path.display(), sha256_hex(bytes)))
    .concat();
    std::fs::write(root.join("expected.txt"), expected).unwrap();

    let fresh_paths = StorePaths::new(root.join("fresh/store")).unwrap();
    let fresh = crate::open(&fresh_paths, &test_seams(real_fs())).unwrap();
    std::fs::write(root.join("live-incarnation.txt"), live_incarnation(&fresh)).unwrap();
    let cancel = AtomicBool::new(false);
    let selection = fresh
        .verify_external_backup(
            receipt.location().parent().unwrap(),
            receipt.generation_id(),
            &cancel,
        )
        .unwrap()
        .unwrap();
    let metadata = restore_metadata(&source, receipt.generation_id());
    let cut = Arc::new(CutFs::new(real_fs(), nth));
    let backups = faulty_backups_seeded(&fresh, &fresh_paths, cut, 9);
    let location = receipt.location();
    let stage = backups
        .stage_restore(selection, location, metadata, &cancel)
        .unwrap();
    let stage = backups.qualify_restore_stage(stage, &cancel).unwrap();
    // Record the new incarnation before the swap: the parent cannot
    // recompute it after a kill.
    std::fs::write(root.join("staged-incarnation.txt"), stage.incarnation()).unwrap();
    // The live store closes all its connections; the coordinator's own
    // fs still carries the cut seam for the swap itself.
    fresh.close().unwrap();
    backups.swap_restore_stage(stage, &cancel).unwrap();
}

#[test]
fn restore_swap_cuts_leave_a_recoverable_store() {
    let scratch = ScratchDir::new("swap-cuts");
    let points = [1, 2, 5, 13, 21, 34, 55, 89, 144, 233];
    let cuts = run_crash_cut_points(
        SWAP_CUT_N,
        SWAP_CUT_ROOT,
        "backups::tests::swap_cut_child",
        &points,
        &mut |n| {
            let root = scratch.path().join(format!("cut-{n}"));
            std::fs::create_dir(&root).unwrap();
            root
        },
        &mut |root| {
            let root = PathBuf::from(root);
            let expected = std::fs::read_to_string(root.join("expected.txt")).unwrap();
            for line in expected.lines() {
                let (path, hash) = line.split_once(' ').unwrap();
                assert_eq!(sha256_hex(&std::fs::read(path).unwrap()), hash, "{path}");
            }
            let fresh_paths = StorePaths::new(root.join("fresh/store")).unwrap();
            let mut marker = fresh_paths.data_root().as_os_str().to_os_string();
            marker.push(".restore-pending");
            let marker = PathBuf::from(marker);
            let live = std::fs::read_to_string(root.join("live-incarnation.txt")).unwrap();
            let staged = std::fs::read_to_string(root.join("staged-incarnation.txt")).ok();
            match crate::open(&fresh_paths, &test_seams(real_fs())) {
                Ok(store) => {
                    // Recovery resolved the marker; the root holds either
                    // the old store or the quarantined staged image.
                    assert!(!marker.exists(), "a resolved marker must be gone");
                    let incarnation = store_incarnation(&store);
                    let preserved = fresh_paths
                        .data_root()
                        .parent()
                        .unwrap()
                        .join(format!("store.preserved-{live}"));
                    match staged.as_deref() {
                        Some(staged) if incarnation == staged => {
                            assert!(
                                preserved.join("state.sqlite").is_file(),
                                "a swapped root keeps the old root preserved"
                            );
                        }
                        _ => assert_eq!(incarnation, live, "the root must be the old store"),
                    }
                    store.close().unwrap();
                }
                Err(StoreError::Corrupt { .. }) => {
                    // A torn or inconsistent marker fails closed; the
                    // store simply does not open.
                }
                Err(other) => panic!("open after a swap cut failed unexpectedly: {other:?}"),
            }
        },
    );
    assert!(!cuts.killed.is_empty(), "no cut point killed the child");
    assert!(
        cuts.survived.iter().any(|point| *point > 1),
        "the highest cut points should survive a complete run"
    );
}
