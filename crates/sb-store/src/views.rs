//! Saved views (spec/02, spec/07 Views): named queries with their
//! own sort. Migration 1's schema; these are its reads and writes.

use rusqlite::{OptionalExtension, Transaction, params};

use crate::StoreError;
use crate::query::Query;
use crate::schema::classify;

/// A saved view's stored message order (spec/02 `saved_views.sort`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ViewSort {
    /// Newest first by `(sort_date, occurrence_id)` (spec/06).
    DateDesc,
    /// Oldest first. Reserved in the pilot: Search has no ascending
    /// date order yet (spec/07 Views).
    DateAsc,
    /// Native BM25 relevance, lowest score first (spec/06).
    Relevance,
}

impl ViewSort {
    fn from_stored(name: &str) -> Result<Self, StoreError> {
        match name {
            "date_desc" => Ok(Self::DateDesc),
            "date_asc" => Ok(Self::DateAsc),
            "relevance" => Ok(Self::Relevance),
            _ => Err(StoreError::corrupt(format!(
                "a saved view's sort is '{name}'"
            ))),
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Self::DateDesc => "date_desc",
            Self::DateAsc => "date_asc",
            Self::Relevance => "relevance",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedView {
    pub view_id: String,
    pub name: String,
    /// The canonical v1 query AST JSON (spec/06).
    pub query_json: String,
    pub sort: ViewSort,
    /// The sidebar position; new views append.
    pub sort_order: i64,
    /// How many times the view changed (spec/02 rule 9).
    pub revision: i64,
}

/// The shared byte bound on a view's name (spec/02).
const MAX_NAME_BYTES: usize = 128;

/// `upsert_saved_view` (spec/07): insert the view or update the row
/// `view_id` names. A no-op write leaves the revision untouched; a real
/// change bumps it (spec/02 rule 9). The AST is stored canonically.
pub fn upsert_saved_view(
    tx: &Transaction,
    view_id: &str,
    name: &str,
    query_json: &str,
    sort: ViewSort,
) -> Result<SavedView, StoreError> {
    valid_name(name)?;
    let canonical = canonical_query(query_json)?;
    if let Some(current) = saved_view(tx, view_id)?
        && current.name == name
        && current.query_json == canonical
        && current.sort == sort
    {
        return Ok(current);
    }
    // A new view appends after the current sidebar; an update keeps it.
    tx.execute(
        "INSERT INTO saved_views (view_id, name, query_json, sort, sort_order, revision)
         VALUES (?1, ?2, ?3, ?4,
                 (SELECT COALESCE(MAX(sort_order), 0) + 1 FROM saved_views), 1)
         ON CONFLICT(view_id) DO UPDATE SET
           name = excluded.name, query_json = excluded.query_json,
           sort = excluded.sort, revision = saved_views.revision + 1",
        params![view_id, name, canonical, sort.name()],
    )
    .map_err(|e| classify(e, "save a view"))?;
    saved_view(tx, view_id)?
        .ok_or_else(|| StoreError::corrupt(format!("saved view {view_id} vanished after writing")))
}

/// `delete_saved_view(id)` (spec/07): remove one view; an unknown id is
/// `NotFound`.
pub fn delete_saved_view(tx: &Transaction, view_id: &str) -> Result<(), StoreError> {
    let removed = tx
        .execute("DELETE FROM saved_views WHERE view_id = ?1", [view_id])
        .map_err(|e| classify(e, "delete a view"))?;
    if removed == 0 {
        return Err(StoreError::not_found(format!("saved view {view_id}")));
    }
    Ok(())
}

/// `saved_views()` (spec/07): every saved view, in sidebar order
/// (`sort_order`, then name, then id).
pub fn saved_views(tx: &Transaction) -> Result<Vec<SavedView>, StoreError> {
    let err = |e| classify(e, "list saved views");
    let mut statement = tx
        .prepare(
            "SELECT view_id, name, query_json, sort, sort_order, revision
             FROM saved_views ORDER BY sort_order, name, view_id",
        )
        .map_err(err)?;
    let mut rows = statement.query([]).map_err(err)?;
    let mut listed = Vec::new();
    while let Some(row) = rows.next().map_err(err)? {
        let sort: String = row.get(3).map_err(err)?;
        listed.push(SavedView {
            view_id: row.get(0).map_err(err)?,
            name: row.get(1).map_err(err)?,
            query_json: row.get(2).map_err(err)?,
            sort: ViewSort::from_stored(&sort)?,
            sort_order: row.get(4).map_err(err)?,
            revision: row.get(5).map_err(err)?,
        });
    }
    Ok(listed)
}

/// One saved view, or `None`; a view window resolves its query here.
pub fn saved_view(tx: &Transaction, view_id: &str) -> Result<Option<SavedView>, StoreError> {
    let err = |e| classify(e, "read a saved view");
    let row = tx
        .query_row(
            "SELECT name, query_json, sort, sort_order, revision
             FROM saved_views WHERE view_id = ?1",
            [view_id],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, String>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                ))
            },
        )
        .optional()
        .map_err(err)?;
    row.map(
        |(name, query_json, sort, sort_order, revision)| -> Result<SavedView, StoreError> {
            Ok(SavedView {
                view_id: view_id.to_owned(),
                name,
                query_json,
                sort: ViewSort::from_stored(&sort)?,
                sort_order,
                revision,
            })
        },
    )
    .transpose()
}

/// The canonical spelling of a checked v1 AST, or `InvalidInput`.
fn canonical_query(query_json: &str) -> Result<String, StoreError> {
    let query = Query::parse_json(query_json).map_err(|error| StoreError::InvalidInput {
        message: format!("query_json is not a v1 query AST: {error}"),
    })?;
    Ok(query.canonical_json().to_owned())
}

/// A view's name is 1 through 128 UTF-8 bytes (spec/02).
fn valid_name(name: &str) -> Result<(), StoreError> {
    if name.is_empty() || name.len() > MAX_NAME_BYTES {
        return Err(StoreError::InvalidInput {
            message: "a saved view's name must be 1 through 128 UTF-8 bytes".into(),
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
