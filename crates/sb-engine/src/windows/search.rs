//! Checked Search specs and same-snapshot page/status reads; coverage and
//! later pages' unknown counts come from the per-generation Search memo.

use super::*;
use crate::{Coverage, SearchIndexState, SearchScope};
use sb_resources::{
    CoverageCounts, Kept, PageRead, Query, ReadStop, Scoped, SearchAnchor, SearchPage,
    SearchScope as StoreScope, StoreError, Transaction, UnknownCount, read_search_date_page,
    read_search_relevance_page, search_date_ids, search_relevance_ids,
};

/// Both complete Store sorts (spec/06); lower native BM25 scores rank first.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchSort {
    Date,
    Relevance,
}

/// Immutable query packet; open validates it off the caller's thread.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchSpec {
    query: SearchQuery,
    scope: SearchScope,
    sort: SearchSort,
}

#[derive(Debug, Clone, PartialEq, Eq)]
enum SearchQuery {
    Json(String),
    Validated(Arc<Query>),
}

impl SearchSpec {
    pub fn new(query_json: String, scope: SearchScope, sort: SearchSort) -> Self {
        Self {
            query: SearchQuery::Json(query_json),
            scope,
            sort,
        }
    }

    fn prepare(mut self) -> Result<Self, EngineError> {
        self.scope.stored()?;
        self.query = match self.query {
            SearchQuery::Json(input) => {
                SearchQuery::Validated(Arc::new(Query::parse_json(&input)?))
            }
            query => query,
        };
        Ok(self)
    }

    fn query(&self) -> Result<Arc<Query>, EngineError> {
        match &self.query {
            SearchQuery::Validated(query) => Ok(Arc::clone(query)),
            SearchQuery::Json(_) => Err(EngineError::internal("unvalidated Search window")),
        }
    }
}

/// Query-independent coverage and exact final unknowns from the page snapshot.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchStatus {
    coverage: Coverage,
    excluded_unknown: u64,
}

impl SearchStatus {
    pub fn coverage(&self) -> &Coverage {
        &self.coverage
    }
    pub fn excluded_unknown(&self) -> u64 {
        self.excluded_unknown
    }
}

struct SearchRead {
    page: SearchPage,
    coverage: Coverage,
}

impl SearchRead {
    fn window(self, window_id: WindowId) -> Window {
        let page = self.page;
        let before = page.rows().first().map(|row| Cursor(row.cursor().into()));
        let after = page.rows().last().map(|row| Cursor(row.cursor().into()));
        let has_more_before = page.has_more_before();
        let has_more_after = page.has_more_after();
        let shown = page.rows().len() as u64;
        let total = if has_more_before || has_more_after {
            Count::AtLeast(shown + u64::from(has_more_before) + u64::from(has_more_after))
        } else {
            Count::Exact(shown)
        };
        let status = SearchStatus {
            coverage: self.coverage,
            excluded_unknown: page.excluded_unknown(),
        };
        Window {
            window_id,
            generation: page.generation(),
            before,
            after,
            has_more_before,
            has_more_after,
            total,
            search: Some(status),
            rows: page
                .into_rows()
                .into_iter()
                .map(|hit| list_row(hit.into_parts().0))
                .collect(),
        }
    }
}

impl Engine {
    pub(super) async fn open_search_window(
        &self,
        spec: SearchSpec,
        anchor: Anchor,
        limit: u32,
    ) -> Result<Window, EngineError> {
        check_limit(limit)?;
        let spec = self.compute(move || spec.prepare()).await?;
        let read = self.search_page(spec.clone(), anchor, limit, None).await?;
        let windows = Arc::clone(self.windows());
        let cleanup = self.detached();
        #[cfg(test)]
        let index = Arc::clone(self.indexer());
        let opened = self
            .compute(move || {
                let id = windows.register(WindowSpec::Search(spec), read.page.generation());
                #[cfg(test)]
                index.event(crate::index::Event::SearchRegistered {
                    window_id: id.get(),
                });
                Ok(Opened {
                    window: Some(read.window(id)),
                    windows,
                    cleanup,
                })
            })
            .await?;
        Ok(opened.into_window())
    }

    pub(super) async fn read_search_window(
        &self,
        id: WindowId,
        spec: SearchSpec,
        anchor: Anchor,
        limit: u32,
        generation: u64,
    ) -> Result<Window, EngineError> {
        Ok(self
            .search_page(spec, anchor, limit, Some(generation))
            .await?
            .window(id))
    }

