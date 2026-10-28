//! Search keysets, opaque sort-bound cursors and exact unknowns (spec/07).

use rusqlite::types::{Type, Value};
use rusqlite::{OptionalExtension, Transaction, params_from_iter};
use serde_json::{Value as Json, json};

use super::{Compiled, CoverageCounts, SearchScope, compile_driven, nonnegative_count};
use crate::lists::{self, ListKey, PageRead, StoredListRow};
use crate::query::{MAX_QUERY_INPUT_BYTES, Query};
use crate::{StoreError, Uuid};

const CURSOR_VERSION: u64 = 1;
const CURSOR_BYTES: usize = MAX_QUERY_INPUT_BYTES + 1024;
const BM25: &str = "bm25(d_search_fts,4.0,2.0,1.0,1.0,1.0,0.25,0.1,0.75)";
const CURSOR_FIELDS: [&str; 7] = [
    "v",
    "store",
    "incarnation",
    "spec",
    "generation",
    "occurrence_id",
    "sort_date",
];

/// An opaque sort-bound cursor or any occurrence of a matching logical message.
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
    /// The generation-bound opaque cursor, passed back byte-for-byte.
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
    /// Chosen matching rows in the entrypoint's window order.
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
    sort: Sort,
}

struct Decoded {
    incarnation: Uuid,
    generation: u64,
    key: Key,
}

#[derive(Clone, Copy)]
enum Sort {
    Date,
    Relevance,
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum Key {
    Date(ListKey),
    Score { bits: u64, occurrence_id: i64 },
}

impl Key {
    fn occurrence_id(self) -> i64 {
        match self {
            Self::Date(key) => key.occurrence_id,
            Self::Score { occurrence_id, .. } => occurrence_id,
        }
    }

    fn scalar(self) -> Value {
        match self {
            Self::Date(key) => key.sort_date.into(),
            Self::Score { bits, .. } => f64::from_bits(bits).into(),
        }
    }
}

struct Hit {
    row: StoredListRow,
    key: Key,
}

/// What a caller kept from earlier reads of the same store (spec/07); the
/// default keeps nothing, so the page reads everything itself.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Kept<'a> {
    pub unknown: UnknownCount,
    /// Coverage's scope counts from this snapshot, which let the page skip
    /// candidates a driver proves false (spec/06, M1.6r2).
    pub coverage: Option<&'a CoverageCounts>,
}

/// A page's exact final-unknown count: counted, or reused from an earlier
/// read of the same query and scope at `generation`. A count from another
/// generation is never reused; the page counts again.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum UnknownCount {
    #[default]
    Count,
    Reuse {
        generation: u64,
        excluded_unknown: u64,
    },
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
    kept: Kept<'_>,
) -> Result<PageRead<SearchPage>, StoreError> {
    read_page(tx, query, scope, anchor, limit, at, kept, Sort::Date)
}

/// Read a native weighted-BM25 page, with every result from the supplied tx.
pub fn read_search_relevance_page(
    tx: &Transaction,
    query: &Query,
    scope: SearchScope,
    anchor: SearchAnchor,
    limit: usize,
    at: Option<u64>,
    kept: Kept<'_>,
) -> Result<PageRead<SearchPage>, StoreError> {
    read_page(tx, query, scope, anchor, limit, at, kept, Sort::Relevance)
}

