//! Per-message validation on the verified, seekable snapshot.

use std::cmp::Ordering;
use std::io::{self, Read, Seek, SeekFrom};

use sha2::{Digest, Sha256};

use super::super::offset_sort::{SortEntry, SortKind};
use super::{BundleFormat, FileTable, OffsetSort, Parser, invalid, name_chunk};

pub(in crate::import) fn read_descriptor(
    reader: &mut (impl Read + Seek),
    offset: u64,
) -> io::Result<super::super::proof::ManifestDescriptor> {
    reader.seek(SeekFrom::Start(offset))?;
    let mut p = Parser {
        reader,
        next: None,
        capture: None,
        position: offset,
    };
    let mut sha = None;
    let mut bytes = None;
    let mut record_offset = 0;
    let mut encoded_bytes = 0;
    let mut framing_lf_added = false;
    p.object(|p, key| {
        match key {
            "acquisition" => {
                p.object(|p, key| {
                    match key {
                        "sha256" => sha = super::super::sha256_from_hex(&p.hash()?),
                        "bytes" => bytes = Some(p.integer::<u64>()?),
                        _ => p.value(1)?,
                    }
                    Ok(())
                })?;
            }
            "record_offset" => record_offset = p.integer()?,
            "encoded_bytes" => encoded_bytes = p.integer()?,
            "framing_lf_added" => {
                framing_lf_added = p.peek()? == Some(b't');
                p.boolean()?;
            }
            _ => p.value(1)?,
        }
        Ok(())
    })?;
    Ok(super::super::proof::ManifestDescriptor {
        sha256: sha.ok_or_else(invalid)?,
        original_bytes: bytes.ok_or_else(invalid)?,
        record_offset,
        encoded_bytes,
        framing_lf_added,
    })
}

enum Identity {
    Account,
    Mailbox,
}

#[cfg(test)]
pub(in crate::import) fn validate_messages(
    reader: &mut (impl Read + Seek),
    table: &FileTable,
    scratch: &sb_store::ValidationScratch,
) -> io::Result<()> {
    validate_messages_with_children(reader, table, scratch, &mut |_, _, _, _, _| Ok(()))
}

pub(in crate::import) fn validate_messages_with_children<R: Read + Seek>(
    reader: &mut R,
    table: &FileTable,
    scratch: &sb_store::ValidationScratch,
    child: &mut impl FnMut(&mut R, u64, u64, u64, u64) -> io::Result<()>,
) -> io::Result<()> {
    validate_messages_with_references(reader, table, scratch, child, &mut |_, _, _, _| Ok(()))
}

pub(in crate::import) fn validate_messages_with_references<R: Read + Seek>(
    reader: &mut R,
    table: &FileTable,
    scratch: &sb_store::ValidationScratch,
    child: &mut impl FnMut(&mut R, u64, u64, u64, u64) -> io::Result<()>,
    reference: &mut impl FnMut(&mut R, u64, u64, u64) -> io::Result<()>,
) -> io::Result<()> {
    let (start, end, count) = table.messages;
    if count != table.entries {
        return Err(invalid());
    }
    reader.seek(SeekFrom::Start(start + 1))?;
    let mut p = Parser {
        reader: &mut *reader,
        next: None,
        capture: None,
        position: start + 1,
    };
    let mut digest = Sha256::new();
    for index in 0..count {
        p.whitespace()?;
        if index != 0 {
            p.expect(b',')?;
            p.whitespace()?;
        }
        digest.update(p.message(table.format, scratch)?.to_be_bytes());
    }
    p.whitespace()?;
    p.expect(b']')?;
    let hash = digest.finalize();
    if p.position != end || super::super::sha256_from_hex(&table.ids_hash) != Some(hash.into()) {
        return Err(invalid());
    }
    unique_occurrences(reader, table, scratch)?;
    validate_file_relationships_with_references(reader, table, scratch, child, reference)
}

// The schema and occurrence passes have released both tapes before this pass.
#[cfg(test)]
fn validate_file_relationships<R: Read + Seek>(
    reader: &mut R,
    table: &FileTable,
    scratch: &sb_store::ValidationScratch,
    child: &mut impl FnMut(&mut R, u64, u64, u64, u64) -> io::Result<()>,
) -> io::Result<()> {
    validate_file_relationships_with_references(reader, table, scratch, child, &mut |_, _, _, _| {
        Ok(())
    })
}

