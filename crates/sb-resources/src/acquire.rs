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
use std::ops::ControlFlow;

use sb_mime::{
    Limits, MBOXRD_ENVELOPE, MailboxAddress, MboxError, MboxRecord, MboxScanner, MboxVariant,
    ParseOutcome, unquote_range,
};
use sb_store::imports::{
    BundleFraming, ImportFlags, ImportMessage, ImportParseState, ImportReadState, ImportSourceKind,
    ImportSummary,
};
use sb_store::{
    ObjectStore, PublishedObject, ReadFile, Scoped, Store, StoreError, read_error_as_store_error,
};
use sha2::{Digest, Sha256};

use crate::{
    BundleFormat, DescriptorCursor, ManifestProofRegistry, MboxQuoting, Resources, SourceInventory,
    SourceKind,
};

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
    verification: Option<VerifiedMessage>,
    /// Frozen manifest descriptors for a bundle mboxrd session.
    descriptors: Option<DescriptorCursor>,
    /// Source offset the next record must start at; zero for a fresh
    /// source and the finished offset for a completed one.
    next_offset: u64,
}

/// The frozen manifest descriptor for one bundle EML message: the exact
/// bytes the streaming verifier must observe before the publisher may
/// install the object (spec/09 "Export bundles").
#[derive(Debug, Clone, Copy)]
struct VerifiedMessage {
    sha256: [u8; 32],
    original_bytes: u64,
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
    // A bundle source ingests only through manifest-verified
    // acquisition (`open_bundle`); it never falls back to the plain
    // path (spec/09 "Export bundles"). Verifying its manifest snapshot
    // first means a missing or corrupt copy fails the job the same way.
    if let Some(bundle) = source.bundle() {
        store.open_import_source(bundle.manifest())?;
        return Err(StoreError::Unavailable {
            reason: format!(
                "{} is an export bundle member; it needs manifest-verified acquisition",
                source.path().display()
            ),
            io_kind: None,
        });
    }
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
            "position {offset}/{messages} does not fit {length}/{total}"
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
        verification: None,
        descriptors: None,
        next_offset: 0,
    })
}

/// Open one bundle source's manifest-verified acquisition session,
/// dispatching on the manifest's declared format. A plain source never
/// falls back to the plain path.
pub(crate) fn open_bundle(
    resources: &Resources,
    registry: &ManifestProofRegistry,
    source: &SourceInventory,
    position: SourcePosition,
    progress: impl FnMut(u64) -> ControlFlow<()>,
) -> Result<Scoped<SourceAcquisition>, StoreError> {
    let bundle = source
        .bundle()
        .ok_or_else(|| invalid("manifest-verified acquisition needs a bundle source"))?;
    match bundle.format() {
        BundleFormat::Eml => open_bundle_eml(resources, registry, source, position, progress),
        BundleFormat::Mboxrd => open_bundle_mboxrd(resources, registry, source, position, progress),
    }
}

