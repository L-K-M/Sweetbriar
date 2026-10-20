//! Durable search queue and atomic FTS5 publication (spec/03, spec/06).
//! Call mutation helpers once per writer transaction and propagate any error
//! out of the closure so the whole transaction rolls back. Submit index writes
//! at `WritePriority::Index`; extraction and acquisition reads happen beforehand.
//! [`rebuild`] owns the durable job/worker records; publication rechecks
//! the captured [`IndexFence`] and pauses before any effect.

use std::collections::BTreeSet;

use rusqlite::{Connection, OptionalExtension, Transaction, params};

use crate::{StoreError, Timestamp, schema::classify};

pub mod rebuild;

pub const MAX_INDEX_BATCH: usize = 200;
pub const MAX_INDEX_BATCH_BYTES: usize = 2_000_000;
const MAX_DOCUMENT_BYTES: usize = 1_000_000;
const MAX_ATTACHMENT_NAMES_BYTES: usize = 64_000;
const FIELDS: [&str; 8] = [
    "subject",
    "from_text",
    "to_text",
    "cc_text",
    "body_authored",
    "body_quoted",
    "body_signature",
    "attachment_names",
];

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexReason {
    New,
    Reextract,
    Delete,
}

impl IndexReason {
    fn as_str(self) -> &'static str {
        match self {
            Self::New => "new",
            Self::Reextract => "reextract",
            Self::Delete => "delete",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingDocument {
    pub doc_id: i64,
    pub revision: i64,
    pub reason: IndexReason,
    pub enqueued_at: Timestamp,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum IndexState {
    Indexed,
    Partial,
    Failed,
}

impl IndexState {
    fn as_str(self) -> &'static str {
        match self {
            Self::Indexed => "indexed",
            Self::Partial => "partial",
            Self::Failed => "failed",
        }
    }
}

/// Storage input mapped from sb-mime by resources; leaves cannot depend on
/// each other. Columns follow spec/06's order, from subject to attachment_names.
#[derive(Debug, Clone)]
pub struct IndexDocument {
    pub extractor_version: u32,
    pub state: IndexState,
    pub truncated_fields: Vec<String>,
    pub columns: [String; 8],
}

impl IndexDocument {
    pub fn column_bytes(&self) -> usize {
        self.columns
            .iter()
            .fold(0usize, |n, value| n.saturating_add(value.len()))
    }

    fn validate(&self, version: u32) -> Result<(), StoreError> {
        let mask_valid = self
            .truncated_fields
            .iter()
            .all(|name| FIELDS.contains(&name.as_str()))
            && self
                .truncated_fields
                .windows(2)
                .all(|pair| pair[0] < pair[1]);
        if self.extractor_version != version
            || !mask_valid
            || self.column_bytes() > MAX_DOCUMENT_BYTES
            || self.columns[7].len() > MAX_ATTACHMENT_NAMES_BYTES
            || (self.state == IndexState::Partial) == self.truncated_fields.is_empty()
            || (self.state == IndexState::Failed && self.column_bytes() != 0)
        {
            return Err(invalid("invalid extracted search document"));
        }
        Ok(())
    }
}

/// `None` is a deletion and is valid only for a Delete request. A permanent
/// parse failure is an explicit Failed document, not a deletion or I/O error.
#[derive(Debug, Clone)]
pub struct IndexPublication {
    pub pending: PendingDocument,
    pub document: Option<IndexDocument>,
}

/// What a worker captured in the same read as its pending rows (spec/06):
/// the rebuild epoch and the target extractor version. Publication rechecks
/// both, plus job/phase and pauses, in the writer before any effect;
/// a stale or paused worker's submission is a quiet no-op.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct IndexFence {
    pub epoch: i64,
    pub version: u32,
}

/// One read's worth of pending work and the fence captured with it.
#[derive(Debug, Clone)]
pub struct IndexBatch {
    pub fence: IndexFence,
    pub documents: Vec<PendingDocument>,
}

/// What a read observed about an object's availability (spec/06
/// "Runtime control transactions"): the file is gone, or verification
/// already moved it to `recovery/`. The two recorded states are not
/// interchangeable, so this is an enum, not a bool.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ObservedAvailability {
    Missing,
    Quarantined,
}

