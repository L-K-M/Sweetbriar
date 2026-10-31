use std::io::{self, Cursor, Write};

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

#[test]
fn resumed_scans_keep_absolute_offsets_and_require_a_separator() {
    let mut source = ENVELOPE.to_vec();
    source.extend_from_slice(b"first\n");
    let second = source.len();
    source.extend_from_slice(ENVELOPE);
    source.extend_from_slice(b"second\n");
    let full = records(&source, 4);

    let resumed: Vec<_> = MboxScanner::resume(BufReader::new(&source[second..]), second as u64)
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(resumed, full[1..]);

    // The end of the source resumes to an empty tail.
    let len = source.len() as u64;
    assert_eq!(MboxScanner::resume(BufReader::new(&[][..]), len).count(), 0);

    // A recorded offset that is not a separator is refused, not skipped.
    let inside = second + 1;
    let mut scanner = MboxScanner::resume(BufReader::new(&source[inside..]), inside as u64);
    assert!(matches!(
        scanner.next(),
        Some(Err(MboxError::MissingSeparator { offset })) if offset == inside as u64
    ));
    assert!(scanner.next().is_none());
}

// ===== mboxrd encoder tests =====

const EXPORT_ENVELOPE: &[u8] = b"From MAILER-DAEMON Thu Jan  1 00:00:00 1970\n";

fn expected_record(payload: &[u8]) -> Vec<u8> {
    [EXPORT_ENVELOPE, payload].concat()
}

fn encode(input: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    write_mboxrd_record(input, &mut out).unwrap();
    out
}

fn encode_with_short_writes(input: &[u8], write_chunk: usize) -> Vec<u8> {
    let mut out = ShortSink::new(write_chunk);
    write_mboxrd_record(input, &mut out).unwrap();
    out.buffer
}

struct ShortSink {
    buffer: Vec<u8>,
    chunk_size: usize,
}

impl ShortSink {
    fn new(chunk_size: usize) -> Self {
        Self {
            buffer: Vec::new(),
            chunk_size,
        }
    }
}

