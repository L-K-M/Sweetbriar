//! Backup snapshot proof (spec/03 "Backup generations" steps 1–3):
//! the `in_progress` row that freezes GC, the consistent point-in-time
//! copy while writes continue, pins for exactly the objects the
//! snapshot references, and the durability errors that must surface —
//! never a silent partial, never a `complete` generation. Everything
//! runs through `Store::backup_snapshot` or the real GC path.

use std::env;
use std::path::{Path, PathBuf};
use std::sync::atomic::AtomicBool;
use std::sync::{Arc, Barrier};

use rusqlite::Connection;

use sb_test_support::{
    CutFs, FaultFs, FaultKind, FixedClock, FsOp, ScratchDir, SeededRandom, block_on, real_fs,
    run_crash_cuts,
};
use sb_types::fs::Fs;
use sb_types::time::Timestamp;

use super::{BackupDestination, Backups, Pauses};
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
    open_on(label, real_fs())
}

/// `open` over a chosen filesystem seam.
pub(super) fn open_on(label: &str, fs: Arc<dyn Fs>) -> (ScratchDir, StorePaths, Store) {
    let (scratch, paths) = scratch_paths(label);
    let store = crate::open(&paths, &test_seams(fs)).unwrap();
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
        Arc::clone(&store.latch),
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
                .and_then(|s| backups.complete(s, &AtomicBool::new(false)));
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
    backups.complete(snapshot, &AtomicBool::new(false)).unwrap();
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
    completion_receipt_survives_a_power_cut_when(true);
    completion_receipt_survives_a_power_cut_when(false);
}

