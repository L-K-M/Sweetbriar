//! Freeze scopes and stream immutable metadata (spec/09).
//! File output, receipts and orphan policy belong to M1.7c/e.

use std::collections::HashSet;
use std::io::{self, Write};
use std::ops::ControlFlow;
use std::path::Path;
use std::sync::Arc;

use rusqlite::{OptionalExtension, Transaction};
use sb_types::time::Clock;
use serde_json::{Value as Json, json};
use sha2::{Digest, Sha256};

use crate::lists::{ListScope, PageRead, list_generation, list_ids};
use crate::query::{Query, SearchScope, search_date_ids, search_relevance_ids};
use crate::schema::classify;
use crate::store::await_write;
use crate::{
    Scoped, StoreError, WritePriority, export_occurrence_metadata, read_error_as_store_error,
};

use super::{
    ExportChunkWriter, ExportJob, ExportJobState, MAX_JOB_ID_BYTES, advance_export_job,
    create_export_job, invalid,
};

const CAPTURE_VERSION: u32 = 1;

/// Output intent (spec/09); capture never writes files.
#[derive(Debug, Clone, Copy)]
pub enum ExportFormat {
    Eml,
    Mboxrd,
}

/// Search membership order at the fenced generation (spec/09).
#[derive(Debug, Clone, Copy)]
pub enum ExportSearchSort {
    Date,
    Relevance,
}

/// A frozen Engine selection handle: provenance plus its registry's
/// immutable, already-ordered ids behind one leased Arc (spec/09).
#[derive(Debug, Clone)]
pub struct ExportSelection {
    selection_id: String,
    window_id: u64,
    generation: u64,
    ids: Arc<[i64]>,
}

impl ExportSelection {
    /// Lease registry-owned order and provenance. Validate outside its
    /// lock; add no member-count cap.
    pub fn new(
        selection_id: String,
        window_id: u64,
        generation: u64,
        ids: Arc<[i64]>,
    ) -> Result<Self, StoreError> {
        if selection_id.is_empty() {
            return Err(invalid("a selection id is required"));
        }
        if window_id == 0 {
            return Err(invalid("a selection window id is required"));
        }
        let mut seen = HashSet::with_capacity(ids.len());
        if ids.iter().any(|id| !seen.insert(*id)) {
            return Err(invalid("a selection's membership holds duplicates"));
        }
        Ok(Self {
            selection_id,
            window_id,
            generation,
            ids,
        })
    }

    pub fn selection_id(&self) -> &str {
        &self.selection_id
    }

    pub fn window_id(&self) -> u64 {
        self.window_id
    }

    pub fn generation(&self) -> u64 {
        self.generation
    }

    pub fn count(&self) -> u64 {
        self.ids.len() as u64
    }
}

/// The capture's one membership source (spec/09): each scope resolves
/// to a frozen ordered id list at one fenced generation.
#[derive(Debug, Clone)]
pub enum ExportScope {
    Account {
        account_id: String,
        expected_generation: u64,
    },
    Mailbox {
        mailbox_id: i64,
        expected_generation: u64,
    },
    Search {
        query: Query,
        scope: SearchScope,
        sort: ExportSearchSort,
        expected_generation: u64,
    },
    Selection(ExportSelection),
}

/// A validated capture request: the intent binds once here, so the job
/// request, digest and header always agree (spec/09).
#[derive(Debug, Clone)]
pub struct ExportCaptureRequest {
    scope: ExportScope,
    intent: Json,
    request_json: String,
}

impl ExportCaptureRequest {
    /// `destination` is identity only: an absolute, nonempty UTF-8
    /// path, never touched; output ownership stays M1.7c's.
    pub fn new(
        format: ExportFormat,
        destination: String,
        scope: ExportScope,
    ) -> Result<Self, StoreError> {
        if destination.is_empty() || !Path::new(&destination).is_absolute() {
            return Err(invalid(
                "an export destination must be a nonempty absolute path",
            ));
        }
        if let ExportScope::Account { account_id, .. } = &scope
            && (account_id.is_empty() || account_id.len() > MAX_JOB_ID_BYTES)
        {
            return Err(invalid(
                "an export account id must be 1 through 128 UTF-8 bytes",
            ));
        }
        let intent = intent(format, &destination, &scope)?;
        Ok(Self {
            scope,
            request_json: request_json(&intent)?,
            intent,
        })
    }
}

