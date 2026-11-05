//! Callback-scoped, disposable proof for one captured bundle manifest.
use std::cell::{Cell, RefCell};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::ops::ControlFlow;
use std::sync::RwLock;

use sb_store::{ImportSourceSnapshot, RegistryFile, Scoped, ValidationFile, ValidationScratch};

use super::json_syntax;
use super::offset_sort::{OffsetSort, SortEntry, SortKind};
use super::{BundleFormat, SourceBundle, SourceInventory};

const DESCRIPTOR_SIZE: u64 = 57;
const ROW_SIZE: u64 = 32;
const SLOT_SIZE: u64 = 24;
const REGISTRY_COPY_CHUNK: usize = 64 * 1024;
const HEADER_MAGIC: &[u8; 8] = b"MPROOF1\n";
const HEADER_SIZE: u64 = 8 + 1 + 8 + 8 + 8 + 8 + 32 + 8 + 32;

/// The validated manifest's expected bytes and mbox framing for a message.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ManifestDescriptor {
    /// SHA-256 of the original message bytes.
    pub sha256: [u8; 32],
    /// Length of the original message bytes.
    pub original_bytes: u64,
    /// Offset of the mbox record, or zero for EML.
    pub record_offset: u64,
    /// Length of the encoded mbox record, or zero for EML.
    pub encoded_bytes: u64,
    /// Whether the mbox writer appended a framing newline.
    pub framing_lf_added: bool,
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid manifest proof")
}

fn conflict() -> io::Error {
    io::Error::new(
        io::ErrorKind::InvalidData,
        "conflicting manifest proof identity",
    )
}

fn store_error(error: sb_store::StoreError) -> io::Error {
    io::Error::new(error.io_kind().unwrap_or(io::ErrorKind::Other), error)
}

fn combine(error: io::Error, cleanup: Result<(), sb_store::StoreError>) -> io::Error {
    match cleanup {
        Ok(()) => error,
        Err(cleanup) => io::Error::new(
            error.kind(),
            format!("{error}; registry cleanup failed: {cleanup}"),
        ),
    }
}

fn put_u64(writer: &mut dyn Write, value: u64) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

fn get_u64(reader: &mut (impl Read + ?Sized)) -> io::Result<u64> {
    let mut bytes = [0; 8];
    reader.read_exact(&mut bytes)?;
    Ok(u64::from_le_bytes(bytes))
}

impl ManifestDescriptor {
    fn write(&self, writer: &mut impl Write) -> io::Result<()> {
        writer.write_all(&self.sha256)?;
        put_u64(writer, self.original_bytes)?;
        put_u64(writer, self.record_offset)?;
        put_u64(writer, self.encoded_bytes)?;
        writer.write_all(&[u8::from(self.framing_lf_added)])
    }

    fn read(reader: &mut (impl Read + ?Sized)) -> io::Result<Self> {
        let mut sha256 = [0; 32];
        reader.read_exact(&mut sha256)?;
        let original_bytes = get_u64(reader)?;
        let record_offset = get_u64(reader)?;
        let encoded_bytes = get_u64(reader)?;
        let mut flag = [0];
        reader.read_exact(&mut flag)?;
        let framing_lf_added = match flag[0] {
            0 => false,
            1 => true,
            _ => return Err(invalid()),
        };
        Ok(Self {
            sha256,
            original_bytes,
            record_offset,
            encoded_bytes,
            framing_lf_added,
        })
    }
}

/// The fixed header binding one frozen proof to its snapshot and format.
#[derive(Clone)]
struct RegistryHeader {
    token: String,
    sha256: [u8; 32],
    byte_length: u64,
    format: BundleFormat,
    files: u64,
    records: u64,
    descriptors_len: u64,
    lookup_len: u64,
}

fn write_header(writer: &mut dyn Write, header: &RegistryHeader) -> io::Result<()> {
    let format = match header.format {
        BundleFormat::Eml => 0u8,
        BundleFormat::Mboxrd => 1u8,
    };
    writer.write_all(HEADER_MAGIC)?;
    writer.write_all(&[format])?;
    put_u64(writer, header.files)?;
    put_u64(writer, header.records)?;
    put_u64(writer, header.descriptors_len)?;
    put_u64(writer, header.lookup_len)?;
    writer.write_all(&header.sha256)?;
    put_u64(writer, header.byte_length)?;
    writer.write_all(header.token.as_bytes())
}

fn read_header(reader: &mut impl Read) -> io::Result<RegistryHeader> {
    let mut magic = [0; HEADER_MAGIC.len()];
    reader.read_exact(&mut magic)?;
    if magic != *HEADER_MAGIC {
        return Err(invalid());
    }
    let mut format = [0];
    reader.read_exact(&mut format)?;
    let format = match format[0] {
        0 => BundleFormat::Eml,
        1 => BundleFormat::Mboxrd,
        _ => return Err(invalid()),
    };
    let files = get_u64(reader)?;
    let records = get_u64(reader)?;
    let descriptors_len = get_u64(reader)?;
    let lookup_len = get_u64(reader)?;
    let mut sha256 = [0; 32];
    reader.read_exact(&mut sha256)?;
    let byte_length = get_u64(reader)?;
    let mut token = [0; 32];
    reader.read_exact(&mut token)?;
    let token = std::str::from_utf8(&token)
        .map_err(|_| invalid())?
        .to_owned();
    Ok(RegistryHeader {
        token,
        sha256,
        byte_length,
        format,
        files,
        records,
        descriptors_len,
        lookup_len,
    })
}

fn copy_bounded(
    source: &mut dyn sb_store::ReadFile,
    target: &mut dyn sb_store::WriteFile,
    check: &mut impl FnMut() -> ControlFlow<()>,
) -> io::Result<Scoped<u64>> {
    let mut buffer = vec![0; REGISTRY_COPY_CHUNK];
    let mut copied = 0u64;
    loop {
        if check().is_break() {
            return Ok(Scoped::Stopped);
        }
        let read = source.read(&mut buffer)?;
        if read == 0 {
            return Ok(Scoped::Done(copied));
        }
        target.write_all(&buffer[..read])?;
        copied = copied.checked_add(read as u64).ok_or_else(invalid)?;
    }
}

/// A one-manifest proof, valid only during the enclosing callback.
pub struct ManifestProof {
    identity: ImportSourceSnapshot,
    format: BundleFormat,
    files: u64,
    records: u64,
    descriptors: Box<dyn sb_store::ReadFile>,
    lookup: Box<dyn sb_store::ReadFile>,
}

impl ManifestProof {
    /// Stream one inventory source's descriptors in original message order.
    /// Legacy rows without an ordinal fail closed.
    pub fn visit_source(
        &mut self,
        source: &SourceInventory,
        mut visit: impl FnMut(ManifestDescriptor) -> io::Result<()>,
    ) -> io::Result<()> {
        let bundle = source.bundle().ok_or_else(invalid)?;
        if bundle.manifest() != &self.identity {
            return Err(invalid());
        }
        let (start, count) = validate_source(
            self.files,
            self.records,
            self.format,
            &mut *self.lookup,
            bundle,
            source,
        )?;
        self.descriptors.seek(SeekFrom::Start(
            start.checked_mul(DESCRIPTOR_SIZE).ok_or_else(invalid)?,
        ))?;
        for _ in 0..count {
            visit(ManifestDescriptor::read(&mut self.descriptors)?)?;
        }
        Ok(())
    }

