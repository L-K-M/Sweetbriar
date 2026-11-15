use super::*;

/// Every field, optional value and recorded availability survives the layer boundary.
#[test]
fn a_message_maps_field_for_field() {
    use sb_service::BodyAvailability as S;
    let states = [
        (S::NotAcquired, BodyAvailability::NotAcquired),
        (S::Acquired, BodyAvailability::Acquired),
        (S::AcquisitionMissing, BodyAvailability::AcquisitionMissing),
        (S::AcquisitionCorrupt, BodyAvailability::AcquisitionCorrupt),
    ];
    for (source, expected) in states {
        let address = |name: &str, email: &str| sb_service::ReaderAddress {
            name: Some(name.into()),
            address: Some(email.into()),
        };
        let message = ReaderMessage::from_service(sb_service::ReaderMessage {
            message_id: 101,
            headers: sb_service::ReaderHeaders {
                from: vec![
                    address("From", "from@example.test"),
                    sb_service::ReaderAddress {
                        name: None,
                        address: None,
                    },
                ],
                to: vec![address("To", "to@example.test")],
                cc: vec![address("Cc", "cc@example.test")],
                date: Some(202),
                subject: Some("Subject".into()),
                message_id: Some("<header-id>".into()),
            },
            account_id: "account-id".into(),
            account_display_name: "Account label".into(),
            occurrences: vec![sb_service::ReaderOccurrence {
                occurrence_id: 303,
                mailbox_id: 404,
                mailbox_name: "Mailbox".into(),
            }],
            thread_id: Some(505),
            tags: vec![
                sb_service::ReaderTag {
                    tag_id: "tag-id".into(),
                    name: "Tag label".into(),
                    color_srgb: Some("#123456".into()),
                },
                sb_service::ReaderTag {
                    tag_id: "plain-id".into(),
                    name: "Plain".into(),
                    color_srgb: None,
                },
            ],
            note: Some("Read-only note".into()),
            body_availability: source,
        });
        let address = |name: &str, email: &str| ReaderAddress {
            name: Some(name.into()),
            address: Some(email.into()),
        };
        assert_eq!(message.message_id, 101);
        assert_eq!(message.account_id, "account-id");
        assert_eq!(message.account_display_name, "Account label");
        assert_eq!(
            message.headers.from,
            vec![
                address("From", "from@example.test"),
                ReaderAddress {
                    name: None,
                    address: None
                }
            ]
        );
        assert_eq!(message.headers.to, vec![address("To", "to@example.test")]);
        assert_eq!(message.headers.cc, vec![address("Cc", "cc@example.test")]);
        assert_eq!(message.headers.date, Some(202));
        assert_eq!(message.headers.subject.as_deref(), Some("Subject"));
        assert_eq!(message.headers.message_id.as_deref(), Some("<header-id>"));
        assert_eq!(
            message.occurrences,
            vec![ReaderOccurrence {
                occurrence_id: 303,
                mailbox_id: 404,
                mailbox_name: "Mailbox".into()
            }]
        );
        assert_eq!(message.thread_id, Some(505));
        assert_eq!(
            message.tags,
            vec![
                ReaderTag {
                    tag_id: "tag-id".into(),
                    name: "Tag label".into(),
                    color_srgb: Some("#123456".into())
                },
                ReaderTag {
                    tag_id: "plain-id".into(),
                    name: "Plain".into(),
                    color_srgb: None
                }
            ]
        );
        assert_eq!(message.note.as_deref(), Some("Read-only note"));
        assert_eq!(message.body_availability, expected);
    }
}

/// Each selection variant maps to the service's own (spec/07).
#[test]
fn selections_map_variant_for_variant() {
    assert_eq!(
        MessageSelection::OccurrenceIds(vec![1, 2]).into_service(),
        sb_service::MessageSelection::OccurrenceIds(vec![1, 2])
    );
    assert_eq!(
        MessageSelection::Window {
            window_id: 3,
            generation: 4
        }
        .into_service(),
        sb_service::MessageSelection::Window {
            window_id: 3,
            generation: 4
        }
    );
}

