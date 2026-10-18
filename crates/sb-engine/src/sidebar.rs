//! The sidebar's queries (spec/07 "Other queries"): the accounts, and
//! one account's mailboxes with their counts, each one short read on
//! the blocking pool.

use sb_resources::{MailboxList, StoredAccount, accounts, mailboxes};

use crate::{Engine, EngineError};

impl Engine {
    /// `accounts()` (spec/07): every account, by sort order then id.
    pub async fn accounts(&self) -> Result<Vec<StoredAccount>, EngineError> {
        self.read_through(|resources| resources.read(accounts))
            .await
    }

    /// `mailboxes(account_id)` (spec/07): the account's mailboxes with
    /// their present and unread occurrences, as of one list generation.
    /// An unknown account is `NotFound`.
    pub async fn mailboxes(&self, account_id: &str) -> Result<MailboxList, EngineError> {
        let account = account_id.to_owned();
        self.read_through(move |resources| resources.read(|tx| mailboxes(tx, &account)))
            .await?
            .ok_or_else(|| EngineError::NotFound {
                what: format!("account {account_id}"),
            })
    }
}

#[cfg(test)]
mod tests;
