use super::*;

fn document(raw: &str) -> SearchDocument {
    search_document(raw.as_bytes(), &Limits::default())
}

fn plain(body: &str) -> SearchDocument {
    document(&format!(
        "Content-Type: text/plain; charset=utf-8\r\n\r\n{body}"
    ))
}

fn html(body: &str) -> SearchDocument {
    document(&format!(
        "Content-Type: text/html; charset=utf-8\r\n\r\n{body}"
    ))
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
fn realistic_message_populates_every_column_and_metadata() {
    let raw = concat!(
        "Subject: =?UTF-8?Q?Quarterly_caf=C3=A9?=\r\n",
        "From: =?UTF-8?Q?Andr=C3=A9?= <andre@example.invalid>\r\n",
        "From: Team: Other <other@example.invalid>;\r\n",
        "To: Readers: Alice <alice@example.invalid>, bob@example.invalid;\r\n",
        "Cc: Carol <carol@example.invalid>\r\n",
        "Bcc: private@example.invalid\r\n",
        "MIME-Version: 1.0\r\nContent-Type: multipart/mixed; boundary=outer\r\n\r\n",
        "--outer\r\nContent-Type: multipart/alternative; boundary=alt\r\n\r\n",
        "--alt\r\nContent-Type: text/plain; charset=utf-8\r\n",
        "Content-Transfer-Encoding: quoted-printable\r\n\r\n",
        "Hello caf=C3=A9\r\n\r\n> Previous quarter\r\n\r\n--=20\r\nAndr=C3=A9\r\n",
        "--alt\r\nContent-Type: text/html; charset=utf-8\r\n\r\n",
        "<p>Hello café</p><p>HTML details</p><blockquote>Earlier figures</blockquote>\r\n",
        "--alt--\r\n",
        "--outer\r\nContent-Type: application/pdf\r\n",
        "Content-Disposition: attachment; filename*=utf-8''caf%C3%A9.pdf\r\n\r\nPDF-HIDDEN\r\n",
        "--outer\r\nContent-Type: message/rfc822; name=forward.eml\r\n\r\n",
        "Subject: INNER-HIDDEN\r\n\r\nNESTED-HIDDEN\r\n",
        "--outer--\r\n"
    );
    let doc = document(raw);
    assert_eq!(doc.subject(), "Quarterly café");
    assert_eq!(
        doc.from_text(),
        "André\nandre@example.invalid\nOther\nother@example.invalid"
    );
    assert_eq!(
        doc.to_text(),
        "Alice\nalice@example.invalid\nbob@example.invalid"
    );
    assert_eq!(doc.cc_text(), "Carol\ncarol@example.invalid");
    assert_eq!(doc.body_authored(), "Hello café\n\nHTML details");
    assert_eq!(doc.body_quoted(), "> Previous quarter\n\nEarlier figures");
    assert_eq!(doc.body_signature(), "-- \nAndré");
    assert_eq!(doc.attachment_names(), "café.pdf\nforward.eml");
    assert_eq!(doc.state(), SearchState::Indexed);
    assert_eq!(doc.state().as_str(), "indexed");
    assert_eq!(doc.extractor_version(), 1);
    assert_eq!(SEARCH_EXTRACTOR_VERSION, 1);
    assert_eq!(doc.truncated_fields(), &[] as &[&str]);
    assert_eq!(doc.truncated_fields_json(), "[]");
    assert_eq!(document(raw), doc);
}

#[test]
fn dedup_normalizes_only_comparison_and_keeps_first_text_and_classification() {
    let raw = multipart(
        "alternative",
        &[
            "Content-Type: text/plain; charset=utf-8\r\n\r\nFirst\t café\r\ncontinued\r\n \t\r\nCase\r\n\r\nuncertain",
            "Content-Type: text/html; charset=utf-8\r\n\r\n<p>First &nbsp;café continued</p><p>case</p><blockquote>uncertain</blockquote><p>new</p>",
        ],
    );
    let doc = document(&raw);
    assert_eq!(
        doc.body_authored(),
        "First\t café\ncontinued\n\nCase\n\nuncertain\n\ncase\n\nnew"
    );
    assert_eq!(doc.body_quoted(), "");
    assert_eq!(doc.body_signature(), "");

    let reversed = multipart(
        "alternative",
        &[
            "Content-Type: text/html\r\n\r\n<blockquote>first</blockquote><p>-- <br>signature</p>",
            "Content-Type: text/plain\r\n\r\nfirst\r\n\r\n-- \r\nsignature",
        ],
    );
    let doc = document(&reversed);
    assert_eq!(doc.body_quoted(), "first");
    assert_eq!(doc.body_authored(), "-- \nsignature");
    assert_eq!(doc.body_signature(), "");
}

#[test]
fn plain_explicit_quotes_and_signature_exclude_uncertain_heuristics() {
    let doc = plain(concat!(
        "On Monday, Someone wrote:\r\n",
        " > outer\r\n\t>> nested\r\n",
        "answer\r\n\r\n",
        "Best regards,\r\nName\r\n--\r\n -- \r\n",
        "-- \r\nsigned\r\n> quoted after delimiter\r\n",
        "> -- \r\n>> nested signature\r\nsigned end"
    ));
    assert_eq!(
        doc.body_authored(),
        "On Monday, Someone wrote:\nanswer\n\nBest regards,\nName\n--\n -- "
    );
    assert_eq!(
        doc.body_quoted(),
        " > outer\n\t>> nested\n\n> quoted after delimiter\n> -- \n>> nested signature"
    );
    assert_eq!(doc.body_signature(), "-- \nsigned\nsigned end");
    assert_eq!(plain("> -- \nnormal").body_authored(), "normal");
}

#[test]
fn signature_state_is_local_to_a_candidate_and_flowed_is_decoded_first() {
    let raw = multipart(
        "alternative",
        &[
            "Content-Type: text/plain; format=flowed; delsp=yes\r\n\r\njoin \r\ned\r\n> quo \r\n> ted\r\n-- \r\nsignature",
            "Content-Type: text/plain\r\n\r\nnext candidate",
        ],
    );
    let doc = document(&raw);
    assert_eq!(doc.body_authored(), "joined\n\nnext candidate");
    assert_eq!(doc.body_quoted(), ">quoted");
    assert_eq!(doc.body_signature(), "-- \nsignature");
}

#[test]
fn html_text_nodes_boundaries_entities_and_nested_quotes() {
    let doc = html(concat!(
        "<html><head><style>STYLE-HIDDEN</style><script>SCRIPT-HIDDEN</script></head><body>",
        "<!-- COMMENT-HIDDEN --><p>Hello <b>world</b> &amp; &lt;tag&gt; &fjlig; &#x80;</p>",
        "<div>line<br>next <a href='http://127.0.0.1:9/URL-HIDDEN'>link label</a></div>",
        "<img src='http://127.0.0.1:9/IMAGE-HIDDEN' alt='ALT-HIDDEN'>",
        "<blockquote>outer<blockquote>nested</blockquote>outer end</blockquote>",
        "<p>On Tuesday, Pat wrote:</p><p>-- <br>HTML signature remains authored</p>",
        "<table><tr><td>one</td><td>two</td></tr></table></body></html>"
    ));
    assert_eq!(
        doc.body_authored(),
        "Hello world & <tag> fj €\n\nline\nnext link label\n\nOn Tuesday, Pat wrote:\n\n-- \nHTML signature remains authored\n\none\n\ntwo"
    );
    assert_eq!(doc.body_quoted(), "outer\n\nnested\n\nouter end");
    assert_eq!(doc.body_signature(), "");
    assert_eq!(doc.state(), SearchState::Indexed);
}

#[test]
fn html_tree_repair_preserves_text_and_comments_do_not_merge_entities() {
    assert_eq!(html("<p>first<p>second").body_authored(), "first\n\nsecond");
    assert_eq!(
        html("<p>&not<!--x-->in; &#x<!--x-->41;</p>").body_authored(),
        "¬in; &#x41;"
    );
    assert_eq!(
        html("<script>if (a < b) '<p>hidden</p>';</script><p>kept</p><style>x < y</style>")
            .body_authored(),
        "kept"
    );
    assert_eq!(
        html("<div>one\r\ntwo\rthree</div>").body_authored(),
        "one\ntwo\nthree"
    );
}

#[test]
fn noscript_fallback_is_text_without_raw_markup() {
    let doc = html("<body><noscript><p>fallback &amp; <b>kept</b></p></noscript></body>");
    assert_eq!(doc.body_authored(), "fallback & kept");
    assert_eq!(doc.state(), SearchState::Indexed);
}

#[test]
fn dense_html_text_and_deep_markup_do_not_serialize_or_recurse() {
    let dense = "&amp;©&nbsp;".repeat(100_000);
    let doc = html(&format!("<p>{dense}</p>"));
    assert_eq!(doc.body_authored(), "&©\u{a0}".repeat(100_000));
    assert_eq!(doc.state(), SearchState::Indexed);
    let deep = format!("{}text{}", "<div>".repeat(5_000), "</div>".repeat(5_000));
    assert_eq!(html(&deep).state(), SearchState::Partial);
    let ordinary = format!("{}text{}", "<div>".repeat(64), "</div>".repeat(64));
    assert_eq!(html(&ordinary).body_authored(), "text");
}

#[test]
fn hostile_html_nesting_is_partial_instead_of_unbounded_tree_work() {
    let body = format!("{}text{}", "<div>".repeat(20_000), "</div>".repeat(20_000));
    let doc = html(&body);
    assert_eq!(doc.state(), SearchState::Partial);
    assert_eq!(
        doc.truncated_fields(),
        ["body_authored", "body_quoted", "body_signature"]
    );
}

#[test]
fn hostile_table_repair_is_partial_instead_of_quadratic_sibling_scans() {
    let body = format!("<table>{}</table>", "x<b></b>".repeat(20_000));
    let doc = html(&body);
    assert_eq!(doc.state(), SearchState::Partial);
    assert_eq!(
        doc.truncated_fields(),
        ["body_authored", "body_quoted", "body_signature"]
    );
}

#[test]
fn formatting_list_comparisons_cannot_bypass_the_html_work_budget() {
    let body = (0..9_000)
        .map(|n| format!("<b id='{n}'>"))
        .collect::<String>();
    let doc = html(&format!("{body}kept"));
    assert_eq!(doc.state(), SearchState::Partial);
    assert_eq!(
        doc.truncated_fields(),
        ["body_authored", "body_quoted", "body_signature"]
    );
}

#[test]
fn attributes_on_ignored_tags_are_still_charged() {
    let attrs = (0..2_100)
        .map(|n| format!("a{n}"))
        .collect::<Vec<_>>()
        .join(" ");
    let body = format!("<p>{}kept", format!("</span {attrs}>x").repeat(100));
    let doc = html(&body);
    assert_eq!(doc.state(), SearchState::Partial);
    assert_eq!(
        doc.truncated_fields(),
        ["body_authored", "body_quoted", "body_signature"]
    );
}

#[test]
fn an_html_work_limit_keeps_headers_names_and_other_body_candidates() {
    let hostile = format!(
        "Content-Type: text/html\r\n\r\n<table>{}</table>",
        "x<b></b>".repeat(20_000)
    );
    let raw = format!(
        "Subject: known\r\n{}",
        multipart(
            "mixed",
            &[
                &hostile,
                "Content-Type: text/plain\r\n\r\nretained",
                "Content-Type: application/pdf; name=keep.pdf\r\n\r\nignored",
            ]
        )
    );
    let doc = document(&raw);
    assert_eq!(doc.subject(), "known");
    assert_eq!(doc.body_authored(), "retained");
    assert_eq!(doc.attachment_names(), "keep.pdf");
    assert_eq!(doc.state(), SearchState::Partial);
    assert_eq!(
        doc.truncated_fields(),
        ["body_authored", "body_quoted", "body_signature"]
    );
}

#[test]
fn missing_fields_and_attachment_only_messages_are_successful() {
    let doc = plain("text");
    assert_eq!(
        (&doc.columns[..4], doc.attachment_names()),
        (
            &[String::new(), String::new(), String::new(), String::new()][..],
            ""
        )
    );
    let doc = document("Content-Type: text/plain; name=only.txt\r\n\r\nhidden");
    assert_eq!(doc.attachment_names(), "only.txt");
    assert_eq!(doc.body_authored(), "");
    assert_eq!(doc.state(), SearchState::Indexed);
}

#[test]
fn decimal_document_cap_exact_and_multibyte_cut() {
    let exact = plain(&"é".repeat(MAX_DOCUMENT_BYTES / 2));
    assert_eq!(exact.body_authored().len(), 1_000_000);
    assert_eq!(exact.state(), SearchState::Indexed);
    let raw = format!(
        "Subject: a\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{}",
        "é".repeat(500_000)
    );
    let doc = document(&raw);
    assert_eq!(doc.subject(), "a");
    assert_eq!(doc.body_authored(), "é".repeat(499_999));
    assert_eq!(doc.state(), SearchState::Partial);
    assert_eq!(doc.truncated_fields(), ["body_authored"]);
    assert_eq!(doc.truncated_fields_json(), "[\"body_authored\"]");
    assert_eq!(doc.state().as_str(), "partial");
}

#[test]
fn schema_order_spends_budget_and_marks_every_nonempty_starved_column() {
    // Exercise every column as the cutoff, including headers beyond their
    // MIME header admission budget, directly at the allocation boundary.
    for cutoff in 0..8 {
        let mut columns = std::array::from_fn(|_| "x".to_owned());
        let budget = if cutoff == 7 {
            MAX_NAME_BYTES
        } else {
            MAX_DOCUMENT_BYTES - cutoff
        };
        columns[cutoff] = "é".repeat(budget / 2 + 1);
        let doc = cap(columns.clone(), Vec::new());
        assert_eq!(&doc.columns[..cutoff], &columns[..cutoff]);
        assert_eq!(doc.columns[cutoff].len(), budget / 2 * 2);
        let mut expected = vec![COLUMNS[cutoff]];
        // A character-safe cut can leave one byte for the next field.
        let mut remaining = MAX_DOCUMENT_BYTES - cutoff - doc.columns[cutoff].len();
        for (index, name) in COLUMNS.iter().enumerate().skip(cutoff + 1) {
            if remaining == 0 {
                expected.push(name);
                assert!(doc.columns[index].is_empty());
            } else {
                assert_eq!(doc.columns[index], "x");
                remaining -= 1;
            }
        }
        expected.sort_unstable();
        assert_eq!(doc.truncated_fields(), expected);
    }
    let mut columns: [String; 8] = Default::default();
    columns[0] = "x".repeat(MAX_DOCUMENT_BYTES);
    let doc = cap(columns, Vec::new());
    assert_eq!(doc.state(), SearchState::Indexed); // Empty fields lose no text.
}

#[test]
fn public_extraction_marks_starved_quotes_signatures_and_names() {
    let body = format!(
        "{}\n\n> quoted\n\n-- \nsigned",
        "x".repeat(MAX_DOCUMENT_BYTES)
    );
    let raw = multipart(
        "mixed",
        &[
            &format!("Content-Type: text/plain\r\n\r\n{body}"),
            "Content-Type: application/pdf; name=file.pdf\r\n\r\nignored",
        ],
    );
    let doc = document(&raw);
    assert_eq!(doc.body_authored().len(), MAX_DOCUMENT_BYTES);
    assert_eq!(doc.body_quoted(), "");
    assert_eq!(doc.body_signature(), "");
    assert_eq!(doc.attachment_names(), "");
    assert_eq!(
        doc.truncated_fields(),
        ["attachment_names", "body_quoted", "body_signature"]
    );
    assert_eq!(
        doc.truncated_fields_json(),
        "[\"attachment_names\",\"body_quoted\",\"body_signature\"]"
    );
}

#[test]
fn attachment_name_cap_counts_combined_names_and_newlines_in_utf8() {
    for (size, state) in [
        (64_000, SearchState::Indexed),
        (64_001, SearchState::Partial),
    ] {
        let raw = format!(
            "Content-Type: text/plain; name=\"{}\"\r\n\r\nhidden",
            "x".repeat(size)
        );
        let doc = document(&raw);
        assert_eq!(doc.attachment_names().len(), 64_000);
        assert_eq!(doc.state(), state);
    }
    let raw = multipart(
        "mixed",
        &[
            &format!(
                "Content-Type: text/plain; name=\"{}\"\r\n\r\nhidden",
                "x".repeat(63_998)
            ),
            "Content-Type: text/plain; name*=utf-8''%C3%A9.txt\r\n\r\nhidden",
        ],
    );
    let doc = document(&raw);
    assert_eq!(doc.attachment_names(), format!("{}\n", "x".repeat(63_998)));
    assert_eq!(doc.truncated_fields(), ["attachment_names"]);
}

#[test]
fn failures_have_empty_columns_and_partial_input_has_conservative_mask() {
    for bytes in [b"".as_slice(), b"X-SbMime-Tripwire: panic\r\n\r\nx"] {
        let doc = search_document(bytes, &Limits::default());
        assert_eq!(doc.state(), SearchState::Failed);
        assert_eq!(doc.state().as_str(), "failed");
        assert!(doc.columns.iter().all(String::is_empty));
        assert!(doc.truncated_fields().is_empty());
        assert_eq!(doc.extractor_version(), 1);
    }
    let raw = multipart(
        "alternative",
        &[
            "Content-Type: text/plain\r\n\r\nretained",
            "Content-Type: text/plain\r\nContent-Transfer-Encoding: base64\r\n\r\n!!!!",
        ],
    );
    let doc = document(&raw);
    assert_eq!(doc.body_authored(), "retained");
    assert_eq!(doc.state(), SearchState::Partial);
    let mut expected = COLUMNS;
    expected.sort_unstable();
    assert_eq!(doc.truncated_fields(), expected);
    for limits in [
        Limits::default().with_max_message_bytes(3),
        Limits::default().with_max_depth(1),
        Limits::default().with_max_parts(1),
        Limits::default().with_max_decoded_part_bytes(3),
        Limits::default().with_max_header_bytes(10),
        Limits::default().with_max_header_fields(0),
    ] {
        let doc = search_document(raw.as_bytes(), &limits);
        assert_eq!(doc.state(), SearchState::Partial);
        assert_eq!(doc.truncated_fields(), expected);
        assert!(doc.columns.iter().all(String::is_empty));
    }
    let doc = cap(
        Default::default(),
        vec!["subject", "subject", "attachment_names"],
    );
    assert_eq!(doc.truncated_fields(), ["attachment_names", "subject"]);
}

#[test]
fn html_tree_sink_retains_reparented_foreign_and_template_text() {
    for (source, expected) in [
        (
            "<table>before<tr><td>cell</td></tr>after</table>end",
            "beforeafter\n\ncell\n\nend",
        ),
        ("<p><b>bold<i>both</b>italic</i>tail", "boldbothitalictail"),
        ("<b>one<p>two</b>three", "one\n\ntwothree"),
        (
            "<svg><![CDATA[a & b]]><foreignObject><p>foreign</p></foreignObject></svg>",
            "a & b\n\nforeign",
        ),
        (
            "<math><annotation-xml encoding='text/html'><p>integrated</p></annotation-xml></math>",
            "integrated",
        ),
        (
            "<template><p>template text</p><script>hidden</script></template><p>body text</p>",
            "template text\n\nbody text",
        ),
    ] {
        let doc = html(source);
        assert_eq!(doc.state(), SearchState::Indexed, "{source}");
        assert_eq!(doc.body_authored(), expected, "{source}");
        assert_eq!(doc.body_quoted(), "", "{source}");
    }
}

#[test]
fn lossy_charset_decoding_marks_body_fields_but_literal_replacement_does_not() {
    let doc = search_document(
        b"Subject: known\r\nContent-Type: text/plain; charset=utf-8\r\n\r\na\xffb",
        &Limits::default(),
    );
    assert_eq!(doc.subject(), "known");
    assert_eq!(doc.body_authored(), "a\u{fffd}b");
    assert_eq!(doc.state(), SearchState::Partial);
    assert_eq!(
        doc.truncated_fields(),
        ["body_authored", "body_quoted", "body_signature"]
    );
    let literal = plain("a\u{fffd}b");
    assert_eq!(literal.body_authored(), doc.body_authored());
    assert_eq!(literal.state(), SearchState::Indexed);
    assert!(literal.truncated_fields().is_empty());
}

#[test]
fn repeated_paragraphs_within_a_candidate_survive_cross_candidate_dedup() {
    let raw = multipart(
        "alternative",
        &[
            "Content-Type: text/plain\r\n\r\nrepeat\r\n\r\nrepeat",
            "Content-Type: text/html\r\n\r\n<p>repeat</p><p>new</p><p>new</p>",
        ],
    );
    assert_eq!(
        document(&raw).body_authored(),
        "repeat\n\nrepeat\n\nnew\n\nnew"
    );
}

#[test]
fn additional_block_elements_separate_html_paragraphs() {
    for tag in ["search", "listing", "xmp", "legend"] {
        let source = format!("<{tag}>one</{tag}><{tag}>two</{tag}>");
        assert_eq!(html(&source).body_authored(), "one\n\ntwo", "{tag}");
    }
    assert_eq!(html("one<plaintext>two").body_authored(), "one\n\ntwo");
}

/// The pure size refusal is exactly the document a real MIME size
/// refusal produces: `Partial`, empty columns, all eight fields masked.
#[test]
fn size_refused_matches_a_real_mime_size_refusal() {
    let raw = "Subject: s\r\nContent-Type: text/plain\r\n\r\nbody";
    let refused = search_document(raw.as_bytes(), &Limits::default().with_max_message_bytes(4));
    let doc = SearchDocument::size_refused();
    assert_eq!(doc, refused);
    assert_eq!(doc.state(), SearchState::Partial);
    let mut expected = COLUMNS;
    expected.sort_unstable();
    assert_eq!(doc.truncated_fields(), expected);
    assert!(doc.columns.iter().all(String::is_empty));
    assert_eq!(doc.extractor_version(), SEARCH_EXTRACTOR_VERSION);
}
