//! The strict response gate (spec/04 "Fault handling", the M2.1b
//! decision): a reader layer between the socket and the coroutines
//! that runs its own imap-codec `Fragmentizer` + `ResponseCodec` over
//! the bytes read and releases only complete messages that decode. The
//! first message that doesn't decode poisons the gate; every read after
//! that fails with [`ImapError::Malformed`], so nothing parsed after the
//! fault reaches the caller. The gate is on from the greeting.

use std::collections::VecDeque;

use io_imap::codec::{ResponseCodec, fragmentizer::Fragmentizer};
use tokio::io::AsyncReadExt;

use crate::connection::Wire;
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
    pub(crate) async fn read(&mut self, wire: &mut Wire) -> Result<Vec<u8>, ImapError> {
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
    async fn fill(&mut self, wire: &mut Wire) -> Result<(), ImapError> {
        loop {
            while let Some(_fragment) = self.fragmentizer.progress() {
                if !self.fragmentizer.is_message_complete() {
                    continue;
                }
                match self.fragmentizer.decode_message(&self.codec) {
                    Ok(_) => self
                        .released
                        .extend(self.fragmentizer.message_bytes().iter().copied()),
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
                self.eof = true;
            } else {
                self.fragmentizer.enqueue_bytes(&self.wire_buf[..read]);
            }
        }
    }
}
