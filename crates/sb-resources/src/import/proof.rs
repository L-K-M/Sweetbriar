//! Callback-scoped, disposable proof for one captured bundle manifest.
use std::cell::{Cell, RefCell};
use std::io::{self, BufReader, Read, Seek, SeekFrom, Write};
use std::ops::ControlFlow;

use sb_store::{ImportSourceSnapshot, Scoped, ValidationFile, ValidationScratch};

use super::json_syntax;
use super::offset_sort::{OffsetSort, SortEntry, SortKind};
use super::{BundleFormat, SourceBundle, SourceInventory};

const DESCRIPTOR_SIZE: u64 = 57;
const ROW_SIZE: u64 = 32;
const SLOT_SIZE: u64 = 24;

/// The validated manifest's expected bytes and mbox framing for a message.
#[derive(Debug, Clone, PartialEq, Eq)]
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

fn store_error(error: sb_store::StoreError) -> io::Error {
    io::Error::new(error.io_kind().unwrap_or(io::ErrorKind::Other), error)
}

fn put_u64(writer: &mut impl Write, value: u64) -> io::Result<()> {
    writer.write_all(&value.to_le_bytes())
}

fn get_u64(reader: &mut impl Read) -> io::Result<u64> {
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

    fn read(reader: &mut impl Read) -> io::Result<Self> {
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
        if bundle.manifest() != &self.identity
            || bundle.format() != self.format
            || source.kind() != self.format.source_kind()
        {
            return Err(invalid());
        }
        let ordinal = bundle.file_ordinal().ok_or_else(invalid)?;
        if ordinal >= self.files {
            return Err(invalid());
        }
        self.lookup.seek(SeekFrom::Start(
            ordinal.checked_mul(SLOT_SIZE).ok_or_else(invalid)?,
        ))?;
        let start = get_u64(&mut self.lookup)?;
        let count = get_u64(&mut self.lookup)?;
        let bytes = get_u64(&mut self.lookup)?;
        if bytes != source.snapshot().byte_length()
            || count != source.message_count()
            || start
                .checked_add(count)
                .filter(|end| *end <= self.records)
                .is_none()
        {
            return Err(invalid());
        }
        self.descriptors.seek(SeekFrom::Start(
            start.checked_mul(DESCRIPTOR_SIZE).ok_or_else(invalid)?,
        ))?;
        for _ in 0..count {
            visit(ManifestDescriptor::read(&mut self.descriptors)?)?;
        }
        Ok(())
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
    scratch: &ValidationScratch<'_>,
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
    use sha2::{Digest, Sha256};

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
}
