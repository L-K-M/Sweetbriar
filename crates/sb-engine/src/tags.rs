//! Local tags and notes (spec/02, spec/07): `add_tag`, `remove_tag`,
//! `set_note`. Local-only writes; nothing reaches a server.

use sb_resources::{
    Receipt, WritePriority, add_tag, messages_for_occurrences, remove_tag, run_once, set_note,
};

use crate::engine::validate_idempotency_key;
use crate::{Engine, EngineError, WindowId};

const ADD_TAG: &str = "add_tag";
const REMOVE_TAG: &str = "remove_tag";
const SET_NOTE: &str = "set_note";

/// The most occurrence ids an explicit selection carries (spec/07).
const MAX_SELECTION_IDS: usize = 1_000;

/// The stored byte bound on a tag's name (spec/02).
const MAX_NAME_BYTES: usize = 128;

/// A selection (spec/07): up to 1,000 occurrence ids, or one open
/// window's rows at its held generation. Tags resolve to logical messages.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum MessageSelection {
    /// Explicit occurrence ids.
    OccurrenceIds(Vec<i64>),
    /// An open window's rows at `generation`; a stale generation is
    /// `ResetRequired`.
    Window { window_id: u64, generation: u64 },
}

impl Engine {
    /// `add_tag(selection, tag)` (spec/07): attach the named tag,
    /// creating the definition when the name is new. A repeated key
    /// replays without writing.
    pub async fn add_tag(
        &self,
        idempotency_key: &str,
        selection: MessageSelection,
        tag: &str,
    ) -> Result<Receipt, EngineError> {
        validate_idempotency_key(idempotency_key)?;
        valid_name(tag)?;
        let messages = self.selection_messages(&selection).await?;
        let new_tag_id = self.resources().new_uuid_string();
        let now = self.resources().now();
        let (key, name) = (idempotency_key.to_owned(), tag.to_owned());
        self.command(move |worker| async move {
            worker
                .resources()
                .write(WritePriority::User, move |tx| {
                    run_once(tx, &key, ADD_TAG, now, |tx| {
                        let (tag_id, added) = add_tag(tx, &new_tag_id, &name, &messages, now)?;
                        Ok(encode_count("tag_id", &tag_id, "added", added))
                    })
                })
                .await
                .map_err(EngineError::from)
        })
        .await
    }

    /// `remove_tag(selection, tag)` (spec/07): detach the named tag,
    /// keeping the definition (spec/02 rule 8).
    pub async fn remove_tag(
        &self,
        idempotency_key: &str,
        selection: MessageSelection,
        tag: &str,
    ) -> Result<Receipt, EngineError> {
        validate_idempotency_key(idempotency_key)?;
        valid_name(tag)?;
        let messages = self.selection_messages(&selection).await?;
        let now = self.resources().now();
        let (key, name) = (idempotency_key.to_owned(), tag.to_owned());
        self.command(move |worker| async move {
            worker
                .resources()
                .write(WritePriority::User, move |tx| {
                    run_once(tx, &key, REMOVE_TAG, now, |tx| {
                        let (tag_id, removed) = remove_tag(tx, &name, &messages)?;
                        Ok(encode_count("tag_id", &tag_id, "removed", removed))
                    })
                })
                .await
                .map_err(EngineError::from)
        })
        .await
    }

    /// `set_note(message_id, text)` (spec/07): one note per message,
    /// replaced; empty text removes it. Replays are idempotent.
    pub async fn set_note(
        &self,
        idempotency_key: &str,
        message_id: i64,
        text: &str,
    ) -> Result<Receipt, EngineError> {
        validate_idempotency_key(idempotency_key)?;
        if message_id <= 0 {
            return Err(EngineError::invalid_input(
                "message_id",
                "a message id is a positive integer",
            ));
        }
        let note_id = self.resources().new_uuid_string();
        let now = self.resources().now();
        let (key, id, body) = (idempotency_key.to_owned(), message_id, text.to_owned());
        self.command(move |worker| async move {
            worker
                .resources()
                .write(WritePriority::User, move |tx| {
                    run_once(tx, &key, SET_NOTE, now, |tx| {
                        set_note(tx, &note_id, id, &body, now)?;
                        Ok(format!("{{\"message_id\":{id}}}"))
                    })
                })
                .await
                .map_err(EngineError::from)
        })
        .await
    }

    /// Resolve a selection to logical message ids, preserving order
    /// (`NotFound` names an unknown occurrence). A window token must be
    /// store-backed: the debug source's ids name no messages.
    async fn selection_messages(
        &self,
        selection: &MessageSelection,
    ) -> Result<Vec<i64>, EngineError> {
        let ids = match selection {
            MessageSelection::OccurrenceIds(ids) => {
                if ids.len() > MAX_SELECTION_IDS {
                    return Err(EngineError::invalid_input(
                        "selection",
                        format!("an explicit selection holds at most {MAX_SELECTION_IDS} ids"),
                    ));
                }
                ids.clone()
            }
            MessageSelection::Window {
                window_id,
                generation,
            } => {
                let id = WindowId::from(*window_id);
                if !self.windows().store_backed(id) {
                    return Err(EngineError::invalid_input(
                        "selection",
                        "a tag selection needs a store-backed window",
                    ));
                }
                let index = self.window_ids(id).await?;
                if index.generation() != *generation {
                    return Err(EngineError::ResetRequired {
                        generation: index.generation(),
                    });
                }
                index.ids().to_vec()
            }
        };
        if ids.is_empty() {
            return Ok(Vec::new());
        }
        self.read_through(move |resources| resources.read(|tx| messages_for_occurrences(tx, &ids)))
            .await
    }
}

fn valid_name(name: &str) -> Result<(), EngineError> {
    if name.is_empty() || name.len() > MAX_NAME_BYTES {
        return Err(EngineError::invalid_input(
            "tag",
            format!("a tag's name must be 1 through {MAX_NAME_BYTES} UTF-8 bytes"),
        ));
    }
    Ok(())
}

/// `{"<id_field>":"<id>","<count_field>":n}`.
fn encode_count(id_field: &str, id: &str, count_field: &str, count: u64) -> String {
    let mut body = serde_json::Map::new();
    body.insert(id_field.to_owned(), id.into());
    body.insert(count_field.to_owned(), count.into());
    serde_json::Value::Object(body).to_string()
}

#[cfg(test)]
mod tests;
