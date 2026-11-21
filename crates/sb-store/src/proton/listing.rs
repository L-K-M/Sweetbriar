//! Proton bootstrap writes (spec/04-sync-proton "Mailbox mapping",
//! "Sync loop" step 1, "Coverage"; spec/02 "Proton accounts"): labels
//! reconciled into mailboxes by label id, each listed page committed
//! with its checkpoint, the listing finished with Proton's totals, and
//! the coverage those leave behind.
//!
//! A Proton message's occurrences have no UID: one current occurrence
//! per (mailbox, logical message), which this module keeps since no
//! index can (spec/02). The listed metadata is authoritative, so a
//! label the message no longer carries makes its occurrence `vanished`
//! and every current occurrence takes the message's flags.

use std::collections::{BTreeMap, BTreeSet, HashMap, HashSet};

use rusqlite::{OptionalExtension, Transaction, params};
use sb_types::time::Timestamp;

use super::{
    ProtonAccount, ProtonListingState, complete_listing, require_account, require_revision,
};
use crate::intents::held_flags;
use crate::lists::MailboxRole;
use crate::schema::classify;
use crate::sidebar::MailboxKind;
use crate::sync::{
    HeaderColumns, MAX_HEADER_BATCH, MAX_HEADER_BATCH_BYTES, Reconciled, SyncedFlags,
};
use crate::{StoreError, list_rows};

/// What `message_headers.raw_fields` starts with while it holds listed
/// metadata rather than a body's header block (spec/02).
const METADATA_PREFIX: &[u8] = br#"{"v":1,"proton_metadata":"#;

/// One label or system location as the `proton_api` policy mapped it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedLabel {
    /// The Proton label id, `mailboxes.provider_key`.
    pub provider_key: String,
    /// The display path, `mailboxes.remote_name`; unique per account.
    pub remote_name: String,
    pub delimiter: Option<char>,
    pub kind: MailboxKind,
    pub role: Option<MailboxRole>,
    pub selectable: bool,
}

/// One listed message's metadata, mapped by sb-resources.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListedMessage {
    /// The Proton message id, `logical_messages.provider_key`.
    pub provider_key: String,
    /// Every label the message carries; those without a mailbox (All
    /// Drafts, All Sent, Outbox) are ignored.
    pub label_ids: Vec<String>,
    /// `{"v":1,"proton_metadata":{…}}` as received (spec/02).
    pub raw_metadata: Vec<u8>,
    pub columns: HeaderColumns,
    pub internal_date: Option<Timestamp>,
    pub size: Option<u64>,
    /// The message's flags, equal on every occurrence (spec/02).
    pub flags: SyncedFlags,
}

/// One page of the bootstrap listing and the checkpoint it reaches.
#[derive(Debug, Clone, Copy)]
pub struct ListedPage<'a> {
    pub account_id: &'a str,
    pub messages: &'a [ListedMessage],
    /// The position after this page, `proton_accounts.listing_checkpoint`.
    pub checkpoint: &'a str,
    /// The account row's revision the page was listed under.
    pub expected_revision: i64,
}

/// What one page changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ListedPageStats {
    pub new_messages: usize,
    pub inserted: usize,
    pub refreshed: usize,
    pub vanished: usize,
}

/// One mailbox's local count against Proton's total.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailboxCoverage {
    pub mailbox_id: i64,
    pub remote_name: String,
    /// Present occurrences in the mailbox.
    pub local: u64,
    /// Proton's total when the last listing finished; `None` before.
    pub remote_total: Option<u64>,
}

/// spec/04-sync-proton "Coverage": "Proton messages synced".
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtonCoverage {
    pub listing: ProtonListingState,
    /// Live mailboxes, by name.
    pub mailboxes: Vec<MailboxCoverage>,
    pub messages: u64,
    pub with_body: u64,
    pub body_failures: u64,
}

