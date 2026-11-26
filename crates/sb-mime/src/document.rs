//! Versioned semantic reading document (research 12, spec/08 Reader).
//!
//! The pipeline is MIME selection ([`crate::render_input`]) →
//! sanitization ([`crate::render::sanitize_run`]) → this semantic
//! normalize. A document is produced only when every construct in the
//! selected body is one the model represents; anything else is
//! [`DocumentSupport::Unsupported`] and the reader keeps sender layout.
//! Tables, layout tables, receipts and newsletters with complex layout,
//! forms, scripts, positioned content and unknown elements are
//! unsupported by design. The model covers plain personal mail and
//! simple HTML: paragraphs, headings, emphasis, explicit line breaks,
//! preformatted blocks, nested quotes, nested lists, links with their
//! real destination, and image references that are never fetched.

mod html;
mod plain;

use std::collections::{HashMap, HashSet};

use lol_html::{RewriteStrSettings, element, rewrite_str};
use url::Url;

use crate::render::{ResourcePolicy, decode_references, sanitize_run};
use crate::rendition::decode_cid;
use crate::{Limits, ParseFailure, PartId, RelatedResource, RenderInput, SelectedBody};

use self::html::RawNode;

/// Bump when the document semantics change; the reader caches the
/// normalized form with this stamp and rebuilds on upgrade.
pub const DOCUMENT_VERSION: u32 = 2;

/// A positive structure check: a document exists only for supported
/// structures. Unsupported means sender layout, never a lossy guess.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum DocumentSupport {
    Supported(MailDocument),
    Unsupported,
}

/// The semantic reading document. Every node carries the MIME part it
/// came from; [`MailDocument::source`] is the part the reader displayed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailDocument {
    source: PartId,
    blocks: Vec<Block>,
}