fn completion_receipt_survives_a_power_cut_when(clone: bool) {
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
    // The sim clones; refusing the first clone makes every object a
    // copy, so both paths' flushes face the power cut.
    let fs = Arc::new(FaultFs::new(
        sim.clone(),
        FsOp::CloneFile,
        if clone { u64::MAX } else { 1 },
        FaultKind::Kind(std::io::ErrorKind::Unsupported),
    ));
    let backups = faulty_backups(&store, &paths, fs.clone());
    *backups.active.lock().unwrap() = Some(Arc::downgrade(&snapshot.active));
    let receipt = backups.complete(snapshot, &AtomicBool::new(false)).unwrap();
    assert_eq!(fs.calls(FsOp::CloneFile), 1);
    assert_eq!(
        fs.calls(FsOp::Create) == 1,
        clone,
        "only the manifest is created when cloning"
    );
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

/// A latched store removes no generation (spec/03 "Integrity"): the
/// generations are what a restore picks from. Only the store's file
/// work is latched here, so the writer would still take the rows'
/// removal.
#[test]
fn retention_removes_nothing_on_a_latched_store() {
    let (_scratch, paths, store) = open("backup-retention-latched");
    const HOUR: i64 = 3_600_000;
    for n in 0..30 {
        let (_, path) = seed_generation(&store, paths.backups(), n, n as i64 * HOUR, "complete");
        std::fs::create_dir(&path).unwrap();
        own_fixture(&store, &path);
    }
    store.latch.latch_for_test("a test finding");
    for result in [
        store.backups.retain(),
        store.backup_snapshot(BackupDestination::DataRoot).map(drop),
    ] {
        match result {
            Err(StoreError::Corrupt { detail }) => assert!(detail.contains("a test finding")),
            other => panic!("expected Corrupt, got {other:?}"),
        }
    }
    for n in 0..30 {
        let id = format!("00000000-0000-4000-8000-{n:012x}");
        assert!(generation_row(&store, &id).is_some(), "{n}");
        assert!(paths.backups().join(&id).is_dir(), "{n}");
    }
}

/// A latch that lands while a generation's objects are being copied:
/// the latch waits for the object in flight, and no other object and
/// no manifest follow. Only the file work is latched here, so the
/// writer would still have taken the completion.
#[test]
fn a_latch_stops_a_backup_s_copy_at_the_next_object() {
    let (_scratch, _paths, store) = open("backup-copy-latched");
    for payload in [&b"one"[..], b"two", b"three"] {
        let (id, _) = object(&store, payload);
        reference(&store, &id, "published");
    }
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let location = snapshot.location().to_owned();
    let gate = Arc::new(Barrier::new(2));
    store.backups.set_pauses(Pauses {
        first_object_step: Some(gate.clone()),
        ..Default::default()
    });
    let store = Arc::new(store);
    let worker = {
        let store = store.clone();
        std::thread::spawn(move || store.complete_backup(snapshot).map(drop))
    };
    // The copy holds its first object's step.
    gate.wait();
    let latching = {
        let store = store.clone();
        std::thread::spawn(move || store.latch.latch_for_test("a test finding"))
    };
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(20);
    while store.latch.get().is_none() {
        assert!(
            std::time::Instant::now() < deadline,
            "the latch never began"
        );
        std::thread::sleep(std::time::Duration::from_millis(2));
    }
    assert!(!latching.is_finished(), "the latch waits for the object");
    gate.wait();
    match worker.join().unwrap() {
        Err(StoreError::Corrupt { detail }) => assert!(detail.contains("a test finding")),
        other => panic!("expected Corrupt, got {other:?}"),
    }
    latching.join().unwrap();

    let mut copied = 0;
    let mut pending = vec![location.join("objects")];
    while let Some(dir) = pending.pop() {
        for entry in std::fs::read_dir(&dir).unwrap() {
            let path = entry.unwrap().path();
            if path.is_dir() {
                pending.push(path);
            } else {
                copied += 1;
            }
        }
    }
    assert_eq!(copied, 1, "only the object in flight was copied");
    assert!(!location.join("manifest.json").exists());
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
            &store.backups.store_home(),
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
            &store.backups.store_home(),
            id.into(),
            (start..(start + 200).min(401)).map(entry).collect(),
        )
        .unwrap();
    }
    assert!(
        super::ledger::insert(
            &store.backups.store_home(),
            id.into(),
            vec![entry(500), entry(1)]
        )
        .is_err()
    );
    assert!(
        super::ledger::insert(
            &store.backups.store_home(),
            id.into(),
            (500..701).map(entry).collect()
        )
        .is_err()
    );
    let conn = Connection::open(paths.state_db()).unwrap();
    assert!(
        super::ledger::reference(
            &store.backups.store_home(),
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
            &store.backups.store_home(),
            id.into(),
            vec![hash, entry(500).sha256]
        )
        .is_err()
    );
    assert!(!super::ledger::page(&conn, id, None).unwrap()[1].referenced);
    super::ledger::reference(&store.backups.store_home(), id.into(), vec![hash, hash]).unwrap();
    let first = super::ledger::page(&conn, id, None).unwrap();
    assert_eq!(first.len(), 200);
    assert_eq!(first[0].entry.bytes, u64::MAX);
    assert!(first[1].referenced);
    let second = super::ledger::page(&conn, id, Some(first.last().unwrap().entry.sha256)).unwrap();
    assert_eq!(second.len(), 200);
    let third = super::ledger::page(&conn, id, Some(second.last().unwrap().entry.sha256)).unwrap();
    assert_eq!(third.len(), 1);
    assert_eq!(third[0].entry.sha256, entry(400).sha256);
    super::ledger::purge(&store.backups.store_home(), id).unwrap();
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
fn recorded_verification_missing_generation_is_not_found() {
    let (_scratch, _paths, store) = open("recorded-verification-missing");
    let result = store.verify_recorded_backup(
        &Uuid::from_random_bytes([0x72; 16]).to_string(),
        &std::sync::atomic::AtomicBool::new(false),
    );
    assert!(
        matches!(result, Err(StoreError::NotFound { .. })),
        "{result:?}"
    );
}

