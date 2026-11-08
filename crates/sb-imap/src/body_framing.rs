//! Private bounded response reader for M2.2b3a. Body bytes go to an owned
//! unpublished sink; only an empty placeholder enters the codec. The whole
//! FETCH frame must then prove UID, section and attribute ownership. This
//! is not a command completion: the following driver owns tags, epochs,
//! predecessor draining, tail validation and socket lifetime.

use std::collections::HashSet;
use std::num::NonZeroU32;

use io_imap::codec::{
    ResponseCodec,
    decode::Decoder,
    fragmentizer::{FragmentInfo, Fragmentizer},
};
use io_imap::types::fetch::MessageDataItem;
use io_imap::types::response::{Data, Response, Status};
use tokio::io::{AsyncRead, AsyncReadExt, AsyncWrite, AsyncWriteExt};

use crate::ImapError;
use crate::gate::{Frame, FrameKind, MAX_MESSAGE_SIZE};
use crate::timeout::BodySizeHint;

/// Decimal bytes, matching spec/10's message budget. This leaf has no MIME
/// dependency; the later acquisition layer also applies its parsing limits.
pub(crate) const MAX_BODY_BYTES: u64 = 100_000_000;
const CHUNK: usize = 8192;

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum BodyEvidence {
    Unavailable,
    Bytes(u64),
}

pub(crate) struct BodyFrame {
    pub(crate) frame: Frame,
    pub(crate) body: Option<BodyEvidence>,
    /// Original framing cost; the body placeholder can be shorter.
    pub(crate) metadata_bytes: usize,
}

/// The sink must remain private and unpublished. Cancellation of `next`
/// poisons this reader, including when no I/O error was returned. The driver
/// must drop the owned socket and sink after any failure/cancellation.
pub(crate) struct BodyReader<S> {
    uid: NonZeroU32,
    sink: S,
    input: Box<[u8; CHUNK]>,
    start: usize,
    end: usize,
    failed: bool,
    observed: bool,
    size_hint: BodySizeHint,
}

struct Metadata {
    bytes: Vec<u8>,
    /// Charge original framing bytes, even when a placeholder is shorter.
    charged: usize,
}

impl Metadata {
    fn append(&mut self, bytes: &[u8]) -> Result<(), ImapError> {
        self.charged = self
            .charged
            .checked_add(bytes.len())
            .ok_or(ImapError::FrameBudgetExceeded)?;
        if self.charged > MAX_MESSAGE_SIZE as usize {
            return Err(ImapError::FrameBudgetExceeded);
        }
        self.bytes.extend_from_slice(bytes);
        Ok(())
    }
}

impl<S: AsyncWrite + Unpin> BodyReader<S> {
    pub(crate) fn new(uid: NonZeroU32, sink: S) -> Self {
        Self {
            uid,
            sink,
            input: Box::new([0; CHUNK]),
            start: 0,
            end: 0,
            failed: false,
            observed: false,
            size_hint: BodySizeHint::default(),
        }
    }

    pub(crate) fn with_size_hint(uid: NonZeroU32, sink: S, size_hint: BodySizeHint) -> Self {
        let mut reader = Self::new(uid, sink);
        reader.size_hint = size_hint;
        reader
    }

    /// One complete codec-validated response. Body placeholders are never
    /// evidence of empty mail: `body` carries the actual byte count or NIL.
    pub(crate) async fn next(
        &mut self,
        wire: &mut (impl AsyncRead + Unpin),
    ) -> Result<Option<BodyFrame>, ImapError> {
        if self.failed {
            return Err(ImapError::Closed);
        }
        // This stays set when the future is dropped at any await point.
        self.failed = true;
        let result = self.next_inner(wire).await;
        if result.is_ok() {
            self.failed = false;
        }
        result
    }

    /// Only the following command driver may consume these parts, after its
    /// first matching tagged OK and received-tail checks. Buffered raw bytes
    /// must enter the ordinary gate; they cannot be discarded or count as a
    /// completion. No method here certifies command or publication authority.
    pub(crate) fn into_parts(self) -> Result<(S, Vec<u8>), ImapError> {
        if self.failed {
            return Err(ImapError::Closed);
        }
        Ok((self.sink, self.input[self.start..self.end].to_vec()))
    }

    async fn fill(&mut self, wire: &mut (impl AsyncRead + Unpin)) -> Result<bool, ImapError> {
        if self.start == self.end {
            self.start = 0;
            self.end = wire
                .read(&mut self.input[..])
                .await
                .map_err(ImapError::from_io)?;
        }
        Ok(self.start < self.end)
    }

    async fn byte(&mut self, wire: &mut (impl AsyncRead + Unpin)) -> Result<Option<u8>, ImapError> {
        if !self.fill(wire).await? {
            return Ok(None);
        }
        let byte = self.input[self.start];
        self.start += 1;
        Ok(Some(byte))
    }

