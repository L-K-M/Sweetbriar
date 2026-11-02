//! Backup snapshot proof (spec/03 "Backup generations" steps 1–3):
//! the `in_progress` row that freezes GC, the consistent point-in-time
//! copy while writes continue, pins for exactly the objects the
//! snapshot references, and the durability errors that must surface —
//! never a silent partial, never a `complete` generation. Everything
//! runs through `Store::backup_snapshot` or the real GC path.

use std::env;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Barrier};

use rusqlite::Connection;

use sb_test_support::{
    CutFs, FaultFs, FaultKind, FixedClock, FsOp, ScratchDir, SeededRandom, block_on, real_fs,
    run_crash_cuts,
};
use sb_types::fs::Fs;
use sb_types::time::Timestamp;

use super::{BackupDestination, BackupSnapshot, Backups, Pauses};
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
fn open(label: &str) -> (ScratchDir, StorePaths, Store) {
    let (scratch, paths) = scratch_paths(label);
    let store = crate::open(&paths, &test_seams(real_fs())).unwrap();
    (scratch, paths, store)
}

// --- the fixture rows spec/03 names, written through the writer ---

/// One batch in one writer commit; values are synthetic literals.
fn sql(store: &Store, statement: String) {
    writing(store, move |tx| tx.execute_batch(&statement).map_err(bug));
}

/// The account, identity and one draft revision every fixture hangs
/// on (the schema seeds its own `local_archive` account).
fn draft_rows(store: &Store) {
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
    // Only the generation's own file lives in its directory so far:
    // no WAL leftovers stay behind.
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
    Backups::new(
        fs,
        Arc::new(SeededRandom::new(7)),
        Arc::new(FixedClock::new(Timestamp::from_millis(FIXED_NOW_MS))),
        Arc::clone(&store.writer),
        paths.state_db().to_path_buf(),
        paths.backups().to_path_buf(),
    )
}

/// One fault point's aftermath: `Unavailable`, the row still
/// `in_progress`, and a retry `Conflict` on the leftover.
fn assert_failed_snapshot(store: &Store, result: Result<BackupSnapshot, StoreError>) {
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
    let retry = store
        .backup_snapshot(BackupDestination::DataRoot)
        .unwrap_err();
    assert!(
        matches!(retry, StoreError::Conflict { .. }),
        "the leftover gates the next run: {retry:?}"
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
    let backups = Backups::new(
        Arc::new(CutFs::new(real_fs(), nth)),
        Arc::new(SeededRandom::new(3)),
        Arc::new(FixedClock::new(Timestamp::from_millis(FIXED_NOW_MS))),
        Arc::clone(&store.writer),
        paths.state_db().to_path_buf(),
        paths.backups().to_path_buf(),
    );
    backups.snapshot(BackupDestination::DataRoot).unwrap();
}

/// At every cut point: the store reopens, every generation is
/// `in_progress` — `complete` and `failed` are unreachable — a
/// `backup_in_progress` pin only ever belongs to a live generation,
/// and the next run either refuses the leftover or backs up cleanly
/// (acceptance: "crash cuts ... leave the row `in_progress` for
/// M1.8d's recovery to mark `failed`").
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
            match store.backup_snapshot(BackupDestination::DataRoot) {
                Ok(_) => assert_eq!(open_rows, 0, "two generations ran at once"),
                Err(StoreError::Conflict { .. }) => assert_eq!(open_rows, 1),
                Err(e) => panic!("a cut leftover must not break the next backup: {e:?}"),
            }
        },
    );
    assert!(cuts > 0, "the sweep must have killed children");
}