impl MailDocument {
    pub fn source(&self) -> &PartId {
        &self.source
    }
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }
    pub fn version(&self) -> u32 {
        DOCUMENT_VERSION
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Block {
    Paragraph(Paragraph),
    Heading(Heading),
    Quote(Quote),
    List(List),
    Preformatted(Preformatted),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Paragraph {
    part: PartId,
    inlines: Vec<Inline>,
}

impl Paragraph {
    pub fn part(&self) -> &PartId {
        &self.part
    }
    pub fn inlines(&self) -> &[Inline] {
        &self.inlines
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Heading {
    part: PartId,
    level: HeadingLevel,
    inlines: Vec<Inline>,
}

impl Heading {
    pub fn part(&self) -> &PartId {
        &self.part
    }
    pub fn level(&self) -> HeadingLevel {
        self.level
    }
    pub fn inlines(&self) -> &[Inline] {
        &self.inlines
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HeadingLevel {
    H1,
    H2,
    H3,
    H4,
    H5,
    H6,
}

impl HeadingLevel {
    fn from_tag(tag: &str) -> Option<Self> {
        Some(match tag {
            "h1" => Self::H1,
            "h2" => Self::H2,
            "h3" => Self::H3,
            "h4" => Self::H4,
            "h5" => Self::H5,
            "h6" => Self::H6,
            _ => return None,
        })
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Quote {
    part: PartId,
    blocks: Vec<Block>,
}

impl Quote {
    pub fn part(&self) -> &PartId {
        &self.part
    }
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct List {
    part: PartId,
    ordered: bool,
    start: i64,
    reversed: bool,
    items: Vec<ListItem>,
}

impl List {
    pub fn part(&self) -> &PartId {
        &self.part
    }
    pub fn ordered(&self) -> bool {
        self.ordered
    }
    pub fn start(&self) -> i64 {
        self.start
    }
    pub fn reversed(&self) -> bool {
        self.reversed
    }
    pub fn items(&self) -> &[ListItem] {
        &self.items
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ListItem {
    part: PartId,
    number: Option<i64>,
    blocks: Vec<Block>,
}

impl ListItem {
    pub fn part(&self) -> &PartId {
        &self.part
    }
    /// The HTML `value` override, or the explicit plain-text number.
    pub fn number(&self) -> Option<i64> {
        self.number
    }
    pub fn blocks(&self) -> &[Block] {
        &self.blocks
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Preformatted {
    part: PartId,
    text: String,
}

impl Preformatted {
    pub fn part(&self) -> &PartId {
        &self.part
    }
    pub fn text(&self) -> &str {
        &self.text
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Inline {
    Text(Text),
    Emphasis(Emphasis),
    Link(Link),
    Image(Image),
    LineBreak(LineBreak),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Text {
    part: PartId,
    text: String,
}

impl Text {
    pub fn part(&self) -> &PartId {
        &self.part
    }
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// `em`/`i` are emphasis, `strong`/`b` are strong. The HTML aliases are
/// the same semantic role in mail.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Emphasis {
    part: PartId,
    strong: bool,
    inlines: Vec<Inline>,
}

impl Emphasis {
    pub fn part(&self) -> &PartId {
        &self.part
    }
    pub fn strong(&self) -> bool {
        self.strong
    }
    pub fn inlines(&self) -> &[Inline] {
        &self.inlines
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Link {
    part: PartId,
    destination: String,
    inlines: Vec<Inline>,
}

impl Link {
    pub fn part(&self) -> &PartId {
        &self.part
    }
    /// The sanitizer's canonical destination, never one rebuilt from the
    /// label.
    pub fn destination(&self) -> &str {
        &self.destination
    }
    pub fn inlines(&self) -> &[Inline] {
        &self.inlines
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Image {
    part: PartId,
    alt: String,
    reference: ImageReference,
}

impl Image {
    /// The part carrying the reference (the HTML body).
    pub fn part(&self) -> &PartId {
        &self.part
    }
    pub fn alt(&self) -> &str {
        &self.alt
    }
    /// Where the image comes from. The pipeline never fetches it.
    pub fn reference(&self) -> &ImageReference {
        &self.reference
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ImageReference {
    /// A `cid:` reference the reader resolves against the current
    /// message's resources; `part_id` is the resource's own part.
    Cid { content_id: String, part_id: PartId },
    /// A remote URL. Kept as a reference; remote content stays blocked.
    Remote { url: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LineBreak {
    part: PartId,
}

impl LineBreak {
    pub fn part(&self) -> &PartId {
        &self.part
    }
}

/// Build the reading document from one admitted message. Unparsable or
/// contained-unwind input returns the parse failure; a message with no
/// representable body is [`DocumentSupport::Unsupported`].
pub fn mail_document(bytes: &[u8], limits: &Limits) -> Result<DocumentSupport, ParseFailure> {
    let input = crate::render_input(bytes, limits)?;
    if let Some(html) = input.html() {
        return Ok(
            html_document(&input, html).map_or(DocumentSupport::Unsupported, |document| {
                DocumentSupport::Supported(document)
            }),
        );
    }
    let Some(plain) = input.plain() else {
        return Ok(DocumentSupport::Unsupported);
    };
    let Some(blocks) = plain::blocks(plain.text(), plain.part_id()) else {
        return Ok(DocumentSupport::Unsupported);
    };
    if blocks.is_empty() {
        return Ok(DocumentSupport::Unsupported);
    }
    Ok(DocumentSupport::Supported(MailDocument {
        source: plain.part_id().clone(),
        blocks,
    }))
}

fn html_document(input: &RenderInput, body: &SelectedBody) -> Option<MailDocument> {
    if !html::detect(body.text()) {
        return None;
    }
    let (rewritten, minted, images) = rewrite_images(body.text(), input.resources())?;
    let sanitized = sanitize_run(&rewritten, &minted, ResourcePolicy::Rewritten).ok()?;
    let roots = html::tree(sanitized.html())?;
    let blocks = build_blocks(&roots, body.part_id(), &images)?;
    if blocks.is_empty() {
        return None;
    }
    Some(MailDocument {
        source: body.part_id().clone(),
        blocks,
    })
}

/// The token used in minted `sweetbriar-cid:` URLs while sanitizing. The
/// reader never serves it; the builder maps each URL back to the
/// resource's content id and part.
const CID_TOKEN: &str = "sb-document";

/// The inverse of [`crate::render::decode_serialized`]: the five escapes the
/// pinned HTML5 serializer emits, ampersands first so none is escaped twice.
fn encode_serialized(value: &str) -> String {
    value
        .replace('&', "&amp;")
        .replace('\u{a0}', "&nbsp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

fn rewrite_images(
    source: &str,
    resources: &[RelatedResource],
) -> Option<(String, HashSet<String>, HashMap<String, ImageReference>)> {
    let minted = std::cell::RefCell::new(HashSet::new());
    let images = std::cell::RefCell::new(HashMap::new());
    let rewritten = rewrite_str(
        source,
        RewriteStrSettings::new().append_element_content_handler(element!("img", |el| {
            let Some(src) = el.get_attribute("src") else {
                return Ok(());
            };
            // lol_html hands back the source attribute value with entities
            // intact. Decode it exactly once here and write the escaped form
            // below, so the sanitizer's later parse restores this same value
            // instead of decoding sender HTML a second time.
            let src = decode_references(&src);
            let final_src = if is_cid(&src) {
                let id = decode_cid(&src[4..]).ok_or("dangling cid reference")?;
                let (index, resource) = resources
                    .iter()
                    .enumerate()
                    .find(|(_, resource)| resource.content_id() == id)
                    .ok_or("unknown cid reference")?;
                let url = format!("sweetbriar-cid://{CID_TOKEN}/r{}", index + 1);
                images.borrow_mut().insert(
                    url.clone(),
                    ImageReference::Cid {
                        content_id: resource.content_id().to_owned(),
                        part_id: resource.part_id().clone(),
                    },
                );
                minted.borrow_mut().insert(url.clone());
                url
            } else {
                let url = Url::parse(&src)
                    .ok()
                    .filter(|url| matches!(url.scheme(), "http" | "https"))
                    .ok_or("unsafe image reference")?;
                let url = url.to_string();
                images
                    .borrow_mut()
                    .insert(url.clone(), ImageReference::Remote { url: url.clone() });
                minted.borrow_mut().insert(url.clone());
                encode_serialized(&url)
            };
            el.set_attribute("src", &final_src)?;
            Ok(())
        })),
    )
    .ok()?;
    Some((rewritten, minted.into_inner(), images.into_inner()))
}

fn is_cid(value: &str) -> bool {
    value
        .get(..4)
        .is_some_and(|prefix| prefix.eq_ignore_ascii_case("cid:"))
}

fn is_inline(tag: &str) -> bool {
    matches!(
        tag,
        "a" | "img" | "em" | "strong" | "i" | "b" | "br" | "span"
    )
}

fn is_block(tag: &str) -> bool {
    matches!(
        tag,
        "p" | "h1" | "h2" | "h3" | "h4" | "h5" | "h6" | "blockquote" | "ul" | "ol" | "pre"
    )
}

fn build_blocks(
    nodes: &[html::RawNode],
    part: &PartId,
    images: &HashMap<String, ImageReference>,
) -> Option<Vec<Block>> {
    let mut blocks = Vec::new();
    let mut inlines: Vec<Inline> = Vec::new();
    for node in nodes {
        let RawNode::Element {
            tag,
            attrs,
            children,
            ..
        } = node
        else {
            let RawNode::Text(text) = node else {
                return None;
            };
            // Whitespace between blocks is the serializer's formatting.
            if text.trim().is_empty() && inlines.is_empty() {
                continue;
            }
            push_inline(
                &mut inlines,
                Inline::Text(Text {
                    part: part.clone(),
                    text: text.clone(),
                }),
            );
            continue;
        };
        if is_inline(tag) {
            inlines.extend(build_inlines(std::slice::from_ref(node), part, images)?);
        } else if matches!(tag.as_str(), "div" | "html" | "body") {
            if !inlines.is_empty() {
                blocks.push(paragraph(part, inlines));
                inlines = Vec::new();
            }
            blocks.extend(build_blocks(children, part, images)?);
        } else if is_block(tag) {
            if !inlines.is_empty() {
                blocks.push(paragraph(part, inlines));
                inlines = Vec::new();
            }
            blocks.push(build_block(tag, attrs, children, part, images)?);
        } else {
            return None;
        }
    }
    if !inlines.is_empty() {
        blocks.push(paragraph(part, inlines));
    }
    Some(blocks)
}

fn build_block(
    tag: &str,
    attrs: &[(String, String)],
    children: &[html::RawNode],
    part: &PartId,
    images: &HashMap<String, ImageReference>,
) -> Option<Block> {
    match tag {
        "p" => Some(Block::Paragraph(Paragraph {
            part: part.clone(),
            inlines: build_inlines(children, part, images)?,
        })),
        "h1" | "h2" | "h3" | "h4" | "h5" | "h6" => Some(Block::Heading(Heading {
            part: part.clone(),
            level: HeadingLevel::from_tag(tag)?,
            inlines: build_inlines(children, part, images)?,
        })),
        "blockquote" => Some(Block::Quote(Quote {
            part: part.clone(),
            blocks: build_blocks(children, part, images)?,
        })),
        "ul" | "ol" => Some(Block::List(build_list(tag, attrs, children, part, images)?)),
        "pre" => Some(Block::Preformatted(Preformatted {
            part: part.clone(),
            text: preformatted(children)?,
        })),
        _ => None,
    }
}

fn build_list(
    tag: &str,
    attrs: &[(String, String)],
    children: &[html::RawNode],
    part: &PartId,
    images: &HashMap<String, ImageReference>,
) -> Option<List> {
    let ordered = tag == "ol";
    // An invalid numbering attribute is ignored, as HTML does; the default
    // numbering stands. A reversed list without a valid start counts down
    // from its item count.
    let start = attr(attrs, "start").and_then(|value| parse_number(value));
    let reversed = attr(attrs, "reversed").is_some();
    let mut items = Vec::new();
    for child in children {
        match child {
            RawNode::Text(text) if text.trim().is_empty() => {}
            RawNode::Element {
                tag,
                attrs,
                children,
            } if tag == "li" => {
                let number = attr(attrs, "value").and_then(|value| parse_number(value));
                items.push(ListItem {
                    part: part.clone(),
                    number,
                    blocks: build_blocks(children, part, images)?,
                });
            }
            _ => return None,
        }
    }
    if items.is_empty() {
        return None;
    }
    let start = start.unwrap_or_else(|| {
        if reversed {
            i64::try_from(items.len()).unwrap_or(i64::MAX)
        } else {
            1
        }
    });
    Some(List {
        part: part.clone(),
        ordered,
        start,
        reversed,
        items,
    })
}

fn build_inlines(
    nodes: &[html::RawNode],
    part: &PartId,
    images: &HashMap<String, ImageReference>,
) -> Option<Vec<Inline>> {
    let mut inlines = Vec::new();
    for node in nodes {
        match node {
            RawNode::Text(text) => push_inline(
                &mut inlines,
                Inline::Text(Text {
                    part: part.clone(),
                    text: text.clone(),
                }),
            ),
            RawNode::Element {
                tag,
                attrs,
                children,
            } => match tag.as_str() {
                // A span and an anchor without a destination are transparent.
                "span" | "a" if tag.as_str() == "span" || attr(attrs, "href").is_none() => {
                    for inline in build_inlines(children, part, images)? {
                        push_inline(&mut inlines, inline);
                    }
                }
                "a" => push_inline(
                    &mut inlines,
                    Inline::Link(Link {
                        part: part.clone(),
                        destination: attr(attrs, "href").expect("checked").clone(),
                        inlines: build_inlines(children, part, images)?,
                    }),
                ),
                "img" => push_inline(
                    &mut inlines,
                    Inline::Image(build_image(attrs, part, images)?),
                ),
                "em" | "i" | "strong" | "b" => {
                    let strong = matches!(tag.as_str(), "strong" | "b");
                    push_inline(
                        &mut inlines,
                        Inline::Emphasis(Emphasis {
                            part: part.clone(),
                            strong,
                            inlines: build_inlines(children, part, images)?,
                        }),
                    );
                }
                "br" => push_inline(
                    &mut inlines,
                    Inline::LineBreak(LineBreak { part: part.clone() }),
                ),
                _ => return None,
            },
        }
    }
    Some(inlines)
}

/// The tokenizer emits character references and adjacent text in separate
/// tokens; one text node keeps the model faithful and readable.
fn push_inline(inlines: &mut Vec<Inline>, inline: Inline) {
    if let Inline::Text(text) = &inline
        && let Some(Inline::Text(previous)) = inlines.last_mut()
    {
        previous.text.push_str(&text.text);
        return;
    }
    inlines.push(inline);
}

fn build_image(
    attrs: &[(String, String)],
    part: &PartId,
    images: &HashMap<String, ImageReference>,
) -> Option<Image> {
    let src = attr(attrs, "src")?;
    let reference = images.get(src)?.clone();
    Some(Image {
        part: part.clone(),
        alt: attr(attrs, "alt").cloned().unwrap_or_default(),
        reference,
    })
}

fn preformatted(children: &[html::RawNode]) -> Option<String> {
    let mut out = String::new();
    preformatted_text(children, &mut out)?;
    // HTML5 drops one line feed right after `<pre>`; the tokenizer kept
    // it because the tree builder's rule is not the tokenizer's.
    if let Some(RawNode::Text(text)) = children.first()
        && text.starts_with('\n')
    {
        out.remove(0);
    }
    Some(out)
}

fn preformatted_text(nodes: &[html::RawNode], out: &mut String) -> Option<()> {
    for node in nodes {
        match node {
            RawNode::Text(text) => out.push_str(text),
            RawNode::Element { tag, children, .. } => match tag.as_str() {
                "br" => out.push('\n'),
                "span" | "b" | "i" | "em" | "strong" => preformatted_text(children, out)?,
                _ => return None,
            },
        }
    }
    Some(())
}

fn paragraph(part: &PartId, inlines: Vec<Inline>) -> Block {
    Block::Paragraph(Paragraph {
        part: part.clone(),
        inlines,
    })
}

fn attr<'a>(attrs: &'a [(String, String)], name: &str) -> Option<&'a String> {
    attrs
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value)
}

fn parse_number(value: &str) -> Option<i64> {
    value.trim().parse::<i64>().ok()
}

#[cfg(test)]
mod tests;
