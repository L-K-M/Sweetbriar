//! The sidebar's reads (spec/07 "Other queries", spec/08 "Sidebar"):
//! every account, and one account's mailboxes with their present and
//! unread occurrences, in the caller's short read transaction.

use rusqlite::{OptionalExtension, Transaction};

use crate::StoreError;
use crate::list_rows;
use crate::lists::MailboxRole;
use crate::schema::classify;

/// How an account reaches its mail (spec/02 `accounts.kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountKind {
    Local,
    Imap,
}

/// Whether an account syncs (spec/02 `accounts.state`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountState {
    Active,
    Paused,
    NeedsAttention,
    Disconnected,
}

/// What a mailbox is (spec/02 `mailboxes.kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailboxKind {
    Folder,
    Label,
    Virtual,
}

/// One account as the sidebar lists it. Fields are public for the
/// engine's and service's mapping.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAccount {
    pub account_id: String,
    pub kind: AccountKind,
    pub display_name: String,
    pub state: AccountState,
    pub state_reason: Option<String>,
    pub sort_order: i64,
}

/// One mailbox, with its present occurrences and the unread ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredMailbox {
    pub mailbox_id: i64,
    pub account_id: String,
    pub name: String,
    pub delimiter: Option<String>,
    pub role: Option<MailboxRole>,
    pub kind: MailboxKind,
    pub selectable: bool,
    pub total: u64,
    pub unread: u64,
}

/// An account's mailboxes as of one list generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailboxList {
    pub generation: u64,
    pub mailboxes: Vec<StoredMailbox>,
}

/// Every account, by sort order then id (spec/07).
pub fn accounts(tx: &Transaction) -> Result<Vec<StoredAccount>, StoreError> {
    let mut statement = tx
        .prepare_cached(
            "SELECT account_id, kind, display_name, state, state_reason, sort_order
             FROM accounts ORDER BY sort_order, account_id",
        )
        .map_err(sql_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })
        .map_err(sql_error)?;
    let mut accounts = Vec::new();
    for row in rows {
        let (account_id, kind, display_name, state, state_reason, sort_order) =
            row.map_err(sql_error)?;
        accounts.push(StoredAccount {
            kind: match kind.as_str() {
                "local" => AccountKind::Local,
                "imap" => AccountKind::Imap,
                other => return Err(unknown("account kind", other)),
            },
            state: match state.as_str() {
                "active" => AccountState::Active,
                "paused" => AccountState::Paused,
                "needs_attention" => AccountState::NeedsAttention,
                "disconnected" => AccountState::Disconnected,
                other => return Err(unknown("account state", other)),
            },
            account_id,
            display_name,
            state_reason,
            sort_order,
        });
    }
    Ok(accounts)
}

/// `account`'s mailboxes, folders then labels then virtual ones, each
/// group by name, with their counts and the list generation they show;
/// `None` for an unknown account (spec/07).
pub fn mailboxes(tx: &Transaction, account: &str) -> Result<Option<MailboxList>, StoreError> {
    let known: Option<i64> = tx
        .prepare_cached("SELECT 1 FROM accounts WHERE account_id = ?1")
        .and_then(|mut statement| statement.query_row([account], |row| row.get(0)))
        .optional()
        .map_err(sql_error)?;
    if known.is_none() {
        return Ok(None);
    }
    let generation = list_rows::generation(tx)?;
    let mut statement = tx
        .prepare_cached(
            "SELECT m.mailbox_id, m.account_id, m.remote_name, m.delimiter, m.role, m.kind,
               m.selectable,
               (SELECT count(*) FROM d_list_rows r WHERE r.mailbox_id = m.mailbox_id),
               (SELECT count(*) FROM d_list_rows r
                WHERE r.mailbox_id = m.mailbox_id AND r.unread = 1)
             FROM mailboxes m WHERE m.account_id = ?1
             ORDER BY CASE m.kind WHEN 'folder' THEN 0 WHEN 'label' THEN 1 ELSE 2 END,
               m.remote_name, m.mailbox_id",
        )
        .map_err(sql_error)?;
    let rows = statement
        .query_map([account], |row| {
            Ok((
                row.get::<_, i64>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, Option<String>>(3)?,
                row.get::<_, Option<String>>(4)?,
                row.get::<_, String>(5)?,
                row.get::<_, bool>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, i64>(8)?,
            ))
        })
        .map_err(sql_error)?;
    let mut mailboxes = Vec::new();
    for row in rows {
        let (mailbox_id, account_id, name, delimiter, role, kind, selectable, total, unread) =
            row.map_err(sql_error)?;
        let role = match role {
            Some(text) => {
                Some(MailboxRole::parse(&text).ok_or_else(|| unknown("mailbox role", &text))?)
            }
            None => None,
        };
        mailboxes.push(StoredMailbox {
            mailbox_id,
            account_id,
            name,
            delimiter,
            role,
            kind: match kind.as_str() {
                "folder" => MailboxKind::Folder,
                "label" => MailboxKind::Label,
                "virtual" => MailboxKind::Virtual,
                other => return Err(unknown("mailbox kind", other)),
            },
            selectable,
            total: count(total),
            unread: count(unread),
        });
    }
    Ok(Some(MailboxList {
        generation,
        mailboxes,
    }))
}

/// A `count(*)`, never negative.
fn count(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

/// A value the schema's CHECK constraints rule out: the store is
/// corrupt.
fn unknown(what: &str, value: &str) -> StoreError {
    StoreError::corrupt(format!("unknown {what} {value:?}"))
}

fn sql_error(error: rusqlite::Error) -> StoreError {
    classify(error, "a sidebar read")
}

#[cfg(test)]
mod tests;
