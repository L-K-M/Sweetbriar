//! One sync pass over a live session (spec/04 "Sync loop" steps 1–3):
//! LIST reconciles the account's mailboxes through its policy; then,
//! for every selectable mailbox, EXAMINE checks the epoch, the
//! complete UID list names the UIDs without a current occurrence (new
//! ones above the checkpoint, and ones Bridge materialized late below
//! it), and their headers arrive in batches of at most 250 UIDs, each
//! committed with its `uidnext` checkpoint in one transaction.
//!
//! The transport's failures end the pass for the caller's reconnect
//! path. A command the server refuses for one mailbox (a tagged NO or
//! BAD on its EXAMINE, UID SEARCH or UID FETCH) ends the session too,
//! as sb-imap drops it, but names the mailbox: the caller skips it for
//! a while and reconnects at once, so one refused mailbox never
//! starves the rest. A store failure only skips what it concerns: the
//! next pass starts over from the committed checkpoints, so nothing is
//! lost or stored twice.

use std::collections::{HashMap, HashSet, VecDeque};
use std::num::NonZeroU32;
use std::sync::Arc;
use std::time::{Duration, Instant};

use sb_resources::sync::{
    EpochCheck, HeaderBatch, MAX_HEADER_BATCH, ObservedMailbox, SyncTarget, SyncedFlags,
    SyncedHeader, check_epoch, commit_header_batch, header_columns, mailbox_cursor,
    reconcile_mailboxes, sync_targets,
};
use sb_resources::{
    Connection, HeaderBytes, HeaderFetchMode, HeaderRequest, HeaderRow, ImapError, StoreError,
    Timestamp, WritePriority,
};

use crate::engine::Worker;
use crate::policy::{ListedMailbox, Policy};

/// spec/04 step 3's header fields.
const HEADER_FIELDS: [&str; 13] = [
    "DATE",
    "FROM",
    "REPLY-TO",
    "TO",
    "CC",
    "SUBJECT",
    "MESSAGE-ID",
    "IN-REPLY-TO",
    "REFERENCES",
    "LIST-ID",
    "DELIVERED-TO",
    "X-ORIGINAL-TO",
    "X-PM-INTERNAL-ID",
];

/// How long a mailbox the server refused is left out of passes.
pub(super) const REFUSAL_COOLDOWN: Duration = Duration::from_secs(15 * 60);

/// Why a pass ended early. Either way the session is gone.
#[derive(Debug)]
pub(super) enum PassError {
    /// The session failed; the caller's recovery reconnects.
    Session(ImapError),
    /// The server refused a command for this mailbox only.
    Refused { mailbox_id: i64 },
}

impl From<ImapError> for PassError {
    fn from(error: ImapError) -> Self {
        Self::Session(error)
    }
}

/// Mailboxes the server refused, each left out until its cooldown ends.
#[derive(Debug, Default)]
pub(super) struct Refusals {
    until: HashMap<i64, Instant>,
}

impl Refusals {
    /// Leave `mailbox_id` out of passes for `REFUSAL_COOLDOWN` from `now`.
    pub(super) fn refuse(&mut self, mailbox_id: i64, now: Instant) {
        self.until.insert(mailbox_id, now + REFUSAL_COOLDOWN);
    }

    /// Whether `mailbox_id` is still left out at `now`; forgets expired
    /// refusals.
    fn skips(&mut self, mailbox_id: i64, now: Instant) -> bool {
        self.until.retain(|_, until| now < *until);
        self.until.contains_key(&mailbox_id)
    }
}

/// Run one pass, leaving out the mailboxes in `refusals`.
pub(super) async fn run(
    worker: &Worker,
    account_id: &str,
    policy: Policy,
    connection: &mut Connection,
    refusals: &mut Refusals,
) -> Result<(), PassError> {
    let listing = connection.list_mailboxes().await?.observation;
    let observed: Vec<ObservedMailbox> = listing
        .iter()
        .map(|summary| {
            let mapping = policy.map_mailbox(&ListedMailbox {
                name: &summary.name,
                delimiter: summary.delimiter,
                attributes: &summary.attributes,
            });
            ObservedMailbox {
                remote_name: summary.name.clone(),
                delimiter: summary.delimiter,
                kind: mapping.kind,
                role: mapping.role,
                selectable: mapping.selectable,
            }
        })
        .collect();
    let id = account_id.to_owned();
    let targets = worker
        .resources()
        .write(WritePriority::Sync, move |tx| {
            reconcile_mailboxes(tx, &id, &observed)?;
            sync_targets(tx, &id)
        })
        .await;
    // Without the reconciled mailboxes there is nothing safe to fetch
    // into; the next pass lists again.
    let Ok(targets) = targets else {
        return Ok(());
    };
    for target in targets {
        if refusals.skips(target.mailbox_id, Instant::now()) {
            continue;
        }
        sync_mailbox(worker, account_id, policy, connection, &target)
            .await
            .map_err(|error| match error {
                ImapError::Command { .. } => PassError::Refused {
                    mailbox_id: target.mailbox_id,
                },
                error => PassError::Session(error),
            })?;
    }
    Ok(())
}