/// The deterministic intent (spec/09): `v`, format, destination and a
/// scope descriptor: `kind`, `id`, `generation` for account/mailbox;
/// Search adds canonical query, candidate scope and sort; a selection
/// names provenance, count and ordered-ID hash, never its ids.
fn intent(
    format: ExportFormat,
    destination: &str,
    scope: &ExportScope,
) -> Result<Json, StoreError> {
    let format = match format {
        ExportFormat::Eml => "eml",
        ExportFormat::Mboxrd => "mboxrd",
    };
    let descriptor = match scope {
        ExportScope::Account {
            account_id,
            expected_generation,
        } => json!({
            "kind": "account",
            "id": account_id,
            "generation": expected_generation,
        }),
        ExportScope::Mailbox {
            mailbox_id,
            expected_generation,
        } => json!({
            "kind": "mailbox",
            "id": mailbox_id,
            "generation": expected_generation,
        }),
        ExportScope::Search {
            query,
            scope: candidates,
            sort,
            expected_generation,
        } => json!({
            "kind": "search",
            // Canonical query embedded as a value, never a quoted string.
            "query": serde_json::from_str::<Json>(query.canonical_json())
                .map_err(|_| StoreError::internal("checked canonical query JSON"))?,
            "scope": candidate(candidates),
            "sort": match sort {
                ExportSearchSort::Date => "date",
                ExportSearchSort::Relevance => "relevance",
            },
            "generation": expected_generation,
        }),
        ExportScope::Selection(selection) => json!({
            "kind": "selection",
            "id": selection.selection_id(),
            "window": selection.window_id(),
            "generation": selection.generation(),
            "count": selection.count(),
            "ids_sha256": ids_digest(&selection.ids),
        }),
    };
    Ok(json!({
        "v": CAPTURE_VERSION,
        "format": format,
        "destination": destination,
        "scope": descriptor,
    }))
}

/// The candidate scope as the Search cursor's spec encodes it.
fn candidate(scope: &SearchScope) -> Json {
    match scope {
        SearchScope::All => json!("all"),
        SearchScope::Account(id) => json!({"account": id.to_string()}),
        SearchScope::Mailbox(id) => json!({"mailbox": id}),
    }
}

/// An `io::Write` that feeds a digest, so hashing streams the intent
/// through serde_json's writer without materializing its encoding.
struct DigestWriter(Sha256);

