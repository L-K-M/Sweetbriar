//! The weekly live sample (spec/03 "Reads and verification", plan
//! M1.8h4a): each week re-hashes the objects in one of 52 hash buckets,
//! so every object is read about once a year, and records what it
//! finds on the acquisitions that reference them.
//!
//! - An object's bucket is the first eight bytes of its SHA-256, big
//!   endian, modulo 52. Week `w` (Monday-based UTC weeks since the
//!   epoch, the arithmetic retention uses) samples bucket `w mod 52`.
//! - `store_meta.verified_through_week` is the last week whose bucket a
//!   run finished. A run covers every week after it, up to 52 (all
//!   buckets), so missed weeks are caught up; without one, or with the
//!   clock behind it, a run covers the current week. It moves forward
//!   bucket by bucket and never back.
//! - The sample is every referenced object in the bucket (acquisitions,
//!   send attempts, draft attachments) plus every object file listed in
//!   it, read through the object store's verifying reader: a damaged
//!   file moves to `recovery/`, an absent one is `NotFound`.
//! - Verdicts on acquisitions change `object_state` with the index
//!   revision that goes with it, in one transaction: absent turns
//!   `published` into `missing`, damaged turns `published` or `missing`
//!   into `quarantined`, both enqueue a `delete`; read intact, a
//!   `missing` or `quarantined` acquisition is `published` again with a
//!   `reextract`. The bytes were just hashed, the same proof a
//!   republish gives. A file present again at write time means a
//!   republish landed since the read, and the absent or damaged verdict
//!   is dropped.
//! - Draft attachments and send attempts have no state column: their
//!   verdicts are counted. An unreferenced file that vanished since the
//!   listing was collected by GC.

use std::collections::{BTreeMap, BTreeSet};
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::index::IndexReason;
use crate::messages::{Streamed, drain_object};
use crate::objects::{FileObjectStore, ObjectId, ObjectStore};
use crate::schema::classify;
use crate::store::await_write;
use crate::writer::WritePriority;
use crate::{Store, StoreError};

/// Buckets in the rotation: one a week, every object yearly.
pub const SAMPLE_BUCKETS: u8 = 52;

/// The checkpoint's `store_meta` key (decimal week number).
const CHECKPOINT: &str = "verified_through_week";
const DAY_MS: i64 = 24 * 60 * 60 * 1000;
/// Acquisition verdicts recorded per writer task.
const FINDING_BATCH: usize = 64;
/// Failure details a report keeps; the count covers the rest.
const FAILURE_DETAILS_MAX: usize = 8;

/// What one run found.
#[derive(Clone, Debug, Default, PartialEq, Eq)]
pub struct SampleReport {
    /// The buckets the run covered, in the order it read them.
    pub buckets: Vec<u8>,
    /// Objects read to a verdict.
    pub checked: u64,
    pub intact: u64,
    /// Referenced objects that are absent.
    pub missing: u64,
    /// Damaged objects, now in `recovery/`.
    pub quarantined: u64,
    /// Acquisitions marked `published` again.
    pub healed: u64,
    /// Unreferenced objects collected since the listing.
    pub gone: u64,
    /// Objects that couldn't be read to a verdict (I/O).
    pub failed: u64,
    pub failure_details: Vec<String>,
    /// The run stopped when asked; the bucket it was in waits for the
    /// next run.
    pub stopped: bool,
}

/// The week number of `millis` (UTC, weeks start on Monday).
pub(crate) fn week_of(millis: i64) -> i64 {
    // 1970-01-01 was a Thursday; Monday is three days earlier.
    (millis.div_euclid(DAY_MS) + 3).div_euclid(7)
}

/// The bucket week `week` samples.
pub(crate) fn bucket_of_week(week: i64) -> u8 {
    week.rem_euclid(i64::from(SAMPLE_BUCKETS)) as u8
}

/// An object's bucket.
pub(crate) fn bucket_of(sha256: &[u8; 32]) -> u8 {
    let mut head = [0u8; 8];
    head.copy_from_slice(&sha256[..8]);
    (u64::from_be_bytes(head) % u64::from(SAMPLE_BUCKETS)) as u8
}

