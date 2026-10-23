//! Finite captured-revision passes over the qualified message activator.

use super::*;
use crate::corpus_tracking::{self, counter, set};
use std::collections::VecDeque;

const MAX_MESSAGES: usize = 200;
const RETAINED_QUANTUM: usize = 4_000_000;
#[cfg(test)]
mod tests;

#[derive(Debug)]
enum Entry {
    Current(i64),
    Unready(i64, i64),
    Work(ActivationWork),
    Prepared(ActivationWork),
}

impl Entry {
    fn id(&self) -> i64 {
        match self {
            Self::Current(id) | Self::Unready(id, _) => *id,
            Self::Work(w) | Self::Prepared(w) => w.identity.message,
        }
    }
    fn bytes(&self) -> usize {
        match self {
            Self::Work(w) | Self::Prepared(w) => retained(w),
            _ => METADATA_FRAME,
        }
    }
    fn estimate(&self) -> usize {
        match self {
            Self::Prepared(w) => {
                if w.cost > MAX_BYTES {
                    MAX_EVIDENCE_BYTES
                } else {
                    w.cost
                }
            }
            _ => 0,
        }
    }
}

#[derive(Debug)]
pub struct CatchupSweep {
    owner: Arc<inputs::State>,
    revision: u64,
    target: u32,
    cutoff: i64,
    after: i64,
    deferred: u64,
    unready: u64,
    pending: VecDeque<Entry>,
    ready: Vec<Entry>,
    eof: bool,
}

#[derive(Debug)]
pub struct CatchupBatch(CatchupSweep);

#[derive(Debug)]
pub enum CatchupPreparation {
    Continue(CatchupSweep),
    Prepared(CatchupBatch),
    Recapture,
    Cancelled,
}

#[derive(Debug)]
pub enum CatchupCommit {
    Continue(CatchupSweep, CatchupReport),
    Complete(CatchupReport),
    Recapture,
}

#[derive(Debug, Default)]
pub struct CatchupReport {
    pub processed: u32,
    pub activated: u32,
    pub deferred: u32,
    pub unready: u32,
    pub charged_bytes: u64,
    pub repair_needed: Vec<i64>,
}

#[derive(Debug)]
pub struct CatchupStatus {
    pub target: u32,
    pub inputs: u64,
    pub swept: u64,
    pub deferred: u64,
    pub unready: u64,
}

fn status(conn: &Connection, build: u32) -> Result<CatchupStatus, StoreError> {
    let version: u32 = conn
        .pragma_query_value(None, "user_version", |row| row.get(0))
        .map_err(sql_error)?;
    if version < corpus_tracking::SCHEMA_VERSION {
        return Err(StoreError::unavailable(
            "repair catch-up requires schema 12",
        ));
    }
    let target = repair::target(conn)?;
    repair::supported(target, build)?;
    let value = CatchupStatus {
        target,
        inputs: counter(conn, "repair_inputs")?,
        swept: counter(conn, "repair_swept")?,
        deferred: counter(conn, "repair_deferred")?,
        unready: counter(conn, "repair_unready")?,
    };
    if value.swept > value.inputs {
        return Err(StoreError::corrupt("repair sweep exceeds inputs"));
    }
    Ok(value)
}

fn fill(tx: &Transaction, scan: &mut CatchupSweep) -> Result<(), StoreError> {
    if !scan.pending.is_empty() || scan.eof {
        return Ok(());
    }
    let ids = tx.prepare_cached("SELECT message_id FROM logical_messages WHERE message_id>?1 AND message_id<=?2 ORDER BY message_id LIMIT ?3").map_err(sql_error)?
        .query_map(params![scan.after, scan.cutoff, MAX_MESSAGES as i64], |row| row.get(0)).map_err(sql_error)?
        .collect::<rusqlite::Result<Vec<i64>>>().map_err(sql_error)?;
    scan.eof = ids.is_empty();
    let mut bytes = 0usize;
    for id in ids {
        let entry = match capture(tx, scan.owner.clone(), id)? {
            Capture::Ready(work) => Entry::Work(*work),
            Capture::Current => Entry::Current(id),
            Capture::Unready(acquisition) | Capture::Unavailable(acquisition, _) => {
                Entry::Unready(id, acquisition)
            }
        };
        bytes = bytes.saturating_add(entry.bytes());
        scan.pending.push_back(entry);
        if bytes >= RETAINED_QUANTUM {
            break;
        }
    }
    Ok(())
}

impl Store {
    pub fn begin_catchup(&self, build: u32) -> WriteFuture<Option<CatchupSweep>> {
        let owner = self.activation.handle();
        self.write(WritePriority::Index, move |tx| {
            inputs::healthy(&owner)?;
            status(tx, build)?;
            repair::reconcile_target(tx, build)?;
            // Capture the revision after this job's own target reconciliation.
            corpus_tracking::flush(tx)?;
            let current = status(tx, build)?;
            if current.inputs == current.swept {
                return Ok(None);
            }
            let cutoff = tx
                .query_row(
                    "SELECT coalesce(max(message_id),0) FROM logical_messages",
                    [],
                    |row| row.get(0),
                )
                .map_err(sql_error)?;
            let mut scan = CatchupSweep {
                owner,
                revision: current.inputs,
                target: current.target,
                cutoff,
                after: 0,
                deferred: 0,
                unready: 0,
                pending: VecDeque::new(),
                ready: Vec::new(),
                eof: false,
            };
            fill(tx, &mut scan)?;
            Ok(Some(scan))
        })
    }

