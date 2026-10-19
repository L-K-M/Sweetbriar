//! Synthetic reader fixtures, Unlicense OR 0BSD. No real sender data.
use super::*;

fn input(raw: &str) -> RenderInput {
    render_input(raw.as_bytes(), &Limits::default()).unwrap()
}

fn multipart(kind: &str, params: &str, parts: &[&str]) -> String {
    let mut raw = format!(
        "From: sender@example.invalid\r\nContent-Type: multipart/{kind}; boundary=b{params}\r\n\r\n"
    );
    for part in parts {
        raw.push_str("--b\r\n");
        raw.push_str(part);
        raw.push_str("\r\n");
    }
    raw.push_str("--b--\r\n");
    raw
}

#[test]
fn alternative_last_supported_each_kind_and_stable_ids() {
    let raw = multipart(
        "alternative",
        "",
        &[
            "Content-Type: text/plain\r\n\r\nfirst",
            "Content-Type: text/html\r\n\r\n<b>first</b>",
            "Content-Type: text/plain\r\n\r\nlast",
            "Content-Type: text/html\r\n\r\n<b>last</b>",
            "Content-Type: text/calendar\r\n\r\nunsupported",
            "Content-Type: text/plain\r\nContent-Disposition: attachment\r\n\r\nattached",
        ],
    );
    let result = input(&raw);
    assert_eq!(result.plain().unwrap().text(), "last");
    assert_eq!(result.html().unwrap().text(), "<b>last</b>");
    assert_eq!(result.plain().unwrap().part_id().as_str(), "1.3");
    assert_eq!(result.part_ids(), input(&raw).part_ids());
    assert_eq!(result.part_ids().len(), 7);
    // The leaves beside the bodies; the multipart root is no attachment.
    assert_eq!(result.attachments().len(), 4);
}

#[test]
fn related_start_first_child_and_nested_alternative() {
    let nested = "Content-Type: multipart/alternative; boundary=a\r\nContent-ID: <root@example.invalid>\r\n\r\n--a\r\nContent-Type: text/plain\r\n\r\nplain\r\n--a\r\nContent-Type: text/html\r\n\r\n<img src=cid:pic@example.invalid>\r\n--a--";
    let pic = "Content-Type: image/png\r\nContent-ID: <pic@example.invalid>\r\nContent-Transfer-Encoding: base64\r\n\r\nYWJj";
    let raw = multipart(
        "related",
        "; start=\"<root@example.invalid>\"",
        &[pic, nested],
    );
    let result = input(&raw);
    assert_eq!(result.plain().unwrap().text(), "plain");
    assert!(result.html().unwrap().text().starts_with("<img"));
    let resource = &result.resources()[0];
    assert_eq!(resource.content_id(), "pic@example.invalid");
    assert_eq!(resource.part_id().as_str(), "1.1");
    assert_eq!(resource.media_type(), "image/png");
    assert_eq!(resource.bytes(), b"abc");
    for params in ["", "; start=\"<absent@example.invalid>\""] {
        let raw = multipart("related", params, &[nested, pic]);
        assert!(input(&raw).html().is_some());
    }
    assert!(
        input(&multipart("related", "", &[pic, nested]))
            .html()
            .is_none()
    );
}

#[test]
fn attached_messages_and_text_are_never_bodies_but_have_ids() {
    let raw = multipart(
        "mixed",
        "",
        &[
            "Content-Type: message/rfc822\r\nContent-Disposition: attachment\r\n\r\nSubject: inner\r\nContent-Type: text/html\r\n\r\nwrong",
            "Content-Type: text/plain\r\nContent-Disposition: attachment\r\n\r\nwrong",
            "Content-Type: text/plain\r\n\r\nright",
        ],
    );
    let result = input(&raw);
    assert_eq!(result.plain().unwrap().text(), "right");
    assert!(result.html().is_none());
    assert!(result.part_ids().iter().any(|id| id.as_str() == "1.1.1"));
    let bytes = extract_attachment(raw.as_bytes(), "1.1", &Limits::default()).unwrap();
    assert!(bytes.starts_with(b"Subject: inner"));
    // The attached message is the attachment, not its parts.
    assert_eq!(
        extract_attachment(raw.as_bytes(), "1.1.1", &Limits::default()),
        Err(AttachmentError::UnknownPart)
    );
}

