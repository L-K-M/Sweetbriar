//! Deterministic RFC 822 reconstruction of a decrypted Proton message
//! (spec/04 "Reconstruction"). Proton exposes no raw message, so the
//! original is rebuilt from the stored header block, the decrypted body
//! and the attachments: the same parts always give the same bytes.

use std::fmt;

use crate::Limits;

/// Bump when the reconstruction's bytes change (spec/02's `proton_fetch`
/// `source_ref`).
pub const RECONSTRUCTION_VERSION: u32 = 1;

/// Where an attachment belongs (spec/04 "Reconstruction").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Disposition {
    Inline,
    Attachment,
}

impl Disposition {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::Inline => "inline",
            Self::Attachment => "attachment",
        }
    }
}

/// A typed reconstruction failure; `InvalidInput` carries a static reason
/// only, never message content.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ReconstructError {
    TooLarge,
    InvalidInput(&'static str),
}

impl fmt::Display for ReconstructError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::TooLarge => f.write_str("the reconstructed message exceeds the byte limit"),
            Self::InvalidInput(reason) => {
                write!(f, "the reconstruction input is invalid: {reason}")
            }
        }
    }
}

impl std::error::Error for ReconstructError {}

/// One decrypted attachment part.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconstructPart<'a> {
    pub name: &'a str,
    pub mime_type: &'a str,
    pub disposition: Disposition,
    pub content_id: Option<&'a str>,
    pub data: &'a [u8],
}

/// The decrypted pieces of one Proton message.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReconstructInput<'a> {
    pub message_id: &'a str,
    /// Proton's stored header block.
    pub header: &'a [u8],
    /// `text/plain`, `text/html` or `multipart/*`.
    pub mime_type: &'a str,
    pub body: &'a [u8],
    pub attachments: &'a [ReconstructPart<'a>],
}

const BAD_TYPE: ReconstructError = ReconstructError::InvalidInput(
    "the message mime type is not text/plain, text/html or multipart/*",
);

/// Rebuilds one RFC 822 message from the decrypted parts.
pub fn reconstruct(
    input: &ReconstructInput<'_>,
    limits: &Limits,
) -> Result<Vec<u8>, ReconstructError> {
    let message_id = validate_message_id(input.message_id)?;
    let media_type = normalize_media_type(input.mime_type).ok_or(BAD_TYPE)?;
    let multipart = media_type.starts_with("multipart/");
    if !multipart && !matches!(media_type.as_str(), "text/plain" | "text/html") {
        return Err(BAD_TYPE);
    }
    let cleaned = clean_header(input.header);
    let limit = limits.max_message_bytes();
    // An upper bound before allocating, tight enough not to refuse a
    // message that fits: quoted-printable is at most three bytes per
    // byte plus soft breaks, base64 four per three plus CRLFs, and each
    // part's framing stays under a fixed allowance.
    let mut bound = input
        .header
        .len()
        .saturating_add(input.body.len().saturating_mul(4))
        .saturating_add(4096);
    for part in input.attachments {
        bound = bound
            .saturating_add(base64_len(part.data.len()))
            .saturating_add(part.name.len().saturating_mul(6))
            .saturating_add(512);
    }
    if bound > limit {
        return Err(ReconstructError::TooLarge);
    }
    let mut out = Vec::new();
    if multipart {
        reconstruct_multipart(input, &media_type, message_id, cleaned, &mut out);
    } else {
        reconstruct_text(input, &media_type, message_id, cleaned, &mut out);
    }
    (out.len() <= limit)
        .then_some(out)
        .ok_or(ReconstructError::TooLarge)
}

fn validate_message_id(message_id: &str) -> Result<&str, ReconstructError> {
    if message_id.is_empty() {
        return Err(ReconstructError::InvalidInput("the message id is empty"));
    }
    if message_id.chars().any(char::is_control) {
        return Err(ReconstructError::InvalidInput(
            "the message id carries a control character",
        ));
    }
    Ok(message_id)
}

/// `token/token`, lowercased, parameters dropped.
fn normalize_media_type(value: &str) -> Option<String> {
    let value = value.split(';').next()?.trim();
    let (ty, sub) = value.split_once('/')?;
    (is_token(ty) && is_token(sub))
        .then(|| format!("{}/{}", ty.to_ascii_lowercase(), sub.to_ascii_lowercase()))
}

