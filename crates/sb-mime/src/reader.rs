//! Owned reader input. HTML here is decoded, never safe to display in a web view.

use std::borrow::Cow;

use mail_parser::decoders::charsets::map::charset_decoder;
use mail_parser::parsers::MessageStream;
use mail_parser::{Encoding, Message, MessageParser, MessagePart, MimeHeaders, PartType};

use crate::{Limits, ParseFailure, ParseWarning, verify, walk};

/// Opaque MIME-tree identity, stable for the same acquisition bytes.
/// IDs must only be used with the acquisition which produced them.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct PartId(String);

impl PartId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

/// Warning codes produced by reader extraction (spec/07).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RenderWarningCode {
    PartialParse,
    CharsetReplaced,
    UnsupportedContent,
    AmbiguousCid,
    MissingCid,
    UnsafeUrl,
    RenditionLimit,
    CountSaturated,
}

impl RenderWarningCode {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::PartialParse => "partial_parse",
            Self::CharsetReplaced => "charset_replaced",
            Self::UnsupportedContent => "unsupported_content",
            Self::AmbiguousCid => "ambiguous_cid",
            Self::MissingCid => "missing_cid",
            Self::UnsafeUrl => "unsafe_url",
            Self::RenditionLimit => "rendition_limit",
            Self::CountSaturated => "count_saturated",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderWarning {
    code: RenderWarningCode,
    part_id: Option<PartId>,
}

impl RenderWarning {
    pub(crate) fn new(code: RenderWarningCode, part_id: Option<PartId>) -> Self {
        Self { code, part_id }
    }

    pub fn code(&self) -> RenderWarningCode {
        self.code
    }

    pub fn part_id(&self) -> Option<&PartId> {
        self.part_id.as_ref()
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SelectedBody {
    part_id: PartId,
    text: String,
}

impl SelectedBody {
    pub fn part_id(&self) -> &PartId {
        &self.part_id
    }

    /// Literal plain text, or raw decoded HTML. HTML still requires sanitizing.
    pub fn text(&self) -> &str {
        &self.text
    }
}

/// A potential CID resource, scoped to the selected HTML's related container.
/// Ambiguous IDs are excluded. Media types are declared, not signature-verified.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RelatedResource {
    content_id: String,
    part_id: PartId,
    media_type: String,
    bytes: Vec<u8>,
}

impl RelatedResource {
    pub fn content_id(&self) -> &str {
        &self.content_id
    }
    pub fn part_id(&self) -> &PartId {
        &self.part_id
    }
    pub fn media_type(&self) -> &str {
        &self.media_type
    }
    pub fn bytes(&self) -> &[u8] {
        &self.bytes
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentDescriptor {
    part_id: PartId,
    display_name: String,
    media_type: String,
    decoded_length: Option<u64>,
}

impl AttachmentDescriptor {
    pub fn part_id(&self) -> &PartId {
        &self.part_id
    }
    /// Display only; never a filesystem path. Bounded to 255 UTF-8 bytes.
    pub fn display_name(&self) -> &str {
        &self.display_name
    }
    pub fn media_type(&self) -> &str {
        &self.media_type
    }
    /// None when damaged transfer encoding prevents a reliable decoded size.
    pub fn decoded_length(&self) -> Option<u64> {
        self.decoded_length
    }
}

/// Bounded input for a later renderer. A rejected admission returns no content
/// and a partial-parse warning; an admitted empty body remains `Some("")`.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RenderInput {
    plain: Option<SelectedBody>,
    html: Option<SelectedBody>,
    resources: Vec<RelatedResource>,
    ambiguous_cids: Vec<String>,
    attachments: Vec<AttachmentDescriptor>,
    part_ids: Vec<PartId>,
    warnings: Vec<RenderWarning>,
    parse_warnings: Vec<ParseWarning>,
}

impl RenderInput {
    pub fn plain(&self) -> Option<&SelectedBody> {
        self.plain.as_ref()
    }
    pub fn html(&self) -> Option<&SelectedBody> {
        self.html.as_ref()
    }
    pub fn resources(&self) -> &[RelatedResource] {
        &self.resources
    }
    pub(crate) fn ambiguous_cids(&self) -> &[String] {
        &self.ambiguous_cids
    }
    /// Every non-selected leaf part outside attached messages; an attached
    /// message is one attachment.
    pub fn attachments(&self) -> &[AttachmentDescriptor] {
        &self.attachments
    }
    /// All admitted entities, including containers and embedded messages.
    pub fn part_ids(&self) -> &[PartId] {
        &self.part_ids
    }
    pub fn warnings(&self) -> &[RenderWarning] {
        &self.warnings
    }
    /// Admission/verification details behind `partial_parse`, including limits.
    pub fn parse_warnings(&self) -> &[ParseWarning] {
        &self.parse_warnings
    }