/// Reconcile the account's mailboxes with the labels the policy
/// mapped: new labels are added as `new`, missing ones become `gone`
/// (their occurrences stay), and a renamed label keeps its mailbox.
/// Names swap freely within one reconciliation; a `gone` mailbox whose
/// name a new label takes is renamed aside with its label id.
pub fn reconcile_proton_labels(
    tx: &Transaction,
    account_id: &str,
    observed: &[ObservedLabel],
) -> Result<Reconciled, StoreError> {
    require_account(tx, account_id)?;
    let mut keys = HashSet::new();
    let mut names = HashSet::new();
    for label in observed {
        if !keys.insert(label.provider_key.as_str()) || !names.insert(label.remote_name.as_str()) {
            return Err(invalid("a label id or mailbox name is listed twice"));
        }
    }
    let wanted: HashMap<&str, &ObservedLabel> = observed
        .iter()
        .map(|label| (label.provider_key.as_str(), label))
        .collect();
    let stored = stored_labels(tx, account_id)?;

    // Free every name that moves before any row takes one, so swaps
    // and reuse never trip UNIQUE (account_id, remote_name).
    let mut displaced = BTreeSet::new();
    for row in stored.values() {
        let target = wanted
            .get(row.provider_key.as_str())
            .map(|label| label.remote_name.as_str());
        if target != Some(row.remote_name.as_str()) && names.contains(row.remote_name.as_str()) {
            set_name(tx, row.mailbox_id, &format!("\u{0}{}", row.mailbox_id))?;
            displaced.insert(row.mailbox_id);
        }
    }

    let mut reconciled = Reconciled::default();
    for label in observed {
        let Some(row) = stored.get(&label.provider_key) else {
            reconciled.added.push(insert_label(tx, account_id, label)?);
            continue;
        };
        let returning = row.sync_state == "gone";
        let columns_changed = row.delimiter != label.delimiter.map(String::from)
            || row.role.as_deref() != label.role.map(MailboxRole::as_str)
            || row.kind != label.kind.as_str()
            || row.selectable != label.selectable;
        let renamed = row.remote_name != label.remote_name;
        if !(returning || columns_changed || renamed) {
            continue;
        }
        tx.execute(
            "UPDATE mailboxes
             SET remote_name = ?2, delimiter = ?3, role = ?4, kind = ?5, selectable = ?6,
                 sync_state = CASE WHEN sync_state = 'gone' THEN 'new' ELSE sync_state END,
                 revision = revision + 1
             WHERE mailbox_id = ?1",
            params![
                row.mailbox_id,
                label.remote_name,
                label.delimiter.map(String::from),
                label.role.map(MailboxRole::as_str),
                label.kind.as_str(),
                label.selectable,
            ],
        )
        .map_err(|e| classify(e, "update a Proton mailbox"))?;
        if returning {
            reconciled.returned.push(row.mailbox_id);
        }
        if columns_changed || renamed {
            reconciled.changed.push(row.mailbox_id);
        }
    }

    for row in stored
        .values()
        .filter(|row| !wanted.contains_key(row.provider_key.as_str()))
    {
        if displaced.contains(&row.mailbox_id) {
            set_name(
                tx,
                row.mailbox_id,
                &format!("{} ({})", row.remote_name, row.provider_key),
            )?;
        }
        if row.sync_state != "gone" {
            tx.execute(
                "UPDATE mailboxes SET sync_state = 'gone', revision = revision + 1
                 WHERE mailbox_id = ?1",
                params![row.mailbox_id],
            )
            .map_err(|e| classify(e, "mark a Proton mailbox gone"))?;
            reconciled.gone.push(row.mailbox_id);
        }
    }

    for list in [
        &mut reconciled.added,
        &mut reconciled.changed,
        &mut reconciled.gone,
        &mut reconciled.returned,
    ] {
        list.sort_unstable();
    }
    if reconciled != Reconciled::default() || !displaced.is_empty() {
        list_rows::advance_generation(tx)?;
    }
    Ok(reconciled)
}

/// Commit one listed page with its checkpoint (spec/04-sync-proton
/// "Sync loop" step 1.3): each message found or created by its id,
/// its occurrences following its labels, its listed headers, and the
/// list rows, all in the caller's transaction. Only while listing, and
/// only under the revision the page was listed with.
pub fn commit_listed_page(
    tx: &Transaction,
    page: &ListedPage<'_>,
    now: Timestamp,
) -> Result<ListedPageStats, StoreError> {
    if page.messages.len() > MAX_HEADER_BATCH {
        return Err(invalid(&format!(
            "a listed page holds at most {MAX_HEADER_BATCH} messages"
        )));
    }
    let account = require_account(tx, page.account_id)?;
    require_revision(&account, page.expected_revision)?;
    if account.listing_state != ProtonListingState::Listing {
        return Err(StoreError::conflict("the Proton account is not listing"));
    }
    let mailboxes = live_mailboxes(tx, page.account_id)?;

    let mut remaining = MAX_HEADER_BATCH_BYTES;
    let mut stats = ListedPageStats::default();
    let mut changed = BTreeSet::new();
    let mut seen = HashSet::new();
    for message in page.messages {
        if !seen.insert(message.provider_key.as_str()) {
            return Err(invalid("a listed page names a message twice"));
        }
        remaining = remaining
            .checked_sub(message.raw_metadata.len() + 1024)
            .ok_or_else(|| invalid("a listed page exceeds the write budget"))?;
        let message_id = store_message(
            tx,
            page.account_id,
            message,
            &mailboxes,
            now,
            &mut stats,
            &mut changed,
        )?;
        super::refresh::note_listed(tx, page.account_id, message_id)?;
    }

    if list_rows::refresh_priced(tx, &changed, &mut |bytes| {
        remaining = remaining
            .checked_sub(bytes)
            .ok_or_else(|| invalid("a listed page's list rows exceed the write budget"))?;
        Ok(())
    })? {
        list_rows::advance_generation(tx)?;
    }
    if stats.new_messages > 0 {
        crate::threads::note_new_messages(tx)?;
        crate::seen::note_inputs(tx)?;
    }
    super::update_listing_checkpoint(tx, page.account_id, page.checkpoint, page.expected_revision)?;
    Ok(stats)
}

