//! Fixtures for [`super::reconstruct`]: every body shape spec/04 names,
//! hostile names and types, header replacement, and the determinism
//! proptest. Each fixture parses back through [`crate::parse`] and
//! [`crate::render_input`] to the same structure it was built from.

use proptest::prelude::*;

use super::*;
use crate::{Limits, ParseOutcome, render_input};

const HEADER: &[u8] = b"Subject: synthetic subject msg-1\r\n\
From: Sender <sender@example.test>\r\n\
To: To <to@example.test>\r\n\
Date: Tue, 14 Nov 2023 22:26:40 +0000\r\n\
Message-ID: <msg-1@example.test>\r\n";

/// Builds, parses and summarizes one reconstruction.
fn build(
    id: &str,
    header: &[u8],
    mime_type: &str,
    body: &[u8],
    attachments: &[ReconstructPart<'_>],
) -> (Vec<u8>, crate::RenderInput, crate::MessageSummary) {
    let out = reconstruct(
        &ReconstructInput {
            message_id: id,
            header,
            mime_type,
            body,
            attachments,
        },
        &Limits::default(),
    )
    .expect("it reconstructs");
    let parsed = render_input(&out, &Limits::default()).expect("it admits");
    let summary = match crate::parse(&out, &Limits::default()) {
        ParseOutcome::Parsed(summary) => summary,
        other => panic!("expected Parsed, got {other:?}"),
    };
    (out, parsed, summary)
}

/// The header block as text, without the blank line that ends it.
fn header_text(out: &[u8]) -> String {
    let end = out
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("a blank line ends the header block");
    String::from_utf8_lossy(&out[..end]).into_owned()
}

/// How many lines of the header block carry `name`.
fn field_count(out: &[u8], name: &str) -> usize {
    header_text(out)
        .lines()
        .filter(|line| {
            line.split_once(':')
                .is_some_and(|(key, _)| key.eq_ignore_ascii_case(name))
        })
        .count()
}

/// An attachment part with explicit metadata.
fn part<'a>(
    name: &'a str,
    mime_type: &'a str,
    disposition: Disposition,
    content_id: Option<&'a str>,
    data: &'a [u8],
) -> ReconstructPart<'a> {
    ReconstructPart {
        name,
        mime_type,
        disposition,
        content_id,
        data,
    }
}

fn expanding_input<'a>(id: &'a str, mime_type: &'a str) -> ReconstructInput<'a> {
    ReconstructInput {
        message_id: id,
        header: HEADER,
        mime_type,
        body: b"a body that expands",
        attachments: &[],
    }
}

fn bytes_of(parsed: &crate::RenderInput, output: &[u8], index: usize) -> Vec<u8> {
    let attachment = parsed.attachments().get(index).expect("the attachment");
    crate::extract_attachment(output, attachment.part_id().as_str(), &Limits::default())
        .expect("it decodes")
}

fn names(parsed: &crate::RenderInput) -> Vec<String> {
    parsed
        .attachments()
        .iter()
        .map(|attachment| attachment.display_name().to_owned())
        .collect()
}

#[test]
fn text_and_multipart_bodies_parse_back() {
    let (out, parsed, summary) = build("msg-1", HEADER, "text/plain", b"hello\r\nworld", &[]);
    let header = header_text(&out);
    assert!(header.contains("Content-Type: text/plain; charset=utf-8"));
    assert!(header.contains("Content-Transfer-Encoding: quoted-printable"));
    assert!(header.contains("X-Pm-Internal-Id: msg-1"));
    assert_eq!(summary.subject(), Some("synthetic subject msg-1"));
    assert_eq!(
        summary.from().and_then(|from| from.address()),
        Some("sender@example.test")
    );
    assert_eq!(parsed.plain().expect("plain").text(), "hello\nworld");

    let (out, parsed, _) = build("msg-1", HEADER, "text/html", b"<p>hello</p>", &[]);
    assert!(header_text(&out).contains("Content-Type: text/html; charset=utf-8"));
    assert_eq!(parsed.html().expect("html").text(), "<p>hello</p>");

    let signed = b"Content-Type: multipart/signed; protocol=\"application/pgp-signature\"; \
boundary=\"sig\"\r\n\r\n--sig\r\nContent-Type: text/plain\r\n\r\nsigned text\r\n--sig\r\n\
Content-Type: application/pgp-signature\r\n\r\nsignature bytes\r\n--sig--\r\n";
    let (out, parsed, _) = build("msg-1", HEADER, "multipart/signed", signed, &[]);
    assert!(header_text(&out).contains(
        "Content-Type: multipart/signed; protocol=\"application/pgp-signature\"; boundary=\"sig\""
    ));
    assert_eq!(field_count(&out, "Content-Type"), 1);
    assert_eq!(parsed.plain().expect("signed text").text(), "signed text");
}

