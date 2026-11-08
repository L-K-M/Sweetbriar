//! Native framing proof using synthetic byte sources and owned unpublished
//! sinks. No network, MIME parser, Store or completed-command claim.

use std::io;
use std::pin::Pin;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::task::{Context, Poll};

use tokio::io::ReadBuf;

use super::*;

fn uid() -> NonZeroU32 {
    NonZeroU32::new(7).unwrap()
}

fn reader() -> BodyReader<Vec<u8>> {
    BodyReader::new(uid(), Vec::new())
}

fn literal_frame(body: &[u8], tail: &[u8]) -> Vec<u8> {
    let mut wire = format!("* 2 FETCH (BODY[] {{{}}}\r\n", body.len()).into_bytes();
    wire.extend_from_slice(body);
    wire.extend_from_slice(tail);
    wire
}

/// Force every possible split, including one-byte fragments, without
/// changing the bytes. A later frame remains in the same source.
struct Fragmented<'a> {
    bytes: &'a [u8],
    chunk: usize,
}

impl AsyncRead for Fragmented<'_> {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        out: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        let count = self.bytes.len().min(self.chunk).min(out.remaining());
        out.put_slice(&self.bytes[..count]);
        self.bytes = &self.bytes[count..];
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn literal_uid_after_body_raw_bytes_and_coalesced_tail_survive() {
    let body = b"Subject: caf\xc3\xa9\r\n\r\n* BYE is body text\r\nX9 OK fake\r\n\xff";
    let wire = literal_frame(body, b" UID 7)\r\nA1 OK done\r\n* 8 EXISTS\r\n");
    for chunk in 1..=wire.len() {
        let mut source = Fragmented {
            bytes: &wire,
            chunk,
        };
        let mut reader = reader();
        let frame = reader.next(&mut source).await.unwrap().unwrap();
        assert_eq!(frame.body, Some(BodyEvidence::Bytes(body.len() as u64)));
        assert_eq!(frame.frame.bytes, b"* 2 FETCH (BODY[] {0}\r\n UID 7)\r\n");
        assert!(matches!(frame.frame.kind, FrameKind::Untagged));
        let frame = reader.next(&mut source).await.unwrap().unwrap();
        assert!(matches!(frame.frame.kind, FrameKind::Completion));
        assert!(frame.body.is_none());
        let (sink, buffered) = reader.into_parts().unwrap();
        assert_eq!(sink, body);
        let tail = [buffered.as_slice(), source.bytes].concat();
        assert_eq!(tail, b"* 8 EXISTS\r\n");
    }
}

#[tokio::test]
async fn quoted_bytes_unescape_and_utf8_survives_each_split() {
    let wire = "* 1 FETCH (UID 7 BODY[] \"caf\u{e9} \\\"x\\\" \\\\ end\")\r\n".as_bytes();
    for chunk in 1..=wire.len() {
        let mut source = Fragmented { bytes: wire, chunk };
        let mut reader = reader();
        let frame = reader.next(&mut source).await.unwrap().unwrap();
        let expected = "café \"x\" \\ end".as_bytes();
        assert_eq!(frame.body, Some(BodyEvidence::Bytes(expected.len() as u64)));
        assert_eq!(reader.into_parts().unwrap().0, expected);
    }
}

#[tokio::test]
async fn quoted_utf8_carries_an_incomplete_codepoint_across_output_buffers() {
    let mut expected = vec![b'x'; CHUNK - 1];
    expected.extend_from_slice("é🙂".as_bytes());
    let mut wire = b"* 1 FETCH (UID 7 BODY[] \"".to_vec();
    wire.extend_from_slice(&expected);
    wire.extend_from_slice(b"\")\r\n");
    let mut reader = reader();
    assert_eq!(
        reader
            .next(&mut wire.as_slice())
            .await
            .unwrap()
            .unwrap()
            .body,
        Some(BodyEvidence::Bytes(expected.len() as u64))
    );
    assert_eq!(reader.into_parts().unwrap().0, expected);
}

