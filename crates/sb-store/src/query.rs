//! Checked v1 search queries and scalar operands (spec/06). JSON parsing
//! produces an immutable typed query; text syntax follows in M1.6g.

mod addresses;
mod ast;
mod dates;
mod json;
mod text;

pub use addresses::{AddressError, AddressOperand, DomainOperand};
pub use ast::{
    AddressField, Field, FieldPredicate, Flag, Has, Node, ParseCode, Query, QueryError,
    SearchParseError, SizeOp,
};
pub use dates::{DateBounds, DateError, DatePredicate, DateValue};
pub use text::{TextError, TextOperand};

/// The shared UTF-8 byte ceiling for query inputs and free-text
/// address/domain/literal operands (spec/06), checked before parsing.
/// This is an archive-query ceiling, not an SMTP envelope or local
/// delivery limit; structured dates have their own bounded shapes.
pub const MAX_QUERY_INPUT_BYTES: usize = 16_384;

const MAX_QUERY_NODES: usize = 256;
const MAX_QUERY_DEPTH: usize = 32;

#[cfg(test)]
mod tests;
