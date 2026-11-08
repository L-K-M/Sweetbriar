//! The strict response gate (spec/04 "Fault handling", the M2.1b
//! decision): a reader layer between the socket and the coroutines
//! that runs its own imap-codec fragmentizer and decoders over
//! the bytes read and releases only complete messages that decode. The
//! first message that doesn't decode poisons the gate; every read after
//! that fails with [`ImapError::Malformed`], so nothing parsed after the
//! fault reaches the caller. The gate is on from the greeting.

use std::collections::VecDeque;

use io_imap::codec::{
    GreetingCodec, ResponseCodec,
    fragmentizer::{FragmentInfo, Fragmentizer},
};
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::error::ImapError;

#[cfg(test)]
mod limits;

pub(crate) const MAX_MESSAGE_SIZE: u32 = 50 * 1024 * 1024;

const READ_CHUNK: usize = 8192;

pub(crate) enum FrameKind {
    Greeting,
    Untagged,
    Completion,
    Continuation,
    Bye,
}

pub(crate) struct Frame {
    pub(crate) bytes: Vec<u8>,
    pub(crate) kind: FrameKind,
}

#[derive(Clone, Copy)]
enum GateFault {
    Malformed,
    FrameLimit,
}

pub(crate) struct Gate {
    fragmentizer: Fragmentizer,
    codec: ResponseCodec,
    expects_greeting: bool,
    released: VecDeque<Frame>,
    /// Received bytes not yet released as complete messages.
    pending: usize,
    wire_buf: Box<[u8; READ_CHUNK]>,
    eof: bool,
    fault: Option<GateFault>,
}

impl Gate {
    pub(crate) fn new() -> Self {
        Self {
            fragmentizer: Fragmentizer::new(MAX_MESSAGE_SIZE),
            codec: ResponseCodec::new(),
            expects_greeting: true,
            released: VecDeque::new(),
            pending: 0,
            wire_buf: Box::new([0; READ_CHUNK]),
            eof: false,
            fault: None,
        }
    }

    pub(crate) fn poisoned(&self) -> bool {
        self.fault.is_some()
    }

    /// A following command may inherit ordinary unsolicited data, but
    /// never a completion, continuation or BYE buffered after its predecessor.
    pub(crate) fn reusable(&self) -> bool {
        !self.poisoned()
            && self
                .released
                .iter()
                .all(|frame| matches!(frame.kind, FrameKind::Untagged))
    }

    /// True when bytes read from the wire are still buffered and were
    /// not handed to the coroutine: complete messages past the read
    /// chunk ([`Self::released`]), a partial frame ([`Self::pending`]),
    /// or a decode fault ([`Self::poisoned`]). STARTTLS refuses to
    /// upgrade with any of these, so no plaintext crosses into TLS.
    pub(crate) fn has_plaintext_residual(&self) -> bool {
        self.poisoned() || self.has_buffered()
    }

    /// Bytes already received belong to the previous command boundary,
    /// including an incomplete unsolicited frame that needs another read.
    pub(crate) fn has_buffered(&self) -> bool {
        !self.released.is_empty() || self.pending > 0
    }

    /// Reads one complete validated response. `Ok(empty)` is EOF; a
    /// syntax fault fails with [`ImapError::Malformed`]; an exceeded frame
    /// limit reports [`ImapError::FrameBudgetExceeded`]. Keeping the
    /// boundary prevents a coroutine from replacing one tagged reply
    /// with a later reply in the same socket read.
    #[cfg(test)]
    pub(crate) async fn read(
        &mut self,
        wire: &mut (impl AsyncRead + Unpin),
    ) -> Result<Vec<u8>, ImapError> {
        Ok(self
            .read_frame(wire)
            .await?
            .map_or_else(Vec::new, |frame| frame.bytes))
    }