#[tokio::test]
async fn nil_empty_literal_and_empty_quote_are_different_evidence() {
    for (wire, evidence) in [
        (
            &b"* 1 FETCH (UID 7 BODY[] NIL)\r\n"[..],
            BodyEvidence::Unavailable,
        ),
        (
            &b"* 1 FETCH (UID 7 BODY[] {0}\r\n)\r\n"[..],
            BodyEvidence::Bytes(0),
        ),
        (
            &b"* 1 FETCH (UID 7 BODY[] \"\")\r\n"[..],
            BodyEvidence::Bytes(0),
        ),
    ] {
        let mut reader = reader();
        let response = reader.next(&mut &wire[..]).await.unwrap().unwrap();
        assert_eq!(response.body, Some(evidence));
        assert!(reader.into_parts().unwrap().0.is_empty());
    }
}

#[tokio::test]
async fn ordinary_metadata_literals_and_status_braces_are_not_body_data() {
    for wire in [
        &b"* LIST () \"/\" {4}\r\nTest\r\n"[..],
        &b"* OK status ends in {999999999}\r\n"[..],
        &b"A1 OK tagged status {999999999}\r\n"[..],
        &b"+ continuation {999999999}\r\n"[..],
        &b"* 1 FETCH (FLAGS (\\Seen) UID 9)\r\n"[..],
    ] {
        let mut reader = reader();
        let response = reader.next(&mut &wire[..]).await.unwrap().unwrap();
        assert!(response.body.is_none());
        assert_eq!(response.frame.bytes, wire);
        assert!(reader.into_parts().unwrap().0.is_empty());
    }
    let wire = b"* LIST () \"/\" \"BODY[] \\\"decoy\"\r\n";
    let mut reader = reader();
    assert!(
        reader
            .next(&mut wire.as_slice())
            .await
            .unwrap()
            .unwrap()
            .body
            .is_none()
    );
    assert!(reader.into_parts().unwrap().0.is_empty());
}

#[tokio::test]
async fn body_like_metadata_literal_suffix_cannot_become_a_body_candidate() {
    for message_id in [&b"\"message-id\""[..], &b"{10}\r\nmessage-id"[..]] {
        let mut wire =
            b"* 1 FETCH (ENVELOPE (\"date\" \"subject\" NIL NIL NIL NIL NIL NIL {6}\r\nBODY[] "
                .to_vec();
        wire.extend_from_slice(message_id);
        wire.extend_from_slice(b") UID 7 BODY[] {4}\r\nmail)\r\n");
        assert!(
            decode(&wire).is_ok(),
            "the metadata fixture must be codec-valid"
        );
        let mut reader = reader();
        let frame = reader.next(&mut wire.as_slice()).await.unwrap().unwrap();
        assert_eq!(frame.body, Some(BodyEvidence::Bytes(4)));
        assert_eq!(reader.into_parts().unwrap().0, b"mail");
    }
}

async fn refuses(mut wire: &[u8]) {
    let mut reader = reader();
    assert!(reader.next(&mut wire).await.is_err(), "accepted {wire:?}");
    assert!(matches!(
        reader.next(&mut b"A1 OK ignored\r\n".as_slice()).await,
        Err(ImapError::Closed)
    ));
    assert!(matches!(reader.into_parts(), Err(ImapError::Closed)));
}

