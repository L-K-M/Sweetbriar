//! Reader metadata, body and attachment lease DTOs and queries
//! (spec/07, spec/08), plus the local tag and note commands
//! (spec/07 Messages): records mirroring `sb-service`'s DTOs field
//! by field, converted one way across the boundary.

use crate::{CancelToken, Core, CoreError, Receipt};
use std::sync::Arc;

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum BodyAvailability {
    NotAcquired,
    Acquired,
    AcquisitionMissing,
    AcquisitionCorrupt,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ReaderAddress {
    pub name: Option<String>,
    pub address: Option<String>,
}

/// Parsed headers only. Unknown values stay unknown, even with a list snippet.
#[derive(Debug, Clone, Default, PartialEq, Eq, uniffi::Record)]
pub struct ReaderHeaders {
    pub from: Vec<ReaderAddress>,
    pub to: Vec<ReaderAddress>,
    pub cc: Vec<ReaderAddress>,
    pub date: Option<i64>,
    pub subject: Option<String>,
    pub message_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ReaderOccurrence {
    pub occurrence_id: i64,
    pub mailbox_id: i64,
    pub mailbox_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ReaderTag {
    pub tag_id: String,
    pub name: String,
    pub color_srgb: Option<String>,
}

/// A message selection (spec/07): up to 1,000 explicit occurrence
/// ids, or one open window's rows at the generation its caller holds.
/// Tags resolve to logical messages, not occurrences.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum MessageSelection {
    /// Explicit occurrence ids.
    OccurrenceIds(Vec<i64>),
    /// An open window's rows at `generation`; a stale generation is
    /// `ResetRequired`.
    Window { window_id: u64, generation: u64 },
}

impl MessageSelection {
    fn into_service(self) -> sb_service::MessageSelection {
        match self {
            Self::OccurrenceIds(ids) => sb_service::MessageSelection::OccurrenceIds(ids),
            Self::Window {
                window_id,
                generation,
            } => sb_service::MessageSelection::Window {
                window_id,
                generation,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ReaderMessage {
    pub message_id: i64,
    pub headers: ReaderHeaders,
    pub account_id: String,
    pub account_display_name: String,
    pub occurrences: Vec<ReaderOccurrence>,
    pub thread_id: Option<i64>,
    pub tags: Vec<ReaderTag>,
    pub note: Option<String>,
    pub body_availability: BodyAvailability,
}

impl ReaderMessage {
    fn from_service(value: sb_service::ReaderMessage) -> Self {
        let address = |a: sb_service::ReaderAddress| ReaderAddress {
            name: a.name,
            address: a.address,
        };
        Self {
            message_id: value.message_id,
            headers: ReaderHeaders {
                from: value.headers.from.into_iter().map(address).collect(),
                to: value.headers.to.into_iter().map(address).collect(),
                cc: value.headers.cc.into_iter().map(address).collect(),
                date: value.headers.date,
                subject: value.headers.subject,
                message_id: value.headers.message_id,
            },
            account_id: value.account_id,
            account_display_name: value.account_display_name,
            occurrences: value
                .occurrences
                .into_iter()
                .map(|o| ReaderOccurrence {
                    occurrence_id: o.occurrence_id,
                    mailbox_id: o.mailbox_id,
                    mailbox_name: o.mailbox_name,
                })
                .collect(),
            thread_id: value.thread_id,
            tags: value
                .tags
                .into_iter()
                .map(|t| ReaderTag {
                    tag_id: t.tag_id,
                    name: t.name,
                    color_srgb: t.color_srgb,
                })
                .collect(),
            note: value.note,
            body_availability: match value.body_availability {
                sb_service::BodyAvailability::NotAcquired => BodyAvailability::NotAcquired,
                sb_service::BodyAvailability::Acquired => BodyAvailability::Acquired,
                sb_service::BodyAvailability::AcquisitionMissing => {
                    BodyAvailability::AcquisitionMissing
                }
                sb_service::BodyAvailability::AcquisitionCorrupt => {
                    BodyAvailability::AcquisitionCorrupt
                }
            },
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum BodyKind {
    SanitizedHtml,
    PlainText,
    Source,
}

impl BodyKind {
    fn into_service(self) -> sb_service::BodyKind {
        match self {
            Self::SanitizedHtml => sb_service::BodyKind::SanitizedHtml,
            Self::PlainText => sb_service::BodyKind::PlainText,
            Self::Source => sb_service::BodyKind::Source,
        }
    }
    fn from_service(kind: sb_service::BodyKind) -> Self {
        match kind {
            sb_service::BodyKind::SanitizedHtml => Self::SanitizedHtml,
            sb_service::BodyKind::PlainText => Self::PlainText,
            sb_service::BodyKind::Source => Self::Source,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum RenderWarningCode {
    PartialParse,
    CharsetReplaced,
    UnsupportedContent,
    MissingCid,
    AmbiguousCid,
    UnsafeUrl,
    RenditionLimit,
    CountSaturated,
}

impl RenderWarningCode {
    fn from_service(code: sb_service::RenderWarningCode) -> Self {
        match code {
            sb_service::RenderWarningCode::PartialParse => Self::PartialParse,
            sb_service::RenderWarningCode::CharsetReplaced => Self::CharsetReplaced,
            sb_service::RenderWarningCode::UnsupportedContent => Self::UnsupportedContent,
            sb_service::RenderWarningCode::MissingCid => Self::MissingCid,
            sb_service::RenderWarningCode::AmbiguousCid => Self::AmbiguousCid,
            sb_service::RenderWarningCode::UnsafeUrl => Self::UnsafeUrl,
            sb_service::RenderWarningCode::RenditionLimit => Self::RenditionLimit,
            sb_service::RenderWarningCode::CountSaturated => Self::CountSaturated,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct RenderWarning {
    pub code: RenderWarningCode,
    pub part_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct BodyLease {
    pub lease_id: String,
    pub kind: BodyKind,
    pub file_path: String,
    pub byte_length: u64,
    pub remote_content_blocked: u32,
    pub warnings: Vec<RenderWarning>,
    pub manifest_path: Option<String>,
}

impl BodyLease {
    fn from_service(value: sb_service::BodyLease) -> Self {
        Self {
            lease_id: value.lease_id,
            kind: BodyKind::from_service(value.kind),
            file_path: value.file_path,
            byte_length: value.byte_length,
            remote_content_blocked: value.remote_content_blocked,
            warnings: value
                .warnings
                .into_iter()
                .map(|warning| RenderWarning {
                    code: RenderWarningCode::from_service(warning.code),
                    part_id: warning.part_id,
                })
                .collect(),
            manifest_path: value.manifest_path,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct AttachmentLease {
    pub lease_id: String,
    pub file_path: String,
    pub byte_length: u64,
    pub part_id: String,
    pub display_name: String,
    pub media_type: String,
}

impl AttachmentLease {
    fn from_service(value: sb_service::AttachmentLease) -> Self {
        Self {
            lease_id: value.lease_id,
            file_path: value.file_path,
            byte_length: value.byte_length,
            part_id: value.part_id,
            display_name: value.display_name,
            media_type: value.media_type,
        }
    }
}

/// The reading view's answer for one message: the semantic document, or
/// sender layout when the structure is unsupported (spec/08 Reader:
/// Reading view). Mirrors the service DTO field by field, with nested
/// blocks and inlines as recursive enums, as the model nests them.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum ReadingSupport {
    Supported(ReadingDocument),
    Unsupported,
}

/// The semantic reading document: the displayed part, the model version
/// that produced it, and its blocks.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ReadingDocument {
    pub source_part_id: String,
    pub version: u32,
    pub blocks: Vec<ReadingBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum ReadingBlock {
    Paragraph(ReadingParagraph),
    Heading(ReadingHeading),
    Quote(ReadingQuote),
    List(ReadingList),
    Preformatted(ReadingPreformatted),
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ReadingParagraph {
    pub part_id: String,
    pub inlines: Vec<ReadingInline>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ReadingHeading {
    pub part_id: String,
    pub level: ReadingHeadingLevel,
    pub inlines: Vec<ReadingInline>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum ReadingHeadingLevel {
    H1,
    H2,
    H3,
    H4,
    H5,
    H6,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ReadingQuote {
    pub part_id: String,
    pub blocks: Vec<ReadingBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ReadingList {
    pub part_id: String,
    pub ordered: bool,
    pub start: i64,
    pub reversed: bool,
    pub items: Vec<ReadingListItem>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ReadingListItem {
    pub part_id: String,
    /// The HTML `value` override, or the explicit plain-text number.
    pub number: Option<i64>,
    pub blocks: Vec<ReadingBlock>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ReadingPreformatted {
    pub part_id: String,
    pub text: String,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum ReadingInline {
    Text(ReadingText),
    Emphasis(ReadingEmphasis),
    Link(ReadingLink),
    Image(ReadingImage),
    LineBreak(ReadingLineBreak),
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ReadingText {
    pub part_id: String,
    pub text: String,
}

/// `em`/`i` are emphasis, `strong`/`b` are strong.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ReadingEmphasis {
    pub part_id: String,
    pub strong: bool,
    pub inlines: Vec<ReadingInline>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ReadingLink {
    pub part_id: String,
    /// The sanitizer's canonical destination, never one rebuilt from the
    /// label.
    pub destination: String,
    pub inlines: Vec<ReadingInline>,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ReadingImage {
    pub part_id: String,
    pub alt: String,
    /// Where the image comes from. The pipeline never fetches it.
    pub reference: ReadingImageReference,
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Enum)]
pub enum ReadingImageReference {
    /// A remote URL. Kept as a reference; remote content stays blocked.
    Remote { url: String },
    /// A `cid:` reference the reader resolves against the current
    /// message's resources.
    Attachment { content_id: String, part_id: String },
}

#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct ReadingLineBreak {
    pub part_id: String,
}

impl ReadingSupport {
    fn from_service(value: sb_service::ReadingSupport) -> Self {
        match value {
            sb_service::ReadingSupport::Supported(document) => {
                Self::Supported(ReadingDocument::from_service(document))
            }
            sb_service::ReadingSupport::Unsupported => Self::Unsupported,
        }
    }
}

impl ReadingDocument {
    fn from_service(value: sb_service::ReadingDocument) -> Self {
        Self {
            source_part_id: value.source_part_id,
            version: value.version,
            blocks: value
                .blocks
                .into_iter()
                .map(ReadingBlock::from_service)
                .collect(),
        }
    }
}

impl ReadingBlock {
    fn from_service(value: sb_service::ReadingBlock) -> Self {
        match value {
            sb_service::ReadingBlock::Paragraph(paragraph) => {
                Self::Paragraph(ReadingParagraph::from_service(paragraph))
            }
            sb_service::ReadingBlock::Heading(heading) => {
                Self::Heading(ReadingHeading::from_service(heading))
            }
            sb_service::ReadingBlock::Quote(quote) => {
                Self::Quote(ReadingQuote::from_service(quote))
            }
            sb_service::ReadingBlock::List(list) => Self::List(ReadingList::from_service(list)),
            sb_service::ReadingBlock::Preformatted(preformatted) => {
                Self::Preformatted(ReadingPreformatted::from_service(preformatted))
            }
        }
    }
}

impl ReadingParagraph {
    fn from_service(value: sb_service::ReadingParagraph) -> Self {
        Self {
            part_id: value.part_id,
            inlines: value
                .inlines
                .into_iter()
                .map(ReadingInline::from_service)
                .collect(),
        }
    }
}

impl ReadingHeading {
    fn from_service(value: sb_service::ReadingHeading) -> Self {
        Self {
            part_id: value.part_id,
            level: ReadingHeadingLevel::from_service(value.level),
            inlines: value
                .inlines
                .into_iter()
                .map(ReadingInline::from_service)
                .collect(),
        }
    }
}

impl ReadingHeadingLevel {
    fn from_service(level: sb_service::ReadingHeadingLevel) -> Self {
        match level {
            sb_service::ReadingHeadingLevel::H1 => Self::H1,
            sb_service::ReadingHeadingLevel::H2 => Self::H2,
            sb_service::ReadingHeadingLevel::H3 => Self::H3,
            sb_service::ReadingHeadingLevel::H4 => Self::H4,
            sb_service::ReadingHeadingLevel::H5 => Self::H5,
            sb_service::ReadingHeadingLevel::H6 => Self::H6,
        }
    }
}

impl ReadingQuote {
    fn from_service(value: sb_service::ReadingQuote) -> Self {
        Self {
            part_id: value.part_id,
            blocks: value
                .blocks
                .into_iter()
                .map(ReadingBlock::from_service)
                .collect(),
        }
    }
}

impl ReadingList {
    fn from_service(value: sb_service::ReadingList) -> Self {
        Self {
            part_id: value.part_id,
            ordered: value.ordered,
            start: value.start,
            reversed: value.reversed,
            items: value
                .items
                .into_iter()
                .map(ReadingListItem::from_service)
                .collect(),
        }
    }
}

impl ReadingListItem {
    fn from_service(value: sb_service::ReadingListItem) -> Self {
        Self {
            part_id: value.part_id,
            number: value.number,
            blocks: value
                .blocks
                .into_iter()
                .map(ReadingBlock::from_service)
                .collect(),
        }
    }
}

impl ReadingPreformatted {
    fn from_service(value: sb_service::ReadingPreformatted) -> Self {
        Self {
            part_id: value.part_id,
            text: value.text,
        }
    }
}

impl ReadingInline {
    fn from_service(value: sb_service::ReadingInline) -> Self {
        match value {
            sb_service::ReadingInline::Text(text) => Self::Text(ReadingText::from_service(text)),
            sb_service::ReadingInline::Emphasis(emphasis) => {
                Self::Emphasis(ReadingEmphasis::from_service(emphasis))
            }
            sb_service::ReadingInline::Link(link) => Self::Link(ReadingLink::from_service(link)),
            sb_service::ReadingInline::Image(image) => {
                Self::Image(ReadingImage::from_service(image))
            }
            sb_service::ReadingInline::LineBreak(line_break) => {
                Self::LineBreak(ReadingLineBreak::from_service(line_break))
            }
        }
    }
}

impl ReadingText {
    fn from_service(value: sb_service::ReadingText) -> Self {
        Self {
            part_id: value.part_id,
            text: value.text,
        }
    }
}

impl ReadingEmphasis {
    fn from_service(value: sb_service::ReadingEmphasis) -> Self {
        Self {
            part_id: value.part_id,
            strong: value.strong,
            inlines: value
                .inlines
                .into_iter()
                .map(ReadingInline::from_service)
                .collect(),
        }
    }
}

impl ReadingLink {
    fn from_service(value: sb_service::ReadingLink) -> Self {
        Self {
            part_id: value.part_id,
            destination: value.destination,
            inlines: value
                .inlines
                .into_iter()
                .map(ReadingInline::from_service)
                .collect(),
        }
    }
}

impl ReadingImage {
    fn from_service(value: sb_service::ReadingImage) -> Self {
        Self {
            part_id: value.part_id,
            alt: value.alt,
            reference: ReadingImageReference::from_service(value.reference),
        }
    }
}

impl ReadingImageReference {
    fn from_service(value: sb_service::ReadingImageReference) -> Self {
        match value {
            sb_service::ReadingImageReference::Remote { url } => Self::Remote { url },
            sb_service::ReadingImageReference::Attachment {
                content_id,
                part_id,
            } => Self::Attachment {
                content_id,
                part_id,
            },
        }
    }
}

impl ReadingLineBreak {
    fn from_service(value: sb_service::ReadingLineBreak) -> Self {
        Self {
            part_id: value.part_id,
        }
    }
}

#[uniffi::export]
impl Core {
    /// Open a decoded attachment; cancellation drops the pending lease guard.
    pub async fn open_attachment(
        &self,
        token: Arc<CancelToken>,
        message_id: i64,
        part_id: String,
    ) -> Result<AttachmentLease, CoreError> {
        let value = self
            .query(&token, async |core| {
                core.open_attachment(message_id, part_id).await
            })
            .await?;
        Ok(AttachmentLease::from_service(value))
    }

    /// Open a rendition; cancellation drops the service's pending lease guard.
    pub async fn open_body(
        &self,
        token: Arc<CancelToken>,
        message_id: i64,
        preferred: BodyKind,
    ) -> Result<BodyLease, CoreError> {
        let value = self
            .query(&token, async |core| {
                core.open_body(message_id, preferred.into_service()).await
            })
            .await?;
        Ok(BodyLease::from_service(value))
    }

    /// Revoke in memory now; the engine removes files on its cleanup worker.
    pub fn release_lease(&self, lease_id: String) {
        self.lease_releaser.release_lease(&lease_id);
    }

    /// Reader metadata; unknown messages are `NotFound`.
    pub async fn get_message(
        &self,
        token: Arc<CancelToken>,
        message_id: i64,
    ) -> Result<ReaderMessage, CoreError> {
        let value = self
            .query(&token, async |core| core.get_message(message_id).await)
            .await?;
        Ok(ReaderMessage::from_service(value))
    }
    /// Resolve the selected occurrence; unknown occurrences are `NotFound`.
    pub async fn message_for_occurrence(
        &self,
        token: Arc<CancelToken>,
        occurrence_id: i64,
    ) -> Result<i64, CoreError> {
        let value = self
            .query(&token, async |core| {
                core.message_for_occurrence(occurrence_id).await
            })
            .await?;
        Ok(value)
    }

    /// `add_tag(selection, tag)` (spec/07 Messages): attach the named
    /// tag, creating the definition when the name is new. A repeated
    /// key answers its stored receipt. Like every command it honours
    /// the token only until it holds the core.
    pub async fn add_tag(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        selection: MessageSelection,
        tag: String,
    ) -> Result<Receipt, CoreError> {
        let receipt = self
            .command(&token, async move |core| {
                core.add_tag(&idempotency_key, selection.into_service(), &tag)
                    .await
            })
            .await?;
        Ok(Receipt::from_service(&receipt))
    }

    /// `remove_tag(selection, tag)` (spec/07 Messages): detach the
    /// named tag; the definition stays. A repeated key answers its
    /// stored receipt.
    pub async fn remove_tag(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        selection: MessageSelection,
        tag: String,
    ) -> Result<Receipt, CoreError> {
        let receipt = self
            .command(&token, async move |core| {
                core.remove_tag(&idempotency_key, selection.into_service(), &tag)
                    .await
            })
            .await?;
        Ok(Receipt::from_service(&receipt))
    }

    /// `set_note(message_id, text)` (spec/07 Messages): write Markdown,
    /// one note per message; empty text removes it. A repeated key
    /// answers its stored receipt.
    pub async fn set_note(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        message_id: i64,
        text: String,
    ) -> Result<Receipt, CoreError> {
        let receipt = self
            .command(&token, async move |core| {
                core.set_note(&idempotency_key, message_id, &text).await
            })
            .await?;
        Ok(Receipt::from_service(&receipt))
    }

    /// The reading document for one message (spec/08 Reader: Reading
    /// view), over the same lookup as [`Core::open_body`]: unknown
    /// messages are `NotFound`, a missing body or a parse failure is
    /// `Unavailable`, and an unsupported structure answers
    /// [`ReadingSupport::Unsupported`] (sender layout), never a lossy
    /// guess.
    pub async fn reading_document(
        &self,
        token: Arc<CancelToken>,
        message_id: i64,
    ) -> Result<ReadingSupport, CoreError> {
        let value = self
            .query(&token, async |core| core.reading_document(message_id).await)
            .await?;
        Ok(ReadingSupport::from_service(value))
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod body_tests;

#[cfg(test)]
mod attachment_tests;

#[cfg(test)]
mod reading_tests;
