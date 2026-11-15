use super::*;
use crate::{Anchor, CoreConfig, ImportJobState, ImportTarget, WindowSpec, open_core};
use sb_test_support::{ScratchDir, block_on};
use std::fs;
use std::path::Path;
use std::time::{Duration, Instant};

const PLAIN: &str =
    "Subject: plain\nContent-Type: text/plain\n\nHello world\n\n- first\n- second\n";
const TABLE: &str =
    "Subject: table\nContent-Type: text/html\n\n<table><tr><td>a</td></tr></table>\n";
const HTML: &str = "Subject: html\nContent-Type: text/html\n\n\
    <h2>Hello</h2>\
    <p>Text with <strong>bold</strong>, <em>italic</em> and a \
    <a href=\"https://example.invalid/page\">link</a>.<br>next</p>\
    <ul><li>one</li><li>two<ol start=\"3\"><li>nested</li></ol></li></ul>\
    <blockquote><p>quoted</p></blockquote>\
    <pre>code line</pre>\
    <p><img src=\"https://example.invalid/remote.png\" alt=\"Remote\"></p>\n";

fn wait(mut condition: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(60);
    while !condition() {
        assert!(Instant::now() < end, "reading document test timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// An imported archive with one plain and one table message, and their
/// message ids by subject.
fn imported(root: &Path, cache: &Path) -> (Core, Vec<(String, i64)>) {
    let core = block_on(open_core(CoreConfig {
        data_root: root.into(),
        cache_root: cache.into(),
    }))
    .unwrap();
    let mbox = root.parent().unwrap().join("reading.mbox");
    fs::write(
        &mbox,
        [PLAIN, TABLE, HTML]
            .iter()
            .map(|body| format!("From sender@example.test Sat Oct 17 12:00:00 2026\n{body}"))
            .collect::<String>(),
    )
    .unwrap();
    let job = block_on(core.start_import(
        &core.new_idempotency_key(),
        vec![mbox],
        ImportTarget::LocalArchive,
    ))
    .unwrap();
    wait(|| {
        block_on(core.import_job(&job.job_id)).unwrap().state
            == ImportJobState::AwaitingConfirmation
    });
    block_on(core.confirm_import(&core.new_idempotency_key(), &job.job_id, vec![])).unwrap();
    wait(|| block_on(core.import_job(&job.job_id)).unwrap().state == ImportJobState::Completed);
    let accounts = block_on(core.accounts()).unwrap();
    let mailboxes = block_on(core.mailboxes(&accounts[0].account_id)).unwrap();
    let mailbox = mailboxes
        .mailboxes
        .iter()
        .find(|m| m.name == "reading")
        .unwrap();
    let window =
        block_on(core.open_window(WindowSpec::Mailbox(mailbox.mailbox_id), Anchor::Top, 10))
            .unwrap();
    let rows = window
        .rows
        .into_iter()
        .map(|row| (row.subject, row.message_id))
        .collect();
    block_on(core.close_window(window.window_id));
    (core, rows)
}

#[test]
fn plain_text_round_trips_and_a_table_is_unsupported() {
    let scratch = ScratchDir::new("reading-document");
    let (core, rows) = imported(&scratch.path().join("data"), &scratch.path().join("cache"));
    let id = |subject: &str| {
        rows.iter()
            .find(|(s, _)| s == subject)
            .map(|(_, id)| *id)
            .expect("the imported message")
    };

    let ReadingSupport::Supported(document) = block_on(core.reading_document(id("plain"))).unwrap()
    else {
        panic!("plain personal mail is supported");
    };
    assert_eq!(document.source_part_id, "1");
    assert_eq!(document.version, sb_engine::DOCUMENT_VERSION);
    assert_eq!(document.blocks.len(), 2);
    let ReadingBlock::Paragraph(paragraph) = &document.blocks[0] else {
        panic!("expected a paragraph: {:?}", document.blocks[0]);
    };
    assert_eq!(paragraph.part_id, "1");
    assert_eq!(
        paragraph.inlines,
        vec![ReadingInline::Text(ReadingText {
            part_id: "1".into(),
            text: "Hello world".into(),
        })]
    );
    let ReadingBlock::List(list) = &document.blocks[1] else {
        panic!("expected a list: {:?}", document.blocks[1]);
    };
    assert!(!list.ordered);
    let item_text = |item: &ReadingListItem| match &item.blocks[0] {
        ReadingBlock::Paragraph(paragraph) => match &paragraph.inlines[0] {
            ReadingInline::Text(text) => text.text.clone(),
            other => panic!("expected text: {other:?}"),
        },
        other => panic!("expected a paragraph: {other:?}"),
    };
    assert_eq!(item_text(&list.items[0]), "first");
    assert_eq!(item_text(&list.items[1]), "second");

    assert_eq!(
        block_on(core.reading_document(id("table"))).unwrap(),
        ReadingSupport::Unsupported
    );
}

/// The DTOs mirror every block and inline kind the model represents.
#[test]
fn html_nodes_map_to_the_dto_tree() {
    let scratch = ScratchDir::new("reading-document-html");
    let (core, rows) = imported(&scratch.path().join("data"), &scratch.path().join("cache"));
    let id = rows
        .iter()
        .find(|(s, _)| s == "html")
        .map(|(_, id)| *id)
        .expect("the imported message");

    let ReadingSupport::Supported(document) = block_on(core.reading_document(id)).unwrap() else {
        panic!("simple HTML is supported");
    };
    assert_eq!(document.blocks.len(), 6);

    let ReadingBlock::Heading(heading) = &document.blocks[0] else {
        panic!("expected a heading: {:?}", document.blocks[0]);
    };
    assert_eq!(heading.level, ReadingHeadingLevel::H2);
    assert!(matches!(&heading.inlines[0], ReadingInline::Text(t) if t.text == "Hello"));

    let ReadingBlock::Paragraph(paragraph) = &document.blocks[1] else {
        panic!("expected a paragraph: {:?}", document.blocks[1]);
    };
    assert!(matches!(&paragraph.inlines[1], ReadingInline::Emphasis(e) if e.strong));
    assert!(matches!(&paragraph.inlines[3], ReadingInline::Emphasis(e) if !e.strong));
    assert!(matches!(&paragraph.inlines[5],
        ReadingInline::Link(link) if link.destination == "https://example.invalid/page"));
    assert!(
        paragraph
            .inlines
            .iter()
            .any(|inline| matches!(inline, ReadingInline::LineBreak(_)))
    );

    let ReadingBlock::List(list) = &document.blocks[2] else {
        panic!("expected a list: {:?}", document.blocks[2]);
    };
    assert!(!list.ordered);
    assert_eq!(list.items.len(), 2);
    let ReadingBlock::List(nested) = &list.items[1].blocks[1] else {
        panic!("expected a nested list: {:?}", list.items[1].blocks[1]);
    };
    assert!(nested.ordered);
    assert_eq!(nested.start, 3);

    assert!(matches!(&document.blocks[3], ReadingBlock::Quote(quote) if quote.blocks.len() == 1));
    assert!(matches!(&document.blocks[4],
        ReadingBlock::Preformatted(pre) if pre.text == "code line"));
    let ReadingBlock::Paragraph(last) = &document.blocks[5] else {
        panic!("expected a paragraph: {:?}", document.blocks[5]);
    };
    assert!(matches!(&last.inlines[0],
        ReadingInline::Image(image)
            if image.alt == "Remote"
                && matches!(image.reference, ReadingImageReference::Remote { ref url }
                    if url == "https://example.invalid/remote.png")));
}

#[test]
fn unknown_message_is_not_found() {
    let scratch = ScratchDir::new("reading-document-missing");
    let (core, _) = imported(&scratch.path().join("data"), &scratch.path().join("cache"));
    assert!(matches!(
        block_on(core.reading_document(-1)),
        Err(CoreError::NotFound { .. })
    ));
}
