//! Outgoing MIME composition (spec/05 "Composing", M4.2).
//!
//! Pure: no I/O, no clock, no randomness. The caller supplies the
//! Message-ID and Date, and every MIME boundary is derived from the
//! Message-ID, so the same [`ComposeInput`] always yields the same
//! bytes. `mail-builder` writes the message; `pulldown-cmark` parses the
//! Markdown source, raw HTML in it is escaped as literal text, and the
//! generated HTML is sanitized with the reader's allow-list before it is
//! frozen.

use std::collections::HashSet;
use std::fmt;

use lol_html::{RewriteStrSettings, element, rewrite_str};
use mail_builder::MessageBuilder;
use mail_builder::headers::address::Address;
use mail_builder::headers::content_type::ContentType;
use mail_builder::headers::date::Date;
use mail_builder::headers::message_id::MessageId;
use mail_builder::headers::raw::Raw;
use mail_builder::headers::text::Text;
use mail_builder::mime::{BodyPart, MimePart};
use pulldown_cmark::{CowStr, Event, LinkType, Options, Parser, Tag, TagEnd};

use crate::render::{ResourcePolicy, sanitize_run};

/// Total attachment bytes the Proton limits allow (spec/05 "Composing").
pub const MAX_ATTACHMENT_BYTES: u64 = 25_000_000;
/// Attachment count the Proton limits allow (spec/05 "Composing").
pub const MAX_ATTACHMENTS: usize = 100;

/// How the draft body was written.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposeFormat {
    /// Plain text. `literal` disables flowed wrapping and the
    /// `format=flowed` parameter, for patches and code.
    Plain { literal: bool },
    /// Markdown source, sent as `multipart/alternative`.
    Markdown,
}

/// One mailbox in a draft header.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComposeAddress<'a> {
    pub name: Option<&'a str>,
    pub address: &'a str,
}

/// One attached file. `content_id` marks an inline image, which is sent
/// in `multipart/related` with the HTML part and referenced as `cid:`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ComposeAttachment<'a> {
    pub name: &'a str,
    pub mime_type: &'a str,
    pub data: &'a [u8],
    pub content_id: Option<&'a str>,
}

/// Everything [`compose`] needs. No clock or randomness is read.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ComposeInput<'a> {
    pub from: ComposeAddress<'a>,
    pub to: &'a [ComposeAddress<'a>],
    pub cc: &'a [ComposeAddress<'a>],
    /// Envelope-only recipients: never written to a header (spec/05).
    pub bcc: &'a [ComposeAddress<'a>],
    pub reply_to: Option<ComposeAddress<'a>>,
    pub subject: &'a str,
    /// Plain text or Markdown source, per [`ComposeInput::format`].
    pub body: &'a str,
    pub format: ComposeFormat,
    pub attachments: &'a [ComposeAttachment<'a>],
    /// `uuid@domain`, without the angle brackets.
    pub message_id: &'a str,
    /// Unix seconds; written as an RFC 5322 date.
    pub date: i64,
    pub in_reply_to: Option<&'a str>,
    pub references: &'a [&'a str],
    pub user_agent: &'a str,
}

/// The frozen message: the bytes to publish and submit, plus the two
/// body parts for the Proton submission (spec/05).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Composed {
    pub bytes: Vec<u8>,
    pub plain_text: String,
    pub html: Option<String>,
}

/// Composition failure. `InvalidInput` carries a static reason; header
/// values are rejected before `mail-builder` sees them.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposeError {
    TooLarge { total: u64 },
    TooManyFiles { count: usize },
    InvalidInput(&'static str),
}

impl fmt::Display for ComposeError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge { total } => write!(
                f,
                "attachments total {total} bytes, over the {MAX_ATTACHMENT_BYTES} byte limit"
            ),
            Self::TooManyFiles { count } => {
                write!(f, "{count} files, over the {MAX_ATTACHMENTS} file limit")
            }
            Self::InvalidInput(reason) => f.write_str(reason),
        }
    }
}

impl std::error::Error for ComposeError {}

