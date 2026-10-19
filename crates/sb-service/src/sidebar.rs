//! The sidebar's DTOs and `Core`'s account and mailbox queries (spec/07
//! "Other queries", spec/08 "Sidebar"): service-owned mirrors of the
//! engine's types, converted field by field (spec/01 rule 3).

use crate::{Core, CoreError, MailboxRole};

/// How an account reaches its mail.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountKind {
    Local,
    Imap,
}

/// Whether an account syncs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AccountState {
    Active,
    Paused,
    NeedsAttention,
    Disconnected,
}

/// One account (spec/07 `accounts()`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Account {
    pub account_id: String,
    pub kind: AccountKind,
    pub display_name: String,
    pub state: AccountState,
    pub state_reason: Option<String>,
    pub sort_order: i64,
}

/// What a mailbox is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum MailboxKind {
    Folder,
    Label,
    Virtual,
}

/// One mailbox, with its present occurrences and the unread ones.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mailbox {
    pub mailbox_id: i64,
    pub account_id: String,
    pub name: String,
    pub delimiter: Option<String>,
    pub role: Option<MailboxRole>,
    pub kind: MailboxKind,
    pub selectable: bool,
    pub total: u64,
    pub unread: u64,
}

/// An account's mailboxes as of one list generation.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Mailboxes {
    pub generation: u64,
    pub mailboxes: Vec<Mailbox>,
}

impl Core {
    /// `accounts()` (spec/07): every account, by sort order then id.
    pub async fn accounts(&self) -> Result<Vec<Account>, CoreError> {
        let accounts = self.engine().accounts().await?;
        Ok(accounts
            .into_iter()
            .map(|account| Account {
                account_id: account.account_id,
                kind: match account.kind {
                    sb_engine::AccountKind::Local => AccountKind::Local,
                    sb_engine::AccountKind::Imap => AccountKind::Imap,
                },
                display_name: account.display_name,
                state: match account.state {
                    sb_engine::AccountState::Active => AccountState::Active,
                    sb_engine::AccountState::Paused => AccountState::Paused,
                    sb_engine::AccountState::NeedsAttention => AccountState::NeedsAttention,
                    sb_engine::AccountState::Disconnected => AccountState::Disconnected,
                },
                state_reason: account.state_reason,
                sort_order: account.sort_order,
            })
            .collect())
    }

    /// `mailboxes(account_id)` (spec/07): the account's mailboxes with
    /// their counts, as of one list generation. An unknown account is
    /// `NotFound`.
    pub async fn mailboxes(&self, account_id: &str) -> Result<Mailboxes, CoreError> {
        let list = self.engine().mailboxes(account_id).await?;
        Ok(Mailboxes {
            generation: list.generation,
            mailboxes: list
                .mailboxes
                .into_iter()
                .map(|mailbox| Mailbox {
                    mailbox_id: mailbox.mailbox_id,
                    account_id: mailbox.account_id,
                    name: mailbox.name,
                    delimiter: mailbox.delimiter,
                    role: mailbox.role.map(MailboxRole::from_engine),
                    kind: match mailbox.kind {
                        sb_engine::MailboxKind::Folder => MailboxKind::Folder,
                        sb_engine::MailboxKind::Label => MailboxKind::Label,
                        sb_engine::MailboxKind::Virtual => MailboxKind::Virtual,
                    },
                    selectable: mailbox.selectable,
                    total: mailbox.total,
                    unread: mailbox.unread,
                })
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CoreConfig, open_core};

    /// A fresh core lists its local archive, with no mailboxes yet; an
    /// unknown account is `NotFound`.
    #[test]
    fn a_fresh_core_lists_its_local_archive() {
        let scratch = sb_test_support::ScratchDir::new("service-sidebar");
        let config = CoreConfig {
            data_root: scratch.path().join("store"),
            cache_root: scratch.path().join("cache"),
        };
        let core = sb_test_support::block_on(open_core(config)).unwrap();
        let accounts = sb_test_support::block_on(core.accounts()).unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].kind, AccountKind::Local);
        assert_eq!(accounts[0].state, AccountState::Active);
        let mailboxes = sb_test_support::block_on(core.mailboxes(&accounts[0].account_id)).unwrap();
        assert!(mailboxes.mailboxes.is_empty());
        assert!(matches!(
            sb_test_support::block_on(core.mailboxes("no-such-account")),
            Err(CoreError::NotFound { .. })
        ));
        sb_test_support::block_on(core.shutdown());
    }

