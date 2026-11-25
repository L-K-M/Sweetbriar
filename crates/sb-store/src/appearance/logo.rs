//! Per-account logos (spec/02 "Account logos", M6.6b). A logo replaces
//! an account's monogram in the app, never leaves the device except in
//! backups, and is stored as PNG bytes whose structure is checked
//! without decoding pixels.

use rusqlite::{OptionalExtension, Transaction, params};

use crate::StoreError;

use super::{
    increment, invalid, next_generation, require_account, require_changed, set_generation,
    sql_error, valid_key, valid_revision,
};

/// The square side the app normalizes every chosen image to.
pub const LOGO_SIDE_PIXELS: u32 = 128;

/// The largest stored logo; the schema's own bound (spec/02).
pub const MAX_LOGO_BYTES: usize = 65_536;

const PNG_SIGNATURE: [u8; 8] = [0x89, b'P', b'N', b'G', 0x0D, 0x0A, 0x1A, 0x0A];
const IHDR: &[u8; 4] = b"IHDR";
const IEND: &[u8; 4] = b"IEND";
const IHDR_DATA_BYTES: usize = 13;
const CHUNK_HEADER_BYTES: usize = 8;
const CRC_BYTES: usize = 4;
const PNG_CRC_POLYNOMIAL: u32 = 0xEDB8_8320;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StoredAccountLogo {
    pub account_id: String,
    pub png: Option<Vec<u8>>,
    pub revision: i64,
}

/// Check a logo's PNG structure without decoding pixels (spec/02
/// "Format"): the signature, one well-formed chunk walk, a 13-byte
/// `IHDR` with a matching CRC at 128 by 128, and an empty `IEND` that
/// ends the bytes. Returns the first failure's short description.
pub fn check_logo_png(bytes: &[u8]) -> Result<(), String> {
    if bytes.is_empty() || bytes.len() > MAX_LOGO_BYTES {
        return Err(format!("logo must be 1 through {MAX_LOGO_BYTES} bytes"));
    }
    if !bytes.starts_with(&PNG_SIGNATURE) {
        return Err("logo does not start with the PNG signature".to_owned());
    }

    let mut offset = PNG_SIGNATURE.len();
    let header = read_chunk(bytes, offset)?;
    if header.kind != IHDR {
        return Err("the first PNG chunk is not IHDR".to_owned());
    }
    if header.data.len() != IHDR_DATA_BYTES {
        return Err(format!(
            "IHDR must be {IHDR_DATA_BYTES} bytes, not {}",
            header.data.len()
        ));
    }
    if header.crc != png_crc(header.kind, header.data) {
        return Err("IHDR has a bad CRC".to_owned());
    }
    check_ihdr(header.data)?;
    offset += header.total_bytes();

    let mut last: Option<Chunk<'_>> = None;
    while offset < bytes.len() {
        let chunk = read_chunk(bytes, offset)?;
        offset += chunk.total_bytes();
        if chunk.kind == IEND && offset != bytes.len() {
            return Err("bytes follow IEND".to_owned());
        }
        last = Some(chunk);
    }
    let last = last.ok_or_else(|| "logo has no IEND chunk".to_owned())?;
    if last.kind != IEND {
        return Err("the last PNG chunk is not IEND".to_owned());
    }
    if !last.data.is_empty() {
        return Err("IEND must be empty".to_owned());
    }
    if last.crc != png_crc(last.kind, last.data) {
        return Err("IEND has a bad CRC".to_owned());
    }
    Ok(())
}

struct Chunk<'a> {
    kind: &'a [u8],
    data: &'a [u8],
    crc: u32,
}

impl Chunk<'_> {
    fn total_bytes(&self) -> usize {
        CHUNK_HEADER_BYTES + self.data.len() + CRC_BYTES
    }
}

fn read_chunk(bytes: &[u8], offset: usize) -> Result<Chunk<'_>, String> {
    let data_start = offset + CHUNK_HEADER_BYTES;
    if data_start > bytes.len() {
        return Err("a PNG chunk header runs past the end".to_owned());
    }
    let length = u32::from_be_bytes(
        bytes[offset..offset + 4]
            .try_into()
            .expect("four chunk length bytes"),
    ) as usize;
    let data_end = data_start
        .checked_add(length)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| "a PNG chunk runs past the end".to_owned())?;
    let crc_end = data_end
        .checked_add(CRC_BYTES)
        .filter(|end| *end <= bytes.len())
        .ok_or_else(|| "a PNG chunk CRC runs past the end".to_owned())?;
    Ok(Chunk {
        kind: &bytes[offset + 4..data_start],
        data: &bytes[data_start..data_end],
        crc: u32::from_be_bytes(
            bytes[data_end..crc_end]
                .try_into()
                .expect("four chunk CRC bytes"),
        ),
    })
}

fn check_ihdr(data: &[u8]) -> Result<(), String> {
    let width = u32::from_be_bytes(data[0..4].try_into().expect("four width bytes"));
    let height = u32::from_be_bytes(data[4..8].try_into().expect("four height bytes"));
    if width != LOGO_SIDE_PIXELS || height != LOGO_SIDE_PIXELS {
        return Err(format!(
            "logo must be {LOGO_SIDE_PIXELS} by {LOGO_SIDE_PIXELS} pixels, not {width} by {height}"
        ));
    }
    let depth = data[8];
    let color_type = data[9];
    if !valid_depth(color_type, depth) {
        return Err(format!(
            "invalid bit depth {depth} for color type {color_type}"
        ));
    }
    if data[10] != 0 {
        return Err("logo compression method must be 0".to_owned());
    }
    if data[11] != 0 {
        return Err("logo filter method must be 0".to_owned());
    }
    if !matches!(data[12], 0 | 1) {
        return Err("logo interlace method must be 0 or 1".to_owned());
    }
    Ok(())
}