/// Enforce the local Proton limits before any message is built.
pub fn check_limits(attachments: &[ComposeAttachment<'_>]) -> Result<(), ComposeError> {
    let total: u64 = attachments.iter().map(|a| a.data.len() as u64).sum();
    if total > MAX_ATTACHMENT_BYTES {
        return Err(ComposeError::TooLarge { total });
    }
    if attachments.len() > MAX_ATTACHMENTS {
        return Err(ComposeError::TooManyFiles {
            count: attachments.len(),
        });
    }
    Ok(())
}

/// Build one outgoing message.
pub fn compose(input: &ComposeInput<'_>) -> Result<Composed, ComposeError> {
    check_limits(input.attachments)?;
    validate(input)?;
    match input.format {
        ComposeFormat::Plain { literal } => {
            let plain_text = if literal {
                input.body.to_string()
            } else {
                flowed(input.body)
            };
            let bytes = build_mime(input, &plain_text, None)?;
            Ok(Composed {
                bytes,
                plain_text,
                html: None,
            })
        }
        ComposeFormat::Markdown => {
            let rendered = render_markdown(input.body)?;
            let plain_text = flowed(&rendered.plain);
            let minted = minted_cids(input.attachments);
            let html = sanitize_generated(&rendered.html, &minted)?;
            let bytes = build_mime(input, &plain_text, Some(&html))?;
            Ok(Composed {
                bytes,
                plain_text,
                html: Some(html),
            })
        }
    }
}

fn validate(input: &ComposeInput<'_>) -> Result<(), ComposeError> {
    if input.message_id.is_empty() {
        return Err(ComposeError::InvalidInput("message_id must not be empty"));
    }
    // Written verbatim inside `<>`, like an address.
    if input
        .message_id
        .chars()
        .any(|c| c.is_whitespace() || c.is_control() || matches!(c, '<' | '>'))
    {
        return Err(ComposeError::InvalidInput(
            "message_id must not contain whitespace or <>",
        ));
    }
    reject_crlf(input.subject)?;
    reject_crlf(input.user_agent)?;
    if let Some(in_reply_to) = input.in_reply_to {
        reject_crlf(in_reply_to)?;
    }
    for reference in input.references {
        reject_crlf(reference)?;
    }
    validate_address(&input.from)?;
    for address in input.to.iter().chain(input.cc).chain(input.bcc) {
        validate_address(address)?;
    }
    if let Some(reply_to) = &input.reply_to {
        validate_address(reply_to)?;
    }
    for attachment in input.attachments {
        reject_crlf(attachment.name)?;
        if attachment.mime_type.is_empty() {
            return Err(ComposeError::InvalidInput(
                "attachment mime type must not be empty",
            ));
        }
        reject_crlf(attachment.mime_type)?;
        if let Some(content_id) = attachment.content_id {
            reject_crlf(content_id)?;
        }
    }
    Ok(())
}

fn validate_address(address: &ComposeAddress<'_>) -> Result<(), ComposeError> {
    // mail-builder quotes and escapes the display name but writes the address
    // verbatim inside `<>`, so a delimiter there could add recipients.
    if address.address.is_empty() {
        return Err(ComposeError::InvalidInput("an address must not be empty"));
    }
    if address.address.chars().any(|c| {
        c.is_whitespace() || c.is_control() || matches!(c, '<' | '>' | ',' | ';' | '"' | '\\')
    }) {
        return Err(ComposeError::InvalidInput(
            "an address must not contain whitespace or <>,;\"\\",
        ));
    }
    if let Some(name) = address.name {
        reject_crlf(name)?;
    }
    Ok(())
}

fn reject_crlf(value: &str) -> Result<(), ComposeError> {
    if value.contains(['\r', '\n']) {
        return Err(ComposeError::InvalidInput(
            "header value must not contain CR or LF",
        ));
    }
    Ok(())
}

// Plain text: RFC 3676 flowed wrapping

/// Suggested paragraph wrap width (RFC 3676 section 4.2).
const FLOWED_WRAP: usize = 72;

/// Render plain text, wrapping paragraphs as format=flowed.
fn flowed(body: &str) -> String {
    let normalized = body.replace("\r\n", "\n").replace('\r', "\n");
    let mut lines = normalized.split('\n').collect::<Vec<_>>();
    if lines.last().is_some_and(|line| line.is_empty()) {
        lines.pop();
    }
    let mut out = String::with_capacity(normalized.len() + 16);
    let mut first_block = true;
    let mut index = 0;
    while index < lines.len() {
        if lines[index].is_empty() {
            index += 1;
            continue;
        }
        let start = index;
        while index < lines.len() && !lines[index].is_empty() {
            index += 1;
        }
        let block = &lines[start..index];
        if !first_block {
            out.push('\n');
        }
        first_block = false;
        if is_fixed_block(block) {
            // A trailing space marks a flowed line (RFC 3676 4.2), so a
            // fixed line loses its own; "-- " keeps the separator's.
            for line in block {
                let line = if *line == "-- " {
                    line
                } else {
                    line.trim_end_matches(' ')
                };
                push_flowed_line(&mut out, line, Line::Fixed);
            }
        } else {
            let joined = block
                .iter()
                .map(|line| line.trim_end_matches([' ', '\t']))
                .collect::<Vec<_>>()
                .join(" ");
            for line in wrap_paragraph(&joined) {
                push_flowed_line(&mut out, &line, Line::Wrapped);
            }
        }
    }
    if !out.ends_with('\n') {
        out.push('\n');
    }
    out
}

/// Lines that keep their whitespace: quotes, list items, indented code
/// and signature separators. Flowed text must not reflow them.
fn is_fixed_block(block: &[&str]) -> bool {
    block.iter().any(|line| {
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        if indent > 0 && line[..indent].chars().all(|c| c == ' ' || c == '\t') {
            return true;
        }
        [">", "|", "- ", "* ", "+ ", "-- "]
            .iter()
            .any(|marker| trimmed.starts_with(marker))
            || is_ordered_marker(trimmed)
    })
}

fn is_ordered_marker(line: &str) -> bool {
    let digits = line.chars().take_while(|c| c.is_ascii_digit()).count();
    if digits == 0 {
        return false;
    }
    let rest = &line[digits..];
    rest.starts_with(". ") || rest.starts_with(") ")
}

/// Where a flowed line came from, which decides how a leading `>` reads.
#[derive(Clone, Copy, PartialEq)]
enum Line {
    /// Kept as written: a leading `>` is the writer's quote indicator.
    Fixed,
    /// Produced by rewrapping: a `>` at its start is literal text.
    Wrapped,
}

fn push_flowed_line(out: &mut String, line: &str, origin: Line) {
    // RFC 3676 section 4.4 space-stuffing. An unstuffed `>` marks a quote
    // (section 4.5), so only a `>` the wrapping moved to a line start is
    // stuffed.
    if line.starts_with(' ')
        || line.starts_with("From ")
        || (origin == Line::Wrapped && line.starts_with('>'))
    {
        out.push(' ');
    }
    out.push_str(line);
    out.push('\n');
}

/// Greedy wrap; every line except the last carries the flowed trailing
/// space, and a word longer than the width is left whole.
fn wrap_paragraph(text: &str) -> Vec<String> {
    let mut lines: Vec<String> = Vec::new();
    let mut current = String::new();
    for word in text.split([' ', '\t']).filter(|word| !word.is_empty()) {
        if current.is_empty() {
            current.push_str(word);
        } else if current.len() + 1 + word.len() <= FLOWED_WRAP {
            current.push(' ');
            current.push_str(word);
        } else {
            current.push(' ');
            lines.push(std::mem::take(&mut current));
            current.push_str(word);
        }
    }
    if !current.is_empty() {
        lines.push(current);
    }
    if lines.is_empty() {
        lines.push(String::new());
    }
    lines
}

// Markdown

struct Rendered {
    plain: String,
    html: String,
}

fn markdown_options() -> Options {
    let mut options = Options::empty();
    options.insert(Options::ENABLE_TABLES);
    options.insert(Options::ENABLE_STRIKETHROUGH);
    options
}

/// GFM's extended autolinks (spec/05 "Composing"), which
/// `pulldown-cmark` doesn't parse: a bare `https://`, `http://` or
/// `www.` URL in text outside links and code blocks becomes an autolink.
/// Trailing punctuation and an unmatched `)` stay outside the link, as
/// GFM specifies.
fn with_autolinks<'a>(events: impl Iterator<Item = Event<'a>>) -> Vec<Event<'a>> {
    let mut out = Vec::new();
    let mut opaque = 0usize;
    // The parser splits text at possible emphasis delimiters such as
    // `_`; a URL may span such pieces, so adjacent text joins first.
    let mut pending = String::new();
    for event in events {
        if let Event::Text(text) = &event
            && opaque == 0
        {
            pending.push_str(text);
            continue;
        }
        if !pending.is_empty() {
            split_autolinks(&std::mem::take(&mut pending), &mut out);
        }
        match &event {
            Event::Start(Tag::Link { .. } | Tag::Image { .. } | Tag::CodeBlock(_)) => opaque += 1,
            Event::End(TagEnd::Link | TagEnd::Image | TagEnd::CodeBlock) => {
                opaque = opaque.saturating_sub(1);
            }
            _ => {}
        }
        out.push(event);
    }
    if !pending.is_empty() {
        split_autolinks(&pending, &mut out);
    }
    out
}

