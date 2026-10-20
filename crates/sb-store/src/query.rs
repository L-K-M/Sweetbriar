//! `sb-store::query` — the search query vocabulary (spec/06). M1.6f0
//! supplies only the validated date operands; the AST, JSON validation
//! and text parser land in M1.6f/g.

mod dates;

pub use dates::{DateBounds, DateError, DatePredicate, DateValue};
