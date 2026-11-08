//! Native proofs for pure header request and row validation: request
//! normalization and validation, the exact PEEK request's wire form,
//! and whole-FETCH-frame row evidence decoded with the pinned codec.
//! Everything is synthetic: no socket, store or provider policy is
//! involved. Complete coverage is read coverage of the requested batch
//! only, never MIME or archive coverage, and the batches carry no
//! deletion or checkpoint authority.

use io_imap::codec::{ResponseCodec, decode::Decoder};
use io_imap::types::command::Command;
use io_imap::types::response::Response;

use crate::ImapError;

use super::*;

/// One synthetic observed date and its UTC Unix milliseconds:
/// 2020-01-01 00:00:00 +0200 is 2019-12-31 22:00:00 UTC.
const DATE: &str = "01-Jan-2020 00:00:00 +0200";
const DATE_MS: i64 = 1_577_829_600_000;
/// The same instant behind a negative offset: 2020-01-01 07:00:00 UTC.
const DATE_NEG: &str = "01-Jan-2020 00:00:00 -0700";
const DATE_NEG_MS: i64 = 1_577_862_000_000;
/// A raw UTF-8 header section; `é` is two octets on the wire.
const TEXT: &str = "Subject: café\r\n";

fn nz(uid: u32) -> NonZeroU32 {
    NonZeroU32::new(uid).expect("fixture UID is nonzero")
}

fn uids_of(uids: &[u32]) -> Vec<NonZeroU32> {
    uids.iter().copied().map(nz).collect()
}

fn request(uids: &[u32], fields: &[&str], mode: HeaderFetchMode) -> HeaderRequest {
    let fields = fields.iter().map(|name| name.to_string()).collect();
    HeaderRequest::new(uids_of(uids), fields, mode).expect("fixture request validates")
}

fn context() -> MailboxContext {
    MailboxContext {
        name: "INBOX".into(),
        uid_validity: nz(4100),
    }
}

/// One wire frame decoded with the pinned codec, the way the
/// connection driver feeds the builder.
fn decode(wire: &[u8]) -> Result<Response<'_>, ImapError> {
    let (rest, response) = ResponseCodec::new()
        .decode(wire)
        .map_err(|_| ImapError::Malformed)?;
    assert!(rest.is_empty());
    Ok(response)
}

/// One builder run: every push result, kept so a test can continue
/// past a failure the way a caller ignoring errors would, plus the
/// ambiguity hint and the finished batch.
struct Driven {
    pushes: Vec<Result<(), ImapError>>,
    ambiguous: bool,
    finish: Result<HeaderBatch, ImapError>,
}

fn observe(request: &HeaderRequest, frames: &[String], changed: bool) -> Driven {
    let mut builder = HeaderBuilder::new(request);
    let mut pushes = Vec::new();
    for frame in frames {
        let response = decode(frame.as_bytes()).expect("fixture frame decodes");
        pushes.push(builder.push(&response));
    }
    let ambiguous = builder.ambiguous();
    Driven {
        pushes,
        ambiguous,
        finish: builder.finish(context(), changed),
    }
}

/// One literal announcement followed by its exact raw bytes.
fn lit(text: &str) -> String {
    format!("{{{}}}\r\n{text}", text.len())
}

/// One FETCH frame from space-joined items.
fn fetch_frame(seq: u32, items: &[String]) -> String {
    format!("* {seq} FETCH ({})\r\n", items.join(" "))
}

fn uid(value: u32) -> String {
    format!("UID {value}")
}

fn flags(names: &str) -> String {
    format!("FLAGS ({names})")
}

fn date() -> String {
    date_at(DATE)
}

fn date_at(observed: &str) -> String {
    format!("INTERNALDATE \"{observed}\"")
}

fn size(bytes: u32) -> String {
    format!("RFC822.SIZE {bytes}")
}