    async fn next_inner(
        &mut self,
        wire: &mut (impl AsyncRead + Unpin),
    ) -> Result<Option<BodyFrame>, ImapError> {
        let mut meta = Metadata {
            bytes: Vec::new(),
            charged: 0,
        };
        let mut streamed = None;
        loop {
            let line_start = meta.bytes.len();
            let mut quoted = false;
            let mut escaped = false;
            loop {
                let Some(byte) = self.byte(wire).await? else {
                    return if meta.bytes.is_empty() {
                        Ok(None)
                    } else {
                        Err(ImapError::Malformed)
                    };
                };
                if byte == b'"' && !quoted && full_body_prefix(&meta.bytes, line_start)? {
                    if streamed.is_some() || self.observed {
                        return Err(body_error("repeated body"));
                    }
                    meta.append(b"\"")?;
                    streamed = Some(self.quoted_body(wire).await?);
                    meta.append(b"\"")?;
                    continue;
                }
                meta.append(&[byte])?;
                if quoted {
                    if escaped {
                        escaped = false;
                    } else if byte == b'\\' {
                        escaped = true;
                    } else if byte == b'"' {
                        quoted = false;
                    }
                } else if byte == b'"' {
                    quoted = true;
                }
                if byte == b'\n' {
                    break;
                }
            }
            // Status text may legally end in `{n}`. A complete first-line
            // decode overrides the context-free fragmentizer's announcement.
            let complete = line_start == 0 && decode(&meta.bytes).is_ok();
            let mut fragmentizer = Fragmentizer::new(MAX_MESSAGE_SIZE);
            fragmentizer.enqueue_bytes(&meta.bytes[line_start..]);
            let announcement = match fragmentizer.progress() {
                Some(FragmentInfo::Line { announcement, .. }) => announcement,
                _ => return Err(ImapError::Malformed),
            };
            if complete || announcement.is_none() {
                let response = decode(&meta.bytes)?;
                let body = self.validate_body(&response, streamed)?;
                let kind = match response {
                    Response::Status(Status::Tagged(_)) => FrameKind::Completion,
                    Response::Status(Status::Bye(_)) => FrameKind::Bye,
                    Response::CommandContinuationRequest(_) => FrameKind::Continuation,
                    _ => FrameKind::Untagged,
                };
                return Ok(Some(BodyFrame {
                    metadata_bytes: meta.charged,
                    frame: Frame {
                        bytes: meta.bytes,
                        kind,
                    },
                    body,
                }));
            }
            let length = u64::from(announcement.expect("checked above").length);
            let marker = meta.bytes[line_start..]
                .iter()
                .rposition(|b| *b == b'{')
                .map(|at| at + line_start)
                .ok_or(ImapError::Malformed)?;
            if full_body_prefix(&meta.bytes[..marker], line_start)? {
                if streamed.is_some() || self.observed {
                    return Err(body_error("repeated body"));
                }
                check_body_length(length)?;
                self.size_hint.note(length);
                meta.bytes.truncate(marker);
                meta.bytes.extend_from_slice(b"{0}\r\n");
                self.literal(wire, length, None).await?;
                streamed = Some(length);
            } else {
                // Metadata literals keep their original bytes and limits.
                // literal8 is outside the enabled BODY[] grammar and cannot
                // be smuggled through an empty ordinary-literal placeholder.
                if marker > 0 && meta.bytes[marker - 1] == b'~' {
                    return Err(ImapError::Malformed);
                }
                let count = usize::try_from(length).map_err(|_| ImapError::FrameBudgetExceeded)?;
                if count > (MAX_MESSAGE_SIZE as usize).saturating_sub(meta.charged) {
                    return Err(ImapError::FrameBudgetExceeded);
                }
                self.literal(wire, length, Some(&mut meta)).await?;
            }
        }
    }

    async fn literal(
        &mut self,
        wire: &mut (impl AsyncRead + Unpin),
        mut remaining: u64,
        mut meta: Option<&mut Metadata>,
    ) -> Result<(), ImapError> {
        while remaining > 0 {
            if !self.fill(wire).await? {
                return Err(ImapError::Malformed);
            }
            let count = (self.end - self.start).min(remaining as usize);
            let bytes = &self.input[self.start..self.start + count];
            if let Some(meta) = meta.as_deref_mut() {
                meta.append(bytes)?;
            } else {
                // The pinned ordinary literal grammar is CHAR8, excluding
                // NUL. The placeholder codec cannot inspect streamed bytes.
                if bytes.contains(&0) {
                    return Err(ImapError::Malformed);
                }
                self.sink
                    .write_all(bytes)
                    .await
                    .map_err(ImapError::from_io)?;
            }
            self.start += count;
            remaining -= count as u64;
        }
        Ok(())
    }

