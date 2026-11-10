//! M1.8h4a: the weekly live sample: bucket and week arithmetic, the
//! verdicts it records on acquisitions, catch-up through the
//! checkpoint, and stopping.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, AtomicI64, Ordering};

use sha2::{Digest, Sha256};

use sb_test_support::{ScratchDir, SeededRandom, block_on, real_fs};

use super::{
    CHECKPOINT, DAY_MS, Finding, SAMPLE_BUCKETS, SampleReport, bucket_of, bucket_of_week, record,
    week_of, weeks_to_cover,
};
use crate::changes::CONTENT_CHANGES;
use crate::objects::{ObjectId, ObjectStore, ReservationRelease, release_reservation};
use crate::schema::bug;
use crate::testing::{FIXED_NOW_MS, reading, scratch_paths, writing};
use crate::{Clock, Seams, Store, StoreError, Timestamp, WritePriority};

struct TestClock(AtomicI64);

impl Clock for TestClock {
    fn now(&self) -> Timestamp {
        Timestamp::from_millis(self.0.load(Ordering::SeqCst))
    }
}

/// Noon on the Wednesday of `week`.
fn in_week(week: i64) -> i64 {
    (7 * week - 3 + 2) * DAY_MS + DAY_MS / 2
}

/// The first week from `FIXED_NOW_MS` on that samples `bucket`.
fn week_for(bucket: u8) -> i64 {
    let mut week = week_of(FIXED_NOW_MS);
    while bucket_of_week(week) != bucket {
        week += 1;
    }
    week
}

fn open(label: &str) -> (ScratchDir, Store, Arc<TestClock>) {
    let (scratch, paths) = scratch_paths(label);
    let clock = Arc::new(TestClock(AtomicI64::new(FIXED_NOW_MS)));
    let seams = Seams::new(real_fs(), clock.clone(), Arc::new(SeededRandom::new(7)));
    (scratch, crate::open(&paths, &seams).unwrap(), clock)
}

fn set_week(clock: &TestClock, week: i64) {
    clock.0.store(in_week(week), Ordering::SeqCst);
}

/// A payload whose object falls in `bucket`, distinct per `tag`.
fn payload_in(bucket: u8, tag: &str) -> Vec<u8> {
    (0u32..)
        .map(|n| format!("Subject: {tag} {n}\r\n\r\nsampled bytes\r\n").into_bytes())
        .find(|bytes| bucket_of(&Sha256::digest(bytes).into()) == bucket)
        .unwrap()
}

/// A published object, its reservation released.
fn object(store: &Store, payload: &[u8]) -> ObjectId {
    let publication = store.objects().publish(payload).unwrap();
    let id = *publication.id();
    writing(store, move |tx| {
        release_reservation(tx, &publication, ReservationRelease::Required)
    });
    id
}

/// An acquisition referencing `id` in `state`.
fn acquisition(store: &Store, id: &ObjectId, state: &str) {
    let statement = format!(
        "INSERT INTO acquisitions (content_sha256, byte_length, object_state, first_acquired_at)
         VALUES ('{}', {}, '{state}', 0)",
        id.sha256_hex(),
        id.byte_length()
    );
    writing(store, move |tx| tx.execute_batch(&statement).map_err(bug));
}

fn state(store: &Store, id: &ObjectId) -> String {
    let sha = id.sha256_hex();
    reading(store, move |tx| {
        tx.query_row(
            "SELECT object_state FROM acquisitions WHERE content_sha256 = ?1",
            [sha],
            |row| row.get(0),
        )
        .map_err(bug)
    })
}

/// The index queue's reason for `id`'s acquisition, if queued.
fn queued(store: &Store, id: &ObjectId) -> Option<String> {
    let sha = id.sha256_hex();
    reading(store, move |tx| {
        use rusqlite::OptionalExtension;
        tx.query_row(
            "SELECT q.reason FROM d_index_queue q
             JOIN acquisitions a ON a.acquisition_id = q.doc_id
             WHERE a.content_sha256 = ?1",
            [sha],
            |row| row.get(0),
        )
        .optional()
        .map_err(bug)
    })
}

fn meta(store: &Store, key: &str) -> Option<String> {
    let key = key.to_owned();
    reading(store, move |tx| {
        use rusqlite::OptionalExtension;
        tx.query_row(
            "SELECT value FROM store_meta WHERE key = ?1",
            [key],
            |row| row.get(0),
        )
        .optional()
        .map_err(bug)
    })
}