/// One BODY[...] echo carrying `text` as a raw literal.
fn body(echo: &str, text: &str) -> String {
    format!("{echo} {}", lit(text))
}

/// One BODY[...] echo carrying NIL: no literal was observed.
fn body_nil(echo: &str) -> String {
    format!("{echo} NIL")
}

/// The exact request wire line under a fixed tag.
fn encode_command(request: &HeaderRequest) -> String {
    let command = Command::new("R1", request.command()).expect("fixture tag");
    String::from_utf8(crate::metadata_encoding::encode(&command).expect("exact UID encoding"))
        .expect("ASCII wire line")
}

#[test]
fn request_rejects_empty_and_oversized_uid_batches() {
    let empty = HeaderRequest::new(vec![], vec!["SUBJECT".to_string()], HeaderFetchMode::Full);
    assert_eq!(empty.unwrap_err(), HeaderRequestError::EmptyUids);

    let distinct: Vec<u32> = (1..=251).collect();
    let too_many = HeaderRequest::new(
        uids_of(&distinct),
        vec!["SUBJECT".to_string()],
        HeaderFetchMode::Full,
    );
    assert_eq!(too_many.unwrap_err(), HeaderRequestError::TooManyUids);

    // The limit bounds the input batch before deduplication.
    let repeated = HeaderRequest::new(
        uids_of(&[7; 251]),
        vec!["SUBJECT".to_string()],
        HeaderFetchMode::Full,
    );
    assert_eq!(repeated.unwrap_err(), HeaderRequestError::TooManyUids);

    let max = request(
        &(1..=250).collect::<Vec<_>>(),
        &["SUBJECT"],
        HeaderFetchMode::Full,
    );
    assert_eq!(max.uids().len(), HeaderRequest::MAX_UIDS);
}

#[test]
fn request_rejects_empty_and_invalid_field_names() {
    let empty = HeaderRequest::new(vec![nz(5)], vec![], HeaderFetchMode::Full);
    assert_eq!(empty.unwrap_err(), HeaderRequestError::EmptyFields);
    // Empty name, space, colon, control and non-ASCII octets.
    for name in ["", "SUB JECT", "SUB:JECT", "SUB\u{1}JECT", "café"] {
        let invalid =
            HeaderRequest::new(vec![nz(5)], vec![name.to_string()], HeaderFetchMode::Full);
        assert_eq!(
            invalid.unwrap_err(),
            HeaderRequestError::InvalidFieldName,
            "{name:?}"
        );
    }
}

#[test]
fn request_normalizes_uids_fields_and_preserves_the_mode() {
    let prefix = HeaderFetchMode::Prefix { max_bytes: nz(8) };
    let normalized = request(
        &[u32::MAX, 7, 3, 7],
        &["subject", "SUBJECT", "From", "from"],
        prefix,
    );
    // Sorted and deduplicated UIDs, u32::MAX included, with no range
    // expansion: exactly the three explicit UIDs remain.
    assert_eq!(normalized.uids().to_vec(), uids_of(&[3, 7, u32::MAX]));
    assert_eq!(normalized.fields().to_vec(), ["FROM", "SUBJECT"]);
    assert_eq!(normalized.mode(), prefix);
    assert_eq!(
        request(&[5], &["subject"], HeaderFetchMode::Full).mode(),
        HeaderFetchMode::Full
    );
}

#[test]
fn command_is_a_uid_peek_over_the_selected_fields() {
    let full = request(&[7, u32::MAX], &["subject", "From"], HeaderFetchMode::Full);
    assert_eq!(
        encode_command(&full),
        "R1 UID FETCH 7,4294967295 (UID FLAGS INTERNALDATE RFC822.SIZE \
         BODY.PEEK[HEADER.FIELDS (FROM SUBJECT)])\r\n"
    );
    let prefix = request(
        &[3],
        &["subject"],
        HeaderFetchMode::Prefix {
            max_bytes: nz(1024),
        },
    );
    assert_eq!(
        encode_command(&prefix),
        "R1 UID FETCH 3 (UID FLAGS INTERNALDATE RFC822.SIZE \
         BODY.PEEK[HEADER.FIELDS (SUBJECT)]<0.1024>)\r\n"
    );
}