#[test]
fn charset_transfer_decoding_and_raw_html() {
    let plain = input(
        "Content-Type: text/plain; charset=iso-8859-1\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\ncaf=E9",
    );
    assert_eq!(plain.plain().unwrap().text(), "café");
    let html = input(
        "Content-Type: text/html; charset=utf-8\r\nContent-Transfer-Encoding: base64\r\n\r\nPHNjcmlwdD54PC9zY3JpcHQ+",
    );
    assert_eq!(html.html().unwrap().text(), "<script>x</script>");
    let result = render_input(
        b"Content-Type: text/plain; charset=utf-8\r\n\r\n\xff",
        &Limits::default(),
    )
    .unwrap();
    assert_eq!(result.plain().unwrap().text(), "\u{fffd}");
    assert!(
        result
            .warnings()
            .iter()
            .any(|w| w.code() == RenderWarningCode::CharsetReplaced
                && w.part_id().unwrap().as_str() == "1")
    );
}

#[test]
fn fixed_text_is_literal_and_preserves_whitespace() {
    let result =
        input("Content-Type: text/plain\r\n\r\n<b>*literal*</b>  \r\n From text\r\n> quote\t \r\n");
    assert_eq!(
        result.plain().unwrap().text(),
        "<b>*literal*</b>  \n From text\n> quote\t \n"
    );
    assert_eq!(input("Subject: empty\r\n\r\n").plain().unwrap().text(), "");
}

#[test]
fn flowed_stuffing_quotes_signatures_and_breaks() {
    let cases = [
        (
            " From sender\n >literal\n  leading\n",
            "From sender\n>literal\n leading\n",
        ),
        (
            "> quoted \n> next\n>> deeper\n>\n",
            ">quoted next\n>>deeper\n>\n",
        ),
        (
            "paragraph \n-- \nsignature  \n",
            "paragraph \n-- \nsignature  \n",
        ),
        ("soft \nhard\nfixed\nlast ", "soft hard\nfixed\nlast "),
        ("one \n> two\n", "one \n>two\n"),
        ("-- \n> -- \n", "-- \n>-- \n"),
    ];
    for (source, expected) in cases {
        assert_eq!(flowed(source, DelSp::Keep), expected, "{source:?}");
    }
}

#[test]
fn flowed_delsp_yes_no_absent_and_final_soft_line() {
    for (params, expected) in [
        ("; delsp=yes", "joined\nfinal"),
        ("; delsp=no", "join ed\nfinal "),
        ("", "join ed\nfinal "),
        ("; delsp=invalid", "join ed\nfinal "),
    ] {
        let raw =
            format!("Content-Type: text/plain; format=flowed{params}\r\n\r\njoin \r\ned\r\nfinal ");
        assert_eq!(input(&raw).plain().unwrap().text(), expected);
    }
    assert_eq!(flowed("last \n", DelSp::Delete), "last\n");
    let html = input("Content-Type: text/html; format=flowed; delsp=yes\r\n\r\nx \r\ny");
    assert_eq!(html.html().unwrap().text(), "x \r\ny");
}