impl Write for ShortSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let n = buf.len().min(self.chunk_size);
        self.buffer.extend_from_slice(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn encode_empty_input_produces_envelope_only() {
    let out = encode(b"");
    assert_eq!(out, EXPORT_ENVELOPE);
    let info = write_mboxrd_record(b"".as_slice(), &mut Vec::new()).unwrap();
    assert_eq!(info.original_bytes(), 0);
    assert_eq!(info.encoded_bytes(), EXPORT_ENVELOPE.len() as u64);
    assert!(!info.framing_lf_added());
}

#[test]
fn encode_simple_message_preserves_bytes() {
    let input = b"Subject: test\n\nbody\n";
    let out = encode(input);
    assert_eq!(out, expected_record(b"Subject: test\n\nbody\n"));
    let info = write_mboxrd_record(&input[..], &mut Vec::new()).unwrap();
    assert_eq!(info.original_bytes(), input.len() as u64);
    assert_eq!(
        info.encoded_bytes(),
        EXPORT_ENVELOPE.len() as u64 + input.len() as u64
    );
    assert!(!info.framing_lf_added());
}

#[test]
fn encode_from_line_gets_quoted() {
    let input = b"From quoted line\nnormal\n";
    let out = encode(input);
    assert_eq!(out, expected_record(b">From quoted line\nnormal\n"));
    let info = write_mboxrd_record(&input[..], &mut Vec::new()).unwrap();
    assert_eq!(info.original_bytes(), input.len() as u64);
    assert_eq!(
        info.encoded_bytes(),
        EXPORT_ENVELOPE.len() as u64 + input.len() as u64 + 1
    );
    assert!(!info.framing_lf_added());
}

#[test]
fn encode_gt_from_line_gets_extra_quote() {
    let input = b">From already quoted\n";
    let out = encode(input);
    assert_eq!(out, expected_record(b">>From already quoted\n"));
    let info = write_mboxrd_record(&input[..], &mut Vec::new()).unwrap();
    assert_eq!(info.original_bytes(), input.len() as u64);
    assert_eq!(
        info.encoded_bytes(),
        EXPORT_ENVELOPE.len() as u64 + input.len() as u64 + 1
    );
}

#[test]
fn encode_gt_gt_from_line_gets_extra_quote() {
    let input = b">>From double quoted\n";
    let out = encode(input);
    assert_eq!(out, expected_record(b">>>From double quoted\n"));
    let info = write_mboxrd_record(&input[..], &mut Vec::new()).unwrap();
    assert_eq!(info.original_bytes(), input.len() as u64);
    assert_eq!(
        info.encoded_bytes(),
        EXPORT_ENVELOPE.len() as u64 + input.len() as u64 + 1
    );
}

#[test]
fn encode_near_prefixes_are_not_quoted() {
    // These should NOT be quoted — they don't match `From ` exactly.
    let cases: &[(&[u8], bool)] = &[
        (b"From".as_slice(), true),  // no trailing LF → framing LF added
        (b"Fromx".as_slice(), true), // no trailing LF → framing LF added
        (b"From:no space\n".as_slice(), false),
        (b"from lower\n".as_slice(), false),
        (b"FROM upper\n".as_slice(), false),
        (b">Fromage\n".as_slice(), false),
        (b">>Fro\n".as_slice(), false),
        (b">From\n".as_slice(), false), // `From` without trailing space
    ];
    for (input, framing_lf) in cases {
        let out = encode(input);
        let mut expected = expected_record(input);
        if *framing_lf {
            expected.push(b'\n');
        }
        assert_eq!(out, expected, "input: {:?}", std::str::from_utf8(input));
    }
}

#[test]
fn encode_framing_lf_for_unterminated_input() {
    let input = b"no trailing newline";
    let out = encode(input);
    assert_eq!(out, expected_record(b"no trailing newline\n"));
    let info = write_mboxrd_record(&input[..], &mut Vec::new()).unwrap();
    assert_eq!(info.original_bytes(), input.len() as u64);
    assert_eq!(
        info.encoded_bytes(),
        EXPORT_ENVELOPE.len() as u64 + input.len() as u64 + 1
    );
    assert!(info.framing_lf_added());
}

#[test]
fn encode_framing_lf_not_added_when_input_ends_with_lf() {
    let input = b"ends with newline\n";
    let out = encode(input);
    assert_eq!(out, expected_record(b"ends with newline\n"));
    let info = write_mboxrd_record(&input[..], &mut Vec::new()).unwrap();
    assert!(!info.framing_lf_added());
}

#[test]
fn encode_preserves_crlf_and_binary_bytes() {
    let input = b"Header: val\r\n\r\nbody\r\n\xff\0binary\r\n";
    let out = encode(input);
    let expected = expected_record(input);
    assert_eq!(out, expected);
}

#[test]
fn encode_preserves_blank_lines() {
    let input = b"line1\n\n\nline4\n";
    let out = encode(input);
    let expected = expected_record(input);
    assert_eq!(out, expected);
}

#[test]
fn encode_all_byte_values() {
    let input: Vec<u8> = (0..=255).collect();
    let out = encode(&input);
    // Input ends with 0xFF (no trailing LF), so framing LF is added.
    let mut expected = expected_record(&input);
    expected.push(b'\n');
    assert_eq!(out, expected);
}

#[test]
fn encode_mixed_newlines() {
    let input = b"line1\r\nline2\nline3\rline4\n";
    let out = encode(input);
    let expected = expected_record(input);
    assert_eq!(out, expected);
}

#[test]
fn encode_roundtrip_through_mbox_scanner() {
    // Encode a message, then verify the scanner can recover the original bytes.
    let original = b"Subject: roundtrip\n\nFrom in body\n>From quoted\n>>From double\n";
    let mut encoded = Vec::new();
    let info = write_mboxrd_record(&original[..], &mut encoded).unwrap();
    assert_eq!(info.original_bytes(), original.len() as u64);

    // Scan the encoded mbox to find the record.
    let records: Vec<_> = MboxScanner::new(encoded.as_slice())
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(records.len(), 1);
    let record = &records[0];

    // The record bytes (after unquoting with mboxrd) should match original.
    let mut decoded = Vec::new();
    record
        .reader(Cursor::new(&encoded), MboxVariant::Mboxrd)
        .unwrap()
        .read_to_end(&mut decoded)
        .unwrap();
    assert_eq!(decoded, original);
}

#[test]
fn encode_roundtrip_with_framing_lf() {
    let original = b"no newline at end";
    let mut encoded = Vec::new();
    let info = write_mboxrd_record(&original[..], &mut encoded).unwrap();
    assert!(info.framing_lf_added());

    // The scanner sees the framing LF as part of the record.
    let records: Vec<_> = MboxScanner::new(encoded.as_slice())
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(records.len(), 1);
    let record = &records[0];

    let mut decoded = Vec::new();
    record
        .reader(Cursor::new(&encoded), MboxVariant::Mboxrd)
        .unwrap()
        .read_to_end(&mut decoded)
        .unwrap();
    // With framing LF, the decoded bytes include the added LF.
    // The spec says: "Only the declared framing LF may be removed."
    // Without manifest, the plain import retains the physical LF.
    let expected: Vec<u8> = original.iter().chain(b"\n".iter()).cloned().collect();
    assert_eq!(decoded, expected);
}

#[test]
fn encode_roundtrip_multi_message() {
    let messages: &[&[u8]] = &[
        b"First message\n".as_slice(),
        b"From quoted in second\n".as_slice(),
        b">From also quoted\n".as_slice(),
    ];
    let mut encoded = Vec::new();
    let mut infos = Vec::new();
    for msg in messages {
        let info = write_mboxrd_record(*msg, &mut encoded).unwrap();
        infos.push(info);
    }

    // Scan all records.
    let records: Vec<_> = MboxScanner::new(encoded.as_slice())
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(records.len(), messages.len());

    for (i, record) in records.iter().enumerate() {
        let mut decoded = Vec::new();
        record
            .reader(Cursor::new(&encoded), MboxVariant::Mboxrd)
            .unwrap()
            .read_to_end(&mut decoded)
            .unwrap();
        let expected: Vec<u8> = messages[i]
            .iter()
            .chain(if infos[i].framing_lf_added() {
                b"\n".iter()
            } else {
                b"".iter()
            })
            .cloned()
            .collect();
        assert_eq!(decoded, expected, "message {}", i);
    }
}

#[test]
fn encode_chunk_boundaries_do_not_affect_output() {
    let input = b"From test\n>From quoted\n>>From double\nnormal\n";
    let expected = encode(input);
    for capacity in 1..=32 {
        let source = ShortSource::new(input, capacity);
        let mut out = Vec::new();
        write_mboxrd_record(source, &mut out).unwrap();
        assert_eq!(out, expected, "read capacity {}", capacity);
    }
}

#[test]
fn encode_short_writes_do_not_affect_output() {
    let input = b"From test\n>From quoted\n>>From double\nnormal\n";
    let expected = encode(input);
    for chunk in 1..=16 {
        let out = encode_with_short_writes(input, chunk);
        assert_eq!(out, expected, "write chunk {}", chunk);
    }
}

#[test]
fn encode_huge_line_streams_correctly() {
    const LONG_LINE: usize = 100_000;
    let mut input = Vec::with_capacity(LONG_LINE);
    input.extend(std::iter::repeat_n(b'x', LONG_LINE));
    // No trailing newline — should get framing LF.
    let info = write_mboxrd_record(&input[..], &mut Vec::new()).unwrap();
    assert_eq!(info.original_bytes(), LONG_LINE as u64);
    assert_eq!(
        info.encoded_bytes(),
        EXPORT_ENVELOPE.len() as u64 + LONG_LINE as u64 + 1
    );
    assert!(info.framing_lf_added());
}

#[test]
fn encode_long_quote_runs_stream_correctly() {
    const QUOTE_RUN: usize = 100_000;
    let mut input = Vec::with_capacity(QUOTE_RUN + FROM_PREFIX.len() + 10);
    input.extend(std::iter::repeat_n(b'>', QUOTE_RUN));
    input.extend_from_slice(b"From end\n");
    let info = write_mboxrd_record(&input[..], &mut Vec::new()).unwrap();
    assert_eq!(
        info.original_bytes(),
        (QUOTE_RUN + FROM_PREFIX.len() + 4) as u64
    );
    // One extra > for quoting.
    assert_eq!(
        info.encoded_bytes(),
        EXPORT_ENVELOPE.len() as u64 + (QUOTE_RUN + FROM_PREFIX.len() + 4 + 1) as u64
    );
    assert!(!info.framing_lf_added());

    // Verify the output structure.
    let out = encode(&input);
    assert!(out.starts_with(EXPORT_ENVELOPE));
    // The extra > should be right after the envelope.
    let after_envelope = &out[EXPORT_ENVELOPE.len()..];
    assert_eq!(after_envelope[0], b'>'); // the extra quote
    // Then the original quote run.
    assert_eq!(&after_envelope[1..QUOTE_RUN + 1], &[b'>'; QUOTE_RUN]);
    // Then "From end\n".
    assert_eq!(&after_envelope[QUOTE_RUN + 1..], b"From end\n");
}

#[test]
fn encode_chunk_split_at_from_prefix_boundary() {
    // Test every possible split point within `From ` at line start.
    let input = b"From exact match\n";
    let expected = encode(input);
    for capacity in 1..=FROM_PREFIX.len() + 1 {
        let source = ShortSource::new(input, capacity);
        let mut out = Vec::new();
        write_mboxrd_record(source, &mut out).unwrap();
        assert_eq!(out, expected, "capacity {}", capacity);
    }
}

#[test]
fn encode_chunk_split_at_quoted_from_boundary() {
    // `>From ` split at every boundary.
    let input = b">From quoted\n";
    let expected = encode(input);
    for capacity in 1..=FROM_PREFIX.len() + 2 {
        let source = ShortSource::new(input, capacity);
        let mut out = Vec::new();
        write_mboxrd_record(source, &mut out).unwrap();
        assert_eq!(out, expected, "capacity {}", capacity);
    }
}

#[test]
fn encode_chunk_split_at_double_quoted_from_boundary() {
    let input = b">>From double\n";
    let expected = encode(input);
    for capacity in 1..=FROM_PREFIX.len() + 3 {
        let source = ShortSource::new(input, capacity);
        let mut out = Vec::new();
        write_mboxrd_record(source, &mut out).unwrap();
        assert_eq!(out, expected, "capacity {}", capacity);
    }
}

#[test]
fn encode_duplicate_messages_preserve_multiplicity() {
    let msg = b"Same content\n";
    let mut encoded = Vec::new();
    let info1 = write_mboxrd_record(&msg[..], &mut encoded).unwrap();
    let info2 = write_mboxrd_record(&msg[..], &mut encoded).unwrap();
    assert_eq!(info1, info2);

    let records: Vec<_> = MboxScanner::new(encoded.as_slice())
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(records.len(), 2);
}

#[test]
fn encode_empty_message_between_envelopes() {
    let mut encoded = Vec::new();
    write_mboxrd_record(b"".as_slice(), &mut encoded).unwrap();
    write_mboxrd_record(&b"second\n"[..], &mut encoded).unwrap();

    let records: Vec<_> = MboxScanner::new(encoded.as_slice())
        .collect::<Result<_, _>>()
        .unwrap();
    assert_eq!(records.len(), 2);
    assert!(records[0].byte_range().is_empty());
}

#[test]
fn encode_read_error_propagates_without_info() {
    let input = b"From content\n>From quoted\nunterminated";
    for fail_at in 0..input.len() as u64 {
        let mut source = ShortSource::new(input, 1);
        source.fail_at = Some(fail_at);
        let error = write_mboxrd_record(source, &mut Vec::new()).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Other, "{fail_at}");
    }
}

