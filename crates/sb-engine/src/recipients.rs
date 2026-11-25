//! The recipient check (spec/05 "Recipient check", spec/07, M6.7a):
//! `recipient_fence` answers which recipients of a draft revision are
//! blocked for its route's account, `recipient_allowances` lists the
//! user's allowances, and `allow_recipient` and
//! `remove_recipient_allowance` change them. `send_draft` refuses a
//! blocked revision through [`refuse_blocked`], the same store rule.

use sb_resources::{
    Receipt, StoreError, StoredRecipientFence, StoredRevision, Timestamp, Transaction,
    WritePriority, account_senders_ready, allow_recipient, draft, recipient_allowances,
    recipient_fence, remove_recipient_allowance, run_once,
};

use crate::engine::validate_idempotency_key;
use crate::{Engine, EngineError};

/// The commands' names in `command_receipts` (spec/07).
const ALLOW_RECIPIENT: &str = "allow_recipient";
const REMOVE_RECIPIENT_ALLOWANCE: &str = "remove_recipient_allowance";

/// The longest account id a command takes, in UTF-8 bytes.
const MAX_ACCOUNT_ID_BYTES: usize = 128;
/// The longest address a command takes: RFC 5321's 256-octet path
/// less its angle brackets (§4.5.3.1.3).
const MAX_ADDRESS_BYTES: usize = 254;

/// What `recipient_fence` answers (spec/07): whether the check is
/// ready (spec/02 "Readiness") and each blocked recipient. Nothing is
/// blocked before it is ready.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipientFence {
    pub ready: bool,
    pub blocked: Vec<BlockedRecipient>,
}

/// One blocked recipient: its address as the draft spells it and the
/// sending accounts it has written to.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BlockedRecipient {
    pub address: String,
    pub account_ids: Vec<String>,
}

/// One allowance, for Settings (spec/07): the account, the address as
/// the user allowed it and when.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecipientAllowance {
    pub account_id: String,
    pub address: String,
    pub created_at: Timestamp,
}

impl From<StoredRecipientFence> for RecipientFence {
    fn from(fence: StoredRecipientFence) -> Self {
        Self {
            ready: fence.ready,
            blocked: fence
                .blocked
                .into_iter()
                .map(|recipient| BlockedRecipient {
                    address: recipient.address,
                    account_ids: recipient.accounts,
                })
                .collect(),
        }
    }
}

impl Engine {
    /// `recipient_fence(draft_id, revision_id)` (spec/07): the check for
    /// that revision's route. A revision without a route blocks nothing
    /// yet; a revision that isn't the draft's current one is
    /// `Conflict`, an unknown draft `NotFound`.
    pub async fn recipient_fence(
        &self,
        draft_id: &str,
        revision_id: &str,
    ) -> Result<RecipientFence, EngineError> {
        let (draft_id, revision_id) = (draft_id.to_owned(), revision_id.to_owned());
        let fence = self
            .read_through(move |resources| {
                resources.read(|tx| {
                    let stored = draft(tx, &draft_id)?.ok_or_else(|| StoreError::NotFound {
                        what: format!("draft {draft_id}"),
                    })?;
                    let revision = stored.revision.ok_or_else(|| StoreError::Conflict {
                        detail: format!("draft {draft_id} has no revision to send"),
                    })?;
                    if revision.revision_id != revision_id {
                        return Err(StoreError::Conflict {
                            detail: format!(
                                "draft {draft_id}'s current revision is not {revision_id}"
                            ),
                        });
                    }
                    revision_fence(tx, &revision)
                })
            })
            .await?;
        Ok(fence.into())
    }

    /// `recipient_allowances()` (spec/07): every allowance, ordered by
    /// account then canonical address.
    pub async fn recipient_allowances(&self) -> Result<Vec<RecipientAllowance>, EngineError> {
        let stored = self
            .read_through(|resources| resources.read(recipient_allowances))
            .await?;
        Ok(stored
            .into_iter()
            .map(|allowance| RecipientAllowance {
                account_id: allowance.account_id,
                address: allowance.email,
                created_at: Timestamp::from_millis(allowance.created_at),
            })
            .collect())
    }