/// RFC 2045 `token`: no space, no control and no tspecial.
fn is_token(value: &str) -> bool {
    !value.is_empty()
        && value.bytes().all(|byte| {
            byte > 0x20
                && byte < 0x7f
                && !matches!(
                    byte,
                    b'(' | b')'
                        | b'<'
                        | b'>'
                        | b'@'
                        | b','
                        | b';'
                        | b':'
                        | b'\\'
                        | b'"'
                        | b'/'
                        | b'['
                        | b']'
                        | b'?'
                        | b'='
                )
        })
}

/// A rendered entity: its CRLF-terminated header lines and its body.
type Part = (Vec<u8>, Vec<u8>);

fn reconstruct_text(
    input: &ReconstructInput<'_>,
    media_type: &str,
    message_id: &str,
    cleaned: CleanHeader,
    out: &mut Vec<u8>,
) {
    let (charset, cte, encoded) = match std::str::from_utf8(input.body) {
        Ok(text) => (
            Some("utf-8".to_owned()),
            "quoted-printable",
            quoted_printable(text.as_bytes()),
        ),
        // No charset is invented: the stored block's is kept when it had
        // one, else none is declared.
        Err(_) => (cleaned.charset.clone(), "base64", base64_encode(input.body)),
    };
    let charset = charset
        .as_deref()
        .map(|charset| format!("; charset={charset}"))
        .unwrap_or_default();
    let text: Part = (
        format!("Content-Type: {media_type}{charset}\r\nContent-Transfer-Encoding: {cte}\r\n")
            .into_bytes(),
        encoded,
    );
    let has_cid = |part: &&ReconstructPart<'_>| {
        part.disposition == Disposition::Inline && part.content_id.is_some()
    };
    let mut related = Vec::new();
    let mut others = Vec::new();
    for part in input.attachments {
        if has_cid(&part) {
            related.push(attachment_part(part));
        } else {
            others.push(attachment_part(part));
        }
    }
    let mut position = 0;
    // Inline parts with a Content-ID join the text in a related
    // container; everything else follows in a mixed one. With neither,
    // the message is the single text part.
    let text_or_related = if related.is_empty() {
        text
    } else {
        container(
            "multipart/related",
            std::iter::once(text).chain(related).collect(),
            message_id,
            &mut position,
        )
    };
    let top = if others.is_empty() {
        text_or_related
    } else {
        container(
            "multipart/mixed",
            std::iter::once(text_or_related).chain(others).collect(),
            message_id,
            &mut position,
        )
    };
    push_message_header(out, &cleaned.lines, &top.0, message_id);
    out.extend_from_slice(&top.1);
}

fn reconstruct_multipart(
    input: &ReconstructInput<'_>,
    media_type: &str,
    message_id: &str,
    mut cleaned: CleanHeader,
    out: &mut Vec<u8>,
) {
    // The entity's header lines join the stored block (so its own
    // Content-Type, with its boundary, applies); its body follows as is.
    let (entity_header, entity_body) = split_entity(input.body);
    let mut has_content_type = false;
    for line in entity_header {
        has_content_type |= field_name(line).is_some_and(|name| name.as_slice() == b"content-type");
        cleaned.lines.push(line.to_vec());
    }
    if !has_content_type {
        cleaned
            .lines
            .push(format!("Content-Type: {media_type}").into_bytes());
    }
    push_message_header(out, &cleaned.lines, &[], message_id);
    out.extend_from_slice(entity_body);
}

/// An attachment part: `Content-Type`, `Content-Disposition`, an optional
/// `Content-ID`, base64, 76-character lines.
fn attachment_part(part: &ReconstructPart<'_>) -> Part {
    let name = part
        .name
        .chars()
        .filter(|c| !c.is_control())
        .collect::<String>();
    let mut header = format!(
        "Content-Type: {}; {}\r\nContent-Disposition: {}; {}\r\n",
        normalize_media_type(part.mime_type)
            .unwrap_or_else(|| "application/octet-stream".to_owned()),
        name_parameter("name", &name),
        part.disposition.as_str(),
        name_parameter("filename", &name)
    )
    .into_bytes();
    if let Some(content_id) = part.content_id {
        let content_id = content_id
            .chars()
            .filter(|c| !c.is_control() && !matches!(c, '<' | '>'))
            .collect::<String>();
        if !content_id.is_empty() {
            header.extend_from_slice(format!("Content-ID: <{content_id}>\r\n").as_bytes());
        }
    }
    header.extend_from_slice(b"Content-Transfer-Encoding: base64\r\n");
    (header, base64_encode(part.data))
}