/// Open one bundle EML source's session. The frozen registry proof
/// supplies the one descriptor; the source's own snapshot is verified
/// and reopened. `position` is `START` or the source's finished message.
fn open_bundle_eml(
    resources: &Resources,
    registry: &ManifestProofRegistry,
    source: &SourceInventory,
    position: SourcePosition,
    progress: impl FnMut(u64) -> ControlFlow<()>,
) -> Result<Scoped<SourceAcquisition>, StoreError> {
    let bundle = source
        .bundle()
        .ok_or_else(|| invalid("manifest-verified acquisition needs a bundle source"))?;
    if source.kind() != SourceKind::Eml {
        return Err(invalid("an EML bundle source must have kind eml"));
    }
    let length = source.snapshot().byte_length();
    let total = source.message_count();
    if total != 1 {
        return Err(corrupt(format!(
            "an EML bundle source must hold one message, not {total}"
        )));
    }
    if position != SourcePosition::START && position != SourcePosition::new(length, 1) {
        return Err(invalid(format!(
            "position {}/{} does not fit a one-message source of {length} bytes",
            position.offset(),
            position.messages()
        )));
    }
    let file = resources.store.open_import_source(source.snapshot())?;
    let descriptor = match registry.with_proof(resources, bundle, progress, |proof| {
        let mut descriptors = Vec::new();
        proof.visit_source(source, |descriptor| {
            descriptors.push(descriptor);
            Ok(())
        })?;
        if descriptors.len() != 1 {
            return Err(io::Error::new(
                io::ErrorKind::InvalidData,
                "an EML bundle source must resolve exactly one descriptor",
            ));
        }
        Ok(descriptors.pop().expect("one descriptor"))
    }) {
        Ok(Scoped::Done(descriptor)) => descriptor,
        Ok(Scoped::Stopped) => return Ok(Scoped::Stopped),
        Err(error) => return Err(proof_error(error)),
    };
    if descriptor.original_bytes != length || descriptor.sha256 != source.snapshot().sha256() {
        return Err(corrupt(format!(
            "{}'s manifest descriptor does not match its captured bytes",
            source.path().display()
        )));
    }
    Ok(Scoped::Done(SourceAcquisition {
        file,
        kind: SourceKind::Eml,
        variant: MboxVariant::Mboxrd,
        path: source
            .path()
            .to_str()
            .expect("source paths are UTF-8")
            .to_owned(),
        sha256: source.snapshot().sha256_hex(),
        length,
        total,
        position,
        verification: Some(VerifiedMessage {
            sha256: descriptor.sha256,
            original_bytes: descriptor.original_bytes,
        }),
        descriptors: None,
        next_offset: 0,
    }))
}

/// Open one bundle mboxrd source's session over the frozen registry.
/// The source's own snapshot is verified and reopened; the cursor reads
/// its descriptors directly. A checkpoint must name the exact next
/// record offset and stay inside the captured file.
fn open_bundle_mboxrd(
    resources: &Resources,
    registry: &ManifestProofRegistry,
    source: &SourceInventory,
    position: SourcePosition,
    progress: impl FnMut(u64) -> ControlFlow<()>,
) -> Result<Scoped<SourceAcquisition>, StoreError> {
    let bundle = source
        .bundle()
        .ok_or_else(|| invalid("manifest-verified acquisition needs a bundle source"))?;
    if bundle.format() != BundleFormat::Mboxrd {
        return Err(invalid("an mboxrd bundle source must have format mboxrd"));
    }
    if source.kind() != SourceKind::Mbox {
        return Err(invalid("an mboxrd bundle source must have kind mbox"));
    }
    let length = source.snapshot().byte_length();
    let total = source.message_count();
    let SourcePosition { offset, messages } = position;
    if messages > total || offset > length {
        return Err(invalid(format!(
            "position {offset}/{messages} does not fit {length}/{total}"
        )));
    }
    if total == 0 {
        if position != SourcePosition::START {
            return Err(invalid(format!(
                "zero-message source has only start, not {offset}/{messages}"
            )));
        }
    } else if messages == total && offset != length {
        return Err(invalid(format!(
            "finished source must be at {length}, not {offset}"
        )));
    }
    let file = resources.store.open_import_source(source.snapshot())?;
    let mut cursor = match registry
        .open_source_descriptors(resources, bundle, source, messages, progress)
        .map_err(proof_error)?
    {
        Scoped::Done(cursor) => cursor,
        Scoped::Stopped => return Ok(Scoped::Stopped),
    };
    let next_offset = if total == 0 || messages == total {
        length
    } else {
        cursor.seek_to(messages).map_err(proof_error)?;
        let descriptor = match cursor.next().map_err(proof_error)? {
            Some(descriptor) => descriptor,
            None => {
                return Err(corrupt(format!(
                    "{}: proof ended before {messages}",
                    source.path().display()
                )));
            }
        };
        if descriptor.record_offset != offset {
            return Err(invalid(format!(
                "checkpoint {offset} != descriptor {messages} ({})",
                descriptor.record_offset
            )));
        }
        let end = descriptor
            .record_offset
            .checked_add(descriptor.encoded_bytes)
            .ok_or_else(|| {
                corrupt(format!(
                    "{}: record at {} overflows",
                    source.path().display(),
                    descriptor.record_offset
                ))
            })?;
        if end > length {
            return Err(corrupt(format!(
                "{}: record at {} ends {end} past {length}",
                source.path().display(),
                descriptor.record_offset
            )));
        }
        cursor.seek_to(messages).map_err(proof_error)?;
        offset
    };
    Ok(Scoped::Done(SourceAcquisition {
        file,
        kind: SourceKind::Mbox,
        variant: MboxVariant::Mboxrd,
        path: source
            .path()
            .to_str()
            .expect("source paths are UTF-8")
            .to_owned(),
        sha256: source.snapshot().sha256_hex(),
        length,
        total,
        position,
        verification: None,
        descriptors: Some(cursor),
        next_offset,
    }))
}

