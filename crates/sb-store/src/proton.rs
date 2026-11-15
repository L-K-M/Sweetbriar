//! Proton account rows (spec/02 "Proton accounts"): the SQL helpers
//! behind sign-in (plan M2.13b) and the read-only sync's cursors and
//! body failures (M2.15, M2.16). They write inside the caller's
//! transaction only; the credential itself stays in the secret store,
//! named by `credential_ref` (spec/10).

use rusqlite::{OptionalExtension, Transaction, params};
use sb_types::time::Timestamp;

use crate::StoreError;
use crate::schema::classify;

/// `proton_accounts.password_mode` (spec/02): 1 is one login password,
/// 2 a separate mailbox password.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtonPasswordMode {
    Login,
    Mailbox,
}

impl ProtonPasswordMode {
    fn from_stored(value: i64) -> Result<Self, StoreError> {
        match value {
            1 => Ok(Self::Login),
            2 => Ok(Self::Mailbox),
            _ => Err(StoreError::corrupt(format!(
                "a Proton account's password_mode is {value}"
            ))),
        }
    }

    fn as_i64(self) -> i64 {
        match self {
            Self::Login => 1,
            Self::Mailbox => 2,
        }
    }
}

/// `proton_accounts.listing_state` (spec/02).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtonListingState {
    None,
    Listing,
    Complete,
}

impl ProtonListingState {
    fn from_stored(value: &str) -> Result<Self, StoreError> {
        match value {
            "none" => Ok(Self::None),
            "listing" => Ok(Self::Listing),
            "complete" => Ok(Self::Complete),
            _ => Err(StoreError::corrupt(format!(
                "a Proton account's listing_state is '{value}'"
            ))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::None => "none",
            Self::Listing => "listing",
            Self::Complete => "complete",
        }
    }
}

/// One Proton account's durable row (spec/02 `proton_accounts`). The
/// `revision` changes with the cursors, listing state and diagnostics.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtonAccount {
    pub account_id: String,
    pub api_base_url: String,
    pub username: String,
    pub proton_user_id: String,
    pub credential_ref: String,
    pub password_mode: ProtonPasswordMode,
    pub key_generation: i64,
    /// Last applied event id; `None` until the first bootstrap completes.
    pub event_cursor: Option<String>,
    /// Cursor read before the running bootstrap's listing.
    pub pending_cursor: Option<String>,
    pub listing_state: ProtonListingState,
    pub listing_checkpoint: Option<String>,
    pub refresh_count: i64,
    /// Last refusal code and message, redacted (spec/10).
    pub diagnostics_json: Option<String>,
    pub revision: i64,
}

/// A new Proton account's fields, as sign-in learns them. The remaining
/// columns take their defaults.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NewProtonAccount {
    pub account_id: String,
    pub api_base_url: String,
    pub username: String,
    pub proton_user_id: String,
    pub credential_ref: String,
    pub password_mode: ProtonPasswordMode,
}

/// Insert the account's Proton row in the caller's transaction. A
/// Proton user belongs to one account: a sign-in as another user is
/// `Conflict`, never merged (spec/02).
pub fn insert_proton_account(
    tx: &Transaction,
    new: &NewProtonAccount,
) -> Result<ProtonAccount, StoreError> {
    if let Some(existing) = proton_account_by_user_id(tx, &new.proton_user_id)? {
        return Err(StoreError::conflict(format!(
            "Proton user {} already belongs to account {}",
            new.proton_user_id, existing.account_id
        )));
    }
    if proton_account(tx, &new.account_id)?.is_some() {
        return Err(StoreError::conflict(format!(
            "account {} already has a Proton user",
            new.account_id
        )));
    }
    tx.execute(
        "INSERT INTO proton_accounts
         (account_id, api_base_url, username, proton_user_id, credential_ref, password_mode,
          listing_state)
         VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        params![
            new.account_id,
            new.api_base_url,
            new.username,
            new.proton_user_id,
            new.credential_ref,
            new.password_mode.as_i64(),
            ProtonListingState::None.as_str(),
        ],
    )
    .map_err(|e| classify(e, "create the Proton account"))?;
    let stored = proton_account(tx, &new.account_id)?
        .ok_or_else(|| StoreError::corrupt("the Proton account vanished after writing"))?;
    Ok(stored)
}