/// Steps 2 and 3 for one mailbox.
async fn sync_mailbox(
    worker: &Worker,
    account_id: &str,
    policy: Policy,
    connection: &mut Connection,
    target: &SyncTarget,
) -> Result<(), ImapError> {
    // Switching selections takes UNSELECT, which Bridge advertises; a
    // server without it ends the pass (`FreshConnectionRequired`), so
    // the generic policy will need a session per mailbox.
    let selection = connection.examine(&target.remote_name).await?.observation;
    let uidvalidity = selection.uid_validity.get();
    let id = account_id.to_owned();
    let mailbox_id = target.mailbox_id;
    let cursor = worker
        .resources()
        .write(WritePriority::Sync, move |tx| {
            match check_epoch(tx, &id, mailbox_id, uidvalidity)? {
                // The epoch-change step (plan M2.5) resolves it; until
                // then the mailbox is skipped.
                EpochCheck::Changed => Ok(None),
                EpochCheck::Adopted | EpochCheck::Unchanged => {
                    mailbox_cursor(tx, &id, mailbox_id, uidvalidity).map(Some)
                }
            }
        })
        .await;
    let Ok(Some(cursor)) = cursor else {
        return Ok(());
    };

    let inventory = connection.uid_inventory().await?.observation;
    let known: HashSet<u32> = cursor.current_uids.into_iter().collect();
    // A UID of u32::MAX has no checkpoint above it to commit with; it
    // stays unfetched and the mailbox `backfilling`.
    let mut complete = true;
    let wanted: Vec<NonZeroU32> = inventory
        .uids
        .into_iter()
        .filter(|uid| !known.contains(&uid.get()))
        .filter(|uid| {
            let storable = uid.get() < u32::MAX;
            complete &= storable;
            storable
        })
        .collect();

    let mut checkpoint = Checkpoint {
        account_id,
        mailbox_id,
        uidvalidity,
        stored: cursor.uidnext,
        server_next: selection.uid_next.map(NonZeroU32::get),
    };
    if wanted.is_empty() {
        // Only the checkpoint moves: to the server's UIDNEXT, and the
        // mailbox becomes `synced` when nothing was left out.
        if Some(checkpoint.target(None, true)) != checkpoint.stored || cursor.synced != complete {
            let _ = checkpoint
                .commit(worker, None, true, complete, Arc::default())
                .await;
        }
        return Ok(());
    }

    let chunks = wanted.chunks(MAX_HEADER_BATCH).count();
    for (index, chunk) in wanted.chunks(MAX_HEADER_BATCH).enumerate() {
        let request = HeaderRequest::new(
            chunk.to_vec(),
            HEADER_FIELDS
                .iter()
                .map(|&field| field.to_owned())
                .collect(),
            HeaderFetchMode::Full,
        )
        .expect("a sync batch is a valid header request");
        let fetched = connection.fetch_headers(&request).await?.observation;
        let headers: Vec<SyncedHeader> = fetched
            .rows
            .iter()
            .filter_map(|row| synced_header(policy, row))
            .collect();
        complete &= headers.len() == chunk.len();
        let last_chunk = index + 1 == chunks;
        let highest = chunk.last().map(|uid| uid.get());
        let committed = commit_chunk(
            worker,
            &mut checkpoint,
            headers,
            highest,
            last_chunk,
            &mut complete,
        )
        .await;
        if !committed {
            // The store refused (the mailbox left syncing, or its
            // checkpoint moved): the next pass plans afresh.
            return Ok(());
        }
    }
    Ok(())
}

