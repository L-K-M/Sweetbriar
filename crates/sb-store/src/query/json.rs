//! Serde owns JSON decoding. Borrowed raw members retain duplicate names
//! and exact value spans until the bounded v1 schema walk rejects them.

use std::{fmt, str::FromStr};

use sb_types::{id::Uuid, time::Timestamp};
use serde::{
    Deserialize, Deserializer,
    de::{MapAccess, Visitor},
};
use serde_json::value::RawValue;

use super::text::{TextValidator, validate};
use super::{
    AddressError, AddressOperand, DateError, DatePredicate, DateValue, DomainOperand, Field,
    FieldPredicate, Flag, Has, MAX_QUERY_DEPTH, MAX_QUERY_INPUT_BYTES, MAX_QUERY_NODES, Node,
    ParseCode, Query, QueryError, SearchParseError, SizeOp, TextOperand,
};

type Span = (usize, usize);

struct Members<'a>(Vec<(String, &'a RawValue)>);

impl<'de> Deserialize<'de> for Members<'de> {
    fn deserialize<D: Deserializer<'de>>(de: D) -> Result<Self, D::Error> {
        struct Entries;
        impl<'de> Visitor<'de> for Entries {
            type Value = Members<'de>;
            fn expecting(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
                f.write_str("a JSON object")
            }
            fn visit_map<A: MapAccess<'de>>(self, mut map: A) -> Result<Self::Value, A::Error> {
                let mut entries = Vec::new();
                while let Some(entry) = map.next_entry::<String, &'de RawValue>()? {
                    entries.push(entry);
                }
                Ok(Members(entries))
            }
        }
        de.deserialize_map(Entries)
    }
}

struct Object<'a> {
    entries: Vec<(String, &'a RawValue)>,
    span: Span,
}

impl<'a> Object<'a> {
    fn get(&self, key: &str) -> Option<&'a RawValue> {
        self.entries
            .iter()
            .find(|(name, _)| name == key)
            .map(|(_, raw)| *raw)
    }

    fn required(&self, key: &str) -> Result<&'a RawValue, QueryError> {
        self.get(key)
            .ok_or_else(|| failure(self.span, ParseCode::MissingValue))
    }
}

fn failure(span: Span, code: ParseCode) -> QueryError {
    SearchParseError::new(span, code).into()
}

pub(super) fn parse(input: &str, oracle: Option<TextValidator>) -> Result<Query, QueryError> {
    if input.len() > MAX_QUERY_INPUT_BYTES {
        let start = input.floor_char_boundary(MAX_QUERY_INPUT_BYTES);
        return Err(failure(
            (start, input.ceil_char_boundary(start + 1)),
            ParseCode::Limit,
        ));
    }
    // RawValue also validates trailing input, but does not build an unbounded
    // recursive Value tree before our own AST-depth/node checks run.
    let raw: &RawValue = serde_json::from_str(input).map_err(|e| syntax_error(input, &e))?;
    let mut parser = Parser {
        input,
        left: MAX_QUERY_NODES,
        oracle,
    };
    let envelope = parser.object(raw, &["v", "q"])?;
    let version = envelope.required("v")?;
    if parser.integer::<u64>(version)? != 1 {
        return Err(parser.error(version, ParseCode::InvalidValue));
    }
    let q = envelope.required("q")?;
    let root = parser.node(q, 1)?;
    Query::finish(root, parser.span(q))
}

struct Parser<'a> {
    input: &'a str,
    left: usize,
    oracle: Option<TextValidator>,
}

impl<'a> Parser<'a> {
    fn span(&self, raw: &RawValue) -> Span {
        // Every raw value is borrowed from this input by the Serde visitor.
        let start = raw.get().as_ptr() as usize - self.input.as_ptr() as usize;
        (start, start + raw.get().len())
    }

    fn error(&self, raw: &RawValue, code: ParseCode) -> QueryError {
        failure(self.span(raw), code)
    }