/// The weeks a run in `week` covers after the checkpoint: every week
/// since, at most the last 52; none when `week` is already done; just
/// `week` without a checkpoint or with the clock behind it.
pub(crate) fn weeks_to_cover(checkpoint: Option<i64>, week: i64) -> Vec<i64> {
    match checkpoint {
        Some(done) if done < week => {
            let first = (done + 1).max(week - i64::from(SAMPLE_BUCKETS) + 1);
            (first..=week).collect()
        }
        Some(done) if done == week => Vec::new(),
        _ => vec![week],
    }
}

/// How a sampled object is read and what its verdict touches.
#[derive(Clone, Debug)]
enum Target {
    /// Referenced by an acquisition, with its recorded length and state.
    Acquisition { id: ObjectId, state: String },
    /// Referenced only by a draft or a send attempt.
    Reference { sha: String },
    /// Listed on disk, referenced by nothing.
    Orphan { id: ObjectId },
}

enum Verdict {
    Intact,
    Absent,
    Damaged,
    Failed(StoreError),
    Stopped,
}

/// An acquisition verdict that may change its row.
#[derive(Clone, Copy, Debug)]
enum Finding {
    Absent(ObjectId),
    Damaged(ObjectId),
    Intact(ObjectId),
}

pub(crate) fn verify_sample(store: &Store, stop: &AtomicBool) -> Result<SampleReport, StoreError> {
    verify_sample_with(store, &|| stop.load(Ordering::Acquire))
}

/// [`verify_sample`] with the stop as a closure, polled before each
/// object and each read chunk: a test stops it at a chosen point.
pub(crate) fn verify_sample_with(
    store: &Store,
    stopped: &dyn Fn() -> bool,
) -> Result<SampleReport, StoreError> {
    if store.writer.is_writer_thread() {
        return Err(StoreError::internal(
            "the writer thread cannot verify through its own queue",
        ));
    }
    let week = week_of(store.clock.now().as_millis());
    let checkpoint = store.read(|tx| read_checkpoint(tx))?;
    let mut weeks = Vec::new();
    let mut seen = BTreeSet::new();
    for covered in weeks_to_cover(checkpoint, week) {
        if seen.insert(bucket_of_week(covered)) {
            weeks.push(covered);
        }
    }
    let mut report = SampleReport::default();
    if weeks.is_empty() {
        return Ok(report);
    }
    let mut targets = sample(store, &seen)?;

    for covered in weeks {
        let bucket = bucket_of_week(covered);
        let mut findings = Vec::new();
        for (_, target) in targets.remove(&bucket).unwrap_or_default() {
            let verdict = verify(&store.objects, &target, stopped);
            if matches!(verdict, Verdict::Stopped) {
                report.stopped = true;
                break;
            }
            tally(&mut report, &target, verdict, &mut findings);
            if findings.len() >= FINDING_BATCH {
                report.healed += record(store, std::mem::take(&mut findings))?;
            }
        }
        // What was read stands, stopped or not.
        report.healed += record(store, findings)?;
        if report.stopped {
            return Ok(report);
        }
        advance_checkpoint(store, covered)?;
        report.buckets.push(bucket);
    }
    Ok(report)
}