/// One imported message, through the exported import surface.
fn import_one(core: &Core, path: &std::path::Path) {
    use crate::ImportTarget;
    std::fs::write(
        path,
        "From sender@example.test Sat Oct 17 12:00:00 2026\n\
         Message-ID: <17@ffi.test>\n\
         Date: 17 Oct 2026 12:00:00 +0000\n\
         Subject: alpha one\n\
         Content-Type: text/plain; charset=utf-8\n\n\
         alpha alpha alpha\n",
    )
    .unwrap();
    let started = sb_test_support::block_on(core.start_import(
        CancelToken::new(),
        "20000000-0000-4000-8000-000000000001".into(),
        vec![path.to_string_lossy().into_owned()],
        ImportTarget::LocalArchive,
    ))
    .unwrap();
    wait_import(core, &started.job_id, |job| {
        job.state == crate::ImportJobState::AwaitingConfirmation
    });
    sb_test_support::block_on(core.confirm_import(
        CancelToken::new(),
        "20000000-0000-4000-8000-000000000002".into(),
        started.job_id.clone(),
        vec![],
    ))
    .unwrap();
    wait_import(core, &started.job_id, |job| {
        job.state == crate::ImportJobState::Completed
    });
}

fn wait_import(
    core: &Core,
    job_id: &str,
    done: impl Fn(&crate::ImportJob) -> bool,
) -> crate::ImportJob {
    let deadline = std::time::Instant::now() + std::time::Duration::from_secs(300);
    loop {
        let job = sb_test_support::block_on(core.import_job(CancelToken::new(), job_id.to_owned()))
            .unwrap();
        if done(&job) {
            return job;
        }
        assert!(
            std::time::Instant::now() < deadline,
            "the import never settled"
        );
        std::thread::sleep(std::time::Duration::from_millis(20));
    }
}

