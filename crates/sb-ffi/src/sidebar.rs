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
}
