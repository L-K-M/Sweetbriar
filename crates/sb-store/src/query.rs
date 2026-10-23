//! Checked v1 search queries and scalar operands (spec/06). JSON and text
//! parsing produce one immutable typed query with bounded canonical formats.

mod addresses;
mod ast;
mod counts;
mod dates;
mod format;
mod json;
mod syntax;
mod text;

pub use addresses::{AddressError, AddressOperand, DomainOperand};
pub use ast::{
    AddressField, Field, FieldPredicate, Flag, Has, Node, ParseCode, Query, QueryError,
    SearchParseError, SizeOp,
};
pub use counts::{
    QueryCounts, SearchAnchor, SearchIndexState, SearchLimitation, SearchPage,
    SearchRebuildProgress, SearchScope, StoredSearchCoverage, StoredSearchRow, counts,
    read_search_coverage, read_search_date_page, read_search_relevance_page, search_date_ids,
    search_relevance_ids,
};
pub use dates::{DateBounds, DateError, DatePredicate, DateValue};
pub use format::TextFormatError;
pub use syntax::ParseContext;
pub use text::{TextError, TextOperand};

pub(crate) use counts::{Inputs, inputs};

/// The shared UTF-8 byte ceiling for query inputs and free-text
/// address/domain/literal operands (spec/06), checked before parsing.
/// This is an archive-query ceiling, not an SMTP envelope or local
/// delivery limit; structured dates have their own bounded shapes.
pub const MAX_QUERY_INPUT_BYTES: usize = 16_384;

const MAX_QUERY_NODES: usize = 256;
const MAX_QUERY_DEPTH: usize = 32;

fn source_text_error(error: TextError, span: (usize, usize)) -> QueryError {
    match error {
        TextError::Store(error) => QueryError::Store(error),
        TextError::Limit => SearchParseError::new(span, ParseCode::Limit).into(),
        TextError::Invalid(_) => SearchParseError::new(span, ParseCode::InvalidValue).into(),
    }
}

#[cfg(test)]
mod syntax_tests;
#[cfg(test)]
mod tests;
