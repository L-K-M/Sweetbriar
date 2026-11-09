//! IMAP sync writes (spec/04 "Sync loop", steps 1 and 2): the
//! reconciliation of one LIST with an account's `mailboxes` rows, and
//! the epoch check after EXAMINE. They run inside the caller's writer
//! transaction; the engine maps LIST rows through its provider policy
//! first, so these see `mailboxes` columns, never IMAP attributes.
//!
//! `mailboxes.sync_state` here means:
//! - `new`: listed, its epoch not yet adopted (`uidvalidity` NULL);
//! - `backfilling`: epoch adopted, headers not yet caught up;
//! - `synced`: headers caught up with the server (set by header
//!   batches);
//! - `epoch_changed`: EXAMINE reported another UIDVALIDITY, or a NULL
//!   one with occurrences (after a restore); the epoch-change step
//!   resolves it (plan M2.5) and sync skips the mailbox until then;
//! - `gone`: absent from the last LIST; its occurrences are kept.

use std::collections::BTreeMap;

use rusqlite::{OptionalExtension, Transaction, params};

use crate::StoreError;
use crate::lists::MailboxRole;
use crate::schema::classify;
use crate::sidebar::MailboxKind;

/// One mailbox as the account's provider policy mapped a LIST row.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ObservedMailbox {
    /// The decoded (UTF-8) IMAP name, `mailboxes.remote_name`.
    pub remote_name: String,
    pub delimiter: Option<char>,
    pub kind: MailboxKind,
    pub role: Option<MailboxRole>,
    pub selectable: bool,
}

/// What one reconciliation changed, by `mailbox_id`, each in ascending
/// order. Unchanged mailboxes appear nowhere; a returning mailbox whose
/// columns also changed appears in both `returned` and `changed`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Reconciled {
    /// Newly listed mailboxes, inserted as `new`.
    pub added: Vec<i64>,
    /// Listed mailboxes whose kind, role, delimiter or selectability
    /// changed. A role change matters to the compatibility ledger
    /// (spec/04), so the caller sees it.
    pub changed: Vec<i64>,
    /// Mailboxes missing from the LIST, now `gone`.
    pub gone: Vec<i64>,
    /// `gone` mailboxes listed again.
    pub returned: Vec<i64>,
}

impl Reconciled {
    /// Whether the reconciliation wrote anything.
    pub fn is_empty(&self) -> bool {
        self.added.is_empty()
            && self.changed.is_empty()
            && self.gone.is_empty()
            && self.returned.is_empty()
    }
}

/// What EXAMINE's UIDVALIDITY means for a mailbox (spec/04 step 2).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EpochCheck {
    /// The mailbox had no epoch and no current occurrences: this
    /// UIDVALIDITY is now its epoch, and fetching starts at its
    /// stored `uidnext` (UID 1 when NULL).
    Adopted,
    /// The stored epoch is the server's.
    Unchanged,
    /// The epoch moved, or a NULL epoch has current occurrences: the
    /// mailbox is `epoch_changed` and sync skips it until the
    /// epoch-change step resolves it (spec/04 "Epoch changes").
    Changed,
}

/// The `sync_state` spellings this module writes and compares.
const STATE_NEW: &str = "new";
const STATE_BACKFILLING: &str = "backfilling";
const STATE_EPOCH_CHANGED: &str = "epoch_changed";
const STATE_GONE: &str = "gone";

/// One stored row as reconciliation compares it.
struct StoredMailbox {
    mailbox_id: i64,
    delimiter: Option<String>,
    role: Option<String>,
    kind: String,
    selectable: bool,
    sync_state: String,
    uidvalidity: Option<i64>,
}

