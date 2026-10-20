//! `sb-store::query` — the search query vocabulary (spec/06). M1.6f0
//! supplies the validated date operands and M1.6f0b the validated
//! address operands; the AST, JSON validation and text parser land in
//! M1.6f/g.

mod addresses;
mod dates;

pub use addresses::{AddressError, AddressOperand, DomainOperand};
pub use dates::{DateBounds, DateError, DatePredicate, DateValue};

/// The shared query input ceiling in UTF-8 bytes (spec/06). Every
/// operand rejects input longer than this before any parsing; it is a
/// byte count, not a char count, and an archive-query ceiling — no SMTP
/// envelope or 64-byte local delivery limit applies.
pub const MAX_QUERY_INPUT_BYTES: usize = 16_384;