/// Complete coverage here means only that every UID of this requested
/// batch was read unambiguously: not MIME or archive coverage, and no
/// deletion or checkpoint authority follows from it.
#[test]
fn rows_preserve_raw_utf8_keyed_by_the_uid_item_after_the_literal() {
    let req = request(&[5], &["From", "subject"], HeaderFetchMode::Full);
    // The response items are reordered and lower-case and the echoed
    // field list is recased and reversed; the UID item follows the
    // literal, and the sequence number is never the key.
    let frame = fetch_frame(
        9,
        &[
            body("body[header.fields (subject from)]", TEXT),
            size(4242),
            date(),
            flags("\\Seen"),
            uid(5),
        ],
    );
    let driven = observe(&req, &[frame], false);
    assert!(driven.pushes[0].is_ok());
    assert!(!driven.ambiguous, "one whole frame is not ambiguous");
    let batch = driven.finish.expect("one complete row");
    assert_eq!(batch.mailbox, context());
    assert_eq!(batch.requested_uids, uids_of(&[5]));
    assert!(batch.missing_uids.is_empty());
    assert_eq!(batch.coverage, HeaderCoverage::Complete);
    assert_eq!(
        batch.rows,
        vec![HeaderRow {
            uid: nz(5),
            flags: vec!["\\Seen".to_string()],
            internal_date_ms: DATE_MS,
            rfc822_size: 4242,
            headers: HeaderBytes::Full(TEXT.as_bytes().to_vec()),
        }]
    );
}

#[test]
fn known_empty_flags_utc_dates_and_size_hints_survive() {
    let req = request(&[5, 6], &["SUBJECT"], HeaderFetchMode::Full);
    // Rows arrive out of order. An explicit empty FLAGS list is known
    // "no flags" and still a row, and RFC822.SIZE is only the server's
    // hint: 4242 stays 4242 beside short literals.
    let frames = [
        fetch_frame(
            1,
            &[
                uid(6),
                flags("\\Draft"),
                date_at(DATE_NEG),
                size(4242),
                body("BODY[HEADER.FIELDS (SUBJECT)]", "Subject: two\r\n"),
            ],
        ),
        fetch_frame(
            2,
            &[
                uid(5),
                flags(""),
                date(),
                size(4242),
                body("BODY[HEADER.FIELDS (SUBJECT)]", "Subject: one\r\n"),
            ],
        ),
    ];
    let driven = observe(&req, &frames, false);
    assert!(driven.pushes.iter().all(Result::is_ok));
    assert!(!driven.ambiguous);
    let batch = driven.finish.expect("two complete rows");
    assert_eq!(batch.coverage, HeaderCoverage::Complete);
    assert_eq!(
        batch.rows,
        vec![
            HeaderRow {
                uid: nz(5),
                flags: Vec::new(),
                internal_date_ms: DATE_MS,
                rfc822_size: 4242,
                headers: HeaderBytes::Full(b"Subject: one\r\n".to_vec()),
            },
            HeaderRow {
                uid: nz(6),
                flags: vec!["\\Draft".to_string()],
                internal_date_ms: DATE_NEG_MS,
                rfc822_size: 4242,
                headers: HeaderBytes::Full(b"Subject: two\r\n".to_vec()),
            },
        ]
    );
}

