//! Per-message validation on the verified, seekable snapshot.

use std::cmp::Ordering;
use std::io::{self, Read, Seek, SeekFrom};

use super::{BundleFormat, FileTable, Parser, invalid, name_chunk};

enum Identity {
    Account,
    Mailbox,
}

pub(in crate::import) fn validate_messages(
    reader: &mut (impl Read + Seek),
    table: &FileTable,
) -> io::Result<()> {
    let (start, end, count) = table.messages;
    reader.seek(SeekFrom::Start(start + 1))?;
    let mut p = Parser {
        reader,
        next: None,
        capture: None,
        position: start + 1,
    };
    for index in 0..count {
        p.whitespace()?;
        if index != 0 {
            p.expect(b',')?;
            p.whitespace()?;
        }
        p.message(table.format)?;
    }
    p.whitespace()?;
    p.expect(b']')?;
    if p.position != end {
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

    fn message(&mut self, format: BundleFormat) -> io::Result<()> {
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
        self.object(|p, key| {
            field(key, FIELDS, &mut seen)?;
            match key {
                "v" => {
                    if p.integer::<u64>()? != 1 {
                        return Err(invalid());
                    }
                }
                "occurrence_id" | "message_id" => {
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
        Ok(())
    }
}
