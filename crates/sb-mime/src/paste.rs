//! Pure HTML-to-Markdown paste conversion (spec/07 "Paste conversion",
//! M4.4e1). No store, clock, network or randomness: the same input always
//! gives the same output. Parsing uses the real HTML5 algorithm and both
//! parsing and traversal are bounded, so hostile nesting cannot exhaust
//! memory or overflow the stack.

use std::fmt;

use html5ever::ns;

use dom::{Handle, Tree, parse_fragment};

mod dom;
mod table;

/// Largest HTML fragment [`markdown_from_html`] accepts, in UTF-8 bytes.
pub const MAX_PASTE_HTML_BYTES: usize = 2 * 1024 * 1024;

const MAX_NESTING: usize = 32;
/// The largest list number CommonMark reads (nine digits).
const MAX_LIST_NUMBER: i64 = 999_999_999;

/// What did not survive the conversion, in spec order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum PasteLoss {
    Images,
    Styling,
    Links,
    Tables,
    Other,
}

/// The Markdown source a paste becomes, and what it lost.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MarkdownFromHtml {
    pub markdown: String,
    pub losses: Vec<PasteLoss>,
}

/// Why a paste was refused.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PasteError {
    TooLarge,
    TooComplex,
}

impl fmt::Display for PasteError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            PasteError::TooLarge => write!(f, "HTML is too large: use at most 2 MiB"),
            PasteError::TooComplex => write!(f, "HTML is too complex to convert"),
        }
    }
}

impl std::error::Error for PasteError {}

#[derive(Debug, Default, Clone, Copy)]
struct LossSet(u8);

impl LossSet {
    fn add(&mut self, loss: PasteLoss) {
        self.0 |= 1 << loss as u8;
    }

    fn sorted(&self) -> Vec<PasteLoss> {
        [
            PasteLoss::Images,
            PasteLoss::Styling,
            PasteLoss::Links,
            PasteLoss::Tables,
            PasteLoss::Other,
        ]
        .into_iter()
        .filter(|loss| self.0 & (1 << *loss as u8) != 0)
        .collect()
    }
}

struct ListCtx {
    ordered: Option<u64>,
    base: String,
}

#[derive(Clone)]
enum FrameKind {
    Strong,
    Emphasis,
    Del,
    Code,
    Link(String),
}

struct Frame {
    id: usize,
    saved: String,
    had_content: bool,
    kind: FrameKind,
    /// [`Emitter::flushed`] when the frame opened: a link whose content
    /// was flushed as lines of its own wrapped block content.
    flushed: usize,
}

/// Renders the parsed tree into blocks of finished lines. An explicit
/// frame stack keeps inline nesting off the call stack.
struct Emitter {
    blocks: Vec<Vec<String>>,
    lines: Vec<String>,
    buf: String,
    prefix: String,
    lists: Vec<ListCtx>,
    quote_depth: usize,
    inline: Vec<Frame>,
    losses: LossSet,
    escape_pipes: bool,
    flat: bool,
    code: usize,
    line_has_content: bool,
    inline_started: bool,
    digit_run: bool,
    in_heading: bool,
    pending_breaks: usize,
    /// The line as a list item opened it, so a paragraph that starts the
    /// item stays on the marker line.
    item_marker: Option<String>,
    /// Where the last emphasis closed, so an adjacent run of the same
    /// kind continues it instead of reading as one longer delimiter run.
    last_close: Option<(&'static str, usize)>,
    /// How many lines have been flushed so far.
    flushed: usize,
    /// How many tables enclose the cells this emitter renders. Each
    /// level renders its cells through a new emitter, so past
    /// [`MAX_NESTING`] a table is walked as a plain container instead,
    /// on `convert`'s explicit stack.
    table_depth: usize,
}

impl Emitter {
    fn new(escape_pipes: bool) -> Self {
        Emitter {
            blocks: Vec::new(),
            lines: Vec::new(),
            buf: String::new(),
            prefix: String::new(),
            lists: Vec::new(),
            quote_depth: 0,
            inline: Vec::new(),
            losses: LossSet::default(),
            escape_pipes,
            flat: false,
            code: 0,
            line_has_content: false,
            inline_started: false,
            digit_run: false,
            in_heading: false,
            pending_breaks: 0,
            item_marker: None,
            last_close: None,
            flushed: 0,
            table_depth: 0,
        }
    }

