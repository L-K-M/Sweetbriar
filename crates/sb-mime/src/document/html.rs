//! Tokenizer-level HTML for the semantic document: a positive structure
//! check over the sender's markup, and a bounded tree over the
//! sanitizer's well-formed serialization.
//!
//! The sanitizer has already tree-built and serialized the body, so a
//! stack is enough here; html5ever still decodes character references
//! and attribute values. Nesting and node counts are bounded so a
//! hostile body cannot exhaust memory or recurse without limit.

use std::cell::{Cell, RefCell};

use html5ever::TokenizerResult;
use html5ever::buffer_queue::BufferQueue;
use html5ever::tendril::StrTendril;
use html5ever::tokenizer::{Tag, TagKind, Token, TokenSink, TokenSinkResult, Tokenizer};

/// Nesting deeper than this is not a personal-mail structure the model
/// renders faithfully, so it classifies as unsupported.
const MAX_DEPTH: usize = 128;
/// Node budget for one document body.
const MAX_NODES: usize = 100_000;
const CHUNK: usize = 1_024;
const VOID: &[&str] = &["br", "img"];

/// A tokenizer-level element tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) enum RawNode {
    Element {
        tag: String,
        attrs: Vec<(String, String)>,
        children: Vec<RawNode>,
    },
    Text(String),
}

/// The tags the semantic model represents. Everything else is sender
/// layout: active content, tables and layout containers, forms,
/// positioned or unknown elements.
pub(crate) fn supported_tag(tag: &str) -> bool {
    matches!(
        tag,
        "p" | "h1"
            | "h2"
            | "h3"
            | "h4"
            | "h5"
            | "h6"
            | "blockquote"
            | "ul"
            | "ol"
            | "li"
            | "a"
            | "img"
            | "em"
            | "strong"
            | "i"
            | "b"
            | "br"
            | "pre"
            | "div"
            | "html"
            | "body"
            | "span"
    )
}

/// Attributes the model represents. `div`, `span`, `html` and `body`
/// are transparent containers; `style`, presentational attributes,
/// `class` and `id` are not represented, so they classify as
/// unsupported.
fn supported_attr(tag: &str, attr: &str) -> bool {
    match tag {
        "a" => attr == "href",
        "img" => attr == "src" || attr == "alt",
        "ol" => attr == "start" || attr == "reversed",
        "li" => attr == "value",
        _ => false,
    }
}

struct Frame {
    tag: String,
    attrs: Vec<(String, String)>,
    children: Vec<RawNode>,
}

struct Parser {
    roots: RefCell<Vec<RawNode>>,
    stack: RefCell<Vec<Frame>>,
    supported: Cell<bool>,
    build: bool,
    nodes: Cell<usize>,
}

impl Parser {
    fn new(build: bool) -> Self {
        Self {
            roots: RefCell::new(Vec::new()),
            stack: RefCell::new(Vec::new()),
            supported: Cell::new(true),
            build,
            nodes: Cell::new(0),
        }
    }

    fn append(&self, node: RawNode) {
        let mut stack = self.stack.borrow_mut();
        match stack.last_mut() {
            Some(frame) => frame.children.push(node),
            None => self.roots.borrow_mut().push(node),
        }
    }

    fn check(&self, tag: &Tag) {
        if !supported_tag(tag.name.as_ref()) {
            self.supported.set(false);
        }
        for attr in &tag.attrs {
            if !supported_attr(tag.name.as_ref(), attr.name.local.as_ref()) {
                self.supported.set(false);
            }
        }
    }
}

impl TokenSink for Parser {
    type Handle = ();

    fn process_token(&self, token: Token, _: u64) -> TokenSinkResult<()> {
        match token {
            Token::TagToken(tag) => {
                if !self.build {
                    self.check(&tag);
                    return TokenSinkResult::Continue;
                }
                let name: String = (*tag.name).to_ascii_lowercase();
                match tag.kind {
                    TagKind::StartTag => {
                        if VOID.contains(&name.as_str()) {
                            self.append(RawNode::Element {
                                tag: name,
                                attrs: attrs(&tag),
                                children: Vec::new(),
                            });
                            return TokenSinkResult::Continue;
                        }
                        self.nodes.set(self.nodes.get() + 1);
                        let depth = self.stack.borrow().len();
                        if depth >= MAX_DEPTH || self.nodes.get() > MAX_NODES {
                            self.supported.set(false);
                            return TokenSinkResult::Continue;
                        }
                        self.stack.borrow_mut().push(Frame {
                            tag: name,
                            attrs: attrs(&tag),
                            children: Vec::new(),
                        });
                    }
                    TagKind::EndTag => {
                        let closed = if VOID.contains(&name.as_str()) {
                            None
                        } else {
                            self.stack.borrow_mut().pop()
                        };
                        if let Some(frame) = closed {
                            self.append(RawNode::Element {
                                tag: frame.tag,
                                attrs: frame.attrs,
                                children: frame.children,
                            });
                        }
                    }
                }
            }
            Token::CharacterTokens(text) if self.build && !text.is_empty() => {
                self.append(RawNode::Text(text.to_string()));
            }
            _ => {}
        }
        TokenSinkResult::Continue
    }
}

fn attrs(tag: &Tag) -> Vec<(String, String)> {
    tag.attrs
        .iter()
        .map(|attr| (attr.name.local.to_string(), attr.value.to_string()))
        .collect()
}

fn feed(html: &str, tokenizer: &Tokenizer<Parser>) {
    let input = BufferQueue::default();
    let mut offset = 0;
    while offset < html.len() {
        let end = html.floor_char_boundary((offset + CHUNK).min(html.len()));
        input.push_back(StrTendril::from_slice(&html[offset..end]));
        while !matches!(tokenizer.feed(&input), TokenizerResult::Done) {}
        offset = end;
    }
    tokenizer.end();
}

/// Positive structure check over the sender's markup. Only
/// [`supported_tag`] and [`supported_attr`] pass; scripts, forms,
/// tables, styles and unknown constructs fail.
pub(crate) fn detect(html: &str) -> bool {
    let tokenizer = Tokenizer::new(Parser::new(false), Default::default());
    feed(html, &tokenizer);
    tokenizer.sink.supported.get()
}

/// Build the tree from sanitized, well-formed HTML. `None` when the
/// body exceeds the nesting or node budget.
pub(crate) fn tree(html: &str) -> Option<Vec<RawNode>> {
    let tokenizer = Tokenizer::new(Parser::new(true), Default::default());
    feed(html, &tokenizer);
    tokenizer
        .sink
        .supported
        .get()
        .then(|| tokenizer.sink.roots.into_inner())
}
