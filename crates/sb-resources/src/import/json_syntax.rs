//! Bounded manifest envelope, scope, file-table and message validation.

use std::io::{self, Read, Seek, SeekFrom};

use sha2::{Digest, Sha256};

use super::BundleFormat;

pub(crate) mod cursor;
mod messages;
pub(super) use messages::validate_messages;

// Search query nodes have depth 32 (spec/06). A node can add an object
// and an operands array; the manifest and scope wrappers fit in 16 more.
const SEARCH_QUERY_DEPTH: usize = 32;
const MANIFEST_WRAPPER_DEPTH: usize = 16;
const MAX_DEPTH: usize = SEARCH_QUERY_DEPTH * 2 + MANIFEST_WRAPPER_DEPTH;
// Export job ids have the same byte limit as the ledger (spec/09).
const MAX_JOB_ID_BYTES: usize = 128;
const MAX_ESCAPED_JOB_BYTES: usize = 2 + 6 * MAX_JOB_ID_BYTES;
const UUID_TEXT_BYTES: usize = 36;
const MAX_ESCAPED_UUID_BYTES: usize = 2 + 6 * UUID_TEXT_BYTES;

#[cfg(test)]
fn validate(reader: impl Read) -> io::Result<()> {
    let mut parser = Parser {
        reader,
        next: None,
        capture: None,
        position: 0,
    };
    parser.value(0)?;
    parser.whitespace()?;
    if parser.peek()?.is_some() {
        return Err(invalid());
    }
    Ok(())
}

/// Validate the bounded envelope and scope. Records remain fail-closed until
/// their schema and references are checked by the following step.
pub(super) fn validate_envelope(reader: impl Read) -> io::Result<FileTable> {
    let mut parser = Parser {
        reader,
        next: None,
        capture: None,
        position: 0,
    };
    let table = parser.envelope()?;
    parser.whitespace()?;
    if parser.peek()?.is_some() {
        return Err(invalid());
    }
    Ok(table)
}

/// The validated array's byte range and count, for later record validation.
pub(super) struct FileTable {
    start: u64,
    end: u64,
    count: u64,
    messages: (u64, u64, u64),
    format: BundleFormat,
    entries: u64,
    ids_hash: String,
}

impl FileTable {
    pub(super) fn files(&self) -> (u64, u64) {
        (self.start + 1, self.count)
    }

    pub(super) fn format(&self) -> BundleFormat {
        self.format
    }
}

struct FileName {
    start: u64,
    digest: [u8; 32],
    bytes: u64,
    records: u64,
}

/// One checked declaration, retaining an offset instead of its name.
pub(super) struct Child {
    name: FileName,
    pub(super) bytes: u64,
    pub(super) records: u64,
}

pub(super) fn child(reader: &mut (impl Read + Seek), offset: u64) -> io::Result<(Child, u64)> {
    let (name, next) = read_entry(reader, offset)?;
    Ok((
        Child {
            bytes: name.bytes,
            records: name.records,
            name,
        },
        next,
    ))
}

impl Child {
    pub(super) fn matches(&self, reader: &mut (impl Read + Seek), name: &str) -> io::Result<bool> {
        reader.seek(SeekFrom::Start(self.name.start))?;
        let mut parser = Parser {
            reader,
            next: None,
            capture: None,
            position: self.name.start,
        };
        let mut expected = name.chars();
        loop {
            let actual = parser.string_char()?;
            if actual != expected.next() {
                return Ok(false);
            }
            if actual.is_none() {
                return Ok(true);
            }
        }
    }

