//! Iterative text grammar (spec/06 M1.6g). Grouping uses an input-bounded
//! heap stack; only normalized, bounded typed trees reach Query::finish.

use std::str::FromStr;

use sb_types::{id::Uuid, time::Timestamp};

use super::text::{TextValidator, validate};
use super::{
    AddressOperand, DatePredicate, DateValue, DomainOperand, Field, FieldPredicate, Flag, Has,
    MAX_QUERY_DEPTH, MAX_QUERY_INPUT_BYTES, MAX_QUERY_NODES, Node, ParseCode as Code, Query,
    QueryError, SearchParseError, SizeOp,
};

type Span = (usize, usize);

/// Explicit date defaults, borrowed only for the duration of a parse.
#[derive(Debug, Clone, Copy)]
pub struct ParseContext<'a> {
    tz: &'a str,
    clock: Timestamp,
}

impl<'a> ParseContext<'a> {
    /// Unused defaults are not consulted; consumed zones use the checked leaf.
    pub const fn new(tz: &'a str, clock: Timestamp) -> Self {
        Self { tz, clock }
    }
}

fn error(span: Span, code: Code) -> QueryError {
    SearchParseError::new(span, code).into()
}

#[derive(Clone)]
enum Kind {
    Word(String),
    Quoted(String),
    Open,
    Close,
    Colon,
    Minus,
}

#[derive(Clone)]
struct Token {
    kind: Kind,
    span: Span,
}

impl Token {
    fn value(&self) -> &str {
        match &self.kind {
            Kind::Word(value) | Kind::Quoted(value) => value,
            _ => unreachable!("value token checked"),
        }
    }
}

fn lex(input: &str) -> Result<Vec<Token>, QueryError> {
    let mut tokens: Vec<Token> = Vec::new();
    let mut at = 0;
    while at < input.len() {
        let ch = input[at..].chars().next().unwrap();
        if ch.is_whitespace() {
            at += ch.len_utf8();
            continue;
        }
        let start = at;
        at += ch.len_utf8();
        let kind = match ch {
            '(' => Kind::Open,
            ')' => Kind::Close,
            ':' => Kind::Colon,
            '-' if !tokens
                .last()
                .is_some_and(|token| matches!(token.kind, Kind::Colon)) =>
            {
                Kind::Minus
            }
            '\\' => return Err(error((start, at), Code::UnexpectedToken)),
            '"' => {
                let mut value = String::new();
                loop {
                    let Some(ch) = input[at..].chars().next() else {
                        return Err(error((start, start + 1), Code::UnclosedQuote));
                    };
                    let escape = at;
                    at += ch.len_utf8();
                    if ch == '"' {
                        break;
                    }
                    if ch != '\\' {
                        value.push(ch);
                        continue;
                    }
                    let Some(next) = input[at..].chars().next() else {
                        return Err(error((start, start + 1), Code::UnclosedQuote));
                    };
                    at += next.len_utf8();
                    if !matches!(next, '"' | '\\') {
                        return Err(error((escape, at), Code::InvalidValue));
                    }
                    value.push(next);
                }
                Kind::Quoted(value)
            }
            _ => {
                while let Some(ch) = input[at..].chars().next() {
                    if ch.is_whitespace() || matches!(ch, '(' | ')' | '"' | '\\' | ':') {
                        break;
                    }
                    at += ch.len_utf8();
                }
                Kind::Word(input[start..at].to_owned())
            }
        };
        tokens.push(Token {
            kind,
            span: (start, at),
        });
    }
    Ok(tokens)
}

struct Built {
    node: Node,
    span: Span,
    count: usize,
    depth: usize,
}

fn bounds(span: Span, count: usize, depth: usize) -> Result<(), QueryError> {
    if count > MAX_QUERY_NODES || depth > MAX_QUERY_DEPTH {
        return Err(error(span, Code::Limit));
    }
    Ok(())
}

#[derive(Clone, Copy)]
enum Join {
    And,
    Or,
}

#[derive(Default)]
struct Group {
    nodes: Vec<Node>,
    count: usize,
    depth: usize,
    span: Option<Span>,
}

impl Group {
    fn push(&mut self, value: Built, join: Join) -> Result<(), QueryError> {
        let Built {
            node,
            span,
            count,
            depth,
        } = value;
        self.span = Some((self.span.map_or(span.0, |old| old.0), span.1));
        match (join, node) {
            (Join::And, Node::And(children)) | (Join::Or, Node::Or(children))
                if !children.is_empty() =>
            {
                self.nodes.extend(children);
                self.count += count - 1;
                self.depth = self.depth.max(depth - 1);
            }
            (_, node) => {
                self.nodes.push(node);
                self.count += count;
                self.depth = self.depth.max(depth);
            }
        }
        let container = usize::from(self.nodes.len() > 1);
        bounds(span, self.count + container, self.depth + container)
    }

