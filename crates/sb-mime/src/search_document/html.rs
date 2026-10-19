//! Minimal HTML5 tree sink. An arena holds every node, so neither traversal nor
//! destruction recurses through hostile nesting. Attributes cannot contribute
//! search text; only the parser's foreign-content integration flag is retained.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use html5ever::buffer_queue::BufferQueue;
use html5ever::tendril::StrTendril;
use html5ever::tokenizer::{TagKind, Token, TokenSink, TokenSinkResult, Tokenizer};
use html5ever::tree_builder::{ElementFlags, NodeOrText, QuirksMode, TreeBuilder, TreeSink};
use html5ever::{Attribute, ExpandedName, QualName, TokenizerResult, local_name, ns};

use super::{Kind, Line, append_html_text, boundary, is_block};

type Handle = Rc<Node>;

// Feed the actual HTML5 parser in bounded turns; no separate HTML preflight
// can reliably predict its repaired tree or raw-text context (spec/06).
const INPUT_CHUNK_BYTES: usize = 1_024;
const MAX_NODES: usize = 10_000;
const MAX_WORK: usize = 4_000_000;
const MAX_PENDING_BYTES: usize = 16_384;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HtmlLimit {
    Nodes,
    Work,
    PendingToken,
}

struct Node {
    id: usize,
    name: QualName,
    text: Option<RefCell<StrTendril>>,
    parent: Cell<Option<usize>>,
    children: RefCell<Vec<usize>>,
    template: Cell<Option<usize>>,
    integration: bool,
}

#[derive(Default)]
struct Tree {
    nodes: RefCell<Vec<Handle>>,
    work: Cell<usize>,
    mutations: Cell<usize>,
}

impl Tree {
    fn charge(&self, amount: usize) {
        self.work.set(self.work.get().saturating_add(amount));
    }

    fn mutated(&self) {
        self.mutations.set(self.mutations.get().saturating_add(1));
    }

    fn check_budget(&self) -> Result<(), HtmlLimit> {
        if self.nodes.borrow().len() > MAX_NODES {
            return Err(HtmlLimit::Nodes);
        }
        if self.work.get() > MAX_WORK {
            return Err(HtmlLimit::Work);
        }
        Ok(())
    }

    fn node(&self, id: usize) -> Handle {
        self.charge(1);
        self.nodes.borrow()[id].clone()
    }

    fn create(&self, name: QualName, text: Option<StrTendril>, integration: bool) -> Handle {
        self.charge(1);
        self.mutated();
        let mut nodes = self.nodes.borrow_mut();
        let node = Rc::new(Node {
            id: nodes.len(),
            name,
            text: text.map(RefCell::new),
            parent: Cell::new(None),
            children: RefCell::new(Vec::new()),
            template: Cell::new(None),
            integration,
        });
        nodes.push(node.clone());
        node
    }

    fn anonymous(&self, text: Option<StrTendril>) -> Handle {
        self.create(QualName::new(None, ns!(), local_name!("")), text, false)
    }

    fn detach(&self, node: &Handle) {
        if let Some(parent) = node.parent.take() {
            let parent = self.node(parent);
            self.charge(parent.children.borrow().len());
            self.mutated();
            parent.children.borrow_mut().retain(|id| *id != node.id);
        }
    }

    fn insert(&self, parent: &Handle, index: usize, child: NodeOrText<Handle>) {
        self.mutated();
        let child = match child {
            NodeOrText::AppendNode(node) => {
                self.detach(&node);
                node
            }
            NodeOrText::AppendText(text) => {
                self.charge(text.len());
                if index > 0 {
                    let previous = self.node(parent.children.borrow()[index - 1]);
                    if let Some(contents) = &previous.text {
                        contents.borrow_mut().push_tendril(&text);
                        return;
                    }
                }
                self.anonymous(Some(text))
            }
        };
        child.parent.set(Some(parent.id));
        self.charge(parent.children.borrow().len().saturating_sub(index));
        parent.children.borrow_mut().insert(index, child.id);
    }
}

impl TreeSink for Tree {
    type Handle = Handle;
    type Output = Self;
    type ElemName<'a> = ExpandedName<'a>;

