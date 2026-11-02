//! Garbage collection (spec/03 "Garbage collection"): the `gc_grace`
//! pin is the grace clock. A pass pins first-sight orphans, clears
//! them a reference re-covers, and removes expired orphans one writer
//! task each; the re-check deletes the pins, so a crash before the
//! unlink leaves no stale pin and the next pass restarts the clock.

use std::sync::Arc;
use std::time::Duration;

use rusqlite::{Transaction, TransactionBehavior, params};

use crate::objects::{ObjectId, ObjectStore};
use crate::schema::classify;
use crate::writer::WritePriority;
use crate::{Store, StoreError};

/// spec/03's default grace period; an orphan survives at least this
/// long after first sight before GC may remove it.
pub const GC_GRACE_PERIOD: Duration = Duration::from_secs(7 * 24 * 60 * 60);

/// The `object_pins.pin_kind` the grace clock owns.
const GRACE_KIND: &str = "gc_grace";

/// The `pin_ref` every grace pin carries.
const GRACE_REF: &str = "gc";

/// The pin edits one write transaction applies at most.
const GC_PIN_BATCH: usize = 200;

/// Why a whole GC pass froze before any write (spec/03).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GcGate {
    /// A `backup_generations` row is `in_progress`.
    BackupInProgress,
    /// Some account has an `open` `breaker_events` row (spec/04).
    BreakerOpen,
    /// An `acquisitions` row is not `published`: an object already
    /// proved absent or damaged questions the store's integrity.
    IntegrityDoubt,
}

/// What one GC pass did (spec/03). The counts are objects.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct GcReport {
    /// The whole-pass gate that froze the run; `None` when it ran.
    pub blocked: Option<GcGate>,
    /// Published objects the enumeration found.
    pub scanned: u64,
    /// Orphans that gained a `gc_grace` pin this pass.
    pub grace_started: u64,
    /// Re-referenced objects whose `gc_grace` pins this pass deleted.
    pub grace_cleared: u64,
    /// Objects left in place: referenced, grace pending, or pinned.
    pub kept: u64,
    /// Removal candidates the final re-check re-protected mid-pass.
    pub skipped: u64,
    /// Objects whose unlink failed; the next pass restarts their clocks.
    pub failed: u64,
    /// Each failed object's error, in the order the removals ran.
    pub failure_details: Vec<String>,
    /// Objects removed this pass.
    pub removed: u64,
}

/// One object's database state, read inside one transaction.
struct Seen {
    /// Any row references it.
    referenced: bool,
    /// Its oldest `gc_grace` pin's `created_at`, if it has one.
    grace_oldest: Option<i64>,
    /// Any pin of another kind holds it.
    other_pin: bool,
}

enum Action {
    /// First-sight orphan: start the grace clock.
    Pin,
    /// Re-referenced: delete the grace clock (spec/03).
    Unpin,
    /// Expired orphan clear of every gate: a removal task.
    Remove,
    /// Nothing to do.
    Keep,
}

/// One grace-clock write the pass applies.
#[derive(Clone)]
enum PinEdit {
    Start(String),
    Clear(String),
}

/// The removal task's outcome.
enum Outcome {
    Removed,
    /// The re-check refused the removal.
    Skipped,
    /// The task failed, its error kept for the report; the grace pins
    /// are gone, so the next pass restarts the clock.
    Failed(StoreError),
}

/// The three whole-pass freezes, checked in order (spec/03).
fn gate_blocked(tx: &Transaction) -> Result<Option<GcGate>, StoreError> {
    let (backup, breaker, doubt): (bool, bool, bool) = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM backup_generations WHERE state = 'in_progress'),
                    EXISTS(SELECT 1 FROM breaker_events WHERE state = 'open'),
                    EXISTS(SELECT 1 FROM acquisitions WHERE object_state <> 'published')",
            [],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|e| classify(e, "the GC gate check"))?;
    Ok(if backup {
        Some(GcGate::BackupInProgress)
    } else if breaker {
        Some(GcGate::BreakerOpen)
    } else if doubt {
        Some(GcGate::IntegrityDoubt)
    } else {
        None
    })
}

/// One object's references and pins (spec/03's three named tables).
fn object_seen(tx: &Transaction, sha: &str) -> Result<Seen, StoreError> {
    // min() over no rows is NULL: the oldest grace pin's sighting.
    let (referenced, grace_oldest, other_pin) = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM acquisitions WHERE content_sha256 = ?1)
                    OR EXISTS(SELECT 1 FROM send_attempts WHERE mime_sha256 = ?1)
                    OR EXISTS(SELECT 1 FROM draft_revisions
                              WHERE attachments_json LIKE '%' || ?1 || '%'),
                    (SELECT min(created_at) FROM object_pins
                     WHERE object_sha256 = ?1 AND pin_kind = ?2),
                    EXISTS(SELECT 1 FROM object_pins
                           WHERE object_sha256 = ?1 AND pin_kind <> ?2)",
            params![sha, GRACE_KIND],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(|e| classify(e, "the GC object check"))?;
    Ok(Seen {
        referenced,
        grace_oldest,
        other_pin,
    })
}

/// What one object earns this pass; a `None` cutoff expires nothing.
fn decide(seen: &Seen, cutoff: Option<i64>) -> Action {
    if seen.referenced {
        return match seen.grace_oldest {
            Some(_) => Action::Unpin,
            None => Action::Keep,
        };
    }
    match seen.grace_oldest {
        None => Action::Pin,
        Some(oldest) if !seen.other_pin && cutoff.is_some_and(|c| oldest < c) => Action::Remove,
        _ => Action::Keep,
    }
}