fn damage(store: &Store, id: &ObjectId) {
    let path = store.objects().object_path(id);
    let mut bytes = std::fs::read(&path).unwrap();
    bytes[0] ^= 0x20;
    std::fs::write(&path, bytes).unwrap();
}

fn remove(store: &Store, id: &ObjectId) {
    std::fs::remove_file(store.objects().object_path(id)).unwrap();
}

fn run(store: &Store) -> SampleReport {
    store.verify_sample(&AtomicBool::new(false)).unwrap()
}

// --- arithmetic ---

#[test]
fn weeks_start_on_monday_and_rotate_through_52_buckets() {
    // 1970-01-04 was a Sunday, 1970-01-05 a Monday.
    assert_eq!(week_of(3 * DAY_MS + DAY_MS - 1), 0);
    assert_eq!(week_of(4 * DAY_MS), 1);
    assert_eq!(week_of(-1), 0, "the Monday before the epoch");
    assert_eq!(week_of(-3 * DAY_MS - 1), -1);
    assert_eq!(week_of(in_week(2_900)), 2_900);
    assert_eq!(bucket_of_week(52), 0);
    assert_eq!(bucket_of_week(-1), SAMPLE_BUCKETS - 1);

    let mut sha = [0u8; 32];
    assert_eq!(bucket_of(&sha), 0);
    sha[7] = 53;
    assert_eq!(bucket_of(&sha), 1);
    sha = [0xff; 32];
    assert_eq!(bucket_of(&sha), (u64::MAX % 52) as u8);
}

#[test]
fn a_run_covers_the_weeks_since_its_checkpoint_up_to_a_full_rotation() {
    assert_eq!(weeks_to_cover(None, 100), [100]);
    assert!(weeks_to_cover(Some(100), 100).is_empty(), "already done");
    assert_eq!(weeks_to_cover(Some(105), 100), [100], "the clock went back");
    assert_eq!(weeks_to_cover(Some(97), 100), [98, 99, 100]);
    let year = weeks_to_cover(Some(10), 100);
    assert_eq!(year.len(), usize::from(SAMPLE_BUCKETS));
    assert_eq!((year[0], year[51]), (49, 100));
}

// --- verdicts ---

#[test]
fn a_run_reads_only_its_bucket_and_records_what_it_finds() {
    let (_scratch, store, clock) = open("sample-verdicts");
    let bucket = 7;
    let absent = object(&store, &payload_in(bucket, "absent"));
    let damaged = object(&store, &payload_in(bucket, "damaged"));
    let intact = object(&store, &payload_in(bucket, "intact"));
    let elsewhere = object(&store, &payload_in(bucket + 1, "elsewhere"));
    for id in [&absent, &damaged, &intact, &elsewhere] {
        acquisition(&store, id, "published");
    }
    remove(&store, &absent);
    damage(&store, &damaged);
    damage(&store, &elsewhere);
    let generation = meta(&store, "search_generation");
    let week = week_for(bucket);
    set_week(&clock, week);

    let report = run(&store);
    assert_eq!(
        report,
        SampleReport {
            buckets: vec![bucket],
            checked: 3,
            intact: 1,
            missing: 1,
            quarantined: 1,
            ..SampleReport::default()
        }
    );
    assert_eq!(state(&store, &absent), "missing");
    assert_eq!(state(&store, &damaged), "quarantined");
    assert!(!store.objects().exists(&damaged).unwrap(), "in recovery/");
    assert_eq!(state(&store, &intact), "published");
    // Each lost object leaves the index; search sees one new generation.
    assert_eq!(queued(&store, &absent).as_deref(), Some("delete"));
    assert_eq!(queued(&store, &damaged).as_deref(), Some("delete"));
    assert_eq!(queued(&store, &intact), None);
    assert_ne!(meta(&store, "search_generation"), generation);
    // Another bucket's damage waits for its week.
    assert_eq!(state(&store, &elsewhere), "published");
    assert!(store.objects().exists(&elsewhere).unwrap());
    assert_eq!(meta(&store, CHECKPOINT), Some(week.to_string()));
}