#[test]
fn nil_is_not_real_empty_header_bytes() {
    let req = request(&[5], &["SUBJECT"], HeaderFetchMode::Full);
    let meta = [uid(5), flags("\\Seen"), date(), size(0)];

    // NIL means no literal was observed. The row keeps its metadata
    // and its UID is not reported missing, but the batch is incomplete.
    let mut items = meta.to_vec();
    items.push(body_nil("BODY[HEADER.FIELDS (SUBJECT)]"));
    let driven = observe(&req, &[fetch_frame(1, &items)], false);
    assert!(driven.pushes[0].is_ok());
    let batch = driven.finish.expect("a NIL row is not a failure");
    assert_eq!(
        batch.rows,
        vec![HeaderRow {
            uid: nz(5),
            flags: vec!["\\Seen".to_string()],
            internal_date_ms: DATE_MS,
            rfc822_size: 0,
            headers: HeaderBytes::Unavailable,
        }]
    );
    assert!(batch.missing_uids.is_empty(), "NIL rows are not missing");
    assert_eq!(batch.coverage, HeaderCoverage::Incomplete);

    // A real empty section, quoted or a zero-length literal, is
    // complete evidence and distinct from NIL.
    for empty in [
        "BODY[HEADER.FIELDS (SUBJECT)] \"\"".to_string(),
        body("BODY[HEADER.FIELDS (SUBJECT)]", ""),
    ] {
        let mut items = meta.to_vec();
        items.push(empty);
        let driven = observe(&req, &[fetch_frame(1, &items)], false);
        let batch = driven.finish.expect("real empty bytes are not a failure");
        assert_eq!(
            batch.rows[0].headers,
            HeaderBytes::Full(Vec::new()),
            "real empty section"
        );
        assert_eq!(batch.coverage, HeaderCoverage::Complete);
    }
    assert_ne!(HeaderBytes::Unavailable, HeaderBytes::Full(Vec::new()));
}

#[test]
fn prefix_evidence_stays_partial_even_when_short() {
    let req = request(
        &[5],
        &["SUBJECT"],
        HeaderFetchMode::Prefix { max_bytes: nz(4) },
    );
    let frame = fetch_frame(
        1,
        &[
            uid(5),
            flags("\\Seen"),
            date(),
            size(3),
            body("BODY[HEADER.FIELDS (SUBJECT)]<0>", "Sub"),
        ],
    );
    let driven = observe(&req, &[frame], false);
    assert!(driven.pushes[0].is_ok());
    assert!(
        !driven.ambiguous,
        "a valid first prefix is not a change hint"
    );
    let batch = driven.finish.expect("valid first prefix");
    // Three octets under a four-octet limit prove nothing about the
    // rest of the section: the row stays a first prefix and the batch
    // stays incomplete even without any ambiguity.
    assert_eq!(
        batch.rows,
        vec![HeaderRow {
            uid: nz(5),
            flags: vec!["\\Seen".to_string()],
            internal_date_ms: DATE_MS,
            rfc822_size: 3,
            headers: HeaderBytes::Prefix {
                bytes: b"Sub".to_vec(),
                limit: nz(4),
            },
        }]
    );
    assert!(batch.missing_uids.is_empty());
    assert_eq!(batch.coverage, HeaderCoverage::Incomplete);

    // A prefix filling its limit exactly is still a prefix.
    let exact = fetch_frame(
        1,
        &[
            uid(5),
            flags("\\Seen"),
            date(),
            size(4),
            body("BODY[HEADER.FIELDS (SUBJECT)]<0>", "Subj"),
        ],
    );
    let driven = observe(&req, &[exact], false);
    let batch = driven.finish.expect("valid full-length prefix");
    assert_eq!(
        batch.rows[0].headers,
        HeaderBytes::Prefix {
            bytes: b"Subj".to_vec(),
            limit: nz(4),
        }
    );
    assert_eq!(batch.coverage, HeaderCoverage::Incomplete);
}

