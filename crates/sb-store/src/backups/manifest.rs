//! Streaming, schema-specific format-1 backup manifest parser.
//!
//! Unknown fields are rejected, so nesting is limited to the envelope, the
//! database object and the objects array with its record objects. The sink may
//! receive records before a later error; callers must discard partial results.

use std::io::{self, BufReader, Read};

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Entry {
    pub sha256: [u8; 32],
    pub bytes: u64,
}

#[derive(Debug, PartialEq, Eq)]
pub(super) struct Manifest {
    pub store_uuid: [u8; 16],
    pub generation_id: [u8; 16],
    pub schema_version: i64,
    pub created_at: i64,
    pub database: Entry,
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid backup manifest")
}

/// Parse one manifest. Cancellation and sink errors are propagated unchanged.
pub(super) fn parse(
    input: impl Read,
    mut cancel: impl FnMut() -> io::Result<()>,
    mut sink: impl FnMut(Entry) -> io::Result<()>,
) -> io::Result<Manifest> {
    let mut p = Parser {
        input: BufReader::new(input),
        next: None,
        cancel: &mut cancel,
    };
    let mut seen = 0u16;
    let (mut store, mut generation, mut schema, mut created, mut database) =
        (None, None, None, None, None);
    p.object(|p, key| {
        let bit = match key {
            b"format" => 1,
            b"store_uuid" => 2,
            b"generation_id" => 4,
            b"app_version" => 8,
            b"schema_version" => 16,
            b"created_at" => 32,
            b"database" => 64,
            b"objects" => 128,
            _ => return Err(invalid()),
        };
        if seen & bit != 0 {
            return Err(invalid());
        }
        seen |= bit;
        match bit {
            1 => {
                if p.unsigned()? != 1 {
                    return Err(invalid());
                }
            }
            2 => store = Some(p.uuid()?),
            4 => generation = Some(p.uuid()?),
            8 => p.string(|_| Ok(()))?,
            16 => schema = Some(p.signed()?),
            32 => created = Some(p.signed()?),
            64 => database = Some(p.entry(true)?),
            128 => p.array(&mut sink)?,
            _ => unreachable!(),
        }
        Ok(())
    })?;
    p.whitespace()?;
    if seen != 255 || p.peek()?.is_some() {
        return Err(invalid());
    }
    Ok(Manifest {
        store_uuid: store.ok_or_else(invalid)?,
        generation_id: generation.ok_or_else(invalid)?,
        schema_version: schema.ok_or_else(invalid)?,
        created_at: created.ok_or_else(invalid)?,
        database: database.ok_or_else(invalid)?,
    })
}

struct Parser<'a, R, C> {
    input: BufReader<R>,
    next: Option<u8>,
    cancel: &'a mut C,
}

