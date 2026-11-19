//! Identity writes and From-resolution reads (spec/02 `identities`,
//! spec/05 "Identities and From resolution").
//!
//! The writes are the two identity commands' rows: an upsert for the
//! accounts whose addresses the user maintains, and the one-default
//! flip. Proton accounts are refused here because their identities come
//! from Proton's addresses and a later refresh would overwrite a local
//! edit (M2.12). The read gathers a source occurrence's resolver inputs
//! offline from `message_headers` alone: no acquisition or body is
//! touched, so a reply resolves before its body is downloaded.

use rusqlite::{OptionalExtension, Transaction, params};

use crate::StoreError;
use crate::accounts::IdentitySendable;
use crate::messages::{self, ReaderAddress};
use crate::query::AddressOperand;
use crate::schema::classify;
use crate::sidebar::AccountKind;

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
    let display_name = write.display_name.trim();
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
            || name != display_name
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
                display_name,
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
    // Only rows whose flag changes move their revision: re-setting the
    // current default writes nothing.
    tx.execute(
        "UPDATE identities SET is_default = 0, revision = revision + 1
         WHERE account_id = ?1 AND is_default = 1 AND identity_id <> ?2",
        params![account_id, identity_id],
    )
    .map_err(|e| IdentityError::Store(classify(e, "clear the account's defaults")))?;
    tx.execute(
        "UPDATE identities SET is_default = 1, revision = revision + 1
         WHERE identity_id = ?1 AND account_id = ?2 AND is_default = 0",
        params![identity_id, account_id],
    )
    .map_err(|e| IdentityError::Store(classify(e, "set the account's default")))?;
    Ok(())
}

/// The headers a reply's identity and recipients resolve from, exactly
/// as `message_headers` stores them. Absent fields are empty lists.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SourceMessageHeaders {
    pub from: Vec<ReaderAddress>,
    pub reply_to: Vec<ReaderAddress>,
    pub to: Vec<ReaderAddress>,
    pub cc: Vec<ReaderAddress>,
    /// `Delivered-To`/`X-Original-To` values: plain address strings.
    pub delivered_to: Vec<String>,
}

/// A source occurrence's gathered inputs (spec/05): its account, the
/// message's headers, and the identity a recorded send attempt used.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SourceIdentityInputs {
    pub account_id: String,
    pub account_kind: AccountKind,
    pub headers: SourceMessageHeaders,
    pub sent_identity_id: Option<String>,
}

/// One account identity as resolution sees it (spec/02 `identities`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityRoute {
    pub identity_id: String,
    pub account_id: String,
    pub email: String,
    pub is_default: bool,
    pub sendable: IdentitySendable,
    pub account_kind: AccountKind,
}

/// Everything [`crate::identities`] resolution needs, gathered in one
/// short read (spec/05).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IdentityResolutionInputs {
    /// `None` for a new message.
    pub source: Option<SourceIdentityInputs>,
    pub identities: Vec<IdentityRoute>,
    pub selected_account_id: Option<String>,
    pub global_default_identity_id: Option<String>,
}

fn store_error(error: rusqlite::Error) -> IdentityError {
    IdentityError::Store(classify(error, "read an identity"))
}

fn mailbox_account(tx: &Transaction, mailbox_id: i64) -> Result<String, StoreError> {
    tx.query_row(
        "SELECT account_id FROM mailboxes WHERE mailbox_id = ?1",
        params![mailbox_id],
        |row| row.get(0),
    )
    .optional()
    .map_err(|e| classify(e, "read the selected mailbox"))?
    .ok_or_else(|| StoreError::not_found(format!("mailbox {mailbox_id}")))
}

fn require_account(tx: &Transaction, account_id: &str) -> Result<(), StoreError> {
    let exists = tx
        .query_row(
            "SELECT EXISTS(SELECT 1 FROM accounts WHERE account_id = ?1)",
            params![account_id],
            |row| row.get::<_, bool>(0),
        )
        .map_err(|e| classify(e, "read the selected account"))?;
    if exists {
        Ok(())
    } else {
        Err(StoreError::not_found(format!("account {account_id}")))
    }
}

fn account_kind(kind: &str) -> Result<AccountKind, StoreError> {
    match kind {
        "local" => Ok(AccountKind::Local),
        "imap" => Ok(AccountKind::Imap),
        "proton" => Ok(AccountKind::Proton),
        _ => Err(StoreError::corrupt(format!(
            "an account's kind is '{kind}'"
        ))),
    }
}

fn delivered_to(json: Option<String>) -> Result<Vec<String>, StoreError> {
    let Some(json) = json else {
        return Ok(Vec::new());
    };
    let parsed: serde_json::Value = serde_json::from_str(&json)
        .map_err(|_| StoreError::corrupt("delivered-to values are not JSON"))?;
    let list = parsed
        .as_array()
        .ok_or_else(|| StoreError::corrupt("delivered-to values are not a list"))?;
    list.iter()
        .map(|value| {
            value
                .as_str()
                .map(str::to_owned)
                .ok_or_else(|| StoreError::corrupt("a delivered-to value is not text"))
        })
        .collect()
}