    fn finish(self) -> Self {
        self
    }
    fn parse_error(&self, _: Cow<'static, str>) {}
    fn get_document(&self) -> Handle {
        self.node(0)
    }
    fn elem_name<'a>(&'a self, target: &'a Handle) -> ExpandedName<'a> {
        self.charge(1);
        target.name.expanded()
    }
    fn create_element(&self, name: QualName, attrs: Vec<Attribute>, flags: ElementFlags) -> Handle {
        // The tokenizer checks each attribute against earlier attributes.
        self.charge(attrs.len().saturating_mul(attrs.len()));
        let node = self.create(name, None, flags.mathml_annotation_xml_integration_point);
        if flags.template {
            node.template.set(Some(self.anonymous(None).id));
        }
        node
    }
    fn create_comment(&self, _: StrTendril) -> Handle {
        self.anonymous(None)
    }
    fn create_pi(&self, _: StrTendril, _: StrTendril) -> Handle {
        self.anonymous(None)
    }
    fn append(&self, parent: &Handle, child: NodeOrText<Handle>) {
        let index = parent.children.borrow().len();
        self.insert(parent, index, child);
    }
    fn append_based_on_parent_node(
        &self,
        element: &Handle,
        previous: &Handle,
        child: NodeOrText<Handle>,
    ) {
        if element.parent.get().is_some() {
            self.append_before_sibling(element, child);
        } else {
            self.append(previous, child);
        }
    }
    fn append_doctype_to_document(&self, _: StrTendril, _: StrTendril, _: StrTendril) {}
    fn get_template_contents(&self, target: &Handle) -> Handle {
        self.node(
            target
                .template
                .get()
                .expect("parser requested template contents"),
        )
    }
    fn same_node(&self, a: &Handle, b: &Handle) -> bool {
        self.charge(1);
        a.id == b.id
    }
    fn set_quirks_mode(&self, _: QuirksMode) {}
    fn append_before_sibling(&self, sibling: &Handle, child: NodeOrText<Handle>) {
        let parent = self.node(
            sibling
                .parent
                .get()
                .expect("parser supplied parentless sibling"),
        );
        // Detach first: moving an existing preceding sibling changes the index.
        if let NodeOrText::AppendNode(node) = &child {
            self.detach(node);
        }
        self.charge(parent.children.borrow().len());
        let index = parent
            .children
            .borrow()
            .iter()
            .position(|id| *id == sibling.id)
            .expect("sibling belongs to parent");
        self.insert(&parent, index, child);
    }
    fn add_attrs_if_missing(&self, _: &Handle, attrs: Vec<Attribute>) {
        self.charge(attrs.len().saturating_mul(attrs.len()));
    }
    fn remove_from_parent(&self, target: &Handle) {
        self.detach(target);
    }
    fn reparent_children(&self, node: &Handle, new_parent: &Handle) {
        let children = node.children.take();
        self.charge(children.len());
        self.mutated();
        for id in &children {
            self.node(*id).parent.set(Some(new_parent.id));
        }
        new_parent.children.borrow_mut().extend(children);
    }
    fn is_mathml_annotation_xml_integration_point(&self, target: &Handle) -> bool {
        target.integration
    }
}

// These costs happen inside html5ever, before/without TreeSink callbacks.
// Forward the real tokens and namespace decisions, never re-tokenize HTML.
struct BudgetedBuilder {
    inner: TreeBuilder<Handle, Tree>,
    input_bytes: Cell<usize>,
    token_end_bound: Cell<usize>,
    formatting_count: Cell<usize>,
    formatting_cost: Cell<usize>,
}

impl TokenSink for BudgetedBuilder {
    type Handle = Handle;

    fn process_token(&self, token: Token, line: u64) -> TokenSinkResult<Handle> {
        let tree = &self.inner.sink;
        tree.charge(1);
        if let Token::TagToken(tag) = &token {
            // A token may start in the preceding feed chunk. Each attempted
            // attribute consumes input, including duplicates the tokenizer drops.
            let span = self
                .input_bytes
                .get()
                .saturating_sub(self.token_end_bound.get())
                .saturating_add(INPUT_CHUNK_BYTES);
            tree.charge(tag.attrs.len().saturating_mul(span.max(tag.attrs.len())));
            if tag.kind == TagKind::StartTag
                && matches!(
                    tag.name.as_ref(),
                    "a" | "b"
                        | "big"
                        | "code"
                        | "em"
                        | "font"
                        | "i"
                        | "nobr"
                        | "s"
                        | "small"
                        | "strike"
                        | "strong"
                        | "tt"
                        | "u"
                )
            {
                let bytes = tag.attrs.iter().fold(tag.name.len(), |n, attr| {
                    n.saturating_add(attr.name.local.len())
                        .saturating_add(attr.value.len())
                        .saturating_add(1)
                });
                let cost = bytes
                    .saturating_add(1)
                    .saturating_mul(tag.attrs.len().saturating_add(1));
                // History overestimates a list after pruning or markers, avoiding
                // a second implementation of HTML5's formatting-list state.
                tree.charge(
                    self.formatting_cost
                        .get()
                        .saturating_add(self.formatting_count.get().saturating_mul(cost)),
                );
                self.formatting_count
                    .set(self.formatting_count.get().saturating_add(1));
                self.formatting_cost
                    .set(self.formatting_cost.get().saturating_add(cost));
            }
        }
        if !matches!(token, Token::ParseError(_)) {
            self.token_end_bound.set(self.input_bytes.get());
        }
        if tree.check_budget().is_err() {
            // The caller discards this candidate after the current bounded feed.
            // Do not give the tree builder any more work once its budget is gone.
            return TokenSinkResult::Continue;
        }
        self.inner.process_token(token, line)
    }