/// Finish the listing (spec/04-sync-proton "Sync loop" step 1.4):
/// Proton's per-label totals are recorded for coverage, the live
/// mailboxes are `synced`, what the listing missed goes to the breaker
/// ("Refresh" steps 3 and 4) once `withhold_unlisted` withheld it all,
/// and the pending cursor becomes the event cursor, all in the
/// caller's transaction.
pub fn finish_listing(
    tx: &Transaction,
    account_id: &str,
    totals: &[(String, u64)],
    expected_revision: i64,
    now: Timestamp,
) -> Result<ProtonAccount, StoreError> {
    let account = require_account(tx, account_id)?;
    require_revision(&account, expected_revision)?;
    let Some(cursor) = account.pending_cursor.clone() else {
        return Err(StoreError::conflict("the Proton account is not listing"));
    };
    tx.execute(
        "UPDATE mailboxes SET remote_total = NULL,
             sync_state = CASE WHEN sync_state = 'gone' THEN 'gone' ELSE 'synced' END
         WHERE account_id = ?1",
        params![account_id],
    )
    .map_err(|e| classify(e, "reset Proton totals"))?;
    for (label_id, total) in totals {
        let total = i64::try_from(*total)
            .map_err(|_| invalid("a Proton total exceeds the store's range"))?;
        tx.execute(
            "UPDATE mailboxes SET remote_total = ?3 WHERE account_id = ?1 AND provider_key = ?2",
            params![account_id, label_id, total],
        )
        .map_err(|e| classify(e, "record a Proton total"))?;
    }
    super::refresh::conclude(tx, account_id, &cursor, now)?;
    complete_listing(tx, account_id, &cursor, expected_revision)
}

/// The account's coverage (spec/04-sync-proton "Coverage").
pub fn proton_coverage(tx: &Transaction, account_id: &str) -> Result<ProtonCoverage, StoreError> {
    let account = require_account(tx, account_id)?;
    let read = |e| classify(e, "read Proton coverage");
    let mut statement = tx
        .prepare(
            "SELECT m.mailbox_id, m.remote_name, m.remote_total,
                    (SELECT COUNT(*) FROM occurrences o
                     WHERE o.mailbox_id = m.mailbox_id AND o.state = 'present')
             FROM mailboxes m
             WHERE m.account_id = ?1 AND m.sync_state <> 'gone'
             ORDER BY m.remote_name",
        )
        .map_err(read)?;
    let mailboxes = statement
        .query_map(params![account_id], |row| {
            Ok(MailboxCoverage {
                mailbox_id: row.get(0)?,
                remote_name: row.get(1)?,
                remote_total: row.get::<_, Option<i64>>(2)?.map(count),
                local: count(row.get(3)?),
            })
        })
        .map_err(read)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(read)?;
    let (messages, with_body, body_failures): (i64, i64, i64) = tx
        .query_row(
            "SELECT COUNT(*), COUNT(l.acquisition_id),
                    (SELECT COUNT(*) FROM proton_body_failures f
                     JOIN logical_messages fl ON fl.message_id = f.message_id
                     WHERE fl.account_id = ?1)
             FROM logical_messages l WHERE l.account_id = ?1",
            params![account_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )
        .map_err(read)?;
    Ok(ProtonCoverage {
        listing: account.listing_state,
        mailboxes,
        messages: count(messages),
        with_body: count(with_body),
        body_failures: count(body_failures),
    })
}

struct StoredLabel {
    mailbox_id: i64,
    provider_key: String,
    remote_name: String,
    delimiter: Option<String>,
    role: Option<String>,
    kind: String,
    selectable: bool,
    sync_state: String,
}

fn stored_labels(
    tx: &Transaction,
    account_id: &str,
) -> Result<BTreeMap<String, StoredLabel>, StoreError> {
    let read = |e| classify(e, "read the account's Proton mailboxes");
    let mut statement = tx
        .prepare(
            "SELECT mailbox_id, provider_key, remote_name, delimiter, role, kind, selectable,
                    sync_state
             FROM mailboxes WHERE account_id = ?1 AND provider_key IS NOT NULL",
        )
        .map_err(read)?;
    let rows = statement
        .query_map(params![account_id], |row| {
            Ok(StoredLabel {
                mailbox_id: row.get(0)?,
                provider_key: row.get(1)?,
                remote_name: row.get(2)?,
                delimiter: row.get(3)?,
                role: row.get(4)?,
                kind: row.get(5)?,
                selectable: row.get(6)?,
                sync_state: row.get(7)?,
            })
        })
        .map_err(read)?;
    rows.map(|row| row.map(|row| (row.provider_key.clone(), row)))
        .collect::<Result<BTreeMap<_, _>, _>>()
        .map_err(read)
}

fn set_name(tx: &Transaction, mailbox_id: i64, name: &str) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE mailboxes SET remote_name = ?2, revision = revision + 1 WHERE mailbox_id = ?1",
        params![mailbox_id, name],
    )
    .map_err(|e| classify(e, "rename a Proton mailbox"))?;
    Ok(())
}

