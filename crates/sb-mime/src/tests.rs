//! Tests for `sb_mime::parse`: the synthetic `fixtures/eml` corpus with
//! insta snapshots, plus generated inputs for every spec/10 boundary.
//! Giant inputs are generated in memory; fixtures stay reviewable.

use std::path::Path;

use crate::{
    Limits, MAX_DECODED_PART_BYTES, MAX_HEADER_BYTES, MAX_HEADER_FIELDS, MAX_MESSAGE_BYTES,
    MAX_PARTS, ParseFailure, ParseOutcome, ParseWarning, SNIPPET_BYTES, parse,
};

/// Header-line marker that trips `crate::tripwire` in test builds.
pub(crate) const TRIPWIRE_MARKER: &[u8] = b"X-SbMime-Tripwire: panic";

fn fixture(name: &str) -> Vec<u8> {
    let path = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../fixtures/eml")
        .join(name);
    std::fs::read(&path).unwrap_or_else(|e| panic!("read {path:?}: {e}"))
}

fn assert_parsed(bytes: &[u8]) -> crate::MessageSummary {
    match parse(bytes, &Limits::default()) {
        ParseOutcome::Parsed(s) => s,
        other => panic!("expected Parsed, got {other:?}"),
    }
}

fn assert_partial(bytes: &[u8]) -> (crate::MessageSummary, Vec<ParseWarning>) {
    match parse(bytes, &Limits::default()) {
        ParseOutcome::Partial { summary, warnings } => (summary, warnings),
        other => panic!("expected Partial, got {other:?}"),
    }
}

// ---- generated-input builders ----

/// `levels` nested `multipart/mixed` wrappers around a text/plain leaf.
/// The leaf ends up at depth `levels + 1` (root entity = 1).
fn nested_multipart(levels: usize) -> Vec<u8> {
    let mut entity = b"Content-Type: text/plain\r\n\r\nleaf\r\n".to_vec();
    for i in (0..levels).rev() {
        let mut wrapped =
            format!("Content-Type: multipart/mixed; boundary=b{i}\r\n\r\n--b{i}\r\n").into_bytes();
        wrapped.extend_from_slice(&entity);
        wrapped.extend_from_slice(format!("\r\n--b{i}--\r\n").as_bytes());
        entity = wrapped;
    }
    let mut msg = b"Subject: nested\r\n".to_vec();
    msg.extend_from_slice(&entity);
    msg
}

/// One multipart with `leaves` text/plain siblings: total entities =
/// `leaves + 1` (the container counts too).
fn many_parts(leaves: usize) -> Vec<u8> {
    let mut msg = b"Subject: many\r\nContent-Type: multipart/mixed; boundary=bb\r\n\r\n".to_vec();
    for _ in 0..leaves {
        msg.extend_from_slice(b"--bb\r\nContent-Type: text/plain\r\n\r\nx\r\n");
    }
    msg.extend_from_slice(b"--bb--\r\n");
    msg
}

/// Base64-encode (line-folded at 76 cols like a real MUA would emit).
fn b64(bytes: &[u8]) -> Vec<u8> {
    const A: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789+/";
    let mut out = Vec::with_capacity(bytes.len() / 3 * 4 + 64);
    let mut col = 0;
    for chunk in bytes.chunks(3) {
        let (b0, b1, b2) = (
            chunk[0] as u32,
            *chunk.get(1).unwrap_or(&0) as u32,
            *chunk.get(2).unwrap_or(&0) as u32,
        );
        let n = (b0 << 16) | (b1 << 8) | b2;
        let chars = [
            A[(n >> 18) as usize & 63],
            A[(n >> 12) as usize & 63],
            if chunk.len() > 1 {
                A[(n >> 6) as usize & 63]
            } else {
                b'='
            },
            if chunk.len() > 2 {
                A[n as usize & 63]
            } else {
                b'='
            },
        ];
        out.extend_from_slice(&chars);
        col += 4;
        if col == 76 {
            out.extend_from_slice(b"\r\n");
            col = 0;
        }
    }
    if col != 0 {
        out.extend_from_slice(b"\r\n");
    }
    out
}

