//! Byte-preserving mbox source decoding (spec/09). Inventory retains
//! offsets, not bodies; each body streams through the chosen unquoter.

use std::fmt;
use std::io::{self, BufRead, BufReader, Read, Seek, SeekFrom, Take};
use std::ops::Range;

const FROM_PREFIX: &[u8] = b"From ";

/// The source's escaping convention. Neither normalizes line endings.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MboxVariant {
    /// Only `>From ` loses its first `>`; `>>From ` stays unchanged.
    Mboxo,
    /// Every `>+From ` loses one `>`.
    Mboxrd,
}

/// What bytes alone can establish about the escaping convention.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MboxQuoting {
    /// Both variants produce identical bytes for this source.
    Equivalent,
    /// A `>>+From ` line exists: ask for the variant before ingesting.
    Ambiguous,
}

/// A complete inventory. Obtained only after a successful full scan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MboxInventory {
    messages: u64,
    source_bytes: u64,
    quoting: MboxQuoting,
}

impl MboxInventory {
    pub fn messages(&self) -> u64 {
        self.messages
    }

    pub fn source_bytes(&self) -> u64 {
        self.source_bytes
    }

    pub fn quoting(&self) -> MboxQuoting {
        self.quoting
    }
}

/// Count candidates and detect ambiguity without retaining message bytes
/// or a per-message index. The caller supplies a bounded buffered reader.
pub fn inventory_mbox(reader: impl BufRead) -> Result<MboxInventory, MboxError> {
    let mut scanner = MboxScanner::new(reader);
    let mut messages = 0;
    for record in scanner.by_ref() {
        record?;
        messages += 1;
    }
    Ok(MboxInventory {
        messages,
        source_bytes: scanner.position,
        quoting: scanner.quoting,
    })
}

/// A candidate's offsets in the original source. The range excludes its
/// envelope separator and includes every byte up to the next separator
/// or EOF, including blank lines and a missing final newline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MboxRecord {
    separator_offset: u64,
    start: u64,
    end: u64,
}

impl MboxRecord {
    /// Stable source-message identity for checkpoints and provenance.
    pub fn separator_offset(&self) -> u64 {
        self.separator_offset
    }

    pub fn byte_range(&self) -> Range<u64> {
        self.start..self.end
    }

    /// Stream this range through the selected unquoter. The caller must
    /// verify the source identity still matches its inventory. A source
    /// truncated inside the range yields `UnexpectedEof`, never success.
    pub fn reader<R: Read + Seek>(
        &self,
        mut source: R,
        variant: MboxVariant,
    ) -> io::Result<MboxReader<R>> {
        source.seek(SeekFrom::Start(self.start))?;
        Ok(MboxReader {
            source: BufReader::new(source.take(self.end - self.start)),
            variant,
            line_start: true,
            quotes: 0,
            prefix: [0; FROM_PREFIX.len()],
            prefix_start: 0,
            prefix_end: 0,
            failed: None,
        })
    }
}

/// A scan failure, retaining the source offset for the import report.
#[derive(Debug)]
pub enum MboxError {
    Io {
        offset: u64,
        source: io::Error,
    },
    /// Nonempty input must begin with a `From ` envelope line. Refuse
    /// preambles instead of silently dropping source bytes.
    MissingSeparator {
        offset: u64,
    },
    OffsetOverflow,
}

impl fmt::Display for MboxError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Io { offset, source } => write!(f, "cannot read mbox at byte {offset}: {source}"),
            Self::MissingSeparator { offset } => {
                write!(f, "expected a From separator at byte {offset}")
            }
            Self::OffsetOverflow => f.write_str("mbox source offset exceeds u64"),
        }
    }
}

impl std::error::Error for MboxError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Io { source, .. } => Some(source),
            _ => None,
        }
    }
}

/// A streaming, fused iterator over source ranges. `From ` at the start
/// of a line is an envelope separator, as in spec/09; quoted lines are
/// never separators. An I/O error ends the scan and is returned once.
pub struct MboxScanner<R> {
    source: R,
    position: u64,
    current: Option<(u64, u64)>,
    quoting: MboxQuoting,
    finished: bool,
}

impl<R: BufRead> MboxScanner<R> {
    pub fn new(source: R) -> Self {
        Self {
            source,
            position: 0,
            current: None,
            quoting: MboxQuoting::Equivalent,
            finished: false,
        }
    }

    fn next_record(&mut self) -> Result<Option<MboxRecord>, MboxError> {
        loop {
            let line_start = self.position;
            let Some(prefix) = scan_line(&mut self.source, &mut self.position)? else {
                self.finished = true;
                return Ok(self
                    .current
                    .take()
                    .map(|(separator_offset, start)| MboxRecord {
                        separator_offset,
                        start,
                        end: self.position,
                    }));
            };

            if prefix.matched == FROM_PREFIX.len() {
                if prefix.quotes > 1 {
                    self.quoting = MboxQuoting::Ambiguous;
                }
                if prefix.quotes == 0 {
                    let previous = self.current.replace((line_start, self.position));
                    if let Some((separator_offset, start)) = previous {
                        return Ok(Some(MboxRecord {
                            separator_offset,
                            start,
                            end: line_start,
                        }));
                    }
                    continue;
                }
            }
            if self.current.is_none() {
                return Err(MboxError::MissingSeparator { offset: line_start });
            }
        }
    }
}

impl<R: BufRead> Iterator for MboxScanner<R> {
    type Item = Result<MboxRecord, MboxError>;

    fn next(&mut self) -> Option<Self::Item> {
        if self.finished {
            return None;
        }
        match self.next_record() {
            Ok(record) => record.map(Ok),
            Err(error) => {
                self.finished = true;
                Some(Err(error))
            }
        }
    }
}

