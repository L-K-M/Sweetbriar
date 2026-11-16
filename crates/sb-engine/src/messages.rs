//! Reader metadata queries on the runtime's blocking pool (spec/07).

use crate::{Engine, EngineError};
use sb_resources::{DocumentSupport, ReaderMessage, get_message, message_for_occurrence};

impl Engine {
    /// Read metadata in one short snapshot. Unknown messages are `NotFound`.
    pub async fn get_message(&self, message_id: i64) -> Result<ReaderMessage, EngineError> {
        self.read_through(move |resources| resources.read(|tx| get_message(tx, message_id)))
            .await
    }

    /// Resolve even a retained vanished occurrence. Unknown ids are `NotFound`.
    pub async fn message_for_occurrence(&self, occurrence_id: i64) -> Result<i64, EngineError> {
        self.read_through(move |resources| {
            resources.read(|tx| message_for_occurrence(tx, occurrence_id))
        })
        .await
    }

    /// The reading document for one message (spec/08 Reader: Reading
    /// view), over the same acquisition lookup as the body query:
    /// unknown messages are `NotFound`, a missing body or a parse
    /// failure is `Unavailable`, and unsupported structure is
    /// [`DocumentSupport::Unsupported`] (sender layout).
    pub async fn reading_document(&self, message_id: i64) -> Result<DocumentSupport, EngineError> {
        self.read_through(move |resources| resources.reading_document(message_id))
            .await
    }
}

#[cfg(test)]
mod tests;
