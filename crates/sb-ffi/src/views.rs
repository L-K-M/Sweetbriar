//! Saved-view DTOs and commands over FFI (spec/07 "Commands",
//! "Queries", Views): records mirroring `sb-service`'s DTOs field by
//! field, and the exported `Core` methods. The commands run through
//! `Core::command` under a shell-made idempotency key; the list query
//! races the token at every wait (`Core::query`).

use std::sync::Arc;

use crate::{CancelToken, Core, CoreError, Receipt};

/// A saved view's stored message order (spec/02).
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SavedViewSort {
    DateDesc,
    /// Reserved in the pilot: saving it is `InvalidInput` on `sort`
    /// (spec/07 Views).
    DateAsc,
    Relevance,
}

impl From<sb_service::SavedViewSort> for SavedViewSort {
    fn from(sort: sb_service::SavedViewSort) -> Self {
        match sort {
            sb_service::SavedViewSort::DateDesc => Self::DateDesc,
            sb_service::SavedViewSort::DateAsc => Self::DateAsc,
            sb_service::SavedViewSort::Relevance => Self::Relevance,
        }
    }
}

impl From<SavedViewSort> for sb_service::SavedViewSort {
    fn from(sort: SavedViewSort) -> Self {
        match sort {
            SavedViewSort::DateDesc => Self::DateDesc,
            SavedViewSort::DateAsc => Self::DateAsc,
            SavedViewSort::Relevance => Self::Relevance,
        }
    }
}

/// `upsert_saved_view`'s parameters (spec/07 Views): the view to
/// update, or `None` to create one, with its name, canonical v1 query
/// JSON (spec/06) and sort.
#[derive(Debug, Clone, uniffi::Record)]
pub struct SavedViewParams {
    pub view_id: Option<String>,
    pub name: String,
    pub query_json: String,
    pub sort: SavedViewSort,
}

impl SavedViewParams {
    fn into_service(self) -> sb_service::SavedViewParams {
        sb_service::SavedViewParams {
            view_id: self.view_id,
            name: self.name,
            query_json: self.query_json,
            sort: self.sort.into(),
        }
    }
}

/// One saved view as `saved_views()` reports it (spec/07 Views), in
/// sidebar order (new views append).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SavedView {
    pub view_id: String,
    pub name: String,
    pub query_json: String,
    pub sort: SavedViewSort,
    pub revision: i64,
}

impl SavedView {
    /// Mirror the service's view field by field.
    fn from_service(view: &sb_service::SavedView) -> Self {
        Self {
            view_id: view.view_id.clone(),
            name: view.name.clone(),
            query_json: view.query_json.clone(),
            sort: view.sort.into(),
            revision: view.revision,
        }
    }
}

#[uniffi::export]
impl Core {
    /// `upsert_saved_view(params)` (spec/07 Views): insert the view or
    /// update the row `params.view_id` names. The receipt names the
    /// stored id; a repeated key answers its stored receipt. Like every
    /// command it honours the token only until it holds the core.
    pub async fn upsert_saved_view(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        params: SavedViewParams,
    ) -> Result<Receipt, CoreError> {
        let receipt = self
            .command(&token, async move |core| {
                core.upsert_saved_view(&idempotency_key, params.into_service())
                    .await
            })
            .await?;
        Ok(Receipt::from_service(&receipt))
    }

    /// `delete_saved_view(id)` (spec/07 Views): remove one view; an
    /// unknown id is `NotFound`. The receipt names the id.
    pub async fn delete_saved_view(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        view_id: String,
    ) -> Result<Receipt, CoreError> {
        let receipt = self
            .command(&token, async move |core| {
                core.delete_saved_view(&idempotency_key, &view_id).await
            })
            .await?;
        Ok(Receipt::from_service(&receipt))
    }

    /// `saved_views()` (spec/07 Queries): every view in sidebar order.
    pub async fn saved_views(&self, token: Arc<CancelToken>) -> Result<Vec<SavedView>, CoreError> {
        let views = self
            .query(&token, async |core| core.saved_views().await)
            .await?;
        Ok(views.iter().map(SavedView::from_service).collect())
    }
}

#[cfg(test)]
mod tests;