#[test]
fn large_headers_have_no_lower_raw_cap() {
    let req = request(&[5], &["SUBJECT"], HeaderFetchMode::Full);
    // A ~300 KiB raw section, past any lower 256 KiB cap, beside a
    // high size hint that imposes no body budget on this metadata row.
    let text = format!("X-Synthetic: {}\r\n", "a".repeat(300 * 1024));
    let frame = fetch_frame(
        1,
        &[
            uid(5),
            flags("\\Seen"),
            date(),
            size(u32::MAX),
            body("BODY[HEADER.FIELDS (SUBJECT)]", &text),
        ],
    );
    let driven = observe(&req, &[frame], false);
    assert!(driven.pushes[0].is_ok());
    assert!(!driven.ambiguous);
    let batch = driven.finish.expect("a ~300 KiB header is no failure");
    assert_eq!(
        batch.rows[0].headers,
        HeaderBytes::Full(text.as_bytes().to_vec()),
        "raw bytes preserved past 256 KiB"
    );
    assert_eq!(batch.rows[0].rfc822_size, u32::MAX);
    assert_eq!(batch.coverage, HeaderCoverage::Complete);
}

#[test]
fn missing_unkeyed_and_unrelated_frames_yield_no_row() {
    let req = request(&[5], &["SUBJECT"], HeaderFetchMode::Full);
    let echo = "BODY[HEADER.FIELDS (SUBJECT)]";
    let full = [uid(5), flags("\\Seen"), date(), size(3), body(echo, "Sub")];
    // One frame per required attribute, each missing exactly it.
    for (skip, label) in ["UID", "FLAGS", "INTERNALDATE", "RFC822.SIZE", "BODY"]
        .iter()
        .enumerate()
    {
        let items: Vec<String> = full
            .iter()
            .enumerate()
            .filter(|(at, _)| *at != skip)
            .map(|(_, item)| item.clone())
            .collect();
        let driven = observe(&req, &[fetch_frame(1, &items)], false);
        assert!(driven.pushes[0].is_ok(), "{label} missing is legal");
        assert!(driven.ambiguous, "{label} missing hides updates");
        let batch = driven.finish.expect("incomplete is not a failure");
        assert!(
            batch.rows.is_empty(),
            "{label} missing yields no default row"
        );
        assert_eq!(batch.missing_uids, uids_of(&[5]), "{label}");
        assert_eq!(batch.coverage, HeaderCoverage::Incomplete, "{label}");
    }

    // A complete row for a UID outside the request gets no row and is
    // not missing, but it can still hide an update.
    let unrelated = fetch_frame(
        1,
        &[uid(6), flags("\\Seen"), date(), size(3), body(echo, "Sub")],
    );
    let driven = observe(&req, &[unrelated], false);
    assert!(driven.pushes[0].is_ok());
    assert!(driven.ambiguous);
    let batch = driven.finish.expect("unrelated frames are not failures");
    assert!(batch.rows.is_empty(), "unrequested UIDs never get rows");
    assert_eq!(batch.missing_uids, uids_of(&[5]));
    assert_eq!(batch.coverage, HeaderCoverage::Incomplete);
}

#[test]
fn rows_are_never_stitched_across_frames() {
    let req = request(&[5], &["SUBJECT"], HeaderFetchMode::Full);
    // Together the two frames carry every required attribute, but
    // never in one frame: evidence is taken whole or not at all.
    let frames = [
        fetch_frame(1, &[uid(5), flags("\\Seen"), date(), size(16)]),
        fetch_frame(2, &[uid(5), body("BODY[HEADER.FIELDS (SUBJECT)]", TEXT)]),
    ];
    let driven = observe(&req, &frames, false);
    assert!(driven.pushes.iter().all(Result::is_ok));
    assert!(driven.ambiguous);
    let batch = driven.finish.expect("incomplete is not a failure");
    assert!(batch.rows.is_empty(), "no partial rows survive");
    assert_eq!(batch.missing_uids, uids_of(&[5]));
    assert_eq!(batch.coverage, HeaderCoverage::Incomplete);
}