    pub(crate) async fn read_frame(
        &mut self,
        wire: &mut (impl AsyncRead + Unpin),
    ) -> Result<Option<Frame>, ImapError> {
        loop {
            if matches!(self.fault, Some(GateFault::FrameLimit)) {
                return Err(ImapError::FrameBudgetExceeded);
            }
            if let Some(frame) = self.released.pop_front() {
                return Ok(Some(frame));
            }
            if self.poisoned() {
                return Err(ImapError::Malformed);
            }
            if self.eof {
                return Ok(None);
            }
            self.fill(wire).await?;
        }
    }

    async fn fill(&mut self, wire: &mut (impl AsyncRead + Unpin)) -> Result<(), ImapError> {
        loop {
            self.decode_available()?;
            if !self.released.is_empty() || self.eof || self.poisoned() {
                return Ok(());
            }
            let read = wire
                .read(&mut self.wire_buf[..])
                .await
                .map_err(ImapError::from_io)?;
            if read == 0 {
                self.eof = self.pending == 0;
                if !self.eof {
                    self.fault = Some(GateFault::Malformed);
                }
            } else {
                self.pending += read;
                self.fragmentizer.enqueue_bytes(&self.wire_buf[..read]);
            }
        }
    }

    /// Ingest an already-received streaming-command tail without reading
    /// the socket. Only an empty gate may accept this handoff. Known complete
    /// frames must be ordinary unsolicited data; a partial frame stays under
    /// the predecessor context for the next pre-command drain.
    pub(crate) fn ingest_received(&mut self, bytes: &[u8]) -> Result<VecDeque<Frame>, ImapError> {
        match self.fault {
            Some(GateFault::FrameLimit) => return Err(ImapError::FrameBudgetExceeded),
            Some(GateFault::Malformed) => return Err(ImapError::Malformed),
            None => {}
        }
        if self.eof {
            return Err(ImapError::Closed);
        }
        if self.has_buffered() {
            return Err(ImapError::UnexpectedCompletion);
        }
        self.pending = bytes.len();
        self.fragmentizer.enqueue_bytes(bytes);
        self.decode_available()?;
        self.take_received()
    }

    /// Take complete received unsolicited frames without waiting for a partial
    /// tail. Known faults, completions, continuations and BYE fail closed.
    pub(crate) fn take_received(&mut self) -> Result<VecDeque<Frame>, ImapError> {
        match self.fault {
            Some(GateFault::FrameLimit) => return Err(ImapError::FrameBudgetExceeded),
            Some(GateFault::Malformed) => return Err(ImapError::Malformed),
            None => {}
        }
        if !self.reusable() {
            return Err(ImapError::UnexpectedCompletion);
        }
        Ok(std::mem::take(&mut self.released))
    }

    /// Take complete received unsolicited frames without waiting for a partial
    /// tail. Known faults, completions, continuations and BYE fail closed.
    pub(crate) fn take_received(&mut self) -> Result<VecDeque<Frame>, ImapError> {
        match self.fault {
            Some(GateFault::FrameLimit) => return Err(ImapError::FrameBudgetExceeded),
            Some(GateFault::Malformed) => return Err(ImapError::Malformed),
            None => {}
        }
        if !self.reusable() {
            return Err(ImapError::UnexpectedCompletion);
        }
        Ok(std::mem::take(&mut self.released))
    }

