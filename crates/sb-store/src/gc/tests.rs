//! GC proof (spec/03 "Garbage collection"): the grace clock, every
//! gate, the serialized re-check/unlink races with `publish`, and a
//! proptest history that must never lose a referenced, pinned or
//! in-backup object. Everything runs through `Store`.

use std::collections::HashMap;
use std::io::Read as _;
use std::sync::atomic::{AtomicI64, Ordering};
use std::sync::{Arc, Barrier};
use std::time::Duration;

use proptest::prelude::*;

use sb_test_support::{FaultFs, FaultKind, FsOp, ScratchDir, SeededRandom, block_on, real_fs};
use sb_types::fs::Fs;

use super::{GC_GRACE_PERIOD, GcGate, GcReport};
use crate::imports::{
    ImportBatch, ImportFlags, ImportJobState, ImportMessage, ImportSourceKind, ImportSummary,
    advance_import_job, commit_import_batch, create_import_job, record_import_source,
};
use crate::objects::{Pauses, ReservationRelease, release_reservation};
use crate::schema::bug;
use crate::testing::{FIXED_NOW_MS, scratch_paths, writing};
use crate::{
    Clock, ObjectId, ObjectStore, PublishedObject, Seams, Store, StoreError, StorePaths, Timestamp,
    WritePriority,
};

/// Milliseconds in one day; reservation expiry and grace age off it.
const DAY_MS: i64 = 24 * 60 * 60 * 1000;
const PAYLOAD: &[u8] = b"Subject: collect me\r\n\r\nthe orphan bytes\r\n";
const JOB: &str = "00000000-0000-4000-8000-0000000000c4";
const SOURCE_HASH: &str = "4a4e94e39a4c23bdaeb5df6092bc4ad83bec3e915c16562740edd4495013bb2f";
/// The integrity-doubt fixture's acquisition: never a real payload.
const PHANTOM_SHA: &str = "0000000000000000000000000000000000000000000000000000000000000000";
// One account, identity, draft and revision: the schema seeds its
// own `local_archive` account and forbids a second.
const ACCOUNT: &str = "00000000-0000-4000-8000-0000000000a1";
const IDENTITY: &str = "00000000-0000-4000-8000-0000000000a2";
const DRAFT: &str = "00000000-0000-4000-8000-0000000000a3";
const REVISION: &str = "00000000-0000-4000-8000-0000000000a4";

/// A movable clock: grace ages against it.
struct TestClock(AtomicI64);

impl Clock for TestClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0.load(Ordering::SeqCst))
    }
}

impl TestClock {
    fn at(ms: i64) -> Arc<Self> {
        Arc::new(Self(AtomicI64::new(ms)))
    }

    fn set(&self, ms: i64) {
        self.0.store(ms, Ordering::SeqCst);
    }
}

/// [`GC_GRACE_PERIOD`] in milliseconds.
fn grace_ms() -> i64 {
    i64::try_from(GC_GRACE_PERIOD.as_millis()).unwrap()
}

/// A store whose seams stamp `ms`, over a chosen filesystem.
fn open_with(paths: &StorePaths, clock: &Arc<TestClock>, ms: i64, fs: Arc<dyn Fs>) -> Store {
    clock.set(ms);
    let seams = Seams::new(fs, clock.clone(), Arc::new(SeededRandom::new(7)));
    crate::open(paths, &seams).unwrap()
}

/// The common test prelude: one scratch store at `FIXED_NOW_MS`.
fn open(label: &str) -> (ScratchDir, Store, Arc<TestClock>) {
    let (scratch, paths) = scratch_paths(label);
    let clock = TestClock::at(FIXED_NOW_MS);
    let store = open_with(&paths, &clock, FIXED_NOW_MS, real_fs());
    (scratch, store, clock)
}

/// Count one object's pins of `kind` (`""` counts every kind).
fn pins(store: &Store, sha: &str, kind: &str) -> i64 {
    let sql = "SELECT count(*) FROM object_pins WHERE object_sha256 = ?1
               AND (?2 = '' OR pin_kind = ?2)";
    store
        .read(|tx| {
            tx.query_row(sql, [sha, kind], |row| row.get::<_, i64>(0))
                .map_err(bug)
        })
        .unwrap()
}

fn exists(store: &Store, id: &ObjectId) -> bool {
    store.objects().exists(id).unwrap()
}

/// Free the publication's reservation through the real commit path.
fn abandon(store: &Store, publication: PublishedObject) {
    writing(store, move |tx| {
        release_reservation(tx, &publication, ReservationRelease::Required)
    });
}

