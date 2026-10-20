//! Text rendering of the already-checked tree (spec/06 M1.6g).

use std::{error::Error, fmt};

use super::{DateValue, FieldPredicate, Flag, Has, MAX_QUERY_INPUT_BYTES, Node, SizeOp};

/// Formatting a typed tree has no source string or invented error position.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TextFormatError {
    Limit,
}

impl fmt::Display for TextFormatError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(
            f,
            "canonical text exceeds {MAX_QUERY_INPUT_BYTES} UTF-8 bytes"
        )
    }
}

impl Error for TextFormatError {}

pub(super) fn render(root: &Node) -> Result<String, TextFormatError> {
    let mut out = String::new();
    node(root, &mut out);
    if out.len() > MAX_QUERY_INPUT_BYTES {
        return Err(TextFormatError::Limit);
    }
    Ok(out)
}

fn quoted(value: &str, out: &mut String) {
    out.push('"');
    for ch in value.chars() {
        if matches!(ch, '"' | '\\') {
            out.push('\\');
        }
        out.push(ch);
    }
    out.push('"');
}

fn field(key: &str, value: &str, out: &mut String) {
    out.push_str(key);
    out.push(':');
    quoted(value, out);
}

fn group(children: &[Node], separator: &str, out: &mut String) {
    out.push('(');
    for (index, child) in children.iter().enumerate() {
        if index != 0 {
            out.push_str(separator);
        }
        node(child, out);
    }
    out.push(')');
}

fn node(value: &Node, out: &mut String) {
    match value {
        Node::Text(value) => quoted(value.value(), out),
        Node::Field(FieldPredicate::Contains { field: name, value }) => {
            field(name.as_str(), value.value(), out)
        }
        Node::Field(FieldPredicate::Is {
            field: name,
            address,
        }) => field(&format!("{}.is", name.as_str()), address.value(), out),
        Node::Field(FieldPredicate::Domain {
            field: name,
            domain,
        }) => field(&format!("{}.domain", name.as_str()), domain.value(), out),
        Node::Flag(flag) => field(
            "is",
            match flag {
                Flag::Seen => "read",
                Flag::Flagged => "flagged",
                Flag::Answered => "answered",
                Flag::Draft => "draft",
            },
            out,
        ),
        Node::Tag(id) => field("tag", &id.to_string(), out),
        Node::Has(kind) => field(
            "has",
            match kind {
                Has::Attachment => "attachment",
                Has::Note => "note",
            },
            out,
        ),
        Node::Date(date) => {
            let (key, value) = match date.value() {
                DateValue::Before(value) => ("before", format!("{value}@{}", date.zone())),
                DateValue::After(value) => ("after", format!("{value}@{}", date.zone())),
                DateValue::On(value) => ("on", format!("{value}@{}", date.zone())),
                DateValue::WithinDays { days, clock } => (
                    "within",
                    format!("{days}@{}@{}", date.zone(), clock.as_millis()),
                ),
            };
            field(key, &value, out);
        }
        Node::Size { op, bytes } => field(
            match op {
                SizeOp::Lt => "size.lt",
                SizeOp::Eq => "size.eq",
                SizeOp::Gt => "size.gt",
            },
            &bytes.to_string(),
            out,
        ),
        Node::Account(id) => field("account", &id.to_string(), out),
        Node::Mailbox(id) => field("in", &id.to_string(), out),
        Node::And(children) => group(children, " AND ", out),
        Node::Or(children) => group(children, " OR ", out),
        Node::Not(child) => {
            out.push_str("NOT ");
            node(child, out);
        }
    }
}