impl<R: Read, C: FnMut() -> io::Result<()>> Parser<'_, R, C> {
    fn peek(&mut self) -> io::Result<Option<u8>> {
        if self.next.is_none() {
            (self.cancel)()?;
            let mut byte = [0];
            if self.input.read(&mut byte)? != 0 {
                self.next = Some(byte[0]);
            }
        }
        Ok(self.next)
    }

    fn take(&mut self) -> io::Result<Option<u8>> {
        let byte = self.peek()?;
        self.next = None;
        Ok(byte)
    }

    fn expect(&mut self, expected: u8) -> io::Result<()> {
        if self.take()? == Some(expected) {
            Ok(())
        } else {
            Err(invalid())
        }
    }

    fn whitespace(&mut self) -> io::Result<()> {
        while matches!(self.peek()?, Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.take()?;
        }
        Ok(())
    }

    fn object(
        &mut self,
        mut field: impl FnMut(&mut Self, &[u8]) -> io::Result<()>,
    ) -> io::Result<()> {
        self.whitespace()?;
        self.expect(b'{')?;
        self.whitespace()?;
        if self.peek()? == Some(b'}') {
            self.take()?;
            return Ok(());
        }
        loop {
            let mut key = [0; 32];
            let len = self.fixed_string(&mut key)?;
            self.whitespace()?;
            self.expect(b':')?;
            self.whitespace()?;
            field(self, &key[..len])?;
            self.whitespace()?;
            match self.take()? {
                Some(b',') => self.whitespace()?,
                Some(b'}') => return Ok(()),
                _ => return Err(invalid()),
            }
        }
    }

    fn array(&mut self, sink: &mut impl FnMut(Entry) -> io::Result<()>) -> io::Result<()> {
        self.expect(b'[')?;
        self.whitespace()?;
        if self.peek()? == Some(b']') {
            self.take()?;
            return Ok(());
        }
        loop {
            (self.cancel)()?;
            sink(self.entry(false)?)?;
            self.whitespace()?;
            match self.take()? {
                Some(b',') => self.whitespace()?,
                Some(b']') => return Ok(()),
                _ => return Err(invalid()),
            }
        }
    }

    fn entry(&mut self, database: bool) -> io::Result<Entry> {
        let mut seen = 0;
        let (mut sha, mut bytes) = (None, None);
        self.object(|p, key| {
            let bit = match key {
                b"sha256" => 1,
                b"bytes" => 2,
                b"file" if database => 4,
                _ => return Err(invalid()),
            };
            if seen & bit != 0 {
                return Err(invalid());
            }
            seen |= bit;
            match bit {
                1 => sha = Some(p.hash()?),
                2 => bytes = Some(p.unsigned()?),
                4 => {
                    let mut name = [0; 12];
                    let n = p.fixed_string(&mut name)?;
                    if &name[..n] != b"state.sqlite" {
                        return Err(invalid());
                    }
                }
                _ => unreachable!(),
            }
            Ok(())
        })?;
        if seen != if database { 7 } else { 3 } {
            return Err(invalid());
        }
        Ok(Entry {
            sha256: sha.ok_or_else(invalid)?,
            bytes: bytes.ok_or_else(invalid)?,
        })
    }

    fn unsigned(&mut self) -> io::Result<u64> {
        let first = self.take()?.ok_or_else(invalid)?;
        if !first.is_ascii_digit() {
            return Err(invalid());
        }
        let mut n = u64::from(first - b'0');
        if first == b'0' {
            return Ok(n);
        }
        while let Some(c @ b'0'..=b'9') = self.peek()? {
            self.take()?;
            n = n
                .checked_mul(10)
                .and_then(|n| n.checked_add(u64::from(c - b'0')))
                .ok_or_else(invalid)?;
        }
        Ok(n)
    }

    fn signed(&mut self) -> io::Result<i64> {
        let negative = self.peek()? == Some(b'-');
        if negative {
            self.take()?;
        }
        let n = self.unsigned()?;
        if negative {
            if n == (i64::MAX as u64) + 1 {
                Ok(i64::MIN)
            } else {
                Ok(-i64::try_from(n).map_err(|_| invalid())?)
            }
        } else {
            i64::try_from(n).map_err(|_| invalid())
        }
    }

    fn fixed_string(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let mut len = 0;
        self.string(|ch| {
            let mut buf = [0; 4];
            let bytes = ch.encode_utf8(&mut buf).as_bytes();
            let end = len + bytes.len();
            if end > out.len() {
                return Err(invalid());
            }
            out[len..end].copy_from_slice(bytes);
            len = end;
            Ok(())
        })?;
        Ok(len)
    }

    fn uuid(&mut self) -> io::Result<[u8; 16]> {
        let mut text = [0; 36];
        if self.fixed_string(&mut text)? != 36 {
            return Err(invalid());
        }
        let mut out = [0; 16];
        let mut pos = 0;
        for (i, slot) in out.iter_mut().enumerate() {
            if matches!(i, 4 | 6 | 8 | 10) {
                if text[pos] != b'-' {
                    return Err(invalid());
                }
                pos += 1;
            }
            *slot = hex(text[pos])?.checked_mul(16).ok_or_else(invalid)? + hex(text[pos + 1])?;
            pos += 2;
        }
        Ok(out)
    }

    fn hash(&mut self) -> io::Result<[u8; 32]> {
        let mut text = [0; 64];
        if self.fixed_string(&mut text)? != 64 {
            return Err(invalid());
        }
        let mut out = [0; 32];
        for (slot, pair) in out.iter_mut().zip(text.as_chunks::<2>().0) {
            if pair.iter().any(|c| c.is_ascii_uppercase()) {
                return Err(invalid());
            }
            *slot = hex(pair[0])? * 16 + hex(pair[1])?;
        }
        Ok(out)
    }

    fn string(&mut self, mut emit: impl FnMut(char) -> io::Result<()>) -> io::Result<()> {
        self.expect(b'"')?;
        loop {
            let ch = match self.take()? {
                Some(b'"') => return Ok(()),
                Some(b'\\') => match self.take()? {
                    Some(b'"') => '"',
                    Some(b'\\') => '\\',
                    Some(b'/') => '/',
                    Some(b'b') => '\u{8}',
                    Some(b'f') => '\u{c}',
                    Some(b'n') => '\n',
                    Some(b'r') => '\r',
                    Some(b't') => '\t',
                    Some(b'u') => {
                        let a = self.hex4()?;
                        let scalar = match a {
                            0xd800..=0xdbff => {
                                self.expect(b'\\')?;
                                self.expect(b'u')?;
                                let b = self.hex4()?;
                                if !(0xdc00..=0xdfff).contains(&b) {
                                    return Err(invalid());
                                }
                                0x10000 + ((u32::from(a) - 0xd800) << 10) + u32::from(b) - 0xdc00
                            }
                            0xdc00..=0xdfff => return Err(invalid()),
                            _ => u32::from(a),
                        };
                        char::from_u32(scalar).ok_or_else(invalid)?
                    }
                    _ => return Err(invalid()),
                },
                Some(c @ 0x20..=0x7f) => char::from(c),
                Some(first @ 0x80..=0xff) => {
                    let len = match first {
                        0xc2..=0xdf => 2,
                        0xe0..=0xef => 3,
                        0xf0..=0xf4 => 4,
                        _ => return Err(invalid()),
                    };
                    let mut buf = [0; 4];
                    buf[0] = first;
                    for b in &mut buf[1..len] {
                        *b = self.take()?.ok_or_else(invalid)?;
                    }
                    std::str::from_utf8(&buf[..len])
                        .map_err(|_| invalid())?
                        .chars()
                        .next()
                        .ok_or_else(invalid)?
                }
                _ => return Err(invalid()),
            };
            emit(ch)?;
        }
    }

    fn hex4(&mut self) -> io::Result<u16> {
        let mut n = 0;
        for _ in 0..4 {
            n = n * 16 + u16::from(hex(self.take()?.ok_or_else(invalid)?)?);
        }
        Ok(n)
    }
}