    fn convert(&mut self, tree: &Tree, root: usize) {
        enum Step {
            Enter(usize),
            Exit(usize),
        }
        let mut stack = vec![Step::Enter(root)];
        while let Some(step) = stack.pop() {
            let (id, entering) = match step {
                Step::Enter(id) => (id, true),
                Step::Exit(id) => (id, false),
            };
            let node = tree.node(id);
            if entering {
                if node.comment {
                    continue;
                }
                if let Some(text) = &node.text {
                    let text = text.borrow();
                    self.push_text(&text);
                    continue;
                }
                if self.enter_element(tree, id, &node) {
                    continue;
                }
                stack.push(Step::Exit(id));
                let children = node.children.borrow().clone();
                for child in children.into_iter().rev() {
                    stack.push(Step::Enter(child));
                }
            } else {
                self.exit_element(id, &node);
            }
        }
    }

    fn enter_element(&mut self, tree: &Tree, id: usize, node: &Handle) -> bool {
        let tag: &str = node.name.local.as_ref();

        // Metadata and non-rendered content disappear without a loss.
        // `title` is metadata too; the fragment parser can materialize it
        // in body context even without a `head`.
        if matches!(
            tag,
            "head" | "script" | "style" | "template" | "noscript" | "title"
        ) {
            return true;
        }

        if matches!(
            tag,
            "img" | "picture" | "svg" | "video" | "audio" | "iframe" | "object" | "embed"
        ) {
            self.losses.add(PasteLoss::Images);
            if tag == "img"
                && let Some(alt) = node.attrs.borrow().alt.as_deref()
                && !alt.is_empty()
            {
                self.push_text(alt);
            }
            return true;
        }

        if matches!(
            tag,
            "input" | "button" | "select" | "textarea" | "option" | "canvas" | "math"
        ) {
            self.losses.add(PasteLoss::Other);
            return true;
        }

        if node.name.ns != ns!(html) {
            return false;
        }

        if node.attrs.borrow().styling
            || matches!(tag, "font" | "u" | "mark" | "sub" | "sup" | "small" | "big")
        {
            self.losses.add(PasteLoss::Styling);
        }

        match tag {
            "br" => {
                self.hard_break();
                true
            }
            "hr" => {
                self.emit_rule();
                true
            }
            "pre" => {
                let text = gather_text(tree, id);
                self.emit_pre(&text);
                true
            }
            "table" if self.table_depth < MAX_NESTING => {
                self.emit_table(tree, id);
                true
            }
            "table" => {
                self.losses.add(PasteLoss::Tables);
                false
            }
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => {
                self.heading(tag.as_bytes()[1] - b'0');
                false
            }
            "blockquote" => {
                self.enter_quote();
                false
            }
            "ul" | "ol" | "menu" | "dir" => {
                self.enter_list(tag, node);
                false
            }
            "li" => {
                self.enter_item();
                false
            }
            "strong" | "b" => {
                self.open_frame(id, FrameKind::Strong);
                false
            }
            "em" | "i" => {
                self.open_frame(id, FrameKind::Emphasis);
                false
            }
            "del" | "s" | "strike" => {
                self.open_frame(id, FrameKind::Del);
                false
            }
            "code" => {
                self.open_frame(id, FrameKind::Code);
                self.code += 1;
                false
            }
            "a" => {
                self.enter_link(node);
                false
            }
            _ if is_block_container(tag) => {
                if self.item_marker.as_ref() != Some(&self.buf) {
                    self.start_block();
                }
                false
            }
            _ => false,
        }
    }

