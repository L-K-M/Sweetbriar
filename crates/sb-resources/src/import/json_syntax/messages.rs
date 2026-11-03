//! Per-message validation on the verified, seekable snapshot.

use std::cmp::Ordering;
use std::io::{self, Read, Seek, SeekFrom};

use sha2::{Digest, Sha256};

use super::{
    BundleFormat, FileName, FileTable, Parser, invalid, name_chunk, read_entry, same_name,
};

const BATCH_SIZE: usize = 64;

enum Identity {
    Account,
    Mailbox,
}

pub(in crate::import) fn validate_messages(
    reader: &mut (impl Read + Seek),
    table: &FileTable,
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
    let mut references = 0u64;
    for index in 0..count {
        p.whitespace()?;
        if index != 0 {
            p.expect(b',')?;
            p.whitespace()?;
        }
        let (id, has_file) = p.message(table.format)?;
        digest.update(id.to_be_bytes());
        references = references
            .checked_add(u64::from(has_file))
            .ok_or_else(invalid)?;
    }
    p.whitespace()?;
    p.expect(b']')?;
    let hash: String = digest
        .finalize()
        .iter()
        .map(|byte| format!("{byte:02x}"))
        .collect();
    if p.position != end || hash != table.ids_hash {
        return Err(invalid());
    }
    unique_occurrences(reader, table)?;
    file_references(reader, table, references)
}

struct MessageRef {
    occurrence: i64,
    file: Option<FileName>,
    bytes: Option<u64>,
}