#[allow(clippy::too_many_arguments)] // both public sorts' parameters plus the sort
fn read_page(
    tx: &Transaction,
    query: &Query,
    scope: SearchScope,
    anchor: SearchAnchor,
    limit: usize,
    at: Option<u64>,
    kept: Kept<'_>,
    sort: Sort,
) -> Result<PageRead<SearchPage>, StoreError> {
    if !(1..=lists::MAX_PAGE_ROWS).contains(&limit) {
        return Err(StoreError::InvalidInput {
            message: format!("a search page needs 1 to {} rows", lists::MAX_PAGE_ROWS),
        });
    }
    let plan = compile_driven(tx, query, scope.clone(), kept.coverage)?;
    let binding = Binding::read(tx, query, &scope, generation(plan.generation)?, sort)?;
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
    plan.check_skipped_bindings(tx)?;
    // Pages seek in page order, walking where they can; lookups by row
    // read the plain relation.
    let source = hits(&plan, sort);
    let ordered = walk(&plan, sort).unwrap_or_else(|| source.clone());
    let (hits, before, after) = match anchor {
        SearchAnchor::Top => {
            let mut rows = seek(tx, &plan, &ordered, sort, Side::After, None, limit + 1)?;
            let more = rows.len() > limit;
            rows.truncate(limit);
            (rows, false, more)
        }
        SearchAnchor::After(_) | SearchAnchor::Before(_) => {
            let key = cursor.expect("cursor anchors were decoded above").key;
            let stored = row_by(
                tx,
                &plan,
                &source,
                sort,
                "occurrence_id",
                key.occurrence_id(),
            )?;
            if stored.is_none_or(|hit| hit.key != key) {
                return Err(invalid("a search cursor names no chosen hit with that key"));
            }
            let side = match anchor {
                SearchAnchor::Before(_) => Side::Before,
                _ => Side::After,
            };
            let mut rows = seek(tx, &plan, &ordered, sort, side, Some(key), limit + 1)?;
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
                Some(id) => row_by(tx, &plan, &source, sort, "message_id", id)?,
                None => None,
            };
            let Some(target) = target else {
                return Ok(PageRead::NotInWindow);
            };
            let newer = seek(
                tx,
                &plan,
                &ordered,
                sort,
                Side::Before,
                Some(target.key),
                limit,
            )?;
            let older = seek(
                tx,
                &plan,
                &ordered,
                sort,
                Side::After,
                Some(target.key),
                limit,
            )?;
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
    let keys: Vec<_> = hits.iter().map(|hit| hit.key).collect();
    let mut rows: Vec<_> = hits.into_iter().map(|hit| hit.row).collect();
    lists::attach_tags(tx, &mut rows)?;
    let rows = rows
        .into_iter()
        .zip(keys)
        .map(|(row, key)| StoredSearchRow {
            cursor: binding.mint(key),
            row,
        })
        .collect();
    let excluded_unknown = match kept.unknown {
        UnknownCount::Reuse {
            generation,
            excluded_unknown,
        } if generation == binding.generation => excluded_unknown,
        _ => {
            let sql = format!(
                "SELECT coalesce(sum(v IS NULL),0) FROM ({})",
                plan.unknown_source
            );
            let raw = plan.read(tx, &sql, &[], |statement, values| {
                statement.query_row(params_from_iter(values), |row| row.get(0))
            })?;
            nonnegative_count(raw)?
        }
    };
    Ok(PageRead::Ready(SearchPage {
        generation: binding.generation,
        rows,
        has_more_before: before,
        has_more_after: after,
        excluded_unknown,
    }))
}

/// Every chosen matching occurrence in date order, bound to its generation.
pub fn search_date_ids(
    tx: &Transaction,
    query: &Query,
    scope: SearchScope,
    at: Option<u64>,
    coverage: Option<&CoverageCounts>,
) -> Result<PageRead<(u64, Vec<i64>)>, StoreError> {
    search_ids(tx, query, scope, at, coverage, Sort::Date)
}

/// Every chosen matching occurrence in relevance order, bound to its generation.
pub fn search_relevance_ids(
    tx: &Transaction,
    query: &Query,
    scope: SearchScope,
    at: Option<u64>,
    coverage: Option<&CoverageCounts>,
) -> Result<PageRead<(u64, Vec<i64>)>, StoreError> {
    search_ids(tx, query, scope, at, coverage, Sort::Relevance)
}

fn search_ids(
    tx: &Transaction,
    query: &Query,
    scope: SearchScope,
    at: Option<u64>,
    coverage: Option<&CoverageCounts>,
    sort: Sort,
) -> Result<PageRead<(u64, Vec<i64>)>, StoreError> {
    let plan = compile_driven(tx, query, scope, coverage)?;
    let generation = generation(plan.generation)?;
    if at.is_some_and(|at| at != generation) {
        return Ok(PageRead::Stale { generation });
    }
    plan.check_skipped_bindings(tx)?;
    let order = sort.seek_terms(Side::After).2;
    let sql = format!(
        "SELECT occurrence_id FROM ({}) ORDER BY {order}",
        hits(&plan, sort)
    );
    let ids = plan.read(tx, &sql, &[], |statement, values| {
        statement
            .query_map(params_from_iter(values), |row| row.get(0))?
            .collect()
    })?;
    Ok(PageRead::Ready((generation, ids)))
}

/// The hits relation walked in page order, when the plan walks this sort:
/// the scope's rows through an index of that order, each evaluated, so a
/// page's limit stops the read (spec/06, M1.6r3c).
fn walk(plan: &Compiled, sort: Sort) -> Option<String> {
    let walk = match sort {
        Sort::Date => plan.date_walk.as_ref()?,
        Sort::Relevance => return None,
    };
    Some(format!(
        "SELECT {} FROM {} WHERE {} AND ({})=1",
        lists::COLUMNS,
        walk.source,
        walk.filter,
        plan.predicate
    ))
}

fn hits(plan: &Compiled, sort: Sort) -> String {
    match sort {
        Sort::Date => format!(
            "SELECT {} FROM ({}) h JOIN d_list_rows r
             ON r.occurrence_id=h.chosen_occurrence_id WHERE h.v=1",
            lists::COLUMNS,
            plan.hit_source
        ),
        Sort::Relevance => {
            // One FTS pass per distinct positive leaf scores its documents once.
            let scores: Vec<_> = plan
                .leaves
                .iter()
                .enumerate()
                .map(|(i, leaf)| {
                    format!(
                        "s{i} AS MATERIALIZED (SELECT rowid AS doc,{BM25} AS score FROM d_search_fts
                 WHERE d_search_fts MATCH {})",
                        leaf.phrase
                    )
                })
                .collect();
            let with = if scores.is_empty() {
                String::new()
            } else {
                format!("WITH {} ", scores.join(","))
            };
            let contributions: Vec<_> = plan
                .leaves
                .iter()
                .enumerate()
                .map(|(i, leaf)| {
                    format!(
                        "CASE WHEN ({})=1 THEN coalesce((SELECT score FROM s{i}
                 WHERE doc=lm.acquisition_id),0.0) ELSE 0.0 END",
                        leaf.check
                    )
                })
                .collect();
            // SQLite's left-associative addition retains first-encounter order.
            let score = if contributions.is_empty() {
                "0.0".into()
            } else {
                contributions.join(" + ")
            };
            format!(
                "{with}SELECT {},({score}) AS score FROM {} WHERE {} AND ({})=1",
                lists::COLUMNS,
                super::source(),
                plan.hit_scope,
                plan.predicate
            )
        }
    }
}

