//! The property test for [`super`] (M4.4e1): lossless HTML renders
//! through the composer's plain-text view back to its own text.

use proptest::prelude::*;
use proptest::sample::select;
use proptest::strategy::BoxedStrategy;

use super::*;

// Emphasis content avoids `*`, `_` and `~`, and sequences never place two
// code spans or two emphasis spans side by side: CommonMark reads adjacent
// backtick or delimiter runs as one run. Those constructs are covered by
// unit tests instead.

const TEXT_CHARS: &[char] = &[
    'a', 'b', 'Z', '0', '1', ' ', '*', '_', '[', ']', '~', '`', '\\', '&', '!', '<', ';',
];

/// Characters that sit inside emphasis without needing an escape or
/// forming a line marker.
const PLAIN_CHARS: &[char] = &['a', 'b', 'Z', '0', '1', ' '];

#[derive(Debug, Clone)]
enum Simple {
    Text(String),
    Br,
}

#[derive(Debug, Clone)]
enum Inline {
    Text(String),
    Code(String),
    Br,
    Strong(Vec<Simple>),
    Em(Vec<Simple>),
    Del(Vec<Simple>),
}

#[derive(Debug, Clone)]
enum Node {
    Para(Vec<Inline>),
    Heading(u8, Vec<Inline>),
    Quote(Vec<Node>),
    List(Vec<Vec<Inline>>),
}

fn text_strategy() -> impl Strategy<Value = String> {
    prop::collection::vec(select(TEXT_CHARS.to_vec()), 1..16)
        .prop_map(|chars| chars.into_iter().collect())
}

fn plain_text_strategy() -> impl Strategy<Value = String> {
    prop::collection::vec(select(PLAIN_CHARS.to_vec()), 1..16)
        .prop_map(|chars| chars.into_iter().collect())
}

/// Code content keeps its interior; a leading and trailing space would be
/// stripped again by CommonMark's code-span rule, so it never generates.
fn code_text_strategy() -> impl Strategy<Value = String> {
    text_strategy()
        .prop_map(|text| text.trim_matches(' ').to_string())
        .prop_filter("nonempty code", |text| !text.is_empty())
}

fn simple_strategy() -> BoxedStrategy<Simple> {
    prop_oneof![
        plain_text_strategy().prop_map(Simple::Text),
        Just(Simple::Br)
    ]
    .boxed()
}

fn inline_strategy() -> BoxedStrategy<Inline> {
    prop_oneof![
        text_strategy().prop_map(Inline::Text),
        code_text_strategy().prop_map(Inline::Code),
        Just(Inline::Br),
        prop::collection::vec(simple_strategy(), 0..3).prop_map(Inline::Strong),
        prop::collection::vec(simple_strategy(), 0..3).prop_map(Inline::Em),
        prop::collection::vec(simple_strategy(), 0..3).prop_map(Inline::Del),
    ]
    .boxed()
}

fn inline_seq_strategy() -> BoxedStrategy<Vec<Inline>> {
    prop::collection::vec(inline_strategy(), 0..3)
        .prop_filter("separated code and emphasis", |items| {
            // Code spans and emphasis spans must not touch each other:
            // adjacent backtick or delimiter runs are read as one run.
            fn category(item: &Inline) -> u8 {
                match item {
                    Inline::Code(_) => 1,
                    Inline::Strong(_) | Inline::Em(_) | Inline::Del(_) => 2,
                    Inline::Text(_) | Inline::Br => 0,
                }
            }
            items
                .windows(2)
                .all(|pair| category(&pair[0]) == 0 || category(&pair[1]) == 0)
        })
        .boxed()
}

fn node_strategy(depth: u32) -> BoxedStrategy<Node> {
    let inlines = inline_seq_strategy();
    let heading = (1u8..=3).prop_flat_map({
        let inlines = inlines.clone();
        move |level| {
            inlines
                .clone()
                .prop_map(move |items| Node::Heading(level, items))
        }
    });
    if depth == 0 {
        return prop_oneof![inlines.prop_map(Node::Para), heading].boxed();
    }
    prop_oneof![
        inlines.clone().prop_map(Node::Para),
        heading,
        prop::collection::vec(node_strategy(depth - 1), 0..2).prop_map(Node::Quote),
        prop::collection::vec(prop::collection::vec(inline_strategy(), 1..2), 0..3)
            .prop_map(Node::List),
    ]
    .boxed()
}

