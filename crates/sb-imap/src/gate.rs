//! The strict response gate (spec/04 "Fault handling", the M2.1b
//! decision): a reader layer between the socket and the coroutines
//! that runs its own imap-codec fragmentizer and decoders over
//! the bytes read and releases only complete messages that decode. The
//! first message that doesn't decode poisons the gate; every read after
//! that fails with [`ImapError::Malformed`], so nothing parsed after the
//! fault reaches the caller. The gate is on from the greeting.

use std::collections::VecDeque;

use io_imap::codec::{GreetingCodec, ResponseCodec, fragmentizer::Fragmentizer};
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::error::ImapError;

pub(crate) const MAX_MESSAGE_SIZE: u32 = 50 * 1024 * 1024;

const READ_CHUNK: usize = 8192;

pub(crate) struct Gate {
    fragmentizer: Fragmentizer,
    codec: ResponseCodec,
    expects_greeting: bool,
    released: VecDeque<u8>,
    /// Received bytes not yet released as complete messages.
    pending: usize,
    wire_buf: Box<[u8; READ_CHUNK]>,
    eof: bool,
    poisoned: bool,
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
            poisoned: false,
        }
    }

    pub(crate) fn poisoned(&self) -> bool {
        self.poisoned
    }

    /// Reads the next released bytes. `Ok(empty)` is EOF; a poisoned
    /// gate fails with [`ImapError::Malformed`].
    pub(crate) async fn read(
        &mut self,
        wire: &mut (impl AsyncRead + Unpin),
    ) -> Result<Vec<u8>, ImapError> {
        loop {
            if !self.released.is_empty() {
                let end = self.released.len().min(READ_CHUNK);
                return Ok(self.released.drain(..end).collect());
            }
            if self.poisoned {
                return Err(ImapError::Malformed);
            }
            if self.eof {
                return Ok(Vec::new());
            }
            self.fill(wire).await?;
        }
    }

    async fn fill(&mut self, wire: &mut (impl AsyncRead + Unpin)) -> Result<(), ImapError> {
        loop {
            while let Some(_fragment) = self.fragmentizer.progress() {
                if !self.fragmentizer.is_message_complete() {
                    continue;
                }
                let valid = if self.expects_greeting {
                    self.fragmentizer
                        .decode_message(&GreetingCodec::new())
                        .is_ok()
                } else {
                    self.fragmentizer.decode_message(&self.codec).is_ok()
                };
                if !valid {
                    // Drain released messages before reporting the fault.
                    self.poisoned = true;
                    return Ok(());
                }
                self.expects_greeting = false;
                let bytes = self.fragmentizer.message_bytes();
                self.pending -= bytes.len();
                self.released.extend(bytes.iter().copied());
            }
            if !self.released.is_empty() || self.eof || self.poisoned {
                return Ok(());
            }
            let read = wire
                .read(&mut self.wire_buf[..])
                .await
                .map_err(ImapError::from_io)?;
            if read == 0 {
                self.eof = self.pending == 0;
                self.poisoned = !self.eof;
            } else {
                self.pending += read;
                self.fragmentizer.enqueue_bytes(&self.wire_buf[..read]);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

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
        assert_eq!(gate.read(&mut wire).await.unwrap(), wire_bytes());
        assert!(matches!(
            gate.read(&mut wire).await,
            Err(ImapError::Malformed)
        ));
    }

    #[tokio::test]
    async fn eof_after_complete_responses_is_clean() {
        let mut gate = Gate::new();
        let mut wire = b"* OK first\r\n* OK second\r\n".as_slice();
        assert_eq!(gate.read(&mut wire).await.unwrap(), wire_bytes());
        assert!(gate.read(&mut wire).await.unwrap().is_empty());
    }

    #[tokio::test]
    async fn fragmented_greeting_then_literal_response() {
        let mut gate = Gate::new();
        let mut wire = b"* O"
            .as_slice()
            .chain(b"K ready\r\n* 1 FETCH (BODY[] {5}\r\nhello)\r\n".as_slice());
        assert_eq!(
            gate.read(&mut wire).await.unwrap(),
            b"* OK ready\r\n* 1 FETCH (BODY[] {5}\r\nhello)\r\n"
        );
        assert!(gate.read(&mut wire).await.unwrap().is_empty());
    }

    fn wire_bytes() -> &'static [u8] {
        b"* OK first\r\n* OK second\r\n"
    }
}
