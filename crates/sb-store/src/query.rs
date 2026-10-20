//! `sb-store::query` — the search query vocabulary (spec/06). M1.6f0
//! supplies validated dates, M1.6f0b addresses, and M1.6f0c native text
//! literals and shared UUID parsing. The complete AST and query parsers
//! follow in M1.6f/g.

mod addresses;
mod dates;
mod text;

pub use addresses::{AddressError, AddressOperand, DomainOperand};
pub use dates::{DateBounds, DateError, DatePredicate, DateValue};
pub use text::{TextError, TextOperand};

/// The shared UTF-8 byte ceiling for query inputs and free-text
/// address/domain/literal operands (spec/06), checked before parsing.
/// This is an archive-query ceiling, not an SMTP envelope or local
/// delivery limit; structured dates have their own bounded shapes.
pub const MAX_QUERY_INPUT_BYTES: usize = 16_384;