impl<R: BufRead> std::iter::FusedIterator for MboxScanner<R> {}

#[derive(Default)]
struct Prefix {
    quotes: u64,
    matched: usize,
    done: bool,
}

impl Prefix {
    fn feed(&mut self, bytes: &[u8]) {
        for &byte in bytes {
            if self.done {
                break;
            }
            if self.matched == 0 && byte == b'>' {
                self.quotes += 1;
                continue;
            }
            if byte != FROM_PREFIX[self.matched] {
                self.done = true;
                break;
            }
            self.matched += 1;
            self.done = self.matched == FROM_PREFIX.len();
        }
    }
}

/// Track only the prefix and offset, even for a multi-gigabyte line.
fn scan_line(reader: &mut impl BufRead, position: &mut u64) -> Result<Option<Prefix>, MboxError> {
    let start = *position;
    let mut prefix = Prefix::default();
    loop {
        let chunk = match reader.fill_buf() {
            Ok(chunk) => chunk,
            // A signal interrupted the read, not the source. Keep the
            // partially scanned prefix and resume at the same offset.
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            Err(source) => {
                return Err(MboxError::Io {
                    offset: *position,
                    source,
                });
            }
        };
        if chunk.is_empty() {
            return Ok((*position != start).then_some(prefix));
        }
        let newline = chunk.iter().position(|&byte| byte == b'\n');
        let consumed = newline.map_or(chunk.len(), |at| at + 1);
        *position = position
            .checked_add(consumed as u64)
            .ok_or(MboxError::OffsetOverflow)?;
        prefix.feed(&chunk[..consumed]);
        reader.consume(consumed);
        if newline.is_some() {
            return Ok(Some(prefix));
        }
    }
}

/// An unquoted message stream. Buffering is constant: one standard
/// `BufReader`, a quote count and at most five prefix bytes. Long quote
/// runs are counted, then emitted, rather than buffered as a whole line.
pub struct MboxReader<R: Read> {
    source: BufReader<Take<R>>,
    variant: MboxVariant,
    line_start: bool,
    quotes: u64,
    prefix: [u8; FROM_PREFIX.len()],
    prefix_start: usize,
    prefix_end: usize,
    failed: Option<io::ErrorKind>,
}

impl<R: Read> MboxReader<R> {
    fn prepare_prefix(&mut self) -> io::Result<()> {
        self.line_start = false;
        self.prefix_start = 0;
        self.prefix_end = 0;

        loop {
            let chunk = source_chunk(&mut self.source)?;
            let quotes = chunk.iter().take_while(|&&byte| byte == b'>').count();
            let more = quotes > 0 && quotes == chunk.len();
            self.quotes += quotes as u64;
            self.source.consume(quotes);
            if !more {
                break;
            }
        }
        for &expected in FROM_PREFIX {
            let Some(&byte) = source_chunk(&mut self.source)?.first() else {
                break;
            };
            self.source.consume(1);
            self.prefix[self.prefix_end] = byte;
            self.prefix_end += 1;
            if byte != expected {
                self.line_start = byte == b'\n';
                break;
            }
        }

        let escaped = self.quotes > 0
            && &self.prefix[..self.prefix_end] == FROM_PREFIX
            && (self.variant == MboxVariant::Mboxrd || self.quotes == 1);
        if escaped {
            self.quotes -= 1;
        }
        Ok(())
    }

    fn read_chunk(&mut self, output: &mut [u8]) -> io::Result<usize> {
        loop {
            if self.quotes > 0 {
                let count = self.quotes.min(output.len() as u64) as usize;
                output[..count].fill(b'>');
                self.quotes -= count as u64;
                return Ok(count);
            }
            if self.prefix_start < self.prefix_end {
                let count = output.len().min(self.prefix_end - self.prefix_start);
                output[..count]
                    .copy_from_slice(&self.prefix[self.prefix_start..self.prefix_start + count]);
                self.prefix_start += count;
                return Ok(count);
            }
            if self.line_start {
                self.prepare_prefix()?;
                continue;
            }

            let chunk = source_chunk(&mut self.source)?;
            let count = chunk.len().min(output.len());
            let count = chunk[..count]
                .iter()
                .position(|&byte| byte == b'\n')
                .map_or(count, |at| at + 1);
            output[..count].copy_from_slice(&chunk[..count]);
            self.line_start = count > 0 && chunk[count - 1] == b'\n';
            self.source.consume(count);
            return Ok(count);
        }
    }
}

impl<R: Read> Read for MboxReader<R> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if output.is_empty() {
            return Ok(0);
        }
        if let Some(kind) = self.failed {
            return Err(io::Error::new(
                kind,
                "mbox reader stopped after a source error",
            ));
        }
        let result = self.read_chunk(output);
        if let Err(error) = &result {
            self.failed = Some(error.kind());
        }
        result
    }
}

/// `Take` normally hides early EOF. A changed/truncated source must fail
/// instead of publishing a shorter message under the original range.
fn source_chunk<R: Read>(reader: &mut BufReader<Take<R>>) -> io::Result<&[u8]> {
    // Retry here so a partial quote/prefix stays inside one decoding
    // step. Latching Interrupted would make read_to_end retry forever.
    loop {
        match reader.fill_buf() {
            Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
            result => {
                result?;
                break;
            }
        }
    }
    let chunk = reader.buffer();
    if chunk.is_empty() && reader.get_ref().limit() > 0 {
        return Err(io::Error::new(
            io::ErrorKind::UnexpectedEof,
            "mbox source ended inside an inventoried message",
        ));
    }
    Ok(chunk)
}

#[cfg(test)]
mod tests;
