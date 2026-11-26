//! Synthetic document fixtures, Unlicense OR 0BSD. No real sender data.
use super::*;

macro_rules! block {
    ($blocks:expr, $index:expr, $variant:ident) => {
        match &$blocks[$index] {
            Block::$variant(value) => value,
            other => panic!("expected {} at {}: {other:?}", stringify!($variant), $index),
        }
    };
}

macro_rules! inline {
    ($inlines:expr, $index:expr, $variant:ident) => {
        match &$inlines[$index] {
            Inline::$variant(value) => value,
            other => panic!("expected {} at {}: {other:?}", stringify!($variant), $index),
        }
    };
}

fn document(raw: &str) -> DocumentSupport {
    mail_document(raw.as_bytes(), &Limits::default()).unwrap()
}

fn supported(raw: &str) -> MailDocument {
    match document(raw) {
        DocumentSupport::Supported(document) => document,
        DocumentSupport::Unsupported => panic!("expected a supported document: {raw:?}"),
    }
}

fn plain(body: &str) -> String {
    format!("From: sender@example.invalid\r\nContent-Type: text/plain; charset=utf-8\r\n\r\n{body}")
}

fn html(body: &str) -> String {
    format!("From: sender@example.invalid\r\nContent-Type: text/html; charset=utf-8\r\n\r\n{body}")
}

fn text_of(inline: &Inline) -> &str {
    match inline {
        Inline::Text(text) => text.text(),
        other => panic!("expected text, got {other:?}"),
    }
}

fn assert_all_parts(blocks: &[Block], part: &str) {
    for block in blocks {
        match block {
            Block::Paragraph(paragraph) => {
                assert_eq!(paragraph.part().as_str(), part);
                for inline in paragraph.inlines() {
                    assert_inline_part(inline, part);
                }
            }
            Block::Heading(heading) => {
                assert_eq!(heading.part().as_str(), part);
                for inline in heading.inlines() {
                    assert_inline_part(inline, part);
                }
            }
            Block::Quote(quote) => {
                assert_eq!(quote.part().as_str(), part);
                assert_all_parts(quote.blocks(), part);
            }
            Block::List(list) => {
                assert_eq!(list.part().as_str(), part);
                for item in list.items() {
                    assert_eq!(item.part().as_str(), part);
                    assert_all_parts(item.blocks(), part);
                }
            }
            Block::Preformatted(preformatted) => {
                assert_eq!(preformatted.part().as_str(), part);
            }
        }
    }
}

fn assert_inline_part(inline: &Inline, part: &str) {
    match inline {
        Inline::Text(text) => assert_eq!(text.part().as_str(), part),
        Inline::Emphasis(emphasis) => {
            assert_eq!(emphasis.part().as_str(), part);
            for inline in emphasis.inlines() {
                assert_inline_part(inline, part);
            }
        }
        Inline::Link(link) => {
            assert_eq!(link.part().as_str(), part);
            for inline in link.inlines() {
                assert_inline_part(inline, part);
            }
        }
        Inline::Image(image) => assert_eq!(image.part().as_str(), part),
        Inline::LineBreak(line_break) => assert_eq!(line_break.part().as_str(), part),
    }
}

#[test]
fn plain_text_quotes_and_lists_round_trip() {
    let document = supported(&plain(
        "Hello world\n\n> quoted line\n>> nested line\n\n- first\n- second\n",
    ));
    assert_eq!(document.version(), DOCUMENT_VERSION);
    assert_eq!(document.source().as_str(), "1");
    let blocks = document.blocks();
    assert_eq!(blocks.len(), 3);
    assert_eq!(
        text_of(&block!(blocks, 0, Paragraph).inlines()[0]),
        "Hello world"
    );

    let outer = block!(blocks, 1, Quote);
    assert_eq!(outer.blocks().len(), 2);
    assert_eq!(
        text_of(&block!(outer.blocks(), 0, Paragraph).inlines()[0]),
        "quoted line"
    );
    let nested = block!(outer.blocks(), 1, Quote);
    assert_eq!(nested.blocks().len(), 1);
    assert_eq!(
        text_of(&block!(nested.blocks(), 0, Paragraph).inlines()[0]),
        "nested line"
    );

    let list = block!(blocks, 2, List);
    assert!(!list.ordered());
    assert_eq!(list.start(), 1);
    assert!(!list.reversed());
    let items: Vec<_> = list
        .items()
        .iter()
        .map(|item| text_of(&block!(item.blocks(), 0, Paragraph).inlines()[0]))
        .collect();
    assert_eq!(items, ["first", "second"]);
    assert_all_parts(blocks, "1");
}