    fn end(&self) {
        if self.inner.sink.check_budget().is_ok() {
            self.inner.end();
        }
    }

    fn adjusted_current_node_present_but_not_in_html_namespace(&self) -> bool {
        self.inner
            .adjusted_current_node_present_but_not_in_html_namespace()
    }
}

pub(super) fn html_lines(html: &str) -> Result<Vec<Line>, HtmlLimit> {
    let tree = Tree::default();
    tree.anonymous(None); // Document node is always index zero.
    let tokenizer = Tokenizer::new(
        BudgetedBuilder {
            inner: TreeBuilder::new(tree, Default::default()),
            input_bytes: Cell::new(0),
            token_end_bound: Cell::new(0),
            formatting_count: Cell::new(0),
            formatting_cost: Cell::new(0),
        },
        Default::default(),
    );
    let input = BufferQueue::default();
    let mut offset = 0;
    let mut pending = 0;
    while offset < html.len() {
        let end = html.floor_char_boundary((offset + INPUT_CHUNK_BYTES).min(html.len()));
        let tree = &tokenizer.sink.inner.sink;
        let before = tree.mutations.get();
        tree.charge(end - offset);
        tokenizer.sink.input_bytes.set(end);
        input.push_back(StrTendril::from_slice(&html[offset..end]));
        while !matches!(tokenizer.feed(&input), TokenizerResult::Done) {}
        let tree = &tokenizer.sink.inner.sink;
        tree.check_budget()?;
        pending = if tree.mutations.get() == before {
            pending + end - offset
        } else {
            0
        };
        if pending > MAX_PENDING_BYTES {
            return Err(HtmlLimit::PendingToken);
        }
        offset = end;
    }
    tokenizer.end();
    let tree = tokenizer.sink.inner.sink;
    tree.check_budget()?;
    let mut lines = vec![Line {
        kind: Kind::Authored,
        text: String::new(),
    }];
    let mut stack = vec![(0, false, false)];
    while let Some((id, quoted, exiting)) = stack.pop() {
        let node = tree.node(id);
        if let Some(text) = &node.text {
            append_html_text(&mut lines, &text.borrow(), quoted);
            continue;
        }
        let tag = node.name.local.as_ref();
        if matches!(tag, "script" | "style") {
            continue;
        }
        let html_element = node.name.ns == ns!(html);
        if html_element && is_block(tag) {
            boundary(&mut lines);
        }
        if exiting {
            continue;
        }
        if html_element && tag == "br" {
            lines.push(Line {
                kind: Kind::Authored,
                text: String::new(),
            });
        }
        let quoted = quoted || (html_element && tag == "blockquote");
        stack.push((id, quoted, true));
        if let Some(template) = node.template.get() {
            stack.push((template, quoted, false));
        }
        stack.extend(
            node.children
                .borrow()
                .iter()
                .rev()
                .map(|id| (*id, quoted, false)),
        );
    }
    Ok(lines)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn wide_trees_have_a_node_budget() {
        let source = "<span></span>".repeat(MAX_NODES);
        assert_eq!(html_lines(&source).err(), Some(HtmlLimit::Nodes));
    }

    #[test]
    fn attribute_duplicate_checks_are_charged_as_quadratic_work() {
        let attrs = (0..2_100)
            .map(|n| format!("a{n}"))
            .collect::<Vec<_>>()
            .join(" ");
        let source = format!("<p {attrs}>kept</p>");
        assert!(source.len() < MAX_PENDING_BYTES);
        assert_eq!(html_lines(&source).err(), Some(HtmlLimit::Work));
    }

    #[test]
    fn unfinished_tokens_cannot_accumulate_unbounded_attributes_or_text() {
        let source = format!("<p title='{}", "a".repeat(MAX_PENDING_BYTES));
        assert_eq!(html_lines(&source).err(), Some(HtmlLimit::PendingToken));
    }
}
