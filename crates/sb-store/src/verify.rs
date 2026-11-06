//! Weekly object verification and `integrity_check` (spec/03 "Reads
//! and verification" and "Integrity"): a rotating sample of objects is
//! re-hashed each week — 1/52 of them, so every object sees a full pass
//! within a year — and every object a running backup pins joins the
//! pass. A healthy object leaves its `acquisitions` row `published`; a
//! damaged file is quarantined by the verifying reader and its row
//! becomes `quarantined`, and an absent file marks the row `missing`
//! (GC's integrity doubt check then stops removals until the state is
//! repaired or the reference is gone). `integrity_check` runs the
//! SQLite check on a reader connection and reports its finding rows.
//!
//! All SQL runs in bounded keyset batches and every object reads as a
//! stream, so a pass never holds a whole acquisition table or object in
//! memory.

use std::io;

use rusqlite::params;

use crate::objects::{ObjectId, ObjectStore, read_error_as_store_error};
use crate::schema::classify;
use crate::store::{Store, await_write};
use crate::{StoreError, WritePriority};

/// 1/52 of all objects per weekly pass (spec/03): a year of passes
/// covers every object.
const ROTATION_BUCKETS: u64 = 52;

/// Acquisitions paged per read transaction, and outcome writes
/// committed per write transaction.
const CANDIDATE_BATCH: i64 = 256;
const OUTCOME_BATCH: usize = 64;

/// What one verification pass did, in `acquisitions` terms (spec/03):
/// the rotating sample size, the backup-referenced rows joined in, the
/// objects whose bytes verified, and the rows the pass degraded.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct VerifyReport {
    /// `acquisitions` rows the rotation selected for this pass.
    pub sampled: u64,
    /// Rows joined in because a running backup pins the object.
    pub backup_referenced: u64,
    /// Objects whose bytes read fully and hashed to their id.
    pub verified: u64,
    /// Rows moved to `missing` because the object file is absent.
    pub missing: u64,
    /// Rows moved to `quarantined` because the object failed
    /// verification and was moved to `recovery/`.
    pub quarantined: u64,
}

/// What draining one object proved.
enum DrainOutcome {
    /// The bytes read fully and hashed to the id.
    Verified,
    /// The object file is absent.
    Missing,
    /// The object failed verification; the reader moved it to
    /// `recovery/` before reporting.
    Quarantined,
}

/// Re-hash this week's rotating sample plus every object a running
/// backup pins (spec/03). `rotation` is the pass index; the pass takes
/// the `rotation % 52` bucket so it wraps yearly.
pub fn verify_objects(store: &Store, rotation: u64) -> Result<VerifyReport, StoreError> {
    let bucket = rotation % ROTATION_BUCKETS;
    let mut report = VerifyReport::default();
    let mut after: i64 = 0;
    let mut pending: Vec<(String, &'static str)> = Vec::new();

    loop {
        let candidates = store.read(|tx| {
            let mut stmt = tx
                .prepare(
                    "SELECT a.acquisition_id, a.content_sha256, a.byte_length,
                            (SELECT 1 FROM object_pins p
                              WHERE p.object_sha256 = a.content_sha256
                                AND p.pin_kind = 'backup_in_progress'
                              LIMIT 1) IS NOT NULL AS pinned
                     FROM acquisitions a
                     WHERE a.object_state = 'published' AND a.acquisition_id > ?1
                     ORDER BY a.acquisition_id
                     LIMIT ?2",
                )
                .map_err(|e| classify(e, "list verification candidates"))?;
            stmt.query_map(params![after, CANDIDATE_BATCH], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, bool>(3)?,
                ))
            })
            .map_err(|e| classify(e, "read verification candidates"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| classify(e, "read verification candidates"))
        })?;

        if candidates.is_empty() {
            break;
        }
        after = candidates[candidates.len() - 1].0;

        for (_id, sha, byte_length, pinned) in candidates {
            if byte_length < 0 {
                return Err(StoreError::corrupt(format!(
                    "an acquisition of {sha} has byte_length {byte_length}"
                )));
            }
            let backup_referenced = pinned;
            let sampled = slot(&sha) == bucket;
            if !backup_referenced && !sampled {
                continue;
            }
            if backup_referenced {
                report.backup_referenced += 1;
            } else {
                report.sampled += 1;
            }
            let id = ObjectId::from_hex(&sha, byte_length as u64)?;
            match drain(store, &id)? {
                DrainOutcome::Verified => report.verified += 1,
                DrainOutcome::Missing => {
                    report.missing += 1;
                    pending.push((sha, "missing"));
                }
                DrainOutcome::Quarantined => {
                    report.quarantined += 1;
                    pending.push((sha, "quarantined"));
                }
            }
            if pending.len() >= OUTCOME_BATCH {
                flush_outcomes(store, &mut pending)?;
            }
        }
    }
    flush_outcomes(store, &mut pending)?;
    Ok(report)
}