#[test]
fn simple_html_round_trips() {
    let body = "<h2>Hello</h2>\
        <p>Text with <strong>bold</strong>, <em>italic</em> and a \
        <a href=\"https://example.invalid/page\">link</a>.</p>\
        <ul><li>one</li><li>two<ol start=\"3\"><li>nested</li></ol></li></ul>\
        <blockquote><p>quoted</p></blockquote>";
    let document = supported(&html(body));
    assert_eq!(document.source().as_str(), "1");
    let blocks = document.blocks();
    assert_eq!(blocks.len(), 4);

    let heading = block!(blocks, 0, Heading);
    assert_eq!(heading.level(), HeadingLevel::H2);
    assert_eq!(text_of(&heading.inlines()[0]), "Hello");

    let paragraph = block!(blocks, 1, Paragraph);
    assert_eq!(paragraph.inlines().len(), 7);
    assert_eq!(text_of(&paragraph.inlines()[0]), "Text with ");
    assert!(inline!(paragraph.inlines(), 1, Emphasis).strong());
    assert_eq!(
        text_of(&inline!(paragraph.inlines(), 1, Emphasis).inlines()[0]),
        "bold"
    );
    assert!(!inline!(paragraph.inlines(), 3, Emphasis).strong());
    assert_eq!(
        text_of(&inline!(paragraph.inlines(), 3, Emphasis).inlines()[0]),
        "italic"
    );
    assert_eq!(
        inline!(paragraph.inlines(), 5, Link).destination(),
        "https://example.invalid/page"
    );
    assert_eq!(
        text_of(&inline!(paragraph.inlines(), 5, Link).inlines()[0]),
        "link"
    );

    let list = block!(blocks, 2, List);
    assert!(!list.ordered());
    assert_eq!(list.items().len(), 2);
    let nested = block!(list.items()[1].blocks(), 1, List);
    assert!(nested.ordered());
    assert_eq!(nested.start(), 3);
    assert_eq!(nested.items().len(), 1);
    assert_eq!(block!(blocks, 3, Quote).blocks().len(), 1);
    assert_all_parts(blocks, "1");
}

#[test]
fn images_are_references_with_source_parts() {
    let raw = "From: sender@example.invalid\r\n\
        Content-Type: multipart/related; boundary=b\r\n\r\n\
        --b\r\nContent-Type: text/html\r\n\r\n\
        <p><img src=\"cid:pic@example.invalid\" alt=\"Picture\">\
        <img src=\"https://example.invalid/remote.png\" alt=\"Remote\"></p>\r\n\
        --b\r\nContent-Type: image/png\r\nContent-ID: <pic@example.invalid>\r\n\
        Content-Transfer-Encoding: base64\r\n\r\n\
        iVBORw0KGgo=\r\n--b--\r\n";
    let document = supported(raw);
    let paragraph = block!(document.blocks(), 0, Paragraph);
    assert_eq!(paragraph.inlines().len(), 2);

    let cid = inline!(paragraph.inlines(), 0, Image);
    assert_eq!(cid.alt(), "Picture");
    assert_eq!(cid.part().as_str(), "1.1");
    let ImageReference::Cid {
        content_id,
        part_id,
    } = cid.reference()
    else {
        panic!("cid reference")
    };
    assert_eq!(content_id, "pic@example.invalid");
    assert_eq!(part_id.as_str(), "1.2");

    let remote = inline!(paragraph.inlines(), 1, Image);
    assert_eq!(remote.alt(), "Remote");
    let ImageReference::Remote { url } = remote.reference() else {
        panic!("remote reference")
    };
    assert_eq!(url, "https://example.invalid/remote.png");
    assert_all_parts(document.blocks(), "1.1");
}

