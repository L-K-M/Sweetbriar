use std::collections::BTreeSet;

use rusqlite::{OptionalExtension, Transaction, params};
use serde_json::json;

use super::jobs::source_progress;
use super::{
    FIND_IMPORT_SOURCE, ImportJobState, advance_import_job, canonical_array, canonical_object,
    import_job, import_job_source, invalid, sql_error,
};
use crate::{ObjectId, StoreError, Timestamp, list_rows};

/// Spec/09's maximum transaction batch, independent of body size.
pub const MAX_IMPORT_BATCH: usize = 250;
/// Spec/03's background-write budget; published bodies are outside SQLite.
pub const MAX_IMPORT_CHANGES_BYTES: usize = 2 * 1024 * 1024;
// Reserve space for UUIDs, hashes, integers and row/index framing.
const PER_MESSAGE_ROW_BUDGET: usize = 1024;
// JSON can turn one control byte into six bytes, stored in row and index.
const SOURCE_PATH_BUDGET_MULTIPLIER: usize = 6 * 2;
// The list row's integers and framing in its row and two covering indexes.
const LIST_ROW_BUDGET: usize = 512;
// Sender, subject and snippet repeat in the list row and both its covering
// indexes (spec/02 `d_list_rows`).
const LIST_ROW_TEXT_COPIES: usize = 3;

#[derive(Debug, Clone, Copy)]
pub enum ImportSourceKind {
    Mbox,
    Eml,
}

impl ImportSourceKind {
    fn name(self) -> &'static str {
        match self {
            Self::Mbox => "import_mbox",
            Self::Eml => "import_eml",
        }
    }
}

#[derive(Debug, Clone, Copy, Default)]
pub enum ImportReadState {
    FromHeaders {
        seen: bool,
    },
    #[default]
    ArchiveDefault,
}

#[derive(Debug, Clone, Copy, Default)]
pub struct ImportFlags {
    pub read: ImportReadState,
    pub flagged: bool,
    pub answered: bool,
    pub draft: bool,
    pub deleted: bool,
}

#[derive(Debug, Clone, Copy, Default)]
pub enum ImportParseState {
    Parsed,
    Partial,
    #[default]
    Failed,
}

impl ImportParseState {
    fn name(self) -> &'static str {
        match self {
            Self::Parsed => "ok",
            Self::Partial => "partial",
            Self::Failed => "failed",
        }
    }
}

/// Persistence input mapped by sb-resources from the MIME leaf. Named
/// fields are public for that mapping; no MIME type crosses leaf layers.
#[derive(Debug, Clone, Default)]
pub struct ImportSummary {
    pub parser_version: u32,
    pub parse_state: ImportParseState,
    pub header_message_id: Option<String>,
    pub subject: Option<String>,
    pub from_addr: Option<String>,
    pub from_name: Option<String>,
    pub reply_to_json: Option<String>,
    pub to_json: Option<String>,
    pub cc_json: Option<String>,
    pub date_header: Option<i64>,
    pub in_reply_to: Option<String>,
    pub references_json: Option<String>,
    pub list_id: Option<String>,
    pub delivered_to_json: Option<String>,
    pub has_attachments: bool,
    pub snippet: Option<String>,
}

/// The object comes from publish, never from a re-serialization.
#[derive(Debug)]
pub struct ImportMessage {
    pub object: ObjectId,
    pub source_kind: ImportSourceKind,
    pub source_path: String,
    pub source_sha256: String,
    pub offset: u64,
    pub flags: ImportFlags,
    pub summary: ImportSummary,
}

