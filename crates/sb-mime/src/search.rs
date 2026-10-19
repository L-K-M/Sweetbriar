//! Reader-independent, decoded MIME inputs for search (spec/06).

use mail_parser::{HeaderName, MimeHeaders, PartType};

use crate::extract::{mailboxes, text_of};
use crate::reader::{AdmissionError, collect, decoded_body, plain_text};
use crate::{Limits, ParseFailure, ParseWarning, PartId};

/// The source syntax of a search body candidate.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SearchBodyKind {
    Plain,
    Html,
}

/// One inline candidate, without reader selection or sanitization.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchBody {
    part_id: PartId,
    kind: SearchBodyKind,
    text: String,
    charset_replaced: bool,
}

impl SearchBody {
    pub fn part_id(&self) -> &PartId {
        &self.part_id
    }
    pub fn kind(&self) -> SearchBodyKind {
        self.kind
    }
    /// Flowed-decoded plain text, or raw decoded HTML, never safe to display.
    /// HTML text-node extraction and quote classification belong to M1.6c.
    pub fn text(&self) -> &str {
        &self.text
    }
    /// Whether charset decoding replaced an unreadable byte sequence.
    pub fn charset_replaced(&self) -> bool {
        self.charset_replaced
    }
}

/// An actual decoded MIME filename from a named attachment or inline part,
/// without reader display-name rewriting. Attached subtrees remain opaque.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchAttachmentName {
    part_id: PartId,
    name: String,
}

impl SearchAttachmentName {
    pub fn part_id(&self) -> &PartId {
        &self.part_id
    }
    /// Search text only, never a filesystem path. Unnamed parts add no name.
    pub fn name(&self) -> &str {
        &self.name
    }
}

/// Owned inputs for the later search extractor. Bodies and names each retain
/// MIME order. Headers cover Subject, From, To and Cc; missing fields are empty.
/// Search column caps, paragraph deduplication and classification are applied
/// by M1.6c, not at this boundary.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SearchInputs {
    subject: String,
    from_text: String,
    to_text: String,
    cc_text: String,
    bodies: Vec<SearchBody>,
    attachment_names: Vec<SearchAttachmentName>,
    parse_warnings: Vec<ParseWarning>,
}

impl SearchInputs {
    pub fn subject(&self) -> &str {
        &self.subject
    }
    pub fn from_text(&self) -> &str {
        &self.from_text
    }
    pub fn to_text(&self) -> &str {
        &self.to_text
    }
    pub fn cc_text(&self) -> &str {
        &self.cc_text
    }
    pub fn bodies(&self) -> &[SearchBody] {
        &self.bodies
    }
    pub fn attachment_names(&self) -> &[SearchAttachmentName] {
        &self.attachment_names
    }
    /// Nonempty means input may be missing. A refused admission returns empty
    /// inputs with these warnings, as render_input does, rather than a failure.
    /// The later extractor must propagate this to partial coverage (spec/06).
    pub fn parse_warnings(&self) -> &[ParseWarning] {
        &self.parse_warnings
    }
}

/// Decode search sources under the same admission/verification limits as
/// [`crate::render_input`], independently of reader bodies and renditions.
/// Attached messages and attached containers are opaque: only their own names
/// are retained. No attachment contents or external resources are extracted.
/// Parser failures are errors; unwinds across the entire pipeline are contained.
pub fn search_inputs(bytes: &[u8], limits: &Limits) -> Result<SearchInputs, ParseFailure> {
    std::panic::catch_unwind(|| run(bytes, limits)).unwrap_or(Err(ParseFailure::Panic))
}

fn refused(warnings: Vec<ParseWarning>) -> SearchInputs {
    SearchInputs {
        parse_warnings: warnings,
        ..SearchInputs::default()
    }
}

fn run(bytes: &[u8], limits: &Limits) -> Result<SearchInputs, ParseFailure> {
    let (message, warnings) = match crate::reader::admitted(bytes, limits) {
        Ok(value) => value,
        Err(AdmissionError::Parse(error)) => return Err(error),
        Err(AdmissionError::Limits(warnings)) => return Ok(refused(warnings)),
    };
    let headers = &message.parts[0].headers;
    let address_text = |name| {
        mailboxes(headers, name)
            .iter()
            .flat_map(|mailbox| [mailbox.name(), mailbox.address()])
            .flatten()
            .collect::<Vec<_>>()
            .join("\n")
    };
    let mut result = SearchInputs {
        subject: text_of(headers, HeaderName::Subject).unwrap_or_default(),
        from_text: address_text(HeaderName::From),
        to_text: address_text(HeaderName::To),
        cc_text: address_text(HeaderName::Cc),
        parse_warnings: warnings,
        ..SearchInputs::default()
    };
    // Share identity and decoding mechanics, never the reader's selection policy.
    let mut nodes = Vec::new();
    collect(&message, 0, "1".into(), None, false, &mut nodes);
    let mut stack = vec![0];
    while let Some(index) = stack.pop() {
        let node = &nodes[index];
        let part = node.part;
        let name = part.attachment_name();
        let attached = name.is_some()
            || part
                .content_disposition()
                .is_some_and(|d| d.is_attachment());
        if let Some(name) = name {
            result.attachment_names.push(SearchAttachmentName {
                part_id: node.id.clone(),
                name: name.to_owned(),
            });
        }
        if attached || matches!(part.body, PartType::Message(_)) {
            continue;
        }
        if matches!(part.body, PartType::Multipart(_)) {
            stack.extend(node.children.iter().rev().copied());
            continue;
        }
        let kind = if part.is_content_type("text", "html") {
            SearchBodyKind::Html
        } else if part.content_type().is_none() || part.is_content_type("text", "plain") {
            SearchBodyKind::Plain
        } else {
            continue;
        };
        if part.is_encoding_problem {
            continue; // Admission/verification already recorded the damage.
        }
        if !matches!(part.body, PartType::Text(_) | PartType::Html(_)) {
            continue;
        }
        let (text, charset_replaced) = decoded_body(node);
        // The shared decoder can expand beyond upstream's materialized size.
        if text.len() as u64 > limits.max_decoded_part_bytes() {
            result.parse_warnings.push(ParseWarning::PartTooLarge {
                limit: limits.max_decoded_part_bytes(),
            });
            return Ok(refused(result.parse_warnings));
        }
        let text = match kind {
            SearchBodyKind::Plain => plain_text(&text, part),
            SearchBodyKind::Html => text,
        };
        result.bodies.push(SearchBody {
            part_id: node.id.clone(),
            kind,
            text,
            charset_replaced,
        });
    }
    Ok(result)
}

#[cfg(test)]
mod tests;