/// Reconcile one complete LIST of an IMAP account with its `mailboxes`
/// rows (spec/04 step 1): new names are inserted `new`, listed rows
/// take the observed kind, role, delimiter and selectability, rows
/// missing from the LIST become `gone` with their occurrences kept,
/// and `gone` rows listed again resume (`backfilling` when they keep an
/// epoch, else `new`). Any change advances the list generation, so the
/// sidebar re-reads.
///
/// `NotFound` for an unknown account; `InvalidInput` for a non-IMAP
/// account (the local archive has no LIST), an empty or NUL-bearing
/// name, or a name listed twice.
pub fn reconcile_mailboxes(
    tx: &Transaction,
    account_id: &str,
    observed: &[ObservedMailbox],
) -> Result<Reconciled, StoreError> {
    require_imap_account(tx, account_id)?;
    let mut listed: BTreeMap<&str, &ObservedMailbox> = BTreeMap::new();
    for mailbox in observed {
        if mailbox.remote_name.is_empty() || mailbox.remote_name.contains('\0') {
            return Err(invalid("a listed mailbox name is empty or contains NUL"));
        }
        if listed.insert(&mailbox.remote_name, mailbox).is_some() {
            return Err(invalid("the LIST names a mailbox twice"));
        }
    }

    let stored = stored_mailboxes(tx, account_id)?;
    let mut reconciled = Reconciled::default();
    for (name, mailbox) in &listed {
        let Some(row) = stored.get(*name) else {
            reconciled
                .added
                .push(insert_mailbox(tx, account_id, mailbox)?);
            continue;
        };
        let returning = row.sync_state == STATE_GONE;
        let differs = row.delimiter != mailbox.delimiter.map(String::from)
            || row.role.as_deref() != mailbox.role.map(MailboxRole::as_str)
            || row.kind != mailbox.kind.as_str()
            || row.selectable != mailbox.selectable;
        if !returning && !differs {
            continue;
        }
        let state = if !returning {
            row.sync_state.as_str()
        } else if row.uidvalidity.is_some() {
            STATE_BACKFILLING
        } else {
            STATE_NEW
        };
        tx.execute(
            "UPDATE mailboxes
             SET delimiter = ?2, role = ?3, kind = ?4, selectable = ?5, sync_state = ?6,
                 revision = revision + 1
             WHERE mailbox_id = ?1",
            params![
                row.mailbox_id,
                mailbox.delimiter.map(String::from),
                mailbox.role.map(MailboxRole::as_str),
                mailbox.kind.as_str(),
                mailbox.selectable,
                state,
            ],
        )
        .map_err(|e| classify(e, "update a listed mailbox"))?;
        if returning {
            reconciled.returned.push(row.mailbox_id);
        }
        if differs {
            reconciled.changed.push(row.mailbox_id);
        }
    }

    for (name, row) in &stored {
        if listed.contains_key(name.as_str()) || row.sync_state == STATE_GONE {
            continue;
        }
        tx.execute(
            "UPDATE mailboxes SET sync_state = ?2, revision = revision + 1
             WHERE mailbox_id = ?1",
            params![row.mailbox_id, STATE_GONE],
        )
        .map_err(|e| classify(e, "mark a mailbox gone"))?;
        reconciled.gone.push(row.mailbox_id);
    }

    for ids in [
        &mut reconciled.added,
        &mut reconciled.changed,
        &mut reconciled.gone,
        &mut reconciled.returned,
    ] {
        ids.sort_unstable();
    }
    if !reconciled.is_empty() {
        crate::list_rows::advance_generation(tx)?;
    }
    Ok(reconciled)
}