/// Tags and notes cross through the exported commands: explicit and
/// window selections attach and detach, a repeated idempotency key
/// answers its stored receipt, and empty text clears the note
/// (spec/07 Messages).
#[test]
fn tag_and_note_commands_round_trip_and_replay() {
    use crate::{Anchor, CoreConfig, WindowSpec, open_core};
    use sb_test_support::{ScratchDir, block_on};

    let scratch = ScratchDir::new("ffi-tags-notes");
    let core = block_on(open_core(
        CancelToken::new(),
        CoreConfig {
            data_root: scratch.path().join("store").to_string_lossy().into_owned(),
            cache_root: scratch.path().join("cache").to_string_lossy().into_owned(),
        },
    ))
    .unwrap();
    import_one(&core, &scratch.path().join("Tags.mbox"));

    let account = block_on(core.accounts(CancelToken::new())).unwrap()[0]
        .account_id
        .clone();
    let mailbox = block_on(core.mailboxes(CancelToken::new(), account))
        .unwrap()
        .mailboxes[0]
        .mailbox_id;
    let window = block_on(core.open_window(
        CancelToken::new(),
        WindowSpec::Mailbox {
            mailbox_id: mailbox,
        },
        Anchor::Top,
        10,
    ))
    .unwrap();
    assert_eq!(window.rows.len(), 1);
    let target = &window.rows[0];
    assert_eq!(target.subject, "alpha one");
    let occurrence = target.occurrence_id;
    let message_id = target.message_id;

    // An explicit occurrence selection attaches the named tag.
    let add_key = "20000000-0000-4000-8000-000000000010".to_owned();
    let added = block_on(core.add_tag(
        CancelToken::new(),
        add_key.clone(),
        MessageSelection::OccurrenceIds(vec![occurrence]),
        "work".into(),
    ))
    .unwrap();
    assert_eq!(added.command, "add_tag");
    assert!(added.result_json.contains(r#""added":1"#));
    assert_eq!(
        block_on(core.add_tag(
            CancelToken::new(),
            add_key,
            MessageSelection::OccurrenceIds(vec![occurrence]),
            "work".into(),
        ))
        .unwrap(),
        added,
        "a repeated key answers its stored receipt"
    );
    let read = block_on(core.get_message(CancelToken::new(), message_id)).unwrap();
    assert_eq!(read.tags.len(), 1);
    assert_eq!(read.tags[0].name, "work");

    // A window token names the open window's rows at its generation.
    // The tag above (and the import's follow-up work) bump the list
    // generation asynchronously, so the shell's current generation is
    // read first and a token a bump overtook is retried, as the shell
    // would after `ResetRequired`.
    let mut attempt = 0;
    let later = loop {
        attempt += 1;
        let generation = block_on(core.window_ids(CancelToken::new(), window.window_id))
            .unwrap()
            .generation;
        let window_key = format!("20000000-0000-4000-8000-0000000001{attempt:02}");
        match block_on(core.add_tag(
            CancelToken::new(),
            window_key,
            MessageSelection::Window {
                window_id: window.window_id,
                generation,
            },
            "later".into(),
        )) {
            Err(CoreError::ResetRequired { .. }) if attempt < 50 => {
                std::thread::sleep(std::time::Duration::from_millis(20));
            }
            result => break result.unwrap(),
        }
    };
    assert!(later.result_json.contains(r#""added":1"#));
    let read = block_on(core.get_message(CancelToken::new(), message_id)).unwrap();
    let mut names = read
        .tags
        .iter()
        .map(|tag| tag.name.as_str())
        .collect::<Vec<_>>();
    names.sort_unstable();
    assert_eq!(names, ["later", "work"]);

    // Removal detaches the tag; a repeated key replays.
    let remove_key = "20000000-0000-4000-8000-000000000012".to_owned();
    let removed = block_on(core.remove_tag(
        CancelToken::new(),
        remove_key.clone(),
        MessageSelection::OccurrenceIds(vec![occurrence]),
        "work".into(),
    ))
    .unwrap();
    assert!(removed.result_json.contains(r#""removed":1"#));
    assert_eq!(
        block_on(core.remove_tag(
            CancelToken::new(),
            remove_key,
            MessageSelection::OccurrenceIds(vec![occurrence]),
            "work".into(),
        ))
        .unwrap(),
        removed
    );
    let read = block_on(core.get_message(CancelToken::new(), message_id)).unwrap();
    assert_eq!(
        read.tags
            .iter()
            .map(|tag| tag.name.as_str())
            .collect::<Vec<_>>(),
        ["later"]
    );

    // One note per message; empty text removes it.
    let note_key = "20000000-0000-4000-8000-000000000013".to_owned();
    let noted = block_on(core.set_note(
        CancelToken::new(),
        note_key.clone(),
        message_id,
        "a note".into(),
    ))
    .unwrap();
    assert_eq!(noted.command, "set_note");
    assert_eq!(
        block_on(core.set_note(CancelToken::new(), note_key, message_id, "a note".into(),))
            .unwrap(),
        noted
    );
    let read = block_on(core.get_message(CancelToken::new(), message_id)).unwrap();
    assert_eq!(read.note.as_deref(), Some("a note"));
    let cleared = block_on(core.set_note(
        CancelToken::new(),
        "20000000-0000-4000-8000-000000000014".into(),
        message_id,
        "".into(),
    ))
    .unwrap();
    assert_eq!(cleared.command, "set_note");
    let read = block_on(core.get_message(CancelToken::new(), message_id)).unwrap();
    assert_eq!(read.note, None);

    block_on(core.close_window(window.window_id));
    block_on(core.shutdown());
}

/// Both exported queries honor their cancellation token.
#[test]
fn reader_queries_honor_cancellation() {
    use crate::{CoreConfig, open_core};
    use sb_test_support::{ScratchDir, block_on};
    let scratch = ScratchDir::new("ffi-reader");
    let core = block_on(open_core(
        CancelToken::new(),
        CoreConfig {
            data_root: scratch.path().join("store").to_string_lossy().into_owned(),
            cache_root: scratch.path().join("cache").to_string_lossy().into_owned(),
        },
    ))
    .unwrap();
    let cancelled = CancelToken::new();
    cancelled.cancel();
    assert!(matches!(
        block_on(core.get_message(cancelled.clone(), -1)),
        Err(CoreError::Cancelled)
    ));
    assert!(matches!(
        block_on(core.message_for_occurrence(cancelled, -1)),
        Err(CoreError::Cancelled)
    ));
    block_on(core.shutdown());
}
