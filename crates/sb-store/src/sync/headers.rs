//! Header batches (spec/04 step 3): one fetched batch of a mailbox's
//! headers commits with its `uidnext` checkpoint in one transaction,
//! so a crash resumes without gaps or duplicates. Each header finds or
//! creates its logical message by the provider key the policy read
//! (spec/02: provider evidence only), stores the fetched header
//! literal and its parsed fields in `message_headers`, and inserts or
//! refreshes the occurrence; a locator still bound to another
//! provider key is superseded (UID reuse).

use std::collections::{BTreeSet, HashSet};

use rusqlite::{OptionalExtension, Transaction, params};

use crate::schema::classify;
use crate::{StoreError, Timestamp, list_rows};

/// spec/03: a sync batch holds at most 250 occurrence rows ...
pub const MAX_HEADER_BATCH: usize = 250;
/// ... and at most 2 MB of changes per transaction, the same
/// background-write budget imports use.
pub const MAX_HEADER_BATCH_BYTES: usize = 2 * 1024 * 1024;
/// A conservative charge per header for its occurrence, logical
/// message and index rows, besides its own text.
const HEADER_ROW_OVERHEAD: usize = 1024;

/// The flags IMAP reported for one occurrence (spec/02's occurrence
/// flag columns). `keywords` holds every other flag; the batch stores
/// them sorted and deduplicated.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SyncedFlags {
    pub seen: bool,
    pub flagged: bool,
    pub answered: bool,
    pub draft: bool,
    pub deleted: bool,
    pub keywords: Vec<String>,
}

/// The `message_headers` columns parsed from one header literal
/// (spec/02), mapped by sb-resources from the MIME leaf in the same
/// shapes as `d_message_summary`'s: address lists as
/// `[{"address": …, "name": …}]`, string lists as JSON arrays.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct HeaderColumns {
    pub header_message_id: Option<String>,
    pub subject: Option<String>,
    pub from_json: Option<String>,
    pub reply_to_json: Option<String>,
    pub to_json: Option<String>,
    pub cc_json: Option<String>,
    /// Delivered-To and X-Original-To (spec/02).
    pub delivered_to_json: Option<String>,
    pub date_header: Option<i64>,
    pub in_reply_to: Option<String>,
    pub references_json: Option<String>,
    pub list_id: Option<String>,
}

impl HeaderColumns {
    fn text_bytes(&self) -> usize {
        [
            &self.header_message_id,
            &self.subject,
            &self.from_json,
            &self.reply_to_json,
            &self.to_json,
            &self.cc_json,
            &self.delivered_to_json,
            &self.in_reply_to,
            &self.references_json,
            &self.list_id,
        ]
        .into_iter()
        .map(|text| text.as_deref().map_or(0, str::len))
        .sum()
    }
}

/// One fetched header (spec/04 step 3's item list).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncedHeader {
    /// The occurrence's UID within the batch's epoch; never zero.
    pub uid: u32,
    pub flags: SyncedFlags,
    pub internal_date: Option<Timestamp>,
    pub rfc822_size: Option<u64>,
    /// The policy's grouping key (Proton: X-Pm-Internal-Id); `None`
    /// groups nothing, so the header gets a logical message of its own.
    pub provider_key: Option<String>,
    /// The exact header-fields literal fetched.
    pub raw_fields: Vec<u8>,
    pub columns: HeaderColumns,
}

/// One batch of a mailbox's headers and the checkpoint it commits.
#[derive(Debug, Clone, Copy)]
pub struct HeaderBatch<'a> {
    pub account_id: &'a str,
    pub mailbox_id: i64,
    /// The epoch the headers were fetched in; it must be the mailbox's.
    pub uidvalidity: u32,
    /// The mailbox's `uidnext` when the fetch was planned: a stored
    /// checkpoint that moved since makes the batch stale (`Conflict`).
    pub expected_uidnext: Option<u32>,
    /// The checkpoint this batch commits: above every UID it holds and
    /// never below the stored one. Backfilled UIDs below the stored
    /// checkpoint (spec/04) leave it where it is.
    pub next_uidnext: u32,
    /// Whether the batch reached the server's UIDNEXT: the mailbox
    /// becomes `synced`, else stays `backfilling`.
    pub caught_up: bool,
    pub headers: &'a [SyncedHeader],
}

