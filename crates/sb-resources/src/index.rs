//! Index preparation: the extraction facade (spec/06 "Preparation
//! boundary"). One short store read captures pending tokens and the
//! fence; another gates on the current target, epoch, both pauses and
//! the rebuild phase and resolves acquisition metadata; object reads
//! and MIME extraction then run outside SQLite. Preparation writes no
//! SQLite state and acknowledges no queue entry; the engine owns
//! submission, scheduling and pause policy.

use sb_mime::{Limits, SEARCH_EXTRACTOR_VERSION, SearchDocument, SearchState, search_document};
use sb_store::index::rebuild::{RebuildPhase, latest_rebuild, search_index_status};
use sb_store::index::{
    IndexBatch, IndexDocument, IndexFence, IndexPublication, IndexReason, IndexState,
    MAX_INDEX_BATCH, MAX_INDEX_BATCH_BYTES, PendingDocument, pending_batch, target_version,
};
use sb_store::messages::{AcquisitionRead, StoredAcquisition, index_acquisition};
use sb_store::{Store, StoreError, Transaction};

use crate::Resources;

/// What preparing one captured batch yielded. Non-`Prepared` outcomes
/// are explicit: the queue is neither drained nor acknowledged.
#[derive(Debug)]
pub enum IndexPreparation {
    /// Publications ready for one bounded `WritePriority::Index`
    /// submission under the captured fence, plus the first contextual
    /// failure when one stopped preparation.
    Prepared(PreparedIndexBatch),
    /// The stored target exceeds this build's extractor (spec/07
    /// `BlockedNewerExtractor`): never an I/O pause, never
    /// `NewerStore`. Work stays queued for a newer extractor.
    BlockedNewerExtractor { target: u32 },
    /// A durable pause or an unfinished reset/enqueue phase: work stays
    /// queued until the recorded state changes.
    Blocked(IndexBlocked),
    /// The captured epoch or target no longer matches the store;
    /// recapture before preparing again.
    Stale,
    /// Cancelled before I/O or at a chunk/candidate boundary; the
    /// unsubmitted preparation is discarded.
    Cancelled,
}

/// Why preparation is `Blocked`, from the current status/phase records.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexBlocked {
    /// spec/07 `PausedByUser`; precedence over the I/O pause.
    UserPaused,
    /// spec/07 `PausedIo`; the durable record holds cause and deadline.
    IoPaused,
    /// The latest rebuild is still in Reset or Enqueue.
    Rebuilding(RebuildPhase),
}

/// The valid prefix prepared under one fence: at most 200
/// publications and 2,000,000 retained column bytes (spec/03, 06),
/// plus the first lookup/read failure with its captured token.
#[derive(Debug)]
pub struct PreparedIndexBatch {
    fence: IndexFence,
    publications: Vec<IndexPublication>,
    failure: Option<IndexReadFailure>,
}

impl PreparedIndexBatch {
    /// The fence the batch was prepared under; publication rechecks it.
    pub fn fence(&self) -> IndexFence {
        self.fence
    }

    /// Publications in queue order; `None` documents are deletions.
    pub fn publications(&self) -> &[IndexPublication] {
        &self.publications
    }

    /// The first candidate failure, with its token and stage. The
    /// caller rechecks the token's staleness before acting on it.
    pub fn failure(&self) -> Option<&IndexReadFailure> {
        self.failure.as_ref()
    }

    /// Move publications into the writer without copying their text, retaining
    /// the failure separately for the caller's subsequent policy.
    pub fn into_parts(self) -> (IndexFence, Vec<IndexPublication>, Option<IndexReadFailure>) {
        (self.fence, self.publications, self.failure)
    }
}

/// One candidate's failure under its captured queue token, with the
/// faithful store error (`io_kind` included), so a stale failure can
/// be discarded before any pause is recorded.
#[derive(Debug, Clone)]
pub struct IndexReadFailure {
    pending: PendingDocument,
    stage: IndexReadStage,
    error: StoreError,
}

impl IndexReadFailure {
    /// The queue token the failing candidate was captured under.
    pub fn pending(&self) -> &PendingDocument {
        &self.pending
    }

    /// Which half of preparation failed.
    pub fn stage(&self) -> IndexReadStage {
        self.stage
    }

    /// The failure exactly as the store reported it.
    pub fn error(&self) -> &StoreError {
        &self.error
    }

    /// Consume the context without cloning its token or error.
    pub fn into_parts(self) -> (PendingDocument, IndexReadStage, StoreError) {
        (self.pending, self.stage, self.error)
    }
}

/// Which half of preparation failed. `NotFound` at `Lookup` names an
/// absent acquisition row (a contextual invariant failure); `NotFound`
/// at `Read` names an absent object file.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexReadStage {
    Lookup,
    Read,
}

impl Resources {
    /// One short read: the fence plus the oldest pending documents
    /// (spec/06: the queue's durable watermark). The raw pending view
    /// stays readable while preparation would be blocked.
    pub fn pending_index_batch(&self) -> Result<IndexBatch, StoreError> {
        self.read(|tx| pending_batch(tx, MAX_INDEX_BATCH))
    }

