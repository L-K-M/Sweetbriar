//! `Uuid` (spec/02 rule 3): the textual id form — lowercase,
//! hyphenated — for entities the user or other machines may refer to.

use std::fmt::{self, Write as _};

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
}
