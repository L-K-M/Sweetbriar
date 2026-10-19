//! Reader metadata, body and attachment lease DTOs and queries (spec/07, spec/08).

use crate::{CancelToken, Core, CoreError};
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
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod body_tests;

#[cfg(test)]
mod attachment_tests;
