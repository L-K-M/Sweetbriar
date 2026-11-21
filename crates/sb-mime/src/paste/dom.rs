//! The paste conversion's HTML5 parse (spec/07 "Paste conversion"): an
//! arena of nodes keeping only the attributes the conversion reads, fed
//! in bounded turns under node and work budgets, so hostile input is
//! refused instead of exhausting memory, and nothing recurses on its
//! depth. Modeled on `search_document/html.rs`, which keeps no
//! attributes and a smaller budget.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use html5ever::buffer_queue::BufferQueue;
use html5ever::tendril::StrTendril;
use html5ever::tokenizer::{TagKind, Token, TokenSink, TokenSinkResult, Tokenizer};
use html5ever::tree_builder::{
    ElementFlags, NodeOrText, QuirksMode, Tracer, TreeBuilder, TreeBuilderOpts, TreeSink,
};
use html5ever::{Attribute, ExpandedName, QualName, TokenizerResult, local_name, ns};

// Feed the parser in bounded turns; a hostile document is refused by the
// budgets below rather than by an unbounded allocation.
const INPUT_CHUNK_BYTES: usize = 1_024;
const MAX_NODES: usize = 200_000;
// Enough for any well-formed document under the size cap, whose live
// stack stays shallow; quadratic hostile structure runs out long before
// it costs a second.
const MAX_WORK: usize = 80_000_000;
// html5ever compares each new attribute with every earlier one in its
// tag before the tag reaches the tree, so one long token of attributes
// costs quadratic time no budget sees. A token may run past this many
// bytes only while it holds few places an attribute can start: a pasted
// image's base64 `data:` URL is one long quoted value.
const MAX_PENDING_BYTES: usize = 16_384;
const MAX_PENDING_SEPARATORS: usize = 1_024;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum HtmlLimit {
    Nodes,
    Work,
    PendingToken,
}

/// Attributes the conversion reads. Everything else in the tag is
/// discarded; unknown attributes never affect the result.
#[derive(Debug, Default, Clone)]
pub(super) struct Attrs {
    pub(super) href: Option<String>,
    pub(super) alt: Option<String>,
    pub(super) start: Option<i64>,
    pub(super) colspan: u32,
    pub(super) rowspan: u32,
    pub(super) styling: bool,
}

impl Attrs {
    fn read(attrs: &[Attribute]) -> Self {
        let mut out = Attrs {
            colspan: 1,
            rowspan: 1,
            ..Attrs::default()
        };
        for attr in attrs {
            match attr.name.local.as_ref() {
                "href" => out.href = Some(attr.value.to_string()),
                "alt" => out.alt = Some(attr.value.to_string()),
                "start" => out.start = attr.value.parse().ok(),
                "colspan" => out.colspan = attr.value.parse().unwrap_or(1).max(1),
                "rowspan" => out.rowspan = attr.value.parse().unwrap_or(1).max(1),
                "style" | "color" | "bgcolor" | "face" | "size" | "align" => {
                    out.styling = true;
                }
                _ => {}
            }
        }
        out
    }

    /// Merge attributes the tree builder moves onto a reconstructed
    /// formatting element (the adoption agency algorithm).
    fn merge_missing(&mut self, attrs: &[Attribute]) {
        let other = Attrs::read(attrs);
        self.href = self.href.take().or(other.href);
        self.alt = self.alt.take().or(other.alt);
        self.start = self.start.or(other.start);
        if self.colspan == 1 {
            self.colspan = other.colspan;
        }
        if self.rowspan == 1 {
            self.rowspan = other.rowspan;
        }
        self.styling |= other.styling;
    }
}

pub(super) struct Node {
    pub(super) id: usize,
    pub(super) name: QualName,
    pub(super) text: Option<RefCell<StrTendril>>,
    parent: Cell<Option<usize>>,
    pub(super) children: RefCell<Vec<usize>>,
    pub(super) attrs: RefCell<Attrs>,
    pub(super) comment: bool,
    template: Cell<Option<usize>>,
    integration: Cell<bool>,
}

pub(super) type Handle = Rc<Node>;

#[derive(Default)]
pub(super) struct Tree {
    nodes: RefCell<Vec<Handle>>,
    work: Cell<usize>,
    mutations: Cell<usize>,
    parse_errors: Cell<usize>,
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

    pub(super) fn node(&self, id: usize) -> Handle {
        self.charge(1);
        self.nodes.borrow()[id].clone()
    }

    fn create(
        &self,
        name: QualName,
        text: Option<StrTendril>,
        attrs: Attrs,
        comment: bool,
    ) -> Handle {
        self.charge(1);
        self.mutated();
        let mut nodes = self.nodes.borrow_mut();
        let node = Rc::new(Node {
            id: nodes.len(),
            name,
            text: text.map(RefCell::new),
            parent: Cell::new(None),
            children: RefCell::new(Vec::new()),
            attrs: RefCell::new(attrs),
            comment,
            template: Cell::new(None),
            integration: Cell::new(false),
        });
        nodes.push(node.clone());
        node
    }