fn split_autolinks<'a>(text: &str, out: &mut Vec<Event<'a>>) {
    let plain = |piece: &str| Event::Text(CowStr::from(piece.to_owned()));
    let mut rest = text;
    while let Some((start, scheme_len)) = next_url_start(rest) {
        let candidate = &rest[start..];
        let end = candidate
            .find(|c: char| c.is_whitespace() || c == '<')
            .unwrap_or(candidate.len());
        let url = trim_url(&candidate[..end]);
        if url.len() <= scheme_len {
            out.push(plain(&rest[..start + scheme_len]));
            rest = &rest[start + scheme_len..];
            continue;
        }
        if start > 0 {
            out.push(plain(&rest[..start]));
        }
        let scheme = if url.starts_with("www.") {
            "http://"
        } else {
            ""
        };
        out.push(Event::Start(Tag::Link {
            link_type: LinkType::Autolink,
            dest_url: CowStr::from(format!("{scheme}{url}")),
            title: CowStr::Borrowed(""),
            id: CowStr::Borrowed(""),
        }));
        out.push(plain(url));
        out.push(Event::End(TagEnd::Link));
        rest = &rest[start + url.len()..];
    }
    if !rest.is_empty() {
        out.push(plain(rest));
    }
}

/// The next URL start in `text` at a word boundary, and its prefix length.
fn next_url_start(text: &str) -> Option<(usize, usize)> {
    let mut best: Option<(usize, usize)> = None;
    for prefix in ["https://", "http://", "www."] {
        let mut from = 0;
        while let Some(found) = text[from..].find(prefix) {
            let at = from + found;
            let boundary = text[..at]
                .chars()
                .next_back()
                .is_none_or(|c| c.is_whitespace() || matches!(c, '(' | '*' | '_' | '~'));
            if boundary {
                if best.is_none_or(|(b, _)| at < b) {
                    best = Some((at, prefix.len()));
                }
                break;
            }
            from = at + prefix.len();
        }
    }
    best
}