    pub fn prepare_catchup_step(
        &self,
        mut scan: CatchupSweep,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<CatchupPreparation, StoreError> {
        let owner = self.activation.handle();
        if !inputs::same_owner(&scan.owner, &owner) {
            return Ok(CatchupPreparation::Recapture);
        }
        inputs::healthy(&owner)?;
        if cancelled() {
            return Ok(CatchupPreparation::Cancelled);
        }
        let Some(entry) = scan.pending.pop_front() else {
            return Ok(CatchupPreparation::Prepared(CatchupBatch(scan)));
        };
        let id = entry.id();
        let entry = match entry {
            Entry::Work(work) => match self.prepare_activation(work, cancelled)? {
                ActivationPreparation::Continue(work) => {
                    scan.pending.push_front(Entry::Work(work));
                    return Ok(CatchupPreparation::Continue(scan));
                }
                ActivationPreparation::Prepared(work) => Entry::Prepared(work.0),
                ActivationPreparation::Current => Entry::Current(id),
                ActivationPreparation::Recapture => return Ok(CatchupPreparation::Recapture),
                ActivationPreparation::Cancelled => return Ok(CatchupPreparation::Cancelled),
            },
            other => other,
        };
        let estimated: usize = scan.ready.iter().map(Entry::estimate).sum();
        if !scan.ready.is_empty() && estimated.saturating_add(entry.estimate()) > MAX_BYTES {
            scan.pending.push_front(entry);
            return Ok(CatchupPreparation::Prepared(CatchupBatch(scan)));
        }
        scan.ready.push(entry);
        let bytes: usize = scan.ready.iter().map(Entry::bytes).sum();
        if scan.ready.len() == MAX_MESSAGES || bytes >= RETAINED_QUANTUM || scan.pending.is_empty()
        {
            return Ok(CatchupPreparation::Prepared(CatchupBatch(scan)));
        }
        Ok(CatchupPreparation::Continue(scan))
    }

    pub fn commit_catchup_batch(
        &self,
        batch: CatchupBatch,
        build: u32,
        now: Timestamp,
    ) -> WriteFuture<CatchupCommit> {
        let owner = self.activation.handle();
        self.write_activation(move |tx| commit(tx, batch.0, &owner, build, now))
    }

    pub fn catchup_status(&self, build: u32) -> Result<CatchupStatus, StoreError> {
        inputs::healthy(&self.activation.handle())?;
        self.read(|tx| status(tx, build))
    }
}

fn commit(
    tx: &Transaction,
    mut scan: CatchupSweep,
    owner: &Arc<inputs::State>,
    build: u32,
    now: Timestamp,
) -> Result<CatchupCommit, StoreError> {
    if !inputs::same_owner(&scan.owner, owner) {
        return Ok(CatchupCommit::Recapture);
    }
    inputs::healthy(owner)?;
    let current = status(tx, build)?;
    if current.target != scan.target {
        return Ok(CatchupCommit::Recapture);
    }
    if scan.revision > current.inputs {
        return Err(StoreError::corrupt("catch-up acknowledges future input"));
    }
    let mut verified = VecDeque::new();
    for entry in &scan.ready {
        if let Entry::Prepared(work) = entry {
            let value = preflight(tx, work, owner)?;
            if matches!(value, Err(ActivationCommit::Recapture)) {
                return Ok(CatchupCommit::Recapture);
            }
            verified.push_back(value);
        }
    }
    let mut remaining = MAX_BYTES;
    let mut changes = Changes::default();
    let mut report = CatchupReport::default();
    let mut ready = std::mem::take(&mut scan.ready).into_iter();
    while let Some(entry) = ready.next() {
        let id = entry.id();
        match entry {
            Entry::Prepared(work) => match apply_verified(
                tx,
                work,
                owner,
                now,
                &mut remaining,
                verified
                    .pop_front()
                    .expect("each ready item was preflighted"),
            )? {
                Applied::Carry(work) => {
                    let mut pending = VecDeque::from([Entry::Prepared(*work)]);
                    pending.extend(ready);
                    pending.append(&mut scan.pending);
                    scan.pending = pending;
                    break;
                }
                Applied::Done {
                    receipt,
                    charged,
                    changes: delta,
                } => {
                    match receipt {
                        ActivationCommit::Activated => report.activated += 1,
                        ActivationCommit::Deferred => {
                            report.deferred += 1;
                            scan.deferred += 1;
                        }
                        ActivationCommit::Current => {}
                        ActivationCommit::Recapture => {
                            return Err(StoreError::internal(
                                "source changed inside an activation batch",
                            ));
                        }
                    }
                    report.charged_bytes += charged as u64;
                    changes.merge(delta);
                }
            },
            Entry::Unready(_, acquisition) => {
                report.unready += 1;
                scan.unready += 1;
                report.repair_needed.push(acquisition);
            }
            Entry::Current(_) => {}
            Entry::Work(_) => {
                return Err(StoreError::internal(
                    "unprepared corpus work reached writer",
                ));
            }
        }
        scan.after = id;
        report.processed += 1;
    }
    changes.publish(tx)?;
    report.repair_needed.sort_unstable();
    report.repair_needed.dedup();
    fill(tx, &mut scan)?;
    if !scan.eof || !scan.pending.is_empty() {
        return Ok(CatchupCommit::Continue(scan, report));
    }
    if scan.revision > current.swept {
        if remaining < METADATA_FRAME {
            return Ok(CatchupCommit::Continue(scan, report));
        }
        set(tx, "repair_swept", scan.revision)?;
        set(tx, "repair_deferred", scan.deferred)?;
        set(tx, "repair_unready", scan.unready)?;
        report.charged_bytes += METADATA_FRAME as u64;
    }
    Ok(CatchupCommit::Complete(report))
}