/// The date and relevance hits relations, for plan-shape tests.
#[cfg(test)]
pub(super) fn hit_sources(plan: &Compiled) -> [String; 2] {
    [hits(plan, Sort::Date), hits(plan, Sort::Relevance)]
}

/// A date page's first seek over the walked relation, for plan-shape
/// tests: Top, then After a key.
#[cfg(test)]
pub(super) fn date_walk_seeks(plan: &Compiled) -> Option<[String; 2]> {
    let walked = walk(plan, Sort::Date)?;
    let (column, compare, order) = Sort::Date.seek_terms(Side::After);
    Some([
        format!("SELECT * FROM ({walked}) ORDER BY {order} LIMIT 51"),
        format!(
            "SELECT * FROM ({walked}) WHERE {column} {compare} 0 OR({column}=0 AND occurrence_id < 0)
             ORDER BY {order} LIMIT 51"
        ),
    ])
}

fn seek(
    tx: &Transaction,
    plan: &Compiled,
    source: &str,
    sort: Sort,
    side: Side,
    key: Option<Key>,
    limit: usize,
) -> Result<Vec<Hit>, StoreError> {
    let (column, compare, order) = sort.seek_terms(side);
    let tie = match side {
        Side::After => "<",
        Side::Before => ">",
    };
    let mut values = Vec::new();
    let filter = if let Some(key) = key {
        values.extend([key.scalar(), key.scalar(), key.occurrence_id().into()]);
        format!("WHERE {column} {compare} ? OR({column}=? AND occurrence_id {tie} ?)")
    } else {
        String::new()
    };
    values.push((limit as i64).into());
    let sql = format!("SELECT * FROM ({source}) {filter} ORDER BY {order} LIMIT ?");
    plan.read(tx, &sql, &values, |statement, values| {
        statement
            .query_map(params_from_iter(values), |row| sort.read_hit(row))?
            .collect()
    })
}