/// The run's sample by bucket, each in hash order: acquisitions first,
/// then the other references, then the files no row references.
fn sample(
    store: &Store,
    buckets: &BTreeSet<u8>,
) -> Result<BTreeMap<u8, BTreeMap<String, Target>>, StoreError> {
    let mut sample: BTreeMap<u8, BTreeMap<String, Target>> = BTreeMap::new();
    let wanted = |sha: &[u8; 32]| buckets.contains(&bucket_of(sha)).then(|| bucket_of(sha));
    store.read(|tx| {
        let what = "list the sampled references";
        let mut statement = tx
            .prepare("SELECT content_sha256, byte_length, object_state FROM acquisitions")
            .map_err(|e| classify(e, what))?;
        let mut rows = statement.query([]).map_err(|e| classify(e, what))?;
        while let Some(row) = rows.next().map_err(|e| classify(e, what))? {
            let sha: String = row.get(0).map_err(|e| classify(e, what))?;
            let length: i64 = row.get(1).map_err(|e| classify(e, what))?;
            let state: String = row.get(2).map_err(|e| classify(e, what))?;
            let length = u64::try_from(length)
                .map_err(|_| StoreError::corrupt("an acquisition's byte_length is negative"))?;
            let id = ObjectId::from_hex(&sha, length)?;
            if let Some(bucket) = wanted(&id.sha256()) {
                let target = Target::Acquisition { id, state };
                sample.entry(bucket).or_default().insert(sha, target);
            }
        }
        for sha in other_references(tx)? {
            let id = ObjectId::from_hex(&sha, 0)?;
            if let Some(bucket) = wanted(&id.sha256()) {
                let entries = sample.entry(bucket).or_default();
                entries
                    .entry(sha.clone())
                    .or_insert(Target::Reference { sha });
            }
        }
        Ok(())
    })?;
    for id in store.objects.list()? {
        let id = id?;
        if let Some(bucket) = wanted(&id.sha256()) {
            let entries = sample.entry(bucket).or_default();
            entries
                .entry(id.sha256_hex())
                .or_insert(Target::Orphan { id });
        }
    }
    Ok(sample)
}

/// Send attempts' MIME and every draft revision's attachments.
fn other_references(conn: &Connection) -> Result<BTreeSet<String>, StoreError> {
    let what = "list the sampled references";
    let mut shas = BTreeSet::new();
    let mut statement = conn
        .prepare("SELECT mime_sha256 FROM send_attempts")
        .map_err(|e| classify(e, what))?;
    let mut rows = statement.query([]).map_err(|e| classify(e, what))?;
    while let Some(row) = rows.next().map_err(|e| classify(e, what))? {
        shas.insert(row.get::<_, String>(0).map_err(|e| classify(e, what))?);
    }
    let mut statement = conn
        .prepare("SELECT attachments_json FROM draft_revisions")
        .map_err(|e| classify(e, what))?;
    let mut rows = statement.query([]).map_err(|e| classify(e, what))?;
    while let Some(row) = rows.next().map_err(|e| classify(e, what))? {
        let json: String = row.get(0).map_err(|e| classify(e, what))?;
        for attachment in crate::drafts::parse_attachments(&json)? {
            shas.insert(attachment.object_sha256);
        }
    }
    Ok(shas)
}

/// One object read to its verdict through the verifying reader.
fn verify(objects: &FileObjectStore, target: &Target, stopped: &dyn Fn() -> bool) -> Verdict {
    if stopped() {
        return Verdict::Stopped;
    }
    let opened = match target {
        Target::Acquisition { id, .. } | Target::Orphan { id } => objects.open(id),
        Target::Reference { sha } => objects.open_backup_reference(sha),
    };
    let read = opened.and_then(|mut reader| drain_object(&mut reader, None, stopped));
    match read {
        Ok(Streamed::Verified) => Verdict::Intact,
        Ok(Streamed::Cancelled) => Verdict::Stopped,
        Err(StoreError::NotFound { .. }) => Verdict::Absent,
        Err(StoreError::Corrupt { .. }) => Verdict::Damaged,
        Err(error) => Verdict::Failed(error),
    }
}

fn tally(
    report: &mut SampleReport,
    target: &Target,
    verdict: Verdict,
    findings: &mut Vec<Finding>,
) {
    match (verdict, target) {
        (Verdict::Stopped, _) => return,
        (Verdict::Failed(error), _) => {
            report.failed += 1;
            if report.failure_details.len() < FAILURE_DETAILS_MAX {
                report.failure_details.push(error.to_string());
            }
            return;
        }
        (Verdict::Absent, Target::Orphan { .. }) => report.gone += 1,
        // A quarantined acquisition's file is in `recovery/`.
        // The state read with the sample may be stale by now: `record`
        // decides against the current row. A quarantined acquisition's
        // file is in `recovery/`, so absent is what it reads as.
        (Verdict::Absent, Target::Acquisition { id, state }) => {
            if state == "quarantined" {
                report.quarantined += 1;
            } else {
                report.missing += 1;
            }
            findings.push(Finding::Absent(*id));
        }
        (Verdict::Absent, Target::Reference { .. }) => report.missing += 1,
        (Verdict::Damaged, Target::Acquisition { id, .. }) => {
            report.quarantined += 1;
            findings.push(Finding::Damaged(*id));
        }
        (Verdict::Damaged, _) => report.quarantined += 1,
        (Verdict::Intact, Target::Acquisition { id, state }) => {
            report.intact += 1;
            if state != "published" {
                findings.push(Finding::Intact(*id));
            }
        }
        (Verdict::Intact, _) => report.intact += 1,
    }
    report.checked += 1;
}

