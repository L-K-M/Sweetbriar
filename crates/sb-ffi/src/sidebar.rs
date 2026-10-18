//! The sidebar's surface over FFI (spec/07 "Other queries", spec/08
//! "Sidebar"): records and enums mirroring `sb-service`'s account and
//! mailbox DTOs field by field, and the two exported queries. Both
//! race the token at every wait (`Core::query`).

use std::sync::Arc;

use crate::api::Core;
use crate::cancel::CancelToken;
use crate::error::CoreError;
use crate::windows::MailboxRole;

/// How an account reaches its mail.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum AccountKind {
    Local,
    Imap,
}

/// Whether an account syncs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum AccountState {
    Active,
    Paused,
    NeedsAttention,
    Disconnected,
}

/// One account (spec/07 `accounts()`).
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Account {
    pub account_id: String,
    pub kind: AccountKind,
    pub display_name: String,
    pub state: AccountState,
    pub state_reason: Option<String>,
    pub sort_order: i64,
}

impl Account {
    /// Mirror the service's account field by field.
    fn from_service(account: sb_service::Account) -> Self {
        Self {
            account_id: account.account_id,
            kind: match account.kind {
                sb_service::AccountKind::Local => AccountKind::Local,
                sb_service::AccountKind::Imap => AccountKind::Imap,
            },
            display_name: account.display_name,
            state: match account.state {
                sb_service::AccountState::Active => AccountState::Active,
                sb_service::AccountState::Paused => AccountState::Paused,
                sb_service::AccountState::NeedsAttention => AccountState::NeedsAttention,
                sb_service::AccountState::Disconnected => AccountState::Disconnected,
            },
            state_reason: account.state_reason,
            sort_order: account.sort_order,
        }
    }
}

/// What a mailbox is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, uniffi::Enum)]
pub enum MailboxKind {
    Folder,
    Label,
    Virtual,
}

/// One mailbox, with its present occurrences and the unread ones.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
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

impl Mailbox {
    /// Mirror the service's mailbox field by field.
    fn from_service(mailbox: sb_service::Mailbox) -> Self {
        Self {
            mailbox_id: mailbox.mailbox_id,
            account_id: mailbox.account_id,
            name: mailbox.name,
            delimiter: mailbox.delimiter,
            role: mailbox.role.map(MailboxRole::from_service),
            kind: match mailbox.kind {
                sb_service::MailboxKind::Folder => MailboxKind::Folder,
                sb_service::MailboxKind::Label => MailboxKind::Label,
                sb_service::MailboxKind::Virtual => MailboxKind::Virtual,
            },
            selectable: mailbox.selectable,
            total: mailbox.total,
            unread: mailbox.unread,
        }
    }
}

/// An account's mailboxes as of one list generation.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Mailboxes {
    pub generation: u64,
    pub mailboxes: Vec<Mailbox>,
}

#[uniffi::export]
impl Core {
    /// `accounts()` (spec/07): every account, by sort order then id.
    pub async fn accounts(&self, token: Arc<CancelToken>) -> Result<Vec<Account>, CoreError> {
        let accounts = self
            .query(&token, async |core| core.accounts().await)
            .await?;
        Ok(accounts.into_iter().map(Account::from_service).collect())
    }