/// Commit one fetched chunk. A part the store refuses as invalid (in
/// practice: over the write budget, as long `References` lists in a
/// mailing-list folder can make 250 headers) is halved and retried in
/// UID order; one header too large on its own stays unfetched and the
/// mailbox `backfilling`, so a pass never stalls on it. Answers
/// whether the chunk's checkpoint committed.
async fn commit_chunk(
    worker: &Worker,
    checkpoint: &mut Checkpoint<'_>,
    headers: Vec<SyncedHeader>,
    highest: Option<u32>,
    last_chunk: bool,
    complete: &mut bool,
) -> bool {
    let mut pending: VecDeque<Vec<SyncedHeader>> = VecDeque::from([headers]);
    while let Some(part) = pending.pop_front() {
        let final_part = pending.is_empty();
        // A part's checkpoint covers its own UIDs; the chunk's last part
        // covers the whole chunk, the pass's last one the server's
        // UIDNEXT.
        let above = if final_part {
            highest
        } else {
            part.last().map(|header| header.uid)
        };
        let caught_up = last_chunk && final_part;
        let part = Arc::new(part);
        let committed = checkpoint
            .commit(
                worker,
                above,
                caught_up,
                caught_up && *complete,
                Arc::clone(&part),
            )
            .await;
        match committed {
            Ok(()) => {}
            Err(StoreError::InvalidInput { .. }) if part.len() > 1 => {
                let (front, back) = part.split_at(part.len() / 2);
                pending.push_front(back.to_vec());
                pending.push_front(front.to_vec());
            }
            Err(StoreError::InvalidInput { .. }) if part.len() == 1 => {
                *complete = false;
                if final_part {
                    // The checkpoint still moves past it.
                    pending.push_back(Vec::new());
                }
            }
            Err(_) => return false,
        }
    }
    true
}

/// The mailbox's checkpoint as the pass advances it.
struct Checkpoint<'a> {
    account_id: &'a str,
    mailbox_id: i64,
    uidvalidity: u32,
    /// The stored `uidnext` the next batch expects.
    stored: Option<u32>,
    /// EXAMINE's UIDNEXT, when the server reported one.
    server_next: Option<u32>,
}

impl Checkpoint<'_> {
    /// The checkpoint a batch commits: above its highest UID, never
    /// below the stored one, and at the server's UIDNEXT once the pass
    /// reached it.
    fn target(&self, highest: Option<u32>, reached_server: bool) -> u32 {
        let mut next = self.stored.unwrap_or(1);
        if let Some(highest) = highest {
            next = next.max(highest.saturating_add(1));
        }
        if reached_server && let Some(server) = self.server_next {
            next = next.max(server);
        }
        next
    }

    /// Commit one batch and advance past it.
    async fn commit(
        &mut self,
        worker: &Worker,
        highest: Option<u32>,
        reached_server: bool,
        caught_up: bool,
        headers: Arc<Vec<SyncedHeader>>,
    ) -> Result<(), StoreError> {
        let next_uidnext = self.target(highest, reached_server);
        let account_id = self.account_id.to_owned();
        let (mailbox_id, uidvalidity, expected_uidnext) =
            (self.mailbox_id, self.uidvalidity, self.stored);
        let now = worker.resources().now();
        worker
            .resources()
            .write(WritePriority::Sync, move |tx| {
                commit_header_batch(
                    tx,
                    &HeaderBatch {
                        account_id: &account_id,
                        mailbox_id,
                        uidvalidity,
                        expected_uidnext,
                        next_uidnext,
                        caught_up,
                        headers: &headers,
                    },
                    now,
                )
            })
            .await?;
        self.stored = Some(next_uidnext);
        Ok(())
    }
}

/// One fetched row as the store keeps it, or `None` for a row whose
/// header section the server withheld (it is fetched again later).
fn synced_header(policy: Policy, row: &HeaderRow) -> Option<SyncedHeader> {
    let HeaderBytes::Full(raw_fields) = &row.headers else {
        return None;
    };
    Some(SyncedHeader {
        uid: row.uid.get(),
        flags: synced_flags(&row.flags),
        internal_date: Some(Timestamp::from_millis(row.internal_date_ms)),
        rfc822_size: Some(u64::from(row.rfc822_size)),
        provider_key: policy.grouping_key(raw_fields),
        columns: header_columns(raw_fields),
        raw_fields: raw_fields.clone(),
    })
}

/// IMAP's system flags onto the occurrence columns; every other flag
/// is a keyword. `\Recent` is session state, not the message's.
fn synced_flags(flags: &[String]) -> SyncedFlags {
    let mut synced = SyncedFlags::default();
    for flag in flags {
        let column = match flag.to_ascii_lowercase().as_str() {
            "\\seen" => &mut synced.seen,
            "\\flagged" => &mut synced.flagged,
            "\\answered" => &mut synced.answered,
            "\\draft" => &mut synced.draft,
            "\\deleted" => &mut synced.deleted,
            "\\recent" => continue,
            _ => {
                synced.keywords.push(flag.clone());
                continue;
            }
        };
        *column = true;
    }
    synced
}

#[cfg(test)]
mod tests;
