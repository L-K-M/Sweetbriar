//! Owned Search window packets; validation and snapshots remain below Service.

use super::{Coverage, SearchScope};

/// Raw checked-JSON input, outer scope and sort for a new window handle.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SearchSpec {
    pub query_json: String,
    pub scope: SearchScope,
    pub sort: SearchSort,
}

impl SearchSpec {
    pub(crate) fn into_service(self) -> sb_service::SearchSpec {
        sb_service::SearchSpec {
            query_json: self.query_json,
            scope: self.scope.into_service(),
            sort: match self.sort {
                SearchSort::Date => sb_service::SearchSort::Date,
                SearchSort::Relevance => sb_service::SearchSort::Relevance,
            },
        }
    }
}

/// Date descending or native numeric relevance ascending, with stable id ties.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum SearchSort {
    Date,
    Relevance,
}

/// Coverage and excluded unknowns from the same snapshot as the page's rows.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct SearchStatus {
    pub coverage: Coverage,
    pub excluded_unknown: u64,
}

impl SearchStatus {
    pub(crate) fn from_service(value: &sb_service::SearchStatus) -> Self {
        Self {
            coverage: Coverage::from_service(&value.coverage),
            excluded_unknown: value.excluded_unknown,
        }
    }
}
