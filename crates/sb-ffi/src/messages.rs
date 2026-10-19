//! Reader metadata DTOs and short read-only queries (spec/07, spec/08).

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

#[uniffi::export]
impl Core {
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