/// Run SQLite's `PRAGMA integrity_check` on a reader connection and
/// return its finding rows — `["ok"]` for a clean store, otherwise the
/// corruptions it found (spec/03 "Integrity").
pub fn integrity_check(store: &Store) -> Result<Vec<String>, StoreError> {
    store.read(|tx| {
        tx.prepare("PRAGMA integrity_check")
            .map_err(|e| classify(e, "prepare integrity_check"))?
            .query_map([], |row| row.get::<_, String>(0))
            .map_err(|e| classify(e, "run integrity_check"))?
            .collect::<Result<Vec<_>, _>>()
            .map_err(|e| classify(e, "read integrity_check"))
    })
}

/// The rotation slot an object hex lands in: its first byte mod 52.
/// A digest too short or non-hex to decode a byte lands in slot 0 until
/// `ObjectId::from_hex` rejects it as `Corrupt`.
fn slot(sha256_hex: &str) -> u64 {
    sha256_hex
        .get(..2)
        .and_then(|prefix| u8::from_str_radix(prefix, 16).ok())
        .map(u64::from)
        .unwrap_or(0)
        % ROTATION_BUCKETS
}

/// Read one object to end of file, letting `ObjectReader` verify and
/// quarantine (spec/03): `NotFound` maps to `missing`, a read `Corrupt`
/// to `quarantined`; any other typed store error and unclassified I/O
/// propagate so a transient failure aborts the pass retryably.
fn drain(store: &Store, id: &ObjectId) -> Result<DrainOutcome, StoreError> {
    let mut reader = match store.objects().open(id) {
        Ok(reader) => reader,
        Err(StoreError::NotFound { .. }) => return Ok(DrainOutcome::Missing),
        Err(err) => return Err(err),
    };
    match io::copy(&mut reader, &mut io::sink()) {
        Ok(_) => Ok(DrainOutcome::Verified),
        Err(err) => match read_error_as_store_error(&err) {
            Some(StoreError::NotFound { .. }) => Ok(DrainOutcome::Missing),
            Some(StoreError::Corrupt { .. }) => Ok(DrainOutcome::Quarantined),
            Some(other) => Err(other.clone()),
            None => Err(StoreError::unavailable_io(
                format!("cannot verify object {}: {err}", id.sha256_hex()),
                err.kind(),
            )),
        },
    }
}

/// Commit the pass's degradations in bounded write transactions; each
/// row only leaves `published`, so a concurrent republish is never
/// overwritten (spec/03).
fn flush_outcomes(
    store: &Store,
    pending: &mut Vec<(String, &'static str)>,
) -> Result<(), StoreError> {
    if pending.is_empty() {
        return Ok(());
    }
    let batch = std::mem::take(pending);
    await_write(store.write(WritePriority::Index, move |tx| {
        for (sha, state) in &batch {
            tx.execute(
                "UPDATE acquisitions SET object_state = ?2
                 WHERE content_sha256 = ?1 AND object_state = 'published'",
                params![sha, state],
            )
            .map_err(|e| classify(e, "mark a degraded object"))?;
        }
        Ok(())
    }))
}

#[cfg(test)]
mod tests;
