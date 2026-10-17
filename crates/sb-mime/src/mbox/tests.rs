use std::io::Cursor;

use sha2::{Digest, Sha256};

use super::*;

const ENVELOPE: &[u8] = b"From sender@example.test Sat Oct 17 12:00:00 2026\n";
const QUOTING: &[u8] = include_bytes!("../../../../fixtures/import/quoting.mbox");
const DUPLICATES: &[u8] = include_bytes!("../../../../fixtures/import/Inbox.mbox/mbox");

fn records(bytes: &[u8], capacity: usize) -> Vec<MboxRecord> {
    MboxScanner::new(BufReader::with_capacity(capacity, bytes))
        .collect::<Result<_, _>>()
        .unwrap()
}

fn decode(record: &MboxRecord, bytes: &[u8], variant: MboxVariant) -> Vec<u8> {
    let mut output = Vec::new();
    record
        .reader(Cursor::new(bytes), variant)
        .unwrap()
        .read_to_end(&mut output)
        .unwrap();
    output
}

#[test]
fn fixture_bytes_and_hashes_match_each_variant() {
    let record = records(QUOTING, 7)[0];
    let cases = [
        (
            MboxVariant::Mboxo,
            include_bytes!("../../../../fixtures/import/expected/mboxo.eml").as_slice(),
            "69afa20c11a485abc5a04cace01ff8b167658ea89ac7d3e3a31f0fc709cca413",
        ),
        (
            MboxVariant::Mboxrd,
            include_bytes!("../../../../fixtures/import/expected/mboxrd.eml").as_slice(),
            "1786e24a2decd0fa5fa2b4f3d22faecdc36357d8b117bb6e083a2fe68a329873",
        ),
    ];
    for (variant, expected, sha256) in cases {
        let output = decode(&record, QUOTING, variant);
        assert_eq!(output, expected);
        // Expected messages are authored separately from the splitter.
        let hash: String = Sha256::digest(&output)
            .iter()
            .map(|byte| format!("{byte:02x}"))
            .collect();
        assert_eq!(hash, sha256);
    }
    assert_ne!(
        decode(&record, QUOTING, MboxVariant::Mboxo),
        decode(&record, QUOTING, MboxVariant::Mboxrd)
    );
}

#[test]
fn offsets_preserve_duplicate_candidates_and_source_bytes() {
    let found = records(DUPLICATES, 3);
    assert_eq!(found.len(), 2);
    let half = DUPLICATES.len() as u64 / 2;
    assert_eq!(found[0].separator_offset(), 0);
    assert_eq!(found[0].byte_range(), ENVELOPE.len() as u64..half);
    assert_eq!(found[1].separator_offset(), half);
    assert_eq!(
        found[1].byte_range(),
        half + ENVELOPE.len() as u64..DUPLICATES.len() as u64
    );
    let first = decode(&found[0], DUPLICATES, MboxVariant::Mboxrd);
    let second = decode(&found[1], DUPLICATES, MboxVariant::Mboxrd);
    assert_eq!(first, second);
    assert_eq!(first, &DUPLICATES[ENVELOPE.len()..half as usize]);
    let inventory = inventory_mbox(BufReader::new(DUPLICATES)).unwrap();
    assert_eq!(inventory.messages(), 2);
    assert_eq!(inventory.source_bytes(), DUPLICATES.len() as u64);
    assert_eq!(inventory.quoting(), MboxQuoting::Equivalent);
}

#[test]
fn chunk_and_output_boundaries_do_not_change_bytes() {
    let expected_records = records(QUOTING, QUOTING.len());
    for capacity in 1..=FROM_PREFIX.len() * 2 {
        assert_eq!(records(QUOTING, capacity), expected_records);
        for variant in [MboxVariant::Mboxo, MboxVariant::Mboxrd] {
            let expected = decode(&expected_records[0], QUOTING, variant);
            for output_size in 1..=FROM_PREFIX.len() + 1 {
                let source = ShortSource::new(QUOTING, capacity);
                let mut reader = expected_records[0].reader(source, variant).unwrap();
                let mut output = Vec::new();
                let mut chunk = vec![0; output_size];
                loop {
                    let count = reader.read(&mut chunk).unwrap();
                    if count == 0 {
                        break;
                    }
                    output.extend_from_slice(&chunk[..count]);
                }
                assert_eq!(output, expected, "{capacity}/{output_size}/{variant:?}");
            }
        }
    }
}

