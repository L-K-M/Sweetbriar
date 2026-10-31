//! A native consumer of the public transport API and the existing decoder.

use std::io::{self, BufReader, Cursor, Read, Write};
use std::path::PathBuf;
use std::process::{Command, Stdio};

use sb_mime::{MboxScanner, MboxVariant, write_mboxrd_record};

const EXPORT_ENVELOPE: &[u8] = b"From MAILER-DAEMON Thu Jan  1 00:00:00 1970\n";

struct Chunks<'a> {
    bytes: &'a [u8],
    size: usize,
}

impl Read for Chunks<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        let count = self.bytes.len().min(out.len()).min(self.size);
        out[..count].copy_from_slice(&self.bytes[..count]);
        self.bytes = &self.bytes[count..];
        Ok(count)
    }
}

#[test]
fn public_records_recover_original_bytes_at_every_prefix_split() {
    let binary: Vec<u8> = (0..=u8::MAX).collect();
    let messages = [
        b"".as_slice(),
        b"From body\n>From quoted\r\n>>From twice\n",
        b"Fr>om \nF>rom \nFro>m \nFrom> \n>Fr>om \n",
        b"From\nFromage\n\0\xff\rFrom literal\n>>Fro",
        b"\n\r\n>\n>>>\r\n\n",
        b"unterminated",
        b"trailing CR\r",
        binary.as_slice(),
    ];
    for message in messages {
        for size in 1..=7 {
            let mut encoded = Vec::new();
            let info = write_mboxrd_record(
                Chunks {
                    bytes: message,
                    size,
                },
                &mut encoded,
            )
            .unwrap();
            assert_eq!(info.original_bytes(), message.len() as u64);
            assert_eq!(info.encoded_bytes(), encoded.len() as u64);
            assert_eq!(
                info.framing_lf_added(),
                !message.is_empty() && !message.ends_with(b"\n")
            );
            let records = MboxScanner::new(BufReader::with_capacity(size, encoded.as_slice()))
                .collect::<Result<Vec<_>, _>>()
                .unwrap();
            assert_eq!(records.len(), 1);
            let mut decoded = Vec::new();
            records[0]
                .reader(Cursor::new(&encoded), MboxVariant::Mboxrd)
                .unwrap()
                .read_to_end(&mut decoded)
                .unwrap();
            if info.framing_lf_added() {
                assert_eq!(decoded.pop(), Some(b'\n'));
            }
            assert_eq!(decoded, message, "read size {size}");
        }
    }
}

#[test]
fn adjacent_public_records_preserve_empty_messages_and_duplicate_multiplicity() {
    let messages = [b"tail".as_slice(), b"", b">From body\n", b"tail"];
    let mut encoded = Vec::new();
    let mut framing = Vec::new();
    for message in messages {
        framing.push(write_mboxrd_record(message, &mut encoded).unwrap());
    }
    let records = MboxScanner::new(BufReader::new(encoded.as_slice()))
        .collect::<Result<Vec<_>, _>>()
        .unwrap();
    assert_eq!(records.len(), messages.len());
    let mut offset = 0;
    for ((record, info), message) in records.iter().zip(&framing).zip(messages) {
        assert_eq!(record.separator_offset(), offset);
        offset += info.encoded_bytes();
        let mut decoded = Vec::new();
        record
            .reader(Cursor::new(&encoded), MboxVariant::Mboxrd)
            .unwrap()
            .read_to_end(&mut decoded)
            .unwrap();
        if info.framing_lf_added() {
            assert_eq!(decoded.pop(), Some(b'\n'));
        }
        assert_eq!(decoded, message);
    }
    assert_eq!(offset, encoded.len() as u64);
}

struct InterruptedRead {
    calls: usize,
}

impl Read for InterruptedRead {
    fn read(&mut self, _out: &mut [u8]) -> io::Result<usize> {
        self.calls += 1;
        if self.calls == 1 {
            return Err(io::ErrorKind::Interrupted.into());
        }
        Err(io::Error::other("read continued after interruption"))
    }
}

#[test]
fn source_interruption_returns_without_retrying_the_cancelled_stream() {
    let mut input = InterruptedRead { calls: 0 };
    let error = write_mboxrd_record(&mut input, &mut Vec::new()).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::Interrupted);
    assert_eq!(input.calls, 1);
}

