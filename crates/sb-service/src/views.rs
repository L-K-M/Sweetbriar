//! Saved-view DTOs and `Core`'s view surface (spec/02, spec/07):
//! commands, `saved_views()` and the `View` window spec. Mirrors only
//! (spec/01 rule 3).

use sb_engine::{SavedView as EngineSavedView, ViewSort};

use crate::{Core, CoreError, Receipt};

/// A saved view's stored message order (spec/02).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SavedViewSort {
    DateDesc,
    /// Reserved in the pilot (spec/07 Views).
    DateAsc,
    Relevance,
}

impl SavedViewSort {
    fn into_engine(self) -> ViewSort {
        match self {
            Self::DateDesc => ViewSort::DateDesc,
            Self::DateAsc => ViewSort::DateAsc,
            Self::Relevance => ViewSort::Relevance,
        }
    }

    fn from_engine(sort: ViewSort) -> Self {
        match sort {
            ViewSort::DateDesc => Self::DateDesc,
            ViewSort::DateAsc => Self::DateAsc,
            ViewSort::Relevance => Self::Relevance,
        }
    }
}

/// `upsert_saved_view`'s parameters (spec/07 Views).
#[derive(Debug, Clone)]
pub struct SavedViewParams {
    /// The view to update, or `None` to create one.
    pub view_id: Option<String>,
    pub name: String,
    /// A v1 query AST (spec/06).
    pub query_json: String,
    pub sort: SavedViewSort,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SavedView {
    pub view_id: String,
    pub name: String,
    pub query_json: String,
    pub sort: SavedViewSort,
    pub revision: i64,
}

impl SavedView {
    fn from_engine(view: &EngineSavedView) -> Self {
        Self {
            view_id: view.view_id.clone(),
            name: view.name.clone(),
            query_json: view.query_json.clone(),
            sort: SavedViewSort::from_engine(view.sort),
            revision: view.revision,
        }
    }
}

impl Core {
    /// `upsert_saved_view(params)` (spec/07 Views): insert the view or
    /// update the row `params.view_id` names. The receipt names the id.
    pub async fn upsert_saved_view(
        &self,
        idempotency_key: &str,
        params: SavedViewParams,
    ) -> Result<Receipt, CoreError> {
        let receipt = self
            .engine()
            .upsert_saved_view(
                idempotency_key,
                sb_engine::SavedViewParams {
                    view_id: params.view_id,
                    name: params.name,
                    query_json: params.query_json,
                    sort: params.sort.into_engine(),
                },
            )
            .await?;
        Ok(Receipt::from_engine(&receipt))
    }

    /// `delete_saved_view(id)` (spec/07 Views): remove one view; an
    /// unknown id is `NotFound`.
    pub async fn delete_saved_view(
        &self,
        idempotency_key: &str,
        view_id: &str,
    ) -> Result<Receipt, CoreError> {
        let receipt = self
            .engine()
            .delete_saved_view(idempotency_key, view_id)
            .await?;
        Ok(Receipt::from_engine(&receipt))
    }

    /// `saved_views()` (spec/07 Queries): every view in sidebar order.
    pub async fn saved_views(&self) -> Result<Vec<SavedView>, CoreError> {
        Ok(self
            .engine()
            .saved_views()
            .await?
            .iter()
            .map(SavedView::from_engine)
            .collect())
    }
}