fn escape_html(text: &str, out: &mut String) {
    for c in text.chars() {
        match c {
            '&' => out.push_str("&amp;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(c),
        }
    }
}

fn serialize(doc: &[Node]) -> String {
    let mut out = String::new();
    serialize_blocks(doc, &mut out);
    out
}

fn serialize_blocks(blocks: &[Node], out: &mut String) {
    for (index, node) in blocks.iter().enumerate() {
        if index > 0 {
            out.push('\n');
        }
        serialize_node(node, out);
    }
}

fn serialize_node(node: &Node, out: &mut String) {
    match node {
        Node::Para(inlines) => {
            out.push_str("<p>");
            serialize_inlines(inlines, out);
            out.push_str("</p>");
        }
        Node::Heading(level, inlines) => {
            out.push_str("<h");
            out.push((b'0' + level) as char);
            out.push('>');
            serialize_inlines(inlines, out);
            out.push_str("</h");
            out.push((b'0' + level) as char);
            out.push('>');
        }
        Node::Quote(blocks) => {
            out.push_str("<blockquote>");
            serialize_blocks(blocks, out);
            out.push_str("</blockquote>");
        }
        Node::List(items) => {
            out.push_str("<ul>");
            for (index, item) in items.iter().enumerate() {
                if index > 0 {
                    out.push('\n');
                }
                out.push_str("<li>");
                serialize_inlines(item, out);
                out.push_str("</li>");
            }
            out.push_str("</ul>");
        }
    }
}

fn serialize_inlines(inlines: &[Inline], out: &mut String) {
    for inline in inlines {
        match inline {
            Inline::Text(text) => escape_html(text, out),
            Inline::Code(text) => {
                out.push_str("<code>");
                escape_html(text, out);
                out.push_str("</code>");
            }
            Inline::Br => out.push_str("<br>"),
            Inline::Strong(items) => {
                out.push_str("<strong>");
                serialize_simple(items, out);
                out.push_str("</strong>");
            }
            Inline::Em(items) => {
                out.push_str("<em>");
                serialize_simple(items, out);
                out.push_str("</em>");
            }
            Inline::Del(items) => {
                out.push_str("<del>");
                serialize_simple(items, out);
                out.push_str("</del>");
            }
        }
    }
}

fn serialize_simple(items: &[Simple], out: &mut String) {
    for item in items {
        match item {
            Simple::Text(text) => escape_html(text, out),
            Simple::Br => out.push_str("<br>"),
        }
    }
}

fn ast_text(doc: &[Node]) -> String {
    let mut out = String::new();
    ast_blocks(doc, &mut out);
    out
}

fn ast_blocks(blocks: &[Node], out: &mut String) {
    for (index, node) in blocks.iter().enumerate() {
        if index > 0 && !out.is_empty() {
            out.push('\n');
        }
        ast_node(node, out);
    }
}

fn ast_node(node: &Node, out: &mut String) {
    match node {
        Node::Para(inlines) | Node::Heading(_, inlines) => ast_inlines(inlines, out),
        Node::Quote(blocks) => ast_blocks(blocks, out),
        Node::List(items) => {
            for (index, item) in items.iter().enumerate() {
                if index > 0 && !out.is_empty() {
                    out.push('\n');
                }
                ast_inlines(item, out);
            }
        }
    }
}

fn ast_inlines(inlines: &[Inline], out: &mut String) {
    for inline in inlines {
        match inline {
            Inline::Text(text) | Inline::Code(text) => out.push_str(text),
            Inline::Br => out.push(' '),
            Inline::Strong(items) | Inline::Em(items) | Inline::Del(items) => {
                ast_simple(items, out);
            }
        }
    }
}

fn ast_simple(items: &[Simple], out: &mut String) {
    for item in items {
        match item {
            Simple::Text(text) => out.push_str(text),
            Simple::Br => out.push(' '),
        }
    }
}

/// Marker stripping applied identically to both sides: quote and list
/// prefixes, then ATX heading prefixes, then whitespace collapse.
fn normalise(text: &str) -> String {
    let mut lines = Vec::new();
    for line in text.lines() {
        let mut rest = line.trim_start();
        loop {
            let next = rest
                .strip_prefix("> ")
                .or_else(|| rest.strip_prefix("- "))
                .or_else(|| strip_ordered_marker(rest))
                .or_else(|| strip_heading_marker(rest));
            match next {
                Some(stripped) => rest = stripped.trim_start(),
                None => break,
            }
        }
        lines.push(rest.to_string());
    }
    lines
        .join(" ")
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

fn strip_ordered_marker(line: &str) -> Option<&str> {
    let digits = line.chars().take_while(char::is_ascii_digit).count();
    if digits == 0 {
        return None;
    }
    line[digits..]
        .strip_prefix(". ")
        .or_else(|| line[digits..].strip_prefix(") "))
}

fn strip_heading_marker(line: &str) -> Option<&str> {
    let hashes = line.chars().take_while(|c| *c == '#').count();
    if hashes == 0 {
        return None;
    }
    line[hashes..].strip_prefix(' ')
}

proptest! {
    #![proptest_config(ProptestConfig::with_cases(64))]

    #[test]
    fn lossless_html_renders_back_to_its_text(
        doc in prop::collection::vec(node_strategy(3), 0..4),
    ) {
        let html = serialize(&doc);
        let converted = markdown_from_html(&html).expect("grammar HTML converts");
        prop_assert!(
            converted.losses.is_empty(),
            "unexpected losses {:?} for {:?}",
            converted.losses,
            html,
        );
        let rendered = crate::compose::render_markdown(&converted.markdown)
            .expect("the Markdown is valid");
        let actual = normalise(&rendered.plain);
        let expected = normalise(&ast_text(&doc));
        prop_assert_eq!(actual, expected, "html: {:?}", html);
    }
}