    fn exit_element(&mut self, id: usize, node: &Handle) {
        let tag: &str = node.name.local.as_ref();
        match tag {
            "blockquote" => self.exit_quote(),
            "ul" | "ol" | "menu" | "dir" => self.exit_list(),
            "li" => self.exit_item(),
            "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => self.end_heading(),
            "strong" | "b" => self.close_emphasis(id, "**"),
            "em" | "i" => self.close_emphasis(id, "*"),
            "del" | "s" | "strike" => self.close_emphasis(id, "~~"),
            "code" => {
                self.code = self.code.saturating_sub(1);
                self.close_code(id);
            }
            "a" => self.close_link(id),
            _ if is_block_container(tag) => self.end_block(),
            _ => {}
        }
    }

    fn push_text(&mut self, text: &str) {
        if self.buf.is_empty() && !self.line_has_content && self.inline.is_empty() {
            self.buf = self.prefix.clone();
        }
        let chars: Vec<char> = text.chars().collect();
        let mut index = 0;
        while index < chars.len() {
            let c = chars[index];
            if self.code > 0 {
                if is_html_ws(c) {
                    if !self.buf.ends_with(' ') {
                        self.buf.push(' ');
                    }
                } else {
                    self.materialize_breaks();
                    self.buf.push(c);
                    self.line_has_content = true;
                    self.inline_started = true;
                }
            } else if is_html_ws(c) {
                if !self.buf.ends_with(' ') {
                    self.buf.push(' ');
                }
            } else {
                self.materialize_breaks();
                self.push_escaped(c, &chars, index);
            }
            index += 1;
        }
    }

    /// Turn queued `br` elements into backslash line ends. A break with
    /// nothing before it, or at the end of a block, is dropped: the
    /// backslash is only a break when another line follows.
    fn materialize_breaks(&mut self) {
        let breaks = std::mem::take(&mut self.pending_breaks);
        if self.flat || breaks == 0 || !self.inline_started {
            return;
        }
        for _ in 0..breaks {
            let line = self.buf.trim_end_matches(is_html_ws).to_string();
            self.buf = line;
            self.buf.push('\\');
            if self.inline.is_empty() {
                self.flush_line();
            } else {
                // Inside emphasis or a link the break stays in the run,
                // so its markers span it: `**a\` then `b**`.
                self.buf.push('\n');
                self.buf.push_str(&self.prefix.clone());
                self.digit_run = false;
                self.last_close = None;
            }
        }
        self.inline_started = false;
    }

    fn push_escaped(&mut self, c: char, chars: &[char], index: usize) {
        let at_line_start = !self.inline_started;
        let escape = match c {
            '\\' | '`' | '*' | '_' | '[' | ']' | '~' | '<' => true,
            '!' => chars.get(index + 1) == Some(&'['),
            '&' => entity_like(chars, index),
            '|' => self.escape_pipes,
            // A `#` anywhere in an ATX heading could be read as its
            // closing sequence, so headings escape all of them.
            '#' => at_line_start || self.in_heading,
            '>' | '+' | '-' | '=' => at_line_start,
            // A backslash cannot escape a digit, so the run's `.` or `)`
            // gets the backslash; that keeps the line from becoming a
            // list item while rendering the same text.
            '.' | ')' => self.digit_run,
            _ => false,
        };
        if c.is_ascii_digit() {
            self.digit_run = self.digit_run || !self.inline_started;
        } else if !matches!(c, '.' | ')') {
            self.digit_run = false;
        }
        if escape {
            self.buf.push('\\');
        }
        self.buf.push(c);
        self.line_has_content = true;
        self.inline_started = true;
        if matches!(c, '.' | ')') {
            self.digit_run = false;
        }
    }

    fn open_frame(&mut self, id: usize, kind: FrameKind) {
        self.materialize_breaks();
        self.inline.push(Frame {
            id,
            saved: std::mem::take(&mut self.buf),
            had_content: self.line_has_content,
            kind,
            flushed: self.flushed,
        });
        self.line_has_content = false;
    }

    fn pop_frame(&mut self, id: usize) -> Option<Frame> {
        let position = self.inline.iter().rposition(|frame| frame.id == id)?;
        Some(self.inline.remove(position))
    }