#[test]
fn preserves_binary_line_endings_blank_lines_and_unterminated_tails() {
    let messages = [
        b"Header: value\r\n\r\n>From body\r\n\r\n\xff\0tail".as_slice(),
        b"\n>From body\n\n".as_slice(),
        b">Fro".as_slice(),
        b">".as_slice(),
        b">Fromage\n From literal\rFrom literal".as_slice(),
    ];
    for message in messages {
        let mut source = ENVELOPE.to_vec();
        source.extend_from_slice(message);
        let record = records(&source, 1)[0];
        let expected: Vec<u8> = message
            .split_inclusive(|&b| b == b'\n')
            .flat_map(|line| line.strip_prefix(b">From ").map_or(line, |_| &line[1..]))
            .copied()
            .collect();
        for variant in [MboxVariant::Mboxo, MboxVariant::Mboxrd] {
            assert_eq!(decode(&record, &source, variant), expected);
        }
    }
}

#[test]
fn empty_sources_and_empty_candidates_are_distinct() {
    let inventory = inventory_mbox(b"".as_slice()).unwrap();
    assert_eq!(inventory.messages(), 0);
    assert_eq!(inventory.source_bytes(), 0);

    let source = b"From first\nFrom second";
    let found = records(source, 1);
    assert_eq!(found.len(), 2);
    for record in found {
        assert!(record.byte_range().is_empty());
        assert!(decode(&record, source, MboxVariant::Mboxrd).is_empty());
    }
}

#[test]
fn preambles_are_rejected_instead_of_dropped() {
    for source in [
        b"\nFrom first\n".as_slice(),
        b"Subject: not mbox\n",
        b"From",
    ] {
        let mut scanner = MboxScanner::new(source);
        assert!(matches!(
            scanner.next(),
            Some(Err(MboxError::MissingSeparator { offset: 0 }))
        ));
        assert!(scanner.next().is_none());
        assert!(inventory_mbox(source).is_err());
    }
}

#[test]
fn only_differently_decoded_prefixes_require_a_choice() {
    for (body, quoting) in [
        (b">From once\n".as_slice(), MboxQuoting::Equivalent),
        (b">>From twice\n", MboxQuoting::Ambiguous),
        (b">>>From thrice", MboxQuoting::Ambiguous),
        (b">>Fromage\n>>Fro\n x >>From text", MboxQuoting::Equivalent),
    ] {
        let mut source = ENVELOPE.to_vec();
        source.extend_from_slice(body);
        assert_eq!(
            inventory_mbox(source.as_slice()).unwrap().quoting(),
            quoting
        );
    }
}

#[test]
fn huge_lines_and_quote_runs_stream_without_line_buffers() {
    const LONG_RUN: usize = 1_000_000;
    let mut source = ENVELOPE.to_vec();
    source.extend(std::iter::repeat_n(b'>', LONG_RUN));
    source.extend_from_slice(b"From long\n");
    source.extend(std::iter::repeat_n(b'x', LONG_RUN));
    let record = records(&source, 17)[0];
    for (variant, removed) in [(MboxVariant::Mboxo, 0), (MboxVariant::Mboxrd, 1)] {
        let mut reader = record.reader(Cursor::new(&source), variant).unwrap();
        assert_eq!(reader.read(&mut []).unwrap(), 0);
        let mut output = Vec::new();
        reader.read_to_end(&mut output).unwrap();
        assert_eq!(output, source[ENVELOPE.len() + removed..]);
    }
}