#[test]
fn cid_scope_and_duplicates_fail_closed() {
    let raw = multipart(
        "related",
        "",
        &[
            "Content-Type: text/html\r\n\r\n<img src=cid:duplicate>",
            "Content-Type: image/png\r\nContent-ID: <duplicate>\r\n\r\na",
            "Content-Type: image/jpeg\r\nContent-ID: <duplicate>\r\n\r\nb",
            "Content-Type: message/rfc822\r\n\r\nContent-Type: image/png\r\nContent-ID: <inner>\r\n\r\nc",
            "Content-Type: multipart/related; boundary=n\r\n\r\n--n\r\nContent-Type: text/html\r\n\r\nnested\r\n--n\r\nContent-Type: image/png\r\nContent-ID: <nested>\r\n\r\nd\r\n--n--",
        ],
    );
    let result = input(&raw);
    assert!(result.resources().is_empty());
    assert_eq!(
        result
            .warnings()
            .iter()
            .filter(|w| w.code() == RenderWarningCode::AmbiguousCid)
            .count(),
        2
    );
    let outside = multipart(
        "mixed",
        "",
        &[
            "Content-Type: text/html\r\n\r\n<img src=cid:outside>",
            "Content-Type: image/png\r\nContent-ID: <outside>\r\n\r\nx",
        ],
    );
    assert!(input(&outside).resources().is_empty());
}

#[test]
fn attachment_names_lengths_and_extraction() {
    let name = format!("../../{}:end", "é".repeat(200));
    let attachment = format!(
        "Content-Type: text/plain; charset=iso-8859-1\r\nContent-Disposition: attachment; filename=\"{name}\"\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\ncaf=E9"
    );
    let raw = multipart(
        "mixed",
        "",
        &["Content-Type: text/plain\r\n\r\nbody", &attachment],
    );
    let result = input(&raw);
    let desc = result
        .attachments()
        .iter()
        .find(|d| d.part_id().as_str() == "1.2")
        .unwrap();
    assert!(desc.display_name().len() <= MAX_DISPLAY_NAME_BYTES);
    assert!(!desc.display_name().contains(['/', '\\', ':']));
    assert_eq!(desc.media_type(), "text/plain");
    assert_eq!(desc.decoded_length(), Some(4));
    assert_eq!(
        extract_attachment(raw.as_bytes(), "1.2", &Limits::default()).unwrap(),
        b"caf\xe9"
    );
    assert_eq!(
        extract_attachment(raw.as_bytes(), "unknown", &Limits::default()),
        Err(AttachmentError::UnknownPart)
    );
    assert_eq!(
        extract_attachment(raw.as_bytes(), "1.1", &Limits::default()),
        Err(AttachmentError::BodyPart)
    );
    assert_eq!(
        extract_attachment(
            raw.as_bytes(),
            "1.2",
            &Limits::default().with_max_decoded_part_bytes(2)
        ),
        Err(AttachmentError::LimitExceeded)
    );
    assert_eq!(display_name(Some("a\0b\n/c\\d:e")), "a_b__c_d_e");
    for name in ["", " ", ".", ".."] {
        assert_eq!(display_name(Some(name)), "attachment");
    }
}

#[test]
fn limits_and_unwinds_are_reported() {
    let raw = multipart(
        "mixed",
        "",
        &[
            "Content-Type: text/plain\r\n\r\nx",
            "Content-Type: image/png\r\n\r\ny",
        ],
    );
    for limits in [
        Limits::default().with_max_depth(1),
        Limits::default().with_max_parts(2),
        Limits::default().with_max_message_bytes(3),
    ] {
        let result = render_input(raw.as_bytes(), &limits).unwrap();
        assert!(result.plain().is_none());
        assert_eq!(result.warnings()[0].code().as_str(), "partial_parse");
        assert!(!result.parse_warnings().is_empty());
    }
    let tripwire = b"X-SbMime-Tripwire: panic\r\n\r\nx";
    assert_eq!(
        render_input(tripwire, &Limits::default()),
        Err(ParseFailure::Panic)
    );
    assert_eq!(
        extract_attachment(tripwire, "1", &Limits::default()),
        Err(AttachmentError::Parse(ParseFailure::Panic))
    );
    assert_eq!(
        render_input(b"", &Limits::default()),
        Err(ParseFailure::Unparseable)
    );
    let unsupported = input("Content-Type: application/octet-stream\r\n\r\nx");
    assert!(
        unsupported
            .warnings()
            .iter()
            .any(|w| w.code() == RenderWarningCode::UnsupportedContent)
    );
}