    fn close_emphasis(&mut self, id: usize, marker: &'static str) {
        let Some((frame, inner, inner_content)) = self.close_frame(id) else {
            return;
        };
        // Emphasis around blocks keeps only its last line's markers.
        if self.flushed != frame.flushed {
            self.losses.add(PasteLoss::Styling);
        }
        let (lead, core, trail) = split_edges(&inner);
        if core.is_empty() {
            return;
        }
        // Inside a code span the markers would be literal text.
        if self.code > 0 {
            self.keep_inner(&inner, inner_content, PasteLoss::Styling);
            return;
        }
        if lead.is_empty() && self.last_close == Some((marker, self.buf.len())) {
            self.buf.truncate(self.buf.len() - marker.len());
        } else {
            self.buf.push_str(lead);
            self.buf.push_str(marker);
        }
        self.buf.push_str(core);
        self.buf.push_str(marker);
        self.last_close = Some((marker, self.buf.len()));
        self.buf.push_str(trail);
        self.line_has_content = true;
        self.inline_started = true;
    }

    fn close_code(&mut self, id: usize) {
        let Some((_, inner, _)) = self.close_frame(id) else {
            return;
        };
        if inner.is_empty() {
            return;
        }
        let fence = "`".repeat(longest_run(&inner, '`') + 1);
        // Pad both sides when either edge is a backtick, or when both edges
        // are spaces around other text; CommonMark strips one space from
        // each side again.
        let spaced =
            inner.starts_with(' ') && inner.ends_with(' ') && !inner.trim_matches(' ').is_empty();
        let pad = inner.starts_with('`') || inner.ends_with('`') || spaced;
        self.buf.push_str(&fence);
        if pad {
            self.buf.push(' ');
        }
        self.buf.push_str(&inner);
        if pad {
            self.buf.push(' ');
        }
        self.buf.push_str(&fence);
        self.line_has_content = true;
        self.inline_started = true;
    }

    fn enter_link(&mut self, node: &Handle) {
        let href = node.attrs.borrow().href.as_deref().map(clean_href);
        match href {
            Some(href) if is_safe_href(&href) => {
                self.open_frame(node.id, FrameKind::Link(href));
            }
            Some(_) => self.losses.add(PasteLoss::Links),
            None => {}
        }
    }

    fn close_link(&mut self, id: usize) {
        let Some((frame, inner, inner_content)) = self.close_frame(id) else {
            return;
        };
        let FrameKind::Link(url) = frame.kind else {
            return;
        };
        // Link syntax is literal text inside a code span, and inline
        // syntax cannot span the blocks a link may wrap.
        if self.code > 0 || self.flushed != frame.flushed {
            self.keep_inner(&inner, inner_content, PasteLoss::Links);
            return;
        }
        let text = if inner.trim_matches(is_html_ws).is_empty() {
            let mut escaped = String::new();
            escape_plain(&mut escaped, &url, self.escape_pipes);
            escaped
        } else {
            inner
        };
        self.buf.push('[');
        self.buf.push_str(&text);
        self.buf.push_str("](");
        self.buf.push_str(&link_dest(&url));
        self.buf.push(')');
        self.line_has_content = true;
        self.inline_started = true;
    }

    /// Pop a frame and restore the line it interrupted, returning the
    /// frame, its content and whether that content has text.
    fn close_frame(&mut self, id: usize) -> Option<(Frame, String, bool)> {
        let frame = self.pop_frame(id)?;
        let inner_content = self.line_has_content;
        let mut inner = std::mem::take(&mut self.buf);
        self.buf = frame.saved.clone();
        self.line_has_content = frame.had_content;
        // A frame that saw a line flushed lost its saved text to that
        // line; its content now starts the current line, prefix first.
        if self.flushed != frame.flushed
            && self.buf.is_empty()
            && let Some(rest) = inner.strip_prefix(self.prefix.as_str())
        {
            self.buf = self.prefix.clone();
            inner = rest.to_string();
        }
        Some((frame, inner, inner_content))
    }

    /// Keep a closed frame's content without its decoration.
    fn keep_inner(&mut self, inner: &str, inner_content: bool, loss: PasteLoss) {
        self.losses.add(loss);
        self.buf.push_str(inner);
        if inner_content {
            self.line_has_content = true;
            self.inline_started = true;
        }
    }

