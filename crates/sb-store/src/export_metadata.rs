//! Frozen export metadata from one retained occurrence (spec/09).
//! Own every value so source edits cannot change serialization.
//! Never read headers, objects or other occurrences.

use rusqlite::{OptionalExtension, Row, Transaction};
use serde::Serialize;

use crate::StoreError;
use crate::messages::{self, BodyAvailability};
use crate::schema::classify;

/// Capture format 1's version (spec/09).
const FORMAT_VERSION: u8 = 1;
const SRGB_TEXT_BYTES: usize = 7;

/// An owned, read-only capture-format-1 snapshot.
/// Serialize directly to a writer, without a corpus-sized JSON buffer.
#[derive(Serialize)]
pub struct ExportMetadata {
    v: u8,
    occurrence_id: i64,
    message_id: i64,
    account: ExportAccount,
    mailbox: ExportMailbox,
    occurrence_state: OccurrenceState,
    flags: ExportFlags,
    keywords: Vec<String>,
    acquisition: Option<ExportAcquisition>,
    body_availability: RecordedAvailability,
    tags: Vec<ExportTag>,
    note: Option<String>,
}

#[derive(Serialize)]
struct ExportAccount {
    id: String,
    name: String,
}

#[derive(Serialize)]
struct ExportMailbox {
    id: i64,
    name: String,
}

/// The schema's occurrence states, serializing as their stored words.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum OccurrenceState {
    Present,
    VanishPending,
    Vanished,
    EpochStale,
    Superseded,
}

#[derive(Serialize)]
struct ExportFlags {
    seen: bool,
    flagged: bool,
    answered: bool,
    draft: bool,
    deleted: bool,
}

/// Recorded acquisition identity, never an object-store path.
#[derive(Serialize)]
struct ExportAcquisition {
    id: i64,
    sha256: String,
    bytes: u64,
}

/// Availability as recorded in SQLite (spec/09): a stored fact, never a
/// claim that object bytes were verified.
#[derive(Serialize)]
#[serde(rename_all = "snake_case")]
enum RecordedAvailability {
    NotAcquired,
    Acquired,
    Missing,
    Quarantined,
}

#[derive(Serialize)]
struct ExportTag {
    id: String,
    name: String,
    color_srgb: Option<String>,
}

/// The occurrence row's decoded, validated facts.
struct StoredOccurrence {
    message_id: i64,
    acquisition_id: Option<i64>,
    account: ExportAccount,
    mailbox: ExportMailbox,
    state: OccurrenceState,
    flags: ExportFlags,
    keywords: Vec<String>,
}

/// Snapshot one retained occurrence in the caller's read transaction.
/// Unknown IDs are `NotFound`; malformed stored metadata is `Corrupt`.
/// Environmental and interruption errors keep their typed cause.
pub fn export_occurrence_metadata(tx: &Transaction, id: i64) -> Result<ExportMetadata, StoreError> {
    let stored = stored_occurrence(tx, id)?;
    let acquisition = messages::best_acquisition(tx, stored.message_id)?;
    if acquisition.as_ref().map(|a| a.acquisition_id()) != stored.acquisition_id {
        return Err(StoreError::corrupt(
            "the message's acquisition reference is broken",
        ));
    }
    let body_availability = match acquisition
        .as_ref()
        .map_or(BodyAvailability::NotAcquired, |a| a.availability())
    {
        BodyAvailability::NotAcquired => RecordedAvailability::NotAcquired,
        BodyAvailability::Acquired => RecordedAvailability::Acquired,
        BodyAvailability::AcquisitionMissing => RecordedAvailability::Missing,
        BodyAvailability::AcquisitionCorrupt => RecordedAvailability::Quarantined,
    };
    let acquisition = acquisition.map(|a| ExportAcquisition {
        id: a.acquisition_id(),
        sha256: a.object().sha256_hex(),
        bytes: a.object().byte_length(),
    });
    Ok(ExportMetadata {
        v: FORMAT_VERSION,
        occurrence_id: id,
        message_id: stored.message_id,
        account: stored.account,
        mailbox: stored.mailbox,
        occurrence_state: stored.state,
        flags: stored.flags,
        keywords: stored.keywords,
        acquisition,
        body_availability,
        tags: stored_tags(tx, stored.message_id)?,
        note: stored_note(tx, stored.message_id)?,
    })
}

/// Keep missing or cross-account parents visible as corruption.
/// Inner joins would make a retained occurrence look absent.
fn stored_occurrence(tx: &Transaction, id: i64) -> Result<StoredOccurrence, StoreError> {
    tx.query_row(
        "SELECT m.message_id, o.account_id, o.mailbox_id, o.state,
         o.f_seen, o.f_flagged, o.f_answered, o.f_draft, o.f_deleted, o.keywords,
         a.display_name, b.remote_name, m.acquisition_id
         FROM occurrences o
         LEFT JOIN accounts a ON a.account_id = o.account_id
         LEFT JOIN mailboxes b ON b.mailbox_id = o.mailbox_id AND b.account_id = o.account_id
         LEFT JOIN logical_messages m ON m.message_id = o.message_id AND m.account_id = o.account_id
         WHERE o.occurrence_id = ?1",
        [id],
        |row| Ok(occurrence_row(row)),
    )
    .optional()
    .map_err(sql_error)?
    .ok_or_else(|| StoreError::not_found(format!("occurrence {id}")))?
}