impl Write for DigestWriter {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.0.update(buf);
        Ok(buf.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

/// The bounded job request `{"v":1,"capture":1,"sha256":<digest>}`
/// (spec/09): the digest of the deterministic intent's canonical bytes.
fn request_json(intent: &Json) -> Result<String, StoreError> {
    let mut digest = DigestWriter(Sha256::new());
    serde_json::to_writer(&mut digest, intent)
        .map_err(|_| StoreError::internal("an export intent failed to serialize"))?;
    Ok(json!({"v": CAPTURE_VERSION, "capture": CAPTURE_VERSION, "sha256": hex(&digest.0.finalize())}).to_string())
}

/// SHA-256 of each signed occurrence id's eight-byte big-endian form,
/// in order (spec/09), hex.
fn ids_digest(ids: &[i64]) -> String {
    let mut digest = Sha256::new();
    for id in ids {
        digest.update(id.to_be_bytes());
    }
    hex(&digest.finalize())
}

fn hex(bytes: &[u8]) -> String {
    const HEX_DIGITS: &[u8; 16] = b"0123456789abcdef";
    let mut out = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        out.push(HEX_DIGITS[(byte >> 4) as usize] as char);
        out.push(HEX_DIGITS[(byte & 0x0f) as usize] as char);
    }
    out
}

/// Freeze membership once, then each member in a separate short read.
/// Replay sealed jobs without sources; stops retain a capturing prefix.
/// Store rejects writer-thread reentry before calling this coordinator.
pub(crate) fn capture(
    store: &crate::Store,
    id: &str,
    request: &ExportCaptureRequest,
    clock: &dyn Clock,
    mut progress: impl FnMut(u64) -> ControlFlow<()>,
) -> Result<crate::Scoped<ExportJob>, StoreError> {
    let job_id = id.to_owned();
    let request_json = request.request_json.clone();
    let now = clock.now();
    let job = await_write(store.write(WritePriority::Index, move |tx| {
        create_export_job(tx, &job_id, &request_json, now)
    }))?;
    match job.state() {
        // Sealed or past it: the kept job replays untouched (spec/09).
        ExportJobState::Ready | ExportJobState::Running | ExportJobState::Completed => {
            return Ok(Scoped::Done(job));
        }
        ExportJobState::Cancelled | ExportJobState::Failed => {
            return Err(StoreError::conflict("the export job already ended"));
        }
        ExportJobState::Capturing => {}
    }
    // Selection leases outlive their source windows and keep the original fence.
    let (ids, generation) = match &request.scope {
        ExportScope::Selection(selection) => (Arc::clone(&selection.ids), selection.generation()),
        scope => store.read(|tx| scoped_ids(tx, scope))?,
    };
    if progress(0).is_break() {
        return Ok(Scoped::Stopped);
    }
    let mut sink = store.export_chunk_writer(&job)?;
    write_record(
        &mut sink,
        &json!({
            "v": CAPTURE_VERSION, "kind": "capture", "intent": request.intent,
            "generation": generation, "entries": ids.len() as u64,
            "ids_sha256": ids_digest(&ids),
        }),
    )?;
    let mut completed = 0u64;
    for &id in ids.iter() {
        // Each record freezes in its own short read, released before
        // its serialization can wait on the writer queue (spec/09).
        let metadata = store.read(|tx| export_occurrence_metadata(tx, id))?;
        write_record(&mut sink, &metadata)?;
        completed += 1;
        if progress(completed).is_break() {
            // Drop never flushes: the capturing prefix stays immutable.
            return Ok(Scoped::Stopped);
        }
    }
    let job = sink.finish()?;
    let job_id = id.to_owned();
    let revision = job.revision();
    // Race/cancel conflicts surface naturally through the revision fence.
    let checkpoint = json!({
        "v": CAPTURE_VERSION, "capture": CAPTURE_VERSION, "entries": completed, "chunks": job.captured_chunks(),
    })
    .to_string();
    let now = clock.now();
    let job = await_write(store.write(WritePriority::Index, move |tx| {
        advance_export_job(
            tx,
            &job_id,
            revision,
            ExportJobState::Ready,
            &checkpoint,
            now,
        )
    }))?;
    Ok(Scoped::Done(job))
}

fn scoped_ids(tx: &Transaction, scope: &ExportScope) -> Result<(Arc<[i64]>, u64), StoreError> {
    match scope {
        ExportScope::Account {
            account_id,
            expected_generation,
        } => {
            known_account(tx, account_id)?;
            let generation = list_generation(tx)?;
            if generation != *expected_generation {
                return Err(stale_generation());
            }
            let mut statement = tx
                .prepare_cached(
                    "SELECT occurrence_id FROM d_list_rows WHERE account_id = ?1
                     ORDER BY mailbox_id, sort_date DESC, occurrence_id DESC",
                )
                .map_err(sql_error)?;
            let ids = statement
                .query_map([account_id], |row| row.get(0))
                .map_err(sql_error)?
                .collect::<Result<Vec<i64>, _>>()
                .map_err(sql_error)?;
            Ok((Arc::from(ids), generation))
        }
        ExportScope::Mailbox {
            mailbox_id,
            expected_generation,
        } => {
            known_mailbox(tx, *mailbox_id)?;
            let (generation, ids) = ready(list_ids(
                tx,
                ListScope::Mailbox(*mailbox_id),
                Some(*expected_generation),
            )?)?;
            Ok((Arc::from(ids), generation))
        }
        ExportScope::Search {
            query,
            scope: candidates,
            sort,
            expected_generation,
        } => {
            let search = match sort {
                ExportSearchSort::Date => search_date_ids,
                ExportSearchSort::Relevance => search_relevance_ids,
            };
            let (generation, ids) = ready(search(
                tx,
                query,
                candidates.clone(),
                Some(*expected_generation),
                None,
            )?)?;
            Ok((Arc::from(ids), generation))
        }
        ExportScope::Selection(_) => unreachable!("the lease carries its own membership"),
    }
}

/// `Stale` is `Conflict`, never an output-ready capture (spec/09).
fn ready<T>(read: PageRead<T>) -> Result<T, StoreError> {
    match read {
        PageRead::Ready(value) => Ok(value),
        PageRead::Stale { .. } => Err(stale_generation()),
        // An unanchored ids read cannot leave the window it names.
        PageRead::NotInWindow => Err(StoreError::internal("an ids read left its window")),
    }
}

fn stale_generation() -> StoreError {
    StoreError::conflict("the export's membership generation is stale")
}

/// An unknown account is `NotFound` (spec/09).
fn known_account(tx: &Transaction, account_id: &str) -> Result<(), StoreError> {
    let exists: bool = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM accounts WHERE account_id = ?1)",
            [account_id],
            |row| row.get(0),
        )
        .map_err(sql_error)?;
    if !exists {
        return Err(StoreError::not_found(format!("account {account_id}")));
    }
    Ok(())
}