    /// Reopen a frozen registry proof, checking the header's identity and
    /// exact output lengths before any slot is read.
    fn from_registry(
        scratch: &ValidationScratch,
        bundle: &SourceBundle,
        header: RegistryHeader,
    ) -> io::Result<Self> {
        let snapshot = bundle.manifest();
        if header.token != snapshot.token()
            || header.sha256 != snapshot.sha256()
            || header.byte_length != snapshot.byte_length()
            || header.format != bundle.format()
        {
            return Err(conflict());
        }
        if header.descriptors_len
            != header
                .records
                .checked_mul(DESCRIPTOR_SIZE)
                .ok_or_else(invalid)?
            || header.lookup_len != header.files.checked_mul(SLOT_SIZE).ok_or_else(invalid)?
        {
            return Err(invalid());
        }
        let mut descriptors = scratch
            .open_registry_file(snapshot, RegistryFile::Descriptors)
            .map_err(store_error)?;
        let mut lookup = scratch
            .open_registry_file(snapshot, RegistryFile::Lookup)
            .map_err(store_error)?;
        if descriptors.seek(SeekFrom::End(0))? != header.descriptors_len
            || lookup.seek(SeekFrom::End(0))? != header.lookup_len
        {
            return Err(invalid());
        }
        descriptors.seek(SeekFrom::Start(0))?;
        lookup.seek(SeekFrom::Start(0))?;
        Ok(Self {
            identity: snapshot.clone(),
            format: bundle.format(),
            files: header.files,
            records: header.records,
            descriptors,
            lookup,
        })
    }
}

/// Validate one source against the frozen lookup and return its
/// descriptor group's start and count. Shared by the streaming visit
/// and the owned descriptor cursor; legacy missing ordinals fail closed.
fn validate_source<R: Read + Seek + ?Sized>(
    files: u64,
    records: u64,
    format: BundleFormat,
    lookup: &mut R,
    bundle: &SourceBundle,
    source: &SourceInventory,
) -> io::Result<(u64, u64)> {
    if bundle.format() != format || source.kind() != format.source_kind() {
        return Err(invalid());
    }
    let ordinal = bundle.file_ordinal().ok_or_else(invalid)?;
    if ordinal >= files {
        return Err(invalid());
    }
    lookup.seek(SeekFrom::Start(
        ordinal.checked_mul(SLOT_SIZE).ok_or_else(invalid)?,
    ))?;
    let start = get_u64(lookup)?;
    let count = get_u64(lookup)?;
    let bytes = get_u64(lookup)?;
    if bytes != source.snapshot().byte_length()
        || count != source.message_count()
        || start
            .checked_add(count)
            .filter(|end| *end <= records)
            .is_none()
    {
        return Err(invalid());
    }
    Ok((start, count))
}

/// A narrow owned cursor over one source's frozen descriptors. It reads
/// fixed-size descriptors in order and keeps no per-mailbox vector.
pub(crate) struct DescriptorCursor {
    descriptors: Box<dyn sb_store::ReadFile>,
    start: u64,
    end: u64,
    next: u64,
    pending: Option<ManifestDescriptor>,
}

impl DescriptorCursor {
    fn new(descriptors: Box<dyn sb_store::ReadFile>, start: u64, end: u64) -> Self {
        Self {
            descriptors,
            start,
            end,
            next: start,
            pending: None,
        }
    }

    /// Position at a descriptor index relative to this source's group.
    pub(crate) fn seek_to(&mut self, index: u64) -> io::Result<()> {
        if index > self.end - self.start {
            return Err(invalid());
        }
        self.next = self.start + index;
        self.pending = None;
        self.descriptors.seek(SeekFrom::Start(
            self.next.checked_mul(DESCRIPTOR_SIZE).ok_or_else(invalid)?,
        ))?;
        Ok(())
    }

    /// The next descriptor without consuming it.
    pub(crate) fn peek(&mut self) -> io::Result<Option<ManifestDescriptor>> {
        if self.pending.is_none() && self.next < self.end {
            self.descriptors.seek(SeekFrom::Start(
                self.next.checked_mul(DESCRIPTOR_SIZE).ok_or_else(invalid)?,
            ))?;
            self.pending = Some(ManifestDescriptor::read(&mut *self.descriptors)?);
        }
        Ok(self.pending)
    }

    /// Consume and return the next descriptor, or `None` at the group end.
    pub(crate) fn next(&mut self) -> io::Result<Option<ManifestDescriptor>> {
        self.peek()?;
        let descriptor = self.pending.take();
        if descriptor.is_some() {
            self.next += 1;
        }
        Ok(descriptor)
    }
}

/// A job-lifetime, disposable registry of frozen manifest proofs.
///
/// One owned root lease holds each validated snapshot's token-namespaced
/// fixed files. The first encounter runs the one-manifest producer once
/// and streams its verified outputs into the namespace; later lookups
/// reopen the frozen files directly, without rehashing or rescanning the
/// manifest. The header is created last, so an absent header is never a
/// validated proof.
pub struct ManifestProofRegistry {
    scratch: ValidationScratch,
    setup: RwLock<()>,
}

impl ManifestProofRegistry {
    fn new(scratch: ValidationScratch) -> Self {
        Self {
            scratch,
            setup: RwLock::new(()),
        }
    }

    /// Remove the registry's root lease explicitly, reporting cleanup
    /// failure. Dropping the registry cleans the same way.
    pub fn finish(self) -> Result<(), sb_store::StoreError> {
        self.scratch.finish()
    }

    /// Freeze `bundle`'s proof on first encounter, then open a narrow
    /// cursor over one source's descriptors. `skip` is the number of
    /// already-consumed messages; the cursor starts at that record.
    /// `Break` stops before the cursor exists.
    pub(crate) fn open_source_descriptors(
        &self,
        resources: &crate::Resources,
        bundle: &SourceBundle,
        source: &SourceInventory,
        skip: u64,
        progress: impl FnMut(u64) -> ControlFlow<()>,
    ) -> io::Result<Scoped<DescriptorCursor>> {
        let progress = RefCell::new(progress);
        if (progress.borrow_mut())(0).is_break() {
            return Ok(Scoped::Stopped);
        }
        let header = match self.ensure_header(bundle, resources, &progress)? {
            Scoped::Done(header) => header,
            Scoped::Stopped => return Ok(Scoped::Stopped),
        };
        let mut lookup = self
            .scratch
            .open_registry_file(bundle.manifest(), RegistryFile::Lookup)
            .map_err(store_error)?;
        let (start, count) = validate_source(
            header.files,
            header.records,
            header.format,
            &mut *lookup,
            bundle,
            source,
        )?;
        if skip > count {
            return Err(invalid());
        }
        let descriptors = self
            .scratch
            .open_registry_file(bundle.manifest(), RegistryFile::Descriptors)
            .map_err(store_error)?;
        let mut cursor = DescriptorCursor::new(descriptors, start, start + count);
        cursor.seek_to(skip)?;
        if (progress.borrow_mut())(bundle.manifest().byte_length()).is_break() {
            return Ok(Scoped::Stopped);
        }
        Ok(Scoped::Done(cursor))
    }

    /// Freeze `bundle`'s proof on first encounter, then visit one source
    /// through the frozen outputs. `Break` stops before publication.
    pub fn with_proof<T>(
        &self,
        resources: &crate::Resources,
        bundle: &SourceBundle,
        progress: impl FnMut(u64) -> ControlFlow<()>,
        use_proof: impl FnOnce(&mut ManifestProof) -> io::Result<T>,
    ) -> io::Result<Scoped<T>> {
        let progress = RefCell::new(progress);
        if (progress.borrow_mut())(0).is_break() {
            return Ok(Scoped::Stopped);
        }
        let header = match self.ensure_header(bundle, resources, &progress)? {
            Scoped::Done(header) => header,
            Scoped::Stopped => return Ok(Scoped::Stopped),
        };
        let mut proof = ManifestProof::from_registry(&self.scratch, bundle, header)?;
        if (progress.borrow_mut())(bundle.manifest().byte_length()).is_break() {
            return Ok(Scoped::Stopped);
        }
        use_proof(&mut proof).map(Scoped::Done)
    }