fn insert_label(
    tx: &Transaction,
    account_id: &str,
    label: &ObservedLabel,
) -> Result<i64, StoreError> {
    tx.execute(
        "INSERT INTO mailboxes
         (account_id, remote_name, delimiter, role, kind, selectable, sync_state, provider_key)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, 'new', ?7)",
        params![
            account_id,
            label.remote_name,
            label.delimiter.map(String::from),
            label.role.map(MailboxRole::as_str),
            label.kind.as_str(),
            label.selectable,
            label.provider_key,
        ],
    )
    .map_err(|e| classify(e, "insert a Proton mailbox"))?;
    Ok(tx.last_insert_rowid())
}

/// Label id → mailbox for the account's live mailboxes.
pub(super) fn live_mailboxes(
    tx: &Transaction,
    account_id: &str,
) -> Result<HashMap<String, i64>, StoreError> {
    let read = |e| classify(e, "read the account's live Proton mailboxes");
    let mut statement = tx
        .prepare(
            "SELECT provider_key, mailbox_id FROM mailboxes
             WHERE account_id = ?1 AND provider_key IS NOT NULL AND sync_state <> 'gone'",
        )
        .map_err(read)?;
    let rows = statement
        .query_map(params![account_id], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(read)?;
    rows.collect::<Result<HashMap<_, _>, _>>().map_err(read)
}

pub(super) fn store_message(
    tx: &Transaction,
    account_id: &str,
    message: &ListedMessage,
    mailboxes: &HashMap<String, i64>,
    now: Timestamp,
    stats: &mut ListedPageStats,
    changed: &mut BTreeSet<i64>,
) -> Result<i64, StoreError> {
    require_canonical_metadata(message)?;
    let message_id = find_or_create_message(tx, account_id, message, now, stats)?;
    super::breaker::reappeared(tx, message_id, now)?;
    let mut writes = OccurrenceWrites { stats, changed };
    store_occurrences(
        tx,
        message_id,
        account_id,
        message,
        mailboxes,
        now,
        &mut writes,
    )
}

/// `store_message` without the breaker's reappearance: a message's
/// re-read metadata after a refused body updates its occurrences but
/// leaves `proton_disappearances` alone (plan M2.16d).
pub(super) fn store_reread_message(
    tx: &Transaction,
    account_id: &str,
    message: &ListedMessage,
    mailboxes: &HashMap<String, i64>,
    now: Timestamp,
    stats: &mut ListedPageStats,
    changed: &mut BTreeSet<i64>,
) -> Result<i64, StoreError> {
    require_canonical_metadata(message)?;
    let message_id = find_or_create_message(tx, account_id, message, now, stats)?;
    let mut writes = OccurrenceWrites { stats, changed };
    store_occurrences(
        tx,
        message_id,
        account_id,
        message,
        mailboxes,
        now,
        &mut writes,
    )
}

/// Checked before the message's first write, so invalid metadata
/// touches nothing.
fn require_canonical_metadata(message: &ListedMessage) -> Result<(), StoreError> {
    if !message.raw_metadata.starts_with(METADATA_PREFIX) {
        return Err(invalid(
            "listed metadata must be the canonical proton_metadata JSON",
        ));
    }
    Ok(())
}

/// The writes one message's occurrences make: the page's stats and
/// the list rows its occurrences changed.
struct OccurrenceWrites<'a> {
    stats: &'a mut ListedPageStats,
    changed: &'a mut BTreeSet<i64>,
}