impl ImportMessage {
    /// Conservative metadata budget for a batch builder. Count escaped
    /// provenance twice (row and unique index) and the list row's text
    /// three times; bodies never enter it.
    pub fn change_budget_bytes(&self) -> usize {
        let summary = &self.summary;
        let list_text = list_row_bytes(summary);
        [
            &summary.header_message_id,
            &summary.subject,
            &summary.from_addr,
            &summary.from_name,
            &summary.reply_to_json,
            &summary.to_json,
            &summary.cc_json,
            &summary.in_reply_to,
            &summary.references_json,
            &summary.list_id,
            &summary.delivered_to_json,
            &summary.snippet,
        ]
        .into_iter()
        .flatten()
        .fold(
            PER_MESSAGE_ROW_BUDGET
                .saturating_add(list_text)
                .saturating_add(
                    self.source_path
                        .len()
                        .saturating_mul(SOURCE_PATH_BUDGET_MULTIPLIER),
                ),
            |size, text| size.saturating_add(text.len()),
        )
    }
}

/// What one list row showing `summary` costs the batch budget.
fn list_row_bytes(summary: &ImportSummary) -> usize {
    [
        &summary.subject,
        &summary.from_addr,
        &summary.from_name,
        &summary.snippet,
    ]
    .into_iter()
    .flatten()
    .fold(LIST_ROW_BUDGET, |size, text| {
        size.saturating_add(text.len().saturating_mul(LIST_ROW_TEXT_COPIES))
    })
}

/// The list rows a batch must refresh, and the bytes of those beyond
/// its messages' own estimates.
#[derive(Default)]
struct ListChanges {
    occurrences: BTreeSet<i64>,
    extra_bytes: usize,
}

pub struct ImportBatch<'a> {
    pub job_id: &'a str,
    pub expected_revision: i64,
    pub source_ordinal: u64,
    pub mailbox_name: &'a str,
    pub messages: &'a [ImportMessage],
}

/// Duplicates count shared bytes, including already-imported sources;
/// imported counts new occurrences. Rejected messages still keep bytes.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct ImportBatchStats {
    pub processed: u64,
    pub imported: u64,
    pub duplicates: u64,
    pub rejected: u64,
    pub bytes_stored: u64,
    pub read_from_headers: u64,
    pub read_default: u64,
}

/// Write a batch and its progress together. `progress` is pure engine
/// logic: given the committed counts, return (source, job) checkpoints.
/// Any failure, including stale revisions or invalid JSON, must propagate
/// out of the caller's writer closure so its transaction rolls back.
pub fn commit_import_batch(
    tx: &Transaction,
    batch: &ImportBatch<'_>,
    now: Timestamp,
    progress: impl FnOnce(&ImportBatchStats) -> Result<(String, String), StoreError>,
) -> Result<ImportBatchStats, StoreError> {
    if batch.messages.len() > MAX_IMPORT_BATCH {
        return Err(invalid(format!(
            "an import batch exceeds {MAX_IMPORT_BATCH} messages"
        )));
    }
    // The mailbox name is stored in both its row and its unique index.
    let change_bytes = batch.messages.iter().fold(
        batch.mailbox_name.len().saturating_mul(2),
        |size, message| size.saturating_add(message.change_budget_bytes()),
    );
    if change_bytes > MAX_IMPORT_CHANGES_BYTES {
        return Err(invalid(
            "import metadata exceeds the background-write budget",
        ));
    }
    let job = import_job(tx, batch.job_id)?;
    if job.state() != ImportJobState::Running || job.revision() != batch.expected_revision {
        return Err(StoreError::conflict(
            "import job is not running at the expected revision",
        ));
    }
    import_job_source(tx, batch.job_id, batch.source_ordinal)?;
    let (account, mailbox) = local_mailbox(tx, batch.mailbox_name)?;
    let mut stats = ImportBatchStats::default();
    let mut list = ListChanges::default();
    for message in batch.messages {
        store_message(tx, &account, mailbox, message, now, &mut stats, &mut list)?;
    }
    let change_bytes = change_bytes.saturating_add(list.extra_bytes);
    if change_bytes > MAX_IMPORT_CHANGES_BYTES {
        return Err(invalid(
            "import list rows exceed the background-write budget",
        ));
    }
    // One generation per batch that changed what lists show (spec/02).
    if list_rows::refresh(tx, &list.occurrences)? {
        list_rows::advance_generation(tx)?;
    }
    let (source_checkpoint, job_checkpoint) = progress(&stats)?;
    let source_checkpoint = canonical_object(&source_checkpoint)?;
    let job_checkpoint = canonical_object(&job_checkpoint)?;
    if change_bytes
        .saturating_add(source_checkpoint.len())
        .saturating_add(job_checkpoint.len())
        > MAX_IMPORT_CHANGES_BYTES
    {
        return Err(invalid(
            "import checkpoints exceed the background-write budget",
        ));
    }
    source_progress(tx, batch.job_id, batch.source_ordinal, &source_checkpoint)?;
    advance_import_job(
        tx,
        batch.job_id,
        batch.expected_revision,
        ImportJobState::Running,
        &job_checkpoint,
        now,
    )?;
    Ok(stats)
}