/// The resolver's whole input, gathered from one occurrence (or none for
/// a new message). Header-only: it reads `message_headers` and never an
/// acquisition. `selected_account_id` and `selected_mailbox_id` are the
/// compose context; a mailbox resolves to its account, and the two must
/// agree when both are given. The global default is `identity.default`
/// (spec/07); malformed stored text is `Corrupt`, an absent or empty
/// one selects nothing.
pub fn identity_resolution_inputs(
    tx: &Transaction,
    source_occurrence_id: Option<i64>,
    selected_account_id: Option<&str>,
    selected_mailbox_id: Option<i64>,
) -> Result<IdentityResolutionInputs, StoreError> {
    let err = |e| classify(e, "gather identity resolution inputs");
    let source = match source_occurrence_id {
        Some(occurrence_id) => {
            // Headers are left-joined: a message fetched before any body
            // still resolves, and an absent row is empty rather than
            // damage.
            let row = tx
                .query_row(
                    "SELECT o.account_id, a.kind, h.from_json, h.reply_to_json, h.to_json,
                            h.cc_json, h.delivered_to_json, h.header_message_id
                     FROM occurrences o JOIN accounts a USING (account_id)
                     LEFT JOIN message_headers h ON h.message_id = o.message_id
                     WHERE o.occurrence_id = ?1",
                    params![occurrence_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, String>(1)?,
                            row.get::<_, Option<String>>(2)?,
                            row.get::<_, Option<String>>(3)?,
                            row.get::<_, Option<String>>(4)?,
                            row.get::<_, Option<String>>(5)?,
                            row.get::<_, Option<String>>(6)?,
                            row.get::<_, Option<String>>(7)?,
                        ))
                    },
                )
                .optional()
                .map_err(err)?;
            let Some((account_id, kind, from, reply_to, to, cc, delivered_json, message_id)) = row
            else {
                return Err(StoreError::not_found(format!("occurrence {occurrence_id}")));
            };
            let (from, reply_to, to, cc, delivered) = (
                messages::addresses(from)?,
                messages::addresses(reply_to)?,
                messages::addresses(to)?,
                messages::addresses(cc)?,
                delivered_to(delivered_json)?,
            );
            let sent_identity_id = match message_id.as_deref() {
                Some(message_id) => tx
                    .query_row(
                        "SELECT identity_id FROM send_attempts
                         WHERE message_id_header = ?1
                         ORDER BY created_at DESC, attempt_id DESC LIMIT 1",
                        params![message_id],
                        |row| row.get(0),
                    )
                    .optional()
                    .map_err(err)?,
                None => None,
            };
            Some(SourceIdentityInputs {
                account_id,
                account_kind: account_kind(&kind)?,
                headers: SourceMessageHeaders {
                    from,
                    reply_to,
                    to,
                    cc,
                    delivered_to: delivered,
                },
                sent_identity_id,
            })
        }
        None => None,
    };

    let selected_account_id = match (selected_account_id, selected_mailbox_id) {
        (Some(account), mailbox) => {
            require_account(tx, account)?;
            if let Some(mailbox) = mailbox
                && mailbox_account(tx, mailbox)? != account
            {
                return Err(StoreError::InvalidInput {
                    message: "selected account and mailbox disagree".to_owned(),
                });
            }
            Some(account.to_owned())
        }
        (None, Some(mailbox)) => Some(mailbox_account(tx, mailbox)?),
        (None, None) => None,
    };

    let mut statement = tx
        .prepare(
            "SELECT i.identity_id, i.account_id, i.email, i.is_default, i.sendable, a.kind
             FROM identities i JOIN accounts a USING (account_id)
             ORDER BY a.sort_order, a.account_id, i.identity_id",
        )
        .map_err(err)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, String>(5)?,
            ))
        })
        .map_err(err)?;
    let mut identities = Vec::new();
    for row in rows {
        let (identity_id, account_id, email, is_default, sendable, kind) = row.map_err(err)?;
        identities.push(IdentityRoute {
            identity_id,
            account_id,
            email,
            is_default: is_default != 0,
            sendable: IdentitySendable::from_stored(&sendable).map_err(|_| {
                StoreError::corrupt(format!("an identity's sendable state is '{sendable}'"))
            })?,
            account_kind: account_kind(&kind)?,
        });
    }

    let global_default_identity_id = match tx
        .query_row(
            "SELECT value_json FROM settings WHERE key = 'identity.default'",
            [],
            |row| row.get::<_, String>(0),
        )
        .optional()
        .map_err(err)?
    {
        Some(raw) => {
            let parsed: String = serde_json::from_str(&raw)
                .map_err(|_| StoreError::corrupt("identity.default is not a JSON string"))?;
            if parsed.is_empty() {
                None
            } else {
                Some(parsed)
            }
        }
        None => None,
    };

    Ok(IdentityResolutionInputs {
        source,
        identities,
        selected_account_id,
        global_default_identity_id,
    })
}

#[cfg(test)]
mod tests;