fn row_by(
    tx: &Transaction,
    plan: &Compiled,
    source: &str,
    sort: Sort,
    column: &str,
    id: i64,
) -> Result<Option<Hit>, StoreError> {
    let sql = format!("SELECT * FROM ({source}) WHERE {column}=?");
    plan.read(tx, &sql, &[id.into()], |statement, values| {
        statement
            .query_row(params_from_iter(values), |row| sort.read_hit(row))
            .optional()
    })
}

impl Sort {
    fn name(self) -> &'static str {
        match self {
            Self::Date => "date",
            Self::Relevance => "relevance",
        }
    }

    fn cursor_field(self) -> &'static str {
        match self {
            Self::Date => "sort_date",
            Self::Relevance => "score_bits",
        }
    }

    fn seek_terms(self, side: Side) -> (&'static str, &'static str, &'static str) {
        match (self, side) {
            (Self::Date, Side::After) => ("sort_date", "<", "sort_date DESC,occurrence_id DESC"),
            (Self::Date, Side::Before) => ("sort_date", ">", "sort_date ASC,occurrence_id ASC"),
            (Self::Relevance, Side::After) => ("score", ">", "score ASC,occurrence_id DESC"),
            (Self::Relevance, Side::Before) => ("score", "<", "score DESC,occurrence_id ASC"),
        }
    }

    fn read_hit(self, row: &rusqlite::Row<'_>) -> rusqlite::Result<Hit> {
        let display = lists::read_row(row)?;
        let key = match self {
            Self::Date => Key::Date(display.key()),
            Self::Relevance => {
                let score: f64 = row.get("score")?;
                if !score.is_finite() {
                    return Err(rusqlite::Error::FromSqlConversionFailure(
                        11,
                        Type::Real,
                        std::io::Error::other("nonfinite native search score").into(),
                    ));
                }
                Key::Score {
                    bits: score_bits(score),
                    occurrence_id: display.occurrence_id,
                }
            }
        };
        Ok(Hit { row: display, key })
    }
}

fn score_bits(score: f64) -> u64 {
    if score == 0.0 { 0 } else { score.to_bits() }
}

impl Binding {
    fn read(
        tx: &Transaction,
        query: &Query,
        scope: &SearchScope,
        generation: u64,
        sort: Sort,
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
            spec: json!({"query":query,"scope":scope,"sort":sort.name()}),
            generation,
            sort,
        })
    }

    fn mint(&self, key: Key) -> String {
        let mut value = json!({"v":CURSOR_VERSION,"store":self.store.to_string(),
            "incarnation":self.incarnation.to_string(),"spec":self.spec,
            "generation":self.generation,"occurrence_id":key.occurrence_id()});
        value[self.sort.cursor_field()] = match key {
            Key::Date(key) => json!(key.sort_date),
            Key::Score { bits, .. } => json!(format!("{bits:016x}")),
        };
        value.to_string()
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
        let mut fields = CURSOR_FIELDS;
        fields[6] = self.sort.cursor_field();
        if object.len() != fields.len() || object.keys().any(|key| !fields.contains(&key.as_str()))
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
        let occurrence_id = object["occurrence_id"]
            .as_i64()
            .ok_or_else(|| invalid("invalid cursor occurrence"))?;
        let key = match self.sort {
            Sort::Date => Key::Date(ListKey {
                occurrence_id,
                sort_date: object["sort_date"]
                    .as_i64()
                    .ok_or_else(|| invalid("invalid cursor date"))?,
            }),
            Sort::Relevance => {
                let bits = object["score_bits"]
                    .as_str()
                    .filter(|text| {
                        text.len() == 16
                            && text
                                .bytes()
                                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                    })
                    .and_then(|text| u64::from_str_radix(text, 16).ok())
                    .ok_or_else(|| invalid("invalid cursor score bits"))?;
                let score = f64::from_bits(bits);
                if !score.is_finite() {
                    return Err(invalid("nonfinite cursor score"));
                }
                Key::Score {
                    bits: score_bits(score),
                    occurrence_id,
                }
            }
        };
        Ok(Decoded {
            incarnation: uuid("incarnation")?,
            generation: object["generation"]
                .as_u64()
                .ok_or_else(|| invalid("invalid cursor generation"))?,
            key,
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
    crate::schema::classify(error, "read search pages")
}