#[test]
fn intact_bytes_heal_and_damage_upgrades_a_missing_acquisition() {
    let (_scratch, store, clock) = open("sample-heal");
    let bucket = 11;
    let healed = object(&store, &payload_in(bucket, "healed"));
    let recovered = object(&store, &payload_in(bucket, "recovered"));
    let worse = object(&store, &payload_in(bucket, "worse"));
    let known = object(&store, &payload_in(bucket, "known"));
    acquisition(&store, &healed, "missing");
    acquisition(&store, &recovered, "quarantined");
    acquisition(&store, &worse, "missing");
    acquisition(&store, &known, "quarantined");
    damage(&store, &worse);
    remove(&store, &known);
    set_week(&clock, week_for(bucket));

    let report = run(&store);
    assert_eq!(
        (
            report.checked,
            report.intact,
            report.healed,
            report.quarantined
        ),
        (4, 2, 2, 2),
        "{report:?}"
    );
    assert_eq!(state(&store, &healed), "published");
    assert_eq!(state(&store, &recovered), "published");
    assert_eq!(queued(&store, &healed).as_deref(), Some("reextract"));
    assert_eq!(queued(&store, &recovered).as_deref(), Some("reextract"));
    // Already out of the index: no new revision.
    assert_eq!(state(&store, &worse), "quarantined");
    assert_eq!(queued(&store, &worse), None);
    // Its file is in recovery/ already: nothing changes.
    assert_eq!(state(&store, &known), "quarantined");
}