#[test]
fn encode_write_error_propagates_without_info() {
    let input = b"some content\n";
    let sink = FailingSink {
        buffer: Vec::new(),
        fail_after: 10,
    };
    let result = write_mboxrd_record(&input[..], sink);
    assert!(result.is_err());
}

#[test]
fn encode_interrupted_read_is_reported_at_every_input_position() {
    let input = b"From test\n>From quoted\n";
    for interrupt_at in 0..input.len() as u64 {
        let source = InterruptOnce {
            source: ShortSource::new(input, 1),
            interrupt_at,
            interrupted: false,
        };
        let mut out = Vec::new();
        let error = write_mboxrd_record(source, &mut out).unwrap_err();
        assert_eq!(error.kind(), io::ErrorKind::Interrupted, "{interrupt_at}");
    }
}

struct FailingSink {
    buffer: Vec<u8>,
    fail_after: usize,
}

impl Write for FailingSink {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        if self.buffer.len() >= self.fail_after {
            return Err(io::Error::other("injected write failure"));
        }
        let n = buf.len().min(self.fail_after - self.buffer.len());
        self.buffer.extend_from_slice(&buf[..n]);
        Ok(n)
    }

    fn flush(&mut self) -> io::Result<()> {
        if self.buffer.len() >= self.fail_after {
            return Err(io::Error::other("injected write failure on flush"));
        }
        Ok(())
    }
}