    fn finish(mut self, join: Join) -> Built {
        let span = self.span.expect("nonempty group");
        if self.nodes.len() == 1 {
            return Built {
                node: self.nodes.pop().unwrap(),
                span,
                count: self.count,
                depth: self.depth,
            };
        }
        let node = match join {
            Join::And => Node::And(self.nodes),
            Join::Or => Node::Or(self.nodes),
        };
        Built {
            node,
            span,
            count: self.count + 1,
            depth: self.depth + 1,
        }
    }
}

#[derive(PartialEq)]
enum Need {
    Start,
    Value,
    Operator,
}

struct Frame {
    and: Group,
    or: Group,
    negations: Vec<Span>,
    need: Need,
    opener: Option<Span>,
}

impl Frame {
    fn new(opener: Option<Span>) -> Self {
        Self {
            and: Group::default(),
            or: Group::default(),
            negations: Vec::new(),
            need: Need::Start,
            opener,
        }
    }

    fn push(&mut self, mut value: Built) -> Result<(), QueryError> {
        if let Some(first) = self.negations.first() {
            bounds(
                *first,
                value.count + self.negations.len(),
                value.depth + self.negations.len(),
            )?;
            value.span.0 = first.0;
            value.count += self.negations.len();
            value.depth += self.negations.len();
            for _ in self.negations.drain(..) {
                value.node = Node::Not(Box::new(value.node));
            }
        }
        self.and.push(value, Join::And)?;
        self.need = Need::Operator;
        Ok(())
    }
}

enum Selector {
    Text,
    Contains(Field),
    Is(super::AddressField),
    Domain(super::AddressField),
    Flag,
    Has,
    Tag,
    Account,
    Mailbox,
    Date(&'static str),
    Size(SizeOp),
}

fn selector(token: &Token) -> Result<Selector, QueryError> {
    let key = token.value();
    let parts: Vec<_> = key.split('.').collect();
    if !matches!(token.kind, Kind::Word(_))
        || parts.len() > 2
        || parts
            .iter()
            .any(|part| part.is_empty() || !part.bytes().all(|byte| byte.is_ascii_lowercase()))
    {
        return Err(error(token.span, Code::UnexpectedToken));
    }
    let stem = parts[0];
    let suffix = parts.get(1).copied();
    let op_span = suffix.map_or(token.span, |_| {
        (token.span.0 + stem.len() + 1, token.span.1)
    });
    let field = match stem {
        "from" => Some(Field::From),
        "to" => Some(Field::To),
        "cc" => Some(Field::Cc),
        "subject" => Some(Field::Subject),
        "body" => Some(Field::Body),
        "attachment" => Some(Field::Attachment),
        _ => None,
    };
    if let Some(field) = field {
        return match suffix {
            None => Ok(Selector::Contains(field)),
            Some("is") => field
                .address()
                .map(Selector::Is)
                .ok_or_else(|| error(op_span, Code::InvalidOperator)),
            Some("domain") => field
                .address()
                .map(Selector::Domain)
                .ok_or_else(|| error(op_span, Code::InvalidOperator)),
            _ => Err(error(op_span, Code::InvalidOperator)),
        };
    }
    if stem == "size" {
        return match suffix {
            Some("lt") => Ok(Selector::Size(SizeOp::Lt)),
            Some("eq") => Ok(Selector::Size(SizeOp::Eq)),
            Some("gt") => Ok(Selector::Size(SizeOp::Gt)),
            _ => Err(error(op_span, Code::InvalidOperator)),
        };
    }
    if suffix.is_some() {
        return Err(error(token.span, Code::UnknownKey));
    }
    match stem {
        "is" => Ok(Selector::Flag),
        "has" => Ok(Selector::Has),
        "tag" => Ok(Selector::Tag),
        "account" => Ok(Selector::Account),
        "in" => Ok(Selector::Mailbox),
        "before" => Ok(Selector::Date("before")),
        "after" => Ok(Selector::Date("after")),
        "on" => Ok(Selector::Date("on")),
        "within" => Ok(Selector::Date("within")),
        _ => Err(error(token.span, Code::UnknownKey)),
    }
}

fn integer<T: FromStr>(token: &Token) -> Result<T, QueryError> {
    if token.value().starts_with('+') {
        return Err(error(token.span, Code::InvalidValue));
    }
    token
        .value()
        .parse()
        .map_err(|_| error(token.span, Code::InvalidValue))
}

struct Parser<'a, 'c> {
    tokens: Vec<Token>,
    at: usize,
    end: Span,
    used: usize,
    oracle: Option<TextValidator>,
    context: &'c ParseContext<'a>,
}

pub(super) fn parse(
    input: &str,
    context: &ParseContext<'_>,
    oracle: Option<TextValidator>,
) -> Result<Query, QueryError> {
    if input.len() > MAX_QUERY_INPUT_BYTES {
        let start = input.floor_char_boundary(MAX_QUERY_INPUT_BYTES);
        return Err(error(
            (start, input.ceil_char_boundary(start + 1)),
            Code::Limit,
        ));
    }
    let mut parser = Parser {
        tokens: lex(input)?,
        at: 0,
        end: (input.len(), input.len()),
        used: 0,
        oracle,
        context,
    };
    let root = parser.run()?;
    Query::finish(root.node, root.span)
}

impl Parser<'_, '_> {
    fn bump(&mut self, count: usize, span: Span) -> Result<(), QueryError> {
        // Only irreducible leaves/unaries count here. Same-op containers may
        // disappear at a parent; their exact normalized stats are checked there.
        self.used += count;
        if self.used > MAX_QUERY_NODES {
            return Err(error(span, Code::Limit));
        }
        Ok(())
    }

