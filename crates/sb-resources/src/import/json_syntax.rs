//! Bounded JSON syntax pass for captured manifests. Schema validation follows
//! in the next split; a successful pass never authorizes import by itself.

use std::io::{self, Read};

// Search query nodes have depth 32 (spec/06). A node can add an object
// and an operands array; the manifest and scope wrappers fit in 16 more.
const SEARCH_QUERY_DEPTH: usize = 32;
const MANIFEST_WRAPPER_DEPTH: usize = 16;
const MAX_DEPTH: usize = SEARCH_QUERY_DEPTH * 2 + MANIFEST_WRAPPER_DEPTH;

pub(super) fn validate(reader: impl Read) -> io::Result<()> {
    let mut parser = Parser { reader, next: None };
    parser.value(0)?;
    parser.whitespace()?;
    if parser.peek()?.is_some() {
        return Err(invalid());
    }
    Ok(())
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid manifest JSON")
}

struct Parser<R> {
    reader: R,
    next: Option<u8>,
}

impl<R: Read> Parser<R> {
    fn peek(&mut self) -> io::Result<Option<u8>> {
        if self.next.is_none() {
            let mut byte = [0];
            if self.reader.read(&mut byte)? != 0 {
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

    fn expect(&mut self, byte: u8) -> io::Result<()> {
        if self.take()? == Some(byte) {
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

    fn value(&mut self, depth: usize) -> io::Result<()> {
        self.whitespace()?;
        match self.peek()? {
            Some(b'{') => self.container(depth, b'}', true),
            Some(b'[') => self.container(depth, b']', false),
            Some(b'"') => self.string(),
            Some(b't') => self.literal(b"true"),
            Some(b'f') => self.literal(b"false"),
            Some(b'n') => self.literal(b"null"),
            Some(b'-' | b'0'..=b'9') => self.number(),
            _ => Err(invalid()),
        }
    }

    fn container(&mut self, depth: usize, end: u8, object: bool) -> io::Result<()> {
        if depth >= MAX_DEPTH {
            return Err(invalid());
        }
        self.take()?;
        self.whitespace()?;
        if self.peek()? == Some(end) {
            self.take()?;
            return Ok(());
        }
        loop {
            if object {
                self.string()?;
                self.whitespace()?;
                self.expect(b':')?;
            }
            self.value(depth + 1)?;
            self.whitespace()?;
            match self.take()? {
                Some(b',') => self.whitespace()?,
                Some(byte) if byte == end => return Ok(()),
                _ => return Err(invalid()),
            }
        }
    }

    fn literal(&mut self, text: &[u8]) -> io::Result<()> {
        for &byte in text {
            self.expect(byte)?;
        }
        Ok(())
    }

    fn number(&mut self) -> io::Result<()> {
        if self.peek()? == Some(b'-') {
            self.take()?;
        }
        match self.take()? {
            Some(b'0') => {}
            Some(b'1'..=b'9') => self.digits()?,
            _ => return Err(invalid()),
        }
        if self.peek()? == Some(b'.') {
            self.take()?;
            self.required_digits()?;
        }
        if matches!(self.peek()?, Some(b'e' | b'E')) {
            self.take()?;
            if matches!(self.peek()?, Some(b'+' | b'-')) {
                self.take()?;
            }
            self.required_digits()?;
        }
        Ok(())
    }

    fn required_digits(&mut self) -> io::Result<()> {
        if !matches!(self.peek()?, Some(b'0'..=b'9')) {
            return Err(invalid());
        }
        self.digits()
    }

    fn digits(&mut self) -> io::Result<()> {
        while matches!(self.peek()?, Some(b'0'..=b'9')) {
            self.take()?;
        }
        Ok(())
    }

    fn string(&mut self) -> io::Result<()> {
        self.expect(b'"')?;
        loop {
            match self.take()? {
                Some(b'"') => return Ok(()),
                Some(b'\\') => self.escape()?,
                Some(0x20..=0x7f) => {}
                Some(first @ 0x80..=0xff) => self.utf8(first)?,
                _ => return Err(invalid()),
            }
        }
    }

    fn utf8(&mut self, first: u8) -> io::Result<()> {
        let len = match first {
            0xc2..=0xdf => 2,
            0xe0..=0xef => 3,
            0xf0..=0xf4 => 4,
            _ => return Err(invalid()),
        };
        let mut bytes = [0; 4];
        bytes[0] = first;
        for byte in &mut bytes[1..len] {
            *byte = self.take()?.ok_or_else(invalid)?;
        }
        std::str::from_utf8(&bytes[..len]).map_err(|_| invalid())?;
        Ok(())
    }

    fn escape(&mut self) -> io::Result<()> {
        match self.take()? {
            Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => Ok(()),
            Some(b'u') => {
                let first = self.hex4()?;
                match first {
                    0xd800..=0xdbff => {
                        self.expect(b'\\')?;
                        self.expect(b'u')?;
                        if !(0xdc00..=0xdfff).contains(&self.hex4()?) {
                            return Err(invalid());
                        }
                        Ok(())
                    }
                    0xdc00..=0xdfff => Err(invalid()),
                    _ => Ok(()),
                }
            }
            _ => Err(invalid()),
        }
    }

    fn hex4(&mut self) -> io::Result<u16> {
        let mut value = 0;
        for _ in 0..4 {
            let digit = match self.take()? {
                Some(c @ b'0'..=b'9') => u16::from(c - b'0'),
                Some(c @ b'a'..=b'f') => u16::from(c - b'a' + 10),
                Some(c @ b'A'..=b'F') => u16::from(c - b'A' + 10),
                _ => return Err(invalid()),
            };
            value = value * 16 + digit;
        }
        Ok(value)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn accepted(input: &[u8]) -> bool {
        validate(io::BufReader::with_capacity(1, input)).is_ok()
    }

    #[test]
    fn syntax_and_unicode_across_reads() {
        for input in [
            r#"{"x": [null, true, false, -1.2e+30, "€😀", "\uD83D\uDE00"]}"#,
            "[]",
            "{}",
            "0",
        ] {
            assert!(accepted(input.as_bytes()), "{input}");
        }
        for input in [
            "",
            "[",
            "{",
            "[1,]",
            "{\"x\":}",
            "true false",
            "01",
            "-",
            "1.",
            "1e+",
            "\"\\uD800\"",
            "\"\\uDC00\"",
            "\"\\uD800\\u0041\"",
            "\"\\x\"",
            "\"\n\"",
            "\"unterminated",
            "nul",
            "[false",
        ] {
            assert!(!accepted(input.as_bytes()), "{input}");
        }
        for bytes in [
            &b"\"\xc0\x80\""[..],
            &b"\"\xed\xa0\x80\""[..],
            &b"\"\xf4\x90\x80\x80\""[..],
            &b"\"\xe2\x82\""[..],
        ] {
            assert!(!accepted(bytes));
        }
    }

    #[test]
    fn truncations_and_large_arrays() {
        let document = br#"{"items":[true,false,null,-12.5e+9,"\uD83D\uDE00",{"x":[]}]}"#;
        assert!(accepted(document));
        for end in 0..document.len() {
            assert!(!accepted(&document[..end]), "accepted prefix {end}");
        }
        let entries = std::iter::repeat_n(r#"{"name":"a.eml","bytes":1}"#, 5_000)
            .collect::<Vec<_>>()
            .join(",");
        assert!(accepted(format!("[{entries}]").as_bytes()));
    }

    #[test]
    fn bounds_nesting_without_bounding_token_length() {
        let valid = format!("{}0{}", "[".repeat(MAX_DEPTH), "]".repeat(MAX_DEPTH));
        let invalid = format!("[{valid}]");
        assert!(accepted(valid.as_bytes()));
        assert!(!accepted(invalid.as_bytes()));
        let long = format!(
            "[\"{}\",{}]",
            "\\u0061".repeat(100_000),
            "9".repeat(100_000)
        );
        assert!(accepted(long.as_bytes()));
    }
}
