//! Acquisition of inventoried import sources (spec/09 "Ingest"). Exact
//! bytes stream from the verified snapshot into the object store
//! (spec/03); a bounded prefix feeds the MIME summary and archive flags
//! (spec/10). Nothing here commits rows: the job batches the results.
//!
//! ```text
//!  snapshot ──seek──▶ scanner (one record) ──▶ unquoter ──▶ Tee ──▶ publish
//!                                                            │
//!                                       first budget+1 bytes ▼
//!                                              parse + import_flags
//! ```

use std::io::{self, BufReader, Read, Seek, SeekFrom};

use sb_mime::{
    Limits, MailboxAddress, MboxError, MboxRecord, MboxScanner, MboxVariant, ParseOutcome,
};
use sb_store::imports::{
    ImportFlags, ImportMessage, ImportParseState, ImportReadState, ImportSourceKind, ImportSummary,
};
use sb_store::{ObjectId, ObjectStore, ReadFile, Store, StoreError};

use crate::{MboxQuoting, SourceInventory, SourceKind};

/// Where one source's acquisition continues: the next unread snapshot
/// offset and the messages consumed before it (spec/09). The job stores
/// both in its source checkpoint.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct SourcePosition {
    offset: u64,
    messages: u64,
}

impl SourcePosition {
    /// Nothing consumed yet.
    pub const START: Self = Self {
        offset: 0,
        messages: 0,
    };

    /// From a stored checkpoint; opening checks it against its source.
    pub fn new(offset: u64, messages: u64) -> Self {
        Self { offset, messages }
    }

    pub fn offset(&self) -> u64 {
        self.offset
    }

    pub fn messages(&self) -> u64 {
        self.messages
    }
}

/// Whether flag extraction examined the whole root header block. An
/// incomplete one may have missed a status header, so the report
/// counts it apart from messages that had none (spec/09 "Flags").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum FlagExtraction {
    Complete,
    Incomplete,
}

/// One published message, ready for a batch, and the position after it.
#[derive(Debug)]
pub struct AcquiredMessage {
    message: ImportMessage,
    flags: FlagExtraction,
    next: SourcePosition,
}

impl AcquiredMessage {
    pub fn message(&self) -> &ImportMessage {
        &self.message
    }

    pub fn into_message(self) -> ImportMessage {
        self.message
    }

    pub fn flag_extraction(&self) -> FlagExtraction {
        self.flags
    }

    /// Where acquisition continues once this message is committed.
    pub fn next_position(&self) -> SourcePosition {
        self.next
    }
}

/// One source's verified snapshot, open at a position. It owns the
/// file, so a job can keep it across batch commits.
pub struct SourceAcquisition {
    file: Box<dyn ReadFile>,
    kind: SourceKind,
    variant: MboxVariant,
    path: String,
    sha256: String,
    length: u64,
    total: u64,
    position: SourcePosition,
}

impl SourceAcquisition {
    /// The position the next message starts from.
    pub fn position(&self) -> SourcePosition {
        self.position
    }
}

impl std::fmt::Debug for SourceAcquisition {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SourceAcquisition")
            .field("path", &self.path)
            .field("position", &self.position)
            .finish_non_exhaustive()
    }
}

/// Check `position` against `source`, then verify and reopen its
/// snapshot (spec/03). Equivalent sources decode identically either way,
/// so only `Ambiguous` ones need `variant`.
pub(crate) fn open(
    store: &Store,
    source: &SourceInventory,
    variant: Option<MboxVariant>,
    position: SourcePosition,
) -> Result<SourceAcquisition, StoreError> {
    let variant = match (source.quoting(), variant) {
        (_, Some(variant)) => variant,
        (MboxQuoting::Equivalent, None) => MboxVariant::Mboxrd,
        (MboxQuoting::Ambiguous, None) => {
            return Err(invalid("this mbox needs an mboxo or mboxrd choice"));
        }
    };
    let length = source.snapshot().byte_length();
    let total = source.message_count();
    let SourcePosition { offset, messages } = position;
    let consistent = match source.kind() {
        // One message at offset 0; done means the whole file was read.
        SourceKind::Eml => {
            position == SourcePosition::START || position == SourcePosition::new(length, 1)
        }
        // The first separator starts the file, and the last record
        // ends it, so both ends of the range agree with the count.
        SourceKind::Mbox => {
            offset <= length
                && messages <= total
                && (messages == 0) == (offset == 0)
                && (messages == total) == (offset == length)
        }
    };
    if !consistent {
        return Err(invalid(format!(
            "position {offset}/{messages} does not fit a source of {length} bytes and {total} messages"
        )));
    }
    Ok(SourceAcquisition {
        file: store.open_import_source(source.snapshot())?,
        kind: source.kind(),
        variant,
        path: source
            .path()
            .to_str()
            .expect("source paths are UTF-8")
            .to_owned(),
        sha256: source.snapshot().sha256_hex(),
        length,
        total,
        position,
    })
}