/// GFM's trailing-punctuation rule: `?!.,:*_~'"` never end a URL, and a
/// `)` ends it only when it closes a `(` inside it.
fn trim_url(url: &str) -> &str {
    let mut url = url;
    loop {
        let Some(last) = url.chars().next_back() else {
            return url;
        };
        let unmatched_paren = last == ')' && url.matches(')').count() > url.matches('(').count();
        if matches!(
            last,
            '?' | '!' | '.' | ',' | ':' | '*' | '_' | '~' | '\'' | '"'
        ) || unmatched_paren
        {
            url = &url[..url.len() - last.len_utf8()];
        } else {
            return url;
        }
    }
}

fn render_markdown(source: &str) -> Result<Rendered, ComposeError> {
    let options = markdown_options();
    let plain = {
        let mut renderer = PlainRenderer::default();
        for event in with_autolinks(Parser::new_ext(source, options)) {
            renderer.event(event);
        }
        renderer.finish()
    };
    let html = render_html(source)?;
    Ok(Rendered { plain, html })
}

#[derive(Clone)]
struct LinkInfo {
    dest: String,
    autolink: bool,
}

struct ImageInfo {
    dest: String,
    alt: String,
}

const LINK_AUTOLINKS: &[LinkType] = &[LinkType::Autolink, LinkType::Email];

