#![no_main]

use std::io::{BufReader, Cursor, Read};

use libfuzzer_sys::fuzz_target;
use sb_mime::{MboxQuoting, MboxScanner, MboxVariant, inventory_mbox};

fuzz_target!(|data: &[u8]| {
    // Always enter the body path; arbitrary input may add more separators.
    let envelope = b"From fuzz\n";
    let mut source = envelope.to_vec();
    source.extend_from_slice(data);
    let capacity = 1 + data.first().copied().unwrap_or(0) as usize;
    let records = MboxScanner::new(BufReader::with_capacity(capacity, source.as_slice()))
        .collect::<Result<Vec<_>, _>>()
        .expect("an in-memory mbox beginning with a separator must scan");
    let inventory = inventory_mbox(source.as_slice()).unwrap();
    assert_eq!(inventory.messages(), records.len() as u64);
    assert_eq!(inventory.source_bytes(), source.len() as u64);
    let ambiguous = source.split_inclusive(|&b| b == b'\n').any(|line| {
        let quotes = line.iter().take_while(|&&b| b == b'>').count();
        quotes > 1 && line[quotes..].starts_with(b"From ")
    });
    assert_eq!(inventory.quoting() == MboxQuoting::Ambiguous, ambiguous);

    let mut end = 0;
    for record in records {
        let range = record.byte_range();
        assert_eq!(record.separator_offset(), end);
        assert!(range.start > record.separator_offset());
        assert!(range.end >= range.start);
        let bytes = &source[range.start as usize..range.end as usize];
        end = range.end;

        for variant in [MboxVariant::Mboxo, MboxVariant::Mboxrd] {
            // Deliberately whole-line reference, independent of the
            // production reader's incremental prefix state machine.
            let mut expected = Vec::new();
            for line in bytes.split_inclusive(|&b| b == b'\n') {
                let quotes = line.iter().take_while(|&&b| b == b'>').count();
                let unquote = quotes > 0
                    && line[quotes..].starts_with(b"From ")
                    && (variant == MboxVariant::Mboxrd || quotes == 1);
                expected.extend_from_slice(&line[usize::from(unquote)..]);
            }
            let mut decoded = Vec::new();
            record
                .reader(Cursor::new(&source), variant)
                .unwrap()
                .read_to_end(&mut decoded)
                .unwrap();
            assert_eq!(decoded, expected, "unquoting changed message bytes");
        }
    }
    assert_eq!(end, source.len() as u64);

    // Also exercise the real malformed-input boundary, including preambles.
    let original = inventory_mbox(BufReader::with_capacity(capacity, data));
    assert_eq!(
        original.is_ok(),
        data.is_empty() || data.starts_with(b"From ")
    );
});