    fn anonymous(&self, text: Option<StrTendril>) -> Handle {
        self.create(
            QualName::new(None, ns!(), local_name!("")),
            text,
            Attrs::default(),
            false,
        )
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
        let node = self.create(name, None, Attrs::read(&attrs), false);
        node.integration
            .set(flags.mathml_annotation_xml_integration_point);
        if flags.template {
            node.template.set(Some(self.anonymous(None).id));
        }
        node
    }
    fn create_comment(&self, _: StrTendril) -> Handle {
        self.create(
            QualName::new(None, ns!(), local_name!("")),
            None,
            Attrs::default(),
            true,
        )
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
    fn add_attrs_if_missing(&self, node: &Handle, attrs: Vec<Attribute>) {
        self.charge(attrs.len().saturating_mul(attrs.len()));
        node.attrs.borrow_mut().merge_missing(&attrs);
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
        target.integration.get()
    }
}

// Costs html5ever accrues outside TreeSink callbacks, charged from its
// live state instead of an estimate: every token may scan the open
// element stack (scope checks, reconstructing formatting elements), and a
// formatting start tag also compares its name and attributes with the
// active formatting entries (the Noah's Ark clause). `trace_handles`
// visits exactly those, so counting them never costs more than it
// charges. Forward the real tokens; never re-tokenize.
struct BudgetedBuilder {
    inner: TreeBuilder<Handle, Tree>,
}

/// Counts the handles the tree builder holds.
#[derive(Default)]
struct LiveHandles(Cell<usize>);

impl Tracer for LiveHandles {
    type Handle = Handle;

    fn trace_handle(&self, _: &Handle) {
        self.0.set(self.0.get().saturating_add(1));
    }
}

fn is_formatting(name: &str) -> bool {
    matches!(
        name,
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
}

impl TokenSink for BudgetedBuilder {
    type Handle = Handle;

    fn process_token(&self, token: Token, line: u64) -> TokenSinkResult<Handle> {
        let tree = &self.inner.sink;
        tree.charge(1);
        if matches!(token, Token::ParseError(_)) {
            tree.parse_errors
                .set(tree.parse_errors.get().saturating_add(1));
        }
        // The caller discards this candidate after the current bounded
        // feed; give the tree builder no more work once budget is gone.
        if tree.check_budget().is_err() {
            return TokenSinkResult::Continue;
        }
        let live = LiveHandles::default();
        self.inner.trace_handles(&live);
        let mut per_handle = 1;
        if let Token::TagToken(tag) = &token {
            // Duplicate attribute checks are quadratic in the tag's own.
            tree.charge(tag.attrs.len().saturating_mul(tag.attrs.len()));
            if tag.kind == TagKind::StartTag && is_formatting(tag.name.as_ref()) {
                per_handle = tag.attrs.iter().fold(tag.name.len() + 1, |n, attr| {
                    n.saturating_add(attr.name.local.len())
                        .saturating_add(attr.value.len())
                        .saturating_add(1)
                });
            }
        }
        tree.charge(live.0.get().saturating_mul(per_handle));
        if tree.check_budget().is_err() {
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

pub(super) fn parse_fragment(html: &str) -> Result<Tree, HtmlLimit> {
    let tree = Tree::default();
    tree.anonymous(None); // Document node is always index zero.
    let context = tree.create(
        QualName::new(None, ns!(html), local_name!("body")),
        None,
        Attrs::default(),
        false,
    );
    let tokenizer = Tokenizer::new(
        BudgetedBuilder {
            inner: TreeBuilder::new_for_fragment(
                tree,
                context,
                None,
                TreeBuilderOpts {
                    scripting_enabled: false,
                    ..Default::default()
                },
            ),
        },
        Default::default(),
    );
    let input = BufferQueue::default();
    let mut offset = 0;
    let mut pending = 0;
    let mut separators = 0;
    while offset < html.len() {
        let end = html.floor_char_boundary((offset + INPUT_CHUNK_BYTES).min(html.len()));
        {
            let tree = &tokenizer.sink.inner.sink;
            let before = tree.mutations.get();
            let errors_before = tree.parse_errors.get();
            tree.charge(end - offset);
            input.push_back(StrTendril::from_slice(&html[offset..end]));
            while !matches!(tokenizer.feed(&input), TokenizerResult::Done) {}
            let tree = &tokenizer.sink.inner.sink;
            tree.check_budget()?;
            if tree.mutations.get() == before {
                pending += end - offset;
                separators += attribute_separators(&html[offset..end])
                    + (tree.parse_errors.get() - errors_before);
            } else {
                pending = 0;
                separators = 0;
            }
            if pending > MAX_PENDING_BYTES && separators > MAX_PENDING_SEPARATORS {
                return Err(HtmlLimit::PendingToken);
            }
        }
        offset = end;
    }
    tokenizer.end();
    let tree = tokenizer.sink.inner.sink;
    tree.check_budget()?;
    Ok(tree)
}

/// With the parse errors fed alongside, an upper bound on the
/// attributes `text` can start: every attribute after a tag's first
/// follows whitespace, a closing quote, or a `/`, which outside a value
/// is an unexpected-solidus parse error. Inside a quoted value a `/`
/// is neither, so base64 adds nothing.
fn attribute_separators(text: &str) -> usize {
    text.bytes()
        .filter(|b| b.is_ascii_whitespace() || matches!(b, b'"' | b'\''))
        .count()
}

#[cfg(test)]
mod tests;