fn valid_depth(color_type: u8, depth: u8) -> bool {
    match color_type {
        0 => matches!(depth, 1 | 2 | 4 | 8 | 16),
        2 => matches!(depth, 8 | 16),
        3 => matches!(depth, 1 | 2 | 4 | 8),
        4 => matches!(depth, 8 | 16),
        6 => matches!(depth, 8 | 16),
        _ => false,
    }
}

/// PNG's CRC-32 (ISO-HDLC): reflected polynomial, all-ones init and
/// final xor over the chunk type and data.
fn png_crc(kind: &[u8], data: &[u8]) -> u32 {
    let mut crc = u32::MAX;
    for byte in kind.iter().chain(data) {
        crc ^= u32::from(*byte);
        for _ in 0..8 {
            let mask = (crc & 1).wrapping_neg();
            crc = (crc >> 1) ^ (PNG_CRC_POLYNOMIAL & mask);
        }
    }
    crc ^ u32::MAX
}

/// One account's logo, or `None` without a row. An unknown account is
/// `NotFound`; a stored value that fails [`check_logo_png`] or a
/// non-positive revision is `Corrupt`, never defaulted (spec/02
/// "Format").
pub fn account_logo(
    tx: &Transaction,
    account_id: &str,
) -> Result<Option<StoredAccountLogo>, StoreError> {
    valid_key(account_id)?;
    require_account(tx, account_id)?;
    let row = tx
        .query_row(
            "SELECT png, revision FROM account_logos WHERE account_id = ?1",
            [account_id],
            |row| Ok((row.get::<_, Option<Vec<u8>>>(0)?, row.get::<_, i64>(1)?)),
        )
        .optional()
        .map_err(sql_error)?;
    row.map(|(png, revision)| stored_logo(account_id, png, revision))
        .transpose()
}

/// Every account's logo, ordered by account id, each validated like
/// [`account_logo`] (spec/02 "Account logos").
pub fn account_logos(tx: &Transaction) -> Result<Vec<StoredAccountLogo>, StoreError> {
    let mut statement = tx
        .prepare("SELECT account_id, png, revision FROM account_logos ORDER BY account_id")
        .map_err(sql_error)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, Option<Vec<u8>>>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })
        .map_err(sql_error)?;
    let mut logos = Vec::new();
    for row in rows {
        let (account_id, png, revision) = row.map_err(sql_error)?;
        logos.push(stored_logo(&account_id, png, revision)?);
    }
    Ok(logos)
}

/// Set or clear an account's logo. `expected_revision` is the observed
/// revision, absent without a row; a mismatch is `Conflict`. An
/// unchanged write writes nothing, and every changed write advances
/// `appearance_generation` once (spec/02 "Writes"). Clearing keeps the
/// row with a null `png`, and clearing without a row writes the explicit
/// inherited row, as identity appearance does.
pub fn set_account_logo(
    tx: &Transaction,
    account_id: &str,
    expected_revision: Option<i64>,
    png: Option<&[u8]>,
) -> Result<StoredAccountLogo, StoreError> {
    if let Some(revision) = expected_revision {
        valid_revision(revision)?;
    }
    if let Some(bytes) = png {
        check_logo_png(bytes).map_err(invalid)?;
    }
    valid_key(account_id)?;
    require_account(tx, account_id)?;
    let current = account_logo(tx, account_id)?;
    if current.as_ref().map(|logo| logo.revision) != expected_revision {
        return Err(StoreError::conflict("account logo revision changed"));
    }
    if let Some(current) = current
        .as_ref()
        .filter(|current| current.png.as_deref() == png)
    {
        return Ok(current.clone());
    }
    let revision = match &current {
        Some(current) => increment(current.revision)?,
        None => 1,
    };
    let generation = next_generation(tx)?;
    let changed = tx
        .execute(
            "INSERT INTO account_logos (account_id, png, revision) VALUES (?1, ?2, ?3)
             ON CONFLICT(account_id) DO UPDATE SET png = excluded.png, revision = excluded.revision",
            params![account_id, png, revision],
        )
        .map_err(sql_error)?;
    require_changed(changed)?;
    set_generation(tx, generation)?;
    account_logo(tx, account_id)?
        .ok_or_else(|| StoreError::corrupt("account logo disappeared after its write"))
}

fn stored_logo(
    account_id: &str,
    png: Option<Vec<u8>>,
    revision: i64,
) -> Result<StoredAccountLogo, StoreError> {
    if revision < 1 {
        return Err(StoreError::corrupt("stored logo revision is not positive"));
    }
    if let Some(png) = &png {
        check_logo_png(png)
            .map_err(|detail| StoreError::corrupt(format!("stored logo: {detail}")))?;
    }
    Ok(StoredAccountLogo {
        account_id: account_id.to_owned(),
        png,
        revision,
    })
}

#[cfg(test)]
mod tests;
