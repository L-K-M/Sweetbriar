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
}
