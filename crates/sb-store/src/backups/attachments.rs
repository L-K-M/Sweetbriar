//! Streaming counterpart of drafts::parse_attachments. A caller must discard
//! every emitted reference if parsing, cancellation or the sink fails.

use std::io::{self, Read};

use super::json::Reader;

/// Only 64-byte hashes can match a manifest hash. Other strings remain valid
/// draft references, but are represented as a deterministic mismatch.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Hash {
    Bytes([u8; 64]),
    Unmatchable,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) struct Reference {
    pub hash: Hash,
    pub size: u64,
}

fn invalid() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, "invalid draft attachments")
}

pub(super) fn decode(
    input: impl Read,
    mut cancel: impl FnMut() -> io::Result<()>,
    mut sink: impl FnMut(Reference) -> io::Result<()>,
) -> io::Result<()> {
    let mut p = Reader::new(input, &mut cancel);
    p.whitespace()?;
    p.expect(b'[')?;
    p.whitespace()?;
    if p.peek()? == Some(b']') {
        p.take()?;
    } else {
        loop {
            let reference = entry(&mut p)?;
            p.check_cancel()?;
            sink(reference)?;
            p.whitespace()?;
            match p.take()? {
                Some(b',') => p.whitespace()?,
                Some(b']') => break,
                _ => return Err(invalid()),
            }
        }
    }
    p.finish()?;
    p.check_cancel()
}

fn entry<R: Read, C: FnMut() -> io::Result<()>>(p: &mut Reader<'_, R, C>) -> io::Result<Reference> {
    p.expect(b'{')?;
    p.whitespace()?;
    let mut hash = None;
    let mut filename = false;
    let mut media_type = false;
    let mut size = None;
    if p.peek()? == Some(b'}') {
        p.take()?;
    } else {
        loop {
            let key = p.name(["object_sha256", "filename", "media_type", "size"])?;
            p.whitespace()?;
            p.expect(b':')?;
            p.whitespace()?;
            match key {
                Some(0) if p.peek()? == Some(b'"') => {
                    let mut bytes = [0; 64];
                    let mut len = 0usize;
                    let mut too_long = false;
                    p.string(|c| {
                        let mut buf = [0; 4];
                        let next = c.encode_utf8(&mut buf).as_bytes();
                        if next.len() <= bytes.len() - len && !too_long {
                            bytes[len..len + next.len()].copy_from_slice(next);
                            len += next.len();
                        } else {
                            too_long = true;
                        }
                        Ok(())
                    })?;
                    hash = Some(if !too_long && len == 64 {
                        Hash::Bytes(bytes)
                    } else {
                        Hash::Unmatchable
                    });
                }
                Some(1 | 2) if p.peek()? == Some(b'"') => {
                    p.string(|_| Ok(()))?;
                    if key == Some(1) {
                        filename = true;
                    } else {
                        media_type = true;
                    }
                }
                Some(3) if matches!(p.peek()?, Some(b'-' | b'0'..=b'9')) => {
                    size = p.value_number()?;
                }
                _ => {
                    p.skip_value_serde(2)?;
                    match key {
                        Some(0) => hash = None,
                        Some(1) => filename = false,
                        Some(2) => media_type = false,
                        Some(3) => size = None,
                        _ => {}
                    }
                }
            }
            p.whitespace()?;
            match p.take()? {
                Some(b',') => p.whitespace()?,
                Some(b'}') => break,
                _ => return Err(invalid()),
            }
        }
    }
    if !filename || !media_type {
        return Err(invalid());
    }
    Ok(Reference {
        hash: hash.ok_or_else(invalid)?,
        size: size.ok_or_else(invalid)?,
    })
}

#[cfg(test)]
mod tests;
