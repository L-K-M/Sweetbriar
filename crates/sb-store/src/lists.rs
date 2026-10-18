//! Keyset pages over the list projection (spec/07 "Paging", spec/02
//! `d_list_rows`): one short read returns a page's rows, their tags and
//! the generation they belong to, never with `OFFSET`.
//!
//! ```text
//!        newer ◄────────────── window order ──────────────► older
//!  Top:     [r0 r1 … r(n-1)]
//!  After(k):                  k [ … n rows strictly older … ]
//!  Before(k): [ … n rows strictly newer … ] k
//!  Around(id):        [ ⌊L/2⌋ newer ][id][ older … ]  (clamped at both ends)
//! ```
//!
//! Every page is in window order: `sort_date` descending, then
//! `occurrence_id` descending.

use std::collections::HashMap;

use rusqlite::types::{ToSql, Value};
use rusqlite::{OptionalExtension, Transaction, params_from_iter};

use crate::StoreError;
use crate::list_rows;
use crate::schema::classify;

/// A mailbox's special-use role (spec/02 `mailboxes.role`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum MailboxRole {
    Inbox,
    Sent,
    Drafts,
    Trash,
    Junk,
    Archive,
    All,
    /// Bridge's Starred (spec/04).
    Flagged,
}

impl MailboxRole {
    /// The role as `mailboxes.role` spells it.
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inbox => "inbox",
            Self::Sent => "sent",
            Self::Drafts => "drafts",
            Self::Trash => "trash",
            Self::Junk => "junk",
            Self::Archive => "archive",
            Self::All => "all",
            Self::Flagged => "flagged",
        }
    }
}

/// Which rows a page reads (spec/07 grain): one mailbox's occurrences,
/// every occurrence in mailboxes of one role, or the flagged logical
/// messages or one thread's, through their representatives.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListScope {
    Mailbox(i64),
    Unified(MailboxRole),
    Flagged,
    Thread(i64),
}

impl ListScope {
    /// The rows the scope reads, as `r`. A thread joins its members to
    /// their representatives: the one list read that joins (spec/02).
    fn source(self) -> &'static str {
        match self {
            Self::Thread(_) => {
                "d_thread_members m
                 JOIN d_list_rows r ON r.message_id = m.message_id AND r.representative = 1"
            }
            _ => "d_list_rows r",
        }
    }

    /// The scope's predicate. `Flagged` spells its literals so the
    /// partial `d_list_rows_flagged` index applies at prepare time.
    fn predicate(self) -> &'static str {
        match self {
            Self::Mailbox(_) => "r.mailbox_id = :scope",
            Self::Unified(_) => "r.mailbox_role = :scope",
            Self::Flagged => "r.flagged = 1 AND r.representative = 1",
            Self::Thread(_) => "m.thread_id = :scope",
        }
    }

    /// The statement parameters: the scope's `:scope`, if it has one,
    /// then `extra`.
    fn params(self, extra: &[(&'static str, i64)]) -> Vec<(&'static str, Value)> {
        let scope = match self {
            Self::Mailbox(id) | Self::Thread(id) => Some(Value::Integer(id)),
            Self::Unified(role) => Some(Value::Text(role.as_str().to_owned())),
            Self::Flagged => None,
        };
        scope
            .map(|value| (":scope", value))
            .into_iter()
            .chain(
                extra
                    .iter()
                    .map(|&(name, value)| (name, Value::Integer(value))),
            )
            .collect()
    }
}

/// Named parameters in the shape rusqlite binds.
fn bound<'a>(params: &'a [(&'static str, Value)]) -> Vec<(&'static str, &'a dyn ToSql)> {
    params
        .iter()
        .map(|(name, value)| (*name, value as &dyn ToSql))
        .collect()
}

/// A row's position in window order.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ListKey {
    pub sort_date: i64,
    pub occurrence_id: i64,
}

/// Where a page starts (spec/07 `Anchor`), with cursors already decoded
/// to their rows' keys.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ListAnchor {
    Top,
    /// Rows strictly older than the key's row.
    After(ListKey),
    /// Rows strictly newer than the key's row.
    Before(ListKey),
    /// The page centred on this occurrence.
    Around(i64),
}