#[tokio::test]
async fn complete_frame_ownership_refuses_wrong_missing_and_duplicate_attributes() {
    for tail in [
        &b" UID 8)\r\n"[..],
        b")\r\n",
        b" UID 7 UID 7)\r\n",
        b" UID 7 FLAGS () FLAGS (\\Seen))\r\n",
        b" UID 7 BODY[] NIL)\r\n",
        b" UID 7) trailing\r\n",
        b" UID 7\r\n",
        b" UID 0)\r\n",
    ] {
        refuses(&literal_frame(b"mail", tail)).await;
    }
    for wire in [
        &b"* 1 FETCH (UID 7 BODY[HEADER] {1}\r\nx)\r\n"[..],
        b"* 1 FETCH (UID 7 BODY[]<0> {1}\r\nx)\r\n",
        b"* 1 FETCH (UID 7 BODY.PEEK[] {1}\r\nx)\r\n",
        b"* 1 FETCH (UID 7 BODY[] ~{1}\r\nx)\r\n",
        b"* 1 FETCH (UID 7 BODY[] {4294967296}\r\n",
    ] {
        refuses(wire).await;
    }
}

#[tokio::test]
async fn a_second_body_frame_fails_after_a_valid_first_frame() {
    let mut source =
        b"* 1 FETCH (UID 7 BODY[] \"first\")\r\n* 2 FETCH (UID 7 BODY[] NIL)\r\n".as_slice();
    let mut reader = reader();
    assert!(
        reader
            .next(&mut source)
            .await
            .unwrap()
            .unwrap()
            .body
            .is_some()
    );
    assert!(reader.next(&mut source).await.is_err());
    assert!(matches!(reader.into_parts(), Err(ImapError::Closed)));
}

#[tokio::test]
async fn invalid_quoted_content_and_nul_literal_never_become_observations() {
    for data in [
        &b"bad\\q"[..],
        b"bad\r\nline",
        b"bad\0nul",
        b"bad\xffutf8",
        b"bad\xc3",
        b"bad\xc0\x80",
        b"bad\xed\xa0\x80",
    ] {
        let mut wire = b"* 1 FETCH (UID 7 BODY[] \"".to_vec();
        wire.extend_from_slice(data);
        wire.extend_from_slice(b"\")\r\n");
        refuses(&wire).await;
    }
    refuses(&literal_frame(b"bad\0nul", b" UID 7)\r\n")).await;
}

#[tokio::test]
async fn eof_in_any_nonempty_prefix_fails_closed() {
    let wire = literal_frame(b"mail", b" UID 7)\r\n");
    for split in 1..wire.len() {
        refuses(&wire[..split]).await;
    }
    let wire = b"* 1 FETCH (UID 7 BODY[] \"quoted\\\" mail\")\r\n";
    for split in 1..wire.len() {
        refuses(&wire[..split]).await;
    }
    assert!(reader().next(&mut b"".as_slice()).await.unwrap().is_none());
}

/// A generated payload has no corpus-sized allocation. Counting the sink
/// proves all bytes arrived and no write exceeds the fixed I/O buffer.
struct Generated {
    prefix: &'static [u8],
    remaining: u64,
    suffix: &'static [u8],
}

impl AsyncRead for Generated {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        out: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        if !self.prefix.is_empty() {
            let count = self.prefix.len().min(out.remaining());
            out.put_slice(&self.prefix[..count]);
            self.prefix = &self.prefix[count..];
        } else if self.remaining > 0 {
            let count = (self.remaining as usize).min(out.remaining());
            let target = out.initialize_unfilled_to(count);
            target.fill(b'x');
            out.advance(count);
            self.remaining -= count as u64;
        } else {
            let count = self.suffix.len().min(out.remaining());
            out.put_slice(&self.suffix[..count]);
            self.suffix = &self.suffix[count..];
        }
        Poll::Ready(Ok(()))
    }
}

#[derive(Default)]
struct CountSink {
    bytes: u64,
    largest: usize,
    drops: Arc<AtomicUsize>,
}