    /// An imported mailbox crosses with every field, its counts and the
    /// generation its rows show.
    #[test]
    fn an_imported_mailbox_crosses_with_its_counts() {
        use std::time::{Duration, Instant};

        use crate::{Anchor, ImportJobState, ImportTarget, WindowSpec};

        let scratch = sb_test_support::ScratchDir::new("service-sidebar-import");
        let config = CoreConfig {
            data_root: scratch.path().join("store"),
            cache_root: scratch.path().join("cache"),
        };
        let core = sb_test_support::block_on(open_core(config)).unwrap();
        let archive = sb_test_support::block_on(core.accounts()).unwrap()[0]
            .account_id
            .clone();

        let path = scratch.path().join("Inbox.mbox");
        std::fs::write(
            &path,
            "From a@example.test Sat Oct 17 12:00:00 2026\nStatus: RO\nFrom: Sender <sender@example.test>\nTo: Recipient <to@example.test>\nCc: Copy <cc@example.test>\nDate: Sat, 17 Oct 2026 12:00:00 +0000\nMessage-ID: <reader@example.test>\nSubject: Read\n\nx\n\
             From a@example.test Sat Oct 17 12:00:00 2026\nStatus: O\nSubject: Unread\n\ny\n",
        )
        .unwrap();
        let key = core.new_idempotency_key();
        let started = sb_test_support::block_on(core.start_import(
            &key,
            vec![path],
            ImportTarget::LocalArchive,
        ))
        .unwrap();
        let deadline = Instant::now() + Duration::from_secs(300);
        loop {
            let job = sb_test_support::block_on(core.import_job(&started.job_id)).unwrap();
            if job.state == ImportJobState::AwaitingConfirmation {
                let key = core.new_idempotency_key();
                sb_test_support::block_on(core.confirm_import(&key, &started.job_id, Vec::new()))
                    .unwrap();
            }
            if job.state == ImportJobState::Completed {
                break;
            }
            assert!(Instant::now() < deadline, "the import never finished");
            std::thread::sleep(Duration::from_millis(10));
        }

        // The thread rebuild the import scheduled can still advance the
        // list generation between two reads: read until the list and
        // its window agree.
        let (list, window) = loop {
            let list = sb_test_support::block_on(core.mailboxes(&archive)).unwrap();
            let [inbox] = list.mailboxes.as_slice() else {
                panic!("one mailbox: {:?}", list.mailboxes);
            };
            let window = sb_test_support::block_on(core.open_window(
                WindowSpec::Mailbox(inbox.mailbox_id),
                Anchor::Top,
                10,
            ))
            .unwrap();
            if list.generation == window.generation {
                break (list, window);
            }
            sb_test_support::block_on(core.close_window(window.window_id));
            assert!(
                Instant::now() < deadline,
                "the list generation never settled"
            );
            std::thread::sleep(Duration::from_millis(10));
        };
        let [inbox] = list.mailboxes.as_slice() else {
            panic!("one mailbox: {:?}", list.mailboxes);
        };
        assert_eq!(list.generation, window.generation);
        assert_eq!(window.rows.len(), 2);
        assert_eq!(
            inbox,
            &Mailbox {
                mailbox_id: window.rows[0].mailbox_id,
                account_id: archive,
                name: "Inbox".to_owned(),
                delimiter: None,
                role: None,
                kind: MailboxKind::Folder,
                selectable: true,
                total: 2,
                unread: 1,
            }
        );

        // Reader metadata uses the occurrence selected by the list (M1.5i).
        use crate::{BodyAvailability, ReaderAddress, ReaderOccurrence};
        let address = |name: &str, email: &str| ReaderAddress {
            name: Some(name.into()),
            address: Some(email.into()),
        };

        let row = window
            .rows
            .iter()
            .find(|row| row.subject == "Read")
            .unwrap();
        let id = sb_test_support::block_on(core.message_for_occurrence(row.occurrence_id)).unwrap();
        assert_eq!(id, row.message_id);
        let message = sb_test_support::block_on(core.get_message(id)).unwrap();
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
        assert_eq!(message.account_id, inbox.account_id);
        assert_eq!(message.account_display_name, "Local Archive");
        assert_eq!(
            message.occurrences,
            vec![ReaderOccurrence {
                occurrence_id: row.occurrence_id,
                mailbox_id: inbox.mailbox_id,
                mailbox_name: "Inbox".into(),
            }]
        );
        assert_eq!(message.body_availability, BodyAvailability::Acquired);
        assert!(matches!(
            sb_test_support::block_on(core.get_message(-1)),
            Err(CoreError::NotFound { .. })
        ));
        assert!(matches!(
            sb_test_support::block_on(core.message_for_occurrence(-1)),
            Err(CoreError::NotFound { .. })
        ));
        sb_test_support::block_on(core.shutdown());
    }
}