    /// Return the frozen header, producing it once under the write lock.
    /// Frozen lookups take only a read lock, so they stay parallel; a
    /// lookup concurrent with first production waits for the header.
    fn ensure_header(
        &self,
        bundle: &SourceBundle,
        resources: &crate::Resources,
        progress: &RefCell<impl FnMut(u64) -> ControlFlow<()>>,
    ) -> io::Result<Scoped<RegistryHeader>> {
        {
            let _read = self
                .setup
                .read()
                .map_err(|_| io::Error::other("manifest proof registry lock poisoned"))?;
            if let Some(header) = self.read_header(bundle)? {
                return Ok(Scoped::Done(header));
            }
        }
        let _write = self
            .setup
            .write()
            .map_err(|_| io::Error::other("manifest proof registry lock poisoned"))?;
        if let Some(header) = self.read_header(bundle)? {
            return Ok(Scoped::Done(header));
        }
        self.scratch
            .create_registry_dir(bundle.manifest())
            .map_err(|error| {
                if error.io_kind() == Some(io::ErrorKind::AlreadyExists) {
                    invalid()
                } else {
                    store_error(error)
                }
            })?;
        let produced = resources.with_manifest_proof(
            bundle,
            |n| (progress.borrow_mut())(n),
            |proof| {
                self.freeze(proof, bundle, &mut || {
                    (progress.borrow_mut())(bundle.manifest().byte_length())
                })
            },
        );
        match produced {
            Ok(Scoped::Done(Scoped::Done(()))) => {}
            Ok(Scoped::Done(Scoped::Stopped)) | Ok(Scoped::Stopped) => {
                let cleanup = self.scratch.remove_registry_dir(bundle.manifest());
                return match cleanup {
                    Ok(()) => Ok(Scoped::Stopped),
                    Err(cleanup) => Err(io::Error::other(format!(
                        "manifest proof cancelled; registry cleanup failed: {cleanup}"
                    ))),
                };
            }
            Err(error) => {
                let cleanup = self.scratch.remove_registry_dir(bundle.manifest());
                return Err(combine(error, cleanup));
            }
        }
        Ok(Scoped::Done(self.read_header(bundle)?.ok_or_else(invalid)?))
    }

    /// Read and verify the header, or `None` when no header exists yet.
    fn read_header(&self, bundle: &SourceBundle) -> io::Result<Option<RegistryHeader>> {
        let snapshot = bundle.manifest();
        let mut file = match self
            .scratch
            .open_registry_file(snapshot, RegistryFile::Header)
        {
            Ok(file) => file,
            Err(error) if error.io_kind() == Some(io::ErrorKind::NotFound) => return Ok(None),
            Err(error) => return Err(store_error(error)),
        };
        if file.seek(SeekFrom::End(0))? != HEADER_SIZE {
            return Err(invalid());
        }
        file.seek(SeekFrom::Start(0))?;
        let header = read_header(&mut file)?;
        self.check_header(bundle, &header)?;
        Ok(Some(header))
    }

    fn check_header(&self, bundle: &SourceBundle, header: &RegistryHeader) -> io::Result<()> {
        let snapshot = bundle.manifest();
        if header.token != snapshot.token()
            || header.sha256 != snapshot.sha256()
            || header.byte_length != snapshot.byte_length()
            || header.format != bundle.format()
        {
            return Err(conflict());
        }
        Ok(())
    }

    /// Stream the producer's verified outputs into the snapshot namespace
    /// and freeze the header last.
    fn freeze(
        &self,
        proof: &mut ManifestProof,
        bundle: &SourceBundle,
        check: &mut impl FnMut() -> ControlFlow<()>,
    ) -> io::Result<Scoped<()>> {
        let snapshot = bundle.manifest();
        let descriptors_len = proof
            .records
            .checked_mul(DESCRIPTOR_SIZE)
            .ok_or_else(invalid)?;
        let lookup_len = proof.files.checked_mul(SLOT_SIZE).ok_or_else(invalid)?;
        proof.descriptors.seek(SeekFrom::Start(0))?;
        proof.lookup.seek(SeekFrom::Start(0))?;
        let mut descriptors = self
            .scratch
            .create_registry_file(snapshot, RegistryFile::Descriptors)
            .map_err(store_error)?;
        let mut lookup = self
            .scratch
            .create_registry_file(snapshot, RegistryFile::Lookup)
            .map_err(store_error)?;
        let copied_descriptors =
            match copy_bounded(&mut *proof.descriptors, &mut *descriptors, check)? {
                Scoped::Done(bytes) => bytes,
                Scoped::Stopped => return Ok(Scoped::Stopped),
            };
        let copied_lookup = match copy_bounded(&mut *proof.lookup, &mut *lookup, check)? {
            Scoped::Done(bytes) => bytes,
            Scoped::Stopped => return Ok(Scoped::Stopped),
        };
        if check().is_break() {
            return Ok(Scoped::Stopped);
        }
        descriptors.sync_all()?;
        lookup.sync_all()?;
        if copied_descriptors != descriptors_len || copied_lookup != lookup_len {
            return Err(invalid());
        }
        drop(descriptors);
        drop(lookup);
        if check().is_break() {
            return Ok(Scoped::Stopped);
        }
        let header = RegistryHeader {
            token: snapshot.token().to_owned(),
            sha256: snapshot.sha256(),
            byte_length: snapshot.byte_length(),
            format: bundle.format(),
            files: proof.files,
            records: proof.records,
            descriptors_len,
            lookup_len,
        };
        let mut header_file = self
            .scratch
            .create_registry_file(snapshot, RegistryFile::Header)
            .map_err(store_error)?;
        write_header(&mut *header_file, &header)?;
        header_file.sync_all()?;
        if check().is_break() {
            return Ok(Scoped::Stopped);
        }
        Ok(Scoped::Done(()))
    }
}

struct Scan<'a, R, F> {
    inner: R,
    progress: &'a RefCell<F>,
    stopped: &'a Cell<bool>,
    bytes: u64,
}

impl<R: Read, F: FnMut(u64) -> ControlFlow<()>> Read for Scan<'_, R, F> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.inner.read(buf)?;
        self.bytes = self.bytes.saturating_add(n as u64);
        if (self.progress.borrow_mut())(self.bytes).is_break() {
            self.stopped.set(true);
            return Err(io::Error::other("manifest proof cancelled"));
        }
        Ok(n)
    }
}
impl<R: Seek, F: FnMut(u64) -> ControlFlow<()>> Seek for Scan<'_, R, F> {
    fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
        if (self.progress.borrow_mut())(self.bytes).is_break() {
            self.stopped.set(true);
            return Err(io::Error::other("manifest proof cancelled"));
        }
        self.inner.seek(pos)
    }
}

struct Output {
    descriptors: Box<dyn sb_store::WriteFile>,
    spool: Box<dyn sb_store::WriteFile>,
    records: u64,
    group_start: u64,
}