    fn decode_available(&mut self) -> Result<(), ImapError> {
        loop {
            let fragment = self.fragmentizer.progress();
            // A status/greeting/continuation can legally end in `{n}`.
            // Probe only the first line, and require a complete codec
            // decode before overriding the context-free literal guess.
            // Later literal boundaries never reparse a growing FETCH.
            let complete_text = matches!(
                fragment,
                Some(FragmentInfo::Line {
                    start: 0,
                    announcement: Some(_),
                    ..
                })
            ) && self.decode_kind().is_ok();
            let announced_over_limit = !complete_text
                && match &fragment {
                    Some(FragmentInfo::Line {
                        announcement: Some(literal),
                        ..
                    }) => {
                        self.fragmentizer.message_bytes().len() as u64 + u64::from(literal.length)
                            > u64::from(MAX_MESSAGE_SIZE)
                    }
                    _ => false,
                };
            // progress() can consume an unfinished line/literal and
            // return None. Check its limit before asking for more bytes,
            // as well as declarations that already cannot fit the frame.
            if self.fragmentizer.is_max_message_size_exceeded() || announced_over_limit {
                self.fault = Some(GateFault::FrameLimit);
                self.released.clear();
                self.pending = 0;
                self.fragmentizer = Fragmentizer::new(MAX_MESSAGE_SIZE);
                return Err(ImapError::FrameBudgetExceeded);
            }
            if fragment.is_none() {
                break;
            }
            if !complete_text && !self.fragmentizer.is_message_complete() {
                continue;
            }
            let kind = self.decode_kind();
            let Ok(kind) = kind else {
                // Drain released messages before reporting the fault.
                self.fault = Some(GateFault::Malformed);
                return Ok(());
            };
            self.expects_greeting = false;
            let bytes = self.fragmentizer.message_bytes();
            self.pending -= bytes.len();
            self.released.push_back(Frame {
                bytes: bytes.to_vec(),
                kind,
            });
            if complete_text {
                // The full codec proof above establishes the boundary:
                // there is no literal to skip. Preserve enqueued bytes
                // of following responses, including real literals.
                self.fragmentizer.skip_message();
            }
        }
        Ok(())
    }

    fn decode_kind(&self) -> Result<FrameKind, ()> {
        if self.expects_greeting {
            self.fragmentizer
                .decode_message(&GreetingCodec::new())
                .map(|_| FrameKind::Greeting)
                .map_err(|_| ())
        } else {
            use io_imap::types::response::{Response, Status};
            self.fragmentizer
                .decode_message(&self.codec)
                .map(|response| match response {
                    Response::Status(Status::Tagged(_)) => FrameKind::Completion,
                    Response::Status(Status::Bye(_)) => FrameKind::Bye,
                    Response::CommandContinuationRequest(_) => FrameKind::Continuation,
                    _ => FrameKind::Untagged,
                })
                .map_err(|_| ())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[tokio::test]
    async fn taking_received_frames_keeps_the_partial_predecessor() {
        let mut gate = Gate::new();
        let mut wire = b"* OK ready\r\n* 9 EXISTS\r\n* 2 EX".as_slice();
        gate.read_frame(&mut wire).await.unwrap().unwrap();
        let frames = gate.take_received().unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].bytes, b"* 9 EXISTS\r\n");
        assert!(matches!(frames[0].kind, FrameKind::Untagged));
        assert!(gate.has_buffered(), "partial predecessor remains owned");
        assert!(gate.take_received().unwrap().is_empty());
        let mut rest = b"ISTS\r\n".as_slice();
        let frame = gate.read_frame(&mut rest).await.unwrap().unwrap();
        assert_eq!(frame.bytes, b"* 2 EXISTS\r\n");
        assert!(!gate.has_buffered());
    }

    #[tokio::test]
    async fn eof_after_partial_line_is_malformed() {
        assert_partial(b"* OK incomplete").await;
    }

    #[tokio::test]
    async fn eof_after_partial_literal_is_malformed() {
        assert_partial(b"* 1 FETCH (BODY[] {5}\r\nabc").await;
    }

    async fn assert_partial(partial: &[u8]) {
        let mut gate = Gate::new();
        let bytes = [wire_bytes(), partial].concat();
        let mut wire = bytes.as_slice();
        assert_eq!(gate.read(&mut wire).await.unwrap(), b"* OK first\r\n");
        assert_eq!(gate.read(&mut wire).await.unwrap(), b"* OK second\r\n");
        assert!(matches!(
            gate.read(&mut wire).await,
            Err(ImapError::Malformed)
        ));
    }

