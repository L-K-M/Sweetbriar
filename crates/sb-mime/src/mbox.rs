//! Byte-preserving mbox source decoding (spec/09). Inventory retains
//! offsets, not bodies; each body streams through the chosen unquoter.

use std::fmt;
use std::io::{self, BufRead, BufReader, BufWriter, Read, Seek, SeekFrom, Take, Write};
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

    /// Continue at `offset`, a separator an earlier scan of the same
    /// verified bytes reported, with `source` already positioned there.
    /// Offsets stay absolute; a non-separator is `MissingSeparator`.
    pub fn resume(source: R, offset: u64) -> Self {
        Self {
            position: offset,
            ..Self::new(source)
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

/// Record metadata returned after a successful `write_mboxrd_record`.
/// Fields are private; use the getters.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct MboxrdRecordInfo {
    original_bytes: u64,
    encoded_bytes: u64,
    framing_lf_added: bool,
}

impl MboxrdRecordInfo {
    /// Bytes consumed from the input (the original acquisition size).
    pub fn original_bytes(&self) -> u64 {
        self.original_bytes
    }

    /// Total output bytes: envelope, added `>` quotes and framing LF included.
    pub fn encoded_bytes(&self) -> u64 {
        self.encoded_bytes
    }

    /// True when exactly one LF was appended because the input did not
    /// end with LF. Empty input never sets this.
    pub fn framing_lf_added(&self) -> bool {
        self.framing_lf_added
    }
}

/// Fixed I/O buffers keep memory independent of message and line length.
const ENCODE_READ_CAP: usize = 8192;

/// Bounded write buffer for the mboxrd encoder.
const ENCODE_WRITE_CAP: usize = 8192;
const QUOTE_BLOCK: [u8; ENCODE_WRITE_CAP] = [b'>'; ENCODE_WRITE_CAP];

/// Encode one mboxrd record with a conventional fixed envelope, quoting
/// any input line that starts with
/// zero or more `>` followed by `From ` by prepending one `>`.
///
/// On success, returns [`MboxrdRecordInfo`] with the byte counts.
/// On I/O error, propagates the error without returning info.
/// Read interruption ends the record so a cancelled acquisition stream
/// cannot be retried indefinitely. The caller owns sync and publication.
pub fn write_mboxrd_record(input: impl Read, output: impl Write) -> io::Result<MboxrdRecordInfo> {
    const ENVELOPE: &[u8] = b"From MAILER-DAEMON Thu Jan  1 00:00:00 1970\n";

    let mut buf_in = BufReader::with_capacity(ENCODE_READ_CAP, input);
    let mut buf_out = BufWriter::with_capacity(ENCODE_WRITE_CAP, output);

    buf_out.write_all(ENVELOPE)?;

    let mut original_bytes: u64 = 0;
    let mut encoded_bytes: u64 = ENVELOPE.len() as u64;
    let mut framing_lf_added = false;
    let mut input_empty = true;
    let mut at_line_start = true;

    loop {
        if at_line_start {
            // At line start: count leading `>` and check for `From ` prefix.
            // The scanner consumes the prefix bytes from the buffer; we emit
            // them here (with an extra `>` if needed) so the main loop doesn't
            // re-read them.
            let (quote_count, prefix_buf, prefix_len, need_quote, hit_newline) =
                scan_line_prefix(&mut buf_in)?;

            // If we need to quote, emit one extra `>` before the original `>`s.
            if need_quote {
                buf_out.write_all(b">")?;
                encoded_bytes = encoded_bytes
                    .checked_add(1)
                    .ok_or_else(|| io::Error::other("encoded byte count overflow"))?;
            }

            // Emit the `>` bytes we counted (these were consumed from input).
            if quote_count > 0 {
                let mut remaining = quote_count;
                while remaining > 0 {
                    let to_write = remaining.min(QUOTE_BLOCK.len() as u64) as usize;
                    let n = buf_out.write(&QUOTE_BLOCK[..to_write])?;
                    if n == 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::WriteZero,
                            "failed to write quote bytes",
                        ));
                    }
                    encoded_bytes = encoded_bytes
                        .checked_add(n as u64)
                        .ok_or_else(|| io::Error::other("encoded byte count overflow"))?;
                    remaining -= n as u64;
                }
                original_bytes = original_bytes
                    .checked_add(quote_count)
                    .ok_or_else(|| io::Error::other("original byte count overflow"))?;
            }

            // Emit any prefix bytes we buffered (these were consumed from input).
            if prefix_len > 0 {
                let mut off = 0;
                while off < prefix_len {
                    let n = buf_out.write(&prefix_buf[off..prefix_len])?;
                    if n == 0 {
                        return Err(io::Error::new(
                            io::ErrorKind::WriteZero,
                            "failed to write prefix bytes",
                        ));
                    }
                    encoded_bytes = encoded_bytes
                        .checked_add(n as u64)
                        .ok_or_else(|| io::Error::other("encoded byte count overflow"))?;
                    off += n;
                }
                original_bytes = original_bytes
                    .checked_add(prefix_len as u64)
                    .ok_or_else(|| io::Error::other("original byte count overflow"))?;
            }

            // If we consumed nothing and didn't see a newline, we're at EOF.
            if quote_count == 0 && prefix_len == 0 && !hit_newline {
                break;
            }

            if quote_count > 0 || prefix_len > 0 {
                input_empty = false;
                at_line_start = false;
            }
        }

        // Main streaming: copy bytes until the next `\n` (inclusive).
        let chunk = buf_in.fill_buf()?;
        if chunk.is_empty() {
            break;
        }

        let newline_pos = chunk.iter().position(|&b| b == b'\n');
        let to_consume = newline_pos.map_or(chunk.len(), |pos| pos + 1);

        let mut off = 0;
        while off < to_consume {
            let n = buf_out.write(&chunk[off..to_consume])?;
            if n == 0 {
                return Err(io::Error::new(
                    io::ErrorKind::WriteZero,
                    "failed to write body bytes",
                ));
            }
            off += n;
        }
        let consumed = to_consume;
        buf_in.consume(consumed);
        original_bytes = original_bytes
            .checked_add(consumed as u64)
            .ok_or_else(|| io::Error::other("original byte count overflow"))?;
        encoded_bytes = encoded_bytes
            .checked_add(consumed as u64)
            .ok_or_else(|| io::Error::other("encoded byte count overflow"))?;
        input_empty = false;

        if newline_pos.is_some() {
            at_line_start = true;
        }
    }

    // If input was non-empty and didn't end with `\n`, append one framing LF.
    if !input_empty && !at_line_start {
        buf_out.write_all(b"\n")?;
        encoded_bytes = encoded_bytes
            .checked_add(1)
            .ok_or_else(|| io::Error::other("encoded byte count overflow"))?;
        framing_lf_added = true;
    }

    buf_out.flush()?;

    Ok(MboxrdRecordInfo {
        original_bytes,
        encoded_bytes,
        framing_lf_added,
    })
}