    fn finish(&mut self, frame: Frame, end: Span) -> Result<Built, QueryError> {
        if frame.need == Need::Value {
            return Err(error(end, Code::MissingValue));
        }
        let span = frame.opener.map_or((0, self.end.1), |open| (open.0, end.1));
        let mut root = if frame.and.nodes.is_empty() {
            self.bump(1, span)?;
            Built {
                node: Node::And(Vec::new()),
                span,
                count: 1,
                depth: 1,
            }
        } else {
            let value = frame.and.finish(Join::And);
            if frame.or.nodes.is_empty() {
                value
            } else {
                let mut group = frame.or;
                group.push(value, Join::Or)?;
                group.finish(Join::Or)
            }
        };
        root.span = span;
        Ok(root)
    }

    fn run(&mut self) -> Result<Built, QueryError> {
        let mut frames = vec![Frame::new(None)];
        while let Some(token) = self.tokens.get(self.at).cloned() {
            self.at += 1;
            let frame = frames.last_mut().expect("root frame retained");
            let keyed = self
                .tokens
                .get(self.at)
                .is_some_and(|next| matches!(next.kind, Kind::Colon));
            match &token.kind {
                Kind::Open => frames.push(Frame::new(Some(token.span))),
                Kind::Close => {
                    if frames.len() == 1 {
                        return Err(error(token.span, Code::UnexpectedToken));
                    }
                    let value = self.finish(frames.pop().unwrap(), token.span)?;
                    frames.last_mut().unwrap().push(value)?;
                }
                Kind::Minus => {
                    self.bump(1, token.span)?;
                    frame.negations.push(token.span);
                    frame.need = Need::Value;
                }
                Kind::Word(word) if !keyed && matches!(word.as_str(), "AND" | "OR" | "NOT") => {
                    if word == "NOT" {
                        self.bump(1, token.span)?;
                        frame.negations.push(token.span);
                        frame.need = Need::Value;
                        continue;
                    }
                    if frame.need != Need::Operator {
                        let code = if frame.need == Need::Value {
                            Code::MissingValue
                        } else {
                            Code::UnexpectedToken
                        };
                        return Err(error(token.span, code));
                    }
                    if word == "OR" {
                        let clause = std::mem::take(&mut frame.and).finish(Join::And);
                        frame.or.push(clause, Join::Or)?;
                    }
                    frame.need = Need::Value;
                }
                Kind::Word(_) | Kind::Quoted(_) => frame.push(self.atom(token)?)?,
                Kind::Colon => return Err(error(token.span, Code::UnexpectedToken)),
            }
        }
        let frame = frames.last().unwrap();
        if frame.need == Need::Value {
            return Err(error(self.end, Code::MissingValue));
        }
        if let Some(open) = frame.opener {
            return Err(error(open, Code::UnclosedGroup));
        }
        self.finish(frames.pop().unwrap(), self.end)
    }

