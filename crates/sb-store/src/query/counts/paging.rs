//! Date search pages, opaque bound cursors and exact unknown counts (spec/07).

use rusqlite::{OptionalExtension, Transaction, params_from_iter};
use serde_json::{Value as Json, json};

use super::{Compiled, SearchScope, compile, nonnegative_count};
use crate::lists::{self, ListKey, PageRead, StoredListRow};
use crate::query::{MAX_QUERY_INPUT_BYTES, Query};
use crate::{StoreError, Uuid};

const CURSOR_VERSION: u64 = 1;
const CURSOR_BYTES: usize = MAX_QUERY_INPUT_BYTES + 1024;
const CURSOR_FIELDS: [&str; 7] = [
    "v",
    "store",
    "incarnation",
    "spec",
    "generation",
    "occurrence_id",
    "sort_date",
];

/// An opaque date cursor or any occurrence of a matching logical message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SearchAnchor {
    Top,
    After(String),
    Before(String),
    Around(i64),
}

/// Display data paired with this chosen row's opaque cursor.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredSearchRow {
    row: StoredListRow,
    cursor: String,
}

impl StoredSearchRow {
    /// The chosen row's display fields and tags.
    pub fn row(&self) -> &StoredListRow {
        &self.row
    }
    /// The generation-bound opaque date cursor.
    pub fn cursor(&self) -> &str {
        &self.cursor
    }
    /// Consume display data and cursor without cloning.
    pub fn into_parts(self) -> (StoredListRow, String) {
        (self.row, self.cursor)
    }
}

/// One short snapshot's rows, continuation flags and exact unknown total.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchPage {
    generation: u64,
    rows: Vec<StoredSearchRow>,
    has_more_before: bool,
    has_more_after: bool,
    excluded_unknown: u64,
}

impl SearchPage {
    /// Committed search generation shared by this response.
    pub fn generation(&self) -> u64 {
        self.generation
    }
    /// Chosen matching rows in date window order.
    pub fn rows(&self) -> &[StoredSearchRow] {
        &self.rows
    }
    /// More hits precede this page.
    pub fn has_more_before(&self) -> bool {
        self.has_more_before
    }
    /// More hits follow this page.
    pub fn has_more_after(&self) -> bool {
        self.has_more_after
    }
    /// Final unknowns over the full scope, not only this page.
    pub fn excluded_unknown(&self) -> u64 {
        self.excluded_unknown
    }
    /// Consume the page's rows.
    pub fn into_rows(self) -> Vec<StoredSearchRow> {
        self.rows
    }
}

struct Binding {
    store: Uuid,
    incarnation: Uuid,
    spec: Json,
    generation: u64,
}

struct Decoded {
    incarnation: Uuid,
    generation: u64,
    key: ListKey,
}

#[derive(Clone, Copy)]
enum Side {
    Before,
    After,
}

