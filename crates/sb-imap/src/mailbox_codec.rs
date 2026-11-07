//! Strict RFC 3501 section 5.1.3 mailbox-name codec (M2.2b0).
//!
//! `encode` maps a Rust `&str` to the modified UTF-7 wire form and
//! `decode` is its strict inverse: malformed shifts, noncanonical
//! aliases, odd UTF-16 byte counts and lone surrogates are errors, never
//! replacement characters. The module is private to `sb-imap`; M2.2b1
//! applies it at the LIST/EXAMINE wire boundary. No socket or read API
//! lives here.

use std::fmt;

use base64::{Engine, engine::general_purpose::STANDARD_NO_PAD};

/// A mailbox-name codec failure. The formatted text is static and never
/// contains the offending wire input, so it is safe to log.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum MailboxCodecError {
    /// A wire byte outside printable US-ASCII (0x20..=0x7e).
    NonPrintableWire,
    /// A `&` shift had no `-` terminator.
    UnterminatedShift,
    /// A shift payload is not canonical modified BASE64.
    InvalidShift,
    /// A decoded shift had an odd number of UTF-16 bytes.
    OddUtf16Length,
    /// A decoded shift contained a lone or malformed surrogate.
    LoneSurrogate,
    /// The wire form is not the canonical encoding of the decoded name.
    NonCanonical,
}

impl fmt::Display for MailboxCodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::NonPrintableWire => "mailbox wire bytes must be printable US-ASCII",
            Self::UnterminatedShift => "mailbox shift has no '-' terminator",
            Self::InvalidShift => "mailbox shift payload is not canonical modified BASE64",
            Self::OddUtf16Length => "mailbox shift decodes to an odd number of UTF-16 bytes",
            Self::LoneSurrogate => "mailbox shift decodes to a lone UTF-16 surrogate",
            Self::NonCanonical => "mailbox wire form is not the canonical encoding",
        })
    }
}

impl std::error::Error for MailboxCodecError {}

/// Encodes `name` as modified UTF-7 (RFC 3501 section 5.1.3).
///
/// Printable US-ASCII represents itself except `&`, which becomes `&-`.
/// Every other character is UTF-16BE in one modified BASE64 shift per
/// maximal non-ASCII run, with `,` for `/` and no padding. The encoding
/// is total: controls, NUL and supplementary characters are supported,
/// and no case folding or normalization is applied.
pub(crate) fn encode(name: &str) -> String {
    let mut out = String::with_capacity(name.len());
    let mut shifted: Vec<u8> = Vec::new();

    for ch in name.chars() {
        if is_printable_ascii_char(ch) {
            flush_shift(&mut shifted, &mut out);
            if ch == '&' {
                out.push_str("&-");
            } else {
                out.push(ch);
            }
        } else {
            let mut units = [0_u16; 2];
            for unit in ch.encode_utf16(&mut units) {
                shifted.extend_from_slice(&unit.to_be_bytes());
            }
        }
    }

    flush_shift(&mut shifted, &mut out);
    out
}

/// Decodes a modified UTF-7 wire form strictly.
///
/// Every wire byte must be printable US-ASCII. A shift must be
/// terminated, its payload must be canonical modified BASE64 (no `/`,
/// `=`, whitespace, or nonzero trailing bits), the decoded UTF-16BE
/// must have an even byte count, and [`String::from_utf16`] must accept
/// it. Finally the decoded name must re-encode to the exact wire bytes,
/// which rejects superfluous shifts and modified BASE64 encodings of
/// printable US-ASCII (including `&`). Empty input decodes to empty.
pub(crate) fn decode(wire: &[u8]) -> Result<String, MailboxCodecError> {
    if wire.iter().any(|byte| !is_printable_ascii_byte(*byte)) {
        return Err(MailboxCodecError::NonPrintableWire);
    }

    let mut out = String::with_capacity(wire.len());
    let mut index = 0;

    while index < wire.len() {
        if wire[index] != b'&' {
            out.push(char::from(wire[index]));
            index += 1;
            continue;
        }

        let payload_start = index + 1;
        let mut terminator = payload_start;
        while terminator < wire.len() && wire[terminator] != b'-' {
            terminator += 1;
        }
        if terminator == wire.len() {
            return Err(MailboxCodecError::UnterminatedShift);
        }

        let payload = &wire[payload_start..terminator];
        if payload.is_empty() {
            out.push('&');
        } else {
            let units = decode_shift(payload)?;
            let text = String::from_utf16(&units).map_err(|_| MailboxCodecError::LoneSurrogate)?;
            out.push_str(&text);
        }
        index = terminator + 1;
    }

    if encode(&out).as_bytes() != wire {
        return Err(MailboxCodecError::NonCanonical);
    }

    Ok(out)
}

fn flush_shift(shifted: &mut Vec<u8>, out: &mut String) {
    if shifted.is_empty() {
        return;
    }

    let payload = STANDARD_NO_PAD.encode(shifted.as_slice()).replace('/', ",");
    out.push('&');
    out.push_str(&payload);
    out.push('-');
    shifted.clear();
}