    fn atom(&mut self, first: Token) -> Result<Built, QueryError> {
        let mut span = first.span;
        let keyed = self
            .tokens
            .get(self.at)
            .is_some_and(|next| matches!(next.kind, Kind::Colon));
        let (selected, value) = if keyed {
            let colon = self.tokens[self.at].span;
            self.at += 1;
            if colon.0 != first.span.1 {
                return Err(error(colon, Code::UnexpectedToken));
            }
            let selected = selector(&first)?;
            let value = self
                .tokens
                .get(self.at)
                .cloned()
                .ok_or_else(|| error(self.end, Code::MissingValue))?;
            if value.span.0 != colon.1 {
                return Err(error(value.span, Code::MissingValue));
            }
            match &value.kind {
                Kind::Colon => return Err(error(value.span, Code::UnexpectedToken)),
                Kind::Word(word) if !matches!(word.as_str(), "AND" | "OR" | "NOT") => {}
                Kind::Quoted(_) => {}
                _ => return Err(error(value.span, Code::MissingValue)),
            }
            self.at += 1;
            span.1 = value.span.1;
            (selected, value)
        } else {
            (Selector::Text, first)
        };
        self.bump(1, span)?;
        let invalid = || error(value.span, Code::InvalidValue);
        let text = value.value();
        let mut count = 1;
        let node = match selected {
            Selector::Text => Node::Text(
                validate(text, &mut self.oracle)
                    .map_err(|e| super::source_text_error(e, value.span))?,
            ),
            Selector::Contains(field) => Node::Field(FieldPredicate::Contains {
                field,
                value: validate(text, &mut self.oracle)
                    .map_err(|e| super::source_text_error(e, value.span))?,
            }),
            Selector::Is(field) => Node::Field(FieldPredicate::Is {
                field,
                address: AddressOperand::parse(text).map_err(|_| invalid())?,
            }),
            Selector::Domain(field) => Node::Field(FieldPredicate::Domain {
                field,
                domain: DomainOperand::parse(text).map_err(|_| invalid())?,
            }),
            Selector::Flag => match text {
                "unread" => {
                    self.bump(1, span)?;
                    count = 2;
                    Node::Not(Box::new(Node::Flag(Flag::Seen)))
                }
                "read" => Node::Flag(Flag::Seen),
                "flagged" => Node::Flag(Flag::Flagged),
                "answered" => Node::Flag(Flag::Answered),
                "draft" => Node::Flag(Flag::Draft),
                _ => return Err(invalid()),
            },
            Selector::Has => Node::Has(match text {
                "attachment" => Has::Attachment,
                "note" => Has::Note,
                _ => return Err(invalid()),
            }),
            Selector::Tag => Node::Tag(text.parse::<Uuid>().map_err(|_| invalid())?),
            Selector::Account => Node::Account(text.parse::<Uuid>().map_err(|_| invalid())?),
            Selector::Mailbox => {
                let id = integer(&value)?;
                if id <= 0 {
                    return Err(invalid());
                }
                Node::Mailbox(id)
            }
            Selector::Size(op) => {
                let bytes = integer(&value)?;
                if bytes < 0 {
                    return Err(invalid());
                }
                Node::Size { op, bytes }
            }
            Selector::Date(op) => self.date(op, &value)?,
        };
        Ok(Built {
            node,
            span,
            count,
            depth: count,
        })
    }

    fn date(&self, op: &str, token: &Token) -> Result<Node, QueryError> {
        let invalid = || error(token.span, Code::InvalidValue);
        let parts = token.value().split('@').take(4).collect::<Vec<_>>();
        let (value, zone) = if op == "within" {
            let days = parts[0].parse::<u32>().map_err(|_| invalid())?;
            if parts[0].starts_with('+') {
                return Err(invalid());
            }
            match parts.as_slice() {
                [_] => (
                    DateValue::WithinDays {
                        days,
                        clock: self.context.clock,
                    },
                    self.context.tz,
                ),
                [_, zone, raw] if !raw.starts_with('+') => (
                    DateValue::WithinDays {
                        days,
                        clock: Timestamp::from_millis(raw.parse().map_err(|_| invalid())?),
                    },
                    *zone,
                ),
                _ => return Err(invalid()),
            }
        } else {
            let zone = match parts.as_slice() {
                [_] => self.context.tz,
                [_, zone] => *zone,
                _ => return Err(invalid()),
            };
            let date = parts[0].to_owned();
            let value = match op {
                "before" => DateValue::Before(date),
                "after" => DateValue::After(date),
                _ => DateValue::On(date),
            };
            (value, zone)
        };
        DatePredicate::new(value, zone)
            .map(Node::Date)
            .map_err(|_| invalid())
    }
}
