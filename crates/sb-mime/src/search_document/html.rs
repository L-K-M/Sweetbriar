//! Minimal HTML5 tree sink. An arena holds every node, so neither traversal nor
//! destruction recurses through hostile nesting. Attributes cannot contribute
//! search text; only the parser's foreign-content integration flag is retained.

use std::borrow::Cow;
use std::cell::{Cell, RefCell};
use std::rc::Rc;

use html5ever::tendril::{StrTendril, TendrilSink};
use html5ever::tree_builder::{ElementFlags, NodeOrText, QuirksMode, TreeSink};
use html5ever::{Attribute, ExpandedName, QualName, local_name, ns};

use super::{Kind, Line, append_html_text, boundary, is_block};

type Handle = Rc<Node>;

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
}

impl Tree {
    fn node(&self, id: usize) -> Handle {
        self.nodes.borrow()[id].clone()
    }

    fn create(&self, name: QualName, text: Option<StrTendril>, integration: bool) -> Handle {
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
            self.node(parent)
                .children
                .borrow_mut()
                .retain(|id| *id != node.id);
        }
    }

    fn insert(&self, parent: &Handle, index: usize, child: NodeOrText<Handle>) {
        let child = match child {
            NodeOrText::AppendNode(node) => {
                self.detach(&node);
                node
            }
            NodeOrText::AppendText(text) => {
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
        target.name.expanded()
    }
    fn create_element(&self, name: QualName, _: Vec<Attribute>, flags: ElementFlags) -> Handle {
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
        let index = parent
            .children
            .borrow()
            .iter()
            .position(|id| *id == sibling.id)
            .expect("sibling belongs to parent");
        self.insert(&parent, index, child);
    }
    fn add_attrs_if_missing(&self, _: &Handle, _: Vec<Attribute>) {}
    fn remove_from_parent(&self, target: &Handle) {
        self.detach(target);
    }
    fn reparent_children(&self, node: &Handle, new_parent: &Handle) {
        let children = node.children.take();
        for id in &children {
            self.node(*id).parent.set(Some(new_parent.id));
        }
        new_parent.children.borrow_mut().extend(children);
    }
    fn is_mathml_annotation_xml_integration_point(&self, target: &Handle) -> bool {
        target.integration
    }
}

pub(super) fn html_lines(html: &str) -> Vec<Line> {
    let tree = Tree::default();
    tree.anonymous(None); // Document node is always index zero.
    let tree = html5ever::parse_document(tree, Default::default()).one(html);
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
    lines
}
