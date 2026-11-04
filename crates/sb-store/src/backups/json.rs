//! Private, bounded JSON lexer. No input-sized buffers or recursive descent.
//!
//! Extracted from the manifest scanner. Callers own schema/duplicate semantics
//! and must discard partial results after any error. Lexical number validity
//! does not imply representability by serde_json::Number (for example 1e9999).

use std::io::{self, Read};

const INPUT_BYTES: usize = 8192;
// serde_json starts with 128, decrements on entry, then rejects zero. Count
// every enclosing semantic container too, even when it is not skipped here.
const MAX_CONTAINERS: usize = 127;

#[derive(Debug, PartialEq, Eq)]
pub(super) enum Invalid {
    Syntax,
    Nesting,
    StringCapacity,
    UnsignedInteger,
}

impl std::fmt::Display for Invalid {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(match self {
            Self::Syntax => "invalid JSON syntax",
            Self::Nesting => "JSON nesting limit exceeded",
            Self::StringCapacity => "decoded JSON string exceeds fixed storage",
            Self::UnsignedInteger => "JSON number is not an exact u64",
        })
    }
}

impl std::error::Error for Invalid {}

fn invalid(reason: Invalid) -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, reason)
}

fn syntax() -> io::Error {
    invalid(Invalid::Syntax)
}

pub(super) struct Reader<'a, R, C> {
    input: R,
    buffer: [u8; INPUT_BYTES],
    position: usize,
    filled: usize,
    eof: bool,
    cancel: &'a mut C,
}

#[derive(Clone, Copy)]
enum Frame {
    ArrayFirst,
    ArrayValue,
    ArrayAfter,
    ObjectFirst,
    ObjectKey,
    ObjectAfter,
}

#[derive(PartialEq, Eq)]
enum NumberMode {
    Grammar,
    Unsigned,
}

impl<'a, R: Read, C: FnMut() -> io::Result<()>> Reader<'a, R, C> {
    pub(super) fn new(input: R, cancel: &'a mut C) -> Self {
        Self {
            input,
            buffer: [0; INPUT_BYTES],
            position: 0,
            filled: 0,
            eof: false,
            cancel,
        }
    }

    pub(super) fn check_cancel(&mut self) -> io::Result<()> {
        (self.cancel)()
    }

    pub(super) fn peek(&mut self) -> io::Result<Option<u8>> {
        self.check_cancel()?;
        if self.position == self.filled && !self.eof {
            // Preserve the actual reader error, including custom error payloads.
            self.filled = self.input.read(&mut self.buffer)?;
            self.position = 0;
            self.eof = self.filled == 0;
        }
        Ok((self.position < self.filled).then(|| self.buffer[self.position]))
    }

    pub(super) fn take(&mut self) -> io::Result<Option<u8>> {
        let byte = self.peek()?;
        if byte.is_some() {
            self.position += 1;
        }
        Ok(byte)
    }

    pub(super) fn expect(&mut self, expected: u8) -> io::Result<()> {
        if self.take()? == Some(expected) {
            Ok(())
        } else {
            Err(syntax())
        }
    }

    pub(super) fn whitespace(&mut self) -> io::Result<()> {
        while matches!(self.peek()?, Some(b' ' | b'\n' | b'\r' | b'\t')) {
            self.take()?;
        }
        Ok(())
    }

    pub(super) fn finish(&mut self) -> io::Result<()> {
        self.whitespace()?;
        if self.peek()?.is_some() {
            return Err(syntax());
        }
        Ok(())
    }

    fn delimiter(&mut self) -> io::Result<()> {
        match self.peek()? {
            None | Some(b' ' | b'\n' | b'\r' | b'\t' | b',' | b']' | b'}') => Ok(()),
            _ => Err(syntax()),
        }
    }

    fn literal(&mut self, text: &[u8]) -> io::Result<()> {
        for &byte in text {
            self.expect(byte)?;
        }
        self.delimiter()
    }

    /// Consume one number, with no numeric scratch. The exact mode refuses
    /// signs and non-integer syntax instead of coercing them through a float.
    fn number(&mut self, mode: NumberMode) -> io::Result<u64> {
        let exact = mode == NumberMode::Unsigned;
        if self.peek()? == Some(b'-') {
            if exact {
                return Err(invalid(Invalid::UnsignedInteger));
            }
            self.take()?;
        }
        let first = self.take()?.ok_or_else(syntax)?;
        if !first.is_ascii_digit() {
            return Err(syntax());
        }
        let mut value = u64::from(first - b'0');
        if first != b'0' {
            while let Some(byte @ b'0'..=b'9') = self.peek()? {
                self.take()?;
                if exact {
                    value = value
                        .checked_mul(10)
                        .and_then(|n| n.checked_add(u64::from(byte - b'0')))
                        .ok_or_else(|| invalid(Invalid::UnsignedInteger))?;
                }
            }
        }
        if self.peek()? == Some(b'.') {
            if exact {
                return Err(invalid(Invalid::UnsignedInteger));
            }
            self.take()?;
            self.digits()?;
        }
        if matches!(self.peek()?, Some(b'e' | b'E')) {
            if exact {
                return Err(invalid(Invalid::UnsignedInteger));
            }
            self.take()?;
            if matches!(self.peek()?, Some(b'+' | b'-')) {
                self.take()?;
            }
            self.digits()?;
        }
        self.delimiter()?;
        Ok(value)
    }

    fn digits(&mut self) -> io::Result<()> {
        if !self.take()?.is_some_and(|b| b.is_ascii_digit()) {
            return Err(syntax());
        }
        while self.peek()?.is_some_and(|b| b.is_ascii_digit()) {
            self.take()?;
        }
        Ok(())
    }