/// Check one mailbox's epoch against EXAMINE's `uidvalidity` (spec/04
/// step 2; RFC 3501 makes it nonzero). A NULL epoch is adopted only
/// while the mailbox has no current occurrence; with occurrences
/// (after a restore, spec/03) it counts as a change, as does any other
/// value. A change marks the mailbox `epoch_changed` and touches no
/// occurrence: resolving it is the epoch-change step's (plan M2.5).
///
/// `NotFound` for a mailbox not of `account_id`; `InvalidInput` for a
/// zero UIDVALIDITY or a `gone` mailbox (EXAMINE only follows a LIST
/// that listed it).
pub fn check_epoch(
    tx: &Transaction,
    account_id: &str,
    mailbox_id: i64,
    uidvalidity: u32,
) -> Result<EpochCheck, StoreError> {
    if uidvalidity == 0 {
        return Err(invalid("UIDVALIDITY is never zero"));
    }
    let (stored, state): (Option<i64>, String) = tx
        .query_row(
            "SELECT uidvalidity, sync_state FROM mailboxes
             WHERE mailbox_id = ?1 AND account_id = ?2",
            params![mailbox_id, account_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|e| classify(e, "read a mailbox epoch"))?
        .ok_or_else(|| StoreError::not_found(format!("mailbox {mailbox_id}")))?;
    if state == STATE_GONE {
        return Err(invalid("a gone mailbox has no epoch to check"));
    }
    let observed = i64::from(uidvalidity);
    match stored {
        Some(epoch) if epoch == observed => {
            // The server is back on the stored epoch before the
            // epoch-change step acted: there was no change after all,
            // so sync resumes rather than skipping the mailbox forever.
            if state == STATE_EPOCH_CHANGED {
                tx.execute(
                    "UPDATE mailboxes SET sync_state = ?2, revision = revision + 1
                     WHERE mailbox_id = ?1",
                    params![mailbox_id, STATE_BACKFILLING],
                )
                .map_err(|e| classify(e, "clear a reverted epoch change"))?;
            }
            Ok(EpochCheck::Unchanged)
        }
        None if !has_current_occurrences(tx, mailbox_id)? => {
            tx.execute(
                "UPDATE mailboxes SET uidvalidity = ?2, sync_state = ?3,
                     revision = revision + 1
                 WHERE mailbox_id = ?1",
                params![mailbox_id, observed, STATE_BACKFILLING],
            )
            .map_err(|e| classify(e, "adopt a mailbox epoch"))?;
            Ok(EpochCheck::Adopted)
        }
        _ => {
            if state != STATE_EPOCH_CHANGED {
                tx.execute(
                    "UPDATE mailboxes SET sync_state = ?2, revision = revision + 1
                     WHERE mailbox_id = ?1",
                    params![mailbox_id, STATE_EPOCH_CHANGED],
                )
                .map_err(|e| classify(e, "mark a mailbox epoch changed"))?;
            }
            Ok(EpochCheck::Changed)
        }
    }
}

fn invalid(message: &str) -> StoreError {
    StoreError::InvalidInput {
        message: message.to_owned(),
    }
}

/// The account must exist and sync over IMAP.
fn require_imap_account(tx: &Transaction, account_id: &str) -> Result<(), StoreError> {
    let kind: String = tx
        .query_row(
            "SELECT kind FROM accounts WHERE account_id = ?1",
            params![account_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(|e| classify(e, "read the account kind"))?
        .ok_or_else(|| StoreError::not_found(format!("account {account_id}")))?;
    if kind != "imap" {
        return Err(invalid("only an IMAP account lists mailboxes"));
    }
    Ok(())
}

fn stored_mailboxes(
    tx: &Transaction,
    account_id: &str,
) -> Result<BTreeMap<String, StoredMailbox>, StoreError> {
    let read = |e| classify(e, "read the account's mailboxes");
    let mut statement = tx
        .prepare(
            "SELECT mailbox_id, remote_name, delimiter, role, kind, selectable, sync_state,
                    uidvalidity
             FROM mailboxes WHERE account_id = ?1",
        )
        .map_err(read)?;
    let rows = statement
        .query_map(params![account_id], |row| {
            Ok((
                row.get::<_, String>(1)?,
                StoredMailbox {
                    mailbox_id: row.get(0)?,
                    delimiter: row.get(2)?,
                    role: row.get(3)?,
                    kind: row.get(4)?,
                    selectable: row.get(5)?,
                    sync_state: row.get(6)?,
                    uidvalidity: row.get(7)?,
                },
            ))
        })
        .map_err(read)?;
    rows.collect::<Result<BTreeMap<_, _>, _>>().map_err(read)
}

fn insert_mailbox(
    tx: &Transaction,
    account_id: &str,
    mailbox: &ObservedMailbox,
) -> Result<i64, StoreError> {
    tx.execute(
        "INSERT INTO mailboxes
         (account_id, remote_name, delimiter, role, kind, selectable, sync_state)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            account_id,
            mailbox.remote_name,
            mailbox.delimiter.map(String::from),
            mailbox.role.map(MailboxRole::as_str),
            mailbox.kind.as_str(),
            mailbox.selectable,
            STATE_NEW,
        ],
    )
    .map_err(|e| classify(e, "insert a listed mailbox"))?;
    Ok(tx.last_insert_rowid())
}

/// Whether any occurrence of the mailbox is current (`present` or
/// `vanish_pending`, spec/02's locator states).
fn has_current_occurrences(tx: &Transaction, mailbox_id: i64) -> Result<bool, StoreError> {
    tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM occurrences
                       WHERE mailbox_id = ?1 AND state IN ('present', 'vanish_pending'))",
        params![mailbox_id],
        |row| row.get(0),
    )
    .map_err(|e| classify(e, "check a mailbox's occurrences"))
}

#[cfg(test)]
mod tests;