fn validate_file_relationships_with_references<R: Read + Seek>(
    reader: &mut R,
    table: &FileTable,
    scratch: &sb_store::ValidationScratch,
    child: &mut impl FnMut(&mut R, u64, u64, u64, u64) -> io::Result<()>,
    reference: &mut impl FnMut(&mut R, u64, u64, u64) -> io::Result<()>,
) -> io::Result<()> {
    reader.seek(SeekFrom::Start(table.start + 1))?;
    let mut p = Parser {
        reader,
        next: None,
        capture: None,
        position: table.start + 1,
    };
    let mut sorter = OffsetSort::new(scratch)?;
    let mut compare = |p: &mut Parser<_>, a: SortEntry, b: SortEntry| {
        Ok(p.compare(a.key, b.key)?
            .then(a.kind.cmp(&b.kind))
            .then(a.ordinal.cmp(&b.ordinal)))
    };
    for index in 0..table.count {
        p.whitespace()?;
        if index != 0 {
            p.expect(b',')?;
            p.whitespace()?;
        }
        let file = p.file_entry()?;
        sorter.push_entry(
            SortEntry {
                key: file.start,
                position: file.bytes,
                extra: file.records,
                ordinal: index,
                message_offset: 0,
                kind: SortKind::Declaration,
            },
            &mut p,
            &mut compare,
        )?;
    }
    p.whitespace()?;
    p.expect(b']')?;
    if p.position != table.end {
        return Err(invalid());
    }

    let (start, end, count) = table.messages;
    p.restore(start + 1)?;
    for index in 0..count {
        p.whitespace()?;
        if index != 0 {
            p.expect(b',')?;
            p.whitespace()?;
        }
        let message_offset = p.position;
        let mut name = None;
        let mut occurrence = None;
        let mut bytes = None;
        p.object(|p, key| {
            match key {
                "file" => {
                    p.nullable(|p| {
                        name = Some(p.marked_string()?);
                        Ok(())
                    })?;
                }
                "occurrence_id" => occurrence = Some(p.integer::<i64>()?),
                "acquisition" => {
                    p.nullable(|p| {
                        bytes = Some(p.acquisition()?);
                        Ok(())
                    })?;
                }
                _ => p.value(1)?,
            }
            Ok(())
        })?;
        if let Some(key) = name {
            let bytes = bytes.ok_or_else(invalid)?;
            if table.format == BundleFormat::Eml {
                let expected = format!("{}.eml", occurrence.ok_or_else(invalid)?);
                let resume = p.position;
                let mut at = key;
                let mut chars = ['\0'; 32];
                let length = name_chunk(&mut p.reader, &mut at, &mut chars)?;
                let matches =
                    length < chars.len() && chars[..length].iter().copied().eq(expected.chars());
                p.restore(resume)?;
                if !matches {
                    return Err(invalid());
                }
            }
            sorter.push_entry(
                SortEntry {
                    key,
                    position: bytes,
                    extra: 0,
                    ordinal: index,
                    message_offset,
                    kind: SortKind::Reference,
                },
                &mut p,
                &mut compare,
            )?;
        }
    }
    p.whitespace()?;
    p.expect(b']')?;
    if p.position != end {
        return Err(invalid());
    }

    let mut previous = None;
    let mut declaration = None;
    let mut references = 0u64;
    let check_group = |declaration: Option<(u64, u64, u64)>, references: u64| -> io::Result<()> {
        let (_, records, _) = declaration.ok_or_else(invalid)?;
        if records != references || (table.format == BundleFormat::Eml && references != 1) {
            return Err(invalid());
        }
        Ok(())
    };
    sorter.finish_entries(
        &mut p,
        &mut compare,
        &mut |p| p.reader.stream_position().map(|_| ()),
        |p, entry| {
            if let Some(last) = previous
                && p.compare(last, entry.key)? != Ordering::Equal
            {
                check_group(declaration, references)?;
                let (bytes, records, ordinal) = declaration.ok_or_else(invalid)?;
                let resume = p.position;
                child(p.reader, last, bytes, records, ordinal)?;
                p.restore(resume)?;
                declaration = None;
                references = 0;
            }
            match entry.kind {
                SortKind::Declaration => {
                    if declaration
                        .replace((entry.position, entry.extra, entry.ordinal))
                        .is_some()
                    {
                        return Err(invalid());
                    }
                }
                SortKind::Reference => {
                    references = references.checked_add(1).ok_or_else(invalid)?;
                    if table.format == BundleFormat::Eml
                        && declaration.ok_or_else(invalid)?.0 != entry.position
                    {
                        return Err(invalid());
                    }
                    let (_, _, file_ordinal) = declaration.ok_or_else(invalid)?;
                    let resume = p.position;
                    reference(p.reader, entry.message_offset, entry.ordinal, file_ordinal)?;
                    p.restore(resume)?;
                }
                SortKind::Offset => return Err(invalid()),
            }
            previous = Some(entry.key);
            Ok(())
        },
    )?;
    if let Some(last) = previous {
        check_group(declaration, references)?;
        let (bytes, records, ordinal) = declaration.ok_or_else(invalid)?;
        child(p.reader, last, bytes, records, ordinal)?;
    }
    Ok(())
}