    fn warn(&mut self, code: RenderWarningCode, part_id: Option<PartId>) {
        self.warnings.push(RenderWarning { code, part_id });
    }
}

/// Failure to extract one attachment. No parser payloads or sender text escape.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AttachmentError {
    Parse(ParseFailure),
    /// Admission or verification rejected the message under the supplied limits.
    LimitExceeded,
    UnknownPart,
    BodyPart,
    InvalidEncoding,
}

/// Select reader bodies after the same admission and verification as [`crate::parse`].
/// Unwinds are contained across admission, decoding and extraction.
pub fn render_input(bytes: &[u8], limits: &Limits) -> Result<RenderInput, ParseFailure> {
    std::panic::catch_unwind(|| reader_run(bytes, limits)).unwrap_or(Err(ParseFailure::Panic))
}

/// Extract transfer-decoded bytes (without charset conversion) for a non-body ID.
/// The acquisition is admitted again; IDs from other acquisitions are not valid.
pub fn extract_attachment(
    bytes: &[u8],
    part_id: &str,
    limits: &Limits,
) -> Result<Vec<u8>, AttachmentError> {
    std::panic::catch_unwind(|| {
        let (message, _) = admitted(bytes, limits).map_err(|e| match e {
            AdmissionError::Parse(e) => AttachmentError::Parse(e),
            AdmissionError::Limits(_) => AttachmentError::LimitExceeded,
        })?;
        let mut nodes = Vec::new();
        collect(&message, 0, "1".into(), None, false, &mut nodes);
        let selected = select(&nodes, 0);
        let index = nodes
            .iter()
            .position(|n| n.id.as_str() == part_id)
            .ok_or(AttachmentError::UnknownPart)?;
        if selected.plain == Some(index) || selected.html == Some(index) {
            return Err(AttachmentError::BodyPart);
        }
        // Only the parts `render_input` lists are attachments.
        let node = &nodes[index];
        if matches!(node.part.body, PartType::Multipart(_)) || node.embedded {
            return Err(AttachmentError::UnknownPart);
        }
        decoded(node, limits).map(Cow::into_owned)
    })
    .unwrap_or(Err(AttachmentError::Parse(ParseFailure::Panic)))
}

pub(crate) enum AdmissionError {
    Parse(ParseFailure),
    Limits(Vec<ParseWarning>),
}

pub(crate) fn admitted<'a>(
    bytes: &'a [u8],
    limits: &Limits,
) -> Result<(Message<'a>, Vec<ParseWarning>), AdmissionError> {
    if bytes.len() > limits.max_message_bytes() {
        return Err(AdmissionError::Limits(vec![
            ParseWarning::MessageTooLarge {
                limit: limits.max_message_bytes() as u64,
            },
        ]));
    }
    let walked = walk::walk(bytes, limits, Vec::new());
    if !walked.admitted {
        return Err(AdmissionError::Limits(walked.warnings));
    }
    let message = MessageParser::default()
        .parse(bytes)
        .ok_or(AdmissionError::Parse(ParseFailure::Unparseable))?;
    let mut warnings = walked.warnings;
    if !verify(&message, limits, &mut warnings) {
        return Err(AdmissionError::Limits(warnings));
    }
    // The admission walk and upstream verification may flag the same damage.
    let malformed = warnings
        .iter()
        .filter_map(|warning| match warning {
            ParseWarning::Malformed { parts } => Some(*parts),
            _ => None,
        })
        .max()
        .unwrap_or(0);
    warnings.retain(|w| !matches!(w, ParseWarning::Malformed { .. }));
    if malformed > 0 {
        warnings.push(ParseWarning::Malformed { parts: malformed });
    }
    Ok((message, warnings))
}

pub(crate) struct Node<'a> {
    pub(crate) id: PartId,
    message: &'a Message<'a>,
    pub(crate) part: &'a MessagePart<'a>,
    pub(crate) children: Vec<usize>,
    related: Option<usize>,
    /// Inside an attached message: the attached message is the one
    /// attachment, not each of its parts.
    embedded: bool,
}