/// Publish the next message and summarize its bounded prefix, or `None`
/// once the inventoried count is consumed. A failure leaves the
/// position unchanged; any object it published stays unreferenced.
pub(crate) fn next(
    store: &Store,
    source: &mut SourceAcquisition,
    limits: &Limits,
) -> Result<Option<AcquiredMessage>, StoreError> {
    let SourcePosition { offset, messages } = source.position;
    if messages == source.total {
        return Ok(None);
    }
    let record = match source.kind {
        SourceKind::Eml => None,
        SourceKind::Mbox => Some(record(source)?),
    };
    let end = record.map_or(source.length, |record| record.byte_range().end);
    let next = SourcePosition::new(end, messages + 1);
    // Checked before publishing: an inventory that disagrees with its
    // bytes stops the job instead of storing a guess (spec/09).
    if (next.messages == source.total) != (next.offset == source.length) {
        return Err(corrupt(format!(
            "{} has a message boundary at byte {end} that its inventory of {} does not",
            source.path, source.total
        )));
    }

    // Identity is the separator for mbox and 0 for EML: the position's
    // offset either way.
    let keep = limits.max_message_bytes().saturating_add(1);
    let published = match record {
        None => {
            let length = source.length;
            source
                .file
                .seek(SeekFrom::Start(0))
                .map_err(|e| unreadable(&source.path, offset, e))?;
            publish(store, (&mut source.file).take(length), keep)
        }
        Some(record) => {
            let reader = record
                .reader(&mut source.file, source.variant)
                .map_err(|e| unreadable(&source.path, offset, e))?;
            publish(store, reader, keep)
        }
    };
    let (object, prefix) = match published {
        Ok(published) => published,
        Err(Failure::Read(e)) => return Err(unreadable(&source.path, offset, e)),
        Err(Failure::Store(e)) => return Err(e),
    };
    if record.is_none() && object.byte_length() != source.length {
        return Err(corrupt(format!(
            "{} published {} of its {} bytes",
            source.path,
            object.byte_length(),
            source.length
        )));
    }

    let extracted = sb_mime::import_flags(&prefix, limits);
    let flags = ImportFlags {
        read: extracted
            .seen()
            .map_or(ImportReadState::ArchiveDefault, |seen| {
                ImportReadState::FromHeaders { seen }
            }),
        flagged: extracted.flagged(),
        answered: extracted.answered(),
        draft: extracted.draft(),
        deleted: extracted.deleted(),
    };
    let message = ImportMessage {
        object,
        source_kind: match source.kind {
            SourceKind::Mbox => ImportSourceKind::Mbox,
            SourceKind::Eml => ImportSourceKind::Eml,
        },
        source_path: source.path.clone(),
        source_sha256: source.sha256.clone(),
        offset,
        flags,
        summary: summary(&sb_mime::parse(&prefix, limits)),
    };
    source.position = next;
    Ok(Some(AcquiredMessage {
        message,
        flags: if extracted.is_complete() {
            FlagExtraction::Complete
        } else {
            FlagExtraction::Incomplete
        },
        next,
    }))
}

/// Scan the one record that starts at the position's separator.
fn record(source: &mut SourceAcquisition) -> Result<MboxRecord, StoreError> {
    let offset = source.position.offset;
    source
        .file
        .seek(SeekFrom::Start(offset))
        .map_err(|e| unreadable(&source.path, offset, e))?;
    let rest = (&mut source.file).take(source.length - offset);
    match MboxScanner::resume(BufReader::new(rest), offset).next() {
        Some(Ok(record)) => Ok(record),
        Some(Err(MboxError::Io { offset, source: e })) => Err(unreadable(&source.path, offset, e)),
        Some(Err(error)) => Err(corrupt(format!("{}: {error}", source.path))),
        None => Err(corrupt(format!(
            "{} ended after {} of {} messages",
            source.path, source.position.messages, source.total
        ))),
    }
}