/// The occurrence half of [`store_message`]: the message's labels
/// become its current occurrences, a lost label's occurrence becomes
/// `vanished`, and its headers and list rows follow.
fn store_occurrences(
    tx: &Transaction,
    message_id: i64,
    account_id: &str,
    message: &ListedMessage,
    mailboxes: &HashMap<String, i64>,
    now: Timestamp,
    writes: &mut OccurrenceWrites<'_>,
) -> Result<i64, StoreError> {
    let stats = &mut *writes.stats;
    let changed = &mut *writes.changed;
    let wanted: BTreeSet<i64> = message
        .label_ids
        .iter()
        .filter_map(|label| mailboxes.get(label).copied())
        .collect();
    let live: HashSet<i64> = mailboxes.values().copied().collect();

    // A live flag intent's value stays until the intent is done.
    let flags = held_flags(tx, account_id, &message.provider_key)?.over(&message.flags);
    let current = current_occurrences(tx, message_id)?;
    let mut present = BTreeSet::new();
    for (occurrence_id, mailbox_id) in current {
        if wanted.contains(&mailbox_id) && present.insert(mailbox_id) {
            refresh_occurrence(tx, occurrence_id, message, &flags, now)?;
            stats.refreshed += 1;
        } else if live.contains(&mailbox_id) {
            // A label the message lost, or a second current occurrence
            // the invariant forbids: authoritative, not an inference.
            tx.execute(
                "UPDATE occurrences SET state = 'vanished' WHERE occurrence_id = ?1",
                params![occurrence_id],
            )
            .map_err(|e| classify(e, "vanish a Proton occurrence"))?;
            stats.vanished += 1;
        } else {
            continue;
        }
        changed.insert(occurrence_id);
    }
    for mailbox_id in wanted.difference(&present) {
        changed.insert(insert_occurrence(
            tx,
            account_id,
            *mailbox_id,
            message_id,
            message,
            &flags,
            now,
        )?);
        stats.inserted += 1;
    }
    Ok(message_id)
}

/// The logical message the Proton id names, or a new one. Listed
/// metadata replaces listed metadata; a body's header block, once
/// stored, is kept (spec/02 "Headers before bodies").
fn find_or_create_message(
    tx: &Transaction,
    account_id: &str,
    message: &ListedMessage,
    now: Timestamp,
    stats: &mut ListedPageStats,
) -> Result<i64, StoreError> {
    let existing: Option<i64> = tx
        .query_row(
            "SELECT message_id FROM logical_messages WHERE account_id = ?1 AND provider_key = ?2",
            params![account_id, message.provider_key],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| classify(e, "find a Proton message"))?;
    let message_id = match existing {
        Some(message_id) => {
            let stored: Option<Vec<u8>> = tx
                .query_row(
                    "SELECT raw_fields FROM message_headers WHERE message_id = ?1",
                    params![message_id],
                    |row| row.get(0),
                )
                .optional()
                .map_err(|e| classify(e, "read a Proton message's headers"))?;
            if stored.is_some_and(|raw| !raw.starts_with(METADATA_PREFIX)) {
                return Ok(message_id);
            }
            message_id
        }
        None => {
            tx.execute(
                "INSERT INTO logical_messages (account_id, provider_key, created_at) VALUES (?1, ?2, ?3)",
                params![account_id, message.provider_key, now.as_millis()],
            )
            .map_err(|e| classify(e, "create a Proton message"))?;
            stats.new_messages += 1;
            tx.last_insert_rowid()
        }
    };
    store_headers(tx, message_id, &message.raw_metadata, &message.columns, now)?;
    Ok(message_id)
}