#[test]
fn related_and_mixed_attachments_parse_back() {
    let image = [0x89, 0x50, 0x4e, 0x47, 0x0d, 0x0a, 0x1a, 0x0a];
    let html = b"<img src=\"cid:logo@example.test\">";
    let cid = part(
        "logo.png",
        "image/png",
        Disposition::Inline,
        Some("logo@example.test"),
        &image,
    );
    let (out, parsed, _) = build(
        "msg-1",
        HEADER,
        "text/html",
        html,
        std::slice::from_ref(&cid),
    );
    assert!(header_text(&out).contains("multipart/related"));
    assert_eq!(
        parsed.html().expect("html").text(),
        std::str::from_utf8(html).unwrap()
    );
    assert_eq!(parsed.resources().len(), 1);
    assert_eq!(parsed.resources()[0].content_id(), "logo@example.test");
    assert_eq!(parsed.resources()[0].media_type(), "image/png");
    assert_eq!(parsed.resources()[0].bytes(), image);
    assert_eq!(bytes_of(&parsed, &out, 0), image);

    // An inline part without a Content-ID and an attachment follow in
    // multipart/mixed.
    let first = b"first".as_slice();
    let document = b"pdf".as_slice();
    let parts = [
        part("first.txt", "text/plain", Disposition::Inline, None, first),
        part(
            "manual.pdf",
            "text/plain",
            Disposition::Attachment,
            None,
            document,
        ),
    ];
    let (out, parsed, _) = build("msg-1", HEADER, "text/plain", b"see below", &parts);
    assert!(header_text(&out).contains("multipart/mixed"));
    assert_eq!(parsed.plain().expect("plain").text(), "see below");
    assert_eq!(names(&parsed), ["first.txt", "manual.pdf"]);
    assert_eq!(bytes_of(&parsed, &out, 0), first);
    assert_eq!(bytes_of(&parsed, &out, 1), document);
}

#[test]
fn non_utf8_bodies_keep_or_drop_the_charset() {
    let header = b"Subject: caf\xe9\r\nContent-Type: text/plain; charset=iso-8859-1\r\n";
    let (out, parsed, _) = build("msg-1", header, "text/plain", b"caf\xe9", &[]);
    let header = header_text(&out);
    assert!(header.contains("Content-Type: text/plain; charset=iso-8859-1"));
    assert!(header.contains("Content-Transfer-Encoding: base64"));
    assert_eq!(parsed.plain().expect("body").text(), "café");

    let (out, parsed, _) = build(
        "msg-1",
        b"Subject: caf\xe9\r\n",
        "text/plain",
        b"caf\xe9",
        &[],
    );
    assert!(header_text(&out).contains("Content-Type: text/plain\r\n"));
    // No charset is invented: the bytes are decoded as UTF-8, which
    // replaces the invalid byte.
    assert_eq!(parsed.plain().expect("body").text(), "caf\u{fffd}");
}

#[test]
fn hostile_names_and_types_cannot_inject_or_smuggle_parameters() {
    let payloads: [&[u8]; 6] = [b"a", b"b", b"c", b"d", b"e", b"f"];
    let parts = [
        part(
            "evil\r\nBcc: attacker@example.test",
            "text/plain",
            Disposition::Attachment,
            None,
            payloads[0],
        ),
        part(
            "quote\"name.txt",
            "text/plain",
            Disposition::Attachment,
            None,
            payloads[1],
        ),
        part(
            "../evil.txt",
            "text/plain",
            Disposition::Attachment,
            None,
            payloads[2],
        ),
        part(
            "café – ünïcode.txt",
            "text/plain",
            Disposition::Attachment,
            None,
            payloads[3],
        ),
        // Parameters are dropped, not smuggled through.
        part(
            "bad/type",
            "text/plain; charset=\"x\r\nBcc: y\"",
            Disposition::Attachment,
            None,
            payloads[4],
        ),
        part(
            "wrong-type",
            "not a mime type",
            Disposition::Attachment,
            None,
            payloads[5],
        ),
    ];
    let (out, parsed, _) = build("msg-1", HEADER, "text/plain", b"body", &parts);
    let text = String::from_utf8_lossy(&out);
    assert!(
        !text.lines().any(|line| line.starts_with("Bcc:")),
        "a name injected a header line: {text}"
    );
    assert_eq!(
        names(&parsed),
        [
            "evilBcc_ attacker@example.test",
            "quote\"name.txt",
            ".._evil.txt",
            "café – ünïcode.txt",
            "bad_type",
            "wrong-type",
        ]
    );
    assert_eq!(parsed.attachments()[4].media_type(), "text/plain");
    assert_eq!(
        parsed.attachments()[5].media_type(),
        "application/octet-stream"
    );
    for (index, payload) in payloads.iter().enumerate() {
        assert_eq!(bytes_of(&parsed, &out, index), *payload);
    }
}