#[test]
fn repeated_uids_keep_the_latest_whole_or_drop_the_row() {
    let req = request(&[5], &["SUBJECT"], HeaderFetchMode::Full);
    let echo = "BODY[HEADER.FIELDS (SUBJECT)]";
    let first = fetch_frame(
        1,
        &[uid(5), flags("\\Seen"), date(), size(3), body(echo, "one")],
    );
    let second = fetch_frame(
        2,
        &[uid(5), flags("\\Draft"), date(), size(3), body(echo, "two")],
    );
    let driven = observe(&req, &[first, second], false);
    assert!(driven.pushes.iter().all(Result::is_ok));
    assert!(driven.ambiguous, "a repeat can hide an update");
    let batch = driven.finish.expect("repeats are not failures");
    assert_eq!(
        batch.rows,
        vec![HeaderRow {
            uid: nz(5),
            flags: vec!["\\Draft".to_string()],
            internal_date_ms: DATE_MS,
            rfc822_size: 3,
            headers: HeaderBytes::Full(b"two".to_vec()),
        }],
        "the latest whole frame wins"
    );
    assert_eq!(
        batch.coverage,
        HeaderCoverage::Incomplete,
        "a repeat is never complete"
    );

    // A later incomplete frame for the same UID is an update: the
    // older row must go rather than survive as stitched evidence.
    let stale = fetch_frame(
        1,
        &[uid(5), flags("\\Seen"), date(), size(3), body(echo, "one")],
    );
    let update = fetch_frame(2, &[uid(5), flags("\\Draft"), date(), size(3)]);
    let driven = observe(&req, &[stale, update], false);
    assert!(driven.pushes.iter().all(Result::is_ok));
    let batch = driven.finish.expect("incomplete is not a failure");
    assert!(batch.rows.is_empty(), "the previous row is removed");
    assert_eq!(batch.missing_uids, uids_of(&[5]));
    assert_eq!(batch.coverage, HeaderCoverage::Incomplete);

    // A mailbox-change hint alone already blocks complete coverage.
    let single = fetch_frame(
        1,
        &[uid(5), flags("\\Seen"), date(), size(3), body(echo, "one")],
    );
    let driven = observe(&req, &[single], true);
    assert!(!driven.ambiguous);
    let batch = driven.finish.expect("a change hint is not a failure");
    assert_eq!(batch.rows.len(), 1);
    assert_eq!(batch.coverage, HeaderCoverage::Incomplete);
}

#[test]
fn duplicate_attributes_fail_the_push_and_the_finish() {
    let req = request(&[5], &["SUBJECT"], HeaderFetchMode::Full);
    let echo = "BODY[HEADER.FIELDS (SUBJECT)]";
    let good = fetch_frame(
        1,
        &[uid(5), flags("\\Seen"), date(), size(3), body(echo, "Sub")],
    );
    let duplicates = [
        (
            "UID",
            fetch_frame(
                1,
                &[
                    uid(5),
                    uid(6),
                    flags("\\Seen"),
                    date(),
                    size(3),
                    body(echo, "Sub"),
                ],
            ),
        ),
        (
            "FLAGS",
            fetch_frame(
                1,
                &[
                    uid(5),
                    flags("\\Seen"),
                    flags("\\Draft"),
                    date(),
                    size(3),
                    body(echo, "Sub"),
                ],
            ),
        ),
        (
            "INTERNALDATE",
            fetch_frame(
                1,
                &[
                    uid(5),
                    flags("\\Seen"),
                    date(),
                    date(),
                    size(3),
                    body(echo, "Sub"),
                ],
            ),
        ),
        (
            "RFC822.SIZE",
            fetch_frame(
                1,
                &[
                    uid(5),
                    flags("\\Seen"),
                    date(),
                    size(3),
                    size(4),
                    body(echo, "Sub"),
                ],
            ),
        ),
        (
            "BODY",
            fetch_frame(
                1,
                &[
                    uid(5),
                    flags("\\Seen"),
                    date(),
                    size(3),
                    body(echo, "Sub"),
                    body(echo, "Sub"),
                ],
            ),
        ),
    ];
    for (label, frame) in duplicates {
        let driven = observe(&req, &[good.clone(), frame], false);
        assert!(driven.pushes[0].is_ok());
        assert!(driven.pushes[1].is_err(), "{label} duplicated");
        // A caller may ignore the push error; the finish must still
        // refuse, so no partial observation escapes the exchange.
        assert!(driven.finish.is_err(), "{label} duplicated");
    }
}

