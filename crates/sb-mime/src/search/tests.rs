use super::*;
use crate::render_input;

fn input(raw: &str) -> SearchInputs {
    search_inputs(raw.as_bytes(), &Limits::default()).unwrap()
}

fn multipart(subtype: &str, parts: &[&str]) -> String {
    let mut raw = format!("Content-Type: multipart/{subtype}; boundary=b\r\n\r\n");
    for part in parts {
        raw.push_str(&format!("--b\r\n{part}\r\n"));
    }
    raw.push_str("--b--\r\n");
    raw
}

#[test]
fn all_root_mailboxes_are_decoded_including_groups_and_repeated_headers() {
    let raw = concat!(
        "Subject: =?UTF-8?Q?caf=C3=A9?=\r\n",
        "From: =?UTF-8?Q?Andr=C3=A9?= <one@example.com>, Two <two@example.com>\r\n",
        "From: Team: Three <three@example.com>;\r\n",
        "To: Friends: Four <four@example.com>, five@example.com;\r\n",
        "To: Six <six@example.com>\r\n",
        "Cc: =?ISO-8859-1?Q?Ren=E9?= <seven@example.com>\r\n",
        "Bcc: secret@example.com\r\nReply-To: reply@example.com\r\n\r\nbody"
    );
    let result = input(raw);
    assert_eq!(result.subject(), "café");
    assert_eq!(
        result.from_text(),
        "André\none@example.com\nTwo\ntwo@example.com\nThree\nthree@example.com"
    );
    assert_eq!(
        result.to_text(),
        "Four\nfour@example.com\nfive@example.com\nSix\nsix@example.com"
    );
    assert_eq!(result.cc_text(), "René\nseven@example.com");
    let missing = input("Content-Type: text/plain\r\n\r\nbody");
    assert_eq!(
        (
            missing.subject(),
            missing.from_text(),
            missing.to_text(),
            missing.cc_text()
        ),
        ("", "", "", "")
    );
}

#[test]
fn alternatives_are_all_returned_in_mime_order_with_reader_ids() {
    let raw = multipart(
        "alternative",
        &[
            "Content-Type: text/plain\r\n\r\nfirst",
            "Content-Type: text/html\r\n\r\n<p>second</p>",
            "Content-Type: text/plain\r\n\r\nthird",
        ],
    );
    let result = input(&raw);
    let reader = render_input(raw.as_bytes(), &Limits::default()).unwrap();
    let bodies: Vec<_> = result
        .bodies()
        .iter()
        .map(|b| (b.part_id().as_str(), b.kind(), b.text()))
        .collect();
    assert_eq!(
        bodies,
        [
            ("1.1", SearchBodyKind::Plain, "first"),
            ("1.2", SearchBodyKind::Html, "<p>second</p>"),
            ("1.3", SearchBodyKind::Plain, "third"),
        ]
    );
    assert_eq!(
        result.bodies()[1].part_id(),
        reader.html().unwrap().part_id()
    );
    assert_eq!(
        result.bodies()[2].part_id(),
        reader.plain().unwrap().part_id()
    );
    for body in result.bodies() {
        assert!(reader.part_ids().contains(body.part_id()));
    }
    assert!(result.parse_warnings().is_empty());
}

#[test]
fn related_traversal_is_independent_of_reader_root_and_never_fetches_resources() {
    // Even a start-selected related body does not hide other inline search text.
    let raw = multipart(
        "related; start=\"<chosen>\"",
        &[
            "Content-Type: text/plain\r\n\r\nfirst",
            "Content-Type: text/html\r\nContent-ID: <chosen>\r\n\r\n<p>text &amp; entities</p><img src=http://127.0.0.1:9/image><script>raw</script>",
            "Content-Type: image/png\r\nContent-Disposition: inline; filename=picture.png\r\n\r\nresource",
        ],
    );
    let result = input(&raw);
    assert_eq!(result.bodies().len(), 2);
    assert_eq!(result.bodies()[0].text(), "first");
    assert_eq!(result.bodies()[1].kind(), SearchBodyKind::Html);
    assert_eq!(
        result.bodies()[1].text(),
        "<p>text &amp; entities</p><img src=http://127.0.0.1:9/image><script>raw</script>"
    );
    assert_eq!(result.attachment_names()[0].name(), "picture.png");
}