    /// `mailboxes(account_id)` (spec/07): the account's mailboxes with
    /// their counts, as of one list generation. An unknown account is
    /// `NotFound`.
    pub async fn mailboxes(
        &self,
        token: Arc<CancelToken>,
        account_id: String,
    ) -> Result<Mailboxes, CoreError> {
        let list = self
            .query(&token, async |core| core.mailboxes(&account_id).await)
            .await?;
        Ok(Mailboxes {
            generation: list.generation,
            mailboxes: list
                .mailboxes
                .into_iter()
                .map(Mailbox::from_service)
                .collect(),
        })
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::api::{CoreConfig, open_core};
    use sb_test_support::ScratchDir;

    fn token() -> Arc<CancelToken> {
        CancelToken::new()
    }

    /// Over the exported path a fresh core lists its local archive
    /// with no mailboxes; an unknown account is `NotFound` and a
    /// cancelled token `Cancelled`.
    #[test]
    fn the_sidebar_queries_round_trip_over_the_exported_path() {
        let scratch = ScratchDir::new("ffi-sidebar");
        let config = CoreConfig {
            data_root: scratch.path().join("store").to_string_lossy().into_owned(),
            cache_root: scratch.path().join("cache").to_string_lossy().into_owned(),
        };
        let core = sb_test_support::block_on(open_core(token(), config)).unwrap();

        let accounts = sb_test_support::block_on(core.accounts(token())).unwrap();
        assert_eq!(accounts.len(), 1);
        assert_eq!(accounts[0].kind, AccountKind::Local);
        assert_eq!(accounts[0].state, AccountState::Active);

        let account = accounts[0].account_id.clone();
        let list = sb_test_support::block_on(core.mailboxes(token(), account)).unwrap();
        assert!(list.mailboxes.is_empty());

        let err = sb_test_support::block_on(core.mailboxes(token(), "no-such-account".to_owned()))
            .unwrap_err();
        assert!(matches!(err, CoreError::NotFound { .. }));

        let cancelled = token();
        cancelled.cancel();
        let err = sb_test_support::block_on(core.accounts(cancelled)).unwrap_err();
        assert!(matches!(err, CoreError::Cancelled));

        sb_test_support::block_on(core.shutdown());
    }

    /// Populated rows cross field by field: an account in states a
    /// fresh store lacks, and an imported mailbox with distinct total
    /// and unread counts at the generation its window shows.
    #[test]
    fn populated_sidebar_rows_cross_field_by_field() {
        use std::time::{Duration, Instant};

        const WORK: &str = "00000000-0000-4000-8000-0000000000b0";

        let scratch = ScratchDir::new("ffi-sidebar-populated");
        let store = scratch.path().join("store");
        let config = || CoreConfig {
            data_root: store.to_string_lossy().into_owned(),
            cache_root: scratch.path().join("cache").to_string_lossy().into_owned(),
        };
        // Create the store, then add an account no command makes yet.
        let core = sb_test_support::block_on(open_core(token(), config())).unwrap();
        sb_test_support::block_on(core.shutdown());
        sb_test_support::execute_sql(
            &store.join("state.sqlite"),
            &format!(
                "INSERT INTO accounts (account_id, kind, provider, display_name, state,
                   state_reason, sort_order, created_at)
                 VALUES ('{WORK}', 'imap', 'proton_bridge', 'Work', 'needs_attention',
                   'Bridge is not running', 5, 0)"
            ),
        );
        let core = sb_test_support::block_on(open_core(token(), config())).unwrap();

        let accounts = sb_test_support::block_on(core.accounts(token())).unwrap();
        let archive = accounts[0].account_id.clone();
        assert_eq!(
            accounts,
            vec![
                Account {
                    account_id: archive.clone(),
                    kind: AccountKind::Local,
                    display_name: "Local Archive".to_owned(),
                    state: AccountState::Active,
                    state_reason: None,
                    sort_order: 0,
                },
                Account {
                    account_id: WORK.to_owned(),
                    kind: AccountKind::Imap,
                    display_name: "Work".to_owned(),
                    state: AccountState::NeedsAttention,
                    state_reason: Some("Bridge is not running".to_owned()),
                    sort_order: 5,
                },
            ]
        );

        // Two messages, one unread, imported through the service: the
        // import commands cross the FFI only in M1.4e.
        let path = scratch.path().join("Inbox.mbox");
        std::fs::write(
            &path,
            "From a@example.test Sat Oct 17 12:00:00 2026\nStatus: RO\nSubject: Read\n\nx\n\
             From a@example.test Sat Oct 17 12:00:00 2026\nStatus: O\nSubject: Unread\n\ny\n",
        )
        .unwrap();
        let generation = sb_test_support::block_on(async {
            let guard = core.service.read().await;
            let service = guard.as_ref().unwrap();
            let key = service.new_idempotency_key();
            let job = service
                .start_import(&key, vec![path], sb_service::ImportTarget::LocalArchive)
                .await
                .unwrap()
                .job_id;
            let deadline = Instant::now() + Duration::from_secs(300);
            loop {
                let state = service.import_job(&job).await.unwrap().state;
                if state == sb_service::ImportJobState::AwaitingConfirmation {
                    let key = service.new_idempotency_key();
                    service
                        .confirm_import(&key, &job, Vec::new())
                        .await
                        .unwrap();
                }
                if state == sb_service::ImportJobState::Completed {
                    break;
                }
                assert!(Instant::now() < deadline, "the import never finished");
                std::thread::sleep(Duration::from_millis(10));
            }
            let list = service.mailboxes(&archive).await.unwrap();
            let window = service
                .open_window(
                    sb_service::WindowSpec::Mailbox(list.mailboxes[0].mailbox_id),
                    sb_service::Anchor::Top,
                    1,
                )
                .await
                .unwrap();
            window.generation
        });

        let list = sb_test_support::block_on(core.mailboxes(token(), archive.clone())).unwrap();
        assert_eq!(list.generation, generation);
        let [inbox] = list.mailboxes.as_slice() else {
            panic!("one mailbox: {:?}", list.mailboxes);
        };
        assert_eq!(
            inbox,
            &Mailbox {
                mailbox_id: inbox.mailbox_id,
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

        sb_test_support::block_on(core.shutdown());
    }
}
