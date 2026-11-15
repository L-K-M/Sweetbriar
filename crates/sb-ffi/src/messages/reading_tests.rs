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

/// Every document field, nested block and nested inline survives the
/// layer boundary, including the recursive quote/list and emphasis/link
/// trees.
#[test]
fn a_supported_document_maps_field_for_field() {
    let text = |part_id: &str, text: &str| {
        sb_service::ReadingInline::Text(sb_service::ReadingText {
            part_id: part_id.into(),
            text: text.into(),
        })
    };
    let paragraph = |part_id: &str, inlines: Vec<sb_service::ReadingInline>| {
        sb_service::ReadingBlock::Paragraph(sb_service::ReadingParagraph {
            part_id: part_id.into(),
            inlines,
        })
    };
    let service = sb_service::ReadingSupport::Supported(sb_service::ReadingDocument {
        source_part_id: "1".into(),
        version: 7,
        blocks: vec![
            paragraph(
                "1",
                vec![
                    text("1", "Hello "),
                    sb_service::ReadingInline::Emphasis(sb_service::ReadingEmphasis {
                        part_id: "1".into(),
                        strong: true,
                        inlines: vec![text("1", "bold")],
                    }),
                    sb_service::ReadingInline::Link(sb_service::ReadingLink {
                        part_id: "1".into(),
                        destination: "https://example.invalid/page".into(),
                        inlines: vec![text("1", "link")],
                    }),
                    sb_service::ReadingInline::Image(sb_service::ReadingImage {
                        part_id: "1".into(),
                        alt: "Remote".into(),
                        reference: sb_service::ReadingImageReference::Remote {
                            url: "https://example.invalid/a.png".into(),
                        },
                    }),
                    sb_service::ReadingInline::LineBreak(sb_service::ReadingLineBreak {
                        part_id: "1".into(),
                    }),
                ],
            ),
            sb_service::ReadingBlock::Heading(sb_service::ReadingHeading {
                part_id: "1".into(),
                level: sb_service::ReadingHeadingLevel::H2,
                inlines: vec![text("1", "Heading")],
            }),
            sb_service::ReadingBlock::Quote(sb_service::ReadingQuote {
                part_id: "1".into(),
                blocks: vec![paragraph(
                    "1",
                    vec![
                        text("1", "quoted"),
                        sb_service::ReadingInline::Image(sb_service::ReadingImage {
                            part_id: "1".into(),
                            alt: "Attached".into(),
                            reference: sb_service::ReadingImageReference::Attachment {
                                content_id: "<image@example.invalid>".into(),
                                part_id: "2".into(),
                            },
                        }),
                    ],
                )],
            }),
            sb_service::ReadingBlock::List(sb_service::ReadingList {
                part_id: "1".into(),
                ordered: true,
                start: 3,
                reversed: true,
                items: vec![sb_service::ReadingListItem {
                    part_id: "1".into(),
                    number: Some(9),
                    blocks: vec![
                        paragraph("1", vec![text("1", "item")]),
                        sb_service::ReadingBlock::List(sb_service::ReadingList {
                            part_id: "1".into(),
                            ordered: false,
                            start: 1,
                            reversed: false,
                            items: vec![sb_service::ReadingListItem {
                                part_id: "1".into(),
                                number: None,
                                blocks: vec![paragraph("1", vec![text("1", "nested")])],
                            }],
                        }),
                    ],
                }],
            }),
            sb_service::ReadingBlock::Preformatted(sb_service::ReadingPreformatted {
                part_id: "1".into(),
                text: "code line".into(),
            }),
        ],
    });

    let text = |part_id: &str, text: &str| {
        ReadingInline::Text(ReadingText {
            part_id: part_id.into(),
            text: text.into(),
        })
    };
    let paragraph = |part_id: &str, inlines: Vec<ReadingInline>| {
        ReadingBlock::Paragraph(ReadingParagraph {
            part_id: part_id.into(),
            inlines,
        })
    };
    let expected = ReadingSupport::Supported(ReadingDocument {
        source_part_id: "1".into(),
        version: 7,
        blocks: vec![
            paragraph(
                "1",
                vec![
                    text("1", "Hello "),
                    ReadingInline::Emphasis(ReadingEmphasis {
                        part_id: "1".into(),
                        strong: true,
                        inlines: vec![text("1", "bold")],
                    }),
                    ReadingInline::Link(ReadingLink {
                        part_id: "1".into(),
                        destination: "https://example.invalid/page".into(),
                        inlines: vec![text("1", "link")],
                    }),
                    ReadingInline::Image(ReadingImage {
                        part_id: "1".into(),
                        alt: "Remote".into(),
                        reference: ReadingImageReference::Remote {
                            url: "https://example.invalid/a.png".into(),
                        },
                    }),
                    ReadingInline::LineBreak(ReadingLineBreak {
                        part_id: "1".into(),
                    }),
                ],
            ),
            ReadingBlock::Heading(ReadingHeading {
                part_id: "1".into(),
                level: ReadingHeadingLevel::H2,
                inlines: vec![text("1", "Heading")],
            }),
            ReadingBlock::Quote(ReadingQuote {
                part_id: "1".into(),
                blocks: vec![paragraph(
                    "1",
                    vec![
                        text("1", "quoted"),
                        ReadingInline::Image(ReadingImage {
                            part_id: "1".into(),
                            alt: "Attached".into(),
                            reference: ReadingImageReference::Attachment {
                                content_id: "<image@example.invalid>".into(),
                                part_id: "2".into(),
                            },
                        }),
                    ],
                )],
            }),
            ReadingBlock::List(ReadingList {
                part_id: "1".into(),
                ordered: true,
                start: 3,
                reversed: true,
                items: vec![ReadingListItem {
                    part_id: "1".into(),
                    number: Some(9),
                    blocks: vec![
                        paragraph("1", vec![text("1", "item")]),
                        ReadingBlock::List(ReadingList {
                            part_id: "1".into(),
                            ordered: false,
                            start: 1,
                            reversed: false,
                            items: vec![ReadingListItem {
                                part_id: "1".into(),
                                number: None,
                                blocks: vec![paragraph("1", vec![text("1", "nested")])],
                            }],
                        }),
                    ],
                }],
            }),
            ReadingBlock::Preformatted(ReadingPreformatted {
                part_id: "1".into(),
                text: "code line".into(),
            }),
        ],
    });
    assert_eq!(ReadingSupport::from_service(service), expected);
}