/// An unknown mailbox is `NotFound`; a mailbox whose stored parent is
/// missing is `Corrupt`, never a quietly absent scope.
fn known_mailbox(tx: &Transaction, mailbox_id: i64) -> Result<(), StoreError> {
    let account: Option<String> = tx
        .query_row(
            "SELECT account_id FROM mailboxes WHERE mailbox_id = ?1",
            [mailbox_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_error)?;
    let Some(account) = account else {
        return Err(StoreError::not_found(format!("mailbox {mailbox_id}")));
    };
    known_account(tx, &account).map_err(|error| match error {
        StoreError::NotFound { .. } => StoreError::corrupt("a mailbox's account row is missing"),
        other => other,
    })
}

/// Serialize one record then its LF through the sink; the producing
/// read was already released, and no flush happens between records.
fn write_record(
    sink: &mut ExportChunkWriter<'_>,
    record: &impl serde::Serialize,
) -> Result<(), StoreError> {
    serde_json::to_writer(&mut *sink, record).map_err(recover)?;
    sink.write_all(b"\n").map_err(recover)
}

/// Recover the boxed StoreError the sink reported through io or serde,
/// keeping its typed variant and native I/O kind (spec/09); a bare
/// I/O failure stays `Unavailable` with its kind attached.
fn recover(error: impl Into<io::Error>) -> StoreError {
    let error = error.into();
    read_error_as_store_error(&error)
        .cloned()
        .unwrap_or_else(|| {
            StoreError::unavailable_io(
                format!("cannot write the export capture: {error}"),
                error.kind(),
            )
        })
}

fn sql_error(error: rusqlite::Error) -> StoreError {
    match error {
        rusqlite::Error::InvalidColumnType(..)
        | rusqlite::Error::IntegralValueOutOfRange(..)
        | rusqlite::Error::FromSqlConversionFailure(..)
        | rusqlite::Error::Utf8Error(..) => {
            StoreError::corrupt(format!("invalid stored export scope column: {error}"))
        }
        error => classify(error, "an export capture read"),
    }
}

#[cfg(test)]
mod tests;