/// `levels` of base64-encoded `message/rfc822` nesting: each level is an
/// entity whose decoded body is the next message.
fn encoded_nested(levels: usize) -> Vec<u8> {
    let mut inner = b"Subject: innermost\r\nContent-Type: text/plain\r\n\r\nleaf\r\n".to_vec();
    for _ in 0..levels {
        let mut e =
            b"Content-Type: message/rfc822\r\nContent-Transfer-Encoding: base64\r\n\r\n".to_vec();
        e.extend_from_slice(&b64(&inner));
        inner = e;
    }
    let mut msg = b"Subject: encoded-nest\r\n".to_vec();
    msg.extend_from_slice(&inner);
    msg
}

// ---- fixture corpus (insta snapshots) ----

#[test]
fn fixture_corpus() {
    let dir = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/eml");
    let mut names: Vec<_> = std::fs::read_dir(&dir)
        .unwrap_or_else(|e| panic!("read_dir {dir:?}: {e}"))
        .map(|e| e.unwrap().file_name().into_string().unwrap())
        .filter(|n| n.ends_with(".eml"))
        .collect();
    names.sort();
    assert!(!names.is_empty(), "no fixtures in {dir:?}");
    for name in names {
        let bytes = fixture(&name);
        let outcome = parse(&bytes, &Limits::default());
        insta::assert_debug_snapshot!(name.trim_end_matches(".eml"), outcome);
    }
}

// ---- required summary fields ----

#[test]
fn extracts_all_summary_fields() {
    let s = assert_parsed(&fixture("headers.eml"));
    // Upstream strips the <...> wrappers from id fields.
    assert_eq!(s.message_id(), Some("id-1@example.com"));
    assert_eq!(s.subject(), Some("See attachment"));
    let from = s.from().unwrap();
    assert_eq!(from.name(), Some("Alice Sender"));
    assert_eq!(from.address(), Some("alice@example.com"));
    assert_eq!(s.to().len(), 2);
    assert_eq!(s.cc().len(), 1);
    assert_eq!(s.reply_to().len(), 1);
    assert_eq!(s.in_reply_to(), Some("id-0@example.com"));
    assert_eq!(s.references().len(), 2);
    assert_eq!(s.list_id(), Some("sweetbriar.example.com"));
    assert_eq!(
        s.delivered_to(),
        &["owner@example.com", "alias@example.com"]
    );
    assert!(s.date().is_some());
    assert!(s.snippet().is_some());
}

// ---- spec/10 boundaries (generated inputs) ----

#[test]
fn depth_at_and_over_limit() {
    // leaf at depth 50: 49 multipart wrappers — admitted.
    let ok = nested_multipart(49);
    assert!(matches!(
        parse(&ok, &Limits::default()),
        ParseOutcome::Parsed(_)
    ));
    // leaf at depth 51: Partial + TooDeep.
    let over = nested_multipart(50);
    let (_, warnings) = assert_partial(&over);
    assert!(
        warnings
            .iter()
            .any(|w| matches!(w, ParseWarning::TooDeep { limit: 50 })),
        "{warnings:?}"
    );
}

#[test]
fn parts_at_and_over_limit() {
    // 9,999 leaves + container = 10,000 parts: admitted.
    let ok = many_parts(MAX_PARTS as usize - 1);
    assert!(matches!(
        parse(&ok, &Limits::default()),
        ParseOutcome::Parsed(_)
    ));
    // One more leaf: Partial + TooManyParts.
    let over = many_parts(MAX_PARTS as usize);
    let (_, warnings) = assert_partial(&over);
    assert!(
        warnings
            .iter()
            .any(|w| matches!(w, ParseWarning::TooManyParts { limit } if *limit == MAX_PARTS)),
        "{warnings:?}"
    );
}

#[test]
fn message_bytes_ceiling() {
    // Cheap path: a lowered budget hits the same check.
    let limits = Limits::default().with_max_message_bytes(64);
    let over = fixture("headers.eml");
    match parse(&over, &limits) {
        ParseOutcome::Partial { warnings, .. } => assert!(
            warnings
                .iter()
                .any(|w| matches!(w, ParseWarning::MessageTooLarge { limit: 64 })),
            "{warnings:?}"
        ),
        other => panic!("expected Partial, got {other:?}"),
    }
    // Real ceiling: 100,000,001 raw bytes.
    let mut big = b"Subject: big\r\n\r\n".to_vec();
    big.extend(std::iter::repeat_n(b'x', MAX_MESSAGE_BYTES));
    let (_, warnings) = assert_partial(&big);
    assert!(
        warnings
            .iter()
            .any(|w| matches!(w, ParseWarning::MessageTooLarge { limit } if *limit == MAX_MESSAGE_BYTES as u64)),
        "{warnings:?}"
    );
}