    async fn quoted_body(&mut self, wire: &mut (impl AsyncRead + Unpin)) -> Result<u64, ImapError> {
        let mut bytes = [0; CHUNK];
        let mut used = 0;
        let mut total = 0;
        loop {
            let mut byte = self.byte(wire).await?.ok_or(ImapError::Malformed)?;
            if byte == b'"' {
                std::str::from_utf8(&bytes[..used]).map_err(|_| ImapError::Malformed)?;
                self.size_hint.note(total);
                self.sink
                    .write_all(&bytes[..used])
                    .await
                    .map_err(ImapError::from_io)?;
                return Ok(total);
            }
            if byte == b'\\' {
                byte = self.byte(wire).await?.ok_or(ImapError::Malformed)?;
                if !matches!(byte, b'\\' | b'"') {
                    return Err(ImapError::Malformed);
                }
            }
            if matches!(byte, 0 | b'\r' | b'\n') {
                return Err(ImapError::Malformed);
            }
            total += 1;
            check_body_length(total)?;
            bytes[used] = byte;
            used += 1;
            if used == CHUNK {
                self.size_hint.note(total);
                let valid = match std::str::from_utf8(&bytes) {
                    Ok(_) => used,
                    Err(error) if error.error_len().is_none() => error.valid_up_to(),
                    Err(_) => return Err(ImapError::Malformed),
                };
                self.sink
                    .write_all(&bytes[..valid])
                    .await
                    .map_err(ImapError::from_io)?;
                bytes.copy_within(valid..used, 0);
                used -= valid;
            }
        }
    }

    fn validate_body(
        &mut self,
        response: &Response<'_>,
        streamed: Option<u64>,
    ) -> Result<Option<BodyEvidence>, ImapError> {
        let Response::Data(Data::Fetch { items, .. }) = response else {
            return if streamed.is_none() {
                Ok(None)
            } else {
                Err(ImapError::Malformed)
            };
        };
        if !items
            .as_ref()
            .iter()
            .any(|item| matches!(item, MessageDataItem::BodyExt { .. }))
        {
            return if streamed.is_none() {
                Ok(None)
            } else {
                Err(ImapError::Malformed)
            };
        }
        if self.observed {
            return Err(body_error("repeated body frame"));
        }
        let mut attributes = HashSet::new();
        let mut uid = None;
        let mut body = None;
        for item in items.as_ref() {
            if !attributes.insert(std::mem::discriminant(item)) {
                return Err(body_error("duplicate FETCH attribute"));
            }
            match item {
                MessageDataItem::Uid(value) => uid = Some(*value),
                MessageDataItem::BodyExt {
                    section,
                    origin,
                    data,
                } => {
                    if section.is_some() || origin.is_some() {
                        return Err(body_error("expected a complete BODY[] section"));
                    }
                    body = Some(match &data.0 {
                        None if streamed.is_none() => BodyEvidence::Unavailable,
                        Some(value) if value.as_ref().is_empty() => {
                            BodyEvidence::Bytes(streamed.ok_or(ImapError::Malformed)?)
                        }
                        _ => return Err(ImapError::Malformed),
                    });
                }
                _ => {}
            }
        }
        if uid != Some(self.uid) {
            return Err(body_error("body UID does not match the request"));
        }
        self.observed = true;
        Ok(body)
    }
}

fn full_body_prefix(prefix: &[u8], line_start: usize) -> Result<bool, ImapError> {
    // Literal payload retained as metadata is opaque: a suffix in those
    // bytes cannot combine with a following framing space to form a token.
    if prefix.len().saturating_sub(line_start) < 7
        || !prefix[prefix.len() - 7..].eq_ignore_ascii_case(b"BODY[] ")
    {
        return Ok(false);
    }
    // Classify the fixed-size FETCH head before any whole-prefix probe.
    // Status text can repeat body-like words arbitrarily; a malformed FETCH
    // must fail at its first candidate instead of repeatedly copying and
    // decoding a growing prefix (quadratic work under the metadata cap).
    let Some(rest) = prefix.strip_prefix(b"* ") else {
        return Ok(false);
    };
    let head = &rest[..rest.len().min(11)];
    let Some(space) = head.iter().position(|byte| *byte == b' ') else {
        return Ok(false);
    };
    if space == 0
        || !head[..space].iter().all(u8::is_ascii_digit)
        || !rest
            .get(space + 1..space + 8)
            .is_some_and(|word| word.eq_ignore_ascii_case(b"FETCH ("))
    {
        return Ok(false);
    }
    let mut probe = prefix.to_vec();
    probe.extend_from_slice(b"\"\")\r\n");
    match decode(&probe)? {
        Response::Data(Data::Fetch { .. }) => Ok(true),
        _ => Err(ImapError::Malformed),
    }
}

fn decode(bytes: &[u8]) -> Result<Response<'_>, ImapError> {
    match ResponseCodec::new().decode(bytes) {
        Ok(([], response)) => Ok(response),
        _ => Err(ImapError::Malformed),
    }
}

fn body_error(message: &'static str) -> ImapError {
    ImapError::command("UID FETCH", message)
}

fn check_body_length(bytes: u64) -> Result<(), ImapError> {
    if bytes > MAX_BODY_BYTES {
        return Err(ImapError::BodyBudgetExceeded {
            bytes,
            limit: MAX_BODY_BYTES,
        });
    }
    Ok(())
}

#[cfg(test)]
mod tests;