impl ObservedAvailability {
    fn as_str(self) -> &'static str {
        match self {
            Self::Missing => "missing",
            Self::Quarantined => "quarantined",
        }
    }
}

/// Whether the queue still holds `token`'s exact revision and reason.
/// A superseded, requeued or acknowledged request answers false;
/// anything captured against it is stale and writes nothing.
pub(crate) fn token_current(tx: &Transaction, token: &PendingDocument) -> Result<bool, StoreError> {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM d_index_queue
         WHERE doc_id = ?1 AND revision = ?2 AND reason = ?3)",
        params![token.doc_id, token.revision, token.reason.as_str()],
        |row| row.get(0),
    )
    .map_err(sql_error)
}

/// Record a read observation of a missing or durably quarantined object
/// under its captured queue token (spec/06). The writer rechecks the
/// token's exact revision/reason and an existing published acquisition:
/// a superseded request or a republished source is a quiet no-op. A
/// current observation changes only `object_state`, supersedes the
/// request with a fresh `delete` revision preserving the oldest pending
/// age, and advances search generation once. Identity, provenance,
/// documents, postings, holds, epochs and progress are untouched;
/// source bookkeeping that stays allowed during a pause or rebuild;
/// never a publication, acknowledgement or deletion of authority.
pub fn note_observed_availability(
    tx: &Transaction,
    token: &PendingDocument,
    observed: ObservedAvailability,
    now: Timestamp,
) -> Result<bool, StoreError> {
    observe_availability_with_cut(tx, token, observed, now, &mut || Ok(()))
}

fn observe_availability_with_cut(
    tx: &Transaction,
    token: &PendingDocument,
    observed: ObservedAvailability,
    now: Timestamp,
    cut: &mut impl FnMut() -> Result<(), StoreError>,
) -> Result<bool, StoreError> {
    if token.doc_id <= 0 || token.revision <= 0 || token.reason == IndexReason::Delete {
        return Err(invalid(
            "an availability observation needs a non-delete queue token",
        ));
    }
    cut()?;
    if !token_current(tx, token)? {
        return Ok(false);
    }
    cut()?;
    let state: String = tx
        .query_row(
            "SELECT object_state FROM acquisitions WHERE acquisition_id = ?1",
            [token.doc_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| StoreError::not_found("index acquisition"))?;
    // A still-current non-delete token must name a published
    // acquisition: anything else was already acted on without
    // superseding the queue, which no runtime path produces.
    if state != "published" {
        return Err(StoreError::corrupt(
            "a current index token names an unavailable acquisition",
        ));
    }
    cut()?;
    tx.execute(
        "UPDATE acquisitions SET object_state = ?2 WHERE acquisition_id = ?1",
        params![token.doc_id, observed.as_str()],
    )
    .map_err(sql_error)?;
    cut()?;
    enqueue_one(tx, token.doc_id, IndexReason::Delete, now)?;
    cut()?;
    advance_generation(tx)?;
    Ok(true)
}

/// Target capability, including newer versions this app must not downgrade.
pub fn target_version(conn: &Connection) -> Result<u32, StoreError> {
    u32::try_from(meta_integer(conn, "search_extractor_version")?)
        .ok()
        .filter(|v| *v > 0)
        .ok_or_else(|| StoreError::corrupt("invalid target extractor version"))
}

pub fn search_generation(conn: &Connection) -> Result<i64, StoreError> {
    meta_integer(conn, "search_generation")
}

/// Upsert a bounded set of requests from current source availability. The
/// reason supersedes older work; age remains the oldest still-pending request.
/// A fresh monotonic revision is assigned even for a replay of the same input.
pub fn enqueue(
    tx: &Transaction,
    requests: &[(i64, IndexReason)],
    now: Timestamp,
) -> Result<(), StoreError> {
    check_count(requests.len())?;
    for &(doc_id, reason) in requests {
        enqueue_one(tx, doc_id, reason, now)?;
    }
    if !requests.is_empty() {
        advance_generation(tx)?;
    }
    Ok(())
}

/// Import owns its larger 250-row budget and advances generation once after
/// all acquisition enqueue operations, in the same publication transaction.
pub(crate) fn enqueue_one(
    tx: &Transaction,
    doc_id: i64,
    reason: IndexReason,
    now: Timestamp,
) -> Result<(), StoreError> {
    let available: String = tx
        .query_row(
            "SELECT object_state FROM acquisitions WHERE acquisition_id = ?1",
            [doc_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| StoreError::not_found("index acquisition"))?;
    if (reason == IndexReason::Delete) == (available == "published") {
        return Err(invalid(
            "index request disagrees with acquisition availability",
        ));
    }
    let revision = increment(tx, "index_inputs")?;
    tx.execute(
        "INSERT INTO d_index_queue (doc_id, reason, enqueued_at, revision) VALUES (?1, ?2, ?3, ?4)
         ON CONFLICT(doc_id) DO UPDATE SET reason = excluded.reason, revision = excluded.revision,
           enqueued_at = min(d_index_queue.enqueued_at, excluded.enqueued_at)",
        params![doc_id, reason.as_str(), now.as_millis(), revision],
    )
    .map_err(sql_error)?;
    Ok(())
}

/// Capture tokens and the fence in one short read transaction, then
/// extract outside SQLite. The byte limit is checked on extracted values
/// at publication time.
pub fn pending_batch(conn: &Connection, limit: usize) -> Result<IndexBatch, StoreError> {
    check_count(limit)?;
    let fence = rebuild::fence(conn)?;
    let mut stmt = conn
        .prepare(
            "SELECT doc_id, revision, reason, enqueued_at FROM d_index_queue
         ORDER BY enqueued_at ASC, doc_id ASC LIMIT ?1",
        )
        .map_err(sql_error)?;
    let rows = stmt
        .query_map([limit as i64], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get::<_, String>(2)?,
                row.get(3)?,
            ))
        })
        .map_err(sql_error)?;
    let documents = rows
        .map(|row| {
            let (doc_id, revision, reason, time) = row.map_err(sql_error)?;
            let reason = match reason.as_str() {
                "new" => IndexReason::New,
                "reextract" => IndexReason::Reextract,
                "delete" => IndexReason::Delete,
                _ => return Err(StoreError::corrupt("invalid index queue reason")),
            };
            Ok(PendingDocument {
                doc_id,
                revision,
                reason,
                enqueued_at: Timestamp::from_millis(time),
            })
        })
        .collect::<Result<Vec<_>, _>>()?;
    Ok(IndexBatch { fence, documents })
}