#[test]
fn decoded_part_ceiling() {
    // 26 MB of text in one part: over the 25,000,000 decoded cap, under
    // the 100 MB message cap. Bound check fires before materialization.
    let mut big = b"Subject: big part\r\nContent-Type: text/plain\r\n\r\n".to_vec();
    big.extend(std::iter::repeat_n(
        b'x',
        MAX_DECODED_PART_BYTES as usize + 1,
    ));
    let (_, warnings) = assert_partial(&big);
    assert!(
        warnings
            .iter()
            .any(|w| matches!(w, ParseWarning::PartTooLarge { limit } if *limit == MAX_DECODED_PART_BYTES)),
        "{warnings:?}"
    );
}

#[test]
fn encoded_part_bound_before_decode() {
    // Base64 of a 40 MB blob decodes to ~30 MB > cap: must trip the
    // bound from the *encoded* extent, without materializing it.
    let raw = vec![b'x'; 40_000_000];
    let encoded = b64(&raw);
    let mut msg =
        b"Subject: big b64\r\nContent-Type: application/octet-stream\r\nContent-Transfer-Encoding: base64\r\n\r\n"
            .to_vec();
    msg.extend_from_slice(&encoded);
    let (_, warnings) = assert_partial(&msg);
    assert!(
        warnings
            .iter()
            .any(|w| matches!(w, ParseWarning::PartTooLarge { .. })),
        "{warnings:?}"
    );
    // And a lowered budget does it cheaply.
    let small = Limits::default().with_max_decoded_part_bytes(64);
    let mut over = b"Content-Type: text/plain\r\n\r\n".to_vec();
    over.extend(std::iter::repeat_n(b'x', 100));
    let (_, warnings) = match parse(&over, &small) {
        ParseOutcome::Partial { summary, warnings } => (summary, warnings),
        other => panic!("expected Partial, got {other:?}"),
    };
    assert!(
        warnings
            .iter()
            .any(|w| matches!(w, ParseWarning::PartTooLarge { limit: 64 })),
        "{warnings:?}"
    );
}

#[test]
fn header_byte_and_count_ceilings() {
    // A single header field body over the byte budget.
    let mut msg = b"Subject: ".to_vec();
    msg.extend(std::iter::repeat_n(b'x', MAX_HEADER_BYTES));
    msg.extend_from_slice(b"\r\n\r\nbody");
    let (_, warnings) = assert_partial(&msg);
    assert!(
        warnings.iter().any(
            |w| matches!(w, ParseWarning::HeaderBlockTooLarge { limit } if *limit == MAX_HEADER_BYTES as u64)
        ),
        "{warnings:?}"
    );

    // MAX_HEADER_FIELDS + 1 fields.
    let mut msg = Vec::new();
    for i in 0..=MAX_HEADER_FIELDS {
        msg.extend_from_slice(format!("X-Fill-{i}: v\r\n").as_bytes());
    }
    msg.extend_from_slice(b"\r\nbody");
    let (_, warnings) = assert_partial(&msg);
    assert!(
        warnings.iter().any(
            |w| matches!(w, ParseWarning::TooManyHeaderFields { limit } if *limit == MAX_HEADER_FIELDS as u64)
        ),
        "{warnings:?}"
    );
}

#[test]
fn encoded_nesting_bound() {
    // 3 encoded rfc822 levels decode fine.
    let ok = encoded_nested(3);
    assert!(matches!(
        parse(&ok, &Limits::default()),
        ParseOutcome::Parsed(_)
    ));
    // The 4th stays an opaque attachment: NestedTooDeep + upstream's
    // own encoding flag → Malformed.
    let over = encoded_nested(4);
    let (_, warnings) = assert_partial(&over);
    assert!(
        warnings
            .iter()
            .any(|w| matches!(w, ParseWarning::NestedTooDeep { .. })),
        "{warnings:?}"
    );
}

#[test]
fn nested_unencoded_rfc822_parsed() {
    let s = assert_parsed(&fixture("rfc822-forward.eml"));
    assert!(s.has_attachments());
}

// ---- malformed / hostile recovery ----