/// Map a registry proof failure back to its typed store cause.
fn proof_error(error: io::Error) -> StoreError {
    if let Some(error) = read_error_as_store_error(&error) {
        return error.clone();
    }
    match error.kind() {
        io::ErrorKind::NotFound => StoreError::NotFound {
            what: error.to_string(),
        },
        io::ErrorKind::InvalidData => StoreError::Corrupt {
            detail: error.to_string(),
        },
        io::ErrorKind::Interrupted => StoreError::Interrupted {
            detail: error.to_string(),
        },
        kind => StoreError::Unavailable {
            reason: error.to_string(),
            io_kind: Some(kind),
        },
    }
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
    let keep = limits.max_message_bytes().saturating_add(1);
    if source.descriptors.is_some() {
        let (object, prefix, next, framing) =
            next_bundle_mboxrd(store, source, offset, messages, keep)?;
        source.position = next;
        return Ok(Some(build_message(
            source,
            object,
            &prefix,
            limits,
            offset,
            Some(framing),
            next,
        )));
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
    let published = match record {
        None => {
            let length = source.length;
            source
                .file
                .seek(SeekFrom::Start(0))
                .map_err(|e| unreadable(&source.path, offset, e))?;
            match source.verification {
                Some(verification) => publish(
                    store,
                    VerifiedRead::new(&mut *source.file, verification),
                    keep,
                ),
                None => publish(store, (&mut source.file).take(length), keep),
            }
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
        Err(Failure::Read(e)) => {
            if let Some(error) = read_error_as_store_error(&e) {
                return Err(error.clone());
            }
            return Err(unreadable(&source.path, offset, e));
        }
        Err(Failure::Store(e)) => return Err(e),
    };
    if record.is_none() && object.id().byte_length() != source.length {
        return Err(corrupt(format!(
            "{} published {} of its {} bytes",
            source.path,
            object.id().byte_length(),
            source.length
        )));
    }

    source.position = next;
    Ok(Some(build_message(
        source, object, &prefix, limits, offset, None, next,
    )))
}

/// Build one acquired message and its next position from a verified
/// publication. The caller has already checked the source boundary.
fn build_message(
    source: &SourceAcquisition,
    object: PublishedObject,
    prefix: &[u8],
    limits: &Limits,
    offset: u64,
    bundle_framing: Option<BundleFraming>,
    next: SourcePosition,
) -> AcquiredMessage {
    let extracted = sb_mime::import_flags(prefix, limits);
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
        bundle_framing,
        flags,
        summary: summary(&sb_mime::parse(prefix, limits)),
    };
    AcquiredMessage {
        message,
        flags: if extracted.is_complete() {
            FlagExtraction::Complete
        } else {
            FlagExtraction::Incomplete
        },
        next,
    }
}

/// Verify and publish one bundle mboxrd record from the frozen cursor.
/// Every check runs before the publisher sees a successful EOF: the
/// descriptor's exact range and envelope, contiguous boundaries, the
/// declared framing LF, and the original length and SHA-256.
fn next_bundle_mboxrd(
    store: &Store,
    source: &mut SourceAcquisition,
    offset: u64,
    messages: u64,
    keep: usize,
) -> Result<(PublishedObject, Vec<u8>, SourcePosition, BundleFraming), StoreError> {
    let path = source.path.clone();
    let (descriptor, next_descriptor, end) = {
        let cursor = source.descriptors.as_mut().expect("bundle mboxrd cursor");
        let descriptor = match cursor.next().map_err(proof_error)? {
            Some(descriptor) => descriptor,
            None => {
                return Err(corrupt(format!(
                    "{path}: proof ended after {messages} of {}",
                    source.total
                )));
            }
        };
        if descriptor.record_offset != source.next_offset {
            return Err(corrupt(format!(
                "{path}: record at {} after boundary {}",
                descriptor.record_offset, source.next_offset
            )));
        }
        let end = descriptor
            .record_offset
            .checked_add(descriptor.encoded_bytes)
            .ok_or_else(|| {
                corrupt(format!(
                    "{path}: record at {} overflows",
                    descriptor.record_offset
                ))
            })?;
        if end > source.length {
            return Err(corrupt(format!(
                "{path}: record at {} ends {end} past {}",
                descriptor.record_offset, source.length
            )));
        }
        let next_descriptor = cursor.peek().map_err(proof_error)?;
        (descriptor, next_descriptor, end)
    };
    match next_descriptor {
        Some(next) if next.record_offset != end => {
            return Err(corrupt(format!(
                "{path}: record at {} ends {end}, next at {}",
                descriptor.record_offset, next.record_offset
            )));
        }
        None if messages + 1 != source.total => {
            return Err(corrupt(format!(
                "{path}: proof ended after {} of {}",
                messages + 1,
                source.total
            )));
        }
        None if end != source.length => {
            return Err(corrupt(format!(
                "{path}: last record ends {end} of {}",
                source.length
            )));
        }
        _ => {}
    }
    let envelope_len = MBOXRD_ENVELOPE.len() as u64;
    if descriptor.encoded_bytes < envelope_len {
        return Err(corrupt(format!(
            "{path}: record at {} shorter than envelope",
            descriptor.record_offset
        )));
    }
    let body_start = descriptor.record_offset + envelope_len;
    let body_end = end;
    let unquote_end = if descriptor.framing_lf_added {
        if descriptor.original_bytes == 0 || body_end - body_start < 2 {
            return Err(corrupt(format!(
                "{path}: record at {} pads empty/one-byte body",
                descriptor.record_offset
            )));
        }
        let padding_offset = body_end - 1;
        let mut padding = [0u8; 1];
        source
            .file
            .seek(SeekFrom::Start(padding_offset))
            .map_err(|e| unreadable(&path, offset, e))?;
        source
            .file
            .read_exact(&mut padding)
            .map_err(|e| unreadable(&path, offset, e))?;
        if padding[0] != b'\n' {
            return Err(corrupt(format!(
                "{path}: record at {} missing framing LF",
                descriptor.record_offset
            )));
        }
        let before = body_end - 2;
        let mut previous = [0u8; 1];
        source
            .file
            .seek(SeekFrom::Start(before))
            .map_err(|e| unreadable(&path, offset, e))?;
        source
            .file
            .read_exact(&mut previous)
            .map_err(|e| unreadable(&path, offset, e))?;
        if previous[0] == b'\n' {
            return Err(corrupt(format!(
                "{path}: record at {} pads an LF-terminated original",
                descriptor.record_offset
            )));
        }
        padding_offset
    } else {
        if descriptor.original_bytes > 0 {
            if body_end == body_start {
                return Err(corrupt(format!(
                    "{path}: record at {} missing final LF",
                    descriptor.record_offset
                )));
            }
            let mut last = [0u8; 1];
            source
                .file
                .seek(SeekFrom::Start(body_end - 1))
                .map_err(|e| unreadable(&path, offset, e))?;
            source
                .file
                .read_exact(&mut last)
                .map_err(|e| unreadable(&path, offset, e))?;
            if last[0] != b'\n' {
                return Err(corrupt(format!(
                    "{path}: record at {} missing final LF",
                    descriptor.record_offset
                )));
            }
        } else if body_end != body_start {
            return Err(corrupt(format!(
                "{path}: empty record at {} has padding",
                descriptor.record_offset
            )));
        }
        body_end
    };
    source
        .file
        .seek(SeekFrom::Start(descriptor.record_offset))
        .map_err(|e| unreadable(&path, offset, e))?;
    let mut envelope = [0u8; MBOXRD_ENVELOPE.len()];
    source
        .file
        .read_exact(&mut envelope)
        .map_err(|e| unreadable(&path, offset, e))?;
    if envelope != *MBOXRD_ENVELOPE {
        return Err(corrupt(format!(
            "{path}: record at {} has wrong envelope",
            descriptor.record_offset
        )));
    }
    let decoded = unquote_range(
        &mut *source.file,
        body_start,
        unquote_end,
        MboxVariant::Mboxrd,
    )
    .map_err(|e| unreadable(&path, offset, e))?;
    let verified = VerifiedRead::new(
        decoded,
        VerifiedMessage {
            sha256: descriptor.sha256,
            original_bytes: descriptor.original_bytes,
        },
    );
    let (object, prefix) = match publish(store, verified, keep) {
        Ok(published) => published,
        Err(Failure::Read(e)) => {
            if let Some(error) = read_error_as_store_error(&e) {
                return Err(error.clone());
            }
            return Err(unreadable(&path, offset, e));
        }
        Err(Failure::Store(e)) => return Err(e),
    };
    let framing = BundleFraming {
        original_sha256: descriptor.sha256,
        original_bytes: descriptor.original_bytes,
        record_offset: descriptor.record_offset,
        encoded_bytes: descriptor.encoded_bytes,
        framing_lf_added: descriptor.framing_lf_added,
    };
    source.next_offset = end;
    Ok((
        object,
        prefix,
        SourcePosition::new(end, messages + 1),
        framing,
    ))
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
fn publish(
    store: &Store,
    bytes: impl Read,
    keep: usize,
) -> Result<(PublishedObject, Vec<u8>), Failure> {
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

/// A counted, hashed reader that verifies the expected SHA-256 and
/// length at EOF, before the publisher sees a successful EOF and may
/// install the object. It reads at most the expected bytes and probes
/// one more to reject growth; I/O errors keep their native kind.
struct VerifiedRead<R> {
    inner: R,
    expected: VerifiedMessage,
    hasher: Sha256,
    read: u64,
    done: bool,
}

impl<R: Read> VerifiedRead<R> {
    fn new(inner: R, expected: VerifiedMessage) -> Self {
        Self {
            inner,
            expected,
            hasher: Sha256::new(),
            read: 0,
            done: false,
        }
    }

    fn corrupt(&self, detail: String) -> io::Error {
        io::Error::new(io::ErrorKind::InvalidData, StoreError::Corrupt { detail })
    }

    fn finish(&mut self) -> io::Result<()> {
        let actual: [u8; 32] = self.hasher.clone().finalize().into();
        if self.read != self.expected.original_bytes || actual != self.expected.sha256 {
            return Err(self.corrupt(format!(
                "expected {} bytes hashed {}, found {} bytes hashed {}",
                self.expected.original_bytes,
                hex(&self.expected.sha256),
                self.read,
                hex(&actual)
            )));
        }
        Ok(())
    }
}

impl<R: Read> Read for VerifiedRead<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if self.done {
            return Ok(0);
        }
        if self.read == self.expected.original_bytes {
            let mut extra = [0];
            let n = self.inner.read(&mut extra)?;
            if n > 0 {
                return Err(self.corrupt(format!(
                    "message grew past its {} expected bytes",
                    self.expected.original_bytes
                )));
            }
            self.done = true;
            return self.finish().map(|()| 0);
        }
        let room = (self.expected.original_bytes - self.read).min(buf.len() as u64) as usize;
        let n = self.inner.read(&mut buf[..room])?;
        if n == 0 {
            return Err(self.corrupt(format!(
                "message ended after {} of {} expected bytes",
                self.read, self.expected.original_bytes
            )));
        }
        self.hasher.update(&buf[..n]);
        self.read += n as u64;
        Ok(n)
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|byte| format!("{byte:02x}")).collect()
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
pub(crate) fn addresses(list: &[MailboxAddress]) -> Option<String> {
    (!list.is_empty()).then(|| {
        list.iter()
            .map(|a| serde_json::json!({"address": a.address(), "name": a.name()}))
            .collect::<serde_json::Value>()
            .to_string()
    })
}

pub(crate) fn strings(list: &[String]) -> Option<String> {
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

#[cfg(test)]
mod bundle_tests;

#[cfg(test)]
mod mboxrd_tests;

#[cfg(test)]
mod reservation_tests;