/// Store one message's header literal and its parsed columns, replacing
/// any earlier row (spec/02 "Headers before bodies"): listed metadata
/// over listed metadata, and a body's stored header block last.
pub(super) fn store_headers(
    tx: &Transaction,
    message_id: i64,
    raw_fields: &[u8],
    columns: &HeaderColumns,
    now: Timestamp,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO message_headers
         (message_id, raw_fields, header_message_id, subject, from_json, reply_to_json,
          to_json, cc_json, delivered_to_json, date_header, in_reply_to, references_json,
          list_id, fetched_at)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14)
         ON CONFLICT (message_id) DO UPDATE SET
           raw_fields = excluded.raw_fields, header_message_id = excluded.header_message_id,
           subject = excluded.subject, from_json = excluded.from_json,
           reply_to_json = excluded.reply_to_json, to_json = excluded.to_json,
           cc_json = excluded.cc_json, delivered_to_json = excluded.delivered_to_json,
           date_header = excluded.date_header, in_reply_to = excluded.in_reply_to,
           references_json = excluded.references_json, list_id = excluded.list_id,
           fetched_at = excluded.fetched_at",
        params![
            message_id,
            raw_fields,
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
    .map_err(|e| classify(e, "store a Proton message's headers"))?;
    Ok(())
}

/// The message's current occurrences, oldest first, so a duplicate the
/// invariant forbids is the later one.
pub(super) fn current_occurrences(
    tx: &Transaction,
    message_id: i64,
) -> Result<Vec<(i64, i64)>, StoreError> {
    let read = |e| classify(e, "read a Proton message's occurrences");
    let mut statement = tx
        .prepare(
            "SELECT occurrence_id, mailbox_id FROM occurrences
             WHERE message_id = ?1 AND state IN ('present', 'vanish_pending')
             ORDER BY occurrence_id",
        )
        .map_err(read)?;
    let rows = statement
        .query_map(params![message_id], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(read)?;
    rows.collect::<Result<Vec<_>, _>>().map_err(read)
}

fn insert_occurrence(
    tx: &Transaction,
    account_id: &str,
    mailbox_id: i64,
    message_id: i64,
    message: &ListedMessage,
    flags: &SyncedFlags,
    now: Timestamp,
) -> Result<i64, StoreError> {
    tx.execute(
        "INSERT INTO occurrences
         (account_id, mailbox_id, uidvalidity, uid, message_id, internal_date, rfc822_size,
          f_seen, f_flagged, f_answered, f_draft, f_deleted, keywords, state,
          first_seen_at, last_seen_at)
         VALUES (?1, ?2, NULL, NULL, ?3, ?4, ?5, ?6, ?7, ?8, ?9, 0, ?10, 'present', ?11, ?11)",
        params![
            account_id,
            mailbox_id,
            message_id,
            message.internal_date.map(|date| date.as_millis()),
            size(message.size)?,
            flags.seen,
            flags.flagged,
            flags.answered,
            flags.draft,
            keywords_json(&flags.keywords),
            now.as_millis(),
        ],
    )
    .map_err(|e| classify(e, "insert a Proton occurrence"))?;
    Ok(tx.last_insert_rowid())
}

fn refresh_occurrence(
    tx: &Transaction,
    occurrence_id: i64,
    message: &ListedMessage,
    flags: &SyncedFlags,
    now: Timestamp,
) -> Result<(), StoreError> {
    tx.execute(
        "UPDATE occurrences
         SET internal_date = ?2, rfc822_size = ?3, f_seen = ?4, f_flagged = ?5,
             f_answered = ?6, f_draft = ?7, keywords = ?8, state = 'present', last_seen_at = ?9
         WHERE occurrence_id = ?1",
        params![
            occurrence_id,
            message.internal_date.map(|date| date.as_millis()),
            size(message.size)?,
            flags.seen,
            flags.flagged,
            flags.answered,
            flags.draft,
            keywords_json(&flags.keywords),
            now.as_millis(),
        ],
    )
    .map_err(|e| classify(e, "refresh a Proton occurrence"))?;
    Ok(())
}

fn keywords_json(keywords: &[String]) -> String {
    let canonical: BTreeSet<&str> = keywords.iter().map(String::as_str).collect();
    serde_json::Value::from(canonical.into_iter().collect::<Vec<_>>()).to_string()
}

fn size(size: Option<u64>) -> Result<Option<i64>, StoreError> {
    size.map(|size| {
        i64::try_from(size).map_err(|_| invalid("a Proton message size exceeds the store's range"))
    })
    .transpose()
}

fn count(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

pub(super) fn invalid(message: &str) -> StoreError {
    StoreError::InvalidInput {
        message: message.to_owned(),
    }
}

#[cfg(test)]
mod tests;