/// The account's Proton row, or `None`. `Corrupt` for a stored value
/// outside its range.
pub fn proton_account(
    tx: &Transaction,
    account_id: &str,
) -> Result<Option<ProtonAccount>, StoreError> {
    read_account(tx, "account_id", account_id)
}

/// The account a Proton user id belongs to, or `None`: the sign-in
/// refusal's lookup (spec/02).
pub fn proton_account_by_user_id(
    tx: &Transaction,
    proton_user_id: &str,
) -> Result<Option<ProtonAccount>, StoreError> {
    read_account(tx, "proton_user_id", proton_user_id)
}

/// One account read by a fixed key column. `Corrupt` for a stored
/// value outside its range.
fn read_account(
    tx: &Transaction,
    key: &str,
    value: &str,
) -> Result<Option<ProtonAccount>, StoreError> {
    let sql = format!("SELECT {ACCOUNT_COLUMNS} FROM proton_accounts WHERE {key} = ?1");
    let row = tx
        .query_row(&sql, params![value], |row| {
            Ok((
                row.get(0)?,
                row.get(1)?,
                row.get(2)?,
                row.get(3)?,
                row.get(4)?,
                row.get(5)?,
                row.get(6)?,
                row.get(7)?,
                row.get(8)?,
                row.get(9)?,
                row.get(10)?,
                row.get(11)?,
                row.get(12)?,
                row.get(13)?,
            ))
        })
        .optional()
        .map_err(|e| classify(e, "read the Proton account"))?;
    row.map(stored_account).transpose()
}

/// The `proton_accounts` columns both reads share, leading in each
/// query.
const ACCOUNT_COLUMNS: &str = "account_id, api_base_url, username, proton_user_id,
    credential_ref, password_mode, key_generation, event_cursor, pending_cursor,
    listing_state, listing_checkpoint, refresh_count, diagnostics_json, revision";

#[allow(clippy::type_complexity)]
type StoredAccount = (
    String,
    String,
    String,
    String,
    String,
    i64,
    i64,
    Option<String>,
    Option<String>,
    String,
    Option<String>,
    i64,
    Option<String>,
    i64,
);

/// The stored columns as a [`ProtonAccount`], validating the enumerations
/// and the revision.
fn stored_account(row: StoredAccount) -> Result<ProtonAccount, StoreError> {
    let (
        account_id,
        api_base_url,
        username,
        proton_user_id,
        credential_ref,
        password_mode,
        key_generation,
        event_cursor,
        pending_cursor,
        listing_state,
        listing_checkpoint,
        refresh_count,
        diagnostics_json,
        revision,
    ) = row;
    if revision < 1 {
        return Err(StoreError::corrupt(format!(
            "a Proton account's revision is {revision}"
        )));
    }
    Ok(ProtonAccount {
        account_id,
        api_base_url,
        username,
        proton_user_id,
        credential_ref,
        password_mode: ProtonPasswordMode::from_stored(password_mode)?,
        key_generation,
        event_cursor,
        pending_cursor,
        listing_state: ProtonListingState::from_stored(&listing_state)?,
        listing_checkpoint,
        refresh_count,
        diagnostics_json,
        revision,
    })
}

/// The account's row or `NotFound`; every update starts here.
fn require_account(tx: &Transaction, account_id: &str) -> Result<ProtonAccount, StoreError> {
    proton_account(tx, account_id)?
        .ok_or_else(|| StoreError::not_found(format!("account {account_id}")))
}

/// A stale revision is `Conflict`, even when the values match.
fn require_revision(current: &ProtonAccount, expected: i64) -> Result<(), StoreError> {
    if current.revision != expected {
        return Err(StoreError::conflict(format!(
            "the Proton account's revision is {}, not {expected}",
            current.revision
        )));
    }
    Ok(())
}