    /// Resolve, read, extract and map the captured batch into one
    /// bounded publication batch under spec/10's default MIME limits.
    /// Object reads and parsing happen outside SQLite; the first
    /// lookup/read failure ends the batch after the valid prefix.
    pub fn prepare_index_batch(
        &self,
        batch: IndexBatch,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<IndexPreparation, StoreError> {
        prepare(&self.store, batch, &Limits::default(), cancelled)
    }
}

/// The gating snapshot's verdict, computed inside one short read.
enum Gate {
    /// Metadata for each captured candidate, aligned with
    /// `IndexBatch::documents`; `None` where a delete needs none.
    Open(Vec<Option<Result<StoredAcquisition, StoreError>>>),
    NewerExtractor {
        target: u32,
    },
    Blocked(IndexBlocked),
    Stale,
}

/// Capability first: a newer target must never be extracted or
/// mistaken for an I/O pause. Then check the durable pauses and the
/// reset/enqueue phase, then capture freshness, and only then
/// acquisition metadata for non-delete candidates.
fn gate(tx: &Transaction, batch: &IndexBatch) -> Result<Gate, StoreError> {
    let target = target_version(tx)?;
    if target > SEARCH_EXTRACTOR_VERSION {
        return Ok(Gate::NewerExtractor { target });
    }
    let status = search_index_status(tx)?;
    if status.user_paused() {
        return Ok(Gate::Blocked(IndexBlocked::UserPaused));
    }
    if status.io_pause().is_some() {
        return Ok(Gate::Blocked(IndexBlocked::IoPaused));
    }
    if let Some(job) = latest_rebuild(tx)?
        && matches!(job.phase(), RebuildPhase::Reset | RebuildPhase::Enqueue)
    {
        return Ok(Gate::Blocked(IndexBlocked::Rebuilding(job.phase())));
    }
    if status.epoch() != batch.fence.epoch || target != batch.fence.version {
        return Ok(Gate::Stale);
    }
    let lookups = batch
        .documents
        .iter()
        .map(|pending| {
            (pending.reason != IndexReason::Delete).then(|| index_acquisition(tx, pending.doc_id))
        })
        .collect();
    Ok(Gate::Open(lookups))
}

/// The shared preparation body; tests reach it on an injected
/// filesystem and with a narrowed MIME budget.
fn prepare(
    store: &Store,
    batch: IndexBatch,
    limits: &Limits,
    cancelled: &dyn Fn() -> bool,
) -> Result<IndexPreparation, StoreError> {
    if batch.documents.len() > MAX_INDEX_BATCH {
        return Err(StoreError::InvalidInput {
            message: "index capture exceeds the document budget".into(),
        });
    }
    if cancelled() {
        return Ok(IndexPreparation::Cancelled);
    }
    let lookups = match store.read(|tx| gate(tx, &batch))? {
        Gate::NewerExtractor { target } => {
            return Ok(IndexPreparation::BlockedNewerExtractor { target });
        }
        Gate::Blocked(blocked) => return Ok(IndexPreparation::Blocked(blocked)),
        Gate::Stale => return Ok(IndexPreparation::Stale),
        Gate::Open(lookups) => lookups,
    };
    // The read transaction is released: object reads and MIME work run
    // outside SQLite.
    let retain = limits.max_message_bytes() as u64;
    let mut publications = Vec::new();
    let mut retained = 0usize;
    let mut failure = None;
    for (pending, lookup) in batch.documents.iter().zip(lookups) {
        if cancelled() {
            return Ok(IndexPreparation::Cancelled);
        }
        // Delete requests need no metadata lookup or body I/O.
        let Some(lookup) = lookup else {
            publications.push(IndexPublication {
                pending: pending.clone(),
                document: None,
            });
            continue;
        };
        let acquisition = match lookup {
            Ok(acquisition) => acquisition,
            Err(error) => {
                failure = Some(IndexReadFailure {
                    pending: pending.clone(),
                    stage: IndexReadStage::Lookup,
                    error,
                });
                break;
            }
        };
        let read = store.read_acquisition_bounded(&acquisition, retain, cancelled);
        if cancelled() {
            return Ok(IndexPreparation::Cancelled);
        }
        let document = match read {
            Ok(AcquisitionRead::Verified(bytes)) => search_document(&bytes, limits),
            // EOF-verified but over the MIME cap: bounded size refusal.
            Ok(AcquisitionRead::Oversized) => SearchDocument::size_refused(),
            Ok(AcquisitionRead::Cancelled) => return Ok(IndexPreparation::Cancelled),
            Err(error) => {
                failure = Some(IndexReadFailure {
                    pending: pending.clone(),
                    stage: IndexReadStage::Read,
                    error,
                });
                break;
            }
        };
        if cancelled() {
            return Ok(IndexPreparation::Cancelled);
        }
        let document = map_document(&document);
        // Conservative stop: leave this and all later candidates queued.
        let bytes = retained.saturating_add(document.column_bytes());
        if bytes > MAX_INDEX_BATCH_BYTES {
            break;
        }
        retained = bytes;
        publications.push(IndexPublication {
            pending: pending.clone(),
            document: Some(document),
        });
    }
    if cancelled() {
        return Ok(IndexPreparation::Cancelled);
    }
    Ok(IndexPreparation::Prepared(PreparedIndexBatch {
        fence: batch.fence,
        publications,
        failure,
    }))
}

/// The storage row for one extracted document: all eight columns in
/// schema order, the state and mask, and the extractor version.
fn map_document(document: &SearchDocument) -> IndexDocument {
    let state = match document.state() {
        SearchState::Indexed => IndexState::Indexed,
        SearchState::Partial => IndexState::Partial,
        SearchState::Failed => IndexState::Failed,
    };
    IndexDocument {
        extractor_version: document.extractor_version(),
        state,
        truncated_fields: document
            .truncated_fields()
            .iter()
            .map(|name| (*name).to_owned())
            .collect(),
        columns: [
            document.subject().to_owned(),
            document.from_text().to_owned(),
            document.to_text().to_owned(),
            document.cc_text().to_owned(),
            document.body_authored().to_owned(),
            document.body_quoted().to_owned(),
            document.body_signature().to_owned(),
            document.attachment_names().to_owned(),
        ],
    }
}

#[cfg(test)]
mod tests;
