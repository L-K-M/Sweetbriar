//! Tables (spec/07 "Paste conversion"): a simple table becomes a GFM
//! table, its first row the header; any other becomes one paragraph per
//! row and adds `tables`.

use html5ever::ns;

use super::dom::Tree;
use super::{Emitter, LossSet, PasteLoss, is_block_container, is_html_ws};

impl LossSet {
    fn merge(&mut self, other: LossSet) {
        self.0 |= other.0;
    }
}

impl Emitter {
    pub(super) fn emit_table(&mut self, tree: &Tree, id: usize) {
        let rows = collect_rows(tree, id);
        let simple = rows
            .iter()
            .all(|cells| cells.iter().all(|cell| cell_is_simple(tree, *cell)));
        if !simple {
            self.losses.add(PasteLoss::Tables);
        }
        if rows.is_empty() {
            return;
        }
        if self.flat {
            for cells in &rows {
                let parts: Vec<String> = cells
                    .iter()
                    .map(|&cell| self.render_cell(tree, cell, false))
                    .collect();
                if parts.is_empty() {
                    continue;
                }
                if self.line_has_content && !self.buf.ends_with(' ') {
                    self.buf.push(' ');
                }
                self.buf.push_str(&parts.join(" | "));
                self.line_has_content = true;
            }
            return;
        }
        if !simple {
            for cells in &rows {
                self.start_block();
                let parts: Vec<String> = cells
                    .iter()
                    .map(|&cell| self.render_cell(tree, cell, false))
                    .collect();
                self.buf.push_str(&parts.join(" | "));
                self.line_has_content = true;
                self.flush_line();
                self.end_block();
            }
            return;
        }
        self.start_block();
        let rendered: Vec<Vec<String>> = rows
            .iter()
            .map(|cells| {
                cells
                    .iter()
                    .map(|&cell| self.render_cell(tree, cell, true))
                    .collect()
            })
            .collect();
        let columns = rendered.first().map_or(0, Vec::len);
        for (index, row) in rendered.iter().enumerate() {
            let mut line = self.prefix.clone();
            line.push_str("| ");
            line.push_str(&row.join(" | "));
            line.push_str(" |");
            self.lines.push(line);
            if index == 0 {
                let mut separator = self.prefix.clone();
                separator.push('|');
                for _ in 0..columns.max(1) {
                    separator.push_str(" --- |");
                }
                self.lines.push(separator);
            }
        }
        self.end_block();
    }

    /// A cell's text on one line, whitespace collapsed.
    fn take_text(&mut self) -> String {
        let text = std::mem::take(&mut self.buf);
        let mut out = String::new();
        let mut previous_space = false;
        for c in text.trim_matches(is_html_ws).chars() {
            if is_html_ws(c) {
                if !previous_space {
                    out.push(' ');
                }
                previous_space = true;
            } else {
                out.push(c);
                previous_space = false;
            }
        }
        out.trim_matches(is_html_ws).to_string()
    }

    fn render_cell(&mut self, tree: &Tree, cell: usize, escape_pipes: bool) -> String {
        let mut emitter = Emitter::new(escape_pipes);
        emitter.flat = true;
        emitter.convert(tree, cell);
        let text = emitter.take_text();
        self.losses.merge(emitter.losses);
        text
    }
}

/// The cells of every `tr` belonging to this table, in document order.
/// Nested tables are not descended into.
fn collect_rows(tree: &Tree, table: usize) -> Vec<Vec<usize>> {
    let mut rows = Vec::new();
    let mut stack = vec![(table, false)];
    while let Some((id, nested)) = stack.pop() {
        let node = tree.node(id);
        if node.comment || node.text.is_some() {
            continue;
        }
        let tag: &str = node.name.local.as_ref();
        let nested = nested || (id != table && tag == "table");
        if tag == "tr" && !nested {
            let cells = node
                .children
                .borrow()
                .iter()
                .copied()
                .filter(|cell| {
                    let cell = tree.node(*cell);
                    cell.name.ns == ns!(html) && matches!(cell.name.local.as_ref(), "td" | "th")
                })
                .collect();
            rows.push(cells);
            continue;
        }
        let children = node.children.borrow().clone();
        for child in children.into_iter().rev() {
            stack.push((child, nested));
        }
    }
    rows
}

/// A cell keeps the simple GFM form only when nothing spans, blocks or
/// breaks a line.
fn cell_is_simple(tree: &Tree, cell: usize) -> bool {
    let mut stack = vec![cell];
    while let Some(id) = stack.pop() {
        let node = tree.node(id);
        if node.comment || node.text.is_some() {
            continue;
        }
        if node.name.ns != ns!(html) {
            continue;
        }
        let tag: &str = node.name.local.as_ref();
        let simple = {
            let attrs = node.attrs.borrow();
            attrs.colspan <= 1 && attrs.rowspan <= 1
        };
        if !simple {
            return false;
        }
        if matches!(tag, "td" | "th") && id != cell {
            return false;
        }
        if matches!(
            tag,
            "table"
                | "tr"
                | "caption"
                | "br"
                | "blockquote"
                | "ul"
                | "ol"
                | "li"
                | "pre"
                | "hr"
                | "h1"
                | "h2"
                | "h3"
                | "h4"
                | "h5"
                | "h6"
        ) || is_block_container(tag)
        {
            return false;
        }
        let children = node.children.borrow().clone();
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }
    true
}