/// The removal task's re-check (spec/03); `true` refuses the unlink.
fn removal_blocked(tx: &Transaction, sha: &str, cutoff: Option<i64>) -> Result<bool, StoreError> {
    if gate_blocked(tx)?.is_some() {
        return Ok(true);
    }
    let seen = object_seen(tx, sha)?;
    Ok(seen.referenced
        || seen.other_pin
        || !matches!(seen.grace_oldest, Some(oldest) if cutoff.is_some_and(|c| oldest < c)))
}

/// Apply grace-pin writes in bounded transactions.
fn apply_pin_edits(store: &Store, edits: &[PinEdit], now: i64) -> Result<(), StoreError> {
    for chunk in edits.chunks(GC_PIN_BATCH) {
        let chunk = chunk.to_vec();
        let future =
            store
                .writer
                .submit(WritePriority::Index, move |tx| -> Result<(), StoreError> {
                    for edit in &chunk {
                        match edit {
                            PinEdit::Start(sha) => tx.execute(
                                "INSERT OR IGNORE INTO object_pins
                                 (object_sha256, pin_kind, pin_ref, created_at)
                                 VALUES (?1, ?2, ?3, ?4)",
                                params![sha, GRACE_KIND, GRACE_REF, now],
                            ),
                            PinEdit::Clear(sha) => tx.execute(
                                "DELETE FROM object_pins
                                 WHERE object_sha256 = ?1 AND pin_kind = ?2",
                                params![sha, GRACE_KIND],
                            ),
                        }
                        .map_err(|e| classify(e, "the GC grace-pin write"))?;
                    }
                    Ok(())
                });
        crate::store::await_write(future)?;
    }
    Ok(())
}

/// One object, one writer task (spec/03): re-check, grace-pin delete
/// and unlink stay serialized — a reservation lands before or after.
fn remove_object(store: &Store, id: &ObjectId, cutoff: Option<i64>) -> Outcome {
    let sha = id.sha256_hex();
    let shelf = Arc::clone(&store.objects().shelf);
    let path = store.objects().object_path(id);
    let id = *id;
    let future = store.writer.submit_task(
        WritePriority::Index,
        move |conn| -> Result<Outcome, StoreError> {
            let tx = conn
                .transaction_with_behavior(TransactionBehavior::Immediate)
                .map_err(|e| classify(e, "begin the GC re-check"))?;
            if removal_blocked(&tx, &sha, cutoff)? {
                return Ok(Outcome::Skipped);
            }
            // No stale grace pin may outlive this commit: if the
            // unlink never runs, the next pass restarts the clock.
            tx.execute(
                "DELETE FROM object_pins
                     WHERE object_sha256 = ?1 AND pin_kind = ?2",
                params![sha, GRACE_KIND],
            )
            .map_err(|e| classify(e, "release the GC grace pins"))?;
            tx.commit()
                .map_err(|e| classify(e, "commit the GC re-check"))?;
            // The republish-race proof parks here (spec/03): committed,
            // not yet unlinked. Two waits on one barrier: park, release.
            #[cfg(test)]
            {
                shelf.pause(|p| &p.before_unlink);
                shelf.pause(|p| &p.before_unlink);
            }
            shelf.remove(&id, &path)?;
            Ok(Outcome::Removed)
        },
    );
    match crate::store::await_write(future) {
        Ok(outcome) => outcome,
        Err(err) => Outcome::Failed(err),
    }
}

/// One bounded GC pass (spec/03): enumerate, read the gates and every
/// object in one transaction, write pins, then one task per removal.
pub(crate) fn collect_garbage(store: &Store, grace: Duration) -> Result<GcReport, StoreError> {
    // The pass awaits writer jobs; the writer thread can't await its own.
    if store.writer.is_writer_thread() {
        return Err(StoreError::internal(
            "the writer thread cannot run GC on its own queue",
        ));
    }
    let now = store.clock.now().as_millis();
    // Strictly older than grace; `checked_sub` keeps the epoch floor.
    let cutoff = now.checked_sub(i64::try_from(grace.as_millis()).unwrap_or(i64::MAX));

    let objects = store.objects();
    let ids = objects.list()?.collect::<Result<Vec<_>, _>>()?;
    let mut report = GcReport {
        scanned: ids.len() as u64,
        ..GcReport::default()
    };

    let (blocked, seen) = store.read(|tx| {
        // A frozen pass runs no per-object queries.
        if let Some(gate) = gate_blocked(tx)? {
            return Ok((Some(gate), Vec::new()));
        }
        let seen = ids
            .iter()
            .map(|id| object_seen(tx, &id.sha256_hex()))
            .collect::<Result<Vec<_>, _>>()?;
        Ok((None, seen))
    })?;
    if let Some(gate) = blocked {
        report.blocked = Some(gate);
        return Ok(report);
    }

    let mut edits = Vec::new();
    let mut removals = Vec::new();
    for (id, seen) in ids.iter().zip(&seen) {
        match decide(seen, cutoff) {
            Action::Pin => {
                report.grace_started += 1;
                edits.push(PinEdit::Start(id.sha256_hex()));
            }
            Action::Unpin => {
                report.grace_cleared += 1;
                edits.push(PinEdit::Clear(id.sha256_hex()));
            }
            Action::Remove => removals.push(*id),
            Action::Keep => report.kept += 1,
        }
    }
    apply_pin_edits(store, &edits, now)?;

    for id in &removals {
        match remove_object(store, id, cutoff) {
            Outcome::Removed => report.removed += 1,
            Outcome::Skipped => report.skipped += 1,
            Outcome::Failed(err) => {
                report.failed += 1;
                report.failure_details.push(err.to_string());
            }
        }
    }
    Ok(report)
}

#[cfg(test)]
mod tests;