#[test]
fn malformed_candidate_falls_back_and_retains_attachment_descriptor() {
    let raw = multipart(
        "alternative",
        "",
        &[
            "Content-Type: text/plain\r\n\r\nsafe",
            "Content-Type: text/plain\r\nContent-Transfer-Encoding: base64\r\n\r\n!!!!",
        ],
    );
    let result = input(&raw);
    assert_eq!(result.plain().unwrap().text(), "safe");
    assert_eq!(
        result
            .parse_warnings()
            .iter()
            .filter(|w| matches!(w, ParseWarning::Malformed { .. }))
            .count(),
        1
    );
    let damaged = result
        .attachments()
        .iter()
        .find(|a| a.part_id().as_str() == "1.2")
        .unwrap();
    assert_eq!(damaged.decoded_length(), None);
    assert_eq!(
        extract_attachment(raw.as_bytes(), "1.2", &Limits::default()),
        Err(AttachmentError::InvalidEncoding)
    );
}

#[test]
fn an_encoded_attached_message_extracts_whole() {
    let raw = multipart(
        "mixed",
        "",
        &[
            "Content-Type: text/plain\r\n\r\nbody",
            "Content-Type: message/rfc822\r\nContent-Transfer-Encoding: base64\r\n\r\nU3ViamVjdDogaW5uZXINCg0KbmVzdGVk",
        ],
    );
    let result = input(&raw);
    assert_eq!(result.plain().unwrap().text(), "body");
    assert_eq!(
        extract_attachment(raw.as_bytes(), "1.2", &Limits::default()).unwrap(),
        b"Subject: inner\r\n\r\nnested"
    );
    assert_eq!(
        extract_attachment(raw.as_bytes(), "1.2.1", &Limits::default()),
        Err(AttachmentError::UnknownPart)
    );
}

#[test]
fn nested_cid_scope_wins_and_ids_are_case_sensitive() {
    let raw = multipart(
        "related",
        "",
        &[
            "Content-Type: multipart/related; boundary=n\r\n\r\n--n\r\nContent-Type: text/html\r\n\r\n<img src=cid:Pic>\r\n--n\r\nContent-Type: image/png\r\nContent-ID: <Pic>\r\n\r\ninner\r\n--n--",
            "Content-Type: image/png\r\nContent-ID: <Pic>\r\n\r\nouter",
        ],
    );
    let result = input(&raw);
    assert_eq!(result.resources().len(), 1);
    assert_eq!(result.resources()[0].bytes(), b"inner");
    let raw = multipart(
        "related",
        "",
        &[
            "Content-Type: text/html\r\n\r\nhtml",
            "Content-Type: image/png\r\nContent-ID: <Pic>\r\n\r\na",
            "Content-Type: image/png\r\nContent-ID: <pic>\r\n\r\nb",
        ],
    );
    let result = input(&raw);
    assert_eq!(result.resources().len(), 2);
    assert!(
        !result
            .warnings()
            .iter()
            .any(|w| w.code() == RenderWarningCode::AmbiguousCid)
    );
}

/// Containers and an attached message's inner parts aren't attachments
/// and can't be extracted as one; a leaf extracts under the budget.
#[test]
fn only_leaf_parts_are_attachments() {
    let raw = multipart(
        "mixed",
        "",
        &[
            "Content-Type: text/plain\r\n\r\nx",
            "Content-Type: application/octet-stream\r\n\r\ny",
        ],
    );
    let limits = Limits::default()
        .with_max_depth(2)
        .with_max_parts(3)
        .with_max_decoded_part_bytes(3);
    let result = render_input(raw.as_bytes(), &limits).unwrap();
    assert_eq!(result.plain().unwrap().text(), "x");
    let ids: Vec<_> = result
        .attachments()
        .iter()
        .map(|a| a.part_id().as_str())
        .collect();
    assert_eq!(ids, ["1.2"]);
    assert_eq!(
        extract_attachment(raw.as_bytes(), "1", &limits),
        Err(AttachmentError::UnknownPart)
    );
    assert_eq!(
        extract_attachment(raw.as_bytes(), "1.2", &limits).unwrap(),
        b"y"
    );
}