fn build<R: Read + Seek>(
    reader: &mut R,
    scratch: &ValidationScratch,
    format: BundleFormat,
) -> io::Result<(u64, u64)> {
    let table = json_syntax::validate_envelope(&mut *reader)?;
    if table.format() != format {
        return Err(invalid());
    }
    let files = table.file_count();
    let output = RefCell::new(Output {
        descriptors: scratch
            .create_file(ValidationFile::Descriptors)
            .map_err(store_error)?,
        spool: scratch
            .create_file(ValidationFile::IndexSpool)
            .map_err(store_error)?,
        records: 0,
        group_start: 0,
    });
    json_syntax::validate_messages_with_references(
        reader,
        &table,
        scratch,
        &mut |_, _, bytes, count, ordinal| {
            let mut out = output.borrow_mut();
            if out.records.checked_sub(out.group_start) != Some(count) {
                return Err(invalid());
            }
            let start = out.group_start;
            for value in [ordinal, start, count, bytes] {
                put_u64(&mut out.spool, value)?;
            }
            out.group_start = out.records;
            Ok(())
        },
        &mut |reader, offset, _, _| {
            let descriptor = json_syntax::read_descriptor(reader, offset)?;
            let mut out = output.borrow_mut();
            descriptor.write(&mut out.descriptors)?;
            out.records = out.records.checked_add(1).ok_or_else(invalid)?;
            Ok(())
        },
    )?;
    let mut out = output.into_inner();
    out.descriptors.sync_all()?;
    out.spool.sync_all()?;
    let records = out.records;
    drop(out);
    let mut spool = scratch
        .open_file(ValidationFile::IndexSpool)
        .map_err(store_error)?;
    if spool.seek(SeekFrom::End(0))? != files.checked_mul(ROW_SIZE).ok_or_else(invalid)? {
        return Err(invalid());
    }
    spool.seek(SeekFrom::Start(0))?;
    let mut sorter = OffsetSort::new(scratch)?;
    let mut compare = |_: &mut (), a: SortEntry, b: SortEntry| Ok(a.key.cmp(&b.key));
    for index in 0..files {
        if index % 256 == 0 {
            reader.stream_position()?;
        }
        let entry = SortEntry {
            key: get_u64(&mut spool)?,
            position: get_u64(&mut spool)?,
            extra: get_u64(&mut spool)?,
            ordinal: get_u64(&mut spool)?,
            message_offset: 0,
            kind: SortKind::Offset,
        };
        sorter.push_entry(entry, &mut (), &mut compare)?;
    }
    drop(spool);
    let mut lookup = scratch
        .create_file(ValidationFile::Lookup)
        .map_err(store_error)?;
    let mut expected = 0u64;
    sorter.finish_entries(
        reader,
        &mut |_, a, b| Ok(a.key.cmp(&b.key)),
        &mut |reader| reader.stream_position().map(|_| ()),
        |_, entry| {
            if entry.key != expected
                || entry
                    .position
                    .checked_add(entry.extra)
                    .filter(|end| *end <= records)
                    .is_none()
            {
                return Err(invalid());
            }
            for value in [entry.position, entry.extra, entry.ordinal] {
                put_u64(&mut lookup, value)?;
            }
            expected = expected.checked_add(1).ok_or_else(invalid)?;
            Ok(())
        },
    )?;
    if expected != files {
        return Err(invalid());
    }
    lookup.sync_all()?;
    drop(lookup);
    scratch
        .remove_file(ValidationFile::IndexSpool)
        .map_err(store_error)?;
    Ok((files, records))
}

impl crate::Resources {
    /// Lease a disposable, job-lifetime registry of frozen manifest
    /// proofs. The registry is an owned `Send` value; its root lease is
    /// removed on `finish`, on drop and at the next store open.
    pub fn manifest_proof_registry(&self) -> Result<ManifestProofRegistry, sb_store::StoreError> {
        Ok(ManifestProofRegistry::new(
            self.store.import_validation_scratch()?,
        ))
    }