#[test]
fn wrong_echoes_fail_the_push_and_the_finish() {
    let req = request(&[5], &["SUBJECT"], HeaderFetchMode::Full);
    let head = [uid(5), flags("\\Seen"), date(), size(3)];
    let wrong = [
        (
            "duplicate field echo",
            body("BODY[HEADER.FIELDS (SUBJECT subject)]", "Sub"),
        ),
        ("full header section", body("BODY[HEADER]", "Sub")),
        ("text section", body("BODY[TEXT]", "Sub")),
        (
            "part prefix",
            body("BODY[1.HEADER.FIELDS (SUBJECT)]", "Sub"),
        ),
        (
            "excluded fields",
            body("BODY[HEADER.FIELDS.NOT (SUBJECT)]", "Sub"),
        ),
        (
            "foreign field list",
            body("BODY[HEADER.FIELDS (FROM)]", "Sub"),
        ),
        (
            "widened field list",
            body("BODY[HEADER.FIELDS (SUBJECT FROM)]", "Sub"),
        ),
        (
            "origin on a full read",
            body("BODY[HEADER.FIELDS (SUBJECT)]<0>", "Sub"),
        ),
        (
            "nonzero origin",
            body("BODY[HEADER.FIELDS (SUBJECT)]<2>", "Sub"),
        ),
    ];
    for (label, item) in wrong {
        let mut items = head.to_vec();
        items.push(item);
        let driven = observe(&req, &[fetch_frame(1, &items)], false);
        assert!(driven.pushes[0].is_err(), "{label}");
        assert!(driven.finish.is_err(), "{label}");
    }

    let prefix = request(
        &[5],
        &["SUBJECT"],
        HeaderFetchMode::Prefix { max_bytes: nz(4) },
    );
    let wrong_prefix = [
        (
            "prefix over its limit",
            body("BODY[HEADER.FIELDS (SUBJECT)]<0>", "abcde"),
        ),
        (
            "prefix from another origin",
            body("BODY[HEADER.FIELDS (SUBJECT)]<1>", "ab"),
        ),
        // An origin-less echo claims a whole section the request
        // never made, so it is no evidence of a first prefix.
        (
            "prefix echo without origin",
            body("BODY[HEADER.FIELDS (SUBJECT)]", "ab"),
        ),
    ];
    for (label, item) in wrong_prefix {
        let mut items = head.to_vec();
        items.push(item);
        let driven = observe(&prefix, &[fetch_frame(1, &items)], false);
        assert!(driven.pushes[0].is_err(), "{label}");
        assert!(driven.finish.is_err(), "{label}");
    }
}

#[test]
fn debug_reports_lengths_not_header_bytes() {
    let req = request(&[5], &["SUBJECT"], HeaderFetchMode::Full);
    let frame = fetch_frame(
        1,
        &[
            uid(5),
            flags("\\Seen"),
            date(),
            size(32),
            body(
                "BODY[HEADER.FIELDS (SUBJECT)]",
                "To: secret-recipient@example.invalid\r\n",
            ),
        ],
    );
    let driven = observe(&req, &[frame], false);
    let batch = driven.finish.expect("one complete row");
    let debug = format!("{batch:?}");
    assert!(!debug.contains("secret-recipient"));
    assert!(!debug.contains("example.invalid"));
    let headers = format!("{:?}", batch.rows[0].headers);
    assert!(headers.contains("len"), "lengths are reported");
    assert!(!headers.contains("example.invalid"));
}