struct ReadProbe<'a> {
    bytes: &'a [u8],
    largest_request: usize,
}

impl Read for ReadProbe<'_> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        self.largest_request = self.largest_request.max(out.len());
        self.bytes.read(out)
    }
}

#[derive(Default)]
struct WriteProbe {
    bytes: u64,
    calls: usize,
    largest_write: usize,
}

impl Write for WriteProbe {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        self.bytes += bytes.len() as u64;
        self.calls += 1;
        self.largest_write = self.largest_write.max(bytes.len());
        Ok(bytes.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn long_quote_runs_and_lines_use_bounded_io_not_byte_sized_writes() {
    const RUN_BYTES: usize = 1024 * 1024;
    const IO_BOUND: usize = 64 * 1024;
    let mut message = vec![b'>'; RUN_BYTES];
    message.extend_from_slice(b"From body\n");
    message.resize(message.len() + RUN_BYTES, b'x');
    let mut input = ReadProbe {
        bytes: &message,
        largest_request: 0,
    };
    let mut output = WriteProbe::default();
    let info = write_mboxrd_record(&mut input, &mut output).unwrap();
    assert_eq!(info.original_bytes(), message.len() as u64);
    // One quoted line and one framing LF, independent of the quote run.
    assert_eq!(
        info.encoded_bytes(),
        message.len() as u64 + EXPORT_ENVELOPE.len() as u64 + 2
    );
    assert_eq!(info.encoded_bytes(), output.bytes);
    assert!(info.framing_lf_added());
    assert!(input.largest_request <= IO_BOUND);
    assert!(output.largest_write <= IO_BOUND);
    assert!(output.calls < RUN_BYTES / 32, "output must be buffered");
}

struct ZeroWrite;

impl Write for ZeroWrite {
    fn write(&mut self, _bytes: &[u8]) -> io::Result<usize> {
        Ok(0)
    }

    fn flush(&mut self) -> io::Result<()> {
        Ok(())
    }
}

#[test]
fn a_sink_accepting_no_bytes_never_returns_record_info() {
    let error = write_mboxrd_record(b"message".as_slice(), ZeroWrite).unwrap_err();
    assert_eq!(error.kind(), io::ErrorKind::WriteZero);
}

struct SplitOutput(PathBuf);

impl Drop for SplitOutput {
    fn drop(&mut self) {
        std::fs::remove_dir_all(&self.0).expect("remove the test-owned split output");
    }
}

#[test]
fn an_independent_git_splitter_recognizes_the_transport_envelopes() {
    let root = std::env::temp_dir().join(format!("sb-mboxrd-split-{}", std::process::id()));
    std::fs::create_dir(&root).expect("create fresh split output");
    let root = SplitOutput(root);
    let messages = [
        b"From: first@example.test\r\nSubject: first\r\n\r\nfirst body\r\n".as_slice(),
        b"From: second@example.test\nSubject: second\n\nsecond body\n",
    ];
    let mut encoded = Vec::new();
    for message in messages {
        write_mboxrd_record(message, &mut encoded).unwrap();
    }
    let mut output_arg = std::ffi::OsString::from("-o");
    output_arg.push(&root.0);
    let mut child = Command::new("git")
        .args(["mailsplit", "--keep-cr"])
        .arg(output_arg)
        .stdin(Stdio::piped())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .spawn()
        .expect("Git is required for the independent envelope check");
    child.stdin.take().unwrap().write_all(&encoded).unwrap();
    let result = child.wait_with_output().unwrap();
    assert!(
        result.status.success(),
        "{}",
        String::from_utf8_lossy(&result.stderr)
    );
    let mut files = std::fs::read_dir(&root.0)
        .unwrap()
        .map(|entry| entry.unwrap().path())
        .collect::<Vec<_>>();
    files.sort();
    assert_eq!(files.len(), messages.len());
    for (file, original) in files.iter().zip(messages) {
        // mailsplit retains the Unix envelope; check it before comparing
        // the original payload, including its unchanged CRLF bytes.
        let split = std::fs::read(file).unwrap();
        assert_eq!(split.strip_prefix(EXPORT_ENVELOPE).unwrap(), original);
    }
}