/// Draft attachments have no state column: their verdict is counted.
/// An unreferenced file is sampled too.
#[test]
fn other_references_and_orphans_are_counted() {
    let (_scratch, store, clock) = open("sample-references");
    let bucket = 3;
    let orphan = object(&store, &payload_in(bucket, "orphan"));
    let damaged_orphan = object(&store, &payload_in(bucket, "damaged orphan"));
    let attachment = object(&store, &payload_in(bucket, "attachment"));
    let draft_sha = attachment.sha256_hex();
    writing(&store, move |tx| {
        tx.execute_batch(&format!(
            "INSERT INTO drafts (draft_id, state, current_revision_id, created_at, updated_at)
             VALUES ('00000000-0000-4000-8000-0000000000d1', 'editing',
               '00000000-0000-4000-8000-0000000000d2', 0, 0);
             INSERT INTO draft_revisions (revision_id, draft_id, seq, compose_format, subject,
               to_json, cc_json, bcc_json, body_source, attachments_json, created_at)
             VALUES ('00000000-0000-4000-8000-0000000000d2',
               '00000000-0000-4000-8000-0000000000d1', 1, 'plain', '', '[]', '[]', '[]', '',
               '[{{\"object_sha256\":\"{draft_sha}\",\"filename\":\"a.txt\",
                  \"media_type\":\"text/plain\",\"size\":1}}]', 0);"
        ))
        .map_err(bug)
    });
    remove(&store, &attachment);
    damage(&store, &damaged_orphan);
    set_week(&clock, week_for(bucket));

    let report = run(&store);
    assert_eq!(
        (
            report.checked,
            report.intact,
            report.missing,
            report.quarantined
        ),
        (3, 1, 1, 1),
        "{report:?}"
    );
    assert!(store.objects().exists(&orphan).unwrap());
    assert!(
        !store.objects().exists(&damaged_orphan).unwrap(),
        "in recovery/"
    );
}

/// An unreferenced file that vanished since the listing was collected
/// by GC: gone, not missing.
#[test]
fn a_vanished_orphan_is_gone() {
    let mut report = SampleReport::default();
    let mut findings = Vec::new();
    let id = ObjectId::from_hex(&"ab".repeat(32), 1).unwrap();
    super::tally(
        &mut report,
        &super::Target::Orphan { id },
        super::Verdict::Absent,
        &mut findings,
    );
    assert_eq!((report.checked, report.gone, report.missing), (1, 1, 0));
    assert!(findings.is_empty());
}

#[test]
fn a_republished_file_overrides_an_absent_or_damaged_verdict() {
    let (_scratch, store, _clock) = open("sample-republish");
    let id = object(&store, b"Subject: back\r\n\r\nagain\r\n");
    acquisition(&store, &id, "published");
    // The verdicts were read before the file came back: it is there now.
    let healed = record(&store, vec![Finding::Absent(id), Finding::Damaged(id)]).unwrap();
    assert_eq!(healed, 0);
    assert_eq!(state(&store, &id), "published");
    assert_eq!(queued(&store, &id), None);
}

// --- the checkpoint ---

#[test]
fn missed_weeks_are_caught_up_and_the_checkpoint_never_moves_back() {
    let (_scratch, store, clock) = open("sample-catch-up");
    let week = week_for(20);
    set_week(&clock, week - 3);
    assert_eq!(run(&store).buckets, [17]);

    set_week(&clock, week);
    let report = run(&store);
    assert_eq!(report.buckets, [18, 19, 20]);
    assert_eq!(meta(&store, CHECKPOINT), Some(week.to_string()));
    // Again the same week: nothing is due.
    assert_eq!(run(&store), SampleReport::default());

    // The clock went back: its own week's bucket, the checkpoint stays.
    set_week(&clock, week - 10);
    assert_eq!(run(&store).buckets, [bucket_of_week(week - 10)]);
    assert_eq!(meta(&store, CHECKPOINT), Some(week.to_string()));

    // A year away: each bucket once.
    set_week(&clock, week + 200);
    let report = run(&store);
    assert_eq!(report.buckets.len(), usize::from(SAMPLE_BUCKETS));
    assert_eq!(report.buckets[51], bucket_of_week(week + 200));
}

#[test]
fn a_malformed_checkpoint_is_corrupt() {
    let (_scratch, store, _clock) = open("sample-malformed");
    writing(&store, |tx| {
        tx.execute(
            "INSERT INTO store_meta (key, value) VALUES (?1, 'soon')",
            [CHECKPOINT],
        )
        .map_err(bug)
    });
    let failed = store.verify_sample(&AtomicBool::new(false));
    assert!(
        matches!(failed, Err(StoreError::Corrupt { .. })),
        "{failed:?}"
    );
}

/// The checkpoint is bookkeeping; a recorded verdict is the archive's
/// data and counts toward the next generation.
#[test]
fn only_a_recorded_verdict_counts_as_a_change() {
    let (_scratch, store, clock) = open("sample-counting");
    let bucket = 5;
    let id = object(&store, &payload_in(bucket, "counted"));
    acquisition(&store, &id, "published");
    let counter = || meta(&store, CONTENT_CHANGES);
    set_week(&clock, week_for(bucket));

    let before = counter();
    run(&store);
    assert_eq!(counter(), before, "an intact sample changes nothing");

    remove(&store, &id);
    set_week(&clock, week_for(bucket) + 52);
    run(&store);
    assert_ne!(counter(), before, "a missing verdict is data");
}

// --- stopping ---

#[test]
fn a_stopped_run_records_nothing_and_keeps_the_checkpoint() {
    let (_scratch, store, clock) = open("sample-stopped");
    let bucket = 9;
    let id = object(&store, &payload_in(bucket, "stopped"));
    acquisition(&store, &id, "published");
    remove(&store, &id);
    set_week(&clock, week_for(bucket));

    let report = store.verify_sample(&AtomicBool::new(true)).unwrap();
    assert_eq!(
        report,
        SampleReport {
            stopped: true,
            ..SampleReport::default()
        }
    );
    assert_eq!(state(&store, &id), "published");
    assert_eq!(meta(&store, CHECKPOINT), None);
}

/// Stopped after the first object, the run keeps that verdict and
/// leaves the bucket, and the checkpoint, to the next run.
#[test]
fn a_run_stopped_mid_bucket_keeps_what_it_read() {
    let (_scratch, store, clock) = open("sample-stopped-mid");
    let bucket = 13;
    let ids = [
        object(&store, &payload_in(bucket, "first")),
        object(&store, &payload_in(bucket, "second")),
    ];
    for id in &ids {
        acquisition(&store, id, "published");
        remove(&store, id);
    }
    set_week(&clock, week_for(bucket));

    // The first ask is before the sample is collected; an absent object
    // asks once, before its open, so the third ask is the next object's.
    let asked = std::cell::Cell::new(0);
    let report = super::verify_sample_with(&store, &|| {
        asked.set(asked.get() + 1);
        asked.get() > 2
    })
    .unwrap();
    assert!(report.stopped, "{report:?}");
    assert_eq!((report.checked, report.missing), (1, 1), "{report:?}");
    let mut states: Vec<String> = ids.iter().map(|id| state(&store, id)).collect();
    states.sort();
    assert_eq!(states, ["missing", "published"], "the first verdict stands");
    assert_eq!(meta(&store, CHECKPOINT), None);

    // The next run finishes the bucket.
    let report = run(&store);
    assert_eq!(report.buckets, [bucket]);
    assert!(ids.iter().all(|id| state(&store, id) == "missing"));
}

#[test]
fn the_writer_thread_cannot_run_the_sample() {
    let (_scratch, store, _clock) = open("sample-own-writer");
    let store = Arc::new(store);
    let on_writer = Arc::clone(&store);
    let err = block_on(store.write(WritePriority::User, move |_tx| {
        Ok(on_writer
            .verify_sample(&AtomicBool::new(false))
            .unwrap_err())
    }))
    .unwrap();
    assert!(matches!(err, StoreError::Internal { .. }), "{err:?}");
    Arc::into_inner(store).unwrap().close().unwrap();
}