enum Failure {
    Read(io::Error),
    Store(StoreError),
}

/// Stream `bytes` into the object store, keeping the first `keep` bytes
/// for bounded parsing. A read failure is told apart from the store's.
fn publish(store: &Store, bytes: impl Read, keep: usize) -> Result<(ObjectId, Vec<u8>), Failure> {
    let mut tee = Tee {
        inner: bytes,
        prefix: Vec::new(),
        keep,
        failure: None,
    };
    match store.objects().publish(&mut tee) {
        Ok(object) => Ok((object, tee.prefix)),
        Err(error) => Err(tee.failure.map_or(Failure::Store(error), Failure::Read)),
    }
}

/// Exact bytes pass through untouched; only a bounded prefix is copied.
struct Tee<R> {
    inner: R,
    prefix: Vec<u8>,
    keep: usize,
    failure: Option<io::Error>,
}

impl<R: Read> Read for Tee<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = loop {
            match self.inner.read(buf) {
                Ok(n) => break n,
                // Publication does not retry; a signal is not a failure.
                Err(e) if e.kind() == io::ErrorKind::Interrupted => continue,
                Err(e) => {
                    let report = io::Error::new(e.kind(), e.to_string());
                    self.failure = Some(e);
                    return Err(report);
                }
            }
        };
        let room = self.keep.saturating_sub(self.prefix.len());
        self.prefix.extend_from_slice(&buf[..n.min(room)]);
        Ok(n)
    }
}

/// Map a parse outcome onto the `d_message_summary` row (spec/02).
pub(super) fn summary(outcome: &ParseOutcome) -> ImportSummary {
    let parse_state = match outcome {
        ParseOutcome::Parsed(_) => ImportParseState::Parsed,
        ParseOutcome::Partial { .. } => ImportParseState::Partial,
        ParseOutcome::Failed(_) => ImportParseState::Failed,
    };
    let Some(summary) = outcome.summary() else {
        return ImportSummary {
            parser_version: sb_mime::PARSER_VERSION,
            parse_state,
            ..ImportSummary::default()
        };
    };
    let owned = |text: Option<&str>| text.map(str::to_owned);
    ImportSummary {
        parser_version: summary.parser_version(),
        parse_state,
        header_message_id: owned(summary.message_id()),
        subject: owned(summary.subject()),
        from_addr: owned(summary.from().and_then(MailboxAddress::address)),
        from_name: owned(summary.from().and_then(MailboxAddress::name)),
        reply_to_json: addresses(summary.reply_to()),
        to_json: addresses(summary.to()),
        cc_json: addresses(summary.cc()),
        date_header: summary.date().map(|date| date.as_millis()),
        in_reply_to: owned(summary.in_reply_to()),
        references_json: strings(summary.references()),
        list_id: owned(summary.list_id()),
        delivered_to_json: strings(summary.delivered_to()),
        has_attachments: summary.has_attachments(),
        snippet: owned(summary.snippet()),
    }
}

/// `[{"address": …, "name": …}]`, missing parts `null`; empty is NULL.
fn addresses(list: &[MailboxAddress]) -> Option<String> {
    (!list.is_empty()).then(|| {
        list.iter()
            .map(|a| serde_json::json!({"address": a.address(), "name": a.name()}))
            .collect::<serde_json::Value>()
            .to_string()
    })
}

fn strings(list: &[String]) -> Option<String> {
    (!list.is_empty()).then(|| serde_json::json!(list).to_string())
}

fn unreadable(path: &str, offset: u64, error: io::Error) -> StoreError {
    StoreError::Unavailable {
        reason: format!("cannot read the message at byte {offset} of {path}: {error}"),
        io_kind: Some(error.kind()),
    }
}

fn corrupt(detail: String) -> StoreError {
    StoreError::Corrupt { detail }
}

fn invalid(message: impl Into<String>) -> StoreError {
    StoreError::InvalidInput {
        message: message.into(),
    }
}

#[cfg(test)]
mod tests;
