//! Private manifest descriptors for a single inventoried file.

use std::io::{self, Read, Seek, SeekFrom};

use super::{BundleFormat, FileName, Parser, invalid, read_entry, same_name, validate_envelope};

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Descriptor {
    pub(crate) sha256: String,
    pub(crate) bytes: u64,
    pub(crate) framing: Option<Framing>,
}

#[derive(Debug, PartialEq, Eq)]
pub(crate) struct Framing {
    pub(crate) offset: u64,
    pub(crate) encoded_bytes: u64,
    pub(crate) framing_lf_added: bool,
}

pub(crate) struct Cursor<R> {
    reader: R,
    file: FileName,
    offset: u64,
    remaining: u64,
    count: u64,
    expected: u64,
    format: BundleFormat,
}

impl<R: Read + Seek> Cursor<R> {
    // Admission checked global relationships; reopening the same hashed snapshot
    // preserves that proof. This cursor rechecks the selected declaration and
    // each schema without repeating admission's relationship rescans. The public
    // guard still refuses reconstructed or legacy sources before publication.
    pub(crate) fn prepare(
        mut reader: R,
        ordinal: u64,
        format: BundleFormat,
        bytes: u64,
        count: u64,
    ) -> io::Result<Self> {
        reader.seek(SeekFrom::Start(0))?;
        let table = validate_envelope(&mut reader)?;
        if table.format != format || ordinal >= table.count {
            return Err(invalid());
        }
        let mut offset = table.start + 1;
        let mut selected = None;
        for _ in 0..=ordinal {
            let (entry, next) = read_entry(&mut reader, offset)?;
            selected = Some(entry);
            offset = next;
        }
        let file = selected.ok_or_else(invalid)?;
        if file.bytes != bytes
            || file.records != count
            || (format == BundleFormat::Eml && count != 1)
        {
            return Err(invalid());
        }
        Ok(Self {
            reader,
            file,
            offset: table.messages.0 + 1,
            remaining: table.messages.2,
            count: 0,
            expected: count,
            format,
        })
    }