/// Publish only still-current tokens under the captured fence. Returns
/// the accepted document count; stale, paused or phase-gated submissions
/// write nothing and remain queued. A mixed batch may accept current
/// documents while skipping stale documents.
pub fn publish_batch(
    tx: &Transaction,
    fence: IndexFence,
    batch: &[IndexPublication],
) -> Result<usize, StoreError> {
    publish_with_cut(tx, fence, batch, &mut || Ok(()))
}

fn publish_with_cut(
    tx: &Transaction,
    fence: IndexFence,
    batch: &[IndexPublication],
    cut: &mut impl FnMut() -> Result<(), StoreError>,
) -> Result<usize, StoreError> {
    check_count(batch.len())?;
    let mut ids = BTreeSet::new();
    let mut bytes = 0usize;
    for item in batch {
        if item.pending.doc_id <= 0
            || item.pending.revision <= 0
            || !ids.insert(item.pending.doc_id)
            || (item.pending.reason == IndexReason::Delete) != item.document.is_none()
        {
            return Err(invalid("invalid or duplicate index publication token"));
        }
        if let Some(doc) = &item.document {
            doc.validate(fence.version)?;
            bytes = bytes.saturating_add(doc.column_bytes());
        }
    }
    if bytes > MAX_INDEX_BATCH_BYTES {
        return Err(invalid(
            "index publication exceeds the extracted-column byte budget",
        ));
    }
    cut()?;
    if !rebuild::publication_open(tx, fence)? {
        return Ok(0);
    }
    let mut accepted = 0;
    for item in batch {
        let id = item.pending.doc_id;
        cut()?;
        let current: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM d_index_queue WHERE doc_id = ?1 AND revision = ?2)",
                params![id, item.pending.revision],
                |row| row.get(0),
            )
            .map_err(sql_error)?;
        if !current {
            continue;
        }
        // External-content FTS requires the exact OLD values, before replacing
        // the docs row. Failed rows never had postings to delete.
        cut()?;
        tx.execute(
            "INSERT INTO d_search_fts (d_search_fts, rowid, subject, from_text, to_text,
               cc_text, body_authored, body_quoted, body_signature, attachment_names)
             SELECT 'delete', doc_id, subject, from_text, to_text, cc_text, body_authored,
               body_quoted, body_signature, attachment_names FROM d_search_docs
             WHERE doc_id = ?1 AND state IN ('indexed','partial')",
            [id],
        )
        .map_err(sql_error)?;
        cut()?;
        tx.execute("DELETE FROM d_search_docs WHERE doc_id = ?1", [id])
            .map_err(sql_error)?;
        if let Some(doc) = &item.document {
            cut()?;
            let mask = serde_json::to_string(&doc.truncated_fields)
                .map_err(|e| StoreError::internal(e.to_string()))?;
            let c = &doc.columns;
            tx.execute(
                "INSERT INTO d_search_docs (doc_id, extractor_version, state, truncated_fields,
                   subject, from_text, to_text, cc_text, body_authored, body_quoted, body_signature,
                   attachment_names) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)",
                params![
                    id,
                    fence.version,
                    doc.state.as_str(),
                    mask,
                    c[0],
                    c[1],
                    c[2],
                    c[3],
                    c[4],
                    c[5],
                    c[6],
                    c[7]
                ],
            )
            .map_err(sql_error)?;
            if doc.state != IndexState::Failed {
                cut()?;
                tx.execute(
                    "INSERT INTO d_search_fts (rowid, subject, from_text, to_text, cc_text,
                       body_authored, body_quoted, body_signature, attachment_names)
                     SELECT doc_id, subject, from_text, to_text, cc_text, body_authored,
                       body_quoted, body_signature, attachment_names FROM d_search_docs WHERE doc_id = ?1",
                    [id],
                ).map_err(sql_error)?;
            }
        }
        cut()?;
        tx.execute(
            "DELETE FROM d_index_queue WHERE doc_id = ?1 AND revision = ?2",
            params![id, item.pending.revision],
        )
        .map_err(sql_error)?;
        accepted += 1;
    }
    if accepted > 0 {
        rebuild::count_accepted(tx, fence, accepted)?;
        increment_with_cut(tx, "search_generation", cut)?;
    }
    Ok(accepted)
}