    /// `allow_recipient(account_id, address)` (spec/05, 07): a lasting
    /// allowance of `address` for the account. Allowing an allowed
    /// address writes nothing; the receipt says whether this call
    /// changed anything. The local archive is `InvalidInput`, an unknown
    /// account `NotFound`.
    pub async fn allow_recipient(
        &self,
        idempotency_key: &str,
        account_id: &str,
        address: &str,
    ) -> Result<Receipt, EngineError> {
        valid_arguments(idempotency_key, account_id, address)?;
        let (account_id, address) = (account_id.to_owned(), address.to_owned());
        self.recipient_command(idempotency_key, ALLOW_RECIPIENT, move |tx, now| {
            allow_recipient(tx, &account_id, &address, now.as_millis())
        })
        .await
    }

    /// `remove_recipient_allowance(account_id, address)` (spec/05, 07):
    /// revoke an allowance, so the block applies again. An address that
    /// isn't allowed is `NotFound`; the local archive `InvalidInput`.
    pub async fn remove_recipient_allowance(
        &self,
        idempotency_key: &str,
        account_id: &str,
        address: &str,
    ) -> Result<Receipt, EngineError> {
        valid_arguments(idempotency_key, account_id, address)?;
        let (account_id, address) = (account_id.to_owned(), address.to_owned());
        self.recipient_command(idempotency_key, REMOVE_RECIPIENT_ALLOWANCE, move |tx, _| {
            remove_recipient_allowance(tx, &account_id, &address).map(|()| true)
        })
        .await
    }

    /// One allowance command under its idempotency key: the write and
    /// its receipt, `{"changed":bool}`, commit together.
    async fn recipient_command(
        &self,
        idempotency_key: &str,
        command: &'static str,
        body: impl FnOnce(&Transaction, Timestamp) -> Result<bool, StoreError> + Send + 'static,
    ) -> Result<Receipt, EngineError> {
        let key = idempotency_key.to_owned();
        let now = self.resources().now();
        self.command(move |worker| async move {
            Ok(worker
                .resources()
                .write(WritePriority::User, move |tx| {
                    run_once(tx, &key, command, now, |tx| {
                        body(tx, now).map(|changed| format!("{{\"changed\":{changed}}}"))
                    })
                })
                .await?)
        })
        .await
    }
}

/// The check for one revision (spec/05): its To, Cc and Bcc against the
/// route's account. A revision without a route has nothing to block yet;
/// `send_draft` refuses it on its own.
fn revision_fence(
    tx: &Transaction,
    revision: &StoredRevision,
) -> Result<StoredRecipientFence, StoreError> {
    let (Some(_), Some(account_id)) = (&revision.identity_id, &revision.account_id) else {
        return Ok(StoredRecipientFence {
            ready: account_senders_ready(tx)?,
            blocked: Vec::new(),
        });
    };
    let addresses: Vec<&str> = revision
        .to
        .iter()
        .chain(&revision.cc)
        .chain(&revision.bcc)
        .map(|recipient| recipient.address.as_str())
        .collect();
    recipient_fence(tx, account_id, &addresses)
}

/// `send_draft`'s refusal (spec/05): a revision with a blocked
/// recipient is `Conflict` naming `recipient_fence` and the addresses.
/// No acknowledgement overrides it.
pub(crate) fn refuse_blocked(
    tx: &Transaction,
    revision: &StoredRevision,
) -> Result<(), StoreError> {
    let fence = revision_fence(tx, revision)?;
    if fence.blocked.is_empty() {
        return Ok(());
    }
    let addresses: Vec<&str> = fence
        .blocked
        .iter()
        .map(|recipient| recipient.address.as_str())
        .collect();
    Err(StoreError::Conflict {
        detail: format!(
            "recipient_fence: never wrote to this account, only to another: {}",
            addresses.join(", ")
        ),
    })
}

fn valid_arguments(
    idempotency_key: &str,
    account_id: &str,
    address: &str,
) -> Result<(), EngineError> {
    validate_idempotency_key(idempotency_key)?;
    if account_id.is_empty() || account_id.len() > MAX_ACCOUNT_ID_BYTES {
        return Err(EngineError::invalid_input(
            "account_id",
            format!("an account id is 1 through {MAX_ACCOUNT_ID_BYTES} UTF-8 bytes"),
        ));
    }
    if address.is_empty() || address.len() > MAX_ADDRESS_BYTES {
        return Err(EngineError::invalid_input(
            "address",
            format!("an address is 1 through {MAX_ADDRESS_BYTES} UTF-8 bytes"),
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests;