// The schema pass has finished all tag sorts before this sorter takes the tapes.
fn unique_occurrences(
    reader: &mut (impl Read + Seek),
    table: &FileTable,
    scratch: &sb_store::ValidationScratch,
) -> io::Result<()> {
    let (start, end, count) = table.messages;
    reader.seek(SeekFrom::Start(start + 1))?;
    let mut p = Parser {
        reader,
        next: None,
        capture: None,
        position: start + 1,
    };
    let mut sorter = OffsetSort::new(scratch)?;
    let mut compare = |_: &mut Parser<_>, a: u64, b: u64| Ok(a.cmp(&b));
    for index in 0..count {
        p.whitespace()?;
        if index != 0 {
            p.expect(b',')?;
            p.whitespace()?;
        }
        let mut occurrence = None;
        p.object(|p, key| {
            if key == "occurrence_id" {
                if occurrence.is_some() {
                    return Err(invalid());
                }
                occurrence = Some(p.integer::<i64>()?);
            } else {
                p.value(1)?;
            }
            Ok(())
        })?;
        // Flipping the sign bit maps signed order to unsigned order exactly.
        let key = (occurrence.ok_or_else(invalid)? as u64) ^ (1u64 << 63);
        sorter.push(key, &mut p, &mut compare)?;
    }
    p.whitespace()?;
    p.expect(b']')?;
    if p.position != end {
        return Err(invalid());
    }
    let mut previous = None;
    sorter.finish(
        &mut p,
        &mut compare,
        &mut |p| p.reader.stream_position().map(|_| ()),
        |_, id| {
            if previous == Some(id) {
                return Err(invalid());
            }
            previous = Some(id);
            Ok(())
        },
    )
}

fn field(key: &str, names: &[&str], seen: &mut u32) -> io::Result<()> {
    let index = names
        .iter()
        .position(|name| *name == key)
        .ok_or_else(invalid)?;
    let bit = 1 << index;
    if *seen & bit != 0 {
        return Err(invalid());
    }
    *seen |= bit;
    Ok(())
}

fn complete(seen: u32, names: &[&str]) -> io::Result<()> {
    if seen != (1 << names.len()) - 1 {
        return Err(invalid());
    }
    Ok(())
}

impl<R: Read + Seek> Parser<R> {
    fn restore(&mut self, position: u64) -> io::Result<()> {
        self.reader.seek(SeekFrom::Start(position))?;
        self.position = position;
        self.next = None;
        Ok(())
    }

    // Compare decoded strings in fixed chunks, then resume the enclosing scan.
    fn compare(&mut self, mut left: u64, mut right: u64) -> io::Result<Ordering> {
        const CHUNK: usize = 256;
        let resume = self.position;
        let result = loop {
            let mut a = ['\0'; CHUNK];
            let mut b = ['\0'; CHUNK];
            let a_len = name_chunk(&mut self.reader, &mut left, &mut a)?;
            let b_len = name_chunk(&mut self.reader, &mut right, &mut b)?;
            let order = a[..a_len].cmp(&b[..b_len]);
            if order != Ordering::Equal || a_len < CHUNK || b_len < CHUNK {
                break order;
            }
        };
        self.restore(resume)?;
        Ok(result)
    }

    fn marked_string(&mut self) -> io::Result<u64> {
        self.expect(b'"')?;
        let start = self.position;
        while self.string_char()?.is_some() {}
        Ok(start)
    }

    fn boolean(&mut self) -> io::Result<()> {
        match self.peek()? {
            Some(b't') => self.literal(b"true"),
            Some(b'f') => self.literal(b"false"),
            _ => Err(invalid()),
        }
    }

    fn nullable(&mut self, parse: impl FnOnce(&mut Self) -> io::Result<()>) -> io::Result<bool> {
        if self.peek()? == Some(b'n') {
            self.literal(b"null")?;
            return Ok(false);
        }
        parse(self)?;
        Ok(true)
    }

    fn identity(&mut self, kind: Identity) -> io::Result<()> {
        const FIELDS: &[&str] = &["id", "name"];
        let mut seen = 0;
        self.object(|p, key| {
            field(key, FIELDS, &mut seen)?;
            match key {
                "id" if matches!(kind, Identity::Mailbox) => {
                    p.integer::<i64>()?;
                }
                _ => p.string()?,
            }
            Ok(())
        })?;
        complete(seen, FIELDS)
    }

    fn flags(&mut self) -> io::Result<()> {
        const FIELDS: &[&str] = &["seen", "flagged", "answered", "draft", "deleted"];
        let mut seen = 0;
        self.object(|p, key| {
            field(key, FIELDS, &mut seen)?;
            p.boolean()
        })?;
        complete(seen, FIELDS)
    }

    fn acquisition(&mut self) -> io::Result<u64> {
        const FIELDS: &[&str] = &["id", "sha256", "bytes"];
        let mut seen = 0;
        let mut bytes = 0;
        self.object(|p, key| {
            field(key, FIELDS, &mut seen)?;
            match key {
                "id" => {
                    p.integer::<i64>()?;
                }
                "sha256" => {
                    p.hash()?;
                }
                "bytes" => bytes = p.integer::<u64>()?,
                _ => unreachable!(),
            }
            Ok(())
        })?;
        complete(seen, FIELDS)?;
        Ok(bytes)
    }

