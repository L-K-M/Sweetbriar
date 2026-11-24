use rusqlite::params;

use super::{
    LOGO_SIDE_PIXELS, MAX_LOGO_BYTES, PNG_SIGNATURE, account_logo, account_logos, check_logo_png,
    png_crc, set_account_logo,
};
use crate::StoreError;
use crate::appearance::appearance_generation;
use crate::testing::fixtures::{archive_account, index_connection};

const IDAT: &[u8] = b"arbitrary compressed bytes";
const IHDR_TOTAL_BYTES: usize = 8 + 13 + 4;
const IEND_TOTAL_BYTES: usize = 8 + 4;

fn chunk(kind: &str, data: &[u8]) -> Vec<u8> {
    let mut bytes = Vec::new();
    bytes.extend_from_slice(&(data.len() as u32).to_be_bytes());
    bytes.extend_from_slice(kind.as_bytes());
    bytes.extend_from_slice(data);
    bytes.extend_from_slice(&png_crc(kind.as_bytes(), data).to_be_bytes());
    bytes
}

fn ihdr(width: u32, height: u32, depth: u8, color_type: u8) -> Vec<u8> {
    let mut data = Vec::with_capacity(13);
    data.extend_from_slice(&width.to_be_bytes());
    data.extend_from_slice(&height.to_be_bytes());
    data.extend_from_slice(&[depth, color_type, 0, 0, 0]);
    data
}

fn png_with_ihdr(header: &[u8]) -> Vec<u8> {
    let mut bytes = PNG_SIGNATURE.to_vec();
    bytes.extend_from_slice(&chunk("IHDR", header));
    bytes.extend_from_slice(&chunk("IDAT", IDAT));
    bytes.extend_from_slice(&chunk("IEND", &[]));
    bytes
}

fn png_with_idat(data: &[u8]) -> Vec<u8> {
    let mut bytes = PNG_SIGNATURE.to_vec();
    bytes.extend_from_slice(&chunk(
        "IHDR",
        &ihdr(LOGO_SIDE_PIXELS, LOGO_SIDE_PIXELS, 8, 6),
    ));
    bytes.extend_from_slice(&chunk("IDAT", data));
    bytes.extend_from_slice(&chunk("IEND", &[]));
    bytes
}

fn logo_png() -> Vec<u8> {
    png_with_idat(IDAT)
}

fn assert_rejects(bytes: &[u8], needle: &str) {
    match check_logo_png(bytes) {
        Err(detail) => assert!(detail.contains(needle), "expected {needle:?} in {detail:?}"),
        Ok(()) => panic!("accepted a logo whose failure should mention {needle:?}"),
    }
}

#[test]
fn a_synthetic_128_by_128_png_passes() {
    assert_eq!(check_logo_png(&logo_png()), Ok(()));
}

#[test]
fn structural_failures_report_the_first_problem() {
    let valid = logo_png();
    let idat_offset = PNG_SIGNATURE.len() + IHDR_TOTAL_BYTES;

    let mut wrong_signature = valid.clone();
    wrong_signature[0] = 0;

    let short_ihdr = png_with_ihdr(&[0u8; 12]);

    let mut bad_ihdr_crc = valid.clone();
    bad_ihdr_crc[PNG_SIGNATURE.len() + 8 + 13] ^= 0xFF;

    let without_iend = valid[..valid.len() - IEND_TOTAL_BYTES].to_vec();

    let mut trailing_byte = valid.clone();
    trailing_byte.push(0);

    let mut overlong_chunk = valid.clone();
    overlong_chunk[idat_offset..idat_offset + 4].copy_from_slice(&u32::MAX.to_be_bytes());

    for (bytes, needle) in [
        (Vec::new(), "1 through"),
        (vec![0u8; MAX_LOGO_BYTES + 1], "1 through"),
        (wrong_signature, "signature"),
        (short_ihdr, "IHDR must be 13"),
        (bad_ihdr_crc, "IHDR has a bad CRC"),
        (png_with_ihdr(&ihdr(127, LOGO_SIDE_PIXELS, 8, 6)), "pixels"),
        (png_with_ihdr(&ihdr(LOGO_SIDE_PIXELS, 256, 8, 6)), "pixels"),
        (
            png_with_ihdr(&ihdr(LOGO_SIDE_PIXELS, LOGO_SIDE_PIXELS, 3, 2)),
            "bit depth",
        ),
        (without_iend, "not IEND"),
        (trailing_byte, "follow IEND"),
        (overlong_chunk, "past the end"),
    ] {
        assert_rejects(&bytes, needle);
    }
}

#[test]
fn set_then_read_round_trips() {
    let mut conn = index_connection();
    let archive = archive_account(&conn);
    let tx = conn.transaction().unwrap();
    let png = logo_png();
    let written = set_account_logo(&tx, &archive, None, Some(png.as_slice())).unwrap();
    assert_eq!(written.account_id, archive);
    assert_eq!(written.png.as_deref(), Some(png.as_slice()));
    assert_eq!(written.revision, 1);
    assert_eq!(appearance_generation(&tx).unwrap(), 1);
    assert_eq!(
        account_logo(&tx, &archive).unwrap().as_ref(),
        Some(&written)
    );
    assert_eq!(account_logos(&tx).unwrap(), vec![written]);
}

