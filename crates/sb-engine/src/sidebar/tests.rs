use std::fs;
use std::time::{Duration, Instant};

use sb_resources::ImportJobState;
use sb_test_support::{ScratchDir, block_on};

use crate::{AccountKind, Engine, EngineError, MailboxKind};

/// An import's mailbox appears under the local archive with its counts,
/// and an unknown account's mailboxes are `NotFound` (spec/07).
#[test]
fn an_import_shows_in_the_local_archives_mailboxes() {
    let scratch = ScratchDir::new("engine-sidebar");
    let engine = block_on(Engine::open(scratch.path().join("store"))).unwrap();
    let accounts = block_on(engine.accounts()).unwrap();
    assert_eq!(accounts.len(), 1);
    assert_eq!(accounts[0].kind, AccountKind::Local);
    let archive = accounts[0].account_id.clone();
    assert!(
        block_on(engine.mailboxes(&archive))
            .unwrap()
            .mailboxes
            .is_empty()
    );

    let path = scratch.path().join("Inbox.mbox");
    fs::write(
        &path,
        "From a@example.test Sat Oct 17 12:00:00 2026\nStatus: RO\nFrom: Sender <sender@example.test>\nTo: Recipient <to@example.test>\nCc: Copy <cc@example.test>\nDate: Sat, 17 Oct 2026 12:00:00 +0000\nMessage-ID: <reader@example.test>\nSubject: Read\n\nx\n\
         From a@example.test Sat Oct 17 12:00:00 2026\nStatus: O\nSubject: Unread\n\ny\n",
    )
    .unwrap();
    let id = block_on(engine.start_import(&engine.new_idempotency_key(), vec![path]))
        .unwrap()
        .job_id()
        .to_owned();
    let deadline = Instant::now() + Duration::from_secs(300);
    let mut confirmed = false;
    loop {
        let state = block_on(engine.import_job(&id)).unwrap().state();
        if state == ImportJobState::AwaitingConfirmation && !confirmed {
            block_on(engine.confirm_import(&engine.new_idempotency_key(), &id, Vec::new()))
                .unwrap();
            confirmed = true;
        }
        if state == ImportJobState::Completed {
            break;
        }
        assert!(Instant::now() < deadline, "the import never finished");
        std::thread::sleep(Duration::from_millis(10));
    }

    let list = block_on(engine.mailboxes(&archive)).unwrap();
    assert_eq!(list.mailboxes.len(), 1);
    let inbox = &list.mailboxes[0];
    assert_eq!(
        (
            inbox.name.as_str(),
            inbox.kind,
            inbox.selectable,
            inbox.total,
            inbox.unread
        ),
        ("Inbox", MailboxKind::Folder, true, 2, 1)
    );
    assert!(matches!(
        block_on(engine.mailboxes("no-such-account")),
        Err(EngineError::NotFound { .. })
    ));

    // Reader metadata uses the occurrence selected by the list (M1.5i).
    use crate::{BodyAvailability, ReaderAddress, ReaderOccurrence};
    let address = |name: &str, email: &str| ReaderAddress {
        name: Some(name.into()),
        address: Some(email.into()),
    };
    let window = block_on(engine.open_window(
        crate::WindowSpec::Mailbox(inbox.mailbox_id),
        crate::Anchor::Top,
        10,
    ))
    .unwrap();
    let row = window
        .rows()
        .iter()
        .find(|row| row.subject() == "Read")
        .unwrap();
    let id = block_on(engine.message_for_occurrence(row.occurrence_id())).unwrap();
    assert_eq!(id, row.message_id());
    let message = block_on(engine.get_message(id)).unwrap();
    assert_eq!(message.message_id, id);
    assert_eq!(
        message.headers.from,
        vec![address("Sender", "sender@example.test")]
    );
    assert_eq!(
        message.headers.to,
        vec![address("Recipient", "to@example.test")]
    );
    assert_eq!(message.headers.cc, vec![address("Copy", "cc@example.test")]);
    assert_eq!(message.headers.date, Some(1_792_238_400_000));
    assert_eq!(message.headers.subject.as_deref(), Some("Read"));
    assert_eq!(
        message.headers.message_id.as_deref(),
        Some("reader@example.test")
    );
    assert_eq!(message.account_id, archive);
    assert_eq!(message.account_display_name, "Local Archive");
    assert_eq!(
        message.occurrences,
        vec![ReaderOccurrence {
            occurrence_id: row.occurrence_id(),
            mailbox_id: inbox.mailbox_id,
            mailbox_name: "Inbox".into(),
        }]
    );
    assert_eq!(message.body_availability, BodyAvailability::Acquired);
    assert!(matches!(
        block_on(engine.get_message(-1)),
        Err(EngineError::NotFound { .. })
    ));
    assert!(matches!(
        block_on(engine.message_for_occurrence(-1)),
        Err(EngineError::NotFound { .. })
    ));
    block_on(engine.shutdown()).unwrap();
}
