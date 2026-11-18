//! Identity writes (spec/02 `identities`, spec/07 "Identities").
//!
//! The two identity commands' rows: an upsert for the accounts whose
//! addresses the user maintains, and the one-default flip. Proton
//! accounts are refused here because their identities come from
//! Proton's addresses and a later refresh would overwrite a local edit
//! (M2.12).

use rusqlite::{OptionalExtension, Transaction, params};

use crate::StoreError;
use crate::accounts::IdentitySendable;
use crate::query::AddressOperand;
use crate::schema::classify;

/// Why an identity command refused although its target exists, beyond
/// the ordinary store failures. Both refusals are policy, not damage.
#[derive(Debug, Clone)]
pub enum IdentityError {
    /// The account's identities come from its backend (Proton), so a
    /// local edit would fight the next address refresh (M2.12).
    ProtonAccount { account_id: String },
    /// The identity can't send right now (`sendable`), so it cannot
    /// become the account's sending default (spec/05).
    NotSendable {
        identity_id: String,
        sendable: &'static str,
    },
    /// A caller field was rejected; `field` names it for the API.
    InvalidInput {
        field: &'static str,
        message: String,
    },
    /// Any other store failure.
    Store(StoreError),
}

impl From<StoreError> for IdentityError {
    fn from(error: StoreError) -> Self {
        Self::Store(error)
    }
}

/// One identity write's fields (spec/02 `identities`). The id names an
/// existing row to update or a fresh one to insert; `is_default` is
/// deliberately absent — [`set_default_identity`] owns that flag.
pub struct IdentityWrite {
    pub identity_id: String,
    pub account_id: String,
    pub email: String,
    pub display_name: String,
    pub reply_to: Option<String>,
    pub signature_source: Option<String>,
    pub sendable: IdentitySendable,
}

fn invalid(field: &'static str, message: impl Into<String>) -> IdentityError {
    IdentityError::InvalidInput {
        field,
        message: message.into(),
    }
}

fn validate(write: &IdentityWrite) -> Result<(), IdentityError> {
    if write.identity_id.is_empty() || write.identity_id.len() > 128 {
        return Err(invalid(
            "identity_id",
            "use 1 to 128 UTF-8 bytes without controls",
        ));
    }
    if write.identity_id.chars().any(char::is_control) {
        return Err(invalid("identity_id", "controls are not allowed"));
    }
    if write.account_id.is_empty() {
        return Err(invalid("account_id", "an account id is required"));
    }
    let email = write.email.trim();
    if AddressOperand::parse(email).is_err() {
        return Err(invalid("email", "use one valid address"));
    }
    if write.display_name.trim().is_empty() {
        return Err(invalid("display_name", "a display name is required"));
    }
    if let Some(reply_to) = write.reply_to.as_deref() {
        let reply_to = reply_to.trim();
        if reply_to.is_empty() {
            return Err(invalid("reply_to", "use null to clear it"));
        }
        if AddressOperand::parse(reply_to).is_err() {
            return Err(invalid("reply_to", "use one valid address"));
        }
    }
    Ok(())
}