#[test]
fn tables_receipts_and_complex_layout_are_unsupported() {
    let receipt = "<p>Thank you for your order.</p>\n\
        <table><tr><td>Item</td><td>19.99</td></tr>\
        <tr><td>Total</td><td>19.99</td></tr></table>";
    let newsletter = "<table role=\"presentation\" width=\"600\"><tr>\
        <td style=\"padding:10px\"><table width=\"100%\"><tr>\
        <td><h1>Offer</h1><p>Buy now</p></td></tr></table></td></tr></table>";
    for body in [
        receipt,
        newsletter,
        "<div style=\"position:absolute\">positioned</div>",
        "<script>alert(1)</script><p>text</p>",
        "<form action=\"https://example.invalid\"><input></form>",
        "<iframe src=\"https://example.invalid\"></iframe>",
        "<p>text</p><hr>",
        "<p><code>code</code></p>",
        "<p class=\"x\">classed</p>",
        "<p align=\"center\">aligned</p>",
    ] {
        assert_eq!(
            document(&html(body)),
            DocumentSupport::Unsupported,
            "{body}"
        );
    }
}

#[test]
fn dangling_and_unsafe_image_references_are_unsupported() {
    for body in [
        "<p><img src=\"cid:missing@example.invalid\"></p>",
        "<p><img src=\"data:text/html,bad\"></p>",
        "<p><img src=\"javascript:alert(1)\"></p>",
    ] {
        assert_eq!(
            document(&html(body)),
            DocumentSupport::Unsupported,
            "{body}"
        );
    }
}

#[test]
fn preformatted_and_explicit_breaks() {
    let document = supported(&html("<pre>line one\nline two</pre><p>a<br>b</p>"));
    assert_eq!(
        block!(document.blocks(), 0, Preformatted).text(),
        "line one\nline two"
    );
    let paragraph = block!(document.blocks(), 1, Paragraph);
    assert_eq!(text_of(&paragraph.inlines()[0]), "a");
    assert!(matches!(paragraph.inlines()[1], Inline::LineBreak(_)));
    assert_eq!(text_of(&paragraph.inlines()[2]), "b");
}

#[test]
fn transparent_containers_and_entities() {
    let document = supported(&html(
        "<html><body><div><p>a</p><p>b</p></div><p>c <span>d</span> e &amp; &#65;</p></body></html>",
    ));
    assert_eq!(document.blocks().len(), 3);
    let texts: String = block!(document.blocks(), 2, Paragraph)
        .inlines()
        .iter()
        .filter_map(|inline| match inline {
            Inline::Text(text) => Some(text.text()),
            _ => None,
        })
        .collect();
    assert_eq!(texts, "c d e & A");
}

#[test]
fn html_is_preferred_over_plain_and_keeps_its_part() {
    let raw = "From: sender@example.invalid\r\n\
        Content-Type: multipart/alternative; boundary=b\r\n\r\n\
        --b\r\nContent-Type: text/plain\r\n\r\nplain\r\n\
        --b\r\nContent-Type: text/html\r\n\r\n<p>html</p>\r\n--b--\r\n";
    let document = supported(raw);
    assert_eq!(document.source().as_str(), "1.2");
    assert_all_parts(document.blocks(), "1.2");
}

#[test]
fn flowed_plain_text_is_decoded_before_blocks() {
    let raw = "From: sender@example.invalid\r\n\
        Content-Type: text/plain; format=flowed\r\n\r\n\
        Hello \r\nworld\r\n> quoted\r\n";
    let document = supported(raw);
    assert_eq!(
        text_of(&block!(document.blocks(), 0, Paragraph).inlines()[0]),
        "Hello world"
    );
    let quote = block!(document.blocks(), 1, Quote);
    assert_eq!(
        text_of(&block!(quote.blocks(), 0, Paragraph).inlines()[0]),
        "quoted"
    );
}