    fn heading(&mut self, level: u8) {
        if self.flat {
            return;
        }
        self.start_block();
        self.buf = format!("{}{} ", self.prefix, "#".repeat(level as usize));
        self.line_has_content = true;
        self.in_heading = true;
    }

    fn end_heading(&mut self) {
        if !self.flat {
            self.in_heading = false;
            self.flush_line();
            self.end_block();
        }
    }

    fn enter_quote(&mut self) {
        if self.flat {
            return;
        }
        self.start_block();
        if self.quote_depth < MAX_NESTING {
            self.prefix.push_str("> ");
            if !self.line_has_content {
                self.buf = self.prefix.clone();
            }
        }
        self.quote_depth += 1;
    }

    fn exit_quote(&mut self) {
        if self.flat {
            return;
        }
        self.end_block();
        self.quote_depth = self.quote_depth.saturating_sub(1);
        if self.quote_depth < MAX_NESTING {
            self.prefix.truncate(self.prefix.len().saturating_sub(2));
        }
    }

    fn enter_list(&mut self, tag: &str, node: &Handle) {
        if self.lists.is_empty() {
            self.start_block();
        }
        let base = if self.lists.len() < MAX_NESTING {
            self.prefix.clone()
        } else {
            self.lists
                .last()
                .map(|ctx| ctx.base.clone())
                .unwrap_or_default()
        };
        // CommonMark reads at most nine digits as a list number.
        let ordered = if tag == "ol" {
            Some(
                node.attrs
                    .borrow()
                    .start
                    .unwrap_or(1)
                    .clamp(1, MAX_LIST_NUMBER) as u64,
            )
        } else {
            None
        };
        self.lists.push(ListCtx { ordered, base });
    }

    fn exit_list(&mut self) {
        self.lists.pop();
        if self.lists.is_empty() {
            self.end_block();
        }
    }

    fn enter_item(&mut self) {
        if self.flat {
            if self.line_has_content && !self.buf.ends_with(' ') {
                self.buf.push(' ');
            }
            return;
        }
        // An `li` outside any list (the parser keeps a stray one) is a
        // paragraph.
        let Some(ctx) = self.lists.last_mut() else {
            self.start_block();
            return;
        };
        let marker = match ctx.ordered {
            Some(number) => {
                let marker = format!("{number}. ");
                ctx.ordered = Some((number + 1).min(MAX_LIST_NUMBER as u64));
                marker
            }
            None => "- ".to_string(),
        };
        let base = ctx.base.clone();
        self.flush_line();
        self.buf = format!("{base}{marker}");
        self.item_marker = Some(self.buf.clone());
        self.prefix = format!("{base}{}", " ".repeat(marker.chars().count()));
        self.line_has_content = true;
    }

    fn exit_item(&mut self) {
        if self.flat {
            if self.line_has_content && !self.buf.ends_with(' ') {
                self.buf.push(' ');
            }
            return;
        }
        let Some(base) = self.lists.last().map(|ctx| ctx.base.clone()) else {
            self.end_block();
            return;
        };
        self.flush_line();
        self.prefix = base;
    }

    fn hard_break(&mut self) {
        // An ATX heading is a single line, and a code span can't hold a
        // break, so a break inside either is a space.
        if self.flat || self.in_heading || self.code > 0 {
            if !self.buf.ends_with(' ') {
                self.buf.push(' ');
            }
            return;
        }
        // Cap a run of breaks: hostile input cannot amplify into a
        // million-line output.
        self.pending_breaks = (self.pending_breaks + 1).min(4);
    }

    fn emit_rule(&mut self) {
        if self.flat {
            if self.line_has_content && !self.buf.ends_with(' ') {
                self.buf.push(' ');
            }
            return;
        }
        self.start_block();
        self.lines.push(format!("{}---", self.prefix));
        self.end_block();
    }