#[test]
fn external_discovery_skips_authenticated_invalid_metadata() {
    use sha2::Digest;
    use std::ops::ControlFlow;
    let (_source_scratch, _source_paths, source) = open("external-invalid-metadata-source");
    let invalid = source
        .complete_backup(source.backup_snapshot(BackupDestination::DataRoot).unwrap())
        .unwrap();
    let (_other_scratch, _other_paths, other) = open("external-valid-metadata-source");
    // Fresh synthetic stores share deterministic entropy; give this
    // generation a distinct identifier and a real object to verify.
    let (id, _) = object(&other, b"valid generation");
    reference(&other, &id, "published");
    let valid = other
        .complete_backup(
            other
                .backup_snapshot(BackupDestination::Directory(
                    invalid.location().parent().unwrap().to_path_buf(),
                ))
                .unwrap(),
        )
        .unwrap();
    let database = invalid.location().join(super::SNAPSHOT_FILE);
    let conn = rusqlite::Connection::open(&database).unwrap();
    conn.pragma_update(None, "user_version", 0).unwrap();
    conn.close().unwrap();
    let bytes = std::fs::read(&database).unwrap();
    let digest = sha2::Sha256::digest(&bytes)
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect::<String>();
    let path = invalid.location().join("manifest.json");
    let mut manifest: serde_json::Value =
        serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
    assert!(manifest["database"]["sha256"].is_string());
    manifest["database"]["sha256"] = digest.into();
    manifest["database"]["bytes"] = (bytes.len() as u64).into();
    std::fs::write(path, serde_json::to_vec(&manifest).unwrap()).unwrap();
    let (_fresh_scratch, _fresh_paths, fresh) = open("external-invalid-metadata-fresh");
    let parent = invalid.location().parent().unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    let result = fresh.verify_external_backup(parent, invalid.generation_id(), &cancel);
    assert!(matches!(result, Err(StoreError::Corrupt { ref detail })
        if detail == "invalid backup schema version"));
    let mut found = Vec::new();
    let result = fresh.discover_external_backups(parent, &cancel, |selection| {
        found.push(selection.generation_id().to_owned());
        ControlFlow::Continue(())
    });
    assert!(
        matches!(result, Ok(ControlFlow::Continue(()))),
        "{result:?}"
    );
    assert_eq!(found, [valid.generation_id()]);
}

