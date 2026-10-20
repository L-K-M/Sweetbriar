//! `Uuid` (spec/02 rule 3): the textual id form — lowercase,
//! hyphenated — for entities the user or other machines may refer to.

use std::error::Error;
use std::fmt::{self, Write as _};
use std::str::FromStr;

/// A UUID held as bytes, so an instance is always a valid id and the
/// stored spelling (lowercase, hyphenated, 8-4-4-4-12) can't drift.
/// Version 4 comes from random bytes through [`Random`](crate::random::Random).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub struct Uuid([u8; 16]);

impl Uuid {
    /// A version-4 UUID from random bytes, setting the version and
    /// RFC 9562 variant bits: any 16 bytes a `Random` seam hands back
    /// make a valid id.
    pub fn from_random_bytes(mut bytes: [u8; 16]) -> Self {
        bytes[6] = (bytes[6] & 0x0F) | 0x40;
        bytes[8] = (bytes[8] & 0x3F) | 0x80;
        Self(bytes)
    }
}

/// Why text failed [`Uuid::from_str`]: a static reason only — the
/// rejected input is the caller's and keeps its own span.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct UuidParseError(&'static str);

impl fmt::Display for UuidParseError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "invalid UUID: {}", self.0)
    }
}

impl Error for UuidParseError {}

impl FromStr for Uuid {
    type Err = UuidParseError;

    /// The canonical data form only: exactly 36 ASCII bytes — lowercase
    /// hexadecimal with hyphens at 8/13/18/23 (spec/06). Every byte is
    /// reconstructed as written: unlike [`Uuid::from_random_bytes`] no
    /// version or variant bits are rewritten, so the nil UUID and every
    /// canonical spelling are valid input.
    fn from_str(text: &str) -> Result<Self, Self::Err> {
        let bytes = text.as_bytes();
        if bytes.len() != 36 {
            return Err(UuidParseError("expected 36 hyphenated bytes"));
        }
        let mut out = [0u8; 16];
        let mut digit = 0usize;
        for (i, &byte) in bytes.iter().enumerate() {
            if matches!(i, 8 | 13 | 18 | 23) {
                if byte != b'-' {
                    return Err(UuidParseError("expected '-' separator"));
                }
                continue;
            }
            let nibble = match byte {
                b'0'..=b'9' => byte - b'0',
                b'a'..=b'f' => byte - b'a' + 10,
                _ => return Err(UuidParseError("expected lowercase hexadecimal")),
            };
            out[digit / 2] |= nibble << (4 * (1 - digit % 2));
            digit += 1;
        }
        Ok(Self(out))
    }
}

impl fmt::Display for Uuid {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        for (slot, byte) in self.0.iter().enumerate() {
            if matches!(slot, 4 | 6 | 8 | 10) {
                f.write_char('-')?;
            }
            write!(f, "{byte:02x}")?;
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Sequential bytes pin the whole layout; the version and variant
    /// nibbles show the only two bytes `from_random_bytes` rewrites.
    #[test]
    fn display_is_lowercase_hyphenated_version_4() {
        let uuid = Uuid::from_random_bytes([0, 1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12, 13, 14, 15]);
        assert_eq!(uuid.to_string(), "00010203-0405-4607-8809-0a0b0c0d0e0f");
    }

    /// Set bits are masked, not assumed: bytes already carrying other
    /// version or variant bits still land on 4 and the RFC 9562
    /// variant, all in lowercase.
    #[test]
    fn version_and_variant_bits_are_forced() {
        let uuid = Uuid::from_random_bytes([0xFF; 16]);
        assert_eq!(uuid.to_string(), "ffffffff-ffff-4fff-bfff-ffffffffffff");
        let other = Uuid::from_random_bytes([0, 0, 0, 0, 0, 0, 0x14, 0, 0x22, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(other.to_string(), "00000000-0000-4400-a200-000000000000");
    }

    /// Canonical lowercase text parses byte-for-byte and displays back
    /// identically — nil, v1, v4 and the all-ones spelling are all
    /// accepted without version/variant rewriting (spec/06).
    #[test]
    fn parse_roundtrips_every_canonical_spelling() {
        for text in [
            "00000000-0000-0000-0000-000000000000",
            "6ba7b810-9dad-11d1-80b4-00c04fd430c8",
            "12345678-1234-4234-8234-123456789abc",
            "ffffffff-ffff-ffff-ffff-ffffffffffff",
        ] {
            let uuid: Uuid = text.parse().expect("canonical spelling parses");
            assert_eq!(uuid.to_string(), text);
        }
    }

    /// Anything but exactly 36 lowercase-hex hyphenated bytes fails:
    /// uppercase, wrong length or hyphen positions, non-hex digits and
    /// braces are all rejected.
    #[test]
    fn parse_rejects_non_canonical_spellings() {
        for text in [
            "",
            "12345678123442348234123456789abc",
            "12345678-1234-4234-8234-123456789ab",
            "12345678-1234-4234-8234-123456789abcd",
            "12345678-1234-4234-8234-12345678gabc",
            "123456781234-4234-8234-123456789abc",
            "12345678-12344234-8234-123456789abc",
            "12345678-1234-4234-8234-123456789ABC",
            "{12345678-1234-4234-8234-123456789abc}",
            "12345678-1234-4234-8234-12345678９abc",
        ] {
            assert!(text.parse::<Uuid>().is_err(), "{text}");
        }
    }
}