#[test]
fn attached_container_children_are_not_candidates() {
    let raw = multipart(
        "mixed",
        "",
        &[
            "Content-Type: multipart/alternative; boundary=a\r\nContent-Disposition: attachment\r\n\r\n--a\r\nContent-Type: text/html\r\n\r\nwrong\r\n--a--",
            "Content-Type: text/plain\r\n\r\nbody",
        ],
    );
    let result = input(&raw);
    assert!(result.html().is_none());
    assert_eq!(result.plain().unwrap().text(), "body");
    assert_eq!(result.part_ids().len(), 4);
}

#[test]
fn existing_synthetic_corpus_does_not_unwind_in_reader_or_extractor() {
    let directory = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../fixtures/eml");
    for entry in std::fs::read_dir(directory).unwrap() {
        let path = entry.unwrap().path();
        if path.extension().is_none_or(|ext| ext != "eml") {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        let result = render_input(&bytes, &Limits::default());
        assert!(!matches!(result, Err(ParseFailure::Panic)), "{path:?}");
        if let Ok(result) = result {
            for descriptor in result.attachments() {
                let extracted =
                    extract_attachment(&bytes, descriptor.part_id().as_str(), &Limits::default());
                assert!(
                    !matches!(extracted, Err(AttachmentError::Parse(ParseFailure::Panic))),
                    "{path:?}"
                );
            }
        }
    }
}

#[test]
fn utf8_literal_replacement_character_is_not_a_charset_failure() {
    let result = input("Content-Type: text/plain; charset=utf-8\r\n\r\nLiteral \u{fffd}");
    assert!(result.warnings().is_empty());
    let result = input(
        "Content-Type: text/html; charset=iso-8859-1\r\nContent-Transfer-Encoding: quoted-printable\r\n\r\n<b>caf=E9</b>",
    );
    assert_eq!(result.html().unwrap().text(), "<b>café</b>");
    assert!(result.warnings().is_empty());
}

#[test]
fn text_attachment_transfer_decoder_matches_parser_recovery() {
    for (encoding, encoded, expected) in [
        ("quoted-printable", "a=ZZb", b"a=ZZb".as_slice()),
        ("base64", "A=", b"\0".as_slice()),
    ] {
        let attachment = format!(
            "Content-Type: text/plain\r\nContent-Disposition: attachment\r\nContent-Transfer-Encoding: {encoding}\r\n\r\n{encoded}"
        );
        let raw = multipart(
            "mixed",
            "",
            &["Content-Type: text/plain\r\n\r\nbody", &attachment],
        );
        let result = input(&raw);
        let descriptor = result
            .attachments()
            .iter()
            .find(|a| a.part_id().as_str() == "1.2")
            .unwrap();
        assert_eq!(descriptor.decoded_length(), Some(expected.len() as u64));
        assert_eq!(
            extract_attachment(raw.as_bytes(), "1.2", &Limits::default()).unwrap(),
            expected
        );
    }
}

#[test]
fn filename_only_text_is_an_attachment() {
    let raw = multipart(
        "mixed",
        "",
        &[
            "Content-Type: text/plain; name=\"file.txt\"\r\n\r\nfile",
            "Content-Type: text/plain\r\n\r\nbody",
        ],
    );
    let result = input(&raw);
    assert_eq!(result.plain().unwrap().text(), "body");
    let descriptor = result
        .attachments()
        .iter()
        .find(|a| a.part_id().as_str() == "1.1")
        .unwrap();
    assert_eq!(descriptor.display_name(), "file.txt");
    assert_eq!(
        extract_attachment(raw.as_bytes(), "1.1", &Limits::default()).unwrap(),
        b"file"
    );
}