#[test]
fn external_discovery_skips_missing_source_objects() {
    use std::ops::ControlFlow;
    let (_source_scratch, _source_paths, source) = open("external-missing-object-source");
    let (id, hash) = object(&source, PAYLOAD);
    reference(&source, &id, "published");
    let receipt = source
        .complete_backup(source.backup_snapshot(BackupDestination::DataRoot).unwrap())
        .unwrap();
    let path = receipt.location().join(format!(
        "objects/sha256/{}/{}/{}",
        &hash[..2],
        &hash[2..4],
        hash
    ));
    std::fs::remove_file(path).unwrap();
    let (_fresh_scratch, _fresh_paths, fresh) = open("external-missing-object-fresh");
    let mut visited = 0;
    let result = fresh.discover_external_backups(
        receipt.location().parent().unwrap(),
        &std::sync::atomic::AtomicBool::new(false),
        |_| {
            visited += 1;
            ControlFlow::Continue(())
        },
    );
    assert!(
        matches!(result, Ok(ControlFlow::Continue(()))),
        "{result:?}"
    );
    assert_eq!(visited, 0);
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
fn recorded_verification_incomplete_generation_is_conflict() {
    let (_scratch, _paths, store) = open("recorded-verification-incomplete");
    let snapshot = store.backup_snapshot(BackupDestination::DataRoot).unwrap();
    let generation = snapshot.generation_id().to_owned();
    drop(snapshot);
    for state in ["in_progress", "failed"] {
        sql(
            &store,
            format!("UPDATE backup_generations SET state='{state}'"),
        );
        let result =
            store.verify_recorded_backup(&generation, &std::sync::atomic::AtomicBool::new(false));
        assert!(
            matches!(result, Err(StoreError::Conflict { .. })),
            "{state}: {result:?}"
        );
    }
}

#[test]
fn recorded_verification_missing_database_is_not_found() {
    let (_scratch, _paths, store) = open("recorded-verification-missing-database");
    let receipt = store
        .complete_backup(store.backup_snapshot(BackupDestination::DataRoot).unwrap())
        .unwrap();
    std::fs::remove_file(receipt.location().join(super::SNAPSHOT_FILE)).unwrap();
    let result = store.verify_recorded_backup(
        receipt.generation_id(),
        &std::sync::atomic::AtomicBool::new(false),
    );
    assert!(
        matches!(result, Err(StoreError::NotFound { .. })),
        "{result:?}"
    );
}

#[test]
fn recorded_verification_complete_metadata_cannot_be_missing() {
    for column in [
        "manifest_sha256",
        "object_count",
        "total_bytes",
        "completed_at",
    ] {
        let (_scratch, _paths, store) = open("recorded-verification-missing-metadata");
        let receipt = store
            .complete_backup(store.backup_snapshot(BackupDestination::DataRoot).unwrap())
            .unwrap();
        sql(
            &store,
            format!("UPDATE backup_generations SET {column}=NULL"),
        );
        let result = store.verify_recorded_backup(
            receipt.generation_id(),
            &std::sync::atomic::AtomicBool::new(false),
        );
        assert!(
            matches!(result, Err(StoreError::Corrupt { .. })),
            "{column}: {result:?}"
        );
    }
}

#[test]
fn recorded_verification_manifest_failures_are_typed() {
    let (_scratch, _paths, store) = open("recorded-verification-manifest-syntax");
    let receipt = store
        .complete_backup(store.backup_snapshot(BackupDestination::DataRoot).unwrap())
        .unwrap();
    let path = receipt.location().join("manifest.json");
    let original = std::fs::read(&path).unwrap();
    let cancel = std::sync::atomic::AtomicBool::new(false);
    std::fs::write(&path, b"{\"format\":1}").unwrap();
    let result = store.verify_recorded_backup(receipt.generation_id(), &cancel);
    assert!(
        matches!(result, Err(StoreError::Corrupt { .. })),
        "{result:?}"
    );
    std::fs::remove_file(&path).unwrap();
    let result = store.verify_recorded_backup(receipt.generation_id(), &cancel);
    assert!(
        matches!(result, Err(StoreError::NotFound { .. })),
        "{result:?}"
    );
    std::fs::write(path, original).unwrap();
    assert!(matches!(
        store.verify_recorded_backup(receipt.generation_id(), &cancel),
        Ok(crate::BackupImageInspection::Inspected { .. })
    ));
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
    let result = verify();
    assert!(
        matches!(result, Err(StoreError::NotFound { .. })),
        "{result:?}"
    );
    std::fs::write(&path, original).unwrap();
    let manifest = receipt.location().join("manifest.json");
    let original = std::fs::read(&manifest).unwrap();
    std::fs::write(&manifest, b"{}").unwrap();
    let result = verify();
    assert!(
        matches!(result, Err(StoreError::Corrupt { .. })),
        "{result:?}"
    );
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
    assert!(matches!(
        failing.verify_recorded(receipt.generation_id(), &cancel),
        Err(StoreError::Unavailable {
            io_kind: Some(_),
            ..
        })
    ));
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
            &store.backups.store_home(),
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
                    &store.backups.store_home(),
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
                &store.backups.store_home(),
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

mod changes;
mod clones;
mod keyed;

mod stage_tests {
    mod bindings;
    mod evidence;
    mod qualified;
    mod quarantine_image;
    mod swap;
    mod swap_acceptance;
    use super::*;
    use std::sync::atomic::{AtomicBool, Ordering};

    use sb_test_support::run_crash_cut_points;
    use sha2::{Digest, Sha256};

    use super::super::staging::StagePhase;
    use crate::{BackupReceipt, VerifiedExternalBackup};

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
        staging_source_on(label, real_fs())
    }

    /// `staging_source` with the store over a chosen filesystem seam.
    fn staging_source_on(label: &str, fs: Arc<dyn Fs>) -> StagingSource {
        let (scratch, paths, store) = open_on(label, fs);
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
        fn select_with(&self, store: &Store) -> VerifiedExternalBackup {
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
    fn assert_stage_objects(stage: &crate::PreparedRestoreStage, payloads: &[(String, Vec<u8>)]) {
        let manifest = std::fs::read(stage.path().join("manifest.json")).unwrap();
        assert_eq!(sha256_hex(&manifest), stage.manifest_sha256());
        let value: serde_json::Value = serde_json::from_slice(&manifest).unwrap();
        let objects = value["objects"].as_array().unwrap();
        assert_eq!(objects.len(), payloads.len());
        for entry in objects {
            let sha = entry["sha256"].as_str().unwrap();
            let bytes = entry["bytes"].as_u64().unwrap();
            let (_, payload) = payloads.iter().find(|(hash, _)| hash == sha).unwrap();
            assert_eq!(payload.len() as u64, bytes);
            let path = stage.path().join(format!(
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
            assert!(std::fs::metadata(stage.path().join(dir)).unwrap().is_dir());
        }
    }

    /// A clean stage through the public entry point.
    fn prepare(
        store: &Store,
        selection: VerifiedExternalBackup,
        location: &Path,
        metadata: crate::RestoreMetadata,
    ) -> crate::PreparedRestoreStage {
        store
            .stage_restore(selection, location, metadata, &AtomicBool::new(false))
            .unwrap()
    }

    #[test]
    fn quarantined_image_executes_on_the_real_prepared_copy() {
        use super::super::{quarantine, stage_quarantine};
        let source = staging_source("quarantined-image-smoke");
        let old_incarnation = source.live_incarnation();
        let expected_uuid: String = reading(&source.store, |tx| {
            tx.query_row(
                "SELECT value FROM store_meta WHERE key='store_uuid'",
                [],
                |row| row.get(0),
            )
            .map_err(bug)
        });
        let prepared = prepare(
            &source.store,
            source.select_with(&source.store),
            source.receipt.location(),
            source.metadata,
        );
        let cancel = Arc::new(AtomicBool::new(false));
        let input = quarantine::Quarantine {
            new_incarnation: prepared.incarnation(),
            generation: prepared.generation_id(),
            source_location: prepared.source_location().to_str().unwrap(),
            current_root: &prepared.data_root,
            preserved_root: prepared.preserved_root(),
            manifest_sha256: prepared.manifest_sha256(),
            object_count: prepared.metadata.object_count,
            total_bytes: prepared.metadata.total_bytes,
            completed_at: prepared.metadata.completed_at,
            cancel: &cancel,
            max_work_calls: quarantine::work_allowance(prepared.files.database_bytes),
        };
        let mut phases = Vec::new();
        let image = stage_quarantine::run(
            &prepared.files,
            &test_seams(real_fs()),
            &input,
            &expected_uuid,
            |phase| phases.push(phase),
        )
        .unwrap();
        assert_eq!(phases.len(), 7);
        assert_eq!(image.root_identity, prepared.files.root_identity);
        assert_eq!(image.database_identity, prepared.files.database_identity);
        let bytes = std::fs::read(prepared.path().join("state.sqlite")).unwrap();
        assert_eq!(image.bytes, bytes.len() as u64);
        assert_eq!(image.sha256, <[u8; 32]>::from(Sha256::digest(&bytes)));
        assert_eq!(
            image.schema_version,
            i64::from(crate::schema::LATEST_SCHEMA_VERSION)
        );
        let observed = Connection::open_with_flags(
            prepared.path().join("state.sqlite"),
            rusqlite::OpenFlags::SQLITE_OPEN_READ_ONLY,
        )
        .unwrap();
        let incarnation: String = observed
            .query_row(
                "SELECT value FROM store_meta WHERE key='incarnation'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(incarnation, prepared.incarnation());
        observed.close().unwrap();
        assert_eq!(source.live_incarnation(), old_incarnation);
        source.assert_untouched();
    }

    #[test]
    fn prepared_stage_retains_the_created_directory_and_database() {
        use std::io::{Read, Seek, SeekFrom};
        let source = staging_source("staging-retained-handles");
        let stage = prepare(
            &source.store,
            source.select_with(&source.store),
            source.receipt.location(),
            source.metadata,
        );
        let original = std::fs::read(stage.path().join("state.sqlite")).unwrap();
        assert_eq!(stage.files.database_bytes, original.len() as u64);
        assert_eq!(
            stage.files.database_sha256,
            <[u8; 32]>::from(Sha256::digest(&original))
        );
        let original_path = stage.path().to_path_buf();
        let moved = original_path.with_extension("retained");
        std::fs::rename(&original_path, &moved).unwrap();
        std::fs::create_dir(&original_path).unwrap();
        std::fs::write(original_path.join("state.sqlite"), b"replacement root").unwrap();
        std::fs::rename(moved.join("state.sqlite"), moved.join("retained.sqlite")).unwrap();
        std::fs::write(moved.join("state.sqlite"), b"replacement file").unwrap();
        assert_eq!(
            stage.files.root.identity().unwrap(),
            stage.files.root_identity
        );
        assert_eq!(
            stage.files.database.identity().unwrap(),
            stage.files.database_identity
        );
        let replacement_root = real_fs().open_confined_dir(&original_path).unwrap();
        assert_ne!(
            replacement_root.identity().unwrap(),
            stage.files.root_identity
        );
        let mut reader = stage.files.database_reader();
        let mut bytes = Vec::new();
        reader.read_to_end(&mut bytes).unwrap();
        assert_eq!(bytes, original);
        reader.seek(SeekFrom::Start(0)).unwrap();
        let mut another = stage.files.database_reader();
        let mut magic = [0; 16];
        another.read_exact(&mut magic).unwrap();
        assert_eq!(&magic, b"SQLite format 3\0");
        reader.read_exact(&mut magic).unwrap();
        assert_eq!(&magic, b"SQLite format 3\0");

        // Even later writes stay on the created file. The now-modified image
        // is unqualified; retaining a handle never grants swap authority.
        stage
            .files
            .database
            .write_at(b"unqualified", original.len() as u64)
            .unwrap();
        stage.files.database.sync_all().unwrap();
        assert_eq!(
            std::fs::read(moved.join("retained.sqlite")).unwrap(),
            [original.as_slice(), b"unqualified"].concat()
        );
        assert_eq!(
            std::fs::read(moved.join("state.sqlite")).unwrap(),
            b"replacement file"
        );
        assert_eq!(
            std::fs::read(original_path.join("state.sqlite")).unwrap(),
            b"replacement root"
        );
        assert_eq!(std::fs::read(source.receipt.state_db()).unwrap(), original);
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
        selection: VerifiedExternalBackup,
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
        phase: StagePhase,
    ) -> Result<crate::PreparedRestoreStage, StoreError> {
        let incarnation = match phase {
            StagePhase::Located => "00000000-0000-4000-8000-0000000000d1",
            StagePhase::LiveBound => "00000000-0000-4000-8000-0000000000d6",
            StagePhase::LiveRead => "00000000-0000-4000-8000-0000000000d7",
            StagePhase::DatabaseCopied => "00000000-0000-4000-8000-0000000000d2",
            StagePhase::ManifestCopied => "00000000-0000-4000-8000-0000000000d3",
            StagePhase::ObjectsCopied => "00000000-0000-4000-8000-0000000000d4",
            StagePhase::HandlesRetained => "00000000-0000-4000-8000-0000000000d5",
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
        assert_stage_objects(&internal, &source.payloads);
        let database = std::fs::read(internal.path().join("state.sqlite")).unwrap();
        assert_eq!(
            &database,
            &std::fs::read(source.receipt.state_db()).unwrap()
        );
        assert_eq!(generation_count(&source.store), 1);

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
        assert_stage_objects(&external, &source.payloads);
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
        assert_stage_objects(&stage, &source.payloads);
    }
    #[test]
    fn a_growing_manifest_cannot_fill_staging_beyond_its_verified_length() {
        let source = staging_source("staging-growing-manifest");
        let selected = source.select_with(&source.store);
        let manifest = source.receipt.location().join("manifest.json");
        let original = std::fs::read(&manifest).unwrap();
        let incarnation = "00000000-0000-4000-8000-0000000000d5";
        let result = source.store.backups.stage_into(
            &selected,
            source.receipt.location(),
            source.metadata,
            incarnation,
            &AtomicBool::new(false),
            |phase| {
                if phase == StagePhase::ObjectsCopied {
                    std::fs::write(
                        &manifest,
                        [original.as_slice(), &vec![b' '; 128 * 1024]].concat(),
                    )
                    .unwrap();
                }
            },
        );
        assert!(matches!(result, Err(StoreError::Corrupt { .. })));
        let staged = source
            .paths
            .data_root()
            .with_file_name(format!("store.restoring-{incarnation}"));
        assert!(
            std::fs::metadata(staged.join("manifest.json"))
                .unwrap()
                .len()
                <= original.len() as u64,
            "changed manifest bytes must not consume unverified staging space"
        );
        std::fs::write(&manifest, original).unwrap();
        source.assert_untouched();
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
        assert!(stage_cancel_at(&source, StagePhase::Located).is_err());
        assert!(stage_cancel_at(&source, StagePhase::LiveBound).is_err());
        assert!(stage_cancel_at(&source, StagePhase::LiveRead).is_err());
        assert_no_stage_dir(&source.paths);
        assert!(stage_cancel_at(&source, StagePhase::DatabaseCopied).is_err());
        source.assert_untouched();
        assert!(stage_cancel_at(&source, StagePhase::ManifestCopied).is_err());
        source.assert_untouched();
        assert!(stage_cancel_at(&source, StagePhase::HandlesRetained).is_err());
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
        assert_stage_objects(&first, &source.payloads);
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
        assert_stage_objects(&second, &source.payloads);
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
        let cancel = AtomicBool::new(false);
        for (op, nth, kind) in [
            (FsOp::CreatePrivateDir, 1, FaultKind::Eio),
            (FsOp::Create, 1, FaultKind::Eio),
            (FsOp::Read, 1, FaultKind::Eio),
            (FsOp::Write, 1, FaultKind::ShortWrite),
            (FsOp::SyncFile, 1, FaultKind::Eio),
            (FsOp::SyncDir, 1, FaultKind::Eio),
        ] {
            let (_scratch, fresh_paths, fresh) = open("staging-io-faults-fresh");
            let selection = source.select_with(&fresh);
            let fs = Arc::new(FaultFs::new(real_fs(), op, nth, kind));
            let backups = faulty_backups_seeded(&fresh, &fresh_paths, fs.clone(), 7);
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
            assert!(fs.calls(op) >= nth, "{op:?} was not reached");
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
        backups
            .stage_restore(selection, location, metadata, &cancel)
            .unwrap();
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
            "backups::tests::stage_tests::staging_cut_child",
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
}