    fn decode<T: Deserialize<'a>>(&self, raw: &'a RawValue) -> Result<T, QueryError> {
        serde_json::from_str(raw.get()).map_err(|error| {
            if error.is_data() {
                return self.error(raw, ParseCode::InvalidValue);
            }
            // RawValue can retain unpaired surrogate escapes. String/key
            // decoding diagnoses them later, in coordinates of this slice.
            let syntax = syntax_error(raw.get(), &error);
            let base = self.span(raw).0;
            failure((base + syntax.start(), base + syntax.end()), syntax.code())
        })
    }

    fn integer<T: FromStr>(&self, raw: &RawValue) -> Result<T, QueryError> {
        // JSON syntax is already checked. Standard integer parsing rejects
        // fractions/exponents and overflow, while preserving integer -0 as 0.
        raw.get()
            .parse()
            .map_err(|_| self.error(raw, ParseCode::InvalidValue))
    }

    fn object(&self, raw: &'a RawValue, allowed: &[&str]) -> Result<Object<'a>, QueryError> {
        let Members(entries) = self.decode(raw)?;
        for (index, (name, value)) in entries.iter().enumerate() {
            if !allowed.contains(&name.as_str()) {
                return Err(self.error(value, ParseCode::UnknownKey));
            }
            if entries[..index].iter().any(|(seen, _)| seen == name) {
                return Err(self.error(value, ParseCode::InvalidValue));
            }
        }
        Ok(Object {
            entries,
            span: self.span(raw),
        })
    }

    fn node(&mut self, raw: &'a RawValue, depth: usize) -> Result<Node, QueryError> {
        if depth > MAX_QUERY_DEPTH || self.left == 0 {
            return Err(self.error(raw, ParseCode::Limit));
        }
        self.left -= 1;
        let object = self.object(
            raw,
            &[
                "text", "field", "op", "value", "flag", "tag", "has", "date", "size", "account",
                "mailbox", "and", "or", "not",
            ],
        )?;
        if object
            .entries
            .iter()
            .any(|(name, _)| matches!(name.as_str(), "field" | "op" | "value"))
        {
            if object
                .entries
                .iter()
                .any(|(name, _)| !matches!(name.as_str(), "field" | "op" | "value"))
            {
                return Err(self.error(raw, ParseCode::InvalidValue));
            }
            return self.field(object);
        }
        if object.entries.len() != 1 {
            let code = if object.entries.is_empty() {
                ParseCode::MissingValue
            } else {
                ParseCode::InvalidValue
            };
            return Err(self.error(raw, code));
        }
        let (key, value) = &object.entries[0];
        match key.as_str() {
            "text" => self.fulltext(value).map(Node::Text),
            "flag" => Ok(Node::Flag(match self.decode::<String>(value)?.as_str() {
                "seen" => Flag::Seen,
                "flagged" => Flag::Flagged,
                "answered" => Flag::Answered,
                "draft" => Flag::Draft,
                _ => return Err(self.error(value, ParseCode::InvalidValue)),
            })),
            "has" => Ok(Node::Has(match self.decode::<String>(value)?.as_str() {
                "attachment" => Has::Attachment,
                "note" => Has::Note,
                _ => return Err(self.error(value, ParseCode::InvalidValue)),
            })),
            "tag" | "account" => {
                let id = self
                    .decode::<String>(value)?
                    .parse::<Uuid>()
                    .map_err(|_| self.error(value, ParseCode::InvalidValue))?;
                Ok(if key == "tag" {
                    Node::Tag(id)
                } else {
                    Node::Account(id)
                })
            }
            "mailbox" => {
                let id = self.integer::<i64>(value)?;
                if id <= 0 {
                    return Err(self.error(value, ParseCode::InvalidValue));
                }
                Ok(Node::Mailbox(id))
            }
            "date" => self.date(value),
            "size" => {
                let size = self.object(value, &["op", "bytes"])?;
                let op_raw = size.required("op")?;
                let bytes_raw = size.required("bytes")?;
                let op = match self.decode::<String>(op_raw)?.as_str() {
                    "lt" => SizeOp::Lt,
                    "eq" => SizeOp::Eq,
                    "gt" => SizeOp::Gt,
                    _ => return Err(self.error(op_raw, ParseCode::InvalidValue)),
                };
                let bytes = self.integer::<i64>(bytes_raw)?;
                if bytes < 0 {
                    return Err(self.error(bytes_raw, ParseCode::InvalidValue));
                }
                Ok(Node::Size { op, bytes })
            }
            "and" | "or" => {
                let children = self.decode::<Vec<&RawValue>>(value)?;
                if key == "or" && children.is_empty() {
                    return Err(self.error(value, ParseCode::InvalidValue));
                }
                let nodes = children
                    .into_iter()
                    .map(|child| self.node(child, depth + 1))
                    .collect::<Result<Vec<_>, _>>()?;
                Ok(if key == "and" {
                    Node::And(nodes)
                } else {
                    Node::Or(nodes)
                })
            }
            "not" => Ok(Node::Not(Box::new(self.node(value, depth + 1)?))),
            _ => unreachable!("the checked non-field singleton has a v1 node key"),
        }
    }

    fn field(&mut self, object: Object<'a>) -> Result<Node, QueryError> {
        let field_raw = object.required("field")?;
        let op_raw = object.required("op")?;
        let value_raw = object.required("value")?;
        let field = match self.decode::<String>(field_raw)?.as_str() {
            "from" => Field::From,
            "to" => Field::To,
            "cc" => Field::Cc,
            "subject" => Field::Subject,
            "body" => Field::Body,
            "attachment" => Field::Attachment,
            _ => return Err(self.error(field_raw, ParseCode::InvalidValue)),
        };
        let predicate = match self.decode::<String>(op_raw)?.as_str() {
            "contains" => FieldPredicate::Contains {
                field,
                value: self.fulltext(value_raw)?,
            },
            op @ ("is" | "domain") => {
                let field = field
                    .address()
                    .ok_or_else(|| self.error(op_raw, ParseCode::InvalidOperator))?;
                let value = self.decode::<String>(value_raw)?;
                let rejected = |error| {
                    self.error(
                        value_raw,
                        match error {
                            AddressError::Limit(_) => ParseCode::Limit,
                            _ => ParseCode::InvalidValue,
                        },
                    )
                };
                if op == "is" {
                    FieldPredicate::Is {
                        field,
                        address: AddressOperand::parse(&value).map_err(rejected)?,
                    }
                } else {
                    FieldPredicate::Domain {
                        field,
                        domain: DomainOperand::parse(&value).map_err(rejected)?,
                    }
                }
            }
            _ => return Err(self.error(op_raw, ParseCode::InvalidValue)),
        };
        Ok(Node::Field(predicate))
    }

    fn date(&self, raw: &'a RawValue) -> Result<Node, QueryError> {
        let object = self.object(
            raw,
            &["before", "after", "on", "within_days", "tz", "clock"],
        )?;
        let ops = ["before", "after", "on", "within_days"]
            .into_iter()
            .filter_map(|key| object.get(key).map(|value| (key, value)))
            .collect::<Vec<_>>();
        if ops.len() > 1 {
            return Err(self.error(raw, ParseCode::InvalidValue));
        }
        let Some(&(op, value_raw)) = ops.first() else {
            return Err(self.error(raw, ParseCode::MissingValue));
        };
        let tz_raw = object.required("tz")?;
        let zone = self.decode::<String>(tz_raw)?;
        let value = if op == "within_days" {
            DateValue::WithinDays {
                days: self.integer(value_raw)?,
                clock: Timestamp::from_millis(self.integer(object.required("clock")?)?),
            }
        } else {
            if let Some(clock) = object.get("clock") {
                return Err(self.error(clock, ParseCode::InvalidValue));
            }
            let value = self.decode::<String>(value_raw)?;
            match op {
                "before" => DateValue::Before(value),
                "after" => DateValue::After(value),
                _ => DateValue::On(value),
            }
        };
        let predicate = DatePredicate::new(value, &zone).map_err(|error| {
            let offending = match error {
                DateError::TimeZone(_) => tz_raw,
                DateError::Clock(_) => object.get("clock").expect("relative clock checked"),
                _ => value_raw,
            };
            self.error(offending, ParseCode::InvalidValue)
        })?;
        Ok(Node::Date(predicate))
    }

    fn fulltext(&mut self, raw: &'a RawValue) -> Result<TextOperand, QueryError> {
        let value = self.decode::<String>(raw)?;
        let span = self.span(raw);
        validate(&value, &mut self.oracle).map_err(|error| super::source_text_error(error, span))
    }
}