#[test]
fn unsafe_link_destinations_are_dropped_by_the_sanitizer() {
    let document = supported(&html("<p><a href=\"javascript:alert(1)\">label</a></p>"));
    let paragraph = block!(document.blocks(), 0, Paragraph);
    assert_eq!(paragraph.inlines().len(), 1);
    assert_eq!(text_of(&paragraph.inlines()[0]), "label");
}

#[test]
fn hostile_plain_quote_and_bullet_runs_are_unsupported() {
    let quotes = format!("{}x", "> ".repeat(20_000));
    let bullets = format!("{}x", "- ".repeat(20_000));
    for body in [quotes, bullets] {
        assert_eq!(
            document(&plain(&body)),
            DocumentSupport::Unsupported,
            "leading {}",
            if body.starts_with('>') { '>' } else { '-' }
        );
    }
}

#[test]
fn crlf_plain_body_keeps_one_paragraph() {
    // render_input folds CRLF before plain text reaches the document.
    let raw = plain("first\r\nsecond");
    let input = crate::render_input(raw.as_bytes(), &Limits::default()).unwrap();
    assert_eq!(input.plain().expect("plain body").text(), "first\nsecond");
    let part = input.plain().expect("plain body").part_id().clone();
    // A literal CRLF at this boundary must still fold to one line break.
    let blocks = super::plain::blocks("first\r\nsecond", &part).expect("literal text");
    assert_eq!(blocks.len(), 1);
    let Block::Paragraph(paragraph) = &blocks[0] else {
        panic!("one paragraph")
    };
    assert_eq!(paragraph.inlines().len(), 3);
    assert_eq!(text_of(&paragraph.inlines()[0]), "first");
    assert!(matches!(paragraph.inlines()[1], Inline::LineBreak(_)));
    assert_eq!(text_of(&paragraph.inlines()[2]), "second");
}

#[test]
fn dense_html_nodes_over_budget_are_unsupported() {
    let body = "<br>".repeat(super::html::MAX_NODES + 1);
    assert_eq!(document(&html(&body)), DocumentSupport::Unsupported);
}

#[test]
fn image_src_entities_decode_exactly_once() {
    let document = supported(&html(
        "<p><img src=\"https://example.invalid/a?x=&amp;amp;\"></p>",
    ));
    let paragraph = block!(document.blocks(), 0, Paragraph);
    let image = inline!(paragraph.inlines(), 0, Image);
    let ImageReference::Remote { url } = image.reference() else {
        panic!("remote reference")
    };
    assert_eq!(url, "https://example.invalid/a?x=&amp;");
}

#[test]
fn invalid_list_numbering_attributes_use_defaults() {
    let document = supported(&html(
        "<ol start=\"x\"><li>a</li><li value=\"y\">b</li></ol>",
    ));
    let list = block!(document.blocks(), 0, List);
    assert!(list.ordered());
    assert_eq!(list.start(), 1);
    assert_eq!(list.items().len(), 2);
    assert_eq!(list.items()[0].number(), None);
    assert_eq!(list.items()[1].number(), None);
}

#[test]
fn reversed_lists_default_to_their_item_count() {
    for source in [
        "<ol reversed><li>a</li><li>b</li><li>c</li></ol>",
        "<ol reversed start=\"x\"><li>a</li><li>b</li><li>c</li></ol>",
    ] {
        let document = supported(&html(source));
        let list = block!(document.blocks(), 0, List);
        assert!(list.ordered());
        assert!(list.reversed());
        assert_eq!(list.start(), 3, "{source}");
    }

    let document = supported(&html(
        "<ol reversed start=\"8\"><li>a</li><li>b</li><li>c</li></ol>",
    ));
    let list = block!(document.blocks(), 0, List);
    assert_eq!(list.start(), 8);
}

#[test]
fn empty_bodies_are_unsupported() {
    for raw in [
        "From: sender@example.invalid\r\nContent-Type: text/plain\r\n\r\n",
        "From: sender@example.invalid\r\nContent-Type: text/html\r\n\r\n   ",
    ] {
        assert_eq!(document(raw), DocumentSupport::Unsupported, "{raw}");
    }
}