/// Read a date-ordered page at `at`, with every result from the supplied tx.
pub fn read_search_date_page(
    tx: &Transaction,
    query: &Query,
    scope: SearchScope,
    anchor: SearchAnchor,
    limit: usize,
    at: Option<u64>,
) -> Result<PageRead<SearchPage>, StoreError> {
    if !(1..=lists::MAX_PAGE_ROWS).contains(&limit) {
        return Err(StoreError::InvalidInput {
            message: format!("a search page needs 1 to {} rows", lists::MAX_PAGE_ROWS),
        });
    }
    let plan = compile(tx, query, scope.clone())?;
    let binding = Binding::read(tx, query, &scope, generation(plan.generation)?)?;
    let cursor = match &anchor {
        SearchAnchor::After(value) | SearchAnchor::Before(value) => Some(binding.decode(value)?),
        _ => None,
    };
    if at.is_some_and(|at| at != binding.generation)
        || cursor.as_ref().is_some_and(|cursor| {
            cursor.generation != binding.generation || cursor.incarnation != binding.incarnation
        })
    {
        return Ok(PageRead::Stale {
            generation: binding.generation,
        });
    }
    let source = hits(&plan);
    let (mut rows, before, after) = match anchor {
        SearchAnchor::Top => {
            let mut rows = seek(tx, &plan, &source, Side::After, None, limit + 1)?;
            let more = rows.len() > limit;
            rows.truncate(limit);
            (rows, false, more)
        }
        SearchAnchor::After(_) | SearchAnchor::Before(_) => {
            let key = cursor.expect("cursor anchors were decoded above").key;
            let stored = row_by(tx, &plan, &source, "occurrence_id", key.occurrence_id)?;
            if stored.is_none_or(|row| row.key() != key) {
                return Err(invalid("a search cursor names no chosen hit with that key"));
            }
            let side = match anchor {
                SearchAnchor::Before(_) => Side::Before,
                _ => Side::After,
            };
            let mut rows = seek(tx, &plan, &source, side, Some(key), limit + 1)?;
            let more = rows.len() > limit;
            rows.truncate(limit);
            match side {
                Side::Before => {
                    rows.reverse();
                    (rows, more, true)
                }
                Side::After => (rows, true, more),
            }
        }
        SearchAnchor::Around(id) => {
            let message = tx
                .query_row(
                    "SELECT message_id FROM occurrences WHERE occurrence_id=?1",
                    [id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()
                .map_err(sql_error)?;
            let target = match message {
                Some(id) => row_by(tx, &plan, &source, "message_id", id)?,
                None => None,
            };
            let Some(target) = target else {
                return Ok(PageRead::NotInWindow);
            };
            let newer = seek(tx, &plan, &source, Side::Before, Some(target.key()), limit)?;
            let older = seek(tx, &plan, &source, Side::After, Some(target.key()), limit)?;
            let (before, after) = lists::center_counts(limit, newer.len(), older.len());
            let more_before = newer.len() > before;
            let more_after = older.len() > after;
            let mut rows: Vec<_> = newer.into_iter().take(before).collect();
            rows.reverse();
            rows.push(target);
            rows.extend(older.into_iter().take(after));
            (rows, more_before, more_after)
        }
    };
    lists::attach_tags(tx, &mut rows)?;
    let rows = rows
        .into_iter()
        .map(|row| StoredSearchRow {
            cursor: binding.mint(row.key()),
            row,
        })
        .collect();
    let sql = format!("SELECT coalesce(sum(v IS NULL),0) FROM ({})", plan.source);
    let raw = plan.read(tx, &sql, &[], |statement, values| {
        statement.query_row(params_from_iter(values), |row| row.get(0))
    })?;
    Ok(PageRead::Ready(SearchPage {
        generation: binding.generation,
        rows,
        has_more_before: before,
        has_more_after: after,
        excluded_unknown: nonnegative_count(raw)?,
    }))
}

/// Every chosen matching occurrence in date order, bound to its generation.
pub fn search_date_ids(
    tx: &Transaction,
    query: &Query,
    scope: SearchScope,
    at: Option<u64>,
) -> Result<PageRead<(u64, Vec<i64>)>, StoreError> {
    let plan = compile(tx, query, scope)?;
    let generation = generation(plan.generation)?;
    if at.is_some_and(|at| at != generation) {
        return Ok(PageRead::Stale { generation });
    }
    let sql = format!(
        "SELECT occurrence_id FROM ({}) ORDER BY sort_date DESC,occurrence_id DESC",
        hits(&plan)
    );
    let ids = plan.read(tx, &sql, &[], |statement, values| {
        statement
            .query_map(params_from_iter(values), |row| row.get(0))?
            .collect()
    })?;
    Ok(PageRead::Ready((generation, ids)))
}

fn hits(plan: &Compiled) -> String {
    format!(
        "SELECT {} FROM ({}) h JOIN d_list_rows r
        ON r.occurrence_id=h.chosen_occurrence_id WHERE h.v=1",
        lists::COLUMNS,
        plan.source
    )
}

fn seek(
    tx: &Transaction,
    plan: &Compiled,
    source: &str,
    side: Side,
    key: Option<ListKey>,
    limit: usize,
) -> Result<Vec<StoredListRow>, StoreError> {
    let (compare, order) = match side {
        Side::After => ("<", "sort_date DESC,occurrence_id DESC"),
        Side::Before => (">", "sort_date ASC,occurrence_id ASC"),
    };
    let mut values = Vec::new();
    let filter = if let Some(key) = key {
        values.extend([
            key.sort_date.into(),
            key.sort_date.into(),
            key.occurrence_id.into(),
        ]);
        format!("WHERE sort_date {compare} ? OR(sort_date=? AND occurrence_id {compare} ?)")
    } else {
        String::new()
    };
    values.push((limit as i64).into());
    let sql = format!("SELECT * FROM ({source}) {filter} ORDER BY {order} LIMIT ?");
    plan.read(tx, &sql, &values, |statement, values| {
        statement
            .query_map(params_from_iter(values), lists::read_row)?
            .collect()
    })
}

fn row_by(
    tx: &Transaction,
    plan: &Compiled,
    source: &str,
    column: &str,
    id: i64,
) -> Result<Option<StoredListRow>, StoreError> {
    let sql = format!("SELECT * FROM ({source}) WHERE {column}=?");
    plan.read(tx, &sql, &[id.into()], |statement, values| {
        statement
            .query_row(params_from_iter(values), lists::read_row)
            .optional()
    })
}

impl Binding {
    fn read(
        tx: &Transaction,
        query: &Query,
        scope: &SearchScope,
        generation: u64,
    ) -> Result<Self, StoreError> {
        let identity = |key| {
            let value: Option<String> = tx
                .query_row("SELECT value FROM store_meta WHERE key=?1", [key], |row| {
                    row.get(0)
                })
                .optional()
                .map_err(sql_error)?;
            value
                .and_then(|value| value.parse().ok())
                .ok_or_else(|| StoreError::corrupt("invalid or missing store identity"))
        };
        let scope = match scope {
            SearchScope::All => json!("all"),
            SearchScope::Account(id) => json!({"account":id.to_string()}),
            SearchScope::Mailbox(id) => json!({"mailbox":id}),
        };
        let query: Json =
            serde_json::from_str(query.canonical_json()).expect("checked canonical query JSON");
        Ok(Self {
            store: identity("store_uuid")?,
            incarnation: identity("incarnation")?,
            spec: json!({"query":query,"scope":scope,"sort":"date"}),
            generation,
        })
    }

    fn mint(&self, key: ListKey) -> String {
        json!({"v":CURSOR_VERSION,"store":self.store.to_string(),
            "incarnation":self.incarnation.to_string(),"spec":self.spec,
            "generation":self.generation,"occurrence_id":key.occurrence_id,
            "sort_date":key.sort_date})
        .to_string()
    }

    fn decode(&self, input: &str) -> Result<Decoded, StoreError> {
        if input.len() > CURSOR_BYTES {
            return Err(invalid("a search cursor exceeds its byte limit"));
        }
        let value: Json =
            serde_json::from_str(input).map_err(|_| invalid("a search cursor is not JSON"))?;
        // Byte-exact canonical form also detects duplicates collapsed by Value.
        let canonical = serde_json::to_string(&value)
            .map_err(|_| invalid("a search cursor cannot be serialized"))?;
        if canonical != input {
            return Err(invalid("a search cursor is not canonical"));
        }
        let object = value
            .as_object()
            .ok_or_else(|| invalid("a search cursor is not an object"))?;
        if object.len() != CURSOR_FIELDS.len()
            || object
                .keys()
                .any(|key| !CURSOR_FIELDS.contains(&key.as_str()))
        {
            return Err(invalid("a search cursor has missing or unknown fields"));
        }
        let uuid = |name| {
            object[name]
                .as_str()
                .and_then(|value| value.parse::<Uuid>().ok())
                .ok_or_else(|| invalid("a search cursor has an invalid identity"))
        };
        if object["v"].as_u64() != Some(CURSOR_VERSION)
            || uuid("store")? != self.store
            || object["spec"] != self.spec
        {
            return Err(invalid(
                "a search cursor has a foreign version, store or spec",
            ));
        }
        Ok(Decoded {
            incarnation: uuid("incarnation")?,
            generation: object["generation"]
                .as_u64()
                .ok_or_else(|| invalid("invalid cursor generation"))?,
            key: ListKey {
                occurrence_id: object["occurrence_id"]
                    .as_i64()
                    .ok_or_else(|| invalid("invalid cursor occurrence"))?,
                sort_date: object["sort_date"]
                    .as_i64()
                    .ok_or_else(|| invalid("invalid cursor date"))?,
            },
        })
    }
}

fn generation(value: i64) -> Result<u64, StoreError> {
    value
        .try_into()
        .map_err(|_| StoreError::corrupt("negative search generation"))
}

fn invalid(message: &'static str) -> StoreError {
    StoreError::InvalidInput {
        message: message.into(),
    }
}

fn sql_error(error: rusqlite::Error) -> StoreError {
    crate::schema::classify(error, "read date search pages")
}