/// One row as the projection stores it, with the message's tags.
/// Fields are public for the engine's mapping into its own row type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredListRow {
    pub occurrence_id: i64,
    pub message_id: i64,
    pub account_id: String,
    pub mailbox_id: i64,
    pub sort_date: i64,
    pub sender: Option<String>,
    pub subject: Option<String>,
    pub snippet: Option<String>,
    pub unread: bool,
    pub flagged: bool,
    pub has_attachments: bool,
    /// Tag names, sorted.
    pub tags: Vec<String>,
}

impl StoredListRow {
    /// The row's position in window order.
    pub fn key(&self) -> ListKey {
        ListKey {
            sort_date: self.sort_date,
            occurrence_id: self.occurrence_id,
        }
    }
}

/// A page read at one committed generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListPage {
    pub generation: u64,
    pub rows: Vec<StoredListRow>,
    pub has_more_before: bool,
    pub has_more_after: bool,
}

/// What a read found.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PageRead<T> {
    Ready(T),
    /// The caller's generation is not the store's: it must rebuild.
    Stale {
        generation: u64,
    },
    /// The anchor's row is not in the scope at this generation. For
    /// `Around` the id is not in the window (spec/07 `NotFound`); for a
    /// cursor's key it was not minted on this window (`InvalidInput`).
    NotInWindow,
}

/// Window order and its reverse, for `Before` and the newer half of
/// `Around`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
enum Direction {
    Older,
    Newer,
}

/// The largest page (spec/07 `limit`).
pub const MAX_PAGE_ROWS: usize = 1_000;

const COLUMNS: &str = "r.occurrence_id, r.message_id, r.account_id, r.mailbox_id, r.sort_date,
  r.sender, r.subject, r.snippet, r.unread, r.flagged, r.has_attachments";

/// Read one page of `scope` from `anchor`: at most `limit` rows, in
/// window order, with their tags. With `at`, a store at another
/// generation answers `Stale`. All reads share the caller's
/// transaction, so rows, tags and generation agree.
pub fn read_list_page(
    tx: &Transaction,
    scope: ListScope,
    anchor: ListAnchor,
    limit: usize,
    at: Option<u64>,
) -> Result<PageRead<ListPage>, StoreError> {
    if !(1..=MAX_PAGE_ROWS).contains(&limit) {
        return Err(StoreError::InvalidInput {
            message: format!("a list page holds 1 to {MAX_PAGE_ROWS} rows"),
        });
    }
    let generation = list_rows::generation(tx)?;
    if at.is_some_and(|at| at != generation) {
        return Ok(PageRead::Stale { generation });
    }

    let page = match anchor {
        ListAnchor::Top => {
            let mut rows = rows(tx, scope, None, Direction::Older, limit + 1)?;
            let has_more_after = rows.len() > limit;
            rows.truncate(limit);
            (rows, false, has_more_after)
        }
        ListAnchor::After(key) => {
            if !contains(tx, scope, key)? {
                return Ok(PageRead::NotInWindow);
            }
            let mut rows = rows(tx, scope, Some(key), Direction::Older, limit + 1)?;
            let has_more_after = rows.len() > limit;
            rows.truncate(limit);
            (rows, true, has_more_after)
        }
        ListAnchor::Before(key) => {
            if !contains(tx, scope, key)? {
                return Ok(PageRead::NotInWindow);
            }
            let mut rows = rows(tx, scope, Some(key), Direction::Newer, limit + 1)?;
            let has_more_before = rows.len() > limit;
            rows.truncate(limit);
            rows.reverse();
            (rows, has_more_before, true)
        }
        ListAnchor::Around(occurrence) => {
            let Some(key) = key_of(tx, scope, occurrence)? else {
                return Ok(PageRead::NotInWindow);
            };
            around(tx, scope, key, limit)?
        }
    };
    let (mut rows, has_more_before, has_more_after) = page;
    attach_tags(tx, &mut rows)?;
    Ok(PageRead::Ready(ListPage {
        generation,
        rows,
        has_more_before,
        has_more_after,
    }))
}