#[test]
fn shared_charset_and_transfer_decoder_preserves_replacements() {
    for (raw, expected, replaced) in [
        (b"Content-Type: text/plain; charset=iso-8859-1\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\ncaf=E9".as_slice(), "café", false),
        (b"Content-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\nPHA+eDwvcD4=".as_slice(), "<p>x</p>", false),
        (b"Content-Type: text/plain; charset=utf-8\r\n\r\na\xffb".as_slice(), "a\u{fffd}b", true),
        ("Content-Type: text/plain; charset=utf-8\r\n\r\nLiteral \u{fffd}".as_bytes(), "Literal \u{fffd}", false),
        (b"Content-Type: text/plain; charset=utf-16le\r\nContent-Transfer-Encoding: base64\r\n\r\nQQAA2A==".as_slice(), "A\u{fffd}", true),
    ] {
        let result = search_inputs(raw, &Limits::default()).unwrap();
        assert_eq!(result.bodies().len(), 1);
        let body = &result.bodies()[0];
        assert_eq!(body.text(), expected);
        assert_eq!(body.charset_replaced(), replaced);
        let reader = render_input(raw, &Limits::default()).unwrap();
        let selected = reader.plain().or(reader.html()).unwrap();
        assert_eq!(body.text(), selected.text());
        assert_eq!(body.part_id(), selected.part_id());
    }
}

#[test]
fn plain_text_decodes_flowed_and_line_endings() {
    for (params, expected) in [
        ("; delsp=yes", "joined\n>quotenext\n-- \nsignature\n"),
        ("", "join ed\n>quote next\n-- \nsignature\n"),
    ] {
        let raw = format!(
            "Content-Type: text/plain; format=flowed{params}\r\n\r\njoin \r\ned\r\n> quote \r\n> next\r\n-- \r\nsignature\r\n"
        );
        let result = input(&raw);
        assert_eq!(result.bodies()[0].text(), expected);
        assert_eq!(
            result.bodies()[0].text(),
            render_input(raw.as_bytes(), &Limits::default())
                .unwrap()
                .plain()
                .unwrap()
                .text()
        );
    }
    assert_eq!(
        input("Content-Type: text/plain\r\n\r\nfixed  \r\nline").bodies()[0].text(),
        "fixed  \nline"
    );
}

#[test]
fn attached_messages_and_attachment_contents_are_excluded() {
    let raw = multipart(
        "mixed",
        &[
            "Content-Type: text/plain\r\n\r\nbody",
            "Content-Type: message/rfc822; name=forward.eml\r\n\r\nSubject: inner\r\nFrom: Hidden <hidden@example.com>\r\nContent-Type: text/plain\r\n\r\nnot searchable",
            "Content-Type: message/rfc822\r\nContent-Disposition: attachment; filename=encoded.eml\r\nContent-Transfer-Encoding: base64\r\n\r\nU3ViamVjdDogaW5uZXINCg0KbmVzdGVk",
            "Content-Type: text/plain; name=file.txt\r\n\r\nnot a body",
            "Content-Type: text/html\r\nContent-Disposition: attachment\r\n\r\n<p>not a body</p>",
            "Content-Type: multipart/alternative; boundary=n\r\nContent-Disposition: attachment; filename=container.mime\r\n\r\n--n\r\nContent-Type: text/plain; name=hidden.txt\r\n\r\nhidden\r\n--n--",
            "Content-Type: text/calendar\r\n\r\nnot plain",
        ],
    );
    let result = input(&raw);
    assert_eq!(result.bodies().len(), 1);
    assert_eq!(result.bodies()[0].text(), "body");
    assert_eq!(result.subject(), "");
    assert_eq!(result.from_text(), "");
    let names: Vec<_> = result
        .attachment_names()
        .iter()
        .map(|n| (n.part_id().as_str(), n.name()))
        .collect();
    assert_eq!(
        names,
        [
            ("1.2", "forward.eml"),
            ("1.3", "encoded.eml"),
            ("1.4", "file.txt"),
            ("1.6", "container.mime")
        ]
    );
    let digest = multipart("digest", &["\r\nSubject: inner\r\n\r\nhidden"]);
    assert!(input(&digest).bodies().is_empty());
}

#[test]
fn root_attachments_and_named_containers_remain_opaque() {
    for header in [
        "Content-Type: text/plain; name=root.txt",
        "Content-Type: text/plain\r\nContent-Disposition: attachment; filename=root.txt",
        "Content-Type: message/rfc822; name=root.txt",
    ] {
        let result = input(&format!("{header}\r\n\r\nSubject: inner\r\n\r\nhidden"));
        assert!(result.bodies().is_empty());
        assert_eq!(result.attachment_names().len(), 1);
        assert_eq!(result.attachment_names()[0].name(), "root.txt");
        assert_eq!(result.attachment_names()[0].part_id().as_str(), "1");
        assert!(result.parse_warnings().is_empty());
    }
    let named = "Content-Type: multipart/mixed; boundary=n; name=container.mime\r\n\r\n--n\r\nContent-Type: text/plain; name=hidden.txt\r\n\r\nhidden\r\n--n--";
    for raw in [named.to_owned(), multipart("mixed", &[named])] {
        let result = input(&raw);
        assert!(result.bodies().is_empty());
        assert_eq!(result.attachment_names().len(), 1);
        assert_eq!(result.attachment_names()[0].name(), "container.mime");
        assert!(result.parse_warnings().is_empty());
    }
}

