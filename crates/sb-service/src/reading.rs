//! Reading document DTOs and the reader's reading-document query
//! (spec/08 Reader: Reading view). The DTOs mirror `sb-mime`'s
//! semantic document field by field; nothing from below crosses into
//! L1 (spec/01 rule 3), so every node carries its source part as a
//! string and the conversion happens here.

use crate::{Core, CoreError};

/// The reading view's answer for one message: the semantic document, or
/// sender layout when the structure is unsupported.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadingSupport {
    Supported(ReadingDocument),
    Unsupported,
}

/// The semantic reading document: the part the reader displayed, the
/// model version that produced it, and its blocks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadingDocument {
    pub source_part_id: String,
    pub version: u32,
    pub blocks: Vec<ReadingBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadingBlock {
    Paragraph(ReadingParagraph),
    Heading(ReadingHeading),
    Quote(ReadingQuote),
    List(ReadingList),
    Preformatted(ReadingPreformatted),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadingParagraph {
    pub part_id: String,
    pub inlines: Vec<ReadingInline>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadingHeading {
    pub part_id: String,
    pub level: ReadingHeadingLevel,
    pub inlines: Vec<ReadingInline>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReadingHeadingLevel {
    H1,
    H2,
    H3,
    H4,
    H5,
    H6,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadingQuote {
    pub part_id: String,
    pub blocks: Vec<ReadingBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadingList {
    pub part_id: String,
    pub ordered: bool,
    pub start: i64,
    pub reversed: bool,
    pub items: Vec<ReadingListItem>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadingListItem {
    pub part_id: String,
    /// The HTML `value` override, or the explicit plain-text number.
    pub number: Option<i64>,
    pub blocks: Vec<ReadingBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadingPreformatted {
    pub part_id: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadingInline {
    Text(ReadingText),
    Emphasis(ReadingEmphasis),
    Link(ReadingLink),
    Image(ReadingImage),
    LineBreak(ReadingLineBreak),
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadingText {
    pub part_id: String,
    pub text: String,
}

/// `em`/`i` are emphasis, `strong`/`b` are strong.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadingEmphasis {
    pub part_id: String,
    pub strong: bool,
    pub inlines: Vec<ReadingInline>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadingLink {
    pub part_id: String,
    /// The sanitizer's canonical destination, never one rebuilt from the
    /// label.
    pub destination: String,
    pub inlines: Vec<ReadingInline>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadingImage {
    pub part_id: String,
    pub alt: String,
    /// Where the image comes from. The pipeline never fetches it.
    pub reference: ReadingImageReference,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReadingImageReference {
    /// A remote URL. Kept as a reference; remote content stays blocked.
    Remote { url: String },
    /// A `cid:` reference the reader resolves against the current
    /// message's resources.
    Attachment { content_id: String, part_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReadingLineBreak {
    pub part_id: String,
}

impl ReadingSupport {
    fn from_engine(value: sb_engine::DocumentSupport) -> Self {
        match value {
            sb_engine::DocumentSupport::Supported(document) => {
                ReadingSupport::Supported(ReadingDocument::from_engine(&document))
            }
            sb_engine::DocumentSupport::Unsupported => ReadingSupport::Unsupported,
        }
    }
}

impl ReadingDocument {
    fn from_engine(value: &sb_engine::MailDocument) -> Self {
        Self {
            source_part_id: value.source().as_str().to_owned(),
            version: value.version(),
            blocks: value
                .blocks()
                .iter()
                .map(ReadingBlock::from_engine)
                .collect(),
        }
    }
}

impl ReadingBlock {
    fn from_engine(value: &sb_engine::Block) -> Self {
        match value {
            sb_engine::Block::Paragraph(paragraph) => ReadingBlock::Paragraph(ReadingParagraph {
                part_id: paragraph.part().as_str().to_owned(),
                inlines: ReadingInline::from_engine_all(paragraph.inlines()),
            }),
            sb_engine::Block::Heading(heading) => ReadingBlock::Heading(ReadingHeading {
                part_id: heading.part().as_str().to_owned(),
                level: match heading.level() {
                    sb_engine::HeadingLevel::H1 => ReadingHeadingLevel::H1,
                    sb_engine::HeadingLevel::H2 => ReadingHeadingLevel::H2,
                    sb_engine::HeadingLevel::H3 => ReadingHeadingLevel::H3,
                    sb_engine::HeadingLevel::H4 => ReadingHeadingLevel::H4,
                    sb_engine::HeadingLevel::H5 => ReadingHeadingLevel::H5,
                    sb_engine::HeadingLevel::H6 => ReadingHeadingLevel::H6,
                },
                inlines: ReadingInline::from_engine_all(heading.inlines()),
            }),
            sb_engine::Block::Quote(quote) => ReadingBlock::Quote(ReadingQuote {
                part_id: quote.part().as_str().to_owned(),
                blocks: quote
                    .blocks()
                    .iter()
                    .map(ReadingBlock::from_engine)
                    .collect(),
            }),
            sb_engine::Block::List(list) => ReadingBlock::List(ReadingList {
                part_id: list.part().as_str().to_owned(),
                ordered: list.ordered(),
                start: list.start(),
                reversed: list.reversed(),
                items: list
                    .items()
                    .iter()
                    .map(|item| ReadingListItem {
                        part_id: item.part().as_str().to_owned(),
                        number: item.number(),
                        blocks: item
                            .blocks()
                            .iter()
                            .map(ReadingBlock::from_engine)
                            .collect(),
                    })
                    .collect(),
            }),
            sb_engine::Block::Preformatted(preformatted) => {
                ReadingBlock::Preformatted(ReadingPreformatted {
                    part_id: preformatted.part().as_str().to_owned(),
                    text: preformatted.text().to_owned(),
                })
            }
        }
    }
}

impl ReadingInline {
    fn from_engine_all(inlines: &[sb_engine::Inline]) -> Vec<Self> {
        inlines.iter().map(Self::from_engine).collect()
    }

    fn from_engine(value: &sb_engine::Inline) -> Self {
        match value {
            sb_engine::Inline::Text(text) => ReadingInline::Text(ReadingText {
                part_id: text.part().as_str().to_owned(),
                text: text.text().to_owned(),
            }),
            sb_engine::Inline::Emphasis(emphasis) => ReadingInline::Emphasis(ReadingEmphasis {
                part_id: emphasis.part().as_str().to_owned(),
                strong: emphasis.strong(),
                inlines: Self::from_engine_all(emphasis.inlines()),
            }),
            sb_engine::Inline::Link(link) => ReadingInline::Link(ReadingLink {
                part_id: link.part().as_str().to_owned(),
                destination: link.destination().to_owned(),
                inlines: Self::from_engine_all(link.inlines()),
            }),
            sb_engine::Inline::Image(image) => ReadingInline::Image(ReadingImage {
                part_id: image.part().as_str().to_owned(),
                alt: image.alt().to_owned(),
                reference: match image.reference() {
                    sb_engine::ImageReference::Remote { url } => {
                        ReadingImageReference::Remote { url: url.clone() }
                    }
                    sb_engine::ImageReference::Cid {
                        content_id,
                        part_id,
                    } => ReadingImageReference::Attachment {
                        content_id: content_id.clone(),
                        part_id: part_id.as_str().to_owned(),
                    },
                },
            }),
            sb_engine::Inline::LineBreak(line_break) => {
                ReadingInline::LineBreak(ReadingLineBreak {
                    part_id: line_break.part().as_str().to_owned(),
                })
            }
        }
    }
}

impl Core {
    /// The reading document for one message (spec/08 Reader: Reading
    /// view), over the same lookup as [`Core::open_body`]: unknown
    /// messages are `NotFound`, a missing body or a parse failure is
    /// `Unavailable`, and an unsupported structure is
    /// [`ReadingSupport::Unsupported`] (sender layout), never a lossy
    /// guess.
    pub async fn reading_document(&self, message_id: i64) -> Result<ReadingSupport, CoreError> {
        let value = self.engine().reading_document(message_id).await?;
        Ok(ReadingSupport::from_engine(value))
    }
}

#[cfg(test)]
mod tests;