/// Every occurrence id of `scope` in window order, with the generation
/// they belong to (spec/07 "The id index").
pub fn list_ids(
    tx: &Transaction,
    scope: ListScope,
    at: Option<u64>,
) -> Result<PageRead<(u64, Vec<i64>)>, StoreError> {
    let generation = list_rows::generation(tx)?;
    if at.is_some_and(|at| at != generation) {
        return Ok(PageRead::Stale { generation });
    }
    let sql = ids_sql(scope);
    let params = scope.params(&[]);
    let mut statement = tx.prepare_cached(&sql).map_err(sql_error)?;
    let ids = statement
        .query_map(bound(&params).as_slice(), |row| row.get(0))
        .map_err(sql_error)?
        .collect::<Result<_, _>>()
        .map_err(sql_error)?;
    Ok(PageRead::Ready((generation, ids)))
}

/// The committed list generation (spec/02 `list_generation`).
pub fn list_generation(tx: &Transaction) -> Result<u64, StoreError> {
    list_rows::generation(tx)
}

/// The page around `key`: with the target at rank `r` of `N` and
/// `L = min(limit, N)`, ranks from `clamp(r − ⌊L/2⌋, 0, N − L)` (spec/07),
/// found from both sides of the target without counting the window.
fn around(
    tx: &Transaction,
    scope: ListScope,
    key: ListKey,
    limit: usize,
) -> Result<(Vec<StoredListRow>, bool, bool), StoreError> {
    // Up to `limit` on each side: enough to place the page and to tell
    // whether rows lie past it.
    let newer = rows(tx, scope, Some(key), Direction::Newer, limit)?;
    let older = rows(tx, scope, Some(key), Direction::Older, limit)?;
    let target = rows_at(tx, scope, key)?;
    let len = limit.min(1 + newer.len() + older.len());
    let mut before = (len / 2).min(newer.len());
    if older.len() < len - 1 - before {
        before = len - 1 - older.len();
    }
    let after = len - 1 - before;
    let has_more_before = newer.len() > before;
    let has_more_after = older.len() > after;

    let mut page: Vec<StoredListRow> = newer.into_iter().take(before).collect();
    page.reverse();
    page.extend(target);
    page.extend(older.into_iter().take(after));
    Ok((page, has_more_before, has_more_after))
}

/// Up to `count` rows of `scope` past `from` in `direction`, nearest
/// first; from the newest row when `from` is `None`.
fn rows(
    tx: &Transaction,
    scope: ListScope,
    from: Option<ListKey>,
    direction: Direction,
    count: usize,
) -> Result<Vec<StoredListRow>, StoreError> {
    let sql = page_sql(scope, from.is_some(), direction);
    let count = i64::try_from(count).unwrap_or(i64::MAX);
    let params = match from {
        Some(key) => scope.params(&[
            (":date", key.sort_date),
            (":occurrence", key.occurrence_id),
            (":count", count),
        ]),
        None => scope.params(&[(":count", count)]),
    };
    let mut statement = tx.prepare_cached(&sql).map_err(sql_error)?;
    statement
        .query_map(bound(&params).as_slice(), read_row)
        .map_err(sql_error)?
        .collect::<Result<_, _>>()
        .map_err(sql_error)
}

/// The statement [`list_ids`] runs.
fn ids_sql(scope: ListScope) -> String {
    format!(
        "SELECT r.occurrence_id FROM {} WHERE {}
         ORDER BY r.sort_date DESC, r.occurrence_id DESC",
        scope.source(),
        scope.predicate()
    )
}