    pub(crate) fn next(&mut self) -> io::Result<Option<Descriptor>> {
        while self.remaining > 0 {
            self.reader.seek(SeekFrom::Start(self.offset))?;
            let mut p = Parser {
                reader: &mut self.reader,
                next: None,
                capture: None,
                position: self.offset,
            };
            p.whitespace()?;
            if p.peek()? == Some(b',') {
                p.take()?;
                p.whitespace()?;
            }
            // Validate each schema before extracting its bounded descriptor.
            p.admitted_message(self.format)?;
            self.reader.seek(SeekFrom::Start(self.offset))?;
            let mut p = Parser {
                reader: &mut self.reader,
                next: None,
                capture: None,
                position: self.offset,
            };
            p.whitespace()?;
            if p.peek()? == Some(b',') {
                p.take()?;
                p.whitespace()?;
            }
            let mut file = None;
            let mut sha256 = None;
            let mut bytes = None;
            let mut offset = None;
            let mut encoded = None;
            let mut original = None;
            let mut framing = None;
            p.object(|p, key| {
                match key {
                    "file" => {
                        p.nullable(|p| {
                            file = Some(p.file_name()?);
                            Ok(())
                        })?;
                    }
                    "acquisition" => {
                        p.nullable(|p| {
                            p.object(|p, key| {
                                match key {
                                    "sha256" => sha256 = Some(p.hash()?),
                                    "bytes" => bytes = Some(p.integer::<u64>()?),
                                    _ => p.value(1)?,
                                }
                                Ok(())
                            })
                        })?;
                    }
                    "record_offset" => offset = Some(p.integer::<u64>()?),
                    "encoded_bytes" => encoded = Some(p.integer::<u64>()?),
                    "original_bytes" => original = Some(p.integer::<u64>()?),
                    "framing_lf_added" => {
                        framing = Some(match p.peek()? {
                            Some(b't') => {
                                p.literal(b"true")?;
                                true
                            }
                            Some(b'f') => {
                                p.literal(b"false")?;
                                false
                            }
                            _ => return Err(invalid()),
                        });
                    }
                    _ => p.value(1)?,
                }
                Ok(())
            })?;
            self.offset = p.position;
            self.remaining -= 1;
            let Some(file) = file else { continue };
            if file.digest != self.file.digest
                || !same_name(&mut self.reader, file.start, self.file.start)?
            {
                continue;
            }
            let bytes = bytes.ok_or_else(invalid)?;
            if self.format == BundleFormat::Eml && bytes != self.file.bytes {
                return Err(invalid());
            }
            let framing = match self.format {
                BundleFormat::Eml => None,
                BundleFormat::Mboxrd => {
                    if original != Some(bytes) {
                        return Err(invalid());
                    }
                    Some(Framing {
                        offset: offset.ok_or_else(invalid)?,
                        encoded_bytes: encoded.ok_or_else(invalid)?,
                        framing_lf_added: framing.ok_or_else(invalid)?,
                    })
                }
            };
            self.count = self.count.checked_add(1).ok_or_else(invalid)?;
            if self.count > self.expected {
                return Err(invalid());
            }
            return Ok(Some(Descriptor {
                sha256: sha256.ok_or_else(invalid)?,
                bytes,
                framing,
            }));
        }
        if self.count != self.expected {
            return Err(invalid());
        }
        Ok(None)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::{Value, json};
    use sha2::{Digest, Sha256};
    use std::io::Cursor as Memory;

    fn manifest(format: &str, files: Value, messages: Vec<Value>) -> Vec<u8> {
        let mut ids = Sha256::new();
        for message in &messages {
            ids.update(message["occurrence_id"].as_i64().unwrap().to_be_bytes());
        }
        let hash = ids
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        json!({"v":1,"kind":"export-manifest","job":"j","format":format,"destination":"/d",
            "scope":{"kind":"mailbox","id":1,"generation":1},"generation":1,
            "entries":messages.len(),"ids_sha256":hash,"files":files,"messages":messages})
        .to_string()
        .into_bytes()
    }

    fn eml(file: &str) -> Value {
        json!({"v":1,"occurrence_id":1,"message_id":1,"account":{"id":"a","name":"a"},
            "mailbox":{"id":1,"name":"m"},"occurrence_state":"present",
            "flags":{"seen":false,"flagged":false,"answered":false,"draft":false,"deleted":false},
            "keywords":[],"acquisition":{"id":1,"sha256":"a".repeat(64),"bytes":2},
            "body_availability":"acquired","tags":[],"note":null,"file":file})
    }

    #[test]
    fn eml_decodes_escaped_file_names_and_returns_hash() {
        let raw = manifest(
            "eml",
            json!([{"name":"é.eml","bytes":2,"records":1}]),
            vec![eml("é.eml")],
        );
        let raw = String::from_utf8(raw)
            .unwrap()
            .replacen("é.eml", "\\u00e9.eml", 1)
            .into_bytes();
        let mut cursor = Cursor::prepare(Memory::new(raw), 0, BundleFormat::Eml, 2, 1).unwrap();
        assert_eq!(
            cursor.next().unwrap(),
            Some(Descriptor {
                sha256: "a".repeat(64),
                bytes: 2,
                framing: None
            })
        );
        assert_eq!(cursor.next().unwrap(), None);
    }

    #[test]
    fn empty_mbox_and_invalid_declarations() {
        let raw = manifest(
            "mboxrd",
            json!([{"name":"empty.mbox","bytes":0,"records":0}]),
            vec![],
        );
        assert!(Cursor::prepare(Memory::new(&raw), 1, BundleFormat::Mboxrd, 0, 0).is_err());
        assert!(Cursor::prepare(Memory::new(&raw), 0, BundleFormat::Eml, 0, 0).is_err());
        assert!(Cursor::prepare(Memory::new(&raw), 0, BundleFormat::Mboxrd, 1, 0).is_err());
        let mut cursor = Cursor::prepare(Memory::new(raw), 0, BundleFormat::Mboxrd, 0, 0).unwrap();
        assert_eq!(cursor.next().unwrap(), None);
    }

    #[test]
    fn scanning_many_descriptors_reads_linearly() {
        use std::cell::Cell;
        use std::io::{Read, Seek, SeekFrom};
        use std::rc::Rc;

        struct Counted {
            inner: Memory<Vec<u8>>,
            bytes: Rc<Cell<usize>>,
        }
        impl Read for Counted {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                let read = self.inner.read(buf)?;
                self.bytes.set(self.bytes.get() + read);
                Ok(read)
            }
        }
        impl Seek for Counted {
            fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
                self.inner.seek(position)
            }
        }
        const RECORDS: usize = 1_000;
        const READ_MULTIPLE: usize = 8;
        let messages = (0..RECORDS)
            .map(|id| {
                let mut message = eml("a.mbox");
                message["occurrence_id"] = json!(id);
                message["record_offset"] = json!(id);
                message["encoded_bytes"] = json!(2);
                message["original_bytes"] = json!(2);
                message["framing_lf_added"] = json!(false);
                message
            })
            .collect();
        let raw = manifest(
            "mboxrd",
            json!([{"name":"a.mbox","bytes":2,"records":RECORDS}]),
            messages,
        );
        let len = raw.len();
        let bytes = Rc::new(Cell::new(0));
        let reader = Counted {
            inner: Memory::new(raw),
            bytes: bytes.clone(),
        };
        let mut cursor =
            Cursor::prepare(reader, 0, BundleFormat::Mboxrd, 2, RECORDS as u64).unwrap();
        for _ in 0..RECORDS {
            assert!(cursor.next().unwrap().is_some());
        }
        assert_eq!(cursor.next().unwrap(), None);
        assert!(
            bytes.get() < len * READ_MULTIPLE,
            "{} bytes read from {len}",
            bytes.get()
        );
    }

    #[test]
    fn tag_heavy_cursor_reads_linearly() {
        use std::cell::Cell;
        use std::rc::Rc;

        struct Counted {
            inner: Memory<Vec<u8>>,
            bytes: Rc<Cell<usize>>,
            limit: usize,
        }
        impl Read for Counted {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.bytes.get() >= self.limit {
                    return Err(io::Error::other("cursor read budget exceeded"));
                }
                let available = self.limit - self.bytes.get();
                let limit = buf.len().min(available);
                let read = self.inner.read(&mut buf[..limit])?;
                self.bytes.set(self.bytes.get() + read);
                Ok(read)
            }
        }
        impl Seek for Counted {
            fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
                self.inner.seek(position)
            }
        }

        let mut measurements = Vec::new();
        for count in [100, 200, 400, 800] {
            let mut message = eml("a.eml");
            message["tags"] = json!((0..count)
                .map(|id| json!({"id":format!("id-{id:04}"), "name":format!("tag-{id:04}"), "color_srgb":null}))
                .collect::<Vec<_>>());
            let raw = manifest(
                "eml",
                json!([{"name":"a.eml","bytes":2,"records":1}]),
                vec![message],
            );
            let mut admission = Memory::new(&raw);
            let table = super::super::validate_envelope(&mut admission).unwrap();
            super::super::validate_file_names(&mut admission, &table).unwrap();
            super::super::messages::validate_messages(&mut admission, &table).unwrap();
            let len = raw.len();
            let bytes = Rc::new(Cell::new(0));
            let reader = Counted {
                inner: Memory::new(raw),
                bytes: bytes.clone(),
                limit: len * 1024,
            };
            let mut cursor = Cursor::prepare(reader, 0, BundleFormat::Eml, 2, 1).unwrap();
            assert_eq!(
                cursor.next().unwrap(),
                Some(Descriptor {
                    sha256: "a".repeat(64),
                    bytes: 2,
                    framing: None,
                })
            );
            assert_eq!(cursor.next().unwrap(), None);
            measurements.push((count, len, bytes.get()));
        }
        eprintln!("tag count, manifest bytes, cursor bytes read: {measurements:?}");
        assert!(measurements.iter().all(|(_, len, read)| *read < len * 8));
    }

    #[test]
    fn declared_count_is_checked_at_exhaustion() {
        let mut message = eml("a.mbox");
        message["record_offset"] = json!(0);
        message["encoded_bytes"] = json!(2);
        message["original_bytes"] = json!(2);
        message["framing_lf_added"] = json!(false);
        let raw = manifest(
            "mboxrd",
            json!([{"name":"a.mbox","bytes":2,"records":2}]),
            vec![message],
        );
        let mut cursor = Cursor::prepare(Memory::new(raw), 0, BundleFormat::Mboxrd, 2, 2).unwrap();
        assert!(cursor.next().unwrap().is_some());
        assert!(cursor.next().is_err());
    }

    #[test]
    fn malformed_message_is_rejected() {
        let mut message = eml("a.eml");
        message["flags"]["seen"] = json!("not a boolean");
        let raw = manifest(
            "eml",
            json!([{"name":"a.eml","bytes":2,"records":1}]),
            vec![message],
        );
        let mut cursor = Cursor::prepare(Memory::new(raw), 0, BundleFormat::Eml, 2, 1).unwrap();
        assert!(cursor.next().is_err());

        let mut message = eml("a.eml");
        message["tags"] = json!([{"id":"x", "name":"a", "color_srgb":"bad"}]);
        let raw = manifest(
            "eml",
            json!([{"name":"a.eml","bytes":2,"records":1}]),
            vec![message],
        );
        let mut cursor = Cursor::prepare(Memory::new(raw), 0, BundleFormat::Eml, 2, 1).unwrap();
        assert!(cursor.next().is_err());
    }
}
