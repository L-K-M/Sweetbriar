//! Saved views (spec/02, spec/07 Views): `upsert_saved_view`,
//! `delete_saved_view`, `saved_views()`, and the query a `View(id)`
//! window resolves. Opening one runs the same checked Search machinery
//! as `Search`, so its results equal its query's.

use sb_resources::{
    Query, QueryError, Receipt, SavedView, Uuid, ViewSort, WritePriority, delete_saved_view,
    run_once, saved_view, saved_views, upsert_saved_view,
};

use crate::engine::validate_idempotency_key;
use crate::{Engine, EngineError, SearchScope, SearchSort, SearchSpec};

/// The commands' names in `command_receipts` (spec/07).
const UPSERT_SAVED_VIEW: &str = "upsert_saved_view";
const DELETE_SAVED_VIEW: &str = "delete_saved_view";

/// A view's name is 1 through 128 UTF-8 bytes (spec/02).
const MAX_NAME_BYTES: usize = 128;

/// `upsert_saved_view`'s parameters (spec/07 Views): `view_id` absent
/// creates a view, present updates the one it names.
#[derive(Debug, Clone)]
pub struct SavedViewParams {
    /// The view to update, or `None` to create one.
    pub view_id: Option<String>,
    pub name: String,
    /// A v1 query AST (spec/06), stored canonically.
    pub query_json: String,
    pub sort: ViewSort,
}

impl Engine {
    /// `upsert_saved_view(params)` (spec/07): insert the view or update
    /// the row `params.view_id` names. The query JSON must be a v1 AST
    /// and is stored canonically. The receipt names the stored id.
    pub async fn upsert_saved_view(
        &self,
        idempotency_key: &str,
        params: SavedViewParams,
    ) -> Result<Receipt, EngineError> {
        validate_idempotency_key(idempotency_key)?;
        let view_id = match params.view_id {
            Some(id) => {
                valid_view_id(&id)?;
                id
            }
            None => self.resources().new_uuid_string(),
        };
        valid_name(&params.name)?;
        // Parse before the write so an invalid AST keeps its UTF-8 span,
        // and store the canonical spelling (spec/02 rule 7).
        let query = Query::parse_json(&params.query_json)?;
        let query_json = query.canonical_json().to_owned();
        if matches!(params.sort, ViewSort::DateAsc) {
            return Err(unsupported_sort());
        }
        let now = self.resources().now();
        let (key, name, sort) = (idempotency_key.to_owned(), params.name, params.sort);
        self.command(move |worker| async move {
            worker
                .resources()
                .write(WritePriority::User, move |tx| {
                    run_once(tx, &key, UPSERT_SAVED_VIEW, now, |tx| {
                        let saved = upsert_saved_view(tx, &view_id, &name, &query_json, sort)?;
                        Ok(encode_id("view_id", &saved.view_id))
                    })
                })
                .await
                .map_err(EngineError::from)
        })
        .await
    }

    /// `delete_saved_view(id)` (spec/07): remove one saved view; an
    /// unknown id is `NotFound`. The receipt answers `{"view_id": ...}`.
    pub async fn delete_saved_view(
        &self,
        idempotency_key: &str,
        view_id: &str,
    ) -> Result<Receipt, EngineError> {
        validate_idempotency_key(idempotency_key)?;
        if view_id.is_empty() {
            return Err(EngineError::invalid_input(
                "view_id",
                "a saved view id is required",
            ));
        }
        let now = self.resources().now();
        let (key, id) = (idempotency_key.to_owned(), view_id.to_owned());
        self.command(move |worker| async move {
            worker
                .resources()
                .write(WritePriority::User, move |tx| {
                    run_once(tx, &key, DELETE_SAVED_VIEW, now, |tx| {
                        delete_saved_view(tx, &id)?;
                        Ok(encode_id("view_id", &id))
                    })
                })
                .await
                .map_err(EngineError::from)
        })
        .await
    }

    /// `saved_views()` (spec/07): every saved view in sidebar order.
    pub async fn saved_views(&self) -> Result<Vec<SavedView>, EngineError> {
        self.read_through(|resources| resources.read(saved_views))
            .await
    }

    /// The checked query a `View(id)` window runs (spec/07): the
    /// view's stored AST over scope All, in the view's sort. An unknown
    /// view is `NotFound`; the reserved `date_asc` sort is refused.
    pub(crate) async fn view_query(&self, view_id: &str) -> Result<SearchSpec, EngineError> {
        let id = view_id.to_owned();
        let view = self
            .read_through(move |resources| resources.read(|tx| saved_view(tx, &id)))
            .await?
            .ok_or_else(|| EngineError::NotFound {
                what: format!("saved view {view_id}"),
            })?;
        // Only a direct database edit makes this fail; a parse error
        // names stored damage, not the caller's input.
        let query = Query::parse_json(&view.query_json).map_err(|error| match error {
            QueryError::Parse(_) => {
                EngineError::corrupt("a saved view's query_json is not a v1 query AST")
            }
            QueryError::Store(error) => EngineError::from(error),
        })?;
        let sort = match view.sort {
            ViewSort::DateDesc => SearchSort::Date,
            ViewSort::Relevance => SearchSort::Relevance,
            ViewSort::DateAsc => return Err(unsupported_sort()),
        };
        Ok(SearchSpec::from_query(query, SearchScope::All, sort))
    }
}

fn unsupported_sort() -> EngineError {
    EngineError::invalid_input(
        "sort",
        "a saved view orders its messages date_desc or relevance; date_asc awaits \
         an ascending date sort",
    )
}

fn valid_view_id(view_id: &str) -> Result<(), EngineError> {
    view_id.parse::<Uuid>().map_err(|_| {
        EngineError::invalid_input("view_id", "a saved view id is a lowercase hyphenated UUID")
    })?;
    Ok(())
}

fn valid_name(name: &str) -> Result<(), EngineError> {
    if name.is_empty() || name.len() > MAX_NAME_BYTES {
        return Err(EngineError::invalid_input(
            "name",
            format!("a saved view's name must be 1 through {MAX_NAME_BYTES} UTF-8 bytes"),
        ));
    }
    Ok(())
}

fn encode_id(field: &str, id: &str) -> String {
    let mut body = serde_json::Map::new();
    body.insert(field.to_owned(), id.into());
    serde_json::Value::Object(body).to_string()
}

#[cfg(test)]
pub(crate) mod tests;