/// Plain-text rendering of the same events: emphasis markers removed,
/// lists and quotes kept as plain-text conventions, links as
/// `text <url>`, headings without `#`.
#[derive(Default)]
struct PlainRenderer {
    lines: Vec<String>,
    current: String,
    quote_depth: usize,
    lists: Vec<Option<u64>>,
    links: Vec<LinkInfo>,
    image: Option<ImageInfo>,
    in_code_block: bool,
    row_cells: usize,
}

impl PlainRenderer {
    fn prefix(&self) -> String {
        let mut prefix = "> ".repeat(self.quote_depth);
        prefix.push_str(&"  ".repeat(self.lists.len().saturating_sub(1)));
        prefix
    }

    fn flush(&mut self) {
        if !self.current.is_empty() {
            self.lines.push(std::mem::take(&mut self.current));
        }
    }

    fn start_block(&mut self) {
        self.flush();
        if !self.lines.is_empty() && !self.lines.last().is_some_and(String::is_empty) {
            self.lines.push(String::new());
        }
    }

    fn end_block(&mut self) {
        self.flush();
        if !self.lines.last().is_some_and(String::is_empty) {
            self.lines.push(String::new());
        }
    }

    fn break_line(&mut self) {
        self.flush();
        self.current = self.prefix();
    }

    fn append_inline(&mut self, text: &str) {
        if let Some(image) = self.image.as_mut() {
            image.alt.push_str(text);
        } else {
            self.current.push_str(text);
        }
    }

    /// Code blocks and raw HTML keep their line breaks; code is indented
    /// so the flowed writer leaves it fixed.
    fn append_lines(&mut self, text: &str, indent: bool) {
        if let Some(image) = self.image.as_mut() {
            image.alt.push_str(text);
            return;
        }
        for (index, part) in text.split('\n').enumerate() {
            if index > 0 {
                self.break_line();
            }
            if indent {
                self.current.push_str("    ");
            }
            self.current.push_str(part);
        }
    }

    fn event(&mut self, event: Event<'_>) {
        match event {
            Event::Start(tag) => self.start(tag),
            Event::End(tag) => self.end(tag),
            Event::Text(text) => {
                if self.in_code_block {
                    self.append_lines(&text, true);
                } else {
                    self.append_inline(&text);
                }
            }
            Event::Code(code) => self.append_inline(&code),
            Event::Html(html) | Event::InlineHtml(html) => self.append_lines(&html, false),
            Event::SoftBreak | Event::HardBreak => {
                if let Some(image) = self.image.as_mut() {
                    image.alt.push(' ');
                } else {
                    self.break_line();
                }
            }
            Event::Rule => {
                self.start_block();
                self.current.push_str("***");
                self.end_block();
            }
            _ => {}
        }
    }

    fn start(&mut self, tag: Tag<'_>) {
        match tag {
            // A loose list's item opens with a paragraph; its marker
            // stays on the item's first line.
            Tag::Paragraph if !self.current.is_empty() => {}
            Tag::Paragraph | Tag::Heading { .. } => {
                self.start_block();
                self.current = self.prefix();
            }
            Tag::BlockQuote(_) => {
                self.start_block();
                self.quote_depth += 1;
            }
            Tag::CodeBlock(_) => {
                self.start_block();
                self.in_code_block = true;
                self.current = self.prefix();
            }
            Tag::HtmlBlock => {
                self.start_block();
                self.current = self.prefix();
            }
            Tag::List(start) => {
                self.flush();
                self.lists.push(start);
            }
            Tag::Item => {
                self.flush();
                self.current = self.prefix();
                match self.lists.last_mut() {
                    Some(Some(next)) => {
                        self.current.push_str(&format!("{next}. "));
                        *next += 1;
                    }
                    _ => self.current.push_str("- "),
                }
            }
            Tag::Table(_) => self.start_block(),
            Tag::TableHead | Tag::TableRow => {
                self.flush();
                self.current = self.prefix();
                self.current.push_str("| ");
                self.row_cells = 0;
            }
            Tag::TableCell => {
                if self.row_cells > 0 {
                    self.current.push_str(" | ");
                }
                self.row_cells += 1;
            }
            Tag::Emphasis | Tag::Strong | Tag::Strikethrough => {}
            Tag::Link {
                link_type,
                dest_url,
                ..
            } => {
                self.links.push(LinkInfo {
                    dest: dest_url.to_string(),
                    autolink: LINK_AUTOLINKS.contains(&link_type),
                });
            }
            Tag::Image { dest_url, .. } => {
                self.image = Some(ImageInfo {
                    dest: dest_url.to_string(),
                    alt: String::new(),
                });
            }
            _ => {}
        }
    }

