//! Private tags and notes (spec/02, spec/07): local-only, never sent
//! to a server. Migration 1's schema.

use rusqlite::{OptionalExtension, Transaction, params};
use sb_types::time::Timestamp;

use crate::StoreError;
use crate::schema::classify;

/// A tag's name is 1 through 128 UTF-8 bytes (spec/02).
const MAX_NAME_BYTES: usize = 128;

/// `add_tag(selection, tag)` (spec/07): attach the named tag to every
/// selected logical message, creating the definition under
/// `new_tag_id` when the name is new; an existing name keeps its id.
pub fn add_tag(
    tx: &Transaction,
    new_tag_id: &str,
    name: &str,
    messages: &[i64],
    now: Timestamp,
) -> Result<(String, u64), StoreError> {
    valid_name(name)?;
    let tag_id = match tag_id_by_name(tx, name)? {
        Some(tag_id) => tag_id,
        None => {
            tx.execute(
                "INSERT INTO tags (tag_id, name, revision) VALUES (?1, ?2, 1)",
                params![new_tag_id, name],
            )
            .map_err(|e| classify(e, "create a tag"))?;
            new_tag_id.to_owned()
        }
    };
    let mut added = 0;
    for &message in messages {
        added += tx
            .execute(
                "INSERT INTO message_tags (message_id, tag_id, added_at)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(message_id, tag_id) DO NOTHING",
                params![message, tag_id, now.as_millis()],
            )
            .map_err(|e| classify(e, "attach a tag"))? as u64;
    }
    Ok((tag_id, added))
}

/// `remove_tag(selection, tag)` (spec/07): detach the named tag. The
/// definition stays (spec/02 rule 8); an unknown name is `NotFound`.
/// Answers the tag's id and how many pairs this call removed.
pub fn remove_tag(
    tx: &Transaction,
    name: &str,
    messages: &[i64],
) -> Result<(String, u64), StoreError> {
    valid_name(name)?;
    let tag_id =
        tag_id_by_name(tx, name)?.ok_or_else(|| StoreError::not_found(format!("tag {name}")))?;
    let mut removed = 0;
    for &message in messages {
        removed += tx
            .execute(
                "DELETE FROM message_tags WHERE message_id = ?1 AND tag_id = ?2",
                params![message, tag_id],
            )
            .map_err(|e| classify(e, "detach a tag"))? as u64;
    }
    Ok((tag_id, removed))
}

/// `set_note(message_id, text)` (spec/07): one note per message, its
/// Markdown source replaced; empty text removes it. An unknown message
/// is `NotFound`; rewriting the same source changes nothing.
pub fn set_note(
    tx: &Transaction,
    note_id: &str,
    message_id: i64,
    text: &str,
    now: Timestamp,
) -> Result<(), StoreError> {
    require_message(tx, message_id)?;
    if text.is_empty() {
        tx.execute("DELETE FROM notes WHERE message_id = ?1", [message_id])
            .map_err(|e| classify(e, "remove a note"))?;
        return Ok(());
    }
    if let Some((source, revision)) = note_source(tx, message_id)? {
        if source == text {
            return Ok(());
        }
        let revision = revision
            .checked_add(1)
            .ok_or_else(|| StoreError::corrupt("a note's revision is exhausted"))?;
        tx.execute(
            "UPDATE notes SET body_source = ?2, updated_at = ?3, revision = ?4
             WHERE message_id = ?1",
            params![message_id, text, now.as_millis(), revision],
        )
        .map_err(|e| classify(e, "update a note"))?;
        return Ok(());
    }
    tx.execute(
        "INSERT INTO notes (note_id, message_id, body_source, created_at, updated_at, revision)
         VALUES (?1, ?2, ?3, ?4, ?4, 1)",
        params![note_id, message_id, text, now.as_millis()],
    )
    .map_err(|e| classify(e, "save a note"))?;
    Ok(())
}

fn tag_id_by_name(tx: &Transaction, name: &str) -> Result<Option<String>, StoreError> {
    tx.query_row("SELECT tag_id FROM tags WHERE name = ?1", [name], |row| {
        row.get(0)
    })
    .optional()
    .map_err(|e| classify(e, "read a tag"))
}

fn note_source(tx: &Transaction, message_id: i64) -> Result<Option<(String, i64)>, StoreError> {
    tx.query_row(
        "SELECT body_source, revision FROM notes WHERE message_id = ?1",
        [message_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )
    .optional()
    .map_err(|e| classify(e, "read a note"))
}

fn require_message(tx: &Transaction, message_id: i64) -> Result<(), StoreError> {
    tx.query_row(
        "SELECT 1 FROM logical_messages WHERE message_id = ?1",
        [message_id],
        |_| Ok(()),
    )
    .optional()
    .map_err(|e| classify(e, "check the note's message"))?
    .ok_or_else(|| StoreError::not_found(format!("message {message_id}")))
}

fn valid_name(name: &str) -> Result<(), StoreError> {
    if name.is_empty() || name.len() > MAX_NAME_BYTES {
        return Err(StoreError::InvalidInput {
            message: "a tag's name must be 1 through 128 UTF-8 bytes".into(),
        });
    }
    Ok(())
}