    /// The prefix is display-only; names remain unrestricted by this bound.
    pub(super) fn description(
        &self,
        reader: &mut (impl Read + Seek),
        ordinal: u64,
    ) -> io::Result<(String, bool, String)> {
        const PREFIX_BYTES: usize = 256;
        reader.seek(SeekFrom::Start(self.name.start))?;
        let mut parser = Parser {
            reader,
            next: None,
            capture: None,
            position: self.name.start,
        };
        let mut prefix = String::new();
        let mut length = 0u64;
        let mut truncated = false;
        while let Some(ch) = parser.string_char()? {
            length = length
                .checked_add(ch.len_utf8() as u64)
                .ok_or_else(invalid)?;
            if !truncated && prefix.len() + ch.len_utf8() <= PREFIX_BYTES {
                prefix.push(ch);
            } else {
                truncated = true;
            }
        }
        let digest: String = self
            .name
            .digest
            .iter()
            .map(|b| format!("{b:02x}"))
            .collect();
        let suffix = if truncated { " (truncated)" } else { "" };
        let detail = format!(
            "missing declared child {ordinal}: {prefix:?}{suffix}, {length} bytes, sha256 {digest}"
        );
        Ok((prefix, truncated, detail))
    }
}

/// Compare decoded names exactly; hashes only avoid unnecessary comparisons.
pub(super) fn validate_file_names(
    reader: &mut (impl Read + Seek),
    table: &FileTable,
) -> io::Result<()> {
    // A fixed batch reduces rescans without retaining names for the corpus.
    const BATCH_SIZE: usize = 64;
    let mut outer = table.start + 1;
    let mut processed = 0;
    while processed < table.count {
        let mut batch = Vec::with_capacity(BATCH_SIZE);
        while batch.len() < BATCH_SIZE && processed < table.count {
            let (name, next) = read_entry(reader, outer)?;
            check_name(reader, &batch, &name)?;
            batch.push(name);
            outer = next;
            processed += 1;
        }
        let mut inner = outer;
        for _ in processed..table.count {
            let (other, after) = read_entry(reader, inner)?;
            check_name(reader, &batch, &other)?;
            inner = after;
        }
    }
    if outer > table.end {
        return Err(invalid());
    }
    Ok(())
}

fn check_name(
    reader: &mut (impl Read + Seek),
    batch: &[FileName],
    name: &FileName,
) -> io::Result<()> {
    for previous in batch {
        if previous.digest == name.digest && same_name(reader, previous.start, name.start)? {
            return Err(invalid());
        }
    }
    Ok(())
}

fn read_entry(reader: &mut (impl Read + Seek), offset: u64) -> io::Result<(FileName, u64)> {
    reader.seek(SeekFrom::Start(offset))?;
    let mut parser = Parser {
        reader,
        next: None,
        capture: None,
        position: offset,
    };
    parser.whitespace()?;
    if parser.peek()? == Some(b',') {
        parser.take()?;
    }
    parser.whitespace()?;
    let name = parser.file_entry()?;
    Ok((name, parser.position))
}

fn name_chunk(
    reader: &mut (impl Read + Seek),
    offset: &mut u64,
    out: &mut [char],
) -> io::Result<usize> {
    reader.seek(SeekFrom::Start(*offset))?;
    let mut parser = Parser {
        reader,
        next: None,
        capture: None,
        position: *offset,
    };
    let mut length = 0;
    for slot in out {
        let Some(character) = parser.string_char()? else {
            break;
        };
        *slot = character;
        length += 1;
    }
    *offset = parser.position;
    Ok(length)
}

fn same_name(reader: &mut (impl Read + Seek), mut left: u64, mut right: u64) -> io::Result<bool> {
    const CHUNK: usize = 256;
    loop {
        let mut a = ['\0'; CHUNK];
        let mut b = ['\0'; CHUNK];
        let a_len = name_chunk(reader, &mut left, &mut a)?;
        let b_len = name_chunk(reader, &mut right, &mut b)?;
        if a_len != b_len || a[..a_len] != b[..b_len] {
            return Ok(false);
        }
        if a_len < CHUNK {
            return Ok(true);
        }
    }
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid manifest JSON")
}

