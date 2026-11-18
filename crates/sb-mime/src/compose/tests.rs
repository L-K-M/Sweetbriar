//! Tests for `sb_mime::compose` (M4.2): representative snapshots, the
//! re-parse round trip, the Proton limits, and the header safety rules.

use crate::{
    ComposeAddress, ComposeAttachment, ComposeError, ComposeFormat, ComposeInput, Composed, Limits,
    ParseOutcome, check_limits, compose, extract_attachment, parse, render_input,
};

const MESSAGE_ID: &str = "0191abcd-1234-4abc-8def-0123456789ab@example.org";
const DATE: i64 = 1_760_000_000;

const ADA: ComposeAddress<'static> = ComposeAddress {
    name: Some("Ada Lovelace"),
    address: "ada@example.org",
};
const GRACE: ComposeAddress<'static> = ComposeAddress {
    name: Some("Grace Hopper"),
    address: "grace@example.com",
};
const TO: [ComposeAddress<'static>; 1] = [GRACE];

fn base_input(body: &'static str, format: ComposeFormat) -> ComposeInput<'static> {
    ComposeInput {
        from: ADA,
        to: &TO,
        cc: &[],
        bcc: &[],
        reply_to: None,
        subject: "Sweetbriar draft",
        body,
        format,
        attachments: &[],
        message_id: MESSAGE_ID,
        date: DATE,
        in_reply_to: None,
        references: &[],
        user_agent: "Sweetbriar/0.0.1",
    }
}

/// CRLF is normalized so the committed snapshots stay reviewable.
fn snapshot_text(composed: &Composed) -> String {
    String::from_utf8_lossy(&composed.bytes).replace("\r\n", "\n")
}

// ---- snapshots ----

#[test]
fn plain_flowed_snapshot() {
    let input = base_input(
        "Hello Grace,\n\nThis paragraph is long enough that the composer has to wrap it at the flowed width, and every soft line carries the trailing space that lets a recipient rewrap it.\n\nBye,\nAda",
        ComposeFormat::Plain { literal: false },
    );
    let composed = compose(&input).unwrap();
    insta::assert_snapshot!("plain-flowed", snapshot_text(&composed));
    let text = String::from_utf8_lossy(&composed.bytes);
    assert!(text.contains("format=\"flowed\""), "{text}");
}

#[test]
fn plain_literal_snapshot() {
    let input = base_input(
        "diff --git a/x b/x\n--- a/x\n+++ b/x\n@@ -1 +1 @@\n-a line that is quite long and must not be wrapped because this is a literal patch body\n+new line\n",
        ComposeFormat::Plain { literal: true },
    );
    let composed = compose(&input).unwrap();
    insta::assert_snapshot!("plain-literal", snapshot_text(&composed));
    let text = String::from_utf8_lossy(&composed.bytes);
    assert!(!text.contains("format=flowed"));
    assert!(composed.plain_text.contains(
        "-a line that is quite long and must not be wrapped because this is a literal patch body\n"
    ));
}

#[test]
fn markdown_rich_snapshot() {
    let input = base_input(
        "# Heading\n\nSome *emphasis*, **strong**, ~~struck~~ and `code`.\n\n- first\n- second\n  - nested\n\n1. one\n2. two\n\n> quoted text\n\n[Example](https://example.com/path)\n\n| Left | Center | Right |\n|:-----|:------:|------:|\n| a | b | c |\n\n---\n\n<em>raw html</em>\n",
        ComposeFormat::Markdown,
    );
    let composed = compose(&input).unwrap();
    insta::assert_snapshot!("markdown-rich", snapshot_text(&composed));
    insta::assert_snapshot!("markdown-rich-plain", composed.plain_text);
    insta::assert_snapshot!("markdown-rich-html", composed.html.as_deref().unwrap());
    let text = String::from_utf8_lossy(&composed.bytes);
    assert!(!text.contains("text/markdown"), "{text}");
    assert_eq!(compose(&input).unwrap().bytes, composed.bytes);
}

#[test]
fn markdown_inline_image_and_attachment_snapshot() {
    let image = ComposeAttachment {
        name: "logo.png",
        mime_type: "image/png",
        data: b"\x89PNG\r\n\x1a\n fake image bytes for the snapshot",
        content_id: Some("logo@example.org"),
    };
    let file = ComposeAttachment {
        name: "notes.txt",
        mime_type: "text/plain",
        data: b"attachment bytes",
        content_id: None,
    };
    let attachments = [image, file];
    let mut input = base_input(
        "# With an image\n\n![Logo](cid:logo@example.org)\n\nSee the attached notes.\n",
        ComposeFormat::Markdown,
    );
    input.attachments = &attachments;
    let composed = compose(&input).unwrap();
    insta::assert_snapshot!("markdown-inline-image", snapshot_text(&composed));
    insta::assert_snapshot!(
        "markdown-inline-image-html",
        composed.html.as_deref().unwrap()
    );
    let text = String::from_utf8_lossy(&composed.bytes);
    assert!(text.contains("multipart/related"), "{text}");
    assert!(text.contains("Content-ID: <logo@example.org>"), "{text}");
    assert!(
        composed
            .html
            .as_deref()
            .unwrap()
            .contains("cid:logo@example.org")
    );
}

#[test]
fn reply_headers_snapshot() {
    let cc = [ComposeAddress {
        name: Some("CC Recipient"),
        address: "cc@example.net",
    }];
    let reply_to = ComposeAddress {
        name: Some("Reply Person"),
        address: "reply@example.net",
    };
    let mut input = base_input(
        "Thanks, I will pick it up tomorrow.\n",
        ComposeFormat::Plain { literal: false },
    );
    input.subject = "Re: Thread";
    input.cc = &cc;
    input.reply_to = Some(reply_to);
    input.in_reply_to = Some("original@example.org");
    input.references = &["root@example.org", "original@example.org"];
    let composed = compose(&input).unwrap();
    insta::assert_snapshot!("reply-headers", snapshot_text(&composed));
}

// ---- content rules ----

#[test]
fn markdown_raw_html_is_literal_text() {
    let input = base_input(
        "Before <script>alert(1)</script> and <b>bold</b> after.\n",
        ComposeFormat::Markdown,
    );
    let composed = compose(&input).unwrap();
    let html = composed.html.as_deref().unwrap();
    assert!(
        html.contains("&lt;script&gt;alert(1)&lt;/script&gt;"),
        "{html}"
    );
    assert!(html.contains("&lt;b&gt;bold&lt;/b&gt;"), "{html}");
    assert!(!html.contains("<script>"), "{html}");
    assert!(!html.contains("<b>"), "{html}");
    assert!(composed.plain_text.contains("<script>alert(1)</script>"));
    assert!(composed.plain_text.contains("<b>bold</b>"));
}

#[test]
fn markdown_plain_alternative_has_no_emphasis_or_heading_markers() {
    let input = base_input(
        "# Heading\n\nSome *emphasis*, **strong**, ~~struck~~ and `code`.\n\n- first\n- second\n\n> quote\n\n[Example](https://example.com/path)\n",
        ComposeFormat::Markdown,
    );
    let composed = compose(&input).unwrap();
    let plain = &composed.plain_text;
    assert!(!plain.contains('*'), "{plain}");
    assert!(!plain.contains('_'), "{plain}");
    assert!(!plain.contains('#'), "{plain}");
    assert!(plain.contains("Heading"));
    assert!(plain.contains("Example <https://example.com/path>"));
}

// ---- header safety ----

#[test]
fn bcc_recipients_produce_no_bcc_header() {
    let bcc = [ComposeAddress {
        name: Some("Secret Recipient"),
        address: "secret@example.org",
    }];
    let mut input = base_input("Hello.\n", ComposeFormat::Plain { literal: false });
    input.bcc = &bcc;
    let composed = compose(&input).unwrap();
    let text = String::from_utf8_lossy(&composed.bytes);
    let headers = text.split("\r\n\r\n").next().unwrap_or(&text);
    assert!(!headers.contains("Bcc:"), "{headers}");
    assert!(!headers.contains("secret@example.org"), "{headers}");
    assert!(
        headers.contains("To: \"Grace Hopper\" <grace@example.com>"),
        "{headers}"
    );
}

#[test]
fn crlf_in_header_inputs_is_refused() {
    let mut subject = base_input("Hello.\n", ComposeFormat::Plain { literal: false });
    subject.subject = "bad\r\nBcc: victim@example.org";
    assert!(matches!(
        compose(&subject),
        Err(ComposeError::InvalidInput(_))
    ));

    let mut name = base_input("Hello.\n", ComposeFormat::Plain { literal: false });
    name.from = ComposeAddress {
        name: Some("Ada\nLovelace"),
        address: "ada@example.org",
    };
    assert!(matches!(compose(&name), Err(ComposeError::InvalidInput(_))));

    let mut empty_id = base_input("Hello.\n", ComposeFormat::Plain { literal: false });
    empty_id.message_id = "";
    assert!(matches!(
        compose(&empty_id),
        Err(ComposeError::InvalidInput(_))
    ));
}

// ---- limits ----

#[test]
fn limits_block_with_typed_errors() {
    let big = vec![0u8; 26_000_000];
    let oversized = [ComposeAttachment {
        name: "big.bin",
        mime_type: "application/octet-stream",
        data: &big,
        content_id: None,
    }];
    assert_eq!(
        check_limits(&oversized),
        Err(ComposeError::TooLarge { total: 26_000_000 })
    );
    let mut input = base_input("Hello.\n", ComposeFormat::Plain { literal: false });
    input.attachments = &oversized;
    assert_eq!(
        compose(&input),
        Err(ComposeError::TooLarge { total: 26_000_000 })
    );

    let empty: [ComposeAttachment; 101] = [ComposeAttachment {
        name: "empty.bin",
        mime_type: "application/octet-stream",
        data: &[],
        content_id: None,
    }; 101];
    assert_eq!(
        check_limits(&empty),
        Err(ComposeError::TooManyFiles { count: 101 })
    );
    let mut input = base_input("Hello.\n", ComposeFormat::Plain { literal: false });
    input.attachments = &empty;
    assert_eq!(
        compose(&input),
        Err(ComposeError::TooManyFiles { count: 101 })
    );
}

// ---- re-parse round trip ----

#[test]
fn generated_messages_reparse_to_the_same_content() {
    let image = ComposeAttachment {
        name: "logo.png",
        mime_type: "image/png",
        data: b"\x89PNG\r\n\x1a\n round trip",
        content_id: Some("logo@example.org"),
    };
    let file = ComposeAttachment {
        name: "notes.txt",
        mime_type: "text/plain",
        data: b"attachment bytes",
        content_id: None,
    };
    let mut markdown = base_input(
        "# Heading\n\nSome *emphasis* and a [link](https://example.com/path).\n\n| A | B |\n|---|---|\n| 1 | 2 |\n",
        ComposeFormat::Markdown,
    );
    markdown.subject = "Round trip";
    markdown.cc = &[ComposeAddress {
        name: Some("CC Recipient"),
        address: "cc@example.net",
    }];
    markdown.in_reply_to = Some("original@example.org");
    markdown.references = &["root@example.org", "original@example.org"];
    let attachments = [image, file];
    markdown.attachments = &attachments;

    let drafts = [
        compose(&base_input(
            "A flowed paragraph long enough to wrap at the composer width so the round trip has to survive soft line breaks.\n",
            ComposeFormat::Plain { literal: false },
        ))
        .unwrap(),
        compose(&base_input(
            "diff --git a/x b/x\n--- a/x\n+++ b/x\n",
            ComposeFormat::Plain { literal: true },
        ))
        .unwrap(),
        compose(&markdown).unwrap(),
    ];

    for composed in &drafts {
        let summary = match parse(&composed.bytes, &Limits::default()) {
            ParseOutcome::Parsed(summary) => summary,
            other => panic!("expected Parsed, got {other:?}"),
        };
        let reader = render_input(&composed.bytes, &Limits::default()).unwrap();
        assert!(reader.warnings().is_empty(), "{:?}", reader.warnings());
        assert_eq!(summary.message_id().unwrap(), MESSAGE_ID);
        assert_eq!(
            summary.from().unwrap().address().unwrap(),
            "ada@example.org"
        );
        assert_eq!(summary.to()[0].address().unwrap(), "grace@example.com");
        assert_eq!(
            normalize(reader.plain().unwrap().text()),
            normalize(&unflow(&composed.plain_text))
        );
        assert_eq!(reader.html().is_some(), composed.html.is_some());
        if let Some(html) = &composed.html {
            assert_eq!(normalize(reader.html().unwrap().text()), normalize(html));
        }
    }

    // The reply draft carries the threading and CC headers.
    let summary = match parse(&drafts[2].bytes, &Limits::default()) {
        ParseOutcome::Parsed(summary) => summary,
        other => panic!("expected Parsed, got {other:?}"),
    };
    assert_eq!(summary.subject().unwrap(), "Round trip");
    assert_eq!(summary.cc()[0].address().unwrap(), "cc@example.net");
    assert_eq!(summary.in_reply_to().unwrap(), "original@example.org");
    assert_eq!(
        summary.references(),
        &[
            "root@example.org".to_string(),
            "original@example.org".to_string()
        ]
    );

    let reader = render_input(&drafts[2].bytes, &Limits::default()).unwrap();
    let resource = reader
        .resources()
        .iter()
        .find(|resource| resource.content_id() == "logo@example.org")
        .expect("inline image resource");
    assert_eq!(resource.bytes(), image.data);
    let attachment = reader
        .attachments()
        .iter()
        .find(|attachment| attachment.display_name() == "notes.txt")
        .expect("file attachment");
    assert_eq!(
        extract_attachment(
            &drafts[2].bytes,
            attachment.part_id().as_str(),
            &Limits::default()
        )
        .unwrap(),
        file.data
    );
}

fn normalize(text: &str) -> String {
    text.replace("\r\n", "\n")
}

/// Join the soft lines the composer wrapped (a trailing space before the
/// newline), so the frozen wire text can be compared with what a reader
/// shows after format=flowed unfolding.
fn unflow(text: &str) -> String {
    let mut out = String::new();
    for line in text.split_inclusive('\n') {
        if line.ends_with(" \n") {
            out.push_str(&line[..line.len() - 1]);
        } else {
            out.push_str(line);
        }
    }
    out
}