    /// Verify and index one private manifest for use only inside `use_proof`.
    /// The captured snapshot is immutable by Store contract; scratch is disposable.
    pub fn with_manifest_proof<T>(
        &self,
        bundle: &SourceBundle,
        progress: impl FnMut(u64) -> ControlFlow<()>,
        use_proof: impl FnOnce(&mut ManifestProof) -> io::Result<T>,
    ) -> io::Result<Scoped<T>> {
        let progress = RefCell::new(progress);
        let stopped = Cell::new(false);
        let opened = self
            .store
            .open_import_source_cancellable(bundle.manifest(), |n| (progress.borrow_mut())(n))
            .map_err(store_error)?;
        let file = match opened {
            Scoped::Stopped => return Ok(Scoped::Stopped),
            Scoped::Done(file) => file,
        };
        let scratch = self
            .store
            .import_validation_scratch()
            .map_err(store_error)?;
        let mut reader = BufReader::new(Scan {
            inner: file,
            progress: &progress,
            stopped: &stopped,
            bytes: 0,
        });
        let result = build(&mut reader, &scratch, bundle.format());
        drop(reader);
        let result = if stopped.get() {
            Ok(Scoped::Stopped)
        } else {
            result.and_then(|(files, records)| {
                let mut descriptors = scratch
                    .open_file(ValidationFile::Descriptors)
                    .map_err(store_error)?;
                let mut lookup = scratch
                    .open_file(ValidationFile::Lookup)
                    .map_err(store_error)?;
                if descriptors.seek(SeekFrom::End(0))?
                    != records.checked_mul(DESCRIPTOR_SIZE).ok_or_else(invalid)?
                    || lookup.seek(SeekFrom::End(0))?
                        != files.checked_mul(SLOT_SIZE).ok_or_else(invalid)?
                {
                    return Err(invalid());
                }
                let mut proof = ManifestProof {
                    identity: bundle.manifest().clone(),
                    format: bundle.format(),
                    files,
                    records,
                    descriptors,
                    lookup,
                };
                use_proof(&mut proof).map(Scoped::Done)
            })
        };
        let cleanup = scratch.finish().map_err(store_error);
        match (result, cleanup) {
            (Err(e), Err(cleanup)) => Err(io::Error::new(
                e.kind(),
                format!("{e}; proof workspace cleanup failed: {cleanup}"),
            )),
            (Err(e), Ok(())) => Err(e),
            (Ok(_), Err(e)) => Err(e),
            (Ok(v), Ok(())) => Ok(v),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::super::SourceInventory;
    use super::*;
    use sb_mime::MboxQuoting;
    use sb_test_support::{FaultFs, FaultKind, FsOp, ScratchDir, real_fs};
    use sha2::{Digest, Sha256};
    use std::path::Path;
    use std::sync::Arc;

    fn snapshot(token: char, bytes: u64) -> ImportSourceSnapshot {
        ImportSourceSnapshot::from_metadata(&token.to_string().repeat(32), [3; 32], bytes).unwrap()
    }

    fn source(
        bundle: SourceBundle,
        ordinal: Option<u64>,
        bytes: u64,
        count: u64,
    ) -> SourceInventory {
        SourceInventory {
            path: "/synthetic".into(),
            kind: bundle.format().source_kind(),
            mailbox: "m".into(),
            snapshot: snapshot('b', bytes),
            messages: count,
            quoting: MboxQuoting::Equivalent,
            bundle: Some(SourceBundle::new(
                bundle.manifest().clone(),
                bundle.format(),
                ordinal,
            )),
        }
    }

    fn manifest(count: usize, format: BundleFormat) -> String {
        let mut digest = Sha256::new();
        let mut messages = Vec::new();
        for id in 0..count {
            digest.update((id as i64).to_be_bytes());
            let file = if format == BundleFormat::Eml {
                format!("{id}.eml")
            } else if id % 2 == 0 {
                "a".into()
            } else {
                "z".into()
            };
            let mut message = serde_json::json!({"v":1,"occurrence_id":id,"message_id":2,
                "account":{"id":"a","name":"a"},"mailbox":{"id":1,"name":"m"},
                "occurrence_state":"present","flags":{"seen":false,"flagged":false,"answered":false,"draft":false,"deleted":false},
                "keywords":[],"acquisition":{"id":1,"sha256":"ab".repeat(32),"bytes":42},
                "body_availability":"acquired","tags":[],"note":null,"file":file});
            if format == BundleFormat::Mboxrd {
                message["record_offset"] = (u64::MAX - id as u64).into();
                message["encoded_bytes"] = (u64::MAX - 1).into();
                message["original_bytes"] = 42.into();
                message["framing_lf_added"] = (id % 2 == 0).into();
            }
            messages.push(message);
        }
        let files = if format == BundleFormat::Eml {
            (0..count)
                .rev()
                .map(|id| serde_json::json!({"name":format!("{id}.eml"),"bytes":42,"records":1}))
                .collect::<Vec<_>>()
        } else {
            vec![
                serde_json::json!({"name":"z","bytes":42,"records":count / 2}),
                serde_json::json!({"name":"empty","bytes":0,"records":0}),
                serde_json::json!({"name":"a","bytes":42,"records":count.div_ceil(2)}),
            ]
        };
        let hash = digest
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        serde_json::json!({"v":1,"kind":"export-manifest","job":"j","format":format.name(),"destination":"/d",
            "scope":{"kind":"mailbox","id":1,"generation":1},"generation":1,"entries":count,
            "ids_sha256":hash,"files":files,"messages":messages}).to_string()
    }

    struct CountReader<'a> {
        inner: io::Cursor<&'a [u8]>,
        reads: usize,
    }
    impl Read for CountReader<'_> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.reads += 1;
            self.inner.read(buf)
        }
    }
    impl Seek for CountReader<'_> {
        fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
            self.inner.seek(position)
        }
    }

    fn run_build(
        format: BundleFormat,
        check: impl FnOnce(&mut ManifestProof, &mut CountReader<'_>, &SourceBundle),
    ) {
        let input = manifest(257, format);
        let mut reader = CountReader {
            inner: io::Cursor::new(input.as_bytes()),
            reads: 0,
        };
        let tmp = sb_test_support::ScratchDir::new("manifest-proof");
        let resources = crate::Resources::open(&tmp.path().join("store")).unwrap();
        let scratch = resources.store.import_validation_scratch().unwrap();
        let (files, records) = build(&mut reader, &scratch, format).unwrap();
        assert_eq!(records, 257);
        let bundle = SourceBundle::new(snapshot('a', input.len() as u64), format, Some(0));
        let mut proof = ManifestProof {
            identity: bundle.manifest().clone(),
            format,
            files,
            records,
            descriptors: scratch.open_file(ValidationFile::Descriptors).unwrap(),
            lookup: scratch.open_file(ValidationFile::Lookup).unwrap(),
        };
        check(&mut proof, &mut reader, &bundle);
        drop(proof);
        scratch.finish().unwrap();
    }

    #[test]
    fn eml_random_access_uses_no_manifest_reads() {
        run_build(BundleFormat::Eml, |proof, reader, bundle| {
            let before = reader.reads;
            for ordinal in [0, 256, 129, 17, 0] {
                let mut descriptors = Vec::new();
                proof
                    .visit_source(&source(bundle.clone(), Some(ordinal), 42, 1), |d| {
                        descriptors.push(d);
                        Ok(())
                    })
                    .unwrap();
                assert_eq!(
                    descriptors,
                    vec![ManifestDescriptor {
                        sha256: [0xab; 32],
                        original_bytes: 42,
                        record_offset: 0,
                        encoded_bytes: 0,
                        framing_lf_added: false
                    }]
                );
            }
            assert_eq!(reader.reads, before);
            assert!(
                proof
                    .visit_source(&source(bundle.clone(), None, 42, 1), |_| Ok(()))
                    .is_err()
            );
            assert!(
                proof
                    .visit_source(&source(bundle.clone(), Some(257), 42, 1), |_| Ok(()))
                    .is_err()
            );
            assert!(
                proof
                    .visit_source(&source(bundle.clone(), Some(0), 41, 1), |_| Ok(()))
                    .is_err()
            );
            assert!(
                proof
                    .visit_source(&source(bundle.clone(), Some(0), 42, 2), |_| Ok(()))
                    .is_err()
            );
            let wrong = SourceBundle::new(snapshot('c', 1), BundleFormat::Eml, Some(0));
            assert!(
                proof
                    .visit_source(&source(wrong, Some(0), 42, 1), |_| Ok(()))
                    .is_err()
            );
            let wrong_format =
                SourceBundle::new(bundle.manifest().clone(), BundleFormat::Mboxrd, Some(0));
            assert!(
                proof
                    .visit_source(&source(wrong_format, Some(0), 42, 1), |_| Ok(()))
                    .is_err()
            );
        });
    }

    #[test]
    fn mbox_groups_preserve_order_and_empty_ranges() {
        run_build(BundleFormat::Mboxrd, |proof, reader, bundle| {
            let before = reader.reads;
            for (ordinal, start, count) in [(2, 0, 129), (0, 1, 128)] {
                let mut index = start;
                proof
                    .visit_source(&source(bundle.clone(), Some(ordinal), 42, count), |d| {
                        assert_eq!(d.record_offset, u64::MAX - index);
                        assert_eq!(d.encoded_bytes, u64::MAX - 1);
                        assert_eq!(d.framing_lf_added, start == 0);
                        index += 2;
                        Ok(())
                    })
                    .unwrap();
            }
            proof
                .visit_source(&source(bundle.clone(), Some(1), 0, 0), |_| {
                    panic!("empty range")
                })
                .unwrap();
            assert_eq!(reader.reads, before);
        });
    }

    #[test]
    fn invalid_range_and_truncated_descriptor_fail() {
        let bundle = SourceBundle::new(snapshot('a', 1), BundleFormat::Eml, Some(0));
        for (start, count, descriptor_len) in [(u64::MAX, 1, 57), (0, 1, 56)] {
            let mut slot = Vec::new();
            for v in [start, count, 42] {
                put_u64(&mut slot, v).unwrap();
            }
            let mut proof = ManifestProof {
                identity: bundle.manifest().clone(),
                format: BundleFormat::Eml,
                files: 1,
                records: 1,
                lookup: Box::new(io::Cursor::new(slot)),
                descriptors: Box::new(io::Cursor::new(vec![0; descriptor_len])),
            };
            assert!(
                proof
                    .visit_source(&source(bundle.clone(), Some(0), 42, 1), |_| Ok(()))
                    .is_err()
            );
        }
    }

    #[test]
    fn captured_snapshot_is_retained_after_success_and_cancellation() {
        let tmp = sb_test_support::ScratchDir::new("proof-capture");
        let resources = crate::Resources::open(&tmp.path().join("store")).unwrap();
        let path = tmp.path().join("manifest.json");
        std::fs::write(&path, manifest(3, BundleFormat::Eml)).unwrap();
        let snapshot = match resources
            .store
            .capture_import_source(&path, |_| ControlFlow::Continue(()))
            .unwrap()
        {
            sb_store::ImportSourceCapture::Captured { snapshot, .. } => snapshot,
            _ => panic!("capture cancelled"),
        };
        let bundle = SourceBundle::new(snapshot.clone(), BundleFormat::Eml, Some(0));
        assert!(matches!(
            resources
                .with_manifest_proof(
                    &bundle,
                    |_| ControlFlow::Continue(()),
                    |proof| {
                        proof.visit_source(&source(bundle.clone(), Some(0), 42, 1), |_| Ok(()))
                    }
                )
                .unwrap(),
            Scoped::Done(())
        ));
        let mut calls = 0;
        assert!(matches!(
            resources
                .with_manifest_proof(
                    &bundle,
                    |_| {
                        calls += 1;
                        if calls >= 3 {
                            ControlFlow::Break(())
                        } else {
                            ControlFlow::Continue(())
                        }
                    },
                    |_| -> io::Result<()> { panic!("cancelled proof escaped") }
                )
                .unwrap(),
            Scoped::Stopped
        ));
        assert!(resources.store.open_import_source(&snapshot).is_ok());
        let bad_snapshot =
            ImportSourceSnapshot::from_metadata(snapshot.token(), [0; 32], snapshot.byte_length())
                .unwrap();
        let bad = SourceBundle::new(bad_snapshot, BundleFormat::Eml, Some(0));
        assert!(
            resources
                .with_manifest_proof(&bad, |_| ControlFlow::Continue(()), |_| Ok(()))
                .is_err()
        );
        assert!(resources.store.open_import_source(&snapshot).is_ok());
        assert_eq!(
            std::fs::read_dir(tmp.path().join("store/objects/tmp/import-validation"))
                .unwrap()
                .count(),
            0
        );
    }

    #[test]
    fn scratch_write_fault_fails_and_cleans_workspace() {
        use sb_test_support::{FaultFs, FaultKind, FsOp, real_fs};
        use std::sync::Arc;
        let tmp = sb_test_support::ScratchDir::new("proof-write-fault");
        let root = tmp.path().join("store");
        drop(crate::Resources::open(&root).unwrap());
        let probe = Arc::new(FaultFs::new(
            real_fs(),
            FsOp::Write,
            u64::MAX,
            FaultKind::Eio,
        ));
        let resources =
            crate::Resources::open_on(&root, probe.clone(), Arc::new(crate::SystemClock)).unwrap();
        let opening_calls = probe.calls(FsOp::Write);
        drop(resources);
        let fs = Arc::new(FaultFs::new(
            real_fs(),
            FsOp::Write,
            opening_calls + 1,
            FaultKind::Eio,
        ));
        let resources = crate::Resources::open_on(&root, fs, Arc::new(crate::SystemClock)).unwrap();
        let scratch = resources.store.import_validation_scratch().unwrap();
        let input = manifest(3, BundleFormat::Eml);
        assert!(
            build(
                &mut io::Cursor::new(input.as_bytes()),
                &scratch,
                BundleFormat::Eml
            )
            .is_err()
        );
        drop(scratch);
        assert_eq!(
            std::fs::read_dir(root.join("objects/tmp/import-validation"))
                .unwrap()
                .count(),
            0
        );
    }

    fn captured_bundle(
        resources: &crate::Resources,
        root: &Path,
        index: usize,
        format: BundleFormat,
        ordinal: Option<u64>,
    ) -> SourceBundle {
        captured_bundle_with_count(resources, root, index, 1, format, ordinal)
    }

    fn captured_bundle_with_count(
        resources: &crate::Resources,
        root: &Path,
        index: usize,
        count: usize,
        format: BundleFormat,
        ordinal: Option<u64>,
    ) -> SourceBundle {
        let path = root.join(format!("manifest-{index}.json"));
        std::fs::write(&path, manifest(count, format)).unwrap();
        let snapshot = match resources
            .store
            .capture_import_source(&path, |_| ControlFlow::Continue(()))
            .unwrap()
        {
            sb_store::ImportSourceCapture::Captured { snapshot, .. } => snapshot,
            _ => panic!("capture cancelled"),
        };
        SourceBundle::new(snapshot, format, ordinal)
    }

    fn expected_descriptor() -> ManifestDescriptor {
        ManifestDescriptor {
            sha256: [0xab; 32],
            original_bytes: 42,
            record_offset: 0,
            encoded_bytes: 0,
            framing_lf_added: false,
        }
    }

    fn done<T>(scoped: Scoped<T>) -> T {
        match scoped {
            Scoped::Done(value) => value,
            Scoped::Stopped => panic!("unexpected stop"),
        }
    }

    fn shuffled_order(len: usize) -> Vec<usize> {
        let mut order: Vec<usize> = (0..len).collect();
        let mut state = 0x9e37_79b9_7f4a_7c15u64;
        for index in (1..len).rev() {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            let swap = (state >> 33) as usize % (index + 1);
            order.swap(index, swap);
        }
        order
    }

    fn reopen_with_fault(
        root: &Path,
        op: FsOp,
        kind: FaultKind,
    ) -> (crate::Resources, Arc<FaultFs>) {
        let probe = Arc::new(FaultFs::new(real_fs(), op, u64::MAX, kind));
        let resources =
            crate::Resources::open_on(root, probe.clone(), Arc::new(crate::SystemClock)).unwrap();
        let opening_calls = probe.calls(op);
        drop(resources);
        let fs = Arc::new(FaultFs::new(real_fs(), op, opening_calls + 1, kind));
        let resources =
            crate::Resources::open_on(root, fs.clone(), Arc::new(crate::SystemClock)).unwrap();
        assert_eq!(fs.calls(op), opening_calls);
        (resources, fs)
    }

    #[test]
    fn registry_reuses_frozen_proofs_for_shuffled_sources() {
        let tmp = ScratchDir::new("manifest-proof-registry-shuffled");
        let resources = crate::Resources::open(&tmp.path().join("store")).unwrap();
        let registry = resources.manifest_proof_registry().unwrap();
        let manifests = 33;
        let files = 8;
        let count = manifests * files;
        let mut manifest_bundles = Vec::new();
        let mut sources = Vec::new();
        for manifest in 0..manifests {
            let bundle = captured_bundle_with_count(
                &resources,
                tmp.path(),
                manifest,
                files,
                BundleFormat::Eml,
                Some(0),
            );
            for file in 0..files {
                let source = source(bundle.clone(), Some(file as u64), 42, 1);
                let visited = registry
                    .with_proof(
                        &resources,
                        &bundle,
                        |_| ControlFlow::Continue(()),
                        |proof| {
                            let mut descriptors = Vec::new();
                            proof.visit_source(&source, |descriptor| {
                                descriptors.push(descriptor);
                                Ok(())
                            })?;
                            Ok(descriptors)
                        },
                    )
                    .unwrap();
                assert_eq!(done(visited), vec![expected_descriptor()]);
                sources.push(source);
            }
            manifest_bundles.push(bundle);
        }
        let snapshots = manifest_bundles
            .iter()
            .map(|bundle| bundle.manifest().clone())
            .collect::<Vec<_>>();
        resources.remove_import_sources(&snapshots).unwrap();
        for _ in 0..1 {
            for &index in &shuffled_order(count) {
                let manifest = index / files;
                let visited = registry
                    .with_proof(
                        &resources,
                        &manifest_bundles[manifest],
                        |_| ControlFlow::Continue(()),
                        |proof| {
                            let mut descriptors = Vec::new();
                            proof.visit_source(&sources[index], |descriptor| {
                                descriptors.push(descriptor);
                                Ok(())
                            })?;
                            Ok(descriptors)
                        },
                    )
                    .unwrap();
                assert_eq!(done(visited), vec![expected_descriptor()]);
            }
        }
    }

    #[test]
    fn registry_conflicting_identity_is_rejected() {
        let tmp = ScratchDir::new("manifest-proof-registry-conflict");
        let resources = crate::Resources::open(&tmp.path().join("store")).unwrap();
        let registry = resources.manifest_proof_registry().unwrap();
        let bundle = captured_bundle(&resources, tmp.path(), 0, BundleFormat::Eml, Some(0));
        let good = source(bundle.clone(), Some(0), 42, 1);
        let visited = registry
            .with_proof(
                &resources,
                &bundle,
                |_| ControlFlow::Continue(()),
                |proof| proof.visit_source(&good, |_| Ok(())),
            )
            .unwrap();
        assert!(matches!(visited, Scoped::Done(())));
        let snapshot = bundle.manifest();
        let conflicts = [
            SourceBundle::new(
                ImportSourceSnapshot::from_metadata(
                    snapshot.token(),
                    [0; 32],
                    snapshot.byte_length(),
                )
                .unwrap(),
                BundleFormat::Eml,
                Some(0),
            ),
            SourceBundle::new(
                ImportSourceSnapshot::from_metadata(
                    snapshot.token(),
                    snapshot.sha256(),
                    snapshot.byte_length() + 1,
                )
                .unwrap(),
                BundleFormat::Eml,
                Some(0),
            ),
            SourceBundle::new(snapshot.clone(), BundleFormat::Mboxrd, Some(0)),
        ];
        for conflicting in conflicts {
            let result = registry.with_proof(
                &resources,
                &conflicting,
                |_| ControlFlow::Continue(()),
                |_| Ok(()),
            );
            assert!(result.is_err());
        }
    }

    #[test]
    fn registry_rejects_missing_ordinal_and_range() {
        let tmp = ScratchDir::new("manifest-proof-registry-range");
        let resources = crate::Resources::open(&tmp.path().join("store")).unwrap();
        let registry = resources.manifest_proof_registry().unwrap();
        let bundle = captured_bundle(&resources, tmp.path(), 0, BundleFormat::Eml, Some(0));
        let good = source(bundle.clone(), Some(0), 42, 1);
        let visited = registry
            .with_proof(
                &resources,
                &bundle,
                |_| ControlFlow::Continue(()),
                |proof| proof.visit_source(&good, |_| Ok(())),
            )
            .unwrap();
        assert!(matches!(visited, Scoped::Done(())));
        for bad in [
            source(bundle.clone(), None, 42, 1),
            source(bundle.clone(), Some(1), 42, 1),
            source(bundle.clone(), Some(0), 41, 1),
            source(bundle.clone(), Some(0), 42, 2),
        ] {
            let result = registry.with_proof(
                &resources,
                &bundle,
                |_| ControlFlow::Continue(()),
                |proof| proof.visit_source(&bad, |_| Ok(())),
            );
            assert!(result.is_err());
        }
    }

    #[test]
    fn registry_truncated_workspace_fails_closed() {
        let tmp = ScratchDir::new("manifest-proof-registry-truncated");
        let resources = crate::Resources::open(&tmp.path().join("store")).unwrap();
        let snapshot = ImportSourceSnapshot::from_metadata(&"a".repeat(32), [1; 32], 100).unwrap();
        let bundle = SourceBundle::new(snapshot.clone(), BundleFormat::Eml, Some(0));
        let header = RegistryHeader {
            token: snapshot.token().to_owned(),
            sha256: snapshot.sha256(),
            byte_length: snapshot.byte_length(),
            format: BundleFormat::Eml,
            files: 1,
            records: 1,
            descriptors_len: DESCRIPTOR_SIZE,
            lookup_len: SLOT_SIZE,
        };
        let scratch = resources.store.import_validation_scratch().unwrap();
        scratch.create_registry_dir(&snapshot).unwrap();
        let mut descriptors = scratch
            .create_registry_file(&snapshot, RegistryFile::Descriptors)
            .unwrap();
        descriptors.write_all(&[0; 56]).unwrap();
        descriptors.sync_all().unwrap();
        drop(descriptors);
        let mut lookup = scratch
            .create_registry_file(&snapshot, RegistryFile::Lookup)
            .unwrap();
        for value in [0u64, 1u64, 100u64] {
            lookup.write_all(&value.to_le_bytes()).unwrap();
        }
        lookup.sync_all().unwrap();
        drop(lookup);
        assert!(ManifestProof::from_registry(&scratch, &bundle, header.clone()).is_err());

        scratch.remove_registry_dir(&snapshot).unwrap();
        scratch.create_registry_dir(&snapshot).unwrap();
        let mut descriptors = scratch
            .create_registry_file(&snapshot, RegistryFile::Descriptors)
            .unwrap();
        descriptors
            .write_all(&[0; DESCRIPTOR_SIZE as usize])
            .unwrap();
        descriptors.sync_all().unwrap();
        drop(descriptors);
        let mut lookup = scratch
            .create_registry_file(&snapshot, RegistryFile::Lookup)
            .unwrap();
        lookup.write_all(&[0; 23]).unwrap();
        lookup.sync_all().unwrap();
        drop(lookup);
        assert!(ManifestProof::from_registry(&scratch, &bundle, header.clone()).is_err());

        scratch.remove_registry_dir(&snapshot).unwrap();
        scratch.create_registry_dir(&snapshot).unwrap();
        let mut header_file = scratch
            .create_registry_file(&snapshot, RegistryFile::Header)
            .unwrap();
        header_file
            .write_all(&[0; HEADER_SIZE as usize + 1])
            .unwrap();
        header_file.sync_all().unwrap();
        drop(header_file);
        let registry = ManifestProofRegistry::new(scratch);
        assert!(registry.read_header(&bundle).is_err());
        assert!(
            registry
                .with_proof(
                    &resources,
                    &bundle,
                    |_| ControlFlow::Continue(()),
                    |_| Ok(())
                )
                .is_err()
        );

        let scratch = resources.store.import_validation_scratch().unwrap();
        scratch.create_registry_dir(&snapshot).unwrap();
        let mut descriptors = scratch
            .create_registry_file(&snapshot, RegistryFile::Descriptors)
            .unwrap();
        descriptors
            .write_all(&[0; DESCRIPTOR_SIZE as usize])
            .unwrap();
        descriptors.sync_all().unwrap();
        drop(descriptors);
        let mut lookup = scratch
            .create_registry_file(&snapshot, RegistryFile::Lookup)
            .unwrap();
        for value in [0u64, 1u64, 100u64] {
            lookup.write_all(&value.to_le_bytes()).unwrap();
        }
        lookup.sync_all().unwrap();
        drop(lookup);
        let mut bad_header = header.clone();
        bad_header.descriptors_len += 1;
        assert!(ManifestProof::from_registry(&scratch, &bundle, bad_header).is_err());
        scratch.finish().unwrap();
    }

    #[test]
    fn registry_incomplete_namespace_fails_closed() {
        let tmp = ScratchDir::new("manifest-proof-registry-incomplete");
        let resources = crate::Resources::open(&tmp.path().join("store")).unwrap();
        let snapshot = ImportSourceSnapshot::from_metadata(&"b".repeat(32), [2; 32], 100).unwrap();
        let bundle = SourceBundle::new(snapshot.clone(), BundleFormat::Eml, Some(0));
        let scratch = resources.store.import_validation_scratch().unwrap();
        scratch.create_registry_dir(&snapshot).unwrap();
        let mut descriptors = scratch
            .create_registry_file(&snapshot, RegistryFile::Descriptors)
            .unwrap();
        descriptors
            .write_all(&[0; DESCRIPTOR_SIZE as usize])
            .unwrap();
        descriptors.sync_all().unwrap();
        drop(descriptors);
        let registry = ManifestProofRegistry::new(scratch);
        assert!(
            registry
                .with_proof(
                    &resources,
                    &bundle,
                    |_| ControlFlow::Continue(()),
                    |_| Ok(())
                )
                .is_err()
        );
    }

    #[test]
    fn registry_cancellation_leaves_no_ready_proof() {
        let tmp = ScratchDir::new("manifest-proof-registry-cancel");
        let resources = crate::Resources::open(&tmp.path().join("store")).unwrap();
        let registry = resources.manifest_proof_registry().unwrap();
        let bundle = captured_bundle(&resources, tmp.path(), 0, BundleFormat::Eml, Some(0));
        let mut calls = 0;
        let result = registry.with_proof(
            &resources,
            &bundle,
            |_| {
                calls += 1;
                if calls >= 3 {
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            },
            |_| -> io::Result<()> { panic!("cancelled proof escaped") },
        );
        assert!(matches!(result.unwrap(), Scoped::Stopped));
        assert!(registry.read_header(&bundle).unwrap().is_none());
        registry
            .scratch
            .create_registry_dir(bundle.manifest())
            .unwrap();
        registry
            .scratch
            .remove_registry_dir(bundle.manifest())
            .unwrap();
    }

    #[test]
    fn registry_cancelled_reuse_never_invokes_the_consumer() {
        let tmp = ScratchDir::new("manifest-proof-cancel-reuse");
        let resources = crate::Resources::open(&tmp.path().join("store")).unwrap();
        let registry = resources.manifest_proof_registry().unwrap();
        let bundle = captured_bundle(&resources, tmp.path(), 0, BundleFormat::Eml, Some(0));
        done(
            registry
                .with_proof(
                    &resources,
                    &bundle,
                    |_| ControlFlow::Continue(()),
                    |_| Ok(()),
                )
                .unwrap(),
        );
        let result = registry.with_proof(
            &resources,
            &bundle,
            |_| ControlFlow::Break(()),
            |_| -> io::Result<()> { panic!("cancelled cached proof escaped") },
        );
        assert!(matches!(result.unwrap(), Scoped::Stopped));
        assert!(registry.read_header(&bundle).unwrap().is_some());
    }

    #[test]
    fn registry_cancellation_during_freeze_removes_the_namespace() {
        let tmp = ScratchDir::new("manifest-proof-cancel-freeze");
        let resources = crate::Resources::open(&tmp.path().join("store")).unwrap();
        let registry = resources.manifest_proof_registry().unwrap();
        let bundle = captured_bundle(&resources, tmp.path(), 0, BundleFormat::Eml, Some(0));
        let mut observed_copy = false;
        let result = registry.with_proof(
            &resources,
            &bundle,
            |_| {
                if let Ok(mut file) = registry
                    .scratch
                    .open_registry_file(bundle.manifest(), RegistryFile::Descriptors)
                    && file.seek(SeekFrom::End(0)).unwrap() > 0
                {
                    observed_copy = true;
                    ControlFlow::Break(())
                } else {
                    ControlFlow::Continue(())
                }
            },
            |_| -> io::Result<()> { panic!("cancelled frozen proof escaped") },
        );
        assert!(matches!(result.unwrap(), Scoped::Stopped));
        assert!(observed_copy);
        assert!(registry.read_header(&bundle).unwrap().is_none());
        registry
            .scratch
            .create_registry_dir(bundle.manifest())
            .unwrap();
        registry
            .scratch
            .remove_registry_dir(bundle.manifest())
            .unwrap();
    }

    #[test]
    fn registry_output_copy_checks_cancellation_between_chunks() {
        let tmp = ScratchDir::new("manifest-proof-cancel-chunk");
        let resources = crate::Resources::open(&tmp.path().join("store")).unwrap();
        let scratch = resources.store.import_validation_scratch().unwrap();
        let mut writer = scratch.create_file(ValidationFile::Descriptors).unwrap();
        writer.write_all(&vec![1; 3 * REGISTRY_COPY_CHUNK]).unwrap();
        drop(writer);
        let mut source = scratch.open_file(ValidationFile::Descriptors).unwrap();
        let mut target = scratch.create_file(ValidationFile::Lookup).unwrap();
        let mut checks = 0;
        let result = copy_bounded(&mut *source, &mut *target, &mut || {
            checks += 1;
            if checks == 2 {
                ControlFlow::Break(())
            } else {
                ControlFlow::Continue(())
            }
        })
        .unwrap();
        assert!(matches!(result, Scoped::Stopped));
        drop(target);
        let mut output = scratch.open_file(ValidationFile::Lookup).unwrap();
        assert_eq!(
            output.seek(SeekFrom::End(0)).unwrap(),
            REGISTRY_COPY_CHUNK as u64
        );
        drop(output);
        drop(source);
        scratch.finish().unwrap();
    }

    #[test]
    fn registry_production_faults_leave_no_ready_proof() {
        for op in [FsOp::Read, FsOp::Write, FsOp::Seek, FsOp::SyncFile] {
            let tmp = ScratchDir::new("manifest-proof-registry-fault");
            let root = tmp.path().join("store");
            let resources = crate::Resources::open(&root).unwrap();
            let bundle = captured_bundle(&resources, tmp.path(), 0, BundleFormat::Eml, Some(0));
            drop(resources);
            let (resources, fault) = reopen_with_fault(&root, op, FaultKind::Eio);
            let registry = resources.manifest_proof_registry().unwrap();
            let result = registry.with_proof(
                &resources,
                &bundle,
                |_| ControlFlow::Continue(()),
                |_| Ok(()),
            );
            assert!(result.is_err(), "{op:?}");
            assert!(registry.read_header(&bundle).unwrap().is_none(), "{op:?}");
            assert!(fault.calls(op) > 0, "{op:?}");
            registry
                .scratch
                .create_registry_dir(bundle.manifest())
                .unwrap();
            registry
                .scratch
                .remove_registry_dir(bundle.manifest())
                .unwrap();
        }
    }

    #[test]
    fn registry_reuse_opens_only_fixed_handles() {
        let tmp = ScratchDir::new("manifest-proof-registry-handles");
        let root = tmp.path().join("store");
        let resources = crate::Resources::open(&root).unwrap();
        let bundle = captured_bundle(&resources, tmp.path(), 0, BundleFormat::Eml, Some(0));
        let source = source(bundle.clone(), Some(0), 42, 1);
        drop(resources);
        let probe = Arc::new(FaultFs::new(
            real_fs(),
            FsOp::Open,
            u64::MAX,
            FaultKind::Eio,
        ));
        let resources =
            crate::Resources::open_on(&root, probe.clone(), Arc::new(crate::SystemClock)).unwrap();
        let registry = resources.manifest_proof_registry().unwrap();
        let visited = registry
            .with_proof(
                &resources,
                &bundle,
                |_| ControlFlow::Continue(()),
                |proof| proof.visit_source(&source, |_| Ok(())),
            )
            .unwrap();
        assert!(matches!(visited, Scoped::Done(())));
        let opens = probe.calls(FsOp::Open);
        let visited = registry
            .with_proof(
                &resources,
                &bundle,
                |_| ControlFlow::Continue(()),
                |proof| proof.visit_source(&source, |_| Ok(())),
            )
            .unwrap();
        assert!(matches!(visited, Scoped::Done(())));
        assert_eq!(probe.calls(FsOp::Open) - opens, 3);
    }

    #[test]
    fn registry_capability_is_send_and_static() {
        fn assert_send<T: Send + 'static>() {}
        assert_send::<ManifestProofRegistry>();
        assert_send::<Arc<ManifestProofRegistry>>();
        let tmp = ScratchDir::new("manifest-proof-registry-send");
        let resources = crate::Resources::open(&tmp.path().join("store")).unwrap();
        let registry = resources.manifest_proof_registry().unwrap();
        std::thread::spawn(move || drop(registry)).join().unwrap();
    }

    #[test]
    fn registry_finish_removes_the_root_lease() {
        let tmp = ScratchDir::new("manifest-proof-registry-finish");
        let root = tmp.path().join("store");
        let resources = crate::Resources::open(&root).unwrap();
        let bundle = captured_bundle(&resources, tmp.path(), 0, BundleFormat::Eml, Some(0));
        let source = source(bundle.clone(), Some(0), 42, 1);
        let registry = resources.manifest_proof_registry().unwrap();
        let visited = registry
            .with_proof(
                &resources,
                &bundle,
                |_| ControlFlow::Continue(()),
                |proof| proof.visit_source(&source, |_| Ok(())),
            )
            .unwrap();
        assert!(matches!(visited, Scoped::Done(())));
        registry.finish().unwrap();
        let validation_root = root.join("objects/tmp/import-validation");
        assert_eq!(std::fs::read_dir(validation_root).unwrap().count(), 0);
    }

    #[test]
    fn registry_drop_removes_the_root_lease() {
        let tmp = ScratchDir::new("manifest-proof-registry-drop");
        let root = tmp.path().join("store");
        let resources = crate::Resources::open(&root).unwrap();
        let bundle = captured_bundle(&resources, tmp.path(), 0, BundleFormat::Eml, Some(0));
        let source = source(bundle.clone(), Some(0), 42, 1);
        let registry = resources.manifest_proof_registry().unwrap();
        let visited = registry
            .with_proof(
                &resources,
                &bundle,
                |_| ControlFlow::Continue(()),
                |proof| proof.visit_source(&source, |_| Ok(())),
            )
            .unwrap();
        assert!(matches!(visited, Scoped::Done(())));
        drop(registry);
        let validation_root = root.join("objects/tmp/import-validation");
        assert_eq!(std::fs::read_dir(validation_root).unwrap().count(), 0);
    }

    #[test]
    fn registry_leftovers_are_cleaned_at_startup() {
        let tmp = ScratchDir::new("manifest-proof-registry-startup");
        let root = tmp.path().join("store");
        let resources = crate::Resources::open(&root).unwrap();
        let bundle = captured_bundle(&resources, tmp.path(), 0, BundleFormat::Eml, Some(0));
        let source = source(bundle.clone(), Some(0), 42, 1);
        let registry = resources.manifest_proof_registry().unwrap();
        let visited = registry
            .with_proof(
                &resources,
                &bundle,
                |_| ControlFlow::Continue(()),
                |proof| proof.visit_source(&source, |_| Ok(())),
            )
            .unwrap();
        assert!(matches!(visited, Scoped::Done(())));
        std::mem::forget(registry);
        resources.close().unwrap();
        let validation_root = root.join("objects/tmp/import-validation");
        assert!(validation_root.exists());
        let resources = crate::Resources::open(&root).unwrap();
        assert!(!validation_root.exists());
        resources.close().unwrap();
    }
}