// Re-read only relationship fields after the complete schema pass.
fn read_message(reader: &mut (impl Read + Seek), offset: u64) -> io::Result<(MessageRef, u64)> {
    reader.seek(SeekFrom::Start(offset))?;
    let mut p = Parser {
        reader,
        next: None,
        capture: None,
        position: offset,
    };
    p.whitespace()?;
    if p.peek()? == Some(b',') {
        p.take()?;
        p.whitespace()?;
    }
    let mut occurrence = None;
    let mut file = None;
    let mut bytes = None;
    p.object(|p, key| {
        match key {
            "occurrence_id" => occurrence = Some(p.integer::<i64>()?),
            "file" => {
                p.nullable(|p| {
                    file = Some(p.file_name()?);
                    Ok(())
                })?;
            }
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
    Ok((
        MessageRef {
            occurrence: occurrence.ok_or_else(invalid)?,
            file,
            bytes,
        },
        p.position,
    ))
}

fn unique_occurrences(reader: &mut (impl Read + Seek), table: &FileTable) -> io::Result<()> {
    let (start, end, count) = table.messages;
    let mut outer = start + 1;
    let mut processed = 0;
    while processed < count {
        let mut batch = Vec::with_capacity(BATCH_SIZE);
        while batch.len() < BATCH_SIZE && processed < count {
            let (message, next) = read_message(reader, outer)?;
            if batch.contains(&message.occurrence) {
                return Err(invalid());
            }
            batch.push(message.occurrence);
            outer = next;
            processed += 1;
        }
        let mut inner = outer;
        for _ in processed..count {
            let (message, next) = read_message(reader, inner)?;
            if batch.contains(&message.occurrence) {
                return Err(invalid());
            }
            inner = next;
        }
    }
    if outer > end {
        return Err(invalid());
    }
    Ok(())
}

fn file_references(
    reader: &mut (impl Read + Seek),
    table: &FileTable,
    references: u64,
) -> io::Result<()> {
    let mut outer = table.start + 1;
    let mut processed = 0;
    let mut matched = 0u64;
    while processed < table.count {
        let mut batch = Vec::with_capacity(BATCH_SIZE);
        while batch.len() < BATCH_SIZE && processed < table.count {
            let (entry, next) = read_entry(reader, outer)?;
            batch.push((entry, 0u64));
            outer = next;
            processed += 1;
        }
        let mut offset = table.messages.0 + 1;
        for _ in 0..table.messages.2 {
            let (message, next) = read_message(reader, offset)?;
            offset = next;
            let Some(file) = message.file else { continue };
            for (entry, count) in &mut batch {
                if file.digest == entry.digest && same_name(reader, file.start, entry.start)? {
                    *count = count.checked_add(1).ok_or_else(invalid)?;
                    matched = matched.checked_add(1).ok_or_else(invalid)?;
                    if table.format == BundleFormat::Eml && message.bytes != Some(entry.bytes) {
                        return Err(invalid());
                    }
                    break;
                }
            }
        }
        for (entry, count) in batch {
            if count != entry.records || (table.format == BundleFormat::Eml && count != 1) {
                return Err(invalid());
            }
        }
    }
    if outer > table.end || matched != references {
        return Err(invalid());
    }
    Ok(())
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

    fn tags(&mut self) -> io::Result<()> {
        let first = self.position + 1;
        let mut count = 0u64;
        let mut last_name = None;
        self.array(|p| {
            let (id, name) = p.tag()?;
            let resume = p.position;
            if let Some(last) = last_name
                && p.compare(last, name)? != Ordering::Less
            {
                return Err(invalid());
            }
            // Names are strictly ordered; IDs require a bounded rescan.
            p.restore(first)?;
            for index in 0..count {
                p.whitespace()?;
                if index != 0 {
                    p.expect(b',')?;
                    p.whitespace()?;
                }
                let (prior_id, _) = p.tag()?;
                if p.compare(prior_id, id)? == Ordering::Equal {
                    return Err(invalid());
                }
            }
            p.restore(resume)?;
            count = count.checked_add(1).ok_or_else(invalid)?;
            last_name = Some(name);
            Ok(())
        })
    }

    fn message(&mut self, format: BundleFormat) -> io::Result<(i64, bool)> {
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
                "tags" => p.tags()?,
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
        Ok((occurrence.ok_or_else(invalid)?, file))
    }
}

#[cfg(test)]
mod tests {
    use std::io::Cursor;

    use serde_json::{Value, json};

    use super::*;
    use crate::import::json_syntax::{validate_envelope, validate_file_names};

    fn message(id: i64, file: Option<&str>, format: BundleFormat) -> Value {
        let mut message = json!({
            "v": 1, "occurrence_id": id, "message_id": 9,
            "account": {"id":"a", "name":"a"}, "mailbox": {"id":1, "name":"m"},
            "occurrence_state":"present",
            "flags": {"seen":false,"flagged":false,"answered":false,"draft":false,"deleted":false},
            "keywords": [], "acquisition": {"id":3,"sha256":"a".repeat(64),"bytes":2},
            "body_availability":"acquired", "tags":[], "note":null, "file":file,
        });
        if file.is_none() {
            message["acquisition"] = Value::Null;
            message["body_availability"] = json!("not_acquired");
        } else if format == BundleFormat::Mboxrd {
            message["record_offset"] = json!(0);
            message["encoded_bytes"] = json!(2);
            message["original_bytes"] = json!(2);
            message["framing_lf_added"] = json!(false);
        }
        message
    }

    fn manifest(format: BundleFormat, files: Value, messages: Vec<Value>) -> Value {
        let mut digest = Sha256::new();
        for message in &messages {
            digest.update(message["occurrence_id"].as_i64().unwrap().to_be_bytes());
        }
        let hash: String = digest
            .finalize()
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        json!({
            "v":1, "kind":"export-manifest", "job":"j", "format":format.name(),
            "destination":"/d", "scope":{"kind":"mailbox","id":1,"generation":1},
            "generation":1, "entries":messages.len(), "ids_sha256":hash,
            "files":files, "messages":messages,
        })
    }

    fn check(raw: &str) -> io::Result<()> {
        let mut reader = Cursor::new(raw.as_bytes());
        let table = validate_envelope(&mut reader)?;
        validate_file_names(&mut reader, &table)?;
        validate_messages(&mut reader, &table)
    }

    #[test]
    fn relationship_proofs_and_exact_decoded_references() {
        for format in [BundleFormat::Eml, BundleFormat::Mboxrd] {
            let name = format!("{}😀", "x".repeat(100_000));
            let valid = manifest(
                format,
                json!([{"name":name,"bytes":2,"records":1}]),
                vec![
                    message(i64::MIN, Some(&name), format),
                    message(i64::MAX, None, format),
                ],
            );
            let raw = valid.to_string();
            // One spelling differs but decodes to the declared name.
            let escaped = raw.replacen(&name, &name.replacen('😀', r"\ud83d\ude00", 1), 1);
            assert_ne!(raw, escaped);
            assert!(check(&escaped).is_ok());

            let mut duplicate = valid.clone();
            duplicate["messages"][1]["occurrence_id"] = json!(i64::MIN);
            let duplicate = manifest(
                format,
                duplicate["files"].clone(),
                duplicate["messages"].as_array().unwrap().clone(),
            );
            assert!(check(&duplicate.to_string()).is_err());
            let mut unresolved = valid.clone();
            unresolved["messages"][0]["file"] = json!(format!("{name}z"));
            assert!(check(&unresolved.to_string()).is_err());
        }

        let mut eml = manifest(
            BundleFormat::Eml,
            json!([{"name":"a","bytes":2,"records":1}]),
            vec![message(1, Some("a"), BundleFormat::Eml)],
        );
        eml["files"][0]["bytes"] = json!(3);
        assert!(check(&eml.to_string()).is_err());
        eml["files"][0]["records"] = json!(0);
        eml["messages"][0] = message(1, None, BundleFormat::Eml);
        assert!(check(&eml.to_string()).is_err());
        let repeated = manifest(
            BundleFormat::Eml,
            json!([{"name":"a","bytes":2,"records":2}]),
            vec![
                message(1, Some("a"), BundleFormat::Eml),
                message(2, Some("a"), BundleFormat::Eml),
            ],
        );
        assert!(check(&repeated.to_string()).is_err());
        let empty = manifest(
            BundleFormat::Mboxrd,
            json!([{"name":"empty","bytes":0,"records":0}]),
            vec![],
        );
        assert!(check(&empty.to_string()).is_ok());
    }

    #[test]
    fn distant_repeats_reject_only_occurrence_ids() {
        let format = BundleFormat::Mboxrd;
        let messages = (0..65)
            .map(|id| message(id, Some("a"), format))
            .collect::<Vec<_>>();
        let files = json!([{"name":"a","bytes":2,"records":65}]);
        let valid = manifest(format, files.clone(), messages.clone());
        assert!(check(&valid.to_string()).is_ok());
        let mut duplicate = messages;
        duplicate[64]["occurrence_id"] = json!(0);
        let duplicate = manifest(format, files, duplicate);
        assert!(check(&duplicate.to_string()).is_err());
    }

    #[test]
    fn references_cross_file_batches() {
        let format = BundleFormat::Eml;
        let files = (0..65)
            .map(|id| json!({"name":format!("{id}.eml"),"bytes":2,"records":1}))
            .collect::<Vec<_>>();
        let messages = (0..65)
            .map(|id| message(id, Some(&format!("{id}.eml")), format))
            .collect::<Vec<_>>();
        let valid = manifest(format, json!(files), messages);
        assert!(check(&valid.to_string()).is_ok());
        let mut mismatch = valid.clone();
        mismatch["files"][64]["records"] = json!(0);
        assert!(check(&mismatch.to_string()).is_err());
        let mut missing = valid;
        missing["messages"][64]["file"] = json!("absent.eml");
        assert!(check(&missing.to_string()).is_err());
    }
}