    fn emit_pre(&mut self, text: &str) {
        if self.flat {
            self.push_text(text);
            return;
        }
        self.start_block();
        let fence_len = longest_run(text, '`') + 1;
        let fence = "`".repeat(fence_len.max(3));
        let content = text.strip_suffix('\n').unwrap_or(text);
        let mut body = String::new();
        body.push_str(&fence);
        body.push('\n');
        if !content.is_empty() {
            body.push_str(content);
            body.push('\n');
        }
        body.push_str(&fence);
        for line in body.split('\n') {
            let line = line.trim_end_matches(is_html_ws);
            if line.is_empty() {
                // A blank line without the quote's marker would end it.
                self.lines.push(self.prefix.trim_end().to_string());
            } else {
                self.lines.push(format!("{}{}", self.prefix, line));
            }
        }
        self.end_block();
    }

    fn start_block(&mut self) {
        if !self.flat {
            self.end_block();
        }
    }

    fn end_block(&mut self) {
        if self.flat {
            return;
        }
        self.flush_line();
        self.pending_breaks = 0;
        if !self.lines.is_empty() {
            self.blocks.push(std::mem::take(&mut self.lines));
        }
    }

    fn flush_line(&mut self) {
        // A line ending inside open frames takes the text they saved, in
        // order; their decoration cannot span lines.
        let mut had_content = self.line_has_content;
        let mut line = String::new();
        for frame in &mut self.inline {
            line.push_str(&std::mem::take(&mut frame.saved));
            had_content |= std::mem::take(&mut frame.had_content);
        }
        line.push_str(&std::mem::take(&mut self.buf));
        self.buf = self.prefix.clone();
        self.line_has_content = false;
        self.inline_started = false;
        self.digit_run = false;
        self.item_marker = None;
        self.last_close = None;
        if !had_content {
            return;
        }
        let trimmed = line.trim_end_matches(is_html_ws);
        let cleaned = match trimmed.strip_prefix(self.prefix.as_str()) {
            Some(rest) => format!("{}{}", self.prefix, rest.trim_start_matches(is_html_ws)),
            None => trimmed.to_string(),
        };
        if !cleaned.is_empty() {
            self.lines.push(cleaned);
            self.flushed += 1;
        }
    }