fn local_mailbox(tx: &Transaction, name: &str) -> Result<(String, i64), StoreError> {
    if name.is_empty() || name.contains('\0') {
        return Err(invalid(
            "an import mailbox name is required and cannot contain NUL",
        ));
    }
    let account: String = tx
        .query_row(
            "SELECT account_id FROM accounts WHERE provider = 'local_archive' AND kind = 'local'",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| StoreError::corrupt("local archive is missing"))?;
    tx.execute(
        "INSERT INTO mailboxes (account_id, remote_name, kind, selectable, sync_state)
         VALUES (?1, ?2, 'folder', 1, 'synced') ON CONFLICT(account_id, remote_name) DO NOTHING",
        params![account, name],
    )
    .map_err(sql_error)?;
    let (id, kind, selectable): (i64, String, bool) = tx.query_row(
        "SELECT mailbox_id, kind, selectable FROM mailboxes WHERE account_id = ?1 AND remote_name = ?2",
        params![account, name], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
    ).map_err(sql_error)?;
    if kind != "folder" || !selectable {
        return Err(StoreError::conflict(
            "import destination is not a selectable local folder",
        ));
    }
    Ok((account, id))
}

fn store_message(
    tx: &Transaction,
    account: &str,
    mailbox: i64,
    message: &ImportMessage,
    now: Timestamp,
    stats: &mut ImportBatchStats,
    list: &mut ListChanges,
) -> Result<(), StoreError> {
    let hash = message.object.sha256_hex();
    let length = i64::try_from(message.object.byte_length())
        .map_err(|_| invalid("message size exceeds SQLite's range"))?;
    if message.source_path.is_empty()
        || message.source_path.contains('\0')
        || message.source_sha256.len() != 64
        || !message
            .source_sha256
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        || (matches!(message.source_kind, ImportSourceKind::Eml) && message.offset != 0)
    {
        return Err(invalid("invalid import source path, hash or EML offset"));
    }
    let source_ref = json!({"mailbox": mailbox, "offset": message.offset,
        "path": message.source_path, "sha256": message.source_sha256})
    .to_string();
    let previous: Option<String> = tx
        .query_row(
            FIND_IMPORT_SOURCE,
            params![account, message.source_kind.name(), source_ref],
            |row| row.get(0),
        )
        .optional()
        .map_err(sql_error)?;
    if previous.as_ref().is_some_and(|previous| previous != &hash) {
        return Err(StoreError::conflict(
            "source identity was already imported with different bytes",
        ));
    }
    let inserted = tx.execute(
        "INSERT INTO acquisitions (content_sha256, byte_length, object_state, first_acquired_at)
         VALUES (?1, ?2, 'published', ?3) ON CONFLICT(content_sha256) DO NOTHING",
        params![hash, length, now.as_millis()],
    ).map_err(sql_error)? > 0;
    let (acquisition, stored_length): (i64, i64) = tx
        .query_row(
            "SELECT acquisition_id, byte_length FROM acquisitions WHERE content_sha256 = ?1",
            [&hash],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .map_err(sql_error)?;
    if stored_length != length {
        return Err(StoreError::corrupt(
            "an acquisition hash has a different byte length",
        ));
    }
    tx.execute(
        "UPDATE acquisitions SET object_state = 'published' WHERE acquisition_id = ?1",
        [acquisition],
    )
    .map_err(sql_error)?;
    stats.processed += 1;
    stats.duplicates += u64::from(!inserted);
    stats.rejected += u64::from(matches!(
        message.summary.parse_state,
        ImportParseState::Failed
    ));
    let seen = match message.flags.read {
        ImportReadState::FromHeaders { seen } => {
            stats.read_from_headers += 1;
            seen
        }
        ImportReadState::ArchiveDefault => {
            stats.read_default += 1;
            true
        }
    };
    if inserted {
        stats.bytes_stored = stats
            .bytes_stored
            .checked_add(message.object.byte_length())
            .ok_or_else(|| invalid("import byte count overflow"))?;
    }
    if previous.is_some() {
        return Ok(());
    }
    tx.execute(
        "INSERT INTO acquisition_sources
         (acquisition_id, account_id, source_kind, source_ref, completeness, acquired_at)
         VALUES (?1, ?2, ?3, ?4, 'complete', ?5)",
        params![
            acquisition,
            account,
            message.source_kind.name(),
            source_ref,
            now.as_millis()
        ],
    )
    .map_err(sql_error)?;
    tx.execute(
        "INSERT INTO logical_messages (account_id, acquisition_id, created_at) VALUES (?1, ?2, ?3)",
        params![account, acquisition, now.as_millis()],
    )
    .map_err(sql_error)?;
    let logical = tx.last_insert_rowid();
    tx.execute(
        "INSERT INTO occurrences (account_id, mailbox_id, message_id, rfc822_size,
         f_seen, f_flagged, f_answered, f_draft, f_deleted, state, first_seen_at, last_seen_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 'present', ?10, ?10)",
        params![
            account,
            mailbox,
            logical,
            length,
            seen,
            message.flags.flagged,
            message.flags.answered,
            message.flags.draft,
            message.flags.deleted,
            now.as_millis()
        ],
    )
    .map_err(sql_error)?;
    list.occurrences.insert(tx.last_insert_rowid());
    stats.imported += 1;
    if save_summary(tx, acquisition, &message.summary)? {
        // A first summary for bytes already held changes the rows of
        // every message sharing them (spec/02).
        if !inserted {
            let sharing = list_rows::sharing_acquisition(tx, acquisition)?;
            list.extra_bytes = list.extra_bytes.saturating_add(
                sharing
                    .len()
                    .saturating_mul(list_row_bytes(&message.summary)),
            );
            list.occurrences.extend(sharing);
        }
        tx.execute(
            "INSERT INTO d_index_queue (doc_id, reason, enqueued_at) VALUES (?1, ?2, ?3)
             ON CONFLICT(doc_id) DO UPDATE SET reason = excluded.reason, enqueued_at = excluded.enqueued_at",
            params![acquisition, if inserted { "new" } else { "reextract" }, now.as_millis()],
        ).map_err(sql_error)?;
    }
    Ok(())
}

/// Store `acquisition`'s first summary; an existing one, even from an
/// older parser, stays (spec/02). Returns whether one was stored.
fn save_summary(
    tx: &Transaction,
    acquisition: i64,
    summary: &ImportSummary,
) -> Result<bool, StoreError> {
    let [reply_to, to, cc, references, delivered_to] = [
        &summary.reply_to_json,
        &summary.to_json,
        &summary.cc_json,
        &summary.references_json,
        &summary.delivered_to_json,
    ]
    .map(canonical_array);
    tx.execute(
        "INSERT INTO d_message_summary (acquisition_id, parser_version, parse_state,
         header_message_id, subject, from_addr, from_name, reply_to_json, to_json, cc_json,
         date_header, in_reply_to, references_json, list_id, delivered_to_json, has_attachments, snippet)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
         ON CONFLICT(acquisition_id) DO NOTHING",
        params![acquisition, summary.parser_version, summary.parse_state.name(),
            summary.header_message_id, summary.subject, summary.from_addr, summary.from_name,
            reply_to?, to?, cc?, summary.date_header, summary.in_reply_to, references?,
            summary.list_id, delivered_to?, summary.has_attachments, summary.snippet],
    ).map(|changed| changed > 0).map_err(sql_error)
}
