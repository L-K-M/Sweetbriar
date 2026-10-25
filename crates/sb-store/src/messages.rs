//! Reader metadata in a short snapshot, with object I/O performed afterward.

use std::io::Read;

use rusqlite::{Connection, OptionalExtension, Row, Transaction};
use serde_json::Value;

use crate::message_source::{self, MESSAGES, Source};
use crate::schema::classify;
use crate::{ObjectId, ObjectReader, ObjectStore, Store, StoreError, read_error_as_store_error};

/// Spec/10's decimal raw-message limit, also used by the MIME leaf.
pub const MAX_READER_MESSAGE_BYTES: u64 = 100_000_000;
/// The MIME admission header budget.
pub const MAX_READER_HEADER_BYTES: usize = 262_144;
/// Stored header text of one message, checked before copying or decoding
/// JSON: an admitted header block as the import writer stores it. Address
/// JSON is ~14 bytes per raw byte at worst (`a,` becomes
/// `{"address":"a","name":null},`) and decoding can triple a byte (U+FFFD);
/// 32× bounds both.
pub(crate) const MAX_STORED_HEADER_BYTES: usize = 32 * MAX_READER_HEADER_BYTES;
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

/// The acquisition a queue `doc_id` names (spec/06: the queue key is an
/// acquisition id). `NotFound` for an orphaned queue row; no runtime
/// producer creates one, so absence is an invariant failure, not a
/// transient file read.
pub fn index_acquisition(conn: &Connection, doc_id: i64) -> Result<StoredAcquisition, StoreError> {
    conn.prepare_cached(
        "SELECT acquisition_id, content_sha256, byte_length, object_state
         FROM acquisitions WHERE acquisition_id = ?1",
    )
    .map_err(sql_error)?
    .query_row([doc_id], |row| {
        let hash: String = row.get(1)?;
        let length: i64 = row.get(2)?;
        let state: String = row.get(3)?;
        Ok(acquisition(row.get(0)?, &hash, length, &state))
    })
    .optional()
    .map_err(sql_error)?
    .ok_or_else(|| StoreError::not_found(format!("index acquisition {doc_id}")))?
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

/// Availability recorded in SQLite gates any object open: recorded
/// missing/corrupt states fail explicitly, without filesystem work.
pub(crate) fn check_availability(acquisition: &StoredAcquisition) -> Result<(), StoreError> {
    match acquisition.availability {
        BodyAvailability::AcquisitionMissing => Err(StoreError::not_found("acquisition object")),
        BodyAvailability::AcquisitionCorrupt => {
            Err(StoreError::corrupt("acquisition object is quarantined"))
        }
        BodyAvailability::NotAcquired => Err(StoreError::unavailable("body is not acquired")),
        BodyAvailability::Acquired => Ok(()),
    }
}

/// The outcome of one verification pass over an open object.
enum Streamed {
    /// Read to EOF; the hash and length matched the id.
    Verified,
    /// Stopped at a chunk boundary; nothing was verified.
    Cancelled,
}

/// One full pass through `reader` to EOF, keeping the reader's
/// streaming hash verification, quarantine and error recovery.
/// `sink` retains the bytes; `None` verifies without retaining.
/// `cancelled` is polled before every chunk; a stop reports
/// `Streamed::Cancelled` with no verification claim.
fn drain_object(
    reader: &mut ObjectReader,
    mut sink: Option<&mut Vec<u8>>,
    cancelled: &dyn Fn() -> bool,
) -> Result<Streamed, StoreError> {
    let mut chunk = [0; READ_CHUNK_BYTES];
    loop {
        if cancelled() {
            return Ok(Streamed::Cancelled);
        }
        let count = reader.read(&mut chunk).map_err(|error| {
            read_error_as_store_error(&error)
                .cloned()
                .unwrap_or_else(|| {
                    StoreError::unavailable_io(
                        format!("cannot read acquisition: {error}"),
                        error.kind(),
                    )
                })
        })?;
        if count == 0 {
            return Ok(Streamed::Verified);
        }
        // ObjectReader refuses growth beyond the declared, admitted length.
        if let Some(sink) = sink.as_deref_mut() {
            sink.extend_from_slice(&chunk[..count]);
        }
    }
}

fn never_cancelled() -> bool {
    false
}

/// The domain outcome of a bounded, cancellable acquisition read
/// (spec/06 "Preparation boundary").
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum AcquisitionRead {
    /// The exact bytes, verified to EOF and retained within `retain`.
    Verified(Vec<u8>),
    /// Verified to EOF without retention: the declared length exceeded
    /// `retain`. A bounded size refusal, never an error.
    Oversized,
    /// Stopped before object I/O or at a chunk boundary; nothing was
    /// verified or acknowledged.
    Cancelled,
}

impl Store {
    /// Blocking I/O, outside the metadata transaction. Returns exact bytes only
    /// after EOF verifies the hash; corruption uses the existing quarantine path.
    /// Recorded missing/corrupt states fail explicitly. This read does not update
    /// SQLite: acquisition-state writes belong to the engine (spec/03).
    pub fn read_acquisition(&self, acquisition: &StoredAcquisition) -> Result<Vec<u8>, StoreError> {
        check_availability(acquisition)?;
        if acquisition.object.byte_length() > MAX_READER_MESSAGE_BYTES {
            return Err(StoreError::unavailable(
                "acquisition exceeds the 100 MB message limit",
            ));
        }
        let mut reader = self.objects().open(&acquisition.object)?;
        let capacity = usize::try_from(acquisition.object.byte_length())
            .map_err(|_| StoreError::unavailable("acquisition exceeds addressable memory"))?;
        let mut bytes = Vec::with_capacity(capacity);
        match drain_object(&mut reader, Some(&mut bytes), &never_cancelled)? {
            Streamed::Verified => Ok(bytes),
            Streamed::Cancelled => Err(StoreError::internal(
                "uncancellable read reported cancelled",
            )),
        }
    }

    /// One verified streaming pass retaining at most `retain` bytes
    /// (spec/06): the same availability errors and EOF verification as
    /// [`Store::read_acquisition`], but a declared length over `retain`
    /// is hashed without keeping the body, a bounded size refusal,
    /// not the reader's early over-100 MB error. `retain` may not
    /// exceed the spec/10 100 MB ceiling; a zero `retain` can hold
    /// only an empty object.
    pub fn read_acquisition_bounded(
        &self,
        acquisition: &StoredAcquisition,
        retain: u64,
        cancelled: &dyn Fn() -> bool,
    ) -> Result<AcquisitionRead, StoreError> {
        check_availability(acquisition)?;
        if retain > MAX_READER_MESSAGE_BYTES {
            return Err(StoreError::InvalidInput {
                message: "retention limit exceeds the 100 MB message limit".into(),
            });
        }
        if cancelled() {
            return Ok(AcquisitionRead::Cancelled);
        }
        let mut reader = self.objects().open(&acquisition.object)?;
        let mut bytes = if acquisition.object.byte_length() <= retain {
            let capacity = usize::try_from(acquisition.object.byte_length())
                .map_err(|_| StoreError::unavailable("acquisition exceeds addressable memory"))?;
            Some(Vec::with_capacity(capacity))
        } else {
            None
        };
        match drain_object(&mut reader, bytes.as_mut(), cancelled)? {
            Streamed::Cancelled => Ok(AcquisitionRead::Cancelled),
            Streamed::Verified => Ok(match bytes {
                Some(bytes) => AcquisitionRead::Verified(bytes),
                None => AcquisitionRead::Oversized,
            }),
        }
    }
}

fn headers(tx: &Transaction, message: i64) -> Result<ReaderHeaders, StoreError> {
    let source = Source::Stored;
    let [name, address, to, cc, subject, id, date] = source.fields([
        "from_name",
        "from_addr",
        "to_json",
        "cc_json",
        "subject",
        "header_message_id",
        "date_header",
    ]);
    let (summary, validity, present) = (source.joins(), source.validity(), source.present());
    let summary = tx
        .query_row(
            &format!(
                "SELECT {name},{address},{to},{cc},{subject},{id},{date},{validity},{present}
                 FROM {MESSAGES} {summary} WHERE lm.message_id=?1"
            ),
            [message],
            |row| {
                message_source::check(row, 7)?;
                let mut budget = MAX_STORED_HEADER_BYTES;
                Ok((
                    row.get::<_, bool>(8)?,
                    (|| {
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
                    })(),
                ))
            },
        )
        .optional()
        .map_err(sql_error)?;
    if let Some((true, summary)) = summary {
        return summary;
    }
    tx.query_row(
        "SELECT from_json, to_json, cc_json, subject, header_message_id, date_header
         FROM message_headers WHERE message_id = ?1",
        [message],
        |row| {
            let mut budget = MAX_STORED_HEADER_BYTES;
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

pub(crate) fn header_text(
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

// Predicate reads share the reader's stored-shape and Corrupt policy.
pub(crate) fn addresses(json: Option<String>) -> Result<Vec<ReaderAddress>, StoreError> {
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