struct Parser<R> {
    reader: R,
    next: Option<u8>,
    capture: Option<Vec<u8>>,
    position: u64,
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
        if byte.is_some() {
            self.position = self.position.checked_add(1).ok_or_else(invalid)?;
        }
        if let (Some(bytes), Some(byte)) = (&mut self.capture, byte) {
            if bytes.len() >= sb_store::query::MAX_QUERY_INPUT_BYTES {
                return Err(invalid());
            }
            bytes.push(byte);
        }
        Ok(byte)
    }

    fn decoded(&mut self, limit: usize) -> io::Result<String> {
        self.whitespace()?;
        self.capture = Some(Vec::new());
        let result = self.string();
        let raw = self.capture.take().expect("capture was installed");
        result?;
        if raw.len() > limit {
            return Err(invalid());
        }
        serde_json::from_slice(&raw).map_err(|_| invalid())
    }

    fn object(
        &mut self,
        mut field: impl FnMut(&mut Self, &str) -> io::Result<()>,
    ) -> io::Result<()> {
        self.whitespace()?;
        self.expect(b'{')?;
        self.whitespace()?;
        if self.peek()? == Some(b'}') {
            self.take()?;
            return Ok(());
        }
        loop {
            let key = self.decoded(128)?;
            self.whitespace()?;
            self.expect(b':')?;
            self.whitespace()?;
            field(self, &key)?;
            self.whitespace()?;
            match self.take()? {
                Some(b',') => self.whitespace()?,
                Some(b'}') => return Ok(()),
                _ => return Err(invalid()),
            }
        }
    }

    fn integer<T: std::str::FromStr>(&mut self) -> io::Result<T> {
        self.capture = Some(Vec::new());
        let result = self.number();
        let raw = self.capture.take().expect("capture was installed");
        result?;
        std::str::from_utf8(&raw)
            .map_err(|_| invalid())?
            .parse()
            .map_err(|_| invalid())
    }

    fn hash(&mut self) -> io::Result<String> {
        let hash = self.decoded(386)?;
        if hash.len() != 64
            || !hash
                .bytes()
                .all(|b| b.is_ascii_digit() || (b'a'..=b'f').contains(&b))
        {
            return Err(invalid());
        }
        Ok(hash)
    }

    fn envelope(&mut self) -> io::Result<FileTable> {
        const FIELDS: &[&str] = &[
            "v",
            "kind",
            "job",
            "format",
            "destination",
            "scope",
            "generation",
            "entries",
            "ids_sha256",
            "files",
            "messages",
        ];
        let mut seen = 0u16;
        let mut generation = None;
        let mut entries = None;
        let mut ids_hash = None;
        let mut scope = None;
        let mut files = None;
        let mut messages = None;
        let mut format = None;
        self.object(|p, key| {
            let index = FIELDS
                .iter()
                .position(|name| *name == key)
                .ok_or_else(invalid)?;
            let bit = 1u16 << index;
            if seen & bit != 0 {
                return Err(invalid());
            }
            seen |= bit;
            match key {
                "v" => {
                    if p.integer::<u64>()? != 1 {
                        return Err(invalid());
                    }
                }
                "kind" => {
                    if p.decoded(128)? != "export-manifest" {
                        return Err(invalid());
                    }
                }
                "job" => {
                    // A decoded job id is limited by the export ledger.
                    let id = p.decoded(MAX_ESCAPED_JOB_BYTES)?;
                    if id.is_empty() || id.len() > MAX_JOB_ID_BYTES {
                        return Err(invalid());
                    }
                }
                "destination" => p.absolute_path()?,
                "format" => {
                    format = Some(BundleFormat::from_name(&p.decoded(64)?).ok_or_else(invalid)?);
                }
                "scope" => scope = Some(p.scope()?),
                "generation" => generation = Some(p.integer::<u64>()?),
                "entries" => entries = Some(p.integer::<u64>()?),
                "ids_sha256" => ids_hash = Some(p.hash()?),
                "files" => files = Some(p.file_table()?),
                "messages" => {
                    let start = p.position;
                    p.expect(b'[')?;
                    p.whitespace()?;
                    let mut count = 0u64;
                    if p.peek()? != Some(b']') {
                        loop {
                            p.value(1)?;
                            count = count.checked_add(1).ok_or_else(invalid)?;
                            p.whitespace()?;
                            match p.take()? {
                                Some(b',') => p.whitespace()?,
                                Some(b']') => break,
                                _ => return Err(invalid()),
                            }
                        }
                    } else {
                        p.take()?;
                    }
                    messages = Some((start, p.position, count));
                }
                _ => unreachable!(),
            }
            Ok(())
        })?;
        if seen != (1u16 << FIELDS.len()) - 1 {
            return Err(invalid());
        }
        let (scope_generation, selection) = scope.ok_or_else(invalid)?;
        if generation != Some(scope_generation) {
            return Err(invalid());
        }
        if let Some((count, hash)) = selection
            && (entries != Some(count) || ids_hash.as_ref() != Some(&hash))
        {
            return Err(invalid());
        }
        let mut files = files.ok_or_else(invalid)?;
        files.messages = messages.ok_or_else(invalid)?;
        files.format = format.ok_or_else(invalid)?;
        files.entries = entries.ok_or_else(invalid)?;
        files.ids_hash = ids_hash.ok_or_else(invalid)?;
        Ok(files)
    }

    fn file_table(&mut self) -> io::Result<FileTable> {
        let start = self.position;
        self.expect(b'[')?;
        self.whitespace()?;
        let mut count = 0u64;
        if self.peek()? != Some(b']') {
            loop {
                self.file_entry()?;
                count = count.checked_add(1).ok_or_else(invalid)?;
                self.whitespace()?;
                match self.take()? {
                    Some(b',') => self.whitespace()?,
                    Some(b']') => break,
                    _ => return Err(invalid()),
                }
            }
        } else {
            self.take()?;
        }
        Ok(FileTable {
            start,
            end: self.position,
            count,
            messages: (0, 0, 0),
            format: BundleFormat::Eml,
            entries: 0,
            ids_hash: String::new(),
        })
    }

    fn file_entry(&mut self) -> io::Result<FileName> {
        let mut seen = 0u8;
        let mut name = None;
        let mut bytes = 0;
        let mut records = 0;
        self.object(|p, key| {
            let bit = match key {
                "name" => 1,
                "bytes" => 2,
                "records" => 4,
                _ => return Err(invalid()),
            };
            if seen & bit != 0 {
                return Err(invalid());
            }
            seen |= bit;
            match key {
                "name" => name = Some(p.file_name()?),
                "bytes" => bytes = p.integer::<u64>()?,
                "records" => records = p.integer::<u64>()?,
                _ => unreachable!(),
            }
            Ok(())
        })?;
        if seen != 0b111 {
            return Err(invalid());
        }
        let mut name = name.ok_or_else(invalid)?;
        name.bytes = bytes;
        name.records = records;
        Ok(name)
    }

    fn file_name(&mut self) -> io::Result<FileName> {
        self.expect(b'"')?;
        let start = self.position;
        let mut hash = Sha256::new();
        let mut length = 0u64;
        let mut dots = true;
        while let Some(ch) = self.string_char()? {
            if matches!(ch, '/' | '\\' | '\0') {
                return Err(invalid());
            }
            dots &= ch == '.';
            length = length.checked_add(1).ok_or_else(invalid)?;
            let mut bytes = [0; 4];
            hash.update(ch.encode_utf8(&mut bytes).as_bytes());
        }
        if length == 0 || (dots && length <= 2) {
            return Err(invalid());
        }
        Ok(FileName {
            start,
            digest: hash.finalize().into(),
            bytes: 0,
            records: 0,
        })
    }

    fn string_char(&mut self) -> io::Result<Option<char>> {
        let ch = match self.take()? {
            Some(b'"') => return Ok(None),
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
                    let first = self.hex4()?;
                    let scalar = match first {
                        0xd800..=0xdbff => {
                            self.expect(b'\\')?;
                            self.expect(b'u')?;
                            let second = self.hex4()?;
                            if !(0xdc00..=0xdfff).contains(&second) {
                                return Err(invalid());
                            }
                            0x10000 + ((u32::from(first) - 0xd800) << 10) + u32::from(second)
                                - 0xdc00
                        }
                        0xdc00..=0xdfff => return Err(invalid()),
                        _ => u32::from(first),
                    };
                    char::from_u32(scalar).ok_or_else(invalid)?
                }
                _ => return Err(invalid()),
            },
            Some(byte @ 0x20..=0x7f) => char::from(byte),
            Some(first @ 0x80..=0xff) => {
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
                std::str::from_utf8(&bytes[..len])
                    .map_err(|_| invalid())?
                    .chars()
                    .next()
                    .ok_or_else(invalid)?
            }
            _ => return Err(invalid()),
        };
        Ok(Some(ch))
    }

    fn scope(&mut self) -> io::Result<(u64, Option<(u64, String)>)> {
        const FIELDS: &[&str] = &[
            "kind",
            "id",
            "generation",
            "query",
            "scope",
            "sort",
            "window",
            "count",
            "ids_sha256",
        ];
        let mut seen = 0u16;
        let mut kind = String::new();
        let mut id_string_bytes = None;
        let mut window = 0u64;
        let mut generation = 0u64;
        let mut count = 0u64;
        let mut ids_hash = None;
        self.object(|p, key| {
            let index = FIELDS
                .iter()
                .position(|name| *name == key)
                .ok_or_else(invalid)?;
            let bit = 1u16 << index;
            if seen & bit != 0 {
                return Err(invalid());
            }
            seen |= bit;
            match key {
                "kind" => kind = p.decoded(64)?,
                "id" => {
                    if p.peek()? == Some(b'"') {
                        id_string_bytes = Some(p.nonempty_string()?);
                    } else {
                        p.integer::<i64>()?;
                    }
                }
                "window" => window = p.integer::<u64>()?,
                "generation" => generation = p.integer::<u64>()?,
                "count" => count = p.integer::<u64>()?,
                "ids_sha256" => ids_hash = Some(p.hash()?),
                "sort" => {
                    if !matches!(p.decoded(64)?.as_str(), "date" | "relevance") {
                        return Err(invalid());
                    }
                }
                "query" => {
                    p.capture = Some(Vec::new());
                    let result = p.value(2);
                    let raw = p.capture.take().expect("capture was installed");
                    result?;
                    let query = std::str::from_utf8(&raw).map_err(|_| invalid())?;
                    sb_store::query::Query::parse_json(query).map_err(|_| invalid())?;
                }
                "scope" => p.candidate()?,
                _ => unreachable!(),
            }
            Ok(())
        })?;
        let expected = match kind.as_str() {
            "account" if matches!(id_string_bytes, Some(1..=MAX_JOB_ID_BYTES)) => 0b000000111,
            "mailbox" if id_string_bytes.is_none() => 0b000000111,
            "search" => 0b000111101,
            "selection" if id_string_bytes.is_some() && window != 0 => 0b111000111,
            _ => return Err(invalid()),
        };
        if seen != expected {
            return Err(invalid());
        }
        let selection = if kind == "selection" {
            Some((count, ids_hash.ok_or_else(invalid)?))
        } else {
            None
        };
        Ok((generation, selection))
    }

    fn candidate(&mut self) -> io::Result<()> {
        if self.peek()? == Some(b'"') {
            if self.decoded(32)? != "all" {
                return Err(invalid());
            }
            return Ok(());
        }
        let mut seen = false;
        self.object(|p, key| {
            if seen {
                return Err(invalid());
            }
            seen = true;
            match key {
                "account" => {
                    let id = p.decoded(MAX_ESCAPED_UUID_BYTES)?;
                    id.parse::<sb_store::Uuid>().map_err(|_| invalid())?;
                }
                "mailbox" => {
                    p.integer::<i64>()?;
                }
                _ => return Err(invalid()),
            }
            Ok(())
        })?;
        if !seen {
            return Err(invalid());
        }
        Ok(())
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
        self.string_length().map(|_| ())
    }

    fn string_length(&mut self) -> io::Result<usize> {
        self.expect(b'"')?;
        self.string_body_length()
    }

    fn nonempty_string(&mut self) -> io::Result<usize> {
        let length = self.string_length()?;
        if length == 0 {
            return Err(invalid());
        }
        Ok(length)
    }

    fn absolute_path(&mut self) -> io::Result<()> {
        self.expect(b'"')?;
        match self.take()? {
            Some(b'/') => {}
            Some(b'\\') => match self.take()? {
                Some(b'/') => {}
                Some(b'u') if self.hex4()? == u16::from(b'/') => {}
                _ => return Err(invalid()),
            },
            _ => return Err(invalid()),
        }
        self.string_body_length().map(|_| ())
    }

    fn string_body_length(&mut self) -> io::Result<usize> {
        let mut length = 0usize;
        loop {
            let bytes = match self.take()? {
                Some(b'"') => return Ok(length),
                Some(b'\\') => self.escape()?,
                Some(0x20..=0x7f) => 1,
                Some(first @ 0x80..=0xff) => self.utf8(first)?,
                _ => return Err(invalid()),
            };
            length = length.checked_add(bytes).ok_or_else(invalid)?;
        }
    }

    fn utf8(&mut self, first: u8) -> io::Result<usize> {
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
        Ok(len)
    }

    fn escape(&mut self) -> io::Result<usize> {
        match self.take()? {
            Some(b'"' | b'\\' | b'/' | b'b' | b'f' | b'n' | b'r' | b't') => Ok(1),
            Some(b'u') => {
                let first = self.hex4()?;
                match first {
                    0xd800..=0xdbff => {
                        self.expect(b'\\')?;
                        self.expect(b'u')?;
                        if !(0xdc00..=0xdfff).contains(&self.hex4()?) {
                            return Err(invalid());
                        }
                        Ok(4)
                    }
                    0xdc00..=0xdfff => Err(invalid()),
                    0x0000..=0x007f => Ok(1),
                    0x0080..=0x07ff => Ok(2),
                    _ => Ok(3),
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

    fn envelope(scope: &str) -> String {
        format!(
            r#"{{"v":1,"kind":"export-manifest","job":"j","format":"eml","destination":"/d","scope":{scope},"generation":1,"entries":0,"ids_sha256":"{}","files":[],"messages":[]}}"#,
            "a".repeat(64)
        )
    }

    #[test]
    fn envelope_and_scope_are_strict() {
        for scope in [
            r#"{"kind":"account","id":"account","generation":1}"#,
            r#"{"kind":"mailbox","id":-1,"generation":1}"#,
            r#"{"kind":"selection","id":"sel","window":1,"generation":1,"count":0,"ids_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#,
            r#"{"kind":"search","generation":1,"sort":"date","scope":"all","query":{"v":1,"q":{"and":[]}}}"#,
            r#"{"kind":"search","generation":1,"sort":"date","scope":{"account":"12345678-1234-4234-8234-123456789abc"},"query":{"v":1,"q":{"and":[]}}}"#,
            r#"{"kind":"search","generation":1,"sort":"relevance","scope":{"mailbox":1},"query":{"v":1,"q":{"and":[]}}}"#,
        ] {
            let input = envelope(scope);
            assert!(validate_envelope(input.as_bytes()).is_ok(), "{scope}");
        }
        let valid = envelope(r#"{"kind":"mailbox","id":1,"generation":1}"#);
        for invalid in [
            valid.replace(r#""scope":{"kind":"mailbox","id":1,"generation":1},"#, ""),
            valid.replace(
                r#""kind":"export-manifest""#,
                r#""kind":"export-manifest","\u006bind":"export-manifest""#,
            ),
            valid.replace(
                r#""kind":"mailbox""#,
                r#""kind":"mailbox","\u006bind":"mailbox""#,
            ),
            valid.replace(r#""id":1"#, r#""id":"1""#),
            envelope(r#"{"kind":"account","id":"","generation":1}"#),
            envelope(&format!(
                r#"{{"kind":"account","id":"{}","generation":1}}"#,
                "a".repeat(MAX_JOB_ID_BYTES + 1)
            )),
            envelope(
                r#"{"kind":"selection","id":"sel","window":0,"generation":1,"count":0,"ids_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#,
            ),
            envelope(
                r#"{"kind":"selection","id":"sel","window":1,"generation":1,"count":1,"ids_sha256":"aaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaaa"}"#,
            ),
            envelope(
                r#"{"kind":"selection","id":"sel","window":1,"generation":1,"count":0,"ids_sha256":"bbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbbb"}"#,
            ),
            valid.replacen(r#""generation":1"#, r#""generation":-1"#, 1),
            valid.replacen(r#""generation":1"#, r#""generation":2"#, 1),
            valid.replace(r#""format":"eml""#, r#""format":"unknown""#),
            valid.replace(r#""destination":"/d""#, r#""destination":"relative""#),
            valid.replace(r#""job":"j""#, r#""job":"""#),
            valid.replace(r#""files":[]"#, r#""files":{}"#),
            valid.replace(r#""entries":0"#, r#""entries":18446744073709551616"#),
            valid.replace(r#""messages":[]"#, r#""messages":[],"unknown":0"#),
            envelope(
                r#"{"kind":"search","generation":1,"sort":"date","scope":"all","query":{"v":1,"q":{"unknown":1}}}"#,
            ),
            envelope(
                r#"{"kind":"search","generation":1,"sort":"date","scope":{"account":""},"query":{"v":1,"q":{"and":[]}}}"#,
            ),
            envelope(
                r#"{"kind":"search","generation":1,"sort":"date","scope":{"account":"NOT-A-UUID"},"query":{"v":1,"q":{"and":[]}}}"#,
            ),
        ] {
            assert!(validate_envelope(invalid.as_bytes()).is_err(), "{invalid}");
        }
    }

    #[test]
    fn account_id_limit_counts_decoded_utf8_bytes() {
        let literal = |count| serde_json::to_string(&"é".repeat(count)).unwrap();
        let escaped = |count| format!(r#""{}""#, r#"\u00e9"#.repeat(count));
        let surrogate = |count| format!(r#""{}""#, r#"\uD83D\uDE00"#.repeat(count));
        let manifest = |id: &str| -> String {
            envelope(&format!(r#"{{"kind":"account","id":{id},"generation":1}}"#))
        };

        for id in [literal(64), escaped(64), surrogate(32)] {
            assert!(validate_envelope(manifest(&id).as_bytes()).is_ok(), "{id}");
        }
        for id in [literal(65), escaped(65), surrogate(33)] {
            assert!(validate_envelope(manifest(&id).as_bytes()).is_err(), "{id}");
        }
    }

    #[test]
    fn search_query_limits_and_large_metadata() {
        let nodes = std::iter::repeat_n(r#"{"has":"note"}"#, 256)
            .collect::<Vec<_>>()
            .join(",");
        let scope = format!(
            r#"{{"kind":"search","generation":1,"sort":"date","scope":"all","query":{{"v":1,"q":{{"and":[{nodes}]}}}}}}"#
        );
        assert!(validate_envelope(envelope(&scope).as_bytes()).is_err());
        let huge = "x".repeat(sb_store::query::MAX_QUERY_INPUT_BYTES);
        let scope = format!(
            r#"{{"kind":"search","generation":1,"sort":"date","scope":"all","query":{{"v":1,"q":{{"text":"{huge}"}}}}}}"#
        );
        assert!(validate_envelope(envelope(&scope).as_bytes()).is_err());
        let document = envelope(r#"{"kind":"mailbox","id":1,"generation":1}"#).replace(
            r#""destination":"/d""#,
            &format!(r#""destination":"/{huge}""#),
        );
        assert!(validate_envelope(document.as_bytes()).is_ok());
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
