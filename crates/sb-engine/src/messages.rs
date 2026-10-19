//! Reader metadata queries on the runtime's blocking pool (spec/07).

use crate::{Engine, EngineError};
use sb_resources::{ReaderMessage, get_message, message_for_occurrence};

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
}