    pub(super) fn unsigned(&mut self) -> io::Result<u64> {
        self.number(NumberMode::Unsigned)
    }

    pub(super) fn fixed_string(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let mut len = 0;
        self.string(|ch| {
            let mut buf = [0; 4];
            let bytes = ch.encode_utf8(&mut buf).as_bytes();
            // Subtract first so even a caller-provided huge slice cannot wrap.
            if bytes.len() > out.len() - len {
                return Err(invalid(Invalid::StringCapacity));
            }
            let end = len + bytes.len();
            out[len..end].copy_from_slice(bytes);
            len = end;
            Ok(())
        })?;
        Ok(len)
    }

    /// Match decoded bytes, retaining only one suffix per caller-supplied name.
    /// A mismatching, arbitrarily long name is still fully validated.
    pub(super) fn name<const N: usize>(&mut self, names: [&str; N]) -> io::Result<Option<usize>> {
        let mut remaining = names.map(|name| Some(name.as_bytes()));
        self.string(|ch| {
            let mut buf = [0; 4];
            let bytes = ch.encode_utf8(&mut buf).as_bytes();
            for candidate in &mut remaining {
                *candidate = candidate.and_then(|rest| rest.strip_prefix(bytes));
            }
            Ok(())
        })?;
        Ok(remaining.iter().position(|rest| *rest == Some(&[][..])))
    }

    /// Decode scalar by scalar; sinks must not retain unbounded input.
    pub(super) fn string(
        &mut self,
        mut emit: impl FnMut(char) -> io::Result<()>,
    ) -> io::Result<()> {
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
                                    return Err(syntax());
                                }
                                0x10000 + ((u32::from(a) - 0xd800) << 10) + u32::from(b) - 0xdc00
                            }
                            0xdc00..=0xdfff => return Err(syntax()),
                            _ => u32::from(a),
                        };
                        char::from_u32(scalar).ok_or_else(syntax)?
                    }
                    _ => return Err(syntax()),
                },
                Some(c @ 0x20..=0x7f) => char::from(c),
                Some(first @ 0x80..=0xff) => {
                    let len = match first {
                        0xc2..=0xdf => 2,
                        0xe0..=0xef => 3,
                        0xf0..=0xf4 => 4,
                        _ => return Err(syntax()),
                    };
                    let mut buf = [0; 4];
                    buf[0] = first;
                    for b in &mut buf[1..len] {
                        *b = self.take()?.ok_or_else(syntax)?;
                    }
                    std::str::from_utf8(&buf[..len])
                        .map_err(|_| syntax())?
                        .chars()
                        .next()
                        .ok_or_else(syntax)?
                }
                _ => return Err(syntax()),
            };
            emit(ch)?;
        }
    }

    fn hex4(&mut self) -> io::Result<u16> {
        let mut n = 0;
        for _ in 0..4 {
            n = n * 16 + u16::from(hex(self.take()?.ok_or_else(syntax)?)?);
        }
        Ok(n)
    }

    /// Consume exactly one value. `enclosing` counts containers already opened
    /// by a semantic caller; ignoring an extra field must not reset its budget.
    pub(super) fn skip_value(&mut self, enclosing: usize) -> io::Result<()> {
        if enclosing > MAX_CONTAINERS {
            return Err(invalid(Invalid::Nesting));
        }
        let mut stack = [Frame::ArrayFirst; MAX_CONTAINERS];
        let mut depth = 0;
        loop {
            self.whitespace()?;
            match self.peek()? {
                Some(b'[' | b'{') => {
                    if depth == MAX_CONTAINERS - enclosing {
                        return Err(invalid(Invalid::Nesting));
                    }
                    stack[depth] = if self.take()? == Some(b'[') {
                        Frame::ArrayFirst
                    } else {
                        Frame::ObjectFirst
                    };
                    depth += 1;
                }
                Some(b'"') => self.string(|_| Ok(()))?,
                Some(b't') => self.literal(b"true")?,
                Some(b'f') => self.literal(b"false")?,
                Some(b'n') => self.literal(b"null")?,
                Some(b'-' | b'0'..=b'9') => {
                    self.number(NumberMode::Grammar)?;
                }
                _ => return Err(syntax()),
            }
            loop {
                if depth == 0 {
                    return self.check_cancel();
                }
                self.whitespace()?;
                let frame = &mut stack[depth - 1];
                match *frame {
                    Frame::ArrayFirst if self.peek()? == Some(b']') => {
                        self.take()?;
                        depth -= 1;
                    }
                    Frame::ArrayFirst | Frame::ArrayValue => {
                        *frame = Frame::ArrayAfter;
                        break;
                    }
                    Frame::ObjectFirst if self.peek()? == Some(b'}') => {
                        self.take()?;
                        depth -= 1;
                    }
                    Frame::ObjectFirst | Frame::ObjectKey => {
                        self.string(|_| Ok(()))?;
                        self.whitespace()?;
                        self.expect(b':')?;
                        *frame = Frame::ObjectAfter;
                        break;
                    }
                    Frame::ArrayAfter => match self.take()? {
                        Some(b',') => *frame = Frame::ArrayValue,
                        Some(b']') => depth -= 1,
                        _ => return Err(syntax()),
                    },
                    Frame::ObjectAfter => match self.take()? {
                        Some(b',') => *frame = Frame::ObjectKey,
                        Some(b'}') => depth -= 1,
                        _ => return Err(syntax()),
                    },
                }
            }
        }
    }
}

pub(super) fn hex(c: u8) -> io::Result<u8> {
    match c {
        b'0'..=b'9' => Ok(c - b'0'),
        b'a'..=b'f' => Ok(c - b'a' + 10),
        b'A'..=b'F' => Ok(c - b'A' + 10),
        _ => Err(syntax()),
    }
}

#[cfg(test)]
mod tests;