    fn array(&mut self, mut item: impl FnMut(&mut Self) -> io::Result<()>) -> io::Result<()> {
        self.expect(b'[')?;
        self.whitespace()?;
        if self.peek()? == Some(b']') {
            self.take()?;
            return Ok(());
        }
        loop {
            item(self)?;
            self.whitespace()?;
            match self.take()? {
                Some(b',') => self.whitespace()?,
                Some(b']') => return Ok(()),
                _ => return Err(invalid()),
            }
        }
    }

    fn keywords(&mut self) -> io::Result<()> {
        let mut previous = None;
        self.array(|p| {
            let current = p.marked_string()?;
            if let Some(last) = previous
                && p.compare(last, current)? != Ordering::Less
            {
                return Err(invalid());
            }
            previous = Some(current);
            Ok(())
        })
    }

    fn tag(&mut self) -> io::Result<(u64, u64)> {
        const FIELDS: &[&str] = &["id", "name", "color_srgb"];
        let mut seen = 0;
        let mut id = 0;
        let mut name = 0;
        self.object(|p, key| {
            field(key, FIELDS, &mut seen)?;
            match key {
                "id" => id = p.marked_string()?,
                "name" => name = p.marked_string()?,
                "color_srgb" => {
                    p.nullable(|p| {
                        let color = p.decoded(44)?;
                        if color.len() != 7
                            || !color.starts_with('#')
                            || !color[1..]
                                .bytes()
                                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
                        {
                            return Err(invalid());
                        }
                        Ok(())
                    })?;
                }
                _ => unreachable!(),
            }
            Ok(())
        })?;
        complete(seen, FIELDS)?;
        Ok((id, name))
    }

    fn tags(&mut self, scratch: &sb_store::ValidationScratch) -> io::Result<()> {
        let mut sorter = OffsetSort::new(scratch)?;
        let mut last_name = None;
        self.array(|p| {
            let (id, name) = p.tag()?;
            if let Some(last) = last_name
                && p.compare(last, name)? != Ordering::Less
            {
                return Err(invalid());
            }
            sorter.push(id, p, &mut Self::compare)?;
            last_name = Some(name);
            Ok(())
        })?;
        let mut previous = None;
        sorter.finish(
            self,
            &mut Self::compare,
            &mut |p| p.reader.stream_position().map(|_| ()),
            |p, id| {
                if let Some(last) = previous
                    && p.compare(last, id)? == Ordering::Equal
                {
                    return Err(invalid());
                }
                previous = Some(id);
                Ok(())
            },
        )
    }

    fn message(
        &mut self,
        format: BundleFormat,
        scratch: &sb_store::ValidationScratch,
    ) -> io::Result<i64> {
        const FIELDS: &[&str] = &[
            "v",
            "occurrence_id",
            "message_id",
            "account",
            "mailbox",
            "occurrence_state",
            "flags",
            "keywords",
            "acquisition",
            "body_availability",
            "tags",
            "note",
            "file",
            "record_offset",
            "encoded_bytes",
            "original_bytes",
            "framing_lf_added",
        ];
        const BASE: u32 = (1 << 13) - 1;
        const SPANS: u32 = ((1 << 17) - 1) ^ BASE;
        let mut seen = 0;
        let mut availability = String::new();
        let mut acquisition = None;
        let mut file = false;
        let mut original = None;
        let mut occurrence = None;
        self.object(|p, key| {
            field(key, FIELDS, &mut seen)?;
            match key {
                "v" => {
                    if p.integer::<u64>()? != 1 {
                        return Err(invalid());
                    }
                }
                "occurrence_id" => occurrence = Some(p.integer::<i64>()?),
                "message_id" => {
                    p.integer::<i64>()?;
                }
                "account" => p.identity(Identity::Account)?,
                "mailbox" => p.identity(Identity::Mailbox)?,
                "occurrence_state" => {
                    if !matches!(
                        p.decoded(128)?.as_str(),
                        "present" | "vanish_pending" | "vanished" | "epoch_stale" | "superseded"
                    ) {
                        return Err(invalid());
                    }
                }
                "flags" => p.flags()?,
                "keywords" => p.keywords()?,
                "acquisition" => {
                    p.nullable(|p| {
                        acquisition = Some(p.acquisition()?);
                        Ok(())
                    })?;
                }
                "body_availability" => availability = p.decoded(128)?,
                "tags" => p.tags(scratch)?,
                "note" => {
                    p.nullable(|p| p.string())?;
                }
                "file" => file = p.nullable(|p| p.string())?,
                "record_offset" | "encoded_bytes" => {
                    p.integer::<u64>()?;
                }
                "original_bytes" => original = Some(p.integer::<u64>()?),
                "framing_lf_added" => p.boolean()?,
                _ => unreachable!(),
            }
            Ok(())
        })?;
        let acquired = match availability.as_str() {
            "acquired" if acquisition.is_some() && file => true,
            "not_acquired" if acquisition.is_none() && !file => false,
            "missing" | "quarantined" if acquisition.is_some() && !file => false,
            _ => return Err(invalid()),
        };
        let expected = if acquired && format == BundleFormat::Mboxrd {
            BASE | SPANS
        } else {
            BASE
        };
        if seen != expected
            || (acquired && format == BundleFormat::Mboxrd && original != acquisition)
        {
            return Err(invalid());
        }
        occurrence.ok_or_else(invalid)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    struct CountSeeks<R> {
        inner: R,
        seeks: usize,
        bytes: usize,
    }

    impl<R: Read> Read for CountSeeks<R> {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            let n = self.inner.read(buf)?;
            self.bytes += n;
            Ok(n)
        }
    }