#[test]
fn empty_fails_garbage_is_lenient() {
    // Nothing parseable at all → Failed(Unparseable).
    match parse(b"", &Limits::default()) {
        ParseOutcome::Failed(ParseFailure::Unparseable) => (),
        other => panic!("expected Failed(Unparseable), got {other:?}"),
    }
    // Upstream is best-effort: colon-less lines drop to a body-only
    // message rather than failing — Parsed with an empty summary.
    match parse(
        b"not a message\r\nno colon anywhere\r\n\r\nbody",
        &Limits::default(),
    ) {
        ParseOutcome::Parsed(s) => assert_eq!(s.snippet(), Some("body")),
        other => panic!("expected Parsed, got {other:?}"),
    }
}

#[test]
fn malformed_is_partial() {
    // Declared boundary never occurs: recovered as text, Malformed
    // warning, Partial.
    let (_, warnings) = assert_partial(&fixture("malformed-structure.eml"));
    assert!(
        warnings
            .iter()
            .any(|w| matches!(w, ParseWarning::Malformed { .. })),
        "{warnings:?}"
    );
    // Bad base64 in a part: upstream flags an encoding problem.
    let (_, warnings) = assert_partial(&fixture("malformed-encoding.eml"));
    assert!(
        warnings
            .iter()
            .any(|w| matches!(w, ParseWarning::Malformed { .. })),
        "{warnings:?}"
    );
}

// ---- snippet ----

#[test]
fn snippet_is_bounded_and_unicode_safe() {
    // 600 copies of a 4-byte emoji well past the 512-byte budget.
    let mut msg = b"Content-Type: text/plain; charset=utf-8\r\n\r\n".to_vec();
    for _ in 0..600 {
        msg.extend_from_slice("é😀".as_bytes());
    }
    let s = assert_parsed(&msg);
    let snippet = s.snippet().expect("snippet");
    assert!(snippet.len() <= SNIPPET_BYTES, "len {}", snippet.len());
    assert!(snippet.ends_with("..."));
}

// ---- panic containment ----

#[test]
fn panic_inside_boundary_is_contained() {
    // The marker header trips `crate::tripwire` mid-walk, inside the
    // `catch_unwind` boundary — the actual containment path.
    let mut msg = TRIPWIRE_MARKER.to_vec();
    msg.extend_from_slice(b"\r\n");
    msg.extend_from_slice(&fixture("headers.eml"));
    match parse(&msg, &Limits::default()) {
        ParseOutcome::Failed(ParseFailure::Panic) => (),
        other => panic!("expected Failed(Panic), got {other:?}"),
    }
    // The parser still works afterwards (no poisoned state).
    assert_parsed(&fixture("headers.eml"));
}

// ---- upstream regressions ----

#[test]
fn corrupted_nested_rfc822_does_not_panic() {
    // mail-parser 0.11.9 trips `debug_assert!("Invalid part ID")` on
    // this fuzz-minimized input (truncated nested message/rfc822
    // headers); its release build recovers instead. sb-mime compiles
    // the dep to release semantics in every profile so contained
    // upstream panics stay fuzz findings rather than appearing only
    // in assert-enabled builds — see Cargo.toml `[profile.*.package]`.
    let outcome = parse(&fixture("fuzz-rfc822-partid.eml"), &Limits::default());
    assert!(
        !matches!(outcome, ParseOutcome::Failed(ParseFailure::Panic)),
        "upstream panic leaked into outcome: {outcome:?}"
    );
}

// ---- limits can't be raised ----

#[test]
fn ceilings_cannot_be_raised() {
    let limits = Limits::default()
        .with_max_depth(u32::MAX)
        .with_max_parts(u64::MAX)
        .with_max_message_bytes(usize::MAX)
        .with_max_decoded_part_bytes(u64::MAX)
        .with_max_header_bytes(usize::MAX)
        .with_max_header_fields(usize::MAX);
    assert_eq!(limits.max_depth(), crate::MAX_DEPTH);
    assert_eq!(limits.max_parts(), MAX_PARTS);
    assert_eq!(limits.max_message_bytes(), MAX_MESSAGE_BYTES);
    assert_eq!(limits.max_decoded_part_bytes(), MAX_DECODED_PART_BYTES);
    assert_eq!(limits.max_header_bytes(), MAX_HEADER_BYTES);
    assert_eq!(limits.max_header_fields(), MAX_HEADER_FIELDS);
}