    fn end(&mut self, tag: TagEnd) {
        match tag {
            TagEnd::Paragraph | TagEnd::Heading(_) => self.end_block(),
            TagEnd::BlockQuote(_) => {
                self.flush();
                self.quote_depth = self.quote_depth.saturating_sub(1);
                self.end_block();
            }
            TagEnd::CodeBlock => {
                self.flush();
                self.in_code_block = false;
                self.end_block();
            }
            TagEnd::HtmlBlock => self.end_block(),
            TagEnd::List(_) => {
                self.lists.pop();
                if self.lists.is_empty() {
                    self.end_block();
                } else {
                    self.flush();
                }
            }
            TagEnd::Item => self.flush(),
            TagEnd::Table => {
                self.flush();
                self.end_block();
            }
            TagEnd::TableHead => {
                self.current.push_str(" |");
                self.flush();
                let mut separator = String::from("|");
                for _ in 0..self.row_cells.max(1) {
                    separator.push_str(" --- |");
                }
                self.current = self.prefix();
                self.current.push_str(&separator);
                self.flush();
            }
            TagEnd::TableRow => {
                self.current.push_str(" |");
                self.flush();
            }
            TagEnd::TableCell => {}
            TagEnd::Emphasis | TagEnd::Strong | TagEnd::Strikethrough => {}
            TagEnd::Link => {
                if let Some(link) = self.links.pop()
                    && !link.autolink
                    && !link.dest.is_empty()
                {
                    self.current.push_str(" <");
                    self.current.push_str(&link.dest);
                    self.current.push('>');
                }
            }
            TagEnd::Image => {
                if let Some(image) = self.image.take() {
                    let alt = image.alt.trim();
                    if !alt.is_empty() {
                        self.current.push_str(alt);
                    }
                    if !image.dest.is_empty() && !image.dest.starts_with("cid:") {
                        if !alt.is_empty() {
                            self.current.push(' ');
                        }
                        self.current.push('<');
                        self.current.push_str(&image.dest);
                        self.current.push('>');
                    }
                }
            }
            _ => {}
        }
    }

    fn finish(mut self) -> String {
        self.flush();
        while self.lines.last().is_some_and(String::is_empty) {
            self.lines.pop();
        }
        let mut out = self.lines.join("\n");
        out.push('\n');
        out
    }
}

/// HTML rendering: `pulldown-cmark`'s serializer with the built-in
/// stylesheet applied as inline `style=` attributes. Raw HTML events are
/// escaped as text (spec/05 "Composing").
fn render_html(source: &str) -> Result<String, ComposeError> {
    let options = markdown_options();
    let events = with_autolinks(Parser::new_ext(source, options))
        .into_iter()
        .map(|event| match event {
            Event::Html(html) | Event::InlineHtml(html) => Event::Text(html),
            other => other,
        });
    let mut html = String::new();
    pulldown_cmark::html::push_html(&mut html, events);
    let styled = rewrite_str(
        &html,
        RewriteStrSettings::new().append_element_content_handler(element!("*", |el| {
            let Some(declarations) = style_for(&el.tag_name()) else {
                return Ok(());
            };
            let mut merged = el.get_attribute("style").unwrap_or_default();
            if !merged.is_empty() && !merged.ends_with(';') {
                merged.push(';');
            }
            merged.push_str(declarations);
            el.set_attribute("style", &merged)?;
            Ok(())
        })),
    )
    .map_err(|_| ComposeError::InvalidInput("generated HTML style inlining failed"))?;
    let body = style_for("body").unwrap_or_default();
    Ok(format!(
        "<div style=\"{}\">{}</div>",
        escape_attr(body),
        styled
    ))
}