    impl<R: Seek> Seek for CountSeeks<R> {
        fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
            self.seeks += 1;
            self.inner.seek(pos)
        }
    }

    #[test]
    fn tag_ids_sort_across_runs_and_reuse_the_scratch() {
        let tmp = sb_test_support::ScratchDir::new("tag-sort-boundaries");
        let resources = crate::Resources::open(&tmp.path().join("store")).unwrap();
        let scratch = resources.store.import_validation_scratch().unwrap();
        for count in [255, 256, 257, 777, 0, 1] {
            let make_tags = |duplicate: bool| {
                (0..count)
                    .map(|n| {
                        let id = if n == 0 {
                            "é😀".to_string()
                        } else if duplicate && n == count - 1 {
                            r#"\u00e9\uD83D\uDE00"#.to_string()
                        } else {
                            format!("id{n:04}")
                        };
                        format!(r#"{{"id":"{id}","name":"{n:04}","color_srgb":null}}"#)
                    })
                    .collect::<Vec<_>>()
                    .join(",")
            };
            for duplicate in [false, true] {
                if duplicate && count < 2 {
                    continue;
                }
                let input = format!("[{}],", make_tags(duplicate));
                let mut parser = Parser {
                    reader: io::Cursor::new(input.as_bytes()),
                    next: None,
                    capture: None,
                    position: 0,
                };
                let result = parser.tags(&scratch);
                assert_eq!(result.is_err(), duplicate, "count {count}");
                if !duplicate {
                    parser.expect(b',').unwrap();
                } else {
                    // A failed sort leaves its disposable tape for lease cleanup.
                    scratch.remove(sb_store::ValidationTape::A).unwrap();
                    scratch.remove(sb_store::ValidationTape::B).unwrap();
                }
            }
        }
        scratch.finish().unwrap();
    }

    #[test]
    fn tag_validation_avoids_quadratic_rescans() {
        let tags = (0..1024)
            .map(|n| format!(r#"{{"id":"{n:04}","name":"{n:04}","color_srgb":null}}"#))
            .collect::<Vec<_>>()
            .join(",");
        let input = format!("[{tags}]");
        let mut reader = CountSeeks {
            inner: io::Cursor::new(input.as_bytes()),
            seeks: 0,
            bytes: 0,
        };
        let mut parser = Parser {
            reader: &mut reader,
            next: None,
            capture: None,
            position: 0,
        };
        let tmp = sb_test_support::ScratchDir::new("tag-sort-scaling");
        let resources = crate::Resources::open(&tmp.path().join("store")).unwrap();
        let scratch = resources.store.import_validation_scratch().unwrap();
        parser.tags(&scratch).unwrap();
        scratch.finish().unwrap();
        assert!(reader.seeks < 50_000, "{} seeks", reader.seeks);
    }

    fn manifest(ids: &[i64], tags: &str) -> String {
        let messages = ids.iter().map(|id| format!(r#"{{"v":1,"occurrence_id":{id},"message_id":2,"account":{{"id":"a","name":"a"}},"mailbox":{{"id":1,"name":"m"}},"occurrence_state":"present","flags":{{"seen":false,"flagged":false,"answered":false,"draft":false,"deleted":false}},"keywords":[],"acquisition":null,"body_availability":"not_acquired","tags":[{tags}],"note":null,"file":null}}"#)).collect::<Vec<_>>().join(",");
        let mut digest = Sha256::new();
        for id in ids {
            digest.update(id.to_be_bytes());
        }
        let hash = digest
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect::<String>();
        format!(
            r#"{{"v":1,"kind":"export-manifest","job":"j","format":"eml","destination":"/d","scope":{{"kind":"mailbox","id":1,"generation":1}},"generation":1,"entries":{},"ids_sha256":"{hash}","files":[],"messages":[{messages}]}}"#,
            ids.len()
        )
    }

    fn check_with(input: &str, schema: bool) -> io::Result<()> {
        let table = super::super::validate_envelope(input.as_bytes())?;
        let tmp = sb_test_support::ScratchDir::new("occurrence-validation");
        let resources = crate::Resources::open(&tmp.path().join("store")).unwrap();
        let scratch = resources.store.import_validation_scratch().unwrap();
        let mut reader = io::Cursor::new(input.as_bytes());
        let result = if schema {
            validate_messages(&mut reader, &table, &scratch)
        } else {
            unique_occurrences(&mut reader, &table, &scratch)
        };
        drop(scratch);
        assert_eq!(
            std::fs::read_dir(tmp.path().join("store/objects/tmp/import-validation"))
                .unwrap()
                .count(),
            0
        );
        result
    }

    fn check(input: &str) -> io::Result<()> {
        check_with(input, true)
    }

    fn linked_manifest(count: usize, format: &str) -> serde_json::Value {
        let ids = (0..count as i64).collect::<Vec<_>>();
        let mut document: serde_json::Value = serde_json::from_str(&manifest(&ids, "")).unwrap();
        document["format"] = format.into();
        let mut files = Vec::new();
        for (i, message) in document["messages"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .enumerate()
        {
            let name = if format == "eml" {
                format!("{i}.eml")
            } else {
                format!("file-{i:04}")
            };
            files.push(serde_json::json!({"name": name, "bytes": 42, "records": 1}));
            message["acquisition"] = serde_json::json!({"id":1,"sha256":"a".repeat(64),"bytes":42});
            message["body_availability"] = "acquired".into();
            message["file"] = name.into();
            if format == "mboxrd" {
                message["record_offset"] = 0.into();
                message["encoded_bytes"] = 0.into();
                message["original_bytes"] = 42.into();
                message["framing_lf_added"] = false.into();
            }
        }
        document["files"] = files.into();
        document
    }

    #[test]
    fn grouped_references_keep_message_order_and_seekable_offsets() {
        let mut document = linked_manifest(257, "mboxrd");
        document["files"] = serde_json::json!([
            {"name":"z", "bytes":42, "records":128},
            {"name":"a", "bytes":42, "records":129}
        ]);
        for (index, message) in document["messages"]
            .as_array_mut()
            .unwrap()
            .iter_mut()
            .enumerate()
        {
            message["file"] = if index % 2 == 0 { "a" } else { "z" }.into();
        }
        let input = document.to_string();
        let table = super::super::validate_envelope(input.as_bytes()).unwrap();
        let tmp = sb_test_support::ScratchDir::new("grouped-reference-order");
        let resources = crate::Resources::open(&tmp.path().join("store")).unwrap();
        let scratch = resources.store.import_validation_scratch().unwrap();
        let mut reader = io::Cursor::new(input.as_bytes());
        let mut seen = Vec::new();
        validate_messages_with_references(
            &mut reader,
            &table,
            &scratch,
            &mut |_, _, _, _, _| Ok(()),
            &mut |reader, offset, message_ordinal, file_ordinal| {
                reader.seek(SeekFrom::Start(offset))?;
                let mut first = [0];
                reader.read_exact(&mut first)?;
                assert_eq!(first, [b'{']);
                seen.push((file_ordinal, message_ordinal));
                Ok(())
            },
        )
        .unwrap();
        let expected = (0..257)
            .step_by(2)
            .map(|i| (1, i))
            .chain((1..257).step_by(2).map(|i| (0, i)))
            .collect::<Vec<_>>();
        assert_eq!(seen, expected);
        scratch.finish().unwrap();
    }

    #[test]
    fn file_relationships_cover_sort_boundaries_and_hostile_names() {
        for count in [0, 1, 255, 256, 257] {
            let mut document = linked_manifest(0, "mboxrd");
            document["files"] = (0..count)
                .map(|n| serde_json::json!({"name": format!("{n:04}"), "bytes": 0, "records": 0}))
                .collect::<Vec<_>>()
                .into();
            check(&document.to_string()).unwrap();
        }
        for format in ["eml", "mboxrd"] {
            for count in [0, 1, 255, 256, 257] {
                let document = linked_manifest(count, format);
                check(&document.to_string()).unwrap();
                if count == 257 {
                    let mut unknown = document.clone();
                    unknown["messages"][0]["file"] = "unknown".into();
                    assert!(check(&unknown.to_string()).is_err(), "{format} {count}");
                    let mut wrong_count = document.clone();
                    wrong_count["files"][count - 1]["records"] = 2.into();
                    assert!(check(&wrong_count.to_string()).is_err());
                    let mut duplicate = document.clone();
                    let first = duplicate["files"][0].clone();
                    duplicate["files"].as_array_mut().unwrap().push(first);
                    assert!(check(&duplicate.to_string()).is_err());
                }
            }
        }
        let mut unicode = linked_manifest(257, "mboxrd");
        unicode["files"][0]["name"] = "é😀".into();
        unicode["messages"][0]["file"] = "é😀".into();
        let encoded = unicode
            .to_string()
            .replacen("é😀", r#"\u00e9\ud83d\ude00"#, 1);
        check(&encoded).unwrap();
        unicode["files"][256]["name"] = "é😀".into();
        assert!(check(&unicode.to_string()).is_err());
    }

    #[test]
    fn child_groups_keep_original_file_ordinals() {
        let mut document = linked_manifest(777, "mboxrd");
        document["files"].as_array_mut().unwrap().reverse();
        let input = document.to_string();
        let table = super::super::validate_envelope(input.as_bytes()).unwrap();
        let tmp = sb_test_support::ScratchDir::new("child-ordinals");
        let resources = crate::Resources::open(&tmp.path().join("store")).unwrap();
        let scratch = resources.store.import_validation_scratch().unwrap();
        let mut reader = io::Cursor::new(input.as_bytes());
        let mut ordinals = Vec::new();
        validate_file_relationships(&mut reader, &table, &scratch, &mut |_, _, _, _, ordinal| {
            ordinals.push(ordinal);
            Ok(())
        })
        .unwrap();
        assert_eq!(ordinals, (0..777).rev().collect::<Vec<_>>());
        scratch.finish().unwrap();
    }

    #[test]
    fn file_relationship_counts_and_eml_agreement() {
        let base = linked_manifest(1, "eml");
        for (field, value) in [
            ("name", serde_json::json!("01.eml")),
            ("bytes", 41.into()),
            ("records", 0.into()),
            ("records", 2.into()),
        ] {
            let mut bad = base.clone();
            bad["files"][0][field] = value;
            assert!(check(&bad.to_string()).is_err());
        }
        let mut bad = base.clone();
        bad["messages"][0]["file"] = "01.eml".into();
        bad["files"][0]["name"] = "01.eml".into();
        assert!(check(&bad.to_string()).is_err());
        let mut mbox = linked_manifest(2, "mboxrd");
        mbox["messages"][1]["file"] = mbox["messages"][0]["file"].clone();
        mbox["files"][0]["records"] = 2.into();
        mbox["files"][1]["records"] = 0.into();
        check(&mbox.to_string()).unwrap();
        mbox["files"][1]["records"] = 1.into();
        assert!(check(&mbox.to_string()).is_err());
    }

    #[test]
    fn file_relationship_sort_scales_and_releases_scratch_on_failure() {
        let input = linked_manifest(1024, "mboxrd").to_string();
        let table = super::super::validate_envelope(input.as_bytes()).unwrap();
        let tmp = sb_test_support::ScratchDir::new("file-relationship-scaling");
        let resources = crate::Resources::open(&tmp.path().join("store")).unwrap();
        let scratch = resources.store.import_validation_scratch().unwrap();
        let mut reader = CountSeeks {
            inner: io::Cursor::new(input.as_bytes()),
            seeks: 0,
            bytes: 0,
        };
        validate_file_relationships(&mut reader, &table, &scratch, &mut |_, _, _, _, _| Ok(()))
            .unwrap();
        assert!(reader.seeks < 150_000, "{} seeks", reader.seeks);
        scratch.finish().unwrap();

        // A failure or cancellation while consuming the grouped declarations
        // must release the same scratch lease as a sort failure.
        for kind in [io::ErrorKind::Other, io::ErrorKind::Interrupted] {
            let scratch = resources.store.import_validation_scratch().unwrap();
            let mut reader = io::Cursor::new(input.as_bytes());
            let error =
                validate_file_relationships(&mut reader, &table, &scratch, &mut |_, _, _, _, _| {
                    Err(io::Error::new(kind, "child match stopped"))
                })
                .unwrap_err();
            assert_eq!(error.kind(), kind);
            drop(scratch);
            assert_eq!(
                std::fs::read_dir(tmp.path().join("store/objects/tmp/import-validation"))
                    .unwrap()
                    .count(),
                0
            );
        }

        struct FailAfter<R> {
            inner: R,
            remaining: usize,
            kind: io::ErrorKind,
        }
        impl<R: Read> Read for FailAfter<R> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.remaining == 0 {
                    return Err(io::Error::new(self.kind, "injected"));
                }
                self.remaining -= 1;
                self.inner.read(buf)
            }
        }
        impl<R: Seek> Seek for FailAfter<R> {
            fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
                self.inner.seek(pos)
            }
        }
        for kind in [io::ErrorKind::Other, io::ErrorKind::Interrupted] {
            let scratch = resources.store.import_validation_scratch().unwrap();
            let mut reader = FailAfter {
                inner: io::Cursor::new(input.as_bytes()),
                remaining: 30_000,
                kind,
            };
            assert_eq!(
                validate_file_relationships(
                    &mut reader,
                    &table,
                    &scratch,
                    &mut |_, _, _, _, _| Ok(())
                )
                .unwrap_err()
                .kind(),
                kind
            );
            drop(scratch);
            assert_eq!(
                std::fs::read_dir(tmp.path().join("store/objects/tmp/import-validation"))
                    .unwrap()
                    .count(),
                0
            );
        }
    }

    #[test]
    fn occurrence_count_hash_order_and_schema_are_checked() {
        let ids = [i64::MIN, -1, 0, i64::MAX];
        let input = manifest(&ids, "");
        assert!(input.contains("ed4fe6cf223ef491047f6f966ca180fb21ff97c1db82e1aeaa39f1c9f9febeca"));
        check(&input).unwrap();
        assert!(check(&input.replace("\"entries\":4", "\"entries\":3")).is_err());
        assert!(check(&input.replace("\"entries\":4", "\"entries\":5")).is_err());
        let mut wrong_order = manifest(&[i64::MAX, 0, -1, i64::MIN], "");
        let hash = input
            .split("\"ids_sha256\":\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap();
        let other = wrong_order
            .split("\"ids_sha256\":\"")
            .nth(1)
            .unwrap()
            .split('"')
            .next()
            .unwrap()
            .to_owned();
        wrong_order = wrong_order.replace(&other, hash);
        assert!(check(&wrong_order).is_err());
        assert!(check(&input.replace("\"note\":null", "\"note\":null,\"extra\":1")).is_err());
        assert!(check(&input.replace("\"messages\":[", "\"messages\":[false,")).is_err());
    }

    #[test]
    fn occurrence_sort_boundaries_and_reused_tag_tapes() {
        let tags =
            r#"{"id":"z","name":"a","color_srgb":null},{"id":"a","name":"b","color_srgb":null}"#;
        check(&manifest(&[2, 1], tags)).unwrap();
        assert!(check(&manifest(&[i64::MIN, i64::MAX, i64::MIN], tags)).is_err());
        for count in [0, 1, 255, 256, 257, 777] {
            let ids = (0..count).map(|i| i as i64 - 300).collect::<Vec<_>>();
            check_with(&manifest(&ids, ""), false).unwrap();
            if count > 1 {
                for duplicate in [1, count - 1] {
                    let mut ids = ids.clone();
                    ids[duplicate] = ids[0];
                    assert!(
                        check_with(&manifest(&ids, ""), false).is_err(),
                        "count {count}, duplicate {duplicate}"
                    );
                }
            }
        }
    }

    #[test]
    fn occurrence_validation_reads_a_bounded_number_of_manifest_bytes() {
        let ids = (0..4096).rev().map(i64::from).collect::<Vec<_>>();
        let input = manifest(&ids, "");
        let table = super::super::validate_envelope(input.as_bytes()).unwrap();
        let tmp = sb_test_support::ScratchDir::new("occurrence-scaling");
        let resources = crate::Resources::open(&tmp.path().join("store")).unwrap();
        let scratch = resources.store.import_validation_scratch().unwrap();
        let mut reader = CountSeeks {
            inner: io::Cursor::new(input.as_bytes()),
            seeks: 0,
            bytes: 0,
        };
        unique_occurrences(&mut reader, &table, &scratch).unwrap();
        scratch.finish().unwrap();
        assert!(
            reader.bytes < input.len() * 3,
            "{} bytes for {} input",
            reader.bytes,
            input.len()
        );
        assert!(reader.seeks < 20_000, "{} seeks", reader.seeks);
    }

    #[test]
    fn occurrence_second_scan_propagates_errors_and_cleans_lease() {
        struct FailSecondScan<'a> {
            inner: io::Cursor<&'a [u8]>,
            start: u64,
            starts: u8,
            fail_read: bool,
        }
        impl Read for FailSecondScan<'_> {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.fail_read && self.starts == 2 {
                    return Err(io::Error::other("injected read"));
                }
                self.inner.read(buf)
            }
        }
        impl Seek for FailSecondScan<'_> {
            fn seek(&mut self, pos: SeekFrom) -> io::Result<u64> {
                if pos == SeekFrom::Start(self.start) {
                    self.starts += 1;
                    if self.starts == 2 && !self.fail_read {
                        return Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"));
                    }
                }
                self.inner.seek(pos)
            }
        }
        let input = manifest(&[3, 2, 1], "");
        let table = super::super::validate_envelope(input.as_bytes()).unwrap();
        let tmp = sb_test_support::ScratchDir::new("occurrence-second-scan-fault");
        let resources = crate::Resources::open(&tmp.path().join("store")).unwrap();
        for fail_read in [false, true] {
            let scratch = resources.store.import_validation_scratch().unwrap();
            let mut reader = FailSecondScan {
                inner: io::Cursor::new(input.as_bytes()),
                start: table.messages.0 + 1,
                starts: 0,
                fail_read,
            };
            let error = validate_messages(&mut reader, &table, &scratch).unwrap_err();
            assert_eq!(reader.starts, 2);
            assert_eq!(
                error.kind(),
                if fail_read {
                    io::ErrorKind::Other
                } else {
                    io::ErrorKind::Interrupted
                }
            );
            drop(scratch);
            assert_eq!(
                std::fs::read_dir(tmp.path().join("store/objects/tmp/import-validation"))
                    .unwrap()
                    .count(),
                0
            );
        }
    }
}