fn decode_shift(payload: &[u8]) -> Result<Vec<u16>, MailboxCodecError> {
    let mut standard = Vec::with_capacity(payload.len());
    for &byte in payload {
        match byte {
            b',' => standard.push(b'/'),
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'+' => standard.push(byte),
            _ => return Err(MailboxCodecError::InvalidShift),
        }
    }

    // `STANDARD_NO_PAD` requires no padding and rejects nonzero
    // trailing bits; the modified alphabet has already been mapped.
    let bytes = STANDARD_NO_PAD
        .decode(standard.as_slice())
        .map_err(|_| MailboxCodecError::InvalidShift)?;

    if bytes.len() % 2 != 0 {
        return Err(MailboxCodecError::OddUtf16Length);
    }

    let (pairs, remainder) = bytes.as_chunks::<2>();
    debug_assert!(remainder.is_empty());
    Ok(pairs.iter().map(|pair| u16::from_be_bytes(*pair)).collect())
}

fn is_printable_ascii_byte(byte: u8) -> bool {
    (0x20..=0x7e).contains(&byte)
}

fn is_printable_ascii_char(ch: char) -> bool {
    (0x20..=0x7e).contains(&(ch as u32))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn roundtrip(name: &str) {
        let wire = encode(name);
        assert!(wire.bytes().all(is_printable_ascii_byte));
        let decoded = decode(wire.as_bytes()).expect("canonical wire must decode");
        assert_eq!(decoded, name);
        assert_eq!(encode(&decoded).as_bytes(), wire.as_bytes());
    }

    #[test]
    fn encode_ascii_is_identity() {
        assert_eq!(encode(""), "");
        assert_eq!(encode("Inbox"), "Inbox");
        assert_eq!(encode("Drafts"), "Drafts");
        assert_eq!(encode("Notes/Work"), "Notes/Work");
        assert_eq!(encode("~peter/mail/"), "~peter/mail/");
        roundtrip("");
    }

    #[test]
    fn encode_ampersand_is_doubled() {
        assert_eq!(encode("&"), "&-");
        assert_eq!(encode("&&"), "&-&-");
        assert_eq!(encode("AT&T"), "AT&-T");
    }

    #[test]
    fn rfc3501_section_5_1_3_vectors() {
        assert_eq!(
            encode("~peter/mail/台北/日本語"),
            "~peter/mail/&U,BTFw-/&ZeVnLIqe-"
        );
        assert_eq!(
            decode(b"~peter/mail/&U,BTFw-/&ZeVnLIqe-").unwrap(),
            "~peter/mail/台北/日本語"
        );
        assert_eq!(encode("台北日本語"), "&U,BTF2XlZyyKng-");
        assert_eq!(decode(b"&U,BTF2XlZyyKng-").unwrap(), "台北日本語");
        assert_eq!(decode(b"&Jjo-!").unwrap(), "☺!");
    }

    #[test]
    fn io_imap_pinned_vectors() {
        assert_eq!(encode("Отправленные"), "&BB4EQgQ,BEAEMAQyBDsENQQ9BD0ESwQ1-");
        assert_eq!(
            decode(b"&BB4EQgQ,BEAEMAQyBDsENQQ9BD0ESwQ1-").unwrap(),
            "Отправленные"
        );
        assert_eq!(encode("Notes/Брошены"), "Notes/&BBEEQAQ+BEgENQQ9BEs-");
        assert_eq!(
            decode(b"Notes/&BBEEQAQ+BEgENQQ9BEs-").unwrap(),
            "Notes/Брошены"
        );
    }

    #[test]
    fn supplementary_and_control_vectors() {
        assert_eq!(encode("📮"), "&2D3c7g-");
        assert_eq!(decode(b"&2D3c7g-").unwrap(), "📮");
        assert_eq!(encode("\u{0}"), "&AAA-");
        assert_eq!(decode(b"&AAA-").unwrap(), "\u{0}");
        assert_eq!(encode("\u{1}\u{1f}\u{7f}"), "&AAEAHwB,-");
        assert_eq!(decode(b"&AAEAHwB,-").unwrap(), "\u{1}\u{1f}\u{7f}");
        assert_eq!(encode("\u{0}É"), "&AAAAyQ-");
        assert_eq!(decode(b"&AAAAyQ-").unwrap(), "\u{0}É");
        // '+' is part of the standard alphabet and survives the ',' swap.
        assert_eq!(encode("Ϡ"), "&A+A-");
        assert_eq!(decode(b"&A+A-").unwrap(), "Ϡ");
    }

    #[test]
    fn decode_rejects_unterminated_shift() {
        for wire in [
            "&",
            "&U,BTFw",
            "abc&",
            "&Jjo!", // RFC example: no shift back to US-ASCII before '!'
            "&U,BTFw-&",
            "&U,BTFw-&ZeVnLIqe",
        ] {
            assert_eq!(
                decode(wire.as_bytes()),
                Err(MailboxCodecError::UnterminatedShift),
                "wire {wire:?}"
            );
        }
    }

    #[test]
    fn decode_rejects_invalid_shift_alphabet() {
        for wire in [
            "&U,BTFw*-",
            "&U,BTFw/-",
            "&U,BTFw=-",
            "&U,BTFw -",
            "&U,BTFw!-",
            "&U,BTFw&-",
            "&U,BTFw.-",
            "&U,BTFw_-",
            "&U,BTFw'-",
            "&Q-",
            "&QQB-",
        ] {
            assert_eq!(
                decode(wire.as_bytes()),
                Err(MailboxCodecError::InvalidShift),
                "wire {wire:?}"
            );
        }
    }

    #[test]
    fn decode_rejects_odd_utf16_byte_count() {
        for wire in ["&QQ-", "&QQAA-"] {
            assert_eq!(
                decode(wire.as_bytes()),
                Err(MailboxCodecError::OddUtf16Length),
                "wire {wire:?}"
            );
        }
    }

    #[test]
    fn decode_rejects_lone_surrogates() {
        for wire in ["&2AA-", "&3AA-", "&2AA2AA-", "&3AA3AA-"] {
            assert_eq!(
                decode(wire.as_bytes()),
                Err(MailboxCodecError::LoneSurrogate),
                "wire {wire:?}"
            );
        }
    }

    #[test]
    fn decode_rejects_noncanonical_wire() {
        for wire in [
            "&AEE-",              // shifted printable 'A'
            "&ACY-",              // shifted printable '&'
            "&AEEAyQ-",           // shift encoding printable ASCII plus non-ASCII
            "&U,BTFw-&ZeVnLIqe-", // superfluous adjacent shift (RFC example)
        ] {
            assert_eq!(
                decode(wire.as_bytes()),
                Err(MailboxCodecError::NonCanonical),
                "wire {wire:?}"
            );
        }
    }

    #[test]
    fn decode_rejects_non_ascii_wire_bytes() {
        let wires: [&[u8]; 6] = [
            "台北".as_bytes(),
            "~peter/mail/台北".as_bytes(),
            b"\x00\x01\x1f\x7f",
            b"Drafts\x00",
            b"&\x00-",
            b"&\x7f-",
        ];
        for wire in wires {
            assert_eq!(
                decode(wire),
                Err(MailboxCodecError::NonPrintableWire),
                "wire {wire:?}"
            );
        }
    }

    #[test]
    fn bounded_unicode_sweep_roundtrips() {
        let mut checked = 0_u32;

        for cp in (0..=0x10_ffff).step_by(0x100) {
            if let Some(ch) = char::from_u32(cp) {
                roundtrip(&ch.to_string());
                checked += 1;
            }
        }

        for cp in [
            0x00, 0x01, 0x1f, 0x20, 0x25, 0x26, 0x27, 0x7e, 0x7f, 0x80, 0xff, 0x100, 0xfffe,
            0xffff, 0x1_0000, 0x1_f4ee, 0x10_ffff,
        ] {
            roundtrip(
                &char::from_u32(cp)
                    .expect("non-surrogate code point")
                    .to_string(),
            );
            checked += 1;
        }

        assert!(checked > 4_000, "sweep checked only {checked}");
    }

    #[test]
    fn bounded_mixed_string_roundtrips() {
        let alphabet: Vec<char> =
            "ab&/~\u{0}\u{1}\u{1f}\u{7f}\u{80}\u{ff}\u{20ac}\u{53f0}\u{6717}\u{65e5}\u{1f4ee}\u{410}"
                .chars()
                .collect();
        let mut state = 0x2545_f491_4f6c_dd1d_u64;
        let mut next = || {
            state = state
                .wrapping_mul(6_364_136_223_846_793_005)
                .wrapping_add(1_442_695_040_888_963_407);
            state
        };

        for _ in 0..256 {
            let len = (next() % 24) as usize + 1;
            let mut name = String::new();
            for _ in 0..len {
                name.push(alphabet[(next() % alphabet.len() as u64) as usize]);
            }
            roundtrip(&name);
        }
    }

    #[test]
    fn error_display_never_includes_wire_input() {
        let invalid = decode(b"&U,BTFw*-").expect_err("invalid shift must fail");
        assert_eq!(
            invalid.to_string(),
            "mailbox shift payload is not canonical modified BASE64"
        );
        assert!(!invalid.to_string().contains("U,BTFw"));
        assert!(!invalid.to_string().contains('*'));

        let non_ascii = decode(b"&\x00-").expect_err("non-printable wire must fail");
        assert_eq!(
            non_ascii.to_string(),
            "mailbox wire bytes must be printable US-ASCII"
        );
        assert!(!non_ascii.to_string().contains('\0'));
    }
}