#[test]
fn filenames_decode_rfc2231_and_encoded_words_without_display_rewriting() {
    let raw = multipart(
        "mixed",
        &[
            "Content-Type: application/octet-stream\r\nContent-Disposition: attachment; filename*=utf-8''caf%C3%A9.pdf\r\n\r\nignored",
            "Content-Type: application/octet-stream\r\nContent-Disposition: attachment; filename=\"=?ISO-8859-1?Q?Ren=E9.txt?=\"\r\n\r\nignored",
            "Content-Type: application/octet-stream; name*=utf-8''..%2Fpath%3Aname.txt\r\n\r\nignored",
            "Content-Type: application/octet-stream\r\nContent-Disposition: attachment; filename*0*=utf-8''long%20; filename*1*=caf%C3%A9.txt\r\n\r\nignored",
            "Content-Type: application/octet-stream\r\n\r\nunnamed",
        ],
    );
    let result = input(&raw);
    let names: Vec<_> = result.attachment_names().iter().map(|n| n.name()).collect();
    assert_eq!(
        names,
        ["café.pdf", "René.txt", "../path:name.txt", "long café.txt"]
    );
    let reader = render_input(raw.as_bytes(), &Limits::default()).unwrap();
    for name in result.attachment_names() {
        assert!(
            reader
                .attachments()
                .iter()
                .any(|a| a.part_id() == name.part_id())
        );
    }
}

#[test]
fn admission_limits_refuse_all_content_like_reader() {
    let raw = multipart(
        "mixed",
        &[
            "Content-Type: text/plain\r\n\r\nbody",
            "Content-Type: application/octet-stream; name=file\r\n\r\nbytes",
        ],
    );
    for limits in [
        Limits::default().with_max_message_bytes(3),
        Limits::default().with_max_depth(1),
        Limits::default().with_max_parts(2),
        Limits::default().with_max_decoded_part_bytes(3),
        Limits::default().with_max_header_bytes(10),
        Limits::default().with_max_header_fields(0),
    ] {
        let result = search_inputs(raw.as_bytes(), &limits).unwrap();
        let reader = render_input(raw.as_bytes(), &limits).unwrap();
        assert!(!result.parse_warnings().is_empty());
        assert_eq!(result.parse_warnings(), reader.parse_warnings());
        assert!(result.bodies().is_empty());
        assert!(result.attachment_names().is_empty());
        assert_eq!(result.subject(), "");
    }
    let expanding = "Content-Type: text/plain; charset=\" windows-1252 \"\r\n\r\né";
    let result = search_inputs(
        expanding.as_bytes(),
        &Limits::default().with_max_decoded_part_bytes(2),
    )
    .unwrap();
    assert!(result.bodies().is_empty());
    assert_eq!(
        result.parse_warnings(),
        [ParseWarning::PartTooLarge { limit: 2 }]
    );
}

#[test]
fn damaged_candidates_warn_instead_of_succeeding_silently() {
    let raw = multipart(
        "alternative",
        &[
            "Content-Type: text/plain\r\n\r\nretained",
            "Content-Type: text/plain\r\nContent-Transfer-Encoding: base64\r\n\r\n!!!!",
        ],
    );
    let result = input(&raw);
    assert_eq!(result.bodies().len(), 1);
    assert_eq!(result.bodies()[0].text(), "retained");
    assert!(
        result
            .parse_warnings()
            .iter()
            .any(|w| matches!(w, ParseWarning::Malformed { .. }))
    );
}

#[test]
fn panics_and_unparseable_inputs_are_failures() {
    assert_eq!(
        search_inputs(b"X-SbMime-Tripwire: panic\r\n\r\nx", &Limits::default()),
        Err(ParseFailure::Panic)
    );
    assert_eq!(
        search_inputs(b"", &Limits::default()),
        Err(ParseFailure::Unparseable)
    );
}

#[test]
fn synthetic_corpus_has_stable_ids_and_no_panics() {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/eml");
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|ext| ext != "eml") {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        let result = search_inputs(&bytes, &Limits::default());
        if bytes.is_empty() {
            assert_eq!(result, Err(ParseFailure::Unparseable), "{path:?}");
            continue;
        }
        let result = result.unwrap_or_else(|error| panic!("{path:?}: {error:?}"));
        let reader = render_input(&bytes, &Limits::default()).unwrap();
        for id in result
            .bodies()
            .iter()
            .map(|b| b.part_id())
            .chain(result.attachment_names().iter().map(|n| n.part_id()))
        {
            assert!(reader.part_ids().contains(id), "{path:?}: {id:?}");
        }
    }
}