/// At the start of a line, count leading `>` and peek at the next
/// `FROM_PREFIX.len()` bytes to decide whether this line needs quoting.
/// Consumes the scanned bytes from the reader.
/// Returns `(quote_count, prefix_bytes, prefix_len, needs_quote, hit_newline)`.
///
/// If the first non-`>` byte is `\n`, the `>` bytes are consumed but the
/// `\n` is left for the main streaming loop, which will emit it and then
/// call this function again for the following line. `hit_newline` signals
/// that a `\n` is available but not consumed.
fn scan_line_prefix<R: BufRead>(
    reader: &mut R,
) -> io::Result<(u64, [u8; FROM_PREFIX.len()], usize, bool, bool)> {
    let mut quote_count = 0u64;
    let mut prefix_buf = [0u8; FROM_PREFIX.len()];
    let mut prefix_len = 0;

    loop {
        let chunk = reader.fill_buf()?;
        if chunk.is_empty() {
            return Ok((quote_count, prefix_buf, prefix_len, false, false));
        }

        let mut i = 0;
        // Once a non-quote prefix began, later `>` bytes belong to it.
        // Counting them as leading quotes would reorder chunked input.
        while prefix_len == 0 && i < chunk.len() && chunk[i] == b'>' {
            quote_count = quote_count.checked_add(1).ok_or_else(|| {
                io::Error::new(io::ErrorKind::InvalidData, "mboxrd quote count exceeds u64")
            })?;
            i += 1;
        }
        if i == chunk.len() {
            reader.consume(i);
            continue;
        }

        // The first non-`>` byte.
        if chunk[i] == b'\n' {
            // Keep LF for the copy loop, without dropping a prefix from
            // an earlier chunk (for example `Fro` followed by LF).
            reader.consume(i);
            return Ok((quote_count, prefix_buf, prefix_len, false, true));
        }

        // Stop at `\n` or `FROM_PREFIX.len()` bytes, whichever comes first.
        // Compute everything we need from `chunk` before consuming.
        let nl_pos = chunk[i..].iter().position(|&b| b == b'\n');
        let available = nl_pos.unwrap_or(chunk.len() - i);
        let limit = available.min(FROM_PREFIX.len() - prefix_len);
        let hit_nl = nl_pos.is_some() && nl_pos.unwrap() < chunk.len() - i;

        if limit > 0 {
            prefix_buf[prefix_len..prefix_len + limit].copy_from_slice(&chunk[i..i + limit]);
            prefix_len += limit;
            reader.consume(i + limit);
        } else {
            reader.consume(i);
        }

        if prefix_len == FROM_PREFIX.len() {
            let needs_quote = prefix_buf == FROM_PREFIX;
            return Ok((quote_count, prefix_buf, prefix_len, needs_quote, false));
        }
        // If we stopped at a newline, signal it so the main loop handles `\n`.
        if hit_nl {
            return Ok((quote_count, prefix_buf, prefix_len, false, true));
        }
        // Not enough bytes yet — continue reading across chunk boundaries.
    }
}

#[cfg(test)]
mod tests;