// Admission and verification have already bounded recursion to MAX_DEPTH.
pub(crate) fn collect<'a>(
    message: &'a Message<'a>,
    index: usize,
    id: String,
    related: Option<usize>,
    embedded: bool,
    nodes: &mut Vec<Node<'a>>,
) -> usize {
    let at = nodes.len();
    let part = &message.parts[index];
    nodes.push(Node {
        id: PartId(id.clone()),
        message,
        part,
        children: Vec::new(),
        related,
        embedded,
    });
    let children = match &part.body {
        PartType::Multipart(ids) => {
            let scope = if part.is_content_type("multipart", "related") {
                Some(at)
            } else {
                related
            };
            ids.iter()
                .enumerate()
                .map(|(i, &child)| {
                    collect(
                        message,
                        child as usize,
                        format!("{id}.{}", i + 1),
                        scope,
                        embedded,
                        nodes,
                    )
                })
                .collect()
        }
        // Embedded entities have IDs and descriptors, but never inherit CID scope.
        PartType::Message(inner) => {
            vec![collect(inner, 0, format!("{id}.1"), None, true, nodes)]
        }
        _ => Vec::new(),
    };
    nodes[at].children = children;
    at
}

#[derive(Default)]
struct Selection {
    plain: Option<usize>,
    html: Option<usize>,
}

