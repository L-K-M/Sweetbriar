//! Immutable checked query and typed v1 view (spec/06). A hand-built
//! `Node` is view data, not a `Query`; only the validating parser creates one.

use std::{error::Error, fmt};

use sb_types::id::Uuid;
use serde_json::{Value, json};

use super::MAX_QUERY_INPUT_BYTES;
use super::{AddressOperand, DatePredicate, DateValue, DomainOperand, TextOperand};
use crate::StoreError;

/// Full-text fields, including the three address-bearing fields.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Field {
    From,
    To,
    Cc,
    Subject,
    Body,
    Attachment,
}

impl Field {
    fn as_str(self) -> &'static str {
        match self {
            Self::From => "from",
            Self::To => "to",
            Self::Cc => "cc",
            Self::Subject => "subject",
            Self::Body => "body",
            Self::Attachment => "attachment",
        }
    }

    pub(super) fn address(self) -> Option<AddressField> {
        match self {
            Self::From => Some(AddressField::From),
            Self::To => Some(AddressField::To),
            Self::Cc => Some(AddressField::Cc),
            _ => None,
        }
    }
}

/// The address-capable subset; `subject is` is not representable here.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AddressField {
    From,
    To,
    Cc,
}

impl AddressField {
    fn as_str(self) -> &'static str {
        match self {
            Self::From => "from",
            Self::To => "to",
            Self::Cc => "cc",
        }
    }
}

/// Chosen-occurrence flag predicates.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Flag {
    Seen,
    Flagged,
    Answered,
    Draft,
}

/// Presence predicates; evaluation's unknown rules belong to the compiler.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Has {
    Attachment,
    Note,
}

/// Exact acquired-byte-length comparisons.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SizeOp {
    Lt,
    Eq,
    Gt,
}

/// Field/operator pairs with their checked scalar operands.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FieldPredicate {
    Contains {
        field: Field,
        value: TextOperand,
    },
    Is {
        field: AddressField,
        address: AddressOperand,
    },
    Domain {
        field: AddressField,
        domain: DomainOperand,
    },
}

/// Typed view of a checked query. Only `Query` guarantees node/group and
/// integer invariants; constructing view data never bypasses its boundary.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Node {
    Text(TextOperand),
    Field(FieldPredicate),
    Flag(Flag),
    Tag(Uuid),
    Has(Has),
    Date(DatePredicate),
    Size { op: SizeOp, bytes: i64 },
    Account(Uuid),
    Mailbox(i64),
    And(Vec<Node>),
    Or(Vec<Node>),
    Not(Box<Node>),
}

/// The eight stable query parse codes (spec/06).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ParseCode {
    UnexpectedToken,
    MissingValue,
    UnclosedQuote,
    UnclosedGroup,
    UnknownKey,
    InvalidOperator,
    InvalidValue,
    Limit,
}

impl ParseCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::UnexpectedToken => "unexpected_token",
            Self::MissingValue => "missing_value",
            Self::UnclosedQuote => "unclosed_quote",
            Self::UnclosedGroup => "unclosed_group",
            Self::UnknownKey => "unknown_key",
            Self::InvalidOperator => "invalid_operator",
            Self::InvalidValue => "invalid_value",
            Self::Limit => "limit",
        }
    }
}

/// Half-open UTF-8 byte span and code; no rejected input is retained.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SearchParseError {
    start: usize,
    end: usize,
    code: ParseCode,
}

impl SearchParseError {
    pub(super) fn new((start, end): (usize, usize), code: ParseCode) -> Self {
        Self { start, end, code }
    }

    pub fn start(&self) -> usize {
        self.start
    }
    pub fn end(&self) -> usize {
        self.end
    }
    pub fn code(&self) -> ParseCode {
        self.code
    }
}

impl fmt::Display for SearchParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "{} at bytes [{},{})",
            self.code.as_str(),
            self.start,
            self.end
        )
    }
}