fn push_message_header(
    out: &mut Vec<u8>,
    header_lines: &[Vec<u8>],
    body_header: &[u8],
    message_id: &str,
) {
    for line in header_lines {
        out.extend_from_slice(line);
        out.extend_from_slice(b"\r\n");
    }
    out.extend_from_slice(body_header);
    out.extend_from_slice(format!("X-Pm-Internal-Id: {message_id}\r\n\r\n").as_bytes());
}

/// A boundary from the message id, the container's position and a
/// collision counter. A small FNV-1a hash is enough: it only has to be
/// stable and absent from the enclosed bytes, not secret.
fn boundary(message_id: &str, position: u32, attempt: u32) -> String {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in message_id
        .bytes()
        .chain(position.to_le_bytes())
        .chain(attempt.to_le_bytes())
    {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    format!("sb{hash:016x}")
}

fn contains(haystack: &[u8], needle: &str) -> bool {
    !needle.is_empty()
        && haystack
            .windows(needle.len())
            .any(|w| w == needle.as_bytes())
}

/// Renders one multipart container, choosing a boundary that appears in
/// none of the enclosed parts.
fn container(kind: &str, children: Vec<Part>, message_id: &str, position: &mut u32) -> Part {
    let boundary = (0..)
        .map(|attempt| boundary(message_id, *position, attempt))
        .find(|candidate| {
            !children
                .iter()
                .any(|(header, body)| contains(header, candidate) || contains(body, candidate))
        })
        .expect("a counter always finds a free boundary");
    *position += 1;
    let mut body = Vec::new();
    for (header, body_bytes) in &children {
        body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
        body.extend_from_slice(header);
        body.extend_from_slice(b"\r\n");
        body.extend_from_slice(body_bytes);
        body.extend_from_slice(b"\r\n");
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    (
        format!("Content-Type: {kind}; boundary={boundary}\r\n").into_bytes(),
        body,
    )
}

/// `name=<token>` when the name is a bare token, else RFC 2231
/// `name*=utf-8''...` with everything outside the unreserved set encoded.
fn name_parameter(kind: &str, name: &str) -> String {
    if name.is_empty() {
        return format!("{kind}=\"\"");
    }
    if is_token(name) {
        return format!("{kind}={name}");
    }
    let mut encoded = String::new();
    for byte in name.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'_' | b'.' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    format!("{kind}*=utf-8''{encoded}")
}

/// Splits an entity into its header lines (terminators removed) and the
/// body after the first blank line. A body with no blank line is all
/// header.
fn split_entity(body: &[u8]) -> (Vec<&[u8]>, &[u8]) {
    let mut header = Vec::new();
    let mut offset = 0;
    while offset < body.len() {
        let line_end = body[offset..]
            .iter()
            .position(|&byte| matches!(byte, b'\n' | b'\r'))
            .map_or(body.len(), |position| offset + position);
        let mut next = line_end;
        if body.get(next) == Some(&b'\r') {
            next += 1;
        }
        if body.get(next) == Some(&b'\n') {
            next += 1;
        }
        let line = &body[offset..line_end.min(next)];
        if line.is_empty() {
            return (header, &body[next..]);
        }
        header.push(line);
        offset = next;
    }
    (header, &[])
}

struct CleanHeader {
    lines: Vec<Vec<u8>>,
    charset: Option<String>,
}

/// The stored header block with every `Content-Type`,
/// `Content-Transfer-Encoding` and `X-Pm-Internal-Id` field (folds
/// included) removed, and line endings normalized. The first
/// `Content-Type`'s charset is remembered for a non-UTF-8 body.
fn clean_header(header: &[u8]) -> CleanHeader {
    let raw_lines = split_entity(header).0;
    let mut lines = Vec::new();
    let mut charset = None;
    let mut index = 0;
    while index < raw_lines.len() {
        let line = raw_lines[index];
        if line.is_empty() {
            break;
        }
        let end = raw_lines[index + 1..]
            .iter()
            .position(|line| {
                !line
                    .first()
                    .is_some_and(|byte| matches!(byte, b' ' | b'\t'))
            })
            .map_or(raw_lines.len(), |position| index + 1 + position);
        let name = field_name(line);
        if let Some(name) = &name
            && name.as_slice() == b"content-type"
            && charset.is_none()
        {
            charset = content_type_charset(line, &raw_lines[index + 1..end]);
        }
        let dropped = name.as_deref().is_some_and(|n| {
            matches!(
                n,
                b"content-type" | b"content-transfer-encoding" | b"x-pm-internal-id"
            )
        });
        if !dropped {
            lines.extend(raw_lines[index..end].iter().map(|line| line.to_vec()));
        }
        index = end;
    }
    CleanHeader { lines, charset }
}

/// The lowercased field name of a header line, if it has a colon.
fn field_name(line: &[u8]) -> Option<Vec<u8>> {
    let colon = line.iter().position(|byte| *byte == b':')?;
    let name = line[..colon].trim_ascii_end();
    (!name.is_empty()).then(|| name.to_ascii_lowercase())
}

/// The `charset` parameter of a `Content-Type` field, when it is a token.
fn content_type_charset(line: &[u8], continuations: &[&[u8]]) -> Option<String> {
    let colon = line.iter().position(|byte| *byte == b':')?;
    let mut value = String::from_utf8_lossy(&line[colon + 1..]).into_owned();
    for continuation in continuations {
        value.push(' ');
        value.push_str(std::str::from_utf8(continuation).ok()?.trim_start());
    }
    value
        .split(';')
        .skip(1)
        .filter_map(|parameter| parameter.trim().split_once('='))
        .find(|(key, _)| key.trim().eq_ignore_ascii_case("charset"))
        .map(|(_, value)| value.trim().trim_matches('"'))
        .filter(|value| is_token(value))
        .map(str::to_ascii_lowercase)
}

/// Quoted-printable with 76-character lines, CRLF line endings and
/// soft breaks. Trailing whitespace is encoded so it survives.
fn quoted_printable(bytes: &[u8]) -> Vec<u8> {
    let mut out = Vec::with_capacity(bytes.len() * 2);
    let mut line_len = 0usize;
    for (index, &byte) in bytes.iter().enumerate() {
        let previous = if index == 0 { 0 } else { bytes[index - 1] };
        if byte == b'\n' && previous == b'\r' {
            continue;
        }
        if matches!(byte, b'\r' | b'\n') {
            out.extend_from_slice(b"\r\n");
            line_len = 0;
            continue;
        }
        let encoded: [u8; 3];
        let chunk: &[u8] = if byte == b'=' {
            encoded = *b"=3D";
            &encoded
        } else if matches!(byte, b'\t' | b' ')
            && bytes
                .get(index + 1)
                .is_none_or(|next| matches!(next, b'\r' | b'\n'))
        {
            encoded = if byte == b'\t' { *b"=09" } else { *b"=20" };
            &encoded
        } else if (33..=126).contains(&byte) {
            encoded = [byte, 0, 0];
            &encoded[..1]
        } else {
            const HEX: &[u8; 16] = b"0123456789ABCDEF";
            encoded = [b'=', HEX[(byte >> 4) as usize], HEX[(byte & 0x0f) as usize]];
            &encoded
        };
        // A soft break's `=` counts toward the 76 (RFC 2045 6.7).
        if line_len + chunk.len() > 75 {
            out.extend_from_slice(b"=\r\n");
            line_len = 0;
        }
        out.extend_from_slice(chunk);
        line_len += chunk.len();
    }
    out
}

/// The length [`base64_encode`] gives `len` bytes: four per three, and
/// a CRLF between 76-character lines.
fn base64_len(len: usize) -> usize {
    let groups = len.div_ceil(3);
    groups
        .saturating_mul(4)
        .saturating_add(groups.saturating_sub(1) / 19 * 2)
}

/// Standard base64, 76-character lines (19 groups), no trailing break.
fn base64_encode(bytes: &[u8]) -> Vec<u8> {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(bytes.len().div_ceil(3) * 4 + 2);
    for (index, group) in bytes.chunks(3).enumerate() {
        if index > 0 && index % 19 == 0 {
            out.extend_from_slice(b"\r\n");
        }
        let packed = (u32::from(group[0]) << 16)
            | (u32::from(group.get(1).copied().unwrap_or(0)) << 8)
            | u32::from(group.get(2).copied().unwrap_or(0));
        for shift in [18, 12, 6, 0] {
            let char = match shift {
                6 if group.len() == 1 => b'=',
                0 if group.len() < 3 => b'=',
                _ => ALPHABET[((packed >> shift) & 0x3f) as usize],
            };
            out.push(char);
        }
    }
    out
}

#[cfg(test)]
mod tests;