fn hex(c: u8) -> io::Result<u8> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(invalid()),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::cell::Cell;

    fn document() -> String {
        format!(
            r#"{{"format":1,"store_uuid":"12345678-1234-4234-8234-123456789abc","generation_id":"12345678-1234-4234-8234-123456789abc","app_version":"v😀","schema_version":1,"created_at":-1,"database":{{"file":"state.sqlite","sha256":"{}","bytes":12}},"objects":[{{"sha256":"{}","bytes":3}}]}}"#,
            "ab".repeat(32),
            "cd".repeat(32)
        )
    }

    fn parse_bytes(bytes: &[u8]) -> io::Result<Manifest> {
        parse(bytes, || Ok(()), |_| Ok(()))
    }

    #[test]
    fn valid_and_every_truncation() {
        let doc = document();
        let mut entries = Vec::new();
        let manifest = parse(
            doc.as_bytes(),
            || Ok(()),
            |entry| {
                entries.push(entry);
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(manifest.database.bytes, 12);
        assert_eq!(
            entries,
            [Entry {
                sha256: [0xcd; 32],
                bytes: 3
            }]
        );
        for end in 0..doc.len() {
            assert!(parse_bytes(&doc.as_bytes()[..end]).is_err(), "prefix {end}");
        }
    }

    #[test]
    fn rejects_invalid_schema_and_numbers() {
        let doc = document();
        for invalid in [
            format!("{doc}x"),
            doc.replace(r#""format":1"#, r#""format":1,"format":1"#),
            doc.replace(r#""format":1,"#, ""),
            doc.replace(r#""format":1"#, r#""format":2"#),
            doc.replace(r#""format":1"#, r#""unknown":1"#),
            doc.replace(
                r#""format":1"#,
                &format!(r#""unknown":{}0{}"#, "[".repeat(1000), "]".repeat(1000)),
            ),
            doc.replace(r#""bytes":12"#, r#""bytes":18446744073709551616"#),
            doc.replace(r#""created_at":-1"#, r#""created_at":-9223372036854775809"#),
            doc.replace(r#""schema_version":1"#, r#""schema_version":01"#),
            doc.replace("state.sqlite", "state.sqlite2"),
            doc.replace(&"ab".repeat(32), &"AB".repeat(32)),
            doc.replace(r#""objects":["#, r#""objects":[[],"#),
            doc.replace(r#""bytes":3"#, r#""bytes":3,"bytes":3"#),
        ] {
            assert!(parse_bytes(invalid.as_bytes()).is_err(), "{invalid}");
        }
        assert!(
            parse_bytes(
                doc.replace(r#""created_at":-1"#, r#""created_at":-9223372036854775808"#)
                    .as_bytes()
            )
            .is_ok()
        );
    }

    #[test]
    fn unicode_and_fixed_scalars_are_incremental() {
        let doc = document();
        assert!(parse_bytes(doc.replace("v😀", r#"v\uD83D\uDE00"#).as_bytes()).is_ok());
        for bad in [r#"\uD800"#, r#"\uDC00"#, r#"\uD800\u0041"#, r#"\x"#] {
            assert!(parse_bytes(doc.replace("v😀", bad).as_bytes()).is_err());
        }
        let mut invalid = doc.into_bytes();
        let pos = invalid.iter().position(|&b| b == 0xf0).unwrap();
        invalid[pos] = 0xff;
        assert!(parse_bytes(&invalid).is_err());
        let huge = document().replace("12345678-1234-4234-8234-123456789abc", &"a".repeat(100_000));
        assert!(parse_bytes(huge.as_bytes()).is_err());
    }

    #[test]
    fn unbounded_metadata_and_cancellation() {
        let doc = document().replace("v😀", &"x".repeat(200_000));
        let doc = format!("{}{}{}", " ".repeat(200_000), doc, " ".repeat(200_000));
        assert!(parse_bytes(doc.as_bytes()).is_ok());
        let calls = Cell::new(0);
        let result = parse(
            doc.as_bytes(),
            || {
                calls.set(calls.get() + 1);
                if calls.get() == 50_000 {
                    Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
                } else {
                    Ok(())
                }
            },
            |_| Ok(()),
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
        let doc = document();
        let marker = doc.find("v😀").unwrap();
        let calls = Cell::new(0);
        let result = parse(
            doc.as_bytes(),
            || {
                calls.set(calls.get() + 1);
                if calls.get() == marker + 2 {
                    Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
                } else {
                    Ok(())
                }
            },
            |_| Ok(()),
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
        assert_eq!(
            parse(doc.as_bytes(), || Ok(()), |_| Err(io::Error::other("sink")))
                .unwrap_err()
                .kind(),
            io::ErrorKind::Other
        );
    }

    #[test]
    fn rejects_long_fixed_scalar_without_consuming_it_and_cancels_records() {
        struct Counted<'a> {
            data: &'a [u8],
            read: &'a Cell<usize>,
        }
        impl Read for Counted<'_> {
            fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
                let n = self.data.read(output)?;
                self.read.set(self.read.get() + n);
                Ok(n)
            }
        }
        let doc = document().replace(
            "12345678-1234-4234-8234-123456789abc",
            &"a".repeat(1_000_000),
        );
        let read = Cell::new(0);
        assert!(
            parse(
                Counted {
                    data: doc.as_bytes(),
                    read: &read
                },
                || Ok(()),
                |_| Ok(())
            )
            .is_err()
        );
        assert!(read.get() <= 8192);

        let item = format!(r#"{{"sha256":"{}","bytes":3}}"#, "cd".repeat(32));
        let many = document().replace(
            &item,
            &std::iter::repeat_n(item.as_str(), 1000)
                .collect::<Vec<_>>()
                .join(","),
        );
        let count = Cell::new(0);
        let result = parse(
            many.as_bytes(),
            || {
                if count.get() == 100 {
                    Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
                } else {
                    Ok(())
                }
            },
            |_| {
                count.set(count.get() + 1);
                Ok(())
            },
        );
        assert_eq!(result.unwrap_err().kind(), io::ErrorKind::Interrupted);
        assert_eq!(count.get(), 100);
    }

    #[test]
    fn streams_many_records_without_collecting_them() {
        struct Repeated<'a> {
            item: &'a [u8],
            remaining: usize,
            offset: usize,
        }
        impl Read for Repeated<'_> {
            fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
                if self.remaining == 0 || out.is_empty() {
                    return Ok(0);
                }
                let len = out.len().min(self.item.len() - self.offset);
                out[..len].copy_from_slice(&self.item[self.offset..self.offset + len]);
                self.offset += len;
                if self.offset == self.item.len() {
                    self.offset = 0;
                    self.remaining -= 1;
                }
                Ok(len)
            }
        }
        let doc = document();
        let item = format!(r#"{{"sha256":"{}","bytes":3}}"#, "cd".repeat(32));
        let at = doc.find(&item).unwrap();
        let repeated_item = format!("{item},");
        let input = (&doc.as_bytes()[..at])
            .chain(Repeated {
                item: repeated_item.as_bytes(),
                remaining: 100_000,
                offset: 0,
            })
            .chain(&doc.as_bytes()[at..]);
        let mut count = 0;
        parse(
            input,
            || Ok(()),
            |entry| {
                assert_eq!(entry.bytes, 3);
                count += 1;
                Ok(())
            },
        )
        .unwrap();
        assert_eq!(count, 100_001);
    }
}