/// Record the cursor read before a bootstrap listing starts: the state
/// becomes `listing` and the pending cursor holds it, so a change made
/// during the listing is not lost (plan M2.15).
pub fn begin_listing(
    tx: &Transaction,
    account_id: &str,
    pending_cursor: &str,
    expected_revision: i64,
) -> Result<ProtonAccount, StoreError> {
    let current = require_account(tx, account_id)?;
    require_revision(&current, expected_revision)?;
    if current.listing_state == ProtonListingState::Listing
        && current.pending_cursor.as_deref() == Some(pending_cursor)
        && current.listing_checkpoint.is_none()
    {
        return Ok(current);
    }
    tx.execute(
        "UPDATE proton_accounts
         SET pending_cursor = ?2, listing_state = 'listing', listing_checkpoint = NULL,
             revision = revision + 1
         WHERE account_id = ?1",
        params![account_id, pending_cursor],
    )
    .map_err(|e| classify(e, "begin the Proton listing"))?;
    require_account(tx, account_id)
}

/// Record the listing's last committed page position. Only valid while
/// listing, which the schema's CHECK also enforces.
pub fn update_listing_checkpoint(
    tx: &Transaction,
    account_id: &str,
    checkpoint: &str,
    expected_revision: i64,
) -> Result<ProtonAccount, StoreError> {
    let current = require_account(tx, account_id)?;
    require_revision(&current, expected_revision)?;
    if current.listing_state != ProtonListingState::Listing {
        return Err(StoreError::conflict("the Proton account is not listing"));
    }
    if current.listing_checkpoint.as_deref() == Some(checkpoint) {
        return Ok(current);
    }
    tx.execute(
        "UPDATE proton_accounts
         SET listing_checkpoint = ?2, revision = revision + 1
         WHERE account_id = ?1",
        params![account_id, checkpoint],
    )
    .map_err(|e| classify(e, "record the Proton listing checkpoint"))?;
    require_account(tx, account_id)
}

/// Finish the listing: the pending cursor and checkpoint clear, the
/// event cursor takes the listing's starting cursor, and the state
/// becomes `complete` (plan M2.15).
pub fn complete_listing(
    tx: &Transaction,
    account_id: &str,
    event_cursor: &str,
    expected_revision: i64,
) -> Result<ProtonAccount, StoreError> {
    let current = require_account(tx, account_id)?;
    require_revision(&current, expected_revision)?;
    if current.listing_state == ProtonListingState::Complete
        && current.event_cursor.as_deref() == Some(event_cursor)
    {
        return Ok(current);
    }
    tx.execute(
        "UPDATE proton_accounts
         SET listing_state = 'complete', pending_cursor = NULL, listing_checkpoint = NULL,
             event_cursor = ?2, revision = revision + 1
         WHERE account_id = ?1",
        params![account_id, event_cursor],
    )
    .map_err(|e| classify(e, "complete the Proton listing"))?;
    require_account(tx, account_id)
}

/// Record the last applied event id (plan M2.15).
pub fn set_event_cursor(
    tx: &Transaction,
    account_id: &str,
    event_cursor: &str,
    expected_revision: i64,
) -> Result<ProtonAccount, StoreError> {
    let current = require_account(tx, account_id)?;
    require_revision(&current, expected_revision)?;
    if current.event_cursor.as_deref() == Some(event_cursor) {
        return Ok(current);
    }
    tx.execute(
        "UPDATE proton_accounts
         SET event_cursor = ?2, revision = revision + 1
         WHERE account_id = ?1",
        params![account_id, event_cursor],
    )
    .map_err(|e| classify(e, "record the Proton event cursor"))?;
    require_account(tx, account_id)
}

/// Record the last refusal's redacted diagnostics, or clear them
/// (spec/10).
pub fn set_diagnostics(
    tx: &Transaction,
    account_id: &str,
    diagnostics_json: Option<&str>,
    expected_revision: i64,
) -> Result<ProtonAccount, StoreError> {
    let current = require_account(tx, account_id)?;
    require_revision(&current, expected_revision)?;
    if current.diagnostics_json.as_deref() == diagnostics_json {
        return Ok(current);
    }
    tx.execute(
        "UPDATE proton_accounts
         SET diagnostics_json = ?2, revision = revision + 1
         WHERE account_id = ?1",
        params![account_id, diagnostics_json],
    )
    .map_err(|e| classify(e, "record the Proton diagnostics"))?;
    require_account(tx, account_id)
}