impl Error for SearchParseError {}

/// Invalid syntax and native token-oracle failure are distinct outcomes.
#[derive(Debug)]
pub enum QueryError {
    Parse(SearchParseError),
    Store(StoreError),
}

impl From<SearchParseError> for QueryError {
    fn from(error: SearchParseError) -> Self {
        Self::Parse(error)
    }
}

impl From<StoreError> for QueryError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

impl fmt::Display for QueryError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Parse(error) => error.fmt(f),
            Self::Store(error) => error.fmt(f),
        }
    }
}

impl Error for QueryError {
    fn source(&self) -> Option<&(dyn Error + 'static)> {
        match self {
            Self::Parse(error) => Some(error),
            Self::Store(error) => Some(error),
        }
    }
}

/// Immutable validated tree with already byte-checked canonical JSON.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Query {
    root: Node,
    canonical: String,
}

impl Query {
    /// Strict v1 JSON only; never reinterpret a failure as text syntax.
    pub fn parse_json(input: &str) -> Result<Self, QueryError> {
        super::json::parse(input, None)
    }

    pub fn root(&self) -> &Node {
        &self.root
    }
    pub fn canonical_json(&self) -> &str {
        &self.canonical
    }

    pub(super) fn finish(root: Node, span: (usize, usize)) -> Result<Self, QueryError> {
        // Like storage JSON, Value's ordered map supplies sorted keys and
        // serde_json owns escaping. Preserve scalar originals and group order.
        let canonical = json!({"v": 1, "q": root.to_json()}).to_string();
        if canonical.len() > MAX_QUERY_INPUT_BYTES {
            return Err(SearchParseError::new(span, ParseCode::Limit).into());
        }
        Ok(Self { root, canonical })
    }
}

impl Node {
    fn to_json(&self) -> Value {
        match self {
            Self::Text(value) => json!({"text": value.value()}),
            Self::Field(FieldPredicate::Contains { field, value }) => {
                json!({"field": field.as_str(), "op": "contains", "value": value.value()})
            }
            Self::Field(FieldPredicate::Is { field, address }) => {
                json!({"field": field.as_str(), "op": "is", "value": address.value()})
            }
            Self::Field(FieldPredicate::Domain { field, domain }) => {
                json!({"field": field.as_str(), "op": "domain", "value": domain.value()})
            }
            Self::Flag(flag) => json!({"flag": match flag {
                Flag::Seen => "seen",
                Flag::Flagged => "flagged",
                Flag::Answered => "answered",
                Flag::Draft => "draft",
            }}),
            Self::Tag(id) => json!({"tag": id.to_string()}),
            Self::Has(kind) => json!({"has": match kind {
                Has::Attachment => "attachment",
                Has::Note => "note",
            }}),
            Self::Date(date) => match date.value() {
                DateValue::Before(value) => json!({"date": {"before": value, "tz": date.zone()}}),
                DateValue::After(value) => json!({"date": {"after": value, "tz": date.zone()}}),
                DateValue::On(value) => json!({"date": {"on": value, "tz": date.zone()}}),
                DateValue::WithinDays { days, clock } => {
                    json!({"date": {"within_days": days, "clock": clock.as_millis(), "tz": date.zone()}})
                }
            },
            Self::Size { op, bytes } => json!({"size": {"op": match op {
                SizeOp::Lt => "lt",
                SizeOp::Eq => "eq",
                SizeOp::Gt => "gt",
            }, "bytes": bytes}}),
            Self::Account(id) => json!({"account": id.to_string()}),
            Self::Mailbox(id) => json!({"mailbox": id}),
            Self::And(children) => {
                json!({"and": children.iter().map(Node::to_json).collect::<Vec<_>>()})
            }
            Self::Or(children) => {
                json!({"or": children.iter().map(Node::to_json).collect::<Vec<_>>()})
            }
            Self::Not(child) => json!({"not": child.to_json()}),
        }
    }
}
