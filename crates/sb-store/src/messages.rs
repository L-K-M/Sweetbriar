//! Reader metadata in a short snapshot, with object I/O performed afterward.

use std::io::Read;

use rusqlite::{OptionalExtension, Row, Transaction};
use serde_json::Value;

use crate::schema::classify;
use crate::{ObjectId, ObjectStore, Store, StoreError, read_error_as_store_error};

/// Spec/10's decimal raw-message limit, also used by the MIME leaf.
pub const MAX_READER_MESSAGE_BYTES: u64 = 100_000_000;
/// The MIME admission header budget; checked before copying or decoding JSON.
pub const MAX_READER_HEADER_BYTES: usize = 262_144;
const READ_CHUNK_BYTES: usize = 64 * 1024;

/// Availability recorded in SQLite, not a filesystem verification promise.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BodyAvailability {
    NotAcquired,
    Acquired,
    AcquisitionMissing,
    AcquisitionCorrupt,
}

/// The best complete acquisition selected by `logical_messages.acquisition_id`.
/// Private fields prevent callers from forging a different object or state.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAcquisition {
    acquisition_id: i64,
    object: ObjectId,
    availability: BodyAvailability,
}

impl StoredAcquisition {
    pub fn acquisition_id(&self) -> i64 {
        self.acquisition_id
    }

    pub fn object(&self) -> &ObjectId {
        &self.object
    }