/// The statement [`rows`] runs. Top and keyed reads are distinct
/// statements: an optional bound in one statement would defeat the
/// index seek.
fn page_sql(scope: ListScope, keyed: bool, direction: Direction) -> String {
    let (comparison, order) = match direction {
        Direction::Older => ("<", "DESC"),
        Direction::Newer => (">", "ASC"),
    };
    let seek = if keyed {
        format!("AND (r.sort_date, r.occurrence_id) {comparison} (:date, :occurrence)")
    } else {
        String::new()
    };
    format!(
        "SELECT {COLUMNS} FROM {} WHERE {} {seek}
         ORDER BY r.sort_date {order}, r.occurrence_id {order} LIMIT :count",
        scope.source(),
        scope.predicate()
    )
}

/// The row at `key`, when it belongs to `scope`.
fn rows_at(
    tx: &Transaction,
    scope: ListScope,
    key: ListKey,
) -> Result<Option<StoredListRow>, StoreError> {
    let sql = format!(
        "SELECT {COLUMNS} FROM {}
         WHERE r.occurrence_id = :occurrence AND r.sort_date = :date AND {}",
        scope.source(),
        scope.predicate()
    );
    let params = scope.params(&[(":date", key.sort_date), (":occurrence", key.occurrence_id)]);
    tx.prepare_cached(&sql)
        .and_then(|mut statement| statement.query_row(bound(&params).as_slice(), read_row))
        .optional()
        .map_err(sql_error)
}

fn contains(tx: &Transaction, scope: ListScope, key: ListKey) -> Result<bool, StoreError> {
    Ok(rows_at(tx, scope, key)?.is_some())
}

/// `occurrence`'s key, when its row belongs to `scope`.
fn key_of(
    tx: &Transaction,
    scope: ListScope,
    occurrence: i64,
) -> Result<Option<ListKey>, StoreError> {
    let sql = format!(
        "SELECT r.sort_date FROM {} WHERE r.occurrence_id = :occurrence AND {}",
        scope.source(),
        scope.predicate()
    );
    let params = scope.params(&[(":occurrence", occurrence)]);
    let date: Option<i64> = tx
        .prepare_cached(&sql)
        .and_then(|mut statement| statement.query_row(bound(&params).as_slice(), |row| row.get(0)))
        .optional()
        .map_err(sql_error)?;
    Ok(date.map(|sort_date| ListKey {
        sort_date,
        occurrence_id: occurrence,
    }))
}

fn read_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredListRow> {
    Ok(StoredListRow {
        occurrence_id: row.get(0)?,
        message_id: row.get(1)?,
        account_id: row.get(2)?,
        mailbox_id: row.get(3)?,
        sort_date: row.get(4)?,
        sender: row.get(5)?,
        subject: row.get(6)?,
        snippet: row.get(7)?,
        unread: row.get(8)?,
        flagged: row.get(9)?,
        has_attachments: row.get(10)?,
        tags: Vec::new(),
    })
}

/// Fill every row's tags with one query for the page's messages
/// (spec/02 "Tags in list rows").
fn attach_tags(tx: &Transaction, rows: &mut [StoredListRow]) -> Result<(), StoreError> {
    let mut messages: Vec<i64> = rows.iter().map(|row| row.message_id).collect();
    messages.sort_unstable();
    messages.dedup();
    if messages.is_empty() {
        return Ok(());
    }
    let placeholders = vec!["?"; messages.len()].join(", ");
    let sql = format!(
        "SELECT mt.message_id, t.name FROM message_tags mt JOIN tags t ON t.tag_id = mt.tag_id
         WHERE mt.message_id IN ({placeholders}) ORDER BY mt.message_id, t.name"
    );
    let mut statement = tx.prepare(&sql).map_err(sql_error)?;
    let mut tags: HashMap<i64, Vec<String>> = HashMap::new();
    let found = statement
        .query_map(params_from_iter(&messages), |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(sql_error)?;
    for tag in found {
        let (message, name) = tag.map_err(sql_error)?;
        tags.entry(message).or_default().push(name);
    }
    for row in rows {
        if let Some(names) = tags.get(&row.message_id) {
            row.tags.clone_from(names);
        }
    }
    Ok(())
}

fn sql_error(error: rusqlite::Error) -> StoreError {
    classify(error, "a list read")
}

#[cfg(test)]
mod tests;