/// Records acquisition verdicts in one counted writer task; returns how
/// many rows went back to `published`.
fn record(store: &Store, findings: Vec<Finding>) -> Result<u64, StoreError> {
    if findings.is_empty() {
        return Ok(0);
    }
    let objects = Arc::clone(&store.objects);
    let now = store.clock.now();
    let future = store.writer.submit(WritePriority::Index, move |tx| {
        let mut healed = 0;
        let mut enqueued = false;
        for finding in &findings {
            let (id, to, from, reason): (&ObjectId, &str, &[&str], IndexReason) = match finding {
                Finding::Absent(id) => (id, "missing", &["published"], IndexReason::Delete),
                Finding::Damaged(id) => (
                    id,
                    "quarantined",
                    &["published", "missing"],
                    IndexReason::Delete,
                ),
                Finding::Intact(id) => (
                    id,
                    "published",
                    &["missing", "quarantined"],
                    IndexReason::Reextract,
                ),
            };
            // Serialized with the import commit that marks a republish
            // `published`: a file back at its path since the read is
            // verified bytes, not damage.
            if to != "published" && objects.exists(id)? {
                continue;
            }
            let Some((acquisition, state)) = acquisition_state(tx, &id.sha256_hex())? else {
                continue;
            };
            if !from.contains(&state.as_str()) {
                continue;
            }
            tx.execute(
                "UPDATE acquisitions SET object_state = ?2 WHERE acquisition_id = ?1",
                params![acquisition, to],
            )
            .map_err(|e| classify(e, "record a verification verdict"))?;
            // A missing object turning out damaged is already out of
            // the index.
            if state == "published" || to == "published" {
                crate::index::enqueue_one(tx, acquisition, reason, now)?;
                enqueued = true;
            }
            if to == "published" {
                healed += 1;
            }
        }
        if enqueued {
            crate::index::advance_generation(tx)?;
        }
        Ok(healed)
    });
    await_write(future)
}

fn acquisition_state(tx: &Transaction, sha: &str) -> Result<Option<(i64, String)>, StoreError> {
    tx.query_row(
        "SELECT acquisition_id, object_state FROM acquisitions WHERE content_sha256 = ?1",
        [sha],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
    .map_err(|e| classify(e, "read a sampled acquisition"))
}

/// The last week a run finished; absent before the first.
fn read_checkpoint(conn: &Connection) -> Result<Option<i64>, StoreError> {
    let value: Option<String> = conn
        .query_row(
            "SELECT value FROM store_meta WHERE key = ?1",
            [CHECKPOINT],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| classify(e, "read the verification checkpoint"))?;
    value
        .map(|text| {
            text.parse::<i64>()
                .map_err(|_| StoreError::corrupt(format!("store_meta.{CHECKPOINT} is malformed")))
        })
        .transpose()
}

/// Moves the checkpoint to `week` unless it is already there or past
/// it. The store's bookkeeping: it never makes a backup due.
fn advance_checkpoint(store: &Store, week: i64) -> Result<(), StoreError> {
    let future = store.writer.upkeep(WritePriority::Index, move |tx| {
        if read_checkpoint(tx)?.is_some_and(|done| done >= week) {
            return Ok(());
        }
        tx.execute(
            "INSERT INTO store_meta (key, value) VALUES (?1, ?2)
             ON CONFLICT(key) DO UPDATE SET value = excluded.value",
            params![CHECKPOINT, week.to_string()],
        )
        .map_err(|e| classify(e, "advance the verification checkpoint"))?;
        Ok(())
    });
    await_write(future)
}

#[cfg(test)]
mod tests;