/// One running import job (the reservation tests' fixture).
fn running_job(store: &Store, clock: &TestClock) -> i64 {
    let at = clock.now();
    writing(store, move |tx| {
        create_import_job(tx, JOB, "{\"paths\":[],\"v\":1}", at)?;
        record_import_source(tx, JOB, 0, "{\"path\":\"/gc/src.mbox\",\"v\":1}")?;
        advance_import_job(
            tx,
            JOB,
            1,
            ImportJobState::AwaitingConfirmation,
            "{\"v\":1}",
            at,
        )?;
        Ok(advance_import_job(tx, JOB, 2, ImportJobState::Running, "{\"v\":1}", at)?.revision())
    })
}

/// The real referencing commit; it releases the object's reservation.
fn commit(store: &Store, clock: &TestClock, revision: i64, object: PublishedObject, path: &str) {
    let (at, path) = (clock.now(), path.to_owned());
    block_on(store.write(WritePriority::Sync, move |tx| {
        commit_import_batch(
            tx,
            &ImportBatch {
                job_id: JOB,
                expected_revision: revision,
                source_ordinal: 0,
                mailbox_name: "Collected",
                messages: &[ImportMessage {
                    object,
                    source_kind: ImportSourceKind::Mbox,
                    source_path: path,
                    source_sha256: SOURCE_HASH.to_owned(),
                    offset: 0,
                    flags: ImportFlags::default(),
                    summary: ImportSummary::default(),
                }],
            },
            at,
            |_| Ok(("{\"v\":1}".to_owned(), "{\"v\":1}".to_owned())),
        )
    }))
    .unwrap();
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

/// The one draft revision's attachments: `attached_sha` or empty.
fn draft(store: &Store, attached_sha: Option<&str>) {
    draft_rows(store);
    let json = attached_sha
        .map(|sha| format!("[{{\"object_sha256\":\"{sha}\"}}]"))
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

/// A bare `acquisitions` row: the reference `commit_import_batch`
/// leaves, without the import ceremony. `state` carries doubt.
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

fn unreference(store: &Store, sha: &str) {
    let s = format!("DELETE FROM acquisitions WHERE content_sha256 = '{sha}'");
    sql(store, s);
}

fn pin(store: &Store, sha: &str, kind: &str, tag: &str, at: i64) {
    let s = format!("INSERT OR IGNORE INTO object_pins VALUES ('{sha}','{kind}','{tag}',{at})");
    sql(store, s);
}

fn unpin(store: &Store, sha: &str, kind: &str) {
    let s =
        format!("DELETE FROM object_pins WHERE object_sha256 = '{sha}' AND pin_kind = '{kind}'");
    sql(store, s);
}

fn backup(store: &Store, open: bool) {
    let statement = if open {
        "INSERT OR REPLACE INTO backup_generations
         (generation_id, started_at, state, location)
         VALUES ('gen-1', 0, 'in_progress', '/backup/gen-1')"
    } else {
        "UPDATE backup_generations SET state = 'complete', completed_at = 1
         WHERE state = 'in_progress'"
    };
    sql(store, statement.to_owned());
}

fn breaker(store: &Store, open: bool) {
    draft_rows(store);
    let statement = if open {
        format!(
            "INSERT OR REPLACE INTO breaker_events
             (event_id, account_id, kind, details_json, state, created_at)
             VALUES ('brk-1', '{ACCOUNT}', 'bulk_disappearance', '{{}}', 'open', 0)"
        )
    } else {
        "UPDATE breaker_events SET state = 'kept' WHERE event_id = 'brk-1'".to_owned()
    };
    sql(store, statement);
}

/// The phantom row whose `object_state` carries integrity doubt.
fn doubt(store: &Store, in_doubt: bool) {
    let s = format!(
        "INSERT OR REPLACE INTO acquisitions
         (content_sha256, byte_length, object_state, first_acquired_at)
         VALUES ('{PHANTOM_SHA}', 7, '{}', 0)",
        if in_doubt { "missing" } else { "published" }
    );
    sql(store, s);
}

/// Publish `payload` and free its reservation: a bare orphan.
fn orphan(store: &Store, payload: &[u8]) -> (ObjectId, String) {
    let publication = store.objects().publish(payload).unwrap();
    let (id, sha) = (*publication.id(), publication.id().sha256_hex());
    abandon(store, publication);
    (id, sha)
}

/// An orphan whose grace clock already ran out.
fn expired_orphan(store: &Store, clock: &TestClock, payload: &[u8]) -> (ObjectId, String) {
    let (id, sha) = orphan(store, payload);
    let report = store.collect_garbage(GC_GRACE_PERIOD).unwrap();
    assert_eq!(report.grace_started, 1, "first sight must pin: {report:?}");
    clock.set(clock.now().as_millis() + grace_ms() + 1);
    (id, sha)
}

/// The pass must clear the clock and keep the object.
fn assert_kept(store: &Store, id: &ObjectId, sha: &str) {
    let report = store.collect_garbage(GC_GRACE_PERIOD).unwrap();
    assert_eq!((report.grace_cleared, report.removed), (1, 0), "{report:?}");
    assert!(exists(store, id));
    assert_eq!(pins(store, sha, "gc_grace"), 0);
}

// --- the grace clock (acceptance: "GC's grace clock") ---

/// First sight inserts `gc_grace`; the pin holds inside the period;
/// the orphan falls once it is past.
#[test]
fn the_grace_clock_runs_from_first_sight_to_removal() {
    assert_eq!(GC_GRACE_PERIOD, Duration::from_secs(7 * 24 * 60 * 60));
    let (_scratch, store, clock) = open("gc-grace");
    let (id, sha) = orphan(&store, PAYLOAD);

    let first = store.collect_garbage(GC_GRACE_PERIOD).unwrap();
    assert_eq!(
        first,
        GcReport {
            scanned: 1,
            grace_started: 1,
            ..GcReport::default()
        }
    );
    assert_eq!(pins(&store, &sha, "gc_grace"), 1);
    assert!(exists(&store, &id));

    // Still inside the period: the pin holds the object.
    let inside = store.collect_garbage(GC_GRACE_PERIOD).unwrap();
    assert_eq!((inside.grace_started, inside.kept), (0, 1));
    assert_eq!(inside.removed, 0);
    assert!(exists(&store, &id));

    clock.set(FIXED_NOW_MS + grace_ms() + 1);
    let expired = store.collect_garbage(GC_GRACE_PERIOD).unwrap();
    assert_eq!((expired.removed, expired.failed), (1, 0));
    assert!(!exists(&store, &id));
    assert_eq!(pins(&store, &sha, ""), 0, "no stale pin survives");
    store.close().unwrap();
}

/// A reference reappearing deletes the pin (spec/03); orphaning again
/// restarts the clock from a new first sight.
#[test]
fn a_reference_deletes_the_grace_clock() {
    let (_scratch, store, clock) = open("gc-reclock");
    let (id, sha) = orphan(&store, PAYLOAD);
    store.collect_garbage(GC_GRACE_PERIOD).unwrap();
    assert_eq!(pins(&store, &sha, "gc_grace"), 1);

    reference(&store, &id, "published");
    let reclocked = store.collect_garbage(GC_GRACE_PERIOD).unwrap();
    assert_eq!((reclocked.grace_cleared, reclocked.removed), (1, 0));
    assert_eq!(pins(&store, &sha, "gc_grace"), 0);
    assert!(exists(&store, &id));

    // Unreferenced again: a fresh first sight restarts the clock;
    // the old pin's age does not carry over.
    unreference(&store, &sha);
    clock.set(FIXED_NOW_MS + 30 * DAY_MS);
    let restarted = store.collect_garbage(GC_GRACE_PERIOD).unwrap();
    assert_eq!((restarted.grace_started, restarted.removed), (1, 0));
    assert!(exists(&store, &id));

    // The restarted clock behaves like any other: only expiry removes.
    clock.set(FIXED_NOW_MS + 30 * DAY_MS + grace_ms() + 1);
    let gone = store.collect_garbage(GC_GRACE_PERIOD).unwrap();
    assert_eq!(gone.removed, 1);
    assert!(!exists(&store, &id));
    store.close().unwrap();
}

/// An aborted reservation leaves a plain orphan and dropping the
/// value queues no work; a live one holds past grace until startup
/// expiry spends it (spec/03).
#[test]
fn an_aborted_reservation_and_an_expired_one_let_the_orphan_fall() {
    let (_scratch, paths) = scratch_paths("gc-reservation-lapse");
    let clock = TestClock::at(FIXED_NOW_MS);
    let sha;
    {
        let store = open_with(&paths, &clock, FIXED_NOW_MS, real_fs());
        let aborted = store.objects().publish(PAYLOAD).unwrap();
        sha = aborted.id().sha256_hex();
        let id = *aborted.id();
        assert_eq!(pins(&store, &sha, "reservation"), 1);
        abandon(&store, aborted);
        assert_eq!(pins(&store, &sha, ""), 0, "abort deletes its token");

        // A second publication reserves the same bytes anew; once
        // dropped, its pin still outlives the grace period.
        {
            let _reserved = store.objects().publish(PAYLOAD).unwrap();
        }
        assert_eq!(pins(&store, &sha, "reservation"), 1);
        let sighted = store.collect_garbage(GC_GRACE_PERIOD).unwrap();
        assert_eq!((sighted.grace_started, sighted.removed), (1, 0));
        clock.set(FIXED_NOW_MS + grace_ms() + 1);
        let held = store.collect_garbage(GC_GRACE_PERIOD).unwrap();
        assert_eq!((held.kept, held.removed), (1, 0), "{held:?}");
        assert!(exists(&store, &id));
        store.close().unwrap();
    }
    // Startup expiry spends the abandoned reservation; the expired
    // clock then lets the orphan fall.
    let store = open_with(&paths, &clock, FIXED_NOW_MS + grace_ms() + 1, real_fs());
    assert_eq!(pins(&store, &sha, "reservation"), 0);
    assert_eq!(store.collect_garbage(GC_GRACE_PERIOD).unwrap().removed, 1);
    assert_eq!(pins(&store, &sha, ""), 0);
    store.close().unwrap();
}

// --- the gates (acceptance: "GC never removes a referenced, pinned or
// in-backup object") ---

type RefRow = (&'static [u8], &'static dyn Fn(&Store, &ObjectId, &str));
type GateRow = (GcGate, &'static dyn Fn(&Store, bool));

/// Every named reference kind keeps its expired orphan (spec/03).
#[test]
fn every_reference_kind_keeps_its_object() {
    let (_scratch, store, clock) = open("gc-references");
    let refers: [RefRow; 3] = [
        (PAYLOAD, &|s, id, _| reference(s, id, "published")),
        (b"f", &|s, _, sha| send_attempt(s, sha, "frozen")),
        (b"a", &|s, _, sha| draft(s, Some(sha))),
    ];
    for (payload, refer) in refers {
        let (id, sha) = expired_orphan(&store, &clock, payload);
        refer(&store, &id, &sha);
        assert_kept(&store, &id, &sha);
    }
    store.close().unwrap();
}

/// Any pin kind but `gc_grace` holds the expired orphan (spec/03).
#[test]
fn a_non_grace_pin_blocks_removal() {
    let (_scratch, store, clock) = open("gc-pins");
    for (i, kind) in ["quarantine", "backup_in_progress", "send_attempt"]
        .into_iter()
        .enumerate()
    {
        let payload = format!("Subject: pin {i}\r\n\r\nheld {i}\r\n");
        let (id, sha) = expired_orphan(&store, &clock, payload.as_bytes());
        pin(&store, &sha, kind, "because", FIXED_NOW_MS);
        let held = store.collect_garbage(GC_GRACE_PERIOD).unwrap();
        assert_eq!((held.kept, held.removed), (1, 0), "{kind}");
        assert!(exists(&store, &id), "{kind} let go");
        unpin(&store, &sha, kind);
        let gone = store.collect_garbage(GC_GRACE_PERIOD).unwrap();
        assert_eq!(gone.removed, 1, "{kind}");
        assert!(!exists(&store, &id));
    }
    store.close().unwrap();
}

/// Every whole-pass freeze gate holds and reports (spec/03); a
/// `kept` breaker event does not freeze.
#[test]
fn every_whole_pass_gate_freezes_the_pass() {
    let (_scratch, store, clock) = open("gc-gates");
    let gates: [GateRow; 3] = [
        (GcGate::BackupInProgress, &backup),
        (GcGate::BreakerOpen, &breaker),
        (GcGate::IntegrityDoubt, &doubt),
    ];
    for (i, (gate, set)) in gates.into_iter().enumerate() {
        let payload = format!("Subject: gate {i}\r\n\r\norphan {i}\r\n");
        let (id, _sha) = expired_orphan(&store, &clock, payload.as_bytes());
        set(&store, true);
        let blocked = store.collect_garbage(GC_GRACE_PERIOD).unwrap();
        assert_eq!(blocked.blocked, Some(gate), "{gate:?} freezes");
        assert_eq!((blocked.removed, blocked.grace_started), (0, 0));
        assert!(exists(&store, &id), "{gate:?} removed nothing");
        set(&store, false);
        let free = store.collect_garbage(GC_GRACE_PERIOD).unwrap();
        assert_eq!((free.blocked, free.removed), (None, 1), "{gate:?}");
    }
    store.close().unwrap();
}

/// The writer thread cannot run the pass (it would await itself).
#[test]
fn the_writer_thread_cannot_run_gc_on_its_own_queue() {
    let (_scratch, store, _clock) = open("gc-own-writer");
    let store = Arc::new(store);
    let on_writer = Arc::clone(&store);
    let err = block_on(store.write(WritePriority::User, move |_tx| {
        Ok(on_writer.collect_garbage(GC_GRACE_PERIOD).unwrap_err())
    }))
    .unwrap();
    assert!(matches!(err, StoreError::Internal { .. }), "{err:?}");
    Arc::into_inner(store).unwrap().close().unwrap();
}

/// A failed unlink is never a removal: the re-check already deleted
/// the grace pins, so the next pass restarts the clock (spec/03).
#[test]
fn a_failed_unlink_starts_the_clock_over() {
    let (_scratch, paths) = scratch_paths("gc-unlink-fail");
    let clock = TestClock::at(FIXED_NOW_MS);
    let fs = Arc::new(FaultFs::new(real_fs(), FsOp::Remove, 1, FaultKind::Eio));
    let store = open_with(&paths, &clock, FIXED_NOW_MS, fs);
    let (id, sha) = expired_orphan(&store, &clock, PAYLOAD);
    let failed = store.collect_garbage(GC_GRACE_PERIOD).unwrap();
    assert_eq!((failed.failed, failed.removed), (1, 0), "{failed:?}");
    assert!(exists(&store, &id));
    assert_eq!(pins(&store, &sha, ""), 0);

    let restarted = store.collect_garbage(GC_GRACE_PERIOD).unwrap();
    assert_eq!((restarted.grace_started, restarted.removed), (1, 0));
    assert!(exists(&store, &id));

    clock.set(FIXED_NOW_MS + 30 * DAY_MS);
    let gone = store.collect_garbage(GC_GRACE_PERIOD).unwrap();
    assert_eq!(gone.removed, 1, "{gone:?}");
    assert!(!exists(&store, &id));
    store.close().unwrap();
}

// --- the republish race (acceptance: both orderings) ---

/// The scaffold both orderings share: one expired orphan and a
/// running import job for the referencing commit.
fn race_store(label: &str) -> (ScratchDir, Arc<Store>, Arc<TestClock>, ObjectId, i64) {
    let (scratch, store, clock) = open(label);
    let store = Arc::new(store);
    let (id, _sha) = expired_orphan(&store, &clock, PAYLOAD);
    let revision = running_job(&store, &clock);
    (scratch, store, clock, id, revision)
}

/// Ordering A: the reservation commits first. The publication parks
/// before submitting its reservation and the writer stays blocked, so
/// the commit only queues. The scan queues a removal task behind it;
/// the task's re-check sees the landed pin and skips.
#[test]
fn a_reservation_committed_first_is_honoured_at_the_final_recheck() {
    let (_scratch, store, clock, id, revision) = race_store("gc-race-reservation-first");
    let sha = id.sha256_hex();
    let reservation_gate = Arc::new(Barrier::new(2));
    store.objects().set_pauses(Pauses {
        before_reservation: Some(Arc::clone(&reservation_gate)),
        ..Pauses::default()
    });
    let writer_gate = Arc::new(Barrier::new(2));
    let blocked = {
        let gate = Arc::clone(&writer_gate);
        store.write(WritePriority::Index, move |_tx| {
            gate.wait();
            gate.wait();
            Ok(())
        })
    };
    writer_gate.wait(); // the writer holds the queue
    let on_thread = Arc::clone(&store);
    let parked = std::thread::spawn(move || on_thread.objects().publish(PAYLOAD));
    reservation_gate.wait(); // the publish parked before its commit
    std::thread::sleep(Duration::from_millis(100));
    let on_thread = Arc::clone(&store);
    let pass = std::thread::spawn(move || on_thread.collect_garbage(GC_GRACE_PERIOD));
    std::thread::sleep(Duration::from_millis(100));
    writer_gate.wait(); // release: the reservation commit, then the task
    let report = pass.join().unwrap().unwrap();
    parked.join().unwrap().unwrap();
    block_on(blocked).unwrap();
    store.objects().set_pauses(Pauses::default());

    assert_eq!((report.removed, report.skipped), (0, 1));
    assert!(exists(&store, &id));
    assert_eq!(pins(&store, &sha, "reservation"), 1);

    // A fresh publication's commit releases only its own token; the
    // parked publication's pin survives it.
    let fresh = store.objects().publish(PAYLOAD).unwrap();
    commit(&store, &clock, revision, fresh, "/gc/race.mbox");
    assert!(exists(&store, &id), "the referenced object lives");
    assert_eq!(pins(&store, &sha, "reservation"), 1, "only its token");
    Arc::into_inner(store).unwrap().close().unwrap();
}

/// Ordering B: the unlink lands first. The removal task parks after
/// its re-check commit (the grace pins already deleted); the publish
/// parks before its reservation, whose commit then provably queues
/// behind the parked task. Released, the install step installs anew.
#[test]
fn an_unlink_first_lets_the_republished_reservation_install_anew() {
    let (_scratch, store, clock, id, revision) = race_store("gc-race-unlink-first");
    let sha = id.sha256_hex();
    let reservation_gate = Arc::new(Barrier::new(2));
    let unlink_gate = Arc::new(Barrier::new(2));
    store.objects().set_pauses(Pauses {
        before_reservation: Some(Arc::clone(&reservation_gate)),
        before_unlink: Some(Arc::clone(&unlink_gate)),
    });
    let on_thread = Arc::clone(&store);
    let pass = std::thread::spawn(move || on_thread.collect_garbage(GC_GRACE_PERIOD));
    unlink_gate.wait(); // the removal task parked: re-check committed
    assert_eq!(pins(&store, &sha, "gc_grace"), 0, "pins deleted");
    let on_thread = Arc::clone(&store);
    let parked = std::thread::spawn(move || on_thread.objects().publish(PAYLOAD));
    reservation_gate.wait(); // the publish parked before its commit
    // Released, the commit must queue behind the parked task: wait on
    // the real enqueue, then prove it can neither land nor finish.
    let deadline = std::time::Instant::now() + Duration::from_secs(30);
    while store.writer.queued(WritePriority::Index) == 0 {
        assert!(std::time::Instant::now() < deadline, "never queued");
        std::thread::yield_now();
    }
    assert_eq!(pins(&store, &sha, "reservation"), 0, "not committed");
    assert!(!parked.is_finished(), "the publish still awaits");
    unlink_gate.wait(); // release the unlink; the queued commit runs
    let report = pass.join().unwrap().unwrap();
    let publication = parked.join().unwrap().unwrap();
    assert_eq!((report.removed, report.skipped), (1, 0));
    assert!(exists(&store, &id), "step 3 installed the new copy");
    assert_eq!(pins(&store, &sha, "reservation"), 1);

    // The new copy is the published bytes, not a shell.
    let mut bytes = String::new();
    store
        .objects()
        .open(&id)
        .unwrap()
        .read_to_string(&mut bytes)
        .unwrap();
    assert_eq!(bytes.as_bytes(), PAYLOAD);
    commit(&store, &clock, revision, publication, "/gc/race.mbox");
    assert!(exists(&store, &id));
    assert_eq!(pins(&store, &sha, ""), 0);
    Arc::into_inner(store).unwrap().close().unwrap();
}

// --- random reference histories (acceptance: the proptest) ---

/// One slot in a history: the object's identity plus its model state.
#[derive(Default)]
struct Slot {
    id: Option<ObjectId>,
    sha: String,
    exists: bool,
    /// Live `PublishedObject`s; each holds a reservation pin.
    publications: Vec<PublishedObject>,
    /// A direct `acquisitions` row (the import's row is permanent).
    direct_ref: bool,
    import_ref: bool,
    /// The `quarantine` fixture pins held on it.
    pins: u32,
    /// The grace pin's `created_at`, mirroring the database.
    grace_oldest: Option<i64>,
}

/// The whole pass's frozen flags, mirroring the fixture rows.
#[derive(Default)]
struct World {
    slots: Vec<Slot>,
    /// The one draft revision's attached sha and attempt rows per sha.
    draft_sha: Option<String>,
    attempt_shas: HashMap<String, u32>,
    backup_open: bool,
    breaker_open: bool,
    doubt: bool,
    /// The import job's expected revision; each commit consumes one.
    revision: i64,
    now: i64,
    seq: u64,
}

impl World {
    /// What spec/03 calls a reference: the three tables.
    fn referenced(&self, i: usize) -> bool {
        let slot = &self.slots[i];
        slot.direct_ref
            || slot.import_ref
            || self.draft_sha.as_deref() == Some(slot.sha.as_str())
            || self.attempt_shas.get(&slot.sha).copied().unwrap_or(0) > 0
    }

    /// Non-grace pins: fixture pins plus live publication reservations.
    fn held(&self, i: usize) -> bool {
        self.slots[i].pins > 0 || !self.slots[i].publications.is_empty()
    }

    /// The whole-pass freeze flags.
    fn frozen(&self) -> bool {
        self.backup_open || self.breaker_open || self.doubt
    }

    /// May the pass remove this object right now (spec/03's gates)?
    fn legal_remove(&self, i: usize, cutoff: i64) -> bool {
        !self.referenced(i)
            && !self.held(i)
            && self.slots[i].grace_oldest.is_some_and(|o| o < cutoff)
            && !self.frozen()
    }
}

/// One history step in spec/03's vocabulary.
#[derive(Debug, Clone)]
enum Op {
    Publish(usize),
    ImportRef(usize),
    Abort(usize),
    DirectRef(usize),
    Unreference(usize),
    DraftAttach(usize),
    DraftDetach(usize),
    Attempt(usize),
    AttemptDrop(usize),
    Pin(usize),
    Unpin(usize),
    Backup(bool),
    Breaker(bool),
    Doubt(bool),
    AdvanceDays(u8),
    Gc(u8),
}

const SLOTS: usize = 6;

fn op() -> impl Strategy<Value = Op> {
    prop_oneof![
        4 => (0..SLOTS).prop_map(Op::Publish),
        2 => (0..SLOTS).prop_map(Op::ImportRef),
        3 => (0..SLOTS).prop_map(Op::Abort),
        3 => (0..SLOTS).prop_map(Op::DirectRef),
        2 => (0..SLOTS).prop_map(Op::Unreference),
        2 => (0..SLOTS).prop_map(Op::DraftAttach),
        1 => (0..SLOTS).prop_map(Op::DraftDetach),
        2 => (0..SLOTS).prop_map(Op::Attempt),
        1 => (0..SLOTS).prop_map(Op::AttemptDrop),
        2 => (0..SLOTS).prop_map(Op::Pin),
        1 => (0..SLOTS).prop_map(Op::Unpin),
        1 => prop::bool::ANY.prop_map(Op::Backup),
        1 => prop::bool::ANY.prop_map(Op::Breaker),
        1 => prop::bool::ANY.prop_map(Op::Doubt),
        3 => (0..15u8).prop_map(Op::AdvanceDays),
        6 => (0..9u8).prop_map(Op::Gc),
    ]
}

/// The sha slot `i` would reference, if it has been published.
fn slot_sha(world: &World, i: usize) -> Option<String> {
    let slot = world.slots.get(i)?;
    slot.id.is_some().then(|| slot.sha.clone())
}

/// Apply `op` to store and model together (real writer, real store).
fn apply(store: &Store, clock: &Arc<TestClock>, world: &mut World, op: Op) {
    match op {
        Op::Publish(i) => {
            // One payload per slot: a republish reserves anew.
            let publication = store
                .objects()
                .publish(format!("payload-{i}").as_bytes())
                .unwrap();
            // Never truncate: earlier slots hold live reservations.
            world
                .slots
                .resize_with((i + 1).max(world.slots.len()), Slot::default);
            let slot = &mut world.slots[i];
            if slot.id.is_none() {
                slot.id = Some(*publication.id());
                slot.sha = publication.id().sha256_hex();
            }
            assert_eq!(Some(*publication.id()), slot.id, "one payload, one object");
            slot.exists = true;
            slot.publications.push(publication);
        }
        Op::ImportRef(i) => {
            let Some(publication) = world.slots.get_mut(i).and_then(|s| s.publications.pop())
            else {
                return;
            };
            let tag = format!("/gc/{i}-{}.mbox", world.seq);
            world.seq += 1;
            commit(store, clock, world.revision, publication, &tag);
            world.revision += 1;
            world.slots[i].import_ref = true;
        }
        Op::Abort(i) => {
            // The caller's abort spends the reservation (spec/03).
            if let Some(publication) = world.slots.get_mut(i).and_then(|s| s.publications.pop()) {
                abandon(store, publication);
            }
        }
        Op::DirectRef(i) => {
            let Some(slot) = world.slots.get(i) else {
                return;
            };
            if let Some(id) = slot.id.filter(|_| !slot.direct_ref && !slot.import_ref) {
                reference(store, &id, "published");
                world.slots[i].direct_ref = true;
            }
        }
        Op::Unreference(i) => {
            // Only a direct row may be deleted (the import's is not).
            if let Some(sha) = slot_sha(world, i)
                .filter(|_| world.slots[i].direct_ref && !world.slots[i].import_ref)
            {
                unreference(store, &sha);
                world.slots[i].direct_ref = false;
            }
        }
        Op::DraftAttach(i) => {
            if let Some(sha) =
                slot_sha(world, i).filter(|s| world.draft_sha.as_deref() != Some(s.as_str()))
            {
                draft(store, Some(&sha));
                world.draft_sha = Some(sha);
            }
        }
        Op::DraftDetach(i) => {
            if slot_sha(world, i).as_deref() != world.draft_sha.as_deref() {
                return;
            }
            draft(store, None);
            world.draft_sha = None;
        }
        Op::Attempt(i) => {
            let Some(sha) = slot_sha(world, i) else {
                return;
            };
            send_attempt(store, &sha, &format!("{}", world.seq));
            world.seq += 1;
            *world.attempt_shas.entry(sha).or_insert(0) += 1;
        }
        Op::AttemptDrop(i) => {
            let Some(sha) = slot_sha(world, i) else {
                return;
            };
            sql(
                store,
                format!("DELETE FROM send_attempts WHERE mime_sha256 = '{sha}'"),
            );
            world.attempt_shas.remove(&sha);
        }
        Op::Pin(i) => {
            let Some(sha) = slot_sha(world, i) else {
                return;
            };
            let at = world.seq;
            pin(store, &sha, "quarantine", &format!("p-{at}"), world.now);
            world.seq += 1;
            world.slots[i].pins += 1;
        }
        Op::Unpin(i) => {
            let Some(sha) = world.slots.get(i).map(|s| s.sha.clone()) else {
                return;
            };
            unpin(store, &sha, "quarantine");
            world.slots[i].pins = 0;
        }
        Op::Backup(open) if world.backup_open != open => {
            backup(store, open);
            world.backup_open = open;
        }
        Op::Breaker(open) if world.breaker_open != open => {
            breaker(store, open);
            world.breaker_open = open;
        }
        Op::Doubt(doubted) => {
            doubt(store, doubted);
            world.doubt = doubted;
        }
        Op::AdvanceDays(days) => {
            world.now += i64::from(days) * DAY_MS;
            clock.set(world.now);
        }
        Op::Gc(days) => run_gc(store, world, days),
        _ => {}
    }
}

/// One pass, asserted against the model: the report is exact and
/// nothing protected is unlinked.
fn run_gc(store: &Store, world: &mut World, days: u8) {
    let cutoff = world.now.saturating_sub(i64::from(days) * DAY_MS);
    let (mut scanned, mut started, mut cleared, mut removals) = (0, 0, 0, Vec::new());
    for i in 0..world.slots.len() {
        if !(world.slots[i].id.is_some() && world.slots[i].exists) {
            continue;
        }
        scanned += 1;
        match (world.referenced(i), world.slots[i].grace_oldest) {
            (true, Some(_)) => cleared += 1,
            (false, None) => started += 1,
            _ => {}
        }
        if world.legal_remove(i, cutoff) {
            removals.push(i);
        }
    }
    let report = store
        .collect_garbage(Duration::from_secs(u64::from(days) * 86_400))
        .unwrap();
    if world.frozen() {
        // A frozen gate writes and removes nothing.
        assert_eq!((report.removed, report.grace_started), (0, 0));
        assert!(report.blocked.is_some(), "{report:?}");
        return;
    }
    assert_eq!(
        report,
        GcReport {
            scanned,
            grace_started: started,
            grace_cleared: cleared,
            kept: scanned - started - cleared - removals.len() as u64,
            removed: removals.len() as u64,
            ..GcReport::default()
        },
        "the pass's whole report must match the model"
    );
    // Every legal orphan fell; every protected one stands. Mirror
    // the pass's pin bookkeeping on the model.
    for i in 0..world.slots.len() {
        if !(world.slots[i].id.is_some() && world.slots[i].exists) {
            continue;
        }
        let (removed, referenced) = (removals.contains(&i), world.referenced(i));
        let id = world.slots[i].id.unwrap();
        let slot = &mut world.slots[i];
        if removed {
            assert!(!exists(store, &id), "legal orphan survived: {i}");
            slot.exists = false;
        } else {
            assert!(exists(store, &id), "protected object removed: {i}");
        }
        if referenced || removed {
            slot.grace_oldest = None;
        } else if slot.grace_oldest.is_none() {
            slot.grace_oldest = Some(world.now);
        }
    }
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(24))]
    /// The acceptance's property, both directions: protected objects
    /// survive and collectable orphans really are removed.
    #[test]
    fn gc_never_removes_a_protected_object(ops in prop::collection::vec(op(), 8..72)) {
        let (_scratch, store, clock) = open("gc-proptest");
        let mut world = World {
            revision: running_job(&store, &clock),
            now: FIXED_NOW_MS,
            ..World::default()
        };
        for op in ops {
            apply(&store, &clock, &mut world, op);
        }
        // A generated history may never reach removal; prove it every
        // case. Per-kind-protected objects must outlive a bare orphan
        // once its grace clock runs out.
        let base = world.slots.len();
        for op in [
            Op::Backup(false), Op::Breaker(false), Op::Doubt(false),
            Op::Publish(base), Op::Abort(base), Op::DraftAttach(base),
            Op::Publish(base + 1), Op::Abort(base + 1), Op::Attempt(base + 1),
            Op::Publish(base + 2), Op::Abort(base + 2), Op::Pin(base + 2),
            Op::Publish(base + 3), Op::Abort(base + 3), Op::Gc(0),
            Op::AdvanceDays(1), Op::Backup(true), Op::Gc(0), Op::Backup(false),
            Op::Gc(0),
        ] {
            apply(&store, &clock, &mut world, op);
        }
        store.close().unwrap();
    }
}
