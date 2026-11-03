//! The strict response gate (spec/04 "Fault handling", the M2.1b
//! decision): a reader layer between the socket and the coroutines
//! that runs its own imap-codec `Fragmentizer` + `ResponseCodec` over
//! the bytes read and releases only complete messages that decode. The
//! first message that doesn't decode poisons the gate; every read after
//! that fails with [`ImapError::Malformed`], so nothing parsed after the
//! fault reaches the caller. The gate is on from the greeting.

use std::collections::VecDeque;

use io_imap::codec::{ResponseCodec, fragmentizer::Fragmentizer};
use tokio::io::{AsyncRead, AsyncReadExt};

use crate::error::ImapError;

/// One message's framing cap, what a real client's fragmentizer allows.
pub(crate) const MAX_MESSAGE_SIZE: u32 = 50 * 1024 * 1024;

/// Bytes released per read; the coroutine's own buffer is the limit.
const READ_CHUNK: usize = 8192;

/// The parsing half of the connection's read path.
pub(crate) struct Gate {
    fragmentizer: Fragmentizer,
    codec: ResponseCodec,
    /// Verified message bytes waiting for the coroutine's next read.
    released: VecDeque<u8>,
    /// Received bytes not yet released as complete messages.
    pending: usize,
    wire_buf: Box<[u8; READ_CHUNK]>,
    /// The peer hung up; reported once `released` drains.
    eof: bool,
    /// A message failed decode; every read fails from then on.
    poisoned: bool,
}

impl Gate {
    pub(crate) fn new() -> Self {
        Self {
            fragmentizer: Fragmentizer::new(MAX_MESSAGE_SIZE),
            codec: ResponseCodec::new(),
            released: VecDeque::new(),
            pending: 0,
            wire_buf: Box::new([0; READ_CHUNK]),
            eof: false,
            poisoned: false,
        }
    }

    /// Whether a message failed decode. The connection closes and
    /// becomes unusable once this is set.
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

    /// Releases each framed message's wire bytes once it decodes;
    /// pulls from the wire only when nothing framed is pending.
    async fn fill(&mut self, wire: &mut (impl AsyncRead + Unpin)) -> Result<(), ImapError> {
        loop {
            while let Some(_fragment) = self.fragmentizer.progress() {
                if !self.fragmentizer.is_message_complete() {
                    continue;
                }
                match self.fragmentizer.decode_message(&self.codec) {
                    Ok(_) => {
                        let bytes = self.fragmentizer.message_bytes();
                        self.pending -= bytes.len();
                        self.released.extend(bytes.iter().copied());
                    }
                    // Stop at the fault: the released messages drain
                    // first, then `read` reports the poison.
                    Err(_) => {
                        self.poisoned = true;
                        return Ok(());
                    }
                }
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

    fn wire_bytes() -> &'static [u8] {
        b"* OK first\r\n* OK second\r\n"
    }
}
