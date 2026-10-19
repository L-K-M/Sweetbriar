//! Service-owned body and attachment lease DTOs (spec/07).
use crate::{Core, CoreError};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyKind {
    SanitizedHtml,
    PlainText,
    Source,
}

impl BodyKind {
    fn into_engine(self) -> sb_engine::BodyKind {
        match self {
            Self::SanitizedHtml => sb_engine::BodyKind::SanitizedHtml,
            Self::PlainText => sb_engine::BodyKind::PlainText,
            Self::Source => sb_engine::BodyKind::Source,
        }
    }
    fn from_engine(kind: sb_engine::BodyKind) -> Self {
        match kind {
            sb_engine::BodyKind::SanitizedHtml => Self::SanitizedHtml,
            sb_engine::BodyKind::PlainText => Self::PlainText,
            sb_engine::BodyKind::Source => Self::Source,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
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
    fn from_engine(code: sb_engine::RenderWarningCode) -> Self {
        match code {
            sb_engine::RenderWarningCode::PartialParse => Self::PartialParse,
            sb_engine::RenderWarningCode::CharsetReplaced => Self::CharsetReplaced,
            sb_engine::RenderWarningCode::UnsupportedContent => Self::UnsupportedContent,
            sb_engine::RenderWarningCode::MissingCid => Self::MissingCid,
            sb_engine::RenderWarningCode::AmbiguousCid => Self::AmbiguousCid,
            sb_engine::RenderWarningCode::UnsafeUrl => Self::UnsafeUrl,
            sb_engine::RenderWarningCode::RenditionLimit => Self::RenditionLimit,
            sb_engine::RenderWarningCode::CountSaturated => Self::CountSaturated,
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RenderWarning {
    pub code: RenderWarningCode,
    pub part_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BodyLease {
    pub lease_id: String,
    pub kind: BodyKind,
    pub file_path: String,
    pub byte_length: u64,
    pub remote_content_blocked: u32,
    pub warnings: Vec<RenderWarning>,
    pub manifest_path: Option<String>,
}

/// Revokes leases without waiting on async calls or doing caller-thread I/O.
pub struct LeaseReleaser {
    inner: sb_engine::LeaseReleaser,
}

impl LeaseReleaser {
    pub fn release_lease(&self, lease_id: &str) {
        self.inner.release_lease(lease_id);
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttachmentLease {
    pub lease_id: String,
    pub file_path: String,
    pub byte_length: u64,
    pub part_id: String,
    pub display_name: String,
    pub media_type: String,
}

impl Core {
    pub fn lease_releaser(&self) -> LeaseReleaser {
        LeaseReleaser {
            inner: self.engine().lease_releaser(),
        }
    }

    pub async fn open_body(
        &self,
        message_id: i64,
        preferred: BodyKind,
    ) -> Result<BodyLease, CoreError> {
        let body = self
            .engine()
            .open_body(message_id, preferred.into_engine())
            .await?;
        Ok(BodyLease {
            lease_id: body.lease_id,
            kind: BodyKind::from_engine(body.kind),
            file_path: body.file_path,
            byte_length: body.byte_length,
            remote_content_blocked: body.remote_content_blocked,
            warnings: body
                .warnings
                .iter()
                .map(|warning| RenderWarning {
                    code: RenderWarningCode::from_engine(warning.code()),
                    part_id: warning.part_id().map(|id| id.as_str().to_owned()),
                })
                .collect(),
            manifest_path: body.manifest_path,
        })
    }

    pub async fn open_attachment(
        &self,
        message_id: i64,
        part_id: String,
    ) -> Result<AttachmentLease, CoreError> {
        let attachment = self.engine().open_attachment(message_id, part_id).await?;
        Ok(AttachmentLease {
            lease_id: attachment.lease_id,
            file_path: attachment.file_path,
            byte_length: attachment.byte_length,
            part_id: attachment.part_id,
            display_name: attachment.display_name,
            media_type: attachment.media_type,
        })
    }

    pub fn release_lease(&self, lease_id: &str) {
        self.engine().release_lease(lease_id);
    }
}

#[cfg(test)]
mod tests;

#[cfg(test)]
mod attachment_tests;