fn select(nodes: &[Node<'_>], index: usize) -> Selection {
    let node = &nodes[index];
    let part = node.part;
    if part.is_encoding_problem
        || part.attachment_name().is_some()
        || part
            .content_disposition()
            .is_some_and(|d| d.is_attachment())
    {
        return Selection::default();
    }
    if let PartType::Multipart(_) = &part.body {
        if part.is_content_type("multipart", "related") {
            let start = part
                .content_type()
                .and_then(|c| c.attribute("start"))
                .and_then(cid);
            let root = start
                .and_then(|start| {
                    node.children
                        .iter()
                        .find(|&&i| part_cid(nodes[i].part) == Some(start))
                })
                .or_else(|| node.children.first());
            return root.map_or_else(Selection::default, |&i| select(nodes, i));
        }
        let alternative = part.is_content_type("multipart", "alternative");
        let mut result = Selection::default();
        for &child in &node.children {
            let candidate = select(nodes, child);
            if candidate.plain.is_some() && (alternative || result.plain.is_none()) {
                result.plain = candidate.plain;
            }
            if candidate.html.is_some() && (alternative || result.html.is_none()) {
                result.html = candidate.html;
            }
        }
        return result;
    }
    // Do not follow Message children, or treat arbitrary text/* as text/plain.
    Selection {
        plain: (matches!(part.body, PartType::Text(_))
            && (part.content_type().is_none() || part.is_content_type("text", "plain")))
        .then_some(index),
        html: (matches!(part.body, PartType::Html(_)) && part.is_content_type("text", "html"))
            .then_some(index),
    }
}

fn reader_run(bytes: &[u8], limits: &Limits) -> Result<RenderInput, ParseFailure> {
    let mut result = RenderInput::default();
    let (message, warnings) = match admitted(bytes, limits) {
        Ok(value) => value,
        Err(AdmissionError::Parse(e)) => return Err(e),
        Err(AdmissionError::Limits(warnings)) => {
            result.parse_warnings = warnings;
            result.warn(RenderWarningCode::PartialParse, None);
            return Ok(result);
        }
    };
    result.parse_warnings = warnings;
    if !result.parse_warnings.is_empty() {
        result.warn(RenderWarningCode::PartialParse, None);
    }
    let mut nodes = Vec::new();
    collect(&message, 0, "1".into(), None, false, &mut nodes);
    let selection = select(&nodes, 0);
    for (index, node) in nodes.iter().enumerate() {
        result.part_ids.push(node.id.clone());
        if selection.plain == Some(index) || selection.html == Some(index) {
            let (text, replaced) = decoded_body(node);
            // Admission measured upstream's text; this decoder can expand
            // it (a label upstream reads as UTF-8), so it is bounded again,
            // with admission's outcome.
            if text.len() as u64 > limits.max_decoded_part_bytes() {
                let mut refused = RenderInput {
                    parse_warnings: vec![ParseWarning::PartTooLarge {
                        limit: limits.max_decoded_part_bytes(),
                    }],
                    ..RenderInput::default()
                };
                refused.warn(RenderWarningCode::PartialParse, None);
                return Ok(refused);
            }
            if replaced {
                result.warn(RenderWarningCode::CharsetReplaced, Some(node.id.clone()));
            }
            let body = SelectedBody {
                part_id: node.id.clone(),
                text: if selection.plain == Some(index) {
                    plain_text(&text, node.part)
                } else {
                    text
                },
            };
            if selection.plain == Some(index) {
                result.plain = Some(body);
            } else {
                result.html = Some(body);
            }
            continue;
        }
        // Attachments are the leaf parts beside the bodies: containers
        // aren't, and an attached message counts once, as itself.
        if matches!(node.part.body, PartType::Multipart(_)) || node.embedded {
            continue;
        }
        let length = transfer_decoded(node).ok().map(|bytes| bytes.len() as u64);
        if length.is_none() {
            result.warn(RenderWarningCode::PartialParse, Some(node.id.clone()));
        }
        result.attachments.push(AttachmentDescriptor {
            part_id: node.id.clone(),
            display_name: display_name(node.part.attachment_name()),
            media_type: media_type(node.part),
            decoded_length: length,
        });
    }
    if selection.plain.is_none() && selection.html.is_none() {
        result.warn(RenderWarningCode::UnsupportedContent, None);
    }
    if let Some(scope) = selection.html.and_then(|i| nodes[i].related) {
        // Count IDs before materializing: even a body/unsupported part can make
        // a resource ambiguous. Never authorize an arbitrary duplicate winner.
        let mut ids = std::collections::BTreeMap::<&str, Vec<usize>>::new();
        for (i, node) in nodes.iter().enumerate() {
            if node.related == Some(scope)
                && let Some(id) = part_cid(node.part)
            {
                ids.entry(id).or_default().push(i);
            }
        }
        for (id, matches) in ids {
            if matches.len() != 1 {
                result.ambiguous_cids.push(id.to_owned());
                for i in matches {
                    result.warn(RenderWarningCode::AmbiguousCid, Some(nodes[i].id.clone()));
                }
                continue;
            }
            let index = matches[0];
            let node = &nodes[index];
            if Some(index) == selection.html
                || Some(index) == selection.plain
                || matches!(
                    node.part.body,
                    PartType::Multipart(_) | PartType::Message(_)
                )
            {
                continue;
            }
            if let Ok(bytes) = decoded(node, limits) {
                result.resources.push(RelatedResource {
                    content_id: id.to_owned(),
                    part_id: node.id.clone(),
                    media_type: media_type(node.part),
                    bytes: bytes.into_owned(),
                });
            }
        }
    }
    Ok(result)
}

fn cid(value: &str) -> Option<&str> {
    let value = value.trim();
    let value = value
        .strip_prefix('<')
        .and_then(|s| s.strip_suffix('>'))
        .unwrap_or(value);
    (!value.is_empty()
        && !value
            .chars()
            .any(|c| c.is_control() || c.is_whitespace() || matches!(c, '<' | '>')))
    .then_some(value)
}

fn part_cid<'a>(part: &'a MessagePart<'_>) -> Option<&'a str> {
    part.content_id().and_then(cid)
}

fn media_type(part: &MessagePart<'_>) -> String {
    part.content_type().map_or_else(
        || match part.body {
            PartType::Text(_) => "text/plain".into(),
            PartType::Message(_) => "message/rfc822".into(),
            _ => "application/octet-stream".into(),
        },
        |c| format!("{}/{}", c.ctype(), c.subtype().unwrap_or("octet-stream")).to_ascii_lowercase(),
    )
}

const MAX_DISPLAY_NAME_BYTES: usize = 255;

fn display_name(name: Option<&str>) -> String {
    let mut result = String::new();
    for c in name.unwrap_or("attachment").chars() {
        let c = if c.is_control() || disguises(c) || matches!(c, '/' | '\\' | ':') {
            '_'
        } else {
            c
        };
        if result.len() + c.len_utf8() > MAX_DISPLAY_NAME_BYTES {
            break;
        }
        result.push(c);
    }
    if result.trim().is_empty() || matches!(result.as_str(), "." | "..") {
        return "attachment".into();
    }
    result
}

/// Characters that can disguise a name without showing: bidi marks,
/// embeddings, overrides and isolates (`invoice\u{202e}gpj.exe` shows as
/// `invoiceexe.jpg`), zero-width characters, and line and paragraph
/// separators.
fn disguises(c: char) -> bool {
    matches!(
        c,
        '\u{061c}'
            | '\u{200b}'..='\u{200f}'
            | '\u{2028}'..='\u{202e}'
            | '\u{2060}'..='\u{2069}'
            | '\u{feff}'
    )
}

fn decoded<'a>(node: &'a Node<'_>, limits: &Limits) -> Result<Cow<'a, [u8]>, AttachmentError> {
    let bytes = transfer_decoded(node)?;
    if bytes.len() as u64 > limits.max_decoded_part_bytes() {
        return Err(AttachmentError::LimitExceeded);
    }
    Ok(bytes)
}

/// A body's text and whether decoding replaced bytes its charset can't
/// read. A charset encoding_rs knows is decoded here, by WHATWG rules
/// (a byte order mark decides UTF-16's endianness), so the text shown and
/// the warning come from one decoder and a literal U+FFFD is no loss. For
/// a charset only upstream decodes (UTF-7) its text stands and a U+FFFD
/// is the only sign; an unknown label is read as UTF-8, as upstream does.
pub(crate) fn decoded_body(node: &Node<'_>) -> (String, bool) {
    let upstream = || {
        let text = node.part.text_contents().unwrap_or_default().to_owned();
        let replaced = text.contains('\u{fffd}');
        (text, replaced)
    };
    let Ok(bytes) = transfer_decoded(node) else {
        return upstream();
    };
    let label = node
        .part
        .content_type()
        .and_then(|c| c.attribute("charset"))
        .unwrap_or("utf-8");
    if let Some(encoding) = encoding_rs::Encoding::for_label(label.as_bytes()) {
        let (text, _, replaced) = encoding.decode(&bytes);
        return (text.into_owned(), replaced);
    }
    if charset_decoder(label.as_bytes()).is_some() {
        return upstream();
    }
    match String::from_utf8(bytes.into_owned()) {
        Ok(text) => (text, false),
        Err(error) => (String::from_utf8_lossy(error.as_bytes()).into_owned(), true),
    }
}

fn transfer_decoded<'a>(node: &'a Node<'_>) -> Result<Cow<'a, [u8]>, AttachmentError> {
    if node.part.is_encoding_problem {
        return Err(AttachmentError::InvalidEncoding);
    }
    if matches!(
        node.part.body,
        PartType::Binary(_) | PartType::InlineBinary(_) | PartType::Message(_)
    ) {
        return Ok(Cow::Borrowed(node.part.contents()));
    }
    // Embedded unencoded messages retain offsets into the full backing buffer;
    // raw_message() would slice it and make those offsets invalid.
    let raw = node
        .message
        .raw_message
        .get(node.part.offset_body as usize..node.part.offset_end as usize)
        .ok_or(AttachmentError::InvalidEncoding)?;
    // Decode from the raw extent: text contents() has already applied a
    // charset, which would corrupt a saved non-UTF-8 text attachment.
    let mut stream = MessageStream::new(raw);
    let (end, bytes) = match node.part.encoding {
        Encoding::Base64 => stream.decode_base64_mime(b""),
        Encoding::QuotedPrintable => stream.decode_quoted_printable_mime(b""),
        Encoding::None => return Ok(Cow::Borrowed(raw)),
    };
    if end == usize::MAX {
        return Err(AttachmentError::InvalidEncoding);
    }
    Ok(bytes)
}

pub(crate) fn plain_text(text: &str, part: &MessagePart<'_>) -> String {
    let text = text.replace("\r\n", "\n");
    let ct = part.content_type();
    if !ct
        .and_then(|c| c.attribute("format"))
        .is_some_and(|s| s.eq_ignore_ascii_case("flowed"))
    {
        return text;
    }
    let delsp = ct
        .and_then(|c| c.attribute("delsp"))
        .is_some_and(|s| s.eq_ignore_ascii_case("yes"));
    flowed(&text, if delsp { DelSp::Delete } else { DelSp::Keep })
}

#[derive(Clone, Copy, PartialEq, Eq)]
enum DelSp {
    Keep,
    Delete,
}

fn flowed(text: &str, delsp: DelSp) -> String {
    let mut result = String::with_capacity(text.len());
    let mut pending: Option<usize> = None;
    for physical in text.split_inclusive('\n') {
        let newline = physical.ends_with('\n');
        let line = physical.strip_suffix('\n').unwrap_or(physical);
        let depth = line.bytes().take_while(|&b| b == b'>').count();
        let content = &line[depth..];
        let content = content.strip_prefix(' ').unwrap_or(content);
        let signature = content == "-- ";
        if let Some(previous) = pending
            && (previous != depth || signature)
        {
            result.push('\n');
        }
        let joining = pending == Some(depth) && !signature;
        if !joining {
            result.extend(std::iter::repeat_n('>', depth));
        }
        let soft = content.ends_with(' ') && !signature;
        let content = if soft && delsp == DelSp::Delete {
            &content[..content.len() - 1]
        } else {
            content
        };
        result.push_str(content);
        pending = if soft && newline {
            Some(depth)
        } else {
            if newline {
                result.push('\n');
            }
            None
        };
    }
    if pending.is_some() {
        result.push('\n');
    }
    result
}

#[cfg(test)]
mod tests;