fn wait(mut condition: impl FnMut() -> bool) {
    let end = Instant::now() + Duration::from_secs(60);
    while !condition() {
        assert!(Instant::now() < end, "FFI reading document test timed out");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// An imported archive with one plain and one table message, and their
/// message ids in subject order.
fn imported(root: &Path, cache: &Path) -> (Arc<Core>, Vec<i64>) {
    let core = block_on(open_core(
        CancelToken::new(),
        CoreConfig {
            data_root: root.to_string_lossy().into_owned(),
            cache_root: cache.to_string_lossy().into_owned(),
        },
    ))
    .unwrap();
    let mbox = root.parent().unwrap().join("reading.mbox");
    fs::write(
        &mbox,
        [PLAIN, TABLE]
            .iter()
            .map(|body| format!("From sender@example.test Sat Oct 17 12:00:00 2026\n{body}"))
            .collect::<String>(),
    )
    .unwrap();
    let job = block_on(core.start_import(
        CancelToken::new(),
        "start".into(),
        vec![mbox.to_string_lossy().into_owned()],
        ImportTarget::LocalArchive,
    ))
    .unwrap();
    wait(|| {
        block_on(core.import_job(CancelToken::new(), job.job_id.clone()))
            .unwrap()
            .state
            == ImportJobState::AwaitingConfirmation
    });
    block_on(core.confirm_import(
        CancelToken::new(),
        "confirm".into(),
        job.job_id.clone(),
        vec![],
    ))
    .unwrap();
    wait(|| {
        block_on(core.import_job(CancelToken::new(), job.job_id.clone()))
            .unwrap()
            .state
            == ImportJobState::Completed
    });
    let accounts = block_on(core.accounts(CancelToken::new())).unwrap();
    let mailboxes =
        block_on(core.mailboxes(CancelToken::new(), accounts[0].account_id.clone())).unwrap();
    let mailbox = mailboxes
        .mailboxes
        .iter()
        .find(|m| m.name == "reading")
        .unwrap();
    let window = block_on(core.open_window(
        CancelToken::new(),
        WindowSpec::Mailbox {
            mailbox_id: mailbox.mailbox_id,
        },
        Anchor::Top,
        10,
    ))
    .unwrap();
    let mut rows = window.rows;
    rows.sort_by(|a, b| a.subject.cmp(&b.subject));
    assert_eq!(
        rows.iter().map(|r| r.subject.as_str()).collect::<Vec<_>>(),
        ["plain", "table"]
    );
    block_on(core.close_window(window.window_id));
    (core, rows.iter().map(|r| r.message_id).collect())
}

/// A supported plain message answers its document; a table answers
/// `Unsupported`, the sender-layout fallback.
#[test]
fn plain_text_maps_and_a_table_is_unsupported() {
    let scratch = ScratchDir::new("ffi-reading-document");
    let (core, ids) = imported(&scratch.path().join("data"), &scratch.path().join("cache"));
    let ReadingSupport::Supported(document) =
        block_on(core.reading_document(CancelToken::new(), ids[0])).unwrap()
    else {
        panic!("plain personal mail is supported");
    };
    assert_eq!(document.source_part_id, "1");
    assert_eq!(document.blocks.len(), 2);
    let ReadingBlock::Paragraph(paragraph) = &document.blocks[0] else {
        panic!("expected a paragraph: {:?}", document.blocks[0]);
    };
    assert_eq!(
        paragraph.inlines,
        vec![ReadingInline::Text(ReadingText {
            part_id: "1".into(),
            text: "Hello world".into(),
        })]
    );
    assert!(matches!(&document.blocks[1], ReadingBlock::List(list) if !list.ordered));
    assert_eq!(
        block_on(core.reading_document(CancelToken::new(), ids[1])).unwrap(),
        ReadingSupport::Unsupported
    );
    block_on(core.shutdown());
}

/// An unknown message is `NotFound`, as the reader body query answers
/// it.
#[test]
fn unknown_message_is_not_found() {
    let scratch = ScratchDir::new("ffi-reading-document-missing");
    let (core, _) = imported(&scratch.path().join("data"), &scratch.path().join("cache"));
    assert!(matches!(
        block_on(core.reading_document(CancelToken::new(), -1)),
        Err(CoreError::NotFound { .. })
    ));
    block_on(core.shutdown());
}
