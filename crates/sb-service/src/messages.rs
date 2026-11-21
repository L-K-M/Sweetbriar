//! Reader metadata DTOs, tag/note commands and queries (spec/07).

use crate::{Core, CoreError, Receipt};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyAvailability {
    NotAcquired,
    Acquired,
    AcquisitionMissing,
    AcquisitionCorrupt,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReaderAddress {
    pub name: Option<String>,
    pub address: Option<String>,
}

/// Parsed headers only. Unknown values stay unknown, even with a list snippet.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ReaderHeaders {
    pub from: Vec<ReaderAddress>,
    pub to: Vec<ReaderAddress>,
    pub cc: Vec<ReaderAddress>,
    pub date: Option<i64>,
    pub subject: Option<String>,
    pub message_id: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReaderOccurrence {
    pub occurrence_id: i64,
    pub mailbox_id: i64,
    pub mailbox_name: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReaderTag {
    pub tag_id: String,
    pub name: String,
    pub color_srgb: Option<String>,
}

/// A message selection (spec/07): up to 1,000 explicit occurrence
/// ids, or one open window's rows at the generation its caller holds.
/// Tags resolve to logical messages, not occurrences.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageSelection {
    /// Explicit occurrence ids.
    OccurrenceIds(Vec<i64>),
    /// An open window's rows at `generation`; a stale generation is
    /// `ResetRequired`.
    Window { window_id: u64, generation: u64 },
}

impl MessageSelection {
    pub(crate) fn into_engine(self) -> sb_engine::MessageSelection {
        match self {
            Self::OccurrenceIds(ids) => sb_engine::MessageSelection::OccurrenceIds(ids),
            Self::Window {
                window_id,
                generation,
            } => sb_engine::MessageSelection::Window {
                window_id,
                generation,
            },
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
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
    fn from_engine(value: sb_engine::ReaderMessage) -> Self {
        let address = |a: sb_engine::ReaderAddress| ReaderAddress {
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
                sb_engine::BodyAvailability::NotAcquired => BodyAvailability::NotAcquired,
                sb_engine::BodyAvailability::Acquired => BodyAvailability::Acquired,
                sb_engine::BodyAvailability::AcquisitionMissing => {
                    BodyAvailability::AcquisitionMissing
                }
                sb_engine::BodyAvailability::AcquisitionCorrupt => {
                    BodyAvailability::AcquisitionCorrupt
                }
            },
        }
    }
}

impl Core {
    /// Reader metadata; unknown messages are `NotFound`.
    pub async fn get_message(&self, message_id: i64) -> Result<ReaderMessage, CoreError> {
        let value = self.engine().get_message(message_id).await?;
        Ok(ReaderMessage::from_engine(value))
    }
    /// Resolve the selected occurrence; unknown occurrences are `NotFound`.
    pub async fn message_for_occurrence(&self, occurrence_id: i64) -> Result<i64, CoreError> {
        Ok(self.engine().message_for_occurrence(occurrence_id).await?)
    }

    /// `add_tag(selection, tag)` (spec/07 Messages): attach the named
    /// tag, creating the definition when the name is new.
    pub async fn add_tag(
        &self,
        idempotency_key: &str,
        selection: MessageSelection,
        tag: &str,
    ) -> Result<Receipt, CoreError> {
        let receipt = self
            .engine()
            .add_tag(idempotency_key, selection.into_engine(), tag)
            .await?;
        Ok(Receipt::from_engine(&receipt))
    }

    /// `remove_tag(selection, tag)` (spec/07 Messages): detach the
    /// named tag; the definition stays.
    pub async fn remove_tag(
        &self,
        idempotency_key: &str,
        selection: MessageSelection,
        tag: &str,
    ) -> Result<Receipt, CoreError> {
        let receipt = self
            .engine()
            .remove_tag(idempotency_key, selection.into_engine(), tag)
            .await?;
        Ok(Receipt::from_engine(&receipt))
    }

    /// `set_note(message_id, text)` (spec/07 Messages): one note per
    /// message; empty text removes it.
    pub async fn set_note(
        &self,
        idempotency_key: &str,
        message_id: i64,
        text: &str,
    ) -> Result<Receipt, CoreError> {
        let receipt = self
            .engine()
            .set_note(idempotency_key, message_id, text)
            .await?;
        Ok(Receipt::from_engine(&receipt))
    }
}