#[test]
fn the_header_block_replaces_the_old_fields_and_adds_the_internal_id_once() {
    let header = b"Subject: test\r\n\
Content-Type: text/plain;\r\n\
\tcharset=us-ascii\r\n\
Content-Transfer-Encoding: base64\r\n\
X-Pm-Internal-Id: old-id\r\n\
X-Pm-Internal-Id: older-id\r\n\
From: a@example.test\r\n";
    let (out, _, _) = build("msg-9", header, "text/html", b"<p>x</p>", &[]);
    let text = header_text(&out);
    assert_eq!(field_count(&out, "Content-Type"), 1, "{text}");
    assert_eq!(field_count(&out, "Content-Transfer-Encoding"), 1, "{text}");
    assert_eq!(field_count(&out, "X-Pm-Internal-Id"), 1, "{text}");
    assert!(text.contains("Content-Type: text/html; charset=utf-8"));
    assert!(text.contains("Content-Transfer-Encoding: quoted-printable"));
    assert!(text.contains("X-Pm-Internal-Id: msg-9"));
    assert!(!text.contains("us-ascii") && !text.contains("old-id") && !text.contains("older-id"));
    assert!(text.contains("Subject: test") && text.contains("From: a@example.test"));
    // The header block ends with exactly one blank line.
    let end = out
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .expect("a blank line");
    assert!(!out[end + 4..].starts_with(b"\r\n"));
}

#[test]
fn invalid_inputs_and_limits_are_typed_failures() {
    for (id, mime_type) in [
        ("", "text/plain"),
        ("msg\r\n1", "text/plain"),
        ("msg-1", "application/pdf"),
    ] {
        let error = reconstruct(&expanding_input(id, mime_type), &Limits::default())
            .expect_err("the input is refused");
        assert!(matches!(error, ReconstructError::InvalidInput(_)));
        assert!(!error.to_string().contains("msg"));
    }
    assert!(matches!(
        reconstruct(
            &expanding_input("msg-1", "text/plain"),
            &Limits::default().with_max_message_bytes(64)
        ),
        Err(ReconstructError::TooLarge)
    ));
}

proptest! {
    #[test]
    fn the_same_parts_always_reconstruct_to_the_same_bytes(
        message_id in "[A-Za-z0-9_-]{1,24}",
        body in ".*",
        attachments in prop::collection::vec(
            ("[A-Za-z0-9 ._-]{0,24}", prop::collection::vec(any::<u8>(), 0..64)),
            0..3,
        ),
    ) {
        let parts: Vec<ReconstructPart<'_>> = attachments
            .iter()
            .map(|(name, data)| ReconstructPart {
                name,
                mime_type: "application/octet-stream",
                disposition: Disposition::Attachment,
                content_id: None,
                data,
            })
            .collect();
        let input = ReconstructInput {
            message_id: &message_id,
            header: HEADER,
            mime_type: "text/plain",
            body: body.as_bytes(),
            attachments: &parts,
        };
        let first = reconstruct(&input, &Limits::default()).expect("it reconstructs");
        let second = reconstruct(&input, &Limits::default()).expect("it reconstructs");
        prop_assert_eq!(&first, &second, "the same parts give the same bytes");

        // A boundary never appears inside the content it encloses.
        if let Some(boundary) = boundary_of(&first) {
            prop_assert!(!body.contains(&boundary));
            for (_, data) in &attachments {
                prop_assert!(!contains(data, &boundary));
            }
        }

        // The reconstruction parses back to the same text and bytes.
        let parsed = render_input(&first, &Limits::default()).expect("it admits");
        let expected = body.replace("\r\n", "\n").replace('\r', "\n");
        let expected = expected.strip_prefix('\u{feff}').unwrap_or(&expected);
        prop_assert_eq!(parsed.plain().expect("a plain body").text(), expected);
        prop_assert_eq!(parsed.attachments().len(), parts.len());
        for (index, (_, data)) in attachments.iter().enumerate() {
            prop_assert_eq!(bytes_of(&parsed, &first, index), data.as_slice());
        }
    }
}

/// The first `boundary=` parameter of the message's header block.
fn boundary_of(output: &[u8]) -> Option<String> {
    let header = header_text(output);
    header.lines().find_map(|line| {
        line.split_once("boundary=")
            .map(|(_, value)| value.trim().trim_matches('"').to_owned())
    })
}