fn escape_attr(value: &str) -> String {
    let mut out = String::with_capacity(value.len());
    for ch in value.chars() {
        match ch {
            '&' => out.push_str("&amp;"),
            '"' => out.push_str("&quot;"),
            '<' => out.push_str("&lt;"),
            '>' => out.push_str("&gt;"),
            _ => out.push(ch),
        }
    }
    out
}

/// The built-in stylesheet (spec/05 "Composing"), one `tag|declarations`
/// line per tag, applied as inline `style=` attributes; the reader's
/// sanitizer checks the properties again (spec/10 "Rendering").
const STYLES: &str = "\
body|font-family:-apple-system, BlinkMacSystemFont, \"Segoe UI\", Helvetica, Arial, sans-serif;font-size:16px;line-height:1.5;color:#1f2328;\n\
p|margin:0 0 1em 0;\n\
h1|font-weight:600;line-height:1.3;margin:1.2em 0 0.5em 0;font-size:1.5em;\n\
h2|font-weight:600;line-height:1.3;margin:1.2em 0 0.5em 0;font-size:1.3em;\n\
h3|font-weight:600;line-height:1.3;margin:1.2em 0 0.5em 0;font-size:1.15em;\n\
h4|font-weight:600;line-height:1.3;margin:1.2em 0 0.5em 0;font-size:1em;\n\
h5|font-weight:600;line-height:1.3;margin:1.2em 0 0.5em 0;font-size:1em;\n\
h6|font-weight:600;line-height:1.3;margin:1.2em 0 0.5em 0;font-size:1em;\n\
blockquote|margin:0 0 1em 1em;padding-left:0.75em;border-left:2px solid #cccccc;color:#555555;\n\
pre|font-family:ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;font-size:0.9em;background-color:#f6f8fa;padding:0.75em;white-space:pre-wrap;\n\
code|font-family:ui-monospace, SFMono-Regular, Menlo, Consolas, monospace;font-size:0.95em;\n\
a|color:#0b57d0;text-decoration:underline;\n\
img|max-width:100%;height:auto;\n\
ul|margin:0 0 1em 0;padding-left:1.5em;\n\
ol|margin:0 0 1em 0;padding-left:1.5em;\n\
li|margin:0.15em 0;\n\
hr|border:0;border-top:1px solid #cccccc;margin:1em 0;\n\
table|border-collapse:collapse;margin:0 0 1em 0;\n\
th|border:1px solid #cccccc;padding:0.35em 0.6em;background-color:#f0f0f0;font-weight:600;\n\
td|border:1px solid #cccccc;padding:0.35em 0.6em;";

fn style_for(tag: &str) -> Option<&'static str> {
    STYLES
        .lines()
        .find_map(|line| line.strip_prefix(tag)?.strip_prefix('|'))
}

// MIME assembly