pub(crate) fn advance_generation(tx: &Transaction) -> Result<(), StoreError> {
    increment(tx, "search_generation").map(|_| ())
}

fn increment(tx: &Transaction, key: &str) -> Result<i64, StoreError> {
    increment_with_cut(tx, key, &mut || Ok(()))
}

fn increment_with_cut(
    tx: &Transaction,
    key: &str,
    cut: &mut impl FnMut() -> Result<(), StoreError>,
) -> Result<i64, StoreError> {
    cut()?;
    let value = meta_integer(tx, key)?
        .checked_add(1)
        .ok_or_else(|| StoreError::unavailable(format!("{key} exhausted")))?;
    cut()?;
    tx.execute(
        "UPDATE store_meta SET value = ?2 WHERE key = ?1",
        params![key, value.to_string()],
    )
    .map_err(sql_error)?;
    Ok(value)
}

fn meta_integer(conn: &Connection, key: &str) -> Result<i64, StoreError> {
    let value: String = conn
        .query_row(
            "SELECT value FROM store_meta WHERE key = ?1",
            [key],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    value
        .parse::<i64>()
        .ok()
        .filter(|v| *v >= 0)
        .ok_or_else(|| StoreError::corrupt(format!("invalid {key}")))
}

fn check_count(count: usize) -> Result<(), StoreError> {
    if count > MAX_INDEX_BATCH {
        return Err(invalid("index batch exceeds 200 documents"));
    }
    Ok(())
}

fn invalid(message: &str) -> StoreError {
    StoreError::InvalidInput {
        message: message.to_owned(),
    }
}

fn sql_error(error: rusqlite::Error) -> StoreError {
    classify(error, "search index storage")
}

#[cfg(test)]
mod tests;