    async fn search_page(
        &self,
        spec: SearchSpec,
        anchor: Anchor,
        limit: u32,
        at: Option<u64>,
    ) -> Result<SearchRead, EngineError> {
        let query = spec.query()?;
        let scope = spec.scope.stored()?;
        let outcome_anchor = match &anchor {
            Anchor::Around(id) => Anchor::Around(*id),
            _ => Anchor::Top,
        };
        let native = match anchor {
            Anchor::Top => SearchAnchor::Top,
            Anchor::Around(id) => SearchAnchor::Around(id),
            Anchor::After(cursor) => SearchAnchor::After(cursor.0),
            Anchor::Before(cursor) => SearchAnchor::Before(cursor.0),
        };
        // spec/07: only later pages reuse a count; a first page counts.
        let memo = Arc::clone(self.search_memo());
        let later = at.is_some() && !matches!(native, SearchAnchor::Top);
        let unknown = if later {
            memo.unknown(query.canonical_json(), &scope)
        } else {
            UnknownCount::Count
        };
        let page_query = Arc::clone(&query);
        let page_scope = scope.clone();
        let Scoped::Done((coverage, read)) = self
            .search_snapshot(scope.clone(), None, move |tx, counts| {
                let (query, scope, limit) = (&page_query, page_scope, limit as usize);
                let kept = Kept {
                    unknown,
                    coverage: Some(counts),
                };
                match spec.sort {
                    SearchSort::Date => {
                        read_search_date_page(tx, query, scope, native, limit, at, kept)
                    }
                    SearchSort::Relevance => {
                        read_search_relevance_page(tx, query, scope, native, limit, at, kept)
                    }
                }
            })
            .await?
        else {
            // Page reads are never scoped to a window's stop; ids are.
            return Err(EngineError::internal("a Search page read was stopped"));
        };
        if let PageRead::Ready(page) = &read {
            #[cfg(test)]
            if matches!(unknown, UnknownCount::Reuse { generation, .. } if generation == page.generation())
            {
                memo.reused_unknown();
            }
            memo.keep_unknown(
                query.canonical_json(),
                scope,
                page.generation(),
                page.excluded_unknown(),
            );
        }
        Ok(SearchRead {
            coverage,
            page: store_outcome(read, &outcome_anchor)?,
        })
    }

    pub(super) async fn search_window_ids(
        &self,
        spec: SearchSpec,
        stop: ReadStop,
    ) -> Result<Scoped<WindowIds>, EngineError> {
        let query = spec.query()?;
        let scope = spec.scope.stored()?;
        let read = self
            .search_snapshot(scope.clone(), Some(stop), move |tx, counts| {
                match spec.sort {
                    SearchSort::Date => search_date_ids(tx, &query, scope, None, Some(counts)),
                    SearchSort::Relevance => {
                        search_relevance_ids(tx, &query, scope, None, Some(counts))
                    }
                }
            })
            .await?;
        let Scoped::Done((_, read)) = read else {
            return Ok(Scoped::Stopped);
        };
        let (generation, ids) = store_outcome(read, &Anchor::Top)?;
        Ok(Scoped::Done(WindowIds { generation, ids }))
    }

    /// Page/ids and capability share one snapshot; failure state is never
    /// kept. The body gets coverage's scope counts, its driven-read proof.
    /// `stop`, when given, scopes the read's SQLite work to the owning
    /// window's lifetime — ids reads only; a page read passes none.
    async fn search_snapshot<T, F>(
        &self,
        scope: StoreScope,
        stop: Option<ReadStop>,
        body: F,
    ) -> Result<Scoped<(Coverage, T)>, EngineError>
    where
        T: Send + 'static,
        F: FnOnce(&Transaction, &CoverageCounts) -> Result<T, StoreError> + Send + 'static,
    {
        let index = Arc::clone(self.indexer());
        let memo = Arc::clone(self.search_memo());
        self.read_through(move |resources| {
            index.readable()?;
            let run = |tx: &Transaction| -> Result<(Coverage, T), StoreError> {
                let (coverage, counts) = memo.coverage(tx, scope)?;
                if coverage.index_state == SearchIndexState::BlockedNewerExtractor {
                    return Err(StoreError::Unavailable {
                        reason: "Search needs a newer app for this index. Update Sweetbriar."
                            .into(),
                        io_kind: None,
                    });
                }
                #[cfg(test)]
                index.event(crate::index::Event::SearchReadCaptured);
                Ok((coverage, body(tx, &counts)?))
            };
            let result = match &stop {
                Some(stop) => resources.read_scoped(stop, run),
                None => resources.read(run).map(Scoped::Done),
            };
            // A stopped read has no index result left to fence.
            if matches!(&result, Ok(Scoped::Stopped)) {
                return result;
            }
            index.readable()?;
            result
        })
        .await
        .map_err(|error| match error {
            EngineError::InvalidInput { message, .. } => {
                EngineError::invalid_input("anchor", message)
            }
            error => error,
        })
    }
}