/// What one batch wrote.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct HeaderBatchStats {
    /// New current occurrences.
    pub inserted: usize,
    /// Current occurrences of the same message whose flags and fetch
    /// facts were refreshed.
    pub refreshed: usize,
    /// Current occurrences a reused UID superseded.
    pub superseded: usize,
    /// Logical messages created.
    pub new_messages: usize,
}

/// The mailbox state a header batch may write into.
const SYNCING_STATES: [&str; 2] = ["backfilling", "synced"];

/// Commit one header batch with its checkpoint (spec/04 step 3), in
/// the caller's writer transaction. Any failure leaves nothing:
/// returning `Err` from the writer closure rolls the batch back.
///
/// `NotFound` for a mailbox not of the account; `Conflict` when the
/// mailbox is not syncing (`new`, `epoch_changed`, `gone`), its epoch
/// is not the batch's, or its checkpoint moved since the fetch was
/// planned; `InvalidInput` for an oversized batch, a zero or repeated
/// UID, a UID at or above `next_uidnext`, or a checkpoint that would
/// move backwards.
pub fn commit_header_batch(
    tx: &Transaction,
    batch: &HeaderBatch<'_>,
    now: Timestamp,
) -> Result<HeaderBatchStats, StoreError> {
    let mut remaining = preflight(batch)?;
    let stored_uidnext = guard_mailbox(tx, batch)?;
    if i64::from(batch.next_uidnext) < stored_uidnext.unwrap_or(1) {
        return Err(invalid("a header batch never moves the checkpoint back"));
    }

    let mut stats = HeaderBatchStats::default();
    let mut changed = BTreeSet::new();
    for header in batch.headers {
        store_header(tx, batch, header, now, &mut stats, &mut changed)?;
    }

    // One generation per batch that changed what lists show (spec/02).
    if list_rows::refresh_priced(tx, &changed, &mut |bytes| {
        remaining = remaining
            .checked_sub(bytes)
            .ok_or_else(|| invalid("a header batch's list rows exceed the write budget"))?;
        Ok(())
    })? {
        list_rows::advance_generation(tx)?;
    }
    // New logical messages make the threads and the seen addresses due.
    if stats.new_messages > 0 {
        crate::threads::note_new_messages(tx)?;
        crate::seen::note_inputs(tx)?;
    }

    let state = if batch.caught_up {
        "synced"
    } else {
        "backfilling"
    };
    tx.execute(
        "UPDATE mailboxes SET uidnext = ?2, sync_state = ?3, revision = revision + 1
         WHERE mailbox_id = ?1",
        params![batch.mailbox_id, i64::from(batch.next_uidnext), state],
    )
    .map_err(|e| classify(e, "commit a header checkpoint"))?;
    Ok(stats)
}

/// Check the batch's shape and charge its text against the budget
/// before any write; the rest of the budget is left for list rows.
fn preflight(batch: &HeaderBatch<'_>) -> Result<usize, StoreError> {
    if batch.headers.len() > MAX_HEADER_BATCH {
        return Err(invalid(&format!(
            "a header batch holds at most {MAX_HEADER_BATCH} headers"
        )));
    }
    if batch.uidvalidity == 0 {
        return Err(invalid("UIDVALIDITY is never zero"));
    }
    let mut uids = HashSet::with_capacity(batch.headers.len());
    let mut charged: usize = 0;
    for header in batch.headers {
        if header.uid == 0 || !uids.insert(header.uid) {
            return Err(invalid("a header batch's UIDs are nonzero and distinct"));
        }
        if header.uid >= batch.next_uidnext {
            return Err(invalid("every UID in a batch lies below its checkpoint"));
        }
        let keywords: usize = header.flags.keywords.iter().map(String::len).sum();
        charged = charged
            .saturating_add(HEADER_ROW_OVERHEAD)
            .saturating_add(header.raw_fields.len())
            .saturating_add(header.columns.text_bytes())
            .saturating_add(header.provider_key.as_deref().map_or(0, str::len))
            .saturating_add(keywords);
    }
    MAX_HEADER_BATCH_BYTES
        .checked_sub(charged)
        .ok_or_else(|| invalid("a header batch exceeds the write budget"))
}