    fn finish(&mut self) -> String {
        self.end_block();
        let mut out = String::new();
        let mut previous: Option<&str> = None;
        for block in &self.blocks {
            if let Some(previous) = previous {
                out.push('\n');
                out.push_str(&quote_separator(previous, &block[0]));
                out.push('\n');
            }
            out.push_str(&block.join("\n"));
            previous = block.last().map(String::as_str);
        }
        out
    }
}

/// The blank line between two blocks: inside a quote it keeps the
/// quote markers both share, or the quote would end there.
fn quote_separator(above: &str, below: &str) -> String {
    let shared: String = above
        .chars()
        .zip(below.chars())
        .take_while(|(a, b)| a == b && matches!(a, '>' | ' '))
        .map(|(a, _)| a)
        .collect();
    shared.trim_end().to_string()
}

fn is_block_container(tag: &str) -> bool {
    matches!(
        tag,
        "address"
            | "article"
            | "aside"
            | "center"
            | "dd"
            | "details"
            | "dialog"
            | "dir"
            | "div"
            | "dl"
            | "dt"
            | "fieldset"
            | "figcaption"
            | "figure"
            | "footer"
            | "form"
            | "header"
            | "hgroup"
            | "legend"
            | "main"
            | "menu"
            | "nav"
            | "p"
            | "search"
            | "section"
            | "summary"
    )
}

fn is_html_ws(c: char) -> bool {
    matches!(c, ' ' | '\t' | '\n' | '\r' | '\x0c')
}

fn longest_run(text: &str, target: char) -> usize {
    let mut best = 0;
    let mut current = 0;
    for c in text.chars() {
        if c == target {
            current += 1;
            best = best.max(current);
        } else {
            current = 0;
        }
    }
    best
}

fn split_edges(text: &str) -> (&str, &str, &str) {
    let lead_len = text.len() - text.trim_start_matches(is_html_ws).len();
    let lead = &text[..lead_len];
    let rest = &text[lead_len..];
    let trail_len = rest.len() - rest.trim_end_matches(is_html_ws).len();
    let core = &rest[..rest.len() - trail_len];
    let trail = &rest[rest.len() - trail_len..];
    (lead, core, trail)
}

fn entity_like(chars: &[char], index: usize) -> bool {
    let mut cursor = index + 1;
    if cursor >= chars.len() {
        return false;
    }
    if chars[cursor] == '#' {
        cursor += 1;
        if cursor < chars.len() && matches!(chars[cursor], 'x' | 'X') {
            cursor += 1;
        }
        let digits = cursor;
        while cursor < chars.len() && chars[cursor].is_ascii_hexdigit() {
            cursor += 1;
        }
        if cursor == digits {
            return false;
        }
    } else {
        let name = cursor;
        while cursor < chars.len() && chars[cursor].is_ascii_alphanumeric() {
            cursor += 1;
        }
        if cursor == name {
            return false;
        }
    }
    cursor < chars.len() && chars[cursor] == ';'
}

/// The URL a browser would follow: without edge whitespace, tabs or
/// line breaks (WHATWG URL parsing drops them), and other control
/// characters percent-encoded as its serializer does; a Markdown
/// destination may not contain them.
fn clean_href(href: &str) -> String {
    let mut out = String::new();
    let trimmed = href.trim_matches(|c: char| c.is_ascii_whitespace() || c.is_ascii_control());
    for c in trimmed.chars().filter(|c| !matches!(c, '\t' | '\n' | '\r')) {
        if c.is_ascii_control() {
            out.push_str(&format!("%{:02X}", c as u32));
        } else {
            out.push(c);
        }
    }
    out
}

fn is_safe_href(href: &str) -> bool {
    let scheme = href.split(':').next().unwrap_or("");
    matches!(
        scheme.to_ascii_lowercase().as_str(),
        "http" | "https" | "mailto"
    )
}

fn link_dest(url: &str) -> String {
    let wrap = url
        .chars()
        .any(|c| matches!(c, ' ' | '(' | ')' | '<' | '>'));
    // CommonMark reads backslash escapes inside a destination, so a
    // literal backslash is doubled in both forms.
    let url = url.replace('\\', "\\\\");
    if !wrap {
        return url;
    }
    let cleaned = url.replace('<', "%3C").replace('>', "%3E");
    format!("<{cleaned}>")
}

fn escape_plain(out: &mut String, text: &str, escape_pipes: bool) {
    let chars: Vec<char> = text.chars().collect();
    for (index, &c) in chars.iter().enumerate() {
        let escape = match c {
            '\\' | '`' | '*' | '_' | '[' | ']' | '~' | '<' => true,
            '&' => entity_like(&chars, index),
            '|' => escape_pipes,
            _ => false,
        };
        if escape {
            out.push('\\');
        }
        out.push(c);
    }
}

/// Concatenate the rendered text of a subtree, verbatim apart from `br`
/// becoming a newline. Used for `pre`, where nothing is escaped or
/// collapsed; `emit_pre` then trims each line's end, as no output line
/// ends in whitespace.
fn gather_text(tree: &Tree, root: usize) -> String {
    let mut out = String::new();
    let mut stack = vec![root];
    while let Some(id) = stack.pop() {
        let node = tree.node(id);
        if node.comment {
            continue;
        }
        if let Some(text) = &node.text {
            out.push_str(&text.borrow());
            continue;
        }
        let tag: &str = node.name.local.as_ref();
        if matches!(
            tag,
            "script" | "style" | "template" | "noscript" | "title" | "head"
        ) {
            continue;
        }
        if tag == "br" {
            out.push('\n');
            continue;
        }
        let children = node.children.borrow().clone();
        for child in children.into_iter().rev() {
            stack.push(child);
        }
    }
    out
}

/// Convert an HTML fragment to Markdown, recording what did not survive.
pub fn markdown_from_html(html: &str) -> Result<MarkdownFromHtml, PasteError> {
    if html.len() > MAX_PASTE_HTML_BYTES {
        return Err(PasteError::TooLarge);
    }
    let tree = parse_fragment(html).map_err(|_| PasteError::TooComplex)?;
    let mut emitter = Emitter::new(false);
    emitter.convert(&tree, 0);
    let markdown = emitter.finish();
    let losses = emitter.losses.sorted();
    Ok(MarkdownFromHtml { markdown, losses })
}

#[cfg(test)]
mod proptests;
#[cfg(test)]
mod tests;