#[test]
fn scanning_io_errors_retain_offsets_and_end_the_iterator() {
    let fail_at = ENVELOPE.len() + 3;
    let mut source = ShortSource::new(QUOTING, 1);
    source.fail_at = Some(fail_at as u64);
    let mut scanner = MboxScanner::new(BufReader::with_capacity(1, source));
    assert!(matches!(
        scanner.next(),
        Some(Err(MboxError::Io { offset, source }))
            if offset == fail_at as u64 && source.kind() == io::ErrorKind::Other
    ));
    assert!(scanner.next().is_none());
}

#[test]
fn changed_source_truncation_and_io_errors_cannot_look_complete() {
    let record = records(QUOTING, 1)[0];
    let source = &QUOTING[..QUOTING.len() - 1];
    let mut reader = record
        .reader(Cursor::new(source), MboxVariant::Mboxrd)
        .unwrap();
    let error = reader.read_to_end(&mut Vec::new()).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::UnexpectedEof);
    assert_eq!(reader.read(&mut [0]).unwrap_err().kind(), error.kind());

    let mut source = ShortSource::new(QUOTING, 1);
    source.fail_at = Some(ENVELOPE.len() as u64 + 3);
    let mut reader = record.reader(source, MboxVariant::Mboxrd).unwrap();
    assert_eq!(
        reader.read_to_end(&mut Vec::new()).unwrap_err().kind(),
        io::ErrorKind::Other
    );
}

#[test]
fn offset_overflow_is_an_error() {
    assert!(matches!(
        scan_line(&mut b"xx".as_slice(), &mut (u64::MAX - 1)),
        Err(MboxError::OffsetOverflow)
    ));
}

#[test]
fn interrupted_source_reads_are_retried_without_poisoning_the_decoder() {
    let record = records(QUOTING, 1)[0];
    for interrupt_at in record.byte_range() {
        let source = InterruptOnce {
            source: ShortSource::new(QUOTING, 1),
            interrupt_at,
            interrupted: false,
        };
        let mut reader = record.reader(source, MboxVariant::Mboxrd).unwrap();
        let mut first = [0];
        assert_eq!(reader.read(&mut first).unwrap(), 1);
        let mut output = first.to_vec();
        reader.read_to_end(&mut output).unwrap();
        assert_eq!(output, decode(&record, QUOTING, MboxVariant::Mboxrd));
    }
}

#[test]
fn interrupted_scans_keep_their_partial_line_and_offset() {
    let expected = records(QUOTING, 1);
    for interrupt_at in 0..QUOTING.len() as u64 {
        let source = InterruptOnce {
            source: ShortSource::new(QUOTING, 1),
            interrupt_at,
            interrupted: false,
        };
        let actual = MboxScanner::new(BufReader::with_capacity(1, source))
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(actual, expected);
    }
}

struct InterruptOnce<'a> {
    source: ShortSource<'a>,
    interrupt_at: u64,
    interrupted: bool,
}

impl Read for InterruptOnce<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if !self.interrupted && self.source.cursor.position() >= self.interrupt_at {
            self.interrupted = true;
            return Err(io::Error::from(io::ErrorKind::Interrupted));
        }
        self.source.read(output)
    }
}

impl Seek for InterruptOnce<'_> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.source.seek(position)
    }
}

struct ShortSource<'a> {
    cursor: Cursor<&'a [u8]>,
    chunk_size: usize,
    fail_at: Option<u64>,
}

impl<'a> ShortSource<'a> {
    fn new(bytes: &'a [u8], chunk_size: usize) -> Self {
        Self {
            cursor: Cursor::new(bytes),
            chunk_size,
            fail_at: None,
        }
    }
}

impl Read for ShortSource<'_> {
    fn read(&mut self, output: &mut [u8]) -> io::Result<usize> {
        if self.fail_at.is_some_and(|at| self.cursor.position() >= at) {
            return Err(io::Error::other("injected source failure"));
        }
        let count = output.len().min(self.chunk_size);
        self.cursor.read(&mut output[..count])
    }
}

impl Seek for ShortSource<'_> {
    fn seek(&mut self, position: SeekFrom) -> io::Result<u64> {
        self.cursor.seek(position)
    }
}