/// The mailbox must be the account's, syncing, in the batch's epoch,
/// and still at the checkpoint the fetch was planned from. Answers
/// the stored `uidnext`.
fn guard_mailbox(tx: &Transaction, batch: &HeaderBatch<'_>) -> Result<Option<i64>, StoreError> {
    let (uidvalidity, uidnext, state): (Option<i64>, Option<i64>, String) = tx
        .query_row(
            "SELECT uidvalidity, uidnext, sync_state FROM mailboxes
             WHERE mailbox_id = ?1 AND account_id = ?2",
            params![batch.mailbox_id, batch.account_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .optional()
        .map_err(|e| classify(e, "read a syncing mailbox"))?
        .ok_or_else(|| StoreError::not_found(format!("mailbox {}", batch.mailbox_id)))?;
    if !SYNCING_STATES.contains(&state.as_str()) {
        return Err(StoreError::conflict(format!(
            "mailbox {} is {state}, not syncing",
            batch.mailbox_id
        )));
    }
    if uidvalidity != Some(i64::from(batch.uidvalidity)) {
        return Err(StoreError::conflict(
            "the batch's epoch is not the mailbox's",
        ));
    }
    if uidnext != batch.expected_uidnext.map(i64::from) {
        return Err(StoreError::conflict(
            "the mailbox's checkpoint moved since the fetch was planned",
        ));
    }
    Ok(uidnext)
}

/// The current occurrence at a locator, with its message's provider key.
struct Bound {
    occurrence_id: i64,
    provider_key: Option<String>,
}

fn store_header(
    tx: &Transaction,
    batch: &HeaderBatch<'_>,
    header: &SyncedHeader,
    now: Timestamp,
    stats: &mut HeaderBatchStats,
    changed: &mut BTreeSet<i64>,
) -> Result<(), StoreError> {
    let uidvalidity = i64::from(batch.uidvalidity);
    let uid = i64::from(header.uid);
    let bound: Option<Bound> = tx
        .query_row(
            "SELECT o.occurrence_id, lm.provider_key
             FROM occurrences o JOIN logical_messages lm ON lm.message_id = o.message_id
             WHERE o.mailbox_id = ?1 AND o.uidvalidity = ?2 AND o.uid = ?3
               AND o.state IN ('present', 'vanish_pending')",
            params![batch.mailbox_id, uidvalidity, uid],
            |row| {
                Ok(Bound {
                    occurrence_id: row.get(0)?,
                    provider_key: row.get(1)?,
                })
            },
        )
        .optional()
        .map_err(|e| classify(e, "read a locator's occurrence"))?;

    if let Some(bound) = bound {
        // A UID reused for another provider key (spec/02): the old
        // binding is superseded, never rewritten.
        let reused = matches!(
            (&bound.provider_key, &header.provider_key),
            (Some(old), Some(new)) if old != new
        );
        if !reused {
            refresh_occurrence(tx, bound.occurrence_id, header, now)?;
            changed.insert(bound.occurrence_id);
            stats.refreshed += 1;
            return Ok(());
        }
        tx.execute(
            "UPDATE occurrences SET state = 'superseded', last_seen_at = ?2
             WHERE occurrence_id = ?1",
            params![bound.occurrence_id, now.as_millis()],
        )
        .map_err(|e| classify(e, "supersede a reused UID"))?;
        changed.insert(bound.occurrence_id);
        stats.superseded += 1;
    }

    let message_id = find_or_create_message(tx, batch.account_id, header, now, stats)?;
    let occurrence_id = insert_occurrence(tx, batch, header, message_id, now)?;
    changed.insert(occurrence_id);
    stats.inserted += 1;
    Ok(())
}

/// The logical message the header's provider key names, or a new one.
/// Headers are stored once per logical message: the first fetch wins,
/// and the body's arrival refreshes them (spec/02).
fn find_or_create_message(
    tx: &Transaction,
    account_id: &str,
    header: &SyncedHeader,
    now: Timestamp,
    stats: &mut HeaderBatchStats,
) -> Result<i64, StoreError> {
    if let Some(key) = &header.provider_key {
        let existing: Option<i64> = tx
            .query_row(
                "SELECT message_id FROM logical_messages
                 WHERE account_id = ?1 AND provider_key = ?2",
                params![account_id, key],
                |row| row.get(0),
            )
            .optional()
            .map_err(|e| classify(e, "find a logical message by provider key"))?;
        if let Some(message_id) = existing {
            return Ok(message_id);
        }
    }
    tx.execute(
        "INSERT INTO logical_messages (account_id, provider_key, created_at)
         VALUES (?1, ?2, ?3)",
        params![account_id, header.provider_key, now.as_millis()],
    )
    .map_err(|e| classify(e, "create a logical message"))?;
    let message_id = tx.last_insert_rowid();
    stats.new_messages += 1;
    let columns = &header.columns;
    tx.execute(
        "INSERT INTO message_headers
         (message_id, raw_fields, header_message_id, subject, from_json, reply_to_json,
          to_json, cc_json, delivered_to_json, date_header, in_reply_to, references_json,
          list_id, fetched_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)",
        params![
            message_id,
            header.raw_fields,
            columns.header_message_id,
            columns.subject,
            columns.from_json,
            columns.reply_to_json,
            columns.to_json,
            columns.cc_json,
            columns.delivered_to_json,
            columns.date_header,
            columns.in_reply_to,
            columns.references_json,
            columns.list_id,
            now.as_millis(),
        ],
    )
    .map_err(|e| classify(e, "store a message's headers"))?;
    Ok(message_id)
}

fn insert_occurrence(
    tx: &Transaction,
    batch: &HeaderBatch<'_>,
    header: &SyncedHeader,
    message_id: i64,
    now: Timestamp,
) -> Result<i64, StoreError> {
    let flags = &header.flags;
    tx.execute(
        "INSERT INTO occurrences
         (account_id, mailbox_id, uidvalidity, uid, message_id, internal_date, rfc822_size,
          f_seen, f_flagged, f_answered, f_draft, f_deleted, keywords, state,
          first_seen_at, last_seen_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, 'present', ?14, ?14)",
        params![
            batch.account_id,
            batch.mailbox_id,
            i64::from(batch.uidvalidity),
            i64::from(header.uid),
            message_id,
            header.internal_date.map(|date| date.as_millis()),
            size(header.rfc822_size)?,
            flags.seen,
            flags.flagged,
            flags.answered,
            flags.draft,
            flags.deleted,
            keywords_json(&flags.keywords),
            now.as_millis(),
        ],
    )
    .map_err(|e| classify(e, "insert an occurrence"))?;
    Ok(tx.last_insert_rowid())
}

/// The same message seen again at its locator: flags and fetch facts
/// follow the server; the binding and `first_seen_at` stay.
fn refresh_occurrence(
    tx: &Transaction,
    occurrence_id: i64,
    header: &SyncedHeader,
    now: Timestamp,
) -> Result<(), StoreError> {
    let flags = &header.flags;
    tx.execute(
        "UPDATE occurrences
         SET f_seen = ?2, f_flagged = ?3, f_answered = ?4, f_draft = ?5, f_deleted = ?6,
             keywords = ?7, internal_date = COALESCE(?8, internal_date),
             rfc822_size = COALESCE(?9, rfc822_size), last_seen_at = ?10
         WHERE occurrence_id = ?1",
        params![
            occurrence_id,
            flags.seen,
            flags.flagged,
            flags.answered,
            flags.draft,
            flags.deleted,
            keywords_json(&flags.keywords),
            header.internal_date.map(|date| date.as_millis()),
            size(header.rfc822_size)?,
            now.as_millis(),
        ],
    )
    .map_err(|e| classify(e, "refresh an occurrence"))?;
    Ok(())
}

/// `occurrences.keywords`: a canonical JSON array, sorted (spec/02).
fn keywords_json(keywords: &[String]) -> String {
    let canonical: BTreeSet<&str> = keywords.iter().map(String::as_str).collect();
    serde_json::Value::from(canonical.into_iter().collect::<Vec<_>>()).to_string()
}

fn size(rfc822_size: Option<u64>) -> Result<Option<i64>, StoreError> {
    rfc822_size
        .map(|size| {
            i64::try_from(size).map_err(|_| invalid("an RFC822.SIZE exceeds the store's range"))
        })
        .transpose()
}

fn invalid(message: &str) -> StoreError {
    StoreError::InvalidInput {
        message: message.to_owned(),
    }
}