    #[tokio::test]
    async fn eof_after_complete_responses_is_clean() {
        let mut gate = Gate::new();
        let mut wire = b"* OK first\r\n* OK second\r\n".as_slice();
        assert_eq!(gate.read(&mut wire).await.unwrap(), b"* OK first\r\n");
        assert_eq!(gate.read(&mut wire).await.unwrap(), b"* OK second\r\n");
        assert!(gate.read(&mut wire).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn fragmented_greeting_then_literal_response() {
        let mut gate = Gate::new();
        let mut wire = b"* O"
            .as_slice()
            .chain(b"K ready\r\n* 1 FETCH (BODY[] {5}\r\nhello)\r\n".as_slice());
        assert_eq!(gate.read(&mut wire).await.unwrap(), b"* OK ready\r\n");
        assert_eq!(
            gate.read(&mut wire).await.unwrap(),
            b"* 1 FETCH (BODY[] {5}\r\nhello)\r\n"
        );
        assert!(gate.read(&mut wire).await.unwrap().is_empty());
    }

    async fn active_gate() -> Gate {
        let mut gate = Gate::new();
        assert_eq!(
            gate.read(&mut b"* OK ready\r\n".as_slice()).await.unwrap(),
            b"* OK ready\r\n"
        );
        gate
    }

    #[tokio::test]
    async fn streaming_tail_releases_complete_data_and_retains_a_partial_frame() {
        let mut gate = active_gate().await;
        let frames = gate.ingest_received(b"* 9 EXISTS\r\n* 2 EX").unwrap();
        assert_eq!(frames.len(), 1);
        assert_eq!(frames[0].bytes, b"* 9 EXISTS\r\n");
        assert!(matches!(frames[0].kind, FrameKind::Untagged));
        assert!(gate.has_buffered());
        assert!(gate.reusable());
        assert_eq!(
            gate.read(&mut b"PUNGE\r\n".as_slice()).await.unwrap(),
            b"* 2 EXPUNGE\r\n"
        );
        assert!(!gate.has_buffered());
    }

    #[tokio::test]
    async fn streaming_tail_refuses_known_completions_bye_and_malformed_data() {
        for bytes in [
            &b"X1 OK unexpected\r\n"[..],
            b"* BYE closing\r\n",
            b"+ continue\r\n",
        ] {
            let mut gate = active_gate().await;
            assert!(matches!(
                gate.ingest_received(bytes),
                Err(ImapError::UnexpectedCompletion)
            ));
            assert!(!gate.reusable());
        }
        let mut gate = active_gate().await;
        assert!(matches!(
            gate.ingest_received(b"* 9 EXISTS\r\nnot a response\r\n"),
            Err(ImapError::Malformed)
        ));
        assert!(gate.poisoned());
    }

    #[tokio::test]
    async fn streaming_tail_keeps_early_typed_literal_limits_and_persistent_failure() {
        let mut gate = active_gate().await;
        let bytes = format!("* 1 FETCH (BODY[] {{{}}}\r\n", MAX_MESSAGE_SIZE + 1);
        assert!(matches!(
            gate.ingest_received(bytes.as_bytes()),
            Err(ImapError::FrameBudgetExceeded)
        ));
        assert!(matches!(
            gate.ingest_received(b"* OK ignored\r\n"),
            Err(ImapError::FrameBudgetExceeded)
        ));
    }

    #[tokio::test]
    async fn streaming_tail_cannot_splice_buffered_predecessor_bytes_or_resurrect_eof() {
        let mut gate = Gate::new();
        let mut wire = b"* OK ready\r\n* 2 EX".as_slice();
        gate.read(&mut wire).await.unwrap();
        assert!(matches!(
            gate.ingest_received(b"injected\r\n"),
            Err(ImapError::UnexpectedCompletion)
        ));
        assert_eq!(
            gate.read(&mut b"PUNGE\r\n".as_slice()).await.unwrap(),
            b"* 2 EXPUNGE\r\n"
        );
        assert!(gate.read(&mut b"".as_slice()).await.unwrap().is_empty());
        assert!(matches!(
            gate.ingest_received(b"* OK too late\r\n"),
            Err(ImapError::Closed)
        ));
    }

    fn wire_bytes() -> &'static [u8] {
        b"* OK first\r\n* OK second\r\n"
    }
}