#[test]
fn an_observed_revision_must_match_the_row() {
    let mut conn = index_connection();
    let archive = archive_account(&conn);
    let tx = conn.transaction().unwrap();
    let png = logo_png();
    assert!(matches!(
        set_account_logo(&tx, &archive, Some(1), Some(png.as_slice())),
        Err(StoreError::Conflict { .. })
    ));
    set_account_logo(&tx, &archive, None, Some(png.as_slice())).unwrap();
    assert!(matches!(
        set_account_logo(&tx, &archive, None, Some(png.as_slice())),
        Err(StoreError::Conflict { .. })
    ));
    assert!(matches!(
        set_account_logo(&tx, &archive, Some(2), Some(png.as_slice())),
        Err(StoreError::Conflict { .. })
    ));
}

#[test]
fn an_unchanged_write_keeps_the_revision_and_generation() {
    let mut conn = index_connection();
    let archive = archive_account(&conn);
    let tx = conn.transaction().unwrap();
    let png = logo_png();
    set_account_logo(&tx, &archive, None, Some(png.as_slice())).unwrap();
    let generation = appearance_generation(&tx).unwrap();
    let rewritten = set_account_logo(&tx, &archive, Some(1), Some(png.as_slice())).unwrap();
    assert_eq!(rewritten.revision, 1);
    assert_eq!(appearance_generation(&tx).unwrap(), generation);
}

#[test]
fn clearing_keeps_the_row_with_null_png_and_a_new_revision() {
    let mut conn = index_connection();
    let archive = archive_account(&conn);
    let tx = conn.transaction().unwrap();
    set_account_logo(&tx, &archive, None, Some(logo_png().as_slice())).unwrap();
    let cleared = set_account_logo(&tx, &archive, Some(1), None).unwrap();
    assert_eq!(cleared.png, None);
    assert_eq!(cleared.revision, 2);
    let stored: Option<Vec<u8>> = tx
        .query_row(
            "SELECT png FROM account_logos WHERE account_id = ?1",
            [&archive],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored, None);
    assert_eq!(account_logo(&tx, &archive).unwrap().unwrap(), cleared);
}

#[test]
fn clearing_without_a_row_writes_an_explicit_null_row() {
    let mut conn = index_connection();
    let archive = archive_account(&conn);
    let tx = conn.transaction().unwrap();
    let cleared = set_account_logo(&tx, &archive, None, None).unwrap();
    assert_eq!(cleared.png, None);
    assert_eq!(cleared.revision, 1);
    assert_eq!(appearance_generation(&tx).unwrap(), 1);
    assert_eq!(account_logo(&tx, &archive).unwrap().unwrap(), cleared);
}

#[test]
fn each_changed_write_advances_the_generation_once() {
    let mut conn = index_connection();
    let archive = archive_account(&conn);
    let tx = conn.transaction().unwrap();
    let first = png_with_idat(b"first");
    let second = png_with_idat(b"second");
    assert_eq!(appearance_generation(&tx).unwrap(), 0);
    set_account_logo(&tx, &archive, None, Some(first.as_slice())).unwrap();
    assert_eq!(appearance_generation(&tx).unwrap(), 1);
    set_account_logo(&tx, &archive, Some(1), Some(second.as_slice())).unwrap();
    assert_eq!(appearance_generation(&tx).unwrap(), 2);
    set_account_logo(&tx, &archive, Some(2), None).unwrap();
    assert_eq!(appearance_generation(&tx).unwrap(), 3);
    set_account_logo(&tx, &archive, Some(3), Some(first.as_slice())).unwrap();
    assert_eq!(appearance_generation(&tx).unwrap(), 4);
}

#[test]
fn an_unknown_account_is_not_found() {
    let mut conn = index_connection();
    let tx = conn.transaction().unwrap();
    assert!(matches!(
        account_logo(&tx, "missing"),
        Err(StoreError::NotFound { .. })
    ));
    assert!(matches!(
        set_account_logo(&tx, "missing", None, Some(logo_png().as_slice())),
        Err(StoreError::NotFound { .. })
    ));
}

#[test]
fn a_bad_png_is_invalid_input_before_it_reaches_the_store() {
    let mut conn = index_connection();
    let archive = archive_account(&conn);
    let tx = conn.transaction().unwrap();
    for bytes in [Vec::new(), vec![0u8; MAX_LOGO_BYTES + 1]] {
        assert!(matches!(
            set_account_logo(&tx, &archive, None, Some(bytes.as_slice())),
            Err(StoreError::InvalidInput { .. })
        ));
    }
}

#[test]
fn a_stored_value_that_fails_the_checks_is_corrupt() {
    let mut conn = index_connection();
    let archive = archive_account(&conn);
    conn.execute(
        "INSERT INTO account_logos (account_id, png, revision) VALUES (?1, ?2, 1)",
        params![archive, vec![0u8; 8]],
    )
    .unwrap();
    let tx = conn.transaction().unwrap();
    assert!(matches!(
        account_logo(&tx, &archive),
        Err(StoreError::Corrupt { .. })
    ));
    assert!(matches!(
        account_logos(&tx),
        Err(StoreError::Corrupt { .. })
    ));
}
