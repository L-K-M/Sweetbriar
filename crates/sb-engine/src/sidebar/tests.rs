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
        "From a@example.test Sat Oct 17 12:00:00 2026\nStatus: RO\nSubject: Read\n\nx\n\
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
    block_on(engine.shutdown()).unwrap();
}