impl Drop for CountSink {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

impl AsyncWrite for CountSink {
    fn poll_write(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        bytes: &[u8],
    ) -> Poll<io::Result<usize>> {
        self.bytes += bytes.len() as u64;
        self.largest = self.largest.max(bytes.len());
        Poll::Ready(Ok(bytes.len()))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn full_decimal_budget_literal_and_large_quote_use_bounded_writes() {
    for (prefix, length, suffix) in [
        (
            &b"* 1 FETCH (BODY[] {100000000}\r\n"[..],
            MAX_BODY_BYTES,
            &b" UID 7)\r\n"[..],
        ),
        (
            &b"* 1 FETCH (UID 7 BODY[] \""[..],
            MAX_BODY_BYTES,
            &b"\")\r\n"[..],
        ),
    ] {
        let mut wire = Generated {
            prefix,
            remaining: length,
            suffix,
        };
        let mut reader = BodyReader::new(uid(), CountSink::default());
        let frame = reader.next(&mut wire).await.unwrap().unwrap();
        assert_eq!(frame.body, Some(BodyEvidence::Bytes(length)));
        assert!(frame.frame.bytes.len() < 100);
        let (sink, tail) = reader.into_parts().unwrap();
        assert_eq!(sink.bytes, length);
        assert!(sink.largest <= CHUNK);
        assert!(tail.is_empty());
    }
}

#[tokio::test]
async fn oversized_quote_is_a_typed_failure_after_the_last_allowed_byte() {
    let mut source = Generated {
        prefix: b"* 1 FETCH (UID 7 BODY[] \"",
        remaining: MAX_BODY_BYTES + 1,
        suffix: b"\")\r\n",
    };
    let mut reader = BodyReader::new(uid(), CountSink::default());
    assert!(matches!(
        reader.next(&mut source).await,
        Err(ImapError::BodyBudgetExceeded {
            bytes: 100_000_001,
            limit: 100_000_000
        })
    ));
    assert!(reader.sink.bytes <= MAX_BODY_BYTES);
    assert!(matches!(reader.into_parts(), Err(ImapError::Closed)));
}

#[tokio::test]
async fn exact_metadata_frame_limit_is_inclusive() {
    let mut source = Generated {
        prefix: b"* OK ",
        remaining: u64::from(MAX_MESSAGE_SIZE) - b"* OK \r\n".len() as u64,
        suffix: b"\r\n",
    };
    let frame = reader().next(&mut source).await.unwrap().unwrap();
    assert_eq!(frame.frame.bytes.len(), MAX_MESSAGE_SIZE as usize);
    assert!(frame.body.is_none());
}

struct Pending {
    prefix: &'static [u8],
    reads: Arc<AtomicUsize>,
}

impl AsyncRead for Pending {
    fn poll_read(
        mut self: Pin<&mut Self>,
        _: &mut Context<'_>,
        out: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        self.reads.fetch_add(1, Ordering::SeqCst);
        if self.prefix.is_empty() {
            return Poll::Pending;
        }
        let count = self.prefix.len().min(out.remaining());
        out.put_slice(&self.prefix[..count]);
        self.prefix = &self.prefix[count..];
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn oversized_announcement_fails_before_reading_payload_or_eof() {
    let reads = Arc::new(AtomicUsize::new(0));
    let mut wire = Pending {
        prefix: b"* 1 FETCH (UID 7 BODY[] {100000001}\r\n",
        reads: reads.clone(),
    };
    let mut reader = BodyReader::new(uid(), CountSink::default());
    let result = tokio::time::timeout(std::time::Duration::from_secs(1), reader.next(&mut wire))
        .await
        .unwrap();
    assert!(matches!(
        result,
        Err(ImapError::BodyBudgetExceeded {
            bytes: 100_000_001,
            limit: 100_000_000
        })
    ));
    assert_eq!(reads.load(Ordering::SeqCst), 1);
    assert_eq!(reader.sink.bytes, 0);
    assert!(matches!(reader.into_parts(), Err(ImapError::Closed)));
}

#[tokio::test]
async fn invalid_fetch_prefix_is_refused_at_the_first_body_candidate() {
    let mut wire = Pending {
        prefix: b"* 1 FETCH (NOT-AN-ATTRIBUTE BODY[] \"",
        reads: Arc::new(AtomicUsize::new(0)),
    };
    let mut reader = reader();
    // Repeated failed whole-prefix probes would make a hostile growing
    // line quadratic. Refuse the first invalid FETCH candidate immediately.
    let result =
        tokio::time::timeout(std::time::Duration::from_millis(50), reader.next(&mut wire)).await;
    assert!(matches!(result, Ok(Err(ImapError::Malformed))));
}

#[tokio::test]
async fn metadata_literal_and_unfinished_line_keep_the_original_limit() {
    let wire = format!("* LIST () \"/\" {{{}}}\r\n", MAX_MESSAGE_SIZE);
    let mut source = wire.as_bytes();
    assert!(matches!(
        reader().next(&mut source).await,
        Err(ImapError::FrameBudgetExceeded)
    ));
    let mut source = Generated {
        prefix: b"* OK ",
        remaining: u64::from(MAX_MESSAGE_SIZE),
        suffix: b"",
    };
    assert!(matches!(
        reader().next(&mut source).await,
        Err(ImapError::FrameBudgetExceeded)
    ));
}

#[tokio::test]
async fn cancellation_poisoning_and_owned_sink_drop_are_observable() {
    for prefix in [
        &b"* 1 FETCH (UID 7 BODY[] {10}\r\npart"[..],
        &b"* 1 FETCH (UID 7 BODY[] \"part"[..],
    ] {
        let drops = Arc::new(AtomicUsize::new(0));
        let mut reader = BodyReader::new(
            uid(),
            CountSink {
                drops: drops.clone(),
                ..CountSink::default()
            },
        );
        let mut wire = Pending {
            prefix,
            reads: Arc::new(AtomicUsize::new(0)),
        };
        assert!(
            tokio::time::timeout(std::time::Duration::from_millis(5), reader.next(&mut wire))
                .await
                .is_err()
        );
        assert!(matches!(
            reader.next(&mut b"A1 OK ignored\r\n".as_slice()).await,
            Err(ImapError::Closed)
        ));
        assert!(matches!(reader.into_parts(), Err(ImapError::Closed)));
        assert_eq!(drops.load(Ordering::SeqCst), 1);
    }
}

struct FailingWire;
impl AsyncRead for FailingWire {
    fn poll_read(
        self: Pin<&mut Self>,
        _: &mut Context<'_>,
        _: &mut ReadBuf<'_>,
    ) -> Poll<io::Result<()>> {
        Poll::Ready(Err(io::ErrorKind::ConnectionReset.into()))
    }
}

struct FailingSink;
impl AsyncWrite for FailingSink {
    fn poll_write(self: Pin<&mut Self>, _: &mut Context<'_>, _: &[u8]) -> Poll<io::Result<usize>> {
        Poll::Ready(Err(io::ErrorKind::StorageFull.into()))
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

#[tokio::test]
async fn source_and_sink_failures_keep_native_kinds_and_never_return_sink() {
    let mut reader = reader();
    assert!(matches!(
        reader.next(&mut FailingWire).await,
        Err(ImapError::Io {
            kind: io::ErrorKind::ConnectionReset,
            ..
        })
    ));
    assert!(matches!(reader.into_parts(), Err(ImapError::Closed)));
    for wire in [
        &b"* 1 FETCH (UID 7 BODY[] {1}\r\nx)\r\n"[..],
        &b"* 1 FETCH (UID 7 BODY[] \"x\")\r\n"[..],
    ] {
        let mut reader = BodyReader::new(uid(), FailingSink);
        assert!(matches!(
            reader.next(&mut &wire[..]).await,
            Err(ImapError::Io {
                kind: io::ErrorKind::StorageFull,
                ..
            })
        ));
        assert!(matches!(reader.into_parts(), Err(ImapError::Closed)));
    }
}