#[test]
fn encode_bounded_io_probes_huge_input_with_small_buffers() {
    // Verify bounded behavior: 1-byte read, 1-byte write on a large input.
    let size = 10_000;
    let input: Vec<u8> = (0..size).map(|i| (i % 256) as u8).collect();
    let source = ShortSource::new(&input, 1);
    let sink = ShortSink::new(1);
    let info = write_mboxrd_record(source, sink).unwrap();
    assert_eq!(info.original_bytes(), size as u64);
    assert_eq!(
        info.encoded_bytes(),
        EXPORT_ENVELOPE.len() as u64 + size as u64 + 1
    );
    assert!(info.framing_lf_added());
}

#[test]
fn encode_from_line_in_middle_of_message() {
    // `From ` only matters at the start of a line.
    let input = b"normal\nFrom in middle\nalso normal\n";
    let out = encode(input);
    assert_eq!(
        out,
        expected_record(b"normal\n>From in middle\nalso normal\n")
    );
}

#[test]
fn encode_from_not_at_line_start_is_not_quoted() {
    let input = b"prefix From not at start\n";
    let out = encode(input);
    let expected = expected_record(input);
    assert_eq!(out, expected);
}

#[test]
fn encode_info_encoded_bytes_matches_output() {
    let input = b"From test\n>From quoted\n>>From double\nno newline";
    let mut out = Vec::new();
    let info = write_mboxrd_record(&input[..], &mut out).unwrap();
    assert_eq!(info.encoded_bytes(), out.len() as u64);
    assert_eq!(info.original_bytes(), input.len() as u64);
}