    pub fn availability(&self) -> BodyAvailability {
        self.availability
    }
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

/// Resolve any retained occurrence, including one that has vanished.
/// An unknown id is `NotFound`, never an unrelated representative row.
pub fn message_for_occurrence(tx: &Transaction, occurrence: i64) -> Result<i64, StoreError> {
    tx.query_row(
        "SELECT message_id FROM occurrences WHERE occurrence_id = ?1",
        [occurrence],
        |row| row.get(0),
    )
    .optional()
    .map_err(sql_error)?
    .ok_or_else(|| StoreError::not_found(format!("occurrence {occurrence}")))
}

/// Metadata only: no file I/O or long-lived reader transaction (spec/03, 07).
pub fn get_message(tx: &Transaction, message: i64) -> Result<ReaderMessage, StoreError> {
    let (account_id, account_display_name, thread_id, note) = tx
        .query_row(
            "SELECT m.account_id, a.display_name,
         (SELECT thread_id FROM d_thread_members WHERE message_id = m.message_id),
         (SELECT body_source FROM notes WHERE message_id = m.message_id)
         FROM logical_messages m JOIN accounts a USING (account_id) WHERE m.message_id = ?1",
            [message],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(sql_error)?
        .ok_or_else(|| StoreError::not_found(format!("message {message}")))?;
    let acquisition = best_acquisition(tx, message)?;
    let headers = headers(tx, message)?;
    let mut statement = tx
        .prepare_cached(
            "SELECT o.occurrence_id, o.mailbox_id, b.remote_name
         FROM occurrences o JOIN mailboxes b USING (mailbox_id)
         WHERE o.message_id = ?1 AND o.state = 'present' ORDER BY o.occurrence_id",
        )
        .map_err(sql_error)?;
    let occurrences = statement
        .query_map([message], |row| {
            Ok(ReaderOccurrence {
                occurrence_id: row.get(0)?,
                mailbox_id: row.get(1)?,
                mailbox_name: row.get(2)?,
            })
        })
        .map_err(sql_error)?
        .collect::<Result<_, _>>()
        .map_err(sql_error)?;
    let mut statement = tx
        .prepare_cached(
            "SELECT t.tag_id, t.name, t.color_srgb FROM message_tags mt JOIN tags t USING (tag_id)
         WHERE mt.message_id = ?1 ORDER BY t.name, t.tag_id",
        )
        .map_err(sql_error)?;
    let tags = statement
        .query_map([message], |row| {
            Ok(ReaderTag {
                tag_id: row.get(0)?,
                name: row.get(1)?,
                color_srgb: row.get(2)?,
            })
        })
        .map_err(sql_error)?
        .collect::<Result<_, _>>()
        .map_err(sql_error)?;
    Ok(ReaderMessage {
        message_id: message,
        headers,
        account_id,
        account_display_name,
        occurrences,
        thread_id,
        tags,
        note,
        body_availability: acquisition.map_or(BodyAvailability::NotAcquired, |a| a.availability),
    })
}

/// `None` means a known message has no body yet; unknown messages are `NotFound`.
pub fn best_acquisition(
    tx: &Transaction,
    message: i64,
) -> Result<Option<StoredAcquisition>, StoreError> {
    tx.query_row(
        "SELECT a.acquisition_id, a.content_sha256, a.byte_length, a.object_state
         FROM logical_messages m LEFT JOIN acquisitions a USING (acquisition_id)
         WHERE m.message_id = ?1",
        [message],
        |row| {
            let Some(acquisition_id) = row.get::<_, Option<i64>>(0)? else {
                return Ok(Ok(None));
            };
            let hash: String = row.get(1)?;
            let length: i64 = row.get(2)?;
            let state: String = row.get(3)?;
            Ok(acquisition(acquisition_id, &hash, length, &state).map(Some))
        },
    )
    .optional()
    .map_err(sql_error)?
    .ok_or_else(|| StoreError::not_found(format!("message {message}")))?
}

fn acquisition(
    id: i64,
    hash: &str,
    length: i64,
    state: &str,
) -> Result<StoredAcquisition, StoreError> {
    let availability = match state {
        "published" => BodyAvailability::Acquired,
        "missing" => BodyAvailability::AcquisitionMissing,
        "quarantined" => BodyAvailability::AcquisitionCorrupt,
        _ => return Err(StoreError::corrupt("unknown acquisition state")),
    };
    if hash.len() != 64
        || !hash
            .bytes()
            .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
    {
        return Err(StoreError::corrupt("invalid acquisition SHA-256"));
    }
    let mut bytes = [0; 32];
    for (index, byte) in bytes.iter_mut().enumerate() {
        *byte = u8::from_str_radix(&hash[index * 2..index * 2 + 2], 16)
            .map_err(|_| StoreError::corrupt("invalid acquisition SHA-256"))?;
    }
    let length =
        u64::try_from(length).map_err(|_| StoreError::corrupt("negative acquisition size"))?;
    Ok(StoredAcquisition {
        acquisition_id: id,
        object: ObjectId::from_parts(bytes, length),
        availability,
    })
}

impl Store {
    /// Blocking I/O, outside the metadata transaction. Returns exact bytes only
    /// after EOF verifies the hash; corruption uses the existing quarantine path.
    /// Recorded missing/corrupt states fail explicitly. This read does not update
    /// SQLite: acquisition-state writes belong to the engine (spec/03).
    pub fn read_acquisition(&self, acquisition: &StoredAcquisition) -> Result<Vec<u8>, StoreError> {
        match acquisition.availability {
            BodyAvailability::AcquisitionMissing => {
                return Err(StoreError::not_found("acquisition object"));
            }
            BodyAvailability::AcquisitionCorrupt => {
                return Err(StoreError::corrupt("acquisition object is quarantined"));
            }
            BodyAvailability::NotAcquired => {
                return Err(StoreError::unavailable("body is not acquired"));
            }
            BodyAvailability::Acquired => {}
        }
        if acquisition.object.byte_length() > MAX_READER_MESSAGE_BYTES {
            return Err(StoreError::unavailable(
                "acquisition exceeds the 100 MB message limit",
            ));
        }
        let mut reader = self.objects().open(&acquisition.object)?;
        let capacity = usize::try_from(acquisition.object.byte_length())
            .map_err(|_| StoreError::unavailable("acquisition exceeds addressable memory"))?;
        let mut bytes = Vec::with_capacity(capacity);
        let mut chunk = [0; READ_CHUNK_BYTES];
        loop {
            let count = reader.read(&mut chunk).map_err(|error| {
                read_error_as_store_error(&error)
                    .cloned()
                    .unwrap_or_else(|| {
                        StoreError::unavailable(format!("cannot read acquisition: {error}"))
                    })
            })?;
            if count == 0 {
                return Ok(bytes);
            }
            // ObjectReader refuses growth beyond the declared, admitted length.
            bytes.extend_from_slice(&chunk[..count]);
        }
    }
}

fn headers(tx: &Transaction, message: i64) -> Result<ReaderHeaders, StoreError> {
    let summary = tx
        .query_row(
            "SELECT s.from_name, s.from_addr, s.to_json, s.cc_json, s.subject,
         s.header_message_id, s.date_header FROM logical_messages m
         JOIN d_message_summary s USING (acquisition_id) WHERE m.message_id = ?1",
            [message],
            |row| {
                let mut budget = MAX_READER_HEADER_BYTES;
                Ok((|| {
                    let name = header_text(row, 0, &mut budget)?;
                    let address = header_text(row, 1, &mut budget)?;
                    let from = if name.is_some() || address.is_some() {
                        vec![ReaderAddress { name, address }]
                    } else {
                        Vec::new()
                    };
                    build_headers(
                        from,
                        header_text(row, 2, &mut budget)?,
                        header_text(row, 3, &mut budget)?,
                        header_text(row, 4, &mut budget)?,
                        header_text(row, 5, &mut budget)?,
                        row.get(6).map_err(sql_error)?,
                    )
                })())
            },
        )
        .optional()
        .map_err(sql_error)?;
    if let Some(summary) = summary {
        return summary;
    }
    tx.query_row(
        "SELECT from_json, to_json, cc_json, subject, header_message_id, date_header
         FROM message_headers WHERE message_id = ?1",
        [message],
        |row| {
            let mut budget = MAX_READER_HEADER_BYTES;
            Ok((|| {
                let from = header_text(row, 0, &mut budget)?;
                build_headers(
                    addresses(from)?,
                    header_text(row, 1, &mut budget)?,
                    header_text(row, 2, &mut budget)?,
                    header_text(row, 3, &mut budget)?,
                    header_text(row, 4, &mut budget)?,
                    row.get(5).map_err(sql_error)?,
                )
            })())
        },
    )
    .optional()
    .map_err(sql_error)?
    .unwrap_or_else(|| Ok(ReaderHeaders::default()))
}

fn header_text(
    row: &Row<'_>,
    column: usize,
    budget: &mut usize,
) -> Result<Option<String>, StoreError> {
    let value = row.get_ref(column).map_err(sql_error)?;
    if value == rusqlite::types::ValueRef::Null {
        return Ok(None);
    }
    let text = value
        .as_str()
        .map_err(|_| StoreError::corrupt("invalid reader header text"))?;
    *budget = budget
        .checked_sub(text.len())
        .ok_or_else(|| StoreError::corrupt("reader headers exceed the header byte limit"))?;
    Ok(Some(text.to_owned()))
}

fn build_headers(
    from: Vec<ReaderAddress>,
    to: Option<String>,
    cc: Option<String>,
    subject: Option<String>,
    message_id: Option<String>,
    date: Option<i64>,
) -> Result<ReaderHeaders, StoreError> {
    Ok(ReaderHeaders {
        from,
        to: addresses(to)?,
        cc: addresses(cc)?,
        subject,
        message_id,
        date,
    })
}

fn addresses(json: Option<String>) -> Result<Vec<ReaderAddress>, StoreError> {
    let Some(json) = json else {
        return Ok(Vec::new());
    };
    let value: Value = serde_json::from_str(&json)
        .map_err(|_| StoreError::corrupt("invalid reader address JSON"))?;
    let list = value
        .as_array()
        .ok_or_else(|| StoreError::corrupt("reader addresses must be an array"))?;
    list.iter()
        .map(|value| {
            let object = value
                .as_object()
                .ok_or_else(|| StoreError::corrupt("reader address must be an object"))?;
            let text = |key| match object.get(key) {
                None | Some(Value::Null) => Ok(None),
                Some(Value::String(text)) => Ok(Some(text.clone())),
                _ => Err(StoreError::corrupt("invalid reader address field")),
            };
            Ok(ReaderAddress {
                name: text("name")?,
                address: text("address")?,
            })
        })
        .collect()
}

fn sql_error(error: rusqlite::Error) -> StoreError {
    classify(error, "a reader message query")
}

#[cfg(test)]
mod tests;
