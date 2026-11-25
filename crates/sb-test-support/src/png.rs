//! Synthetic PNG bytes for the account-logo tests (spec/02 "Account
//! logos"). The core checks a logo's structure without decoding
//! pixels, so these carry well-formed chunks with real CRCs around
//! arbitrary IDAT bytes.

/// The 8-byte PNG file signature.
pub const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];

/// The square side every stored logo has.
pub const LOGO_SIDE: u32 = 128;

/// The largest stored logo in bytes, spec/02's bound stated here
/// independently of the implementation's constant.
pub const MAX_LOGO_BYTES: usize = 65_536;

/// Bytes standing in for compressed image data, which no check decodes.
const ARBITRARY_IDAT: &[u8] = b"arbitrary compressed bytes";

/// An 8-bit RGBA image: PNG color type 6 at bit depth 8.
const RGBA_DEPTH: u8 = 8;
const RGBA_COLOR_TYPE: u8 = 6;

/// PNG's CRC-32 (ISO-HDLC) over a chunk's type and data: an independent
/// bit-by-bit implementation of the CRC the PNG specification defines.
pub fn png_crc(kind: &[u8], data: &[u8]) -> u32 {
    const REFLECTED_POLYNOMIAL: u32 = 0xEDB8_8320;
    let mut crc = u32::MAX;
    for byte in kind.iter().chain(data) {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            crc = if crc & 1 == 1 {
                (crc >> 1) ^ REFLECTED_POLYNOMIAL
            } else {
                crc >> 1
            };
        }
    }
    !crc
}

/// One chunk: big-endian length, type, data and CRC.
pub fn png_chunk(kind: &[u8], data: &[u8]) -> Vec<u8> {
    let length = u32::try_from(data.len()).expect("a test chunk fits a PNG length");
    let mut bytes = Vec::with_capacity(data.len() + 12);
    bytes.extend_from_slice(&length.to_be_bytes());
    bytes.extend_from_slice(kind);
    bytes.extend_from_slice(data);
    bytes.extend_from_slice(&png_crc(kind, data).to_be_bytes());
    bytes
}

/// An `IHDR` chunk's 13 data bytes: no compression, filter or interlace
/// variants.
pub fn ihdr_data(width: u32, height: u32, depth: u8, color_type: u8) -> Vec<u8> {
    let mut data = Vec::with_capacity(13);
    data.extend_from_slice(&width.to_be_bytes());
    data.extend_from_slice(&height.to_be_bytes());
    data.extend_from_slice(&[depth, color_type, 0, 0, 0]);
    data
}

/// The signature, an `IHDR` with `header`, one `IDAT` with `idat` and
/// an empty `IEND`.
pub fn png_with(header: &[u8], idat: &[u8]) -> Vec<u8> {
    let mut bytes = PNG_SIGNATURE.to_vec();
    bytes.extend_from_slice(&png_chunk(b"IHDR", header));
    bytes.extend_from_slice(&png_chunk(b"IDAT", idat));
    bytes.extend_from_slice(&png_chunk(b"IEND", &[]));
    bytes
}

/// A 128 x 128 RGBA logo that passes the core's structural check.
pub fn logo_png() -> Vec<u8> {
    logo_png_with(ARBITRARY_IDAT)
}

/// The same logo with other image data, for a second distinct value.
pub fn logo_png_with(idat: &[u8]) -> Vec<u8> {
    png_with(
        &ihdr_data(LOGO_SIDE, LOGO_SIDE, RGBA_DEPTH, RGBA_COLOR_TYPE),
        idat,
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_crc_matches_the_well_known_iend_value() {
        // Every PNG ends in this exact chunk: 0 length, "IEND", AE 42 60 82.
        assert_eq!(png_crc(b"IEND", &[]), 0xAE42_6082);
        assert_eq!(
            png_chunk(b"IEND", &[]),
            [0, 0, 0, 0, b'I', b'E', b'N', b'D', 0xAE, 0x42, 0x60, 0x82]
        );
    }
}
