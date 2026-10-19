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