/// `upsert_identity` (spec/07): insert the identity or update the row
/// `identity_id` names. An id another account holds is `Conflict`, as is
/// an email another identity in the account already holds (the schema's
/// `UNIQUE (account_id, email)`). A Proton account is refused. Returns
/// nothing; the caller's receipt names the id.
pub fn upsert_identity(tx: &Transaction, write: &IdentityWrite) -> Result<(), IdentityError> {
    validate(write)?;
    let kind: String = tx
        .query_row(
            "SELECT kind FROM accounts WHERE account_id = ?1",
            params![write.account_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(store_error)?
        .ok_or_else(|| StoreError::not_found(format!("account {}", write.account_id)))?;
    // A Proton account's identities come from Proton on every sign-in
    // and address event (M2.12), which would overwrite a local edit.
    // Bridge accounts' are the user's to enter (spec/05).
    if kind == "proton" {
        return Err(IdentityError::ProtonAccount {
            account_id: write.account_id.clone(),
        });
    }
    if kind == "local" {
        return Err(invalid(
            "account_id",
            "the local archive can't send, so it has no identities",
        ));
    }

    let owner: Option<String> = tx
        .query_row(
            "SELECT account_id FROM identities WHERE identity_id = ?1",
            params![write.identity_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(store_error)?;
    match owner {
        Some(owner) if owner != write.account_id => {
            return Err(StoreError::conflict(format!(
                "identity {} belongs to another account than {}",
                write.identity_id, write.account_id
            ))
            .into());
        }
        _ => {}
    }
    // Another identity holding this email makes the schema's unique
    // index refuse the write; say so before it does.
    let email_owner: Option<String> = tx
        .query_row(
            "SELECT identity_id FROM identities WHERE account_id = ?1 AND email = ?2",
            params![write.account_id, write.email.trim()],
            |row| row.get(0),
        )
        .optional()
        .map_err(store_error)?;
    if let Some(owner) = email_owner
        && owner != write.identity_id
    {
        return Err(StoreError::conflict(format!(
            "account {} already has an identity with address {}",
            write.account_id,
            write.email.trim()
        ))
        .into());
    }

    let reply_to = write.reply_to.as_deref().map(str::trim);
    // The route fields (spec/02: `config_generation` moves when an
    // identity changes) as stored before this write; a signature edit
    // alone leaves queued sends and pending intents on their generation.
    let before: Option<(String, String, Option<String>, String)> = tx
        .query_row(
            "SELECT email, display_name, reply_to, sendable FROM identities
             WHERE identity_id = ?1",
            params![write.identity_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
        )
        .optional()
        .map_err(store_error)?;
    // Only the server's acceptance of a send validates an address
    // (spec/05); a user's edit may keep an address it already validated.
    if write.sendable == IdentitySendable::Validated
        && !before.as_ref().is_some_and(|(email, _, _, sendable)| {
            email == write.email.trim() && sendable == IdentitySendable::Validated.as_str()
        })
    {
        return Err(invalid(
            "sendable",
            "only a send the server accepts validates an address",
        ));
    }
    let route_changed = before.is_none_or(|(email, name, stored_reply_to, sendable)| {
        email != write.email.trim()
            || name != write.display_name
            || stored_reply_to.as_deref() != reply_to
            || sendable != write.sendable.as_str()
    });
    let changed = tx
        .execute(
            "INSERT INTO identities
             (identity_id, account_id, email, display_name, reply_to, signature_source,
              is_default, sendable)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, 0, ?7)
             ON CONFLICT(identity_id) DO UPDATE SET
               email = excluded.email,
               display_name = excluded.display_name,
               reply_to = excluded.reply_to,
               signature_source = excluded.signature_source,
               sendable = excluded.sendable,
               revision = identities.revision + 1",
            params![
                write.identity_id,
                write.account_id,
                write.email.trim(),
                write.display_name,
                reply_to,
                write.signature_source,
                write.sendable.as_str(),
            ],
        )
        .map_err(|e| IdentityError::Store(classify(e, "upsert an identity")))?;
    debug_assert!(changed == 1);
    if route_changed {
        tx.execute(
            "UPDATE accounts
             SET config_generation = config_generation + 1, revision = revision + 1
             WHERE account_id = ?1",
            params![write.account_id],
        )
        .map_err(|e| IdentityError::Store(classify(e, "update the account")))?;
    }
    Ok(())
}

/// `set_default_identity(identity_id)` (spec/07, spec/05): make the
/// identity its account's one default in one transaction — every other
/// default of that account clears first, so the partial unique index and
/// the row updates agree. An identity that isn't `validated` or
/// `provisional` can't send, so it is refused ([`IdentityError::
/// NotSendable`]); an unknown id is `NotFound`.
pub fn set_default_identity(tx: &Transaction, identity_id: &str) -> Result<(), IdentityError> {
    let (account_id, sendable): (String, String) = tx
        .query_row(
            "SELECT account_id, sendable FROM identities WHERE identity_id = ?1",
            params![identity_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(store_error)?
        .ok_or_else(|| StoreError::not_found(format!("identity {identity_id}")))?;
    let sendable = IdentitySendable::from_stored(&sendable)?;
    if !sendable.can_send() {
        return Err(IdentityError::NotSendable {
            identity_id: identity_id.to_owned(),
            sendable: sendable.as_str(),
        });
    }
    tx.execute(
        "UPDATE identities SET is_default = 0, revision = revision + 1
         WHERE account_id = ?1 AND is_default = 1",
        params![account_id],
    )
    .map_err(|e| IdentityError::Store(classify(e, "clear the account's defaults")))?;
    tx.execute(
        "UPDATE identities SET is_default = 1, revision = revision + 1
         WHERE identity_id = ?1 AND account_id = ?2",
        params![identity_id, account_id],
    )
    .map_err(|e| IdentityError::Store(classify(e, "set the account's default")))?;
    Ok(())
}

fn store_error(error: rusqlite::Error) -> IdentityError {
    IdentityError::Store(classify(error, "read an identity"))
}