fn syntax_error(input: &str, error: &serde_json::Error) -> SearchParseError {
    if error.is_eof() {
        // Locate unmatched delimiters only. Serde still owns all escape,
        // Unicode/surrogate and value validation, including partial scalars.
        let mut stack = Vec::new();
        let mut quote = None;
        let mut escaped = false;
        let mut token = None;
        for (index, byte) in input.bytes().enumerate() {
            if quote.is_some() {
                if escaped {
                    escaped = false;
                } else if byte == b'\\' {
                    escaped = true;
                } else if byte == b'"' {
                    quote = None;
                }
                continue;
            }
            match byte {
                b'"' => {
                    quote = Some(index);
                    token = None;
                }
                b'{' | b'[' => {
                    stack.push((byte, index));
                    token = None;
                }
                b'}' | b']' => {
                    stack.pop();
                    token = None;
                }
                b':' | b',' | b' ' | b'\n' | b'\r' | b'\t' => token = None,
                _ => {
                    token.get_or_insert(index);
                }
            }
        }
        if let Some(start) = quote {
            return SearchParseError::new((start, start + 1), ParseCode::UnclosedQuote);
        }
        if token.is_some_and(|start| serde_json::from_str::<&RawValue>(&input[start..]).is_err()) {
            return SearchParseError::new((input.len(), input.len()), ParseCode::UnexpectedToken);
        }
        let last = input.trim_end().as_bytes().last().copied();
        if last.is_none()
            || last == Some(b':')
            || (last == Some(b',') && stack.last().is_some_and(|&(byte, _)| byte == b'['))
        {
            return SearchParseError::new((input.len(), input.len()), ParseCode::MissingValue);
        }
        if let Some(&(_, start)) = stack.last() {
            return SearchParseError::new((start, start + 1), ParseCode::UnclosedGroup);
        }
    }
    let line_start: usize = input
        .split_inclusive('\n')
        .take(error.line().saturating_sub(1))
        .map(str::len)
        .sum();
    let start =
        input.floor_char_boundary((line_start + error.column().saturating_sub(1)).min(input.len()));
    let end = if start == input.len() {
        start
    } else {
        input.ceil_char_boundary(start + 1)
    };
    SearchParseError::new((start, end), ParseCode::UnexpectedToken)
}