fn minted_cids(attachments: &[ComposeAttachment<'_>]) -> HashSet<String> {
    attachments
        .iter()
        .filter_map(|attachment| attachment.content_id)
        .map(|content_id| format!("cid:{content_id}"))
        .collect()
}

fn sanitize_generated(html: &str, minted: &HashSet<String>) -> Result<String, ComposeError> {
    sanitize_run(html, minted, ResourcePolicy::Outgoing)
        .map(|sanitized| sanitized.html().to_string())
        .map_err(|_| ComposeError::InvalidInput("generated HTML failed sanitizing"))
}

/// FNV-1a over the Message-ID and the part role: deterministic, unique
/// per message and part, and independent of the clock and thread state.
fn boundary(message_id: &str, role: &str) -> String {
    let mut hash: u64 = 0xcbf2_9ce4_8422_2325;
    for byte in message_id.as_bytes().iter().chain(role.as_bytes()) {
        hash ^= u64::from(*byte);
        hash = hash.wrapping_mul(0x100_0000_01b3);
    }
    format!("----=_Sweetbriar_{hash:016x}_{role}")
}

fn build_mime(
    input: &ComposeInput<'_>,
    plain_text: &str,
    html: Option<&str>,
) -> Result<Vec<u8>, ComposeError> {
    let text_flowed = match input.format {
        ComposeFormat::Plain { literal } => !literal,
        ComposeFormat::Markdown => true,
    };
    let text = text_part(plain_text, text_flowed);

    let mut inline = Vec::new();
    let mut files = Vec::new();
    for attachment in input.attachments {
        // Inline images belong with the HTML part; a plain draft has no
        // HTML to reference them, so they go as ordinary attachments.
        if input.format == ComposeFormat::Markdown && attachment.content_id.is_some() {
            inline.push(attachment);
        } else {
            files.push(attachment);
        }
    }

    let root = match input.format {
        ComposeFormat::Plain { .. } => text,
        ComposeFormat::Markdown => {
            let mut alternative = vec![text];
            if let Some(html) = html {
                if inline.is_empty() {
                    alternative.push(html_part(html));
                } else {
                    let mut related = vec![html_part(html)];
                    related.extend(inline.iter().map(|a| inline_part(a)));
                    alternative.push(multipart(
                        "multipart/related",
                        &boundary(input.message_id, "related"),
                        related,
                    ));
                }
            }
            multipart(
                "multipart/alternative",
                &boundary(input.message_id, "alternative"),
                alternative,
            )
        }
    };
    let root = if files.is_empty() {
        root
    } else {
        let mut children = vec![root];
        children.extend(files.iter().map(|a| attachment_part(a)));
        multipart(
            "multipart/mixed",
            &boundary(input.message_id, "mixed"),
            children,
        )
    };

    let mut builder = MessageBuilder::new()
        .header("Message-ID", MessageId::new(input.message_id))
        .header("Date", Date::new(input.date))
        .header("From", address(&input.from));
    // An empty To or Cc (a Bcc-only draft) is left out (RFC 5322 3.4).
    for (name, list) in [("To", input.to), ("Cc", input.cc)] {
        if !list.is_empty() {
            builder = builder.header(name, address_list(list));
        }
    }
    if let Some(reply_to) = &input.reply_to {
        builder = builder.header("Reply-To", address(reply_to));
    }
    builder = builder.header("Subject", Text::new(input.subject));
    if let Some(in_reply_to) = input.in_reply_to {
        builder = builder.header("In-Reply-To", MessageId::new(in_reply_to));
    }
    if !input.references.is_empty() {
        builder = builder.header(
            "References",
            MessageId::new_list(input.references.iter().copied()),
        );
    }
    builder = builder
        .header("User-Agent", Raw::new(input.user_agent))
        .header("MIME-Version", Raw::new("1.0"));
    Ok(builder
        .body(root)
        .write_to_vec()
        .expect("writing to a Vec cannot fail"))
}

fn text_part(text: &str, flowed: bool) -> MimePart<'_> {
    let content_type = ContentType::new("text/plain").attribute("charset", "utf-8");
    let content_type = if flowed {
        content_type.attribute("format", "flowed")
    } else {
        content_type
    };
    MimePart::new(content_type, BodyPart::Text(text.into()))
}

fn html_part(html: &str) -> MimePart<'_> {
    MimePart::new(
        ContentType::new("text/html").attribute("charset", "utf-8"),
        BodyPart::Text(html.into()),
    )
}

fn attachment_part<'a>(attachment: &ComposeAttachment<'a>) -> MimePart<'a> {
    MimePart::new(
        ContentType::new(attachment.mime_type),
        BodyPart::Binary(attachment.data.into()),
    )
    .attachment(attachment.name)
}

fn inline_part<'a>(attachment: &ComposeAttachment<'a>) -> MimePart<'a> {
    MimePart::new(
        ContentType::new(attachment.mime_type),
        BodyPart::Binary(attachment.data.into()),
    )
    .inline()
    .cid(attachment.content_id.unwrap_or_default())
}

fn multipart<'a>(kind: &str, boundary: &str, children: Vec<MimePart<'a>>) -> MimePart<'a> {
    MimePart::new(
        ContentType::new(kind.to_string()).attribute("boundary", boundary.to_string()),
        BodyPart::Multipart(children),
    )
}

fn address<'a>(address: &ComposeAddress<'a>) -> Address<'a> {
    Address::new_address(address.name, address.address)
}

fn address_list<'a>(addresses: &[ComposeAddress<'a>]) -> Address<'a> {
    Address::new_list(addresses.iter().map(address).collect())
}

#[cfg(test)]
mod tests;