fn occurrence_row(row: &Row<'_>) -> Result<StoredOccurrence, StoreError> {
    let state: String = row.get(3).map_err(sql_error)?;
    let keywords: String = row.get(9).map_err(sql_error)?;
    let account_name: Option<String> = row.get(10).map_err(sql_error)?;
    let mailbox_name: Option<String> = row.get(11).map_err(sql_error)?;
    Ok(StoredOccurrence {
        message_id: row.get(0).map_err(sql_error)?,
        acquisition_id: row.get(12).map_err(sql_error)?,
        account: ExportAccount {
            id: row.get(1).map_err(sql_error)?,
            name: account_name
                .ok_or_else(|| StoreError::corrupt("an occurrence's account is missing"))?,
        },
        mailbox: ExportMailbox {
            id: row.get(2).map_err(sql_error)?,
            name: mailbox_name
                .ok_or_else(|| StoreError::corrupt("an occurrence's mailbox is missing"))?,
        },
        state: occurrence_state(&state)?,
        flags: ExportFlags {
            seen: flag(row, 4)?,
            flagged: flag(row, 5)?,
            answered: flag(row, 6)?,
            draft: flag(row, 7)?,
            deleted: flag(row, 8)?,
        },
        keywords: stored_keywords(&keywords)?,
    })
}

/// A stored flag is exactly 0 or 1 (schema CHECK); anything else is
/// corruption, never coerced to a boolean.
fn flag(row: &Row<'_>, column: usize) -> Result<bool, StoreError> {
    match row.get::<_, i64>(column).map_err(sql_error)? {
        0 => Ok(false),
        1 => Ok(true),
        _ => Err(StoreError::corrupt(
            "a stored occurrence flag is not 0 or 1",
        )),
    }
}

fn occurrence_state(stored: &str) -> Result<OccurrenceState, StoreError> {
    Ok(match stored {
        "present" => OccurrenceState::Present,
        "vanish_pending" => OccurrenceState::VanishPending,
        "vanished" => OccurrenceState::Vanished,
        "epoch_stale" => OccurrenceState::EpochStale,
        "superseded" => OccurrenceState::Superseded,
        _ => return Err(StoreError::corrupt("unknown stored occurrence state")),
    })
}

/// Validate the stored keyword set without normalizing unknown strings.
fn stored_keywords(stored: &str) -> Result<Vec<String>, StoreError> {
    let corrupt = || StoreError::corrupt("invalid stored occurrence keywords");
    let keywords: Vec<String> = serde_json::from_str(stored).map_err(|_| corrupt())?;
    if keywords.windows(2).any(|pair| pair[0] >= pair[1]) {
        return Err(corrupt());
    }
    Ok(keywords)
}

/// The message's tags, sorted by name then id as stored (spec/09).
fn stored_tags(tx: &Transaction, message: i64) -> Result<Vec<ExportTag>, StoreError> {
    let rows = tx
        .prepare_cached(
            "SELECT t.tag_id, t.name, t.color_srgb
             FROM message_tags mt LEFT JOIN tags t USING (tag_id)
             WHERE mt.message_id = ?1 ORDER BY t.name, t.tag_id",
        )
        .map_err(sql_error)?
        .query_map([message], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .map_err(sql_error)?
        .collect::<Result<Vec<(String, String, Option<String>)>, _>>()
        .map_err(sql_error)?;
    rows.into_iter()
        .map(|(id, name, color)| {
            Ok(ExportTag {
                id,
                name,
                color_srgb: stored_color(color)?,
            })
        })
        .collect()
}

/// A stored tag color is `#rrggbb` lowercase or NULL (schema CHECK).
fn stored_color(stored: Option<String>) -> Result<Option<String>, StoreError> {
    let Some(color) = stored else {
        return Ok(None);
    };
    let valid = color.len() == SRGB_TEXT_BYTES
        && color.starts_with('#')
        && color[1..]
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b));
    if !valid {
        return Err(StoreError::corrupt("invalid stored tag color"));
    }
    Ok(Some(color))
}

/// The message's note's Markdown source, or `None` (spec/02 `notes`).
fn stored_note(tx: &Transaction, message: i64) -> Result<Option<String>, StoreError> {
    tx.query_row(
        "SELECT body_source FROM notes WHERE message_id = ?1",
        [message],
        |row| row.get(0),
    )
    .optional()
    .map_err(sql_error)
}

/// Row type failures are corruption; preserve environmental/interrupt errors.
fn sql_error(error: rusqlite::Error) -> StoreError {
    match error {
        rusqlite::Error::InvalidColumnType(..)
        | rusqlite::Error::IntegralValueOutOfRange(..)
        | rusqlite::Error::FromSqlConversionFailure(..)
        | rusqlite::Error::Utf8Error(..) => StoreError::corrupt(format!(
            "a stored export metadata value is undecodable: {error}"
        )),
        _ => classify(error, "an export metadata read"),
    }
}

#[cfg(test)]
mod tests;