/// `proton_body_failures.reason` (spec/02).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ProtonBodyFailureReason {
    NoKey,
    Decrypt,
    Attachment,
    TooLarge,
    Reconstruct,
}

impl ProtonBodyFailureReason {
    fn from_stored(value: &str) -> Result<Self, StoreError> {
        match value {
            "no_key" => Ok(Self::NoKey),
            "decrypt" => Ok(Self::Decrypt),
            "attachment" => Ok(Self::Attachment),
            "too_large" => Ok(Self::TooLarge),
            "reconstruct" => Ok(Self::Reconstruct),
            _ => Err(StoreError::corrupt(format!(
                "a Proton body failure's reason is '{value}'"
            ))),
        }
    }

    fn as_str(self) -> &'static str {
        match self {
            Self::NoKey => "no_key",
            Self::Decrypt => "decrypt",
            Self::Attachment => "attachment",
            Self::TooLarge => "too_large",
            Self::Reconstruct => "reconstruct",
        }
    }
}

/// One message's body failure (spec/02 `proton_body_failures`). It is
/// retried once the account's `key_generation` moves past
/// `key_generation` (plan M2.16).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProtonBodyFailure {
    pub message_id: i64,
    pub reason: ProtonBodyFailureReason,
    pub detail: Option<String>,
    pub attempts: i64,
    pub key_generation: i64,
    pub last_at: i64,
}

/// Record a body failure, or increment its attempts when the same
/// message fails again.
pub fn record_body_failure(
    tx: &Transaction,
    message_id: i64,
    reason: ProtonBodyFailureReason,
    detail: Option<&str>,
    key_generation: i64,
    now: Timestamp,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO proton_body_failures
         (message_id, reason, detail, attempts, key_generation, last_at)
         VALUES (?1, ?2, ?3, 1, ?4, ?5)
         ON CONFLICT(message_id) DO UPDATE SET
           reason = excluded.reason, detail = excluded.detail,
           attempts = proton_body_failures.attempts + 1,
           key_generation = excluded.key_generation,
           last_at = excluded.last_at",
        params![
            message_id,
            reason.as_str(),
            detail,
            key_generation,
            now.as_millis(),
        ],
    )
    .map_err(|e| classify(e, "record the Proton body failure"))?;
    Ok(())
}

/// One message's failure, or `None`.
pub fn body_failure(
    tx: &Transaction,
    message_id: i64,
) -> Result<Option<ProtonBodyFailure>, StoreError> {
    let row = tx
        .query_row(
            "SELECT message_id, reason, detail, attempts, key_generation, last_at
             FROM proton_body_failures WHERE message_id = ?1",
            params![message_id],
            |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, i64>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)?,
                ))
            },
        )
        .optional()
        .map_err(|e| classify(e, "read the Proton body failure"))?;
    row.map(stored_failure).transpose()
}

/// Clear a message's failure once its body is acquired or it is
/// abandoned. An unknown message is `NotFound`.
pub fn clear_body_failure(tx: &Transaction, message_id: i64) -> Result<(), StoreError> {
    let removed = tx
        .execute(
            "DELETE FROM proton_body_failures WHERE message_id = ?1",
            params![message_id],
        )
        .map_err(|e| classify(e, "clear the Proton body failure"))?;
    if removed == 0 {
        return Err(StoreError::not_found(format!(
            "the body failure of message {message_id}"
        )));
    }
    Ok(())
}

/// The stored columns as a [`ProtonBodyFailure`].
fn stored_failure(
    row: (i64, String, Option<String>, i64, i64, i64),
) -> Result<ProtonBodyFailure, StoreError> {
    let (message_id, reason, detail, attempts, key_generation, last_at) = row;
    Ok(ProtonBodyFailure {
        message_id,
        reason: ProtonBodyFailureReason::from_stored(&reason)?,
        detail,
        attempts,
        key_generation,
        last_at,
    })
}

#[cfg(test)]
mod tests;
