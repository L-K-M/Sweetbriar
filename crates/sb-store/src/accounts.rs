//! Account rows (spec/02 `accounts`, `imap_endpoints`, `identities`):
//! the SQL helpers behind `add_bridge_account` (spec/07). They write
//! rows inside the caller's transaction only — probing, secrets and
//! ordering live in `sb-resources`; the credential itself never
//! appears here, only its `credential_ref` (spec/10).

use rusqlite::Transaction;
use rusqlite::params;
use sb_types::time::Timestamp;

use crate::StoreError;
use crate::appearance::initialize_account_appearance;
use crate::schema::classify;

/// `imap_security`/`smtp_security` (spec/02).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum EndpointSecurity {
    Tls,
    StartTls,
}

impl EndpointSecurity {
    fn as_str(self) -> &'static str {
        match self {
            Self::Tls => "tls",
            Self::StartTls => "starttls",
        }
    }
}

/// `identities.sendable` (spec/02): what the From list may offer.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum IdentitySendable {
    Validated,
    Provisional,
    Unavailable,
}

impl IdentitySendable {
    fn as_str(self) -> &'static str {
        match self {
            Self::Validated => "validated",
            Self::Provisional => "provisional",
            Self::Unavailable => "unavailable",
        }
    }
}

/// `imap_endpoints.address_mode`, Proton Bridge only (spec/02).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum AddressMode {
    Combined,
    Split,
}

impl AddressMode {
    fn as_str(self) -> &'static str {
        match self {
            Self::Combined => "combined",
            Self::Split => "split",
        }
    }
}

/// One Bridge account's durable rows. Hosts are the literal
/// `127.0.0.1` (spec/10 loopback only); `credential_ref` names the
/// platform secret-store item and is the only credential material the
/// database ever holds.
pub struct NewBridgeAccount {
    pub account_id: String,
    pub display_name: String,
    pub endpoint: NewImapEndpoint,
    pub identities: Vec<NewIdentity>,
}

pub struct NewImapEndpoint {
    pub imap_port: i64,
    pub imap_security: EndpointSecurity,
    pub smtp_port: i64,
    pub smtp_security: EndpointSecurity,
    pub username: String,
    pub credential_ref: String,
    pub trusted_cert_sha256: String,
    /// Proton Bridge only; `None` stays NULL.
    pub address_mode: Option<AddressMode>,
    /// The probe's compatibility ledger (spec/04), already serialized.
    pub capabilities_json: String,
}

pub struct NewIdentity {
    pub identity_id: String,
    pub email: String,
    pub display_name: String,
    pub is_default: bool,
    pub sendable: IdentitySendable,
}

/// Insert the account, its endpoint and its identities plus the
/// default appearance assignment, in the caller's transaction. The
/// account lands `needs_attention` awaiting its credential: until the
/// secret is stored there is no safe active state, and a write that
/// fails before activation leaves a recoverable pause rather than an
/// active account with no credential (spec/10).
pub fn insert_bridge_account(
    tx: &Transaction,
    new: &NewBridgeAccount,
    now: Timestamp,
) -> Result<(), StoreError> {
    tx.execute(
        "INSERT INTO accounts
         (account_id, kind, provider, display_name, state, state_reason, sort_order, created_at)
         VALUES (?1, 'imap', 'proton_bridge', ?2, 'needs_attention', ?5, ?3, ?4)",
        params![
            new.account_id,
            new.display_name,
            next_account_sort_order(tx)?,
            now.as_millis(),
            "the credential is not yet stored"
        ],
    )
    .map_err(|e| classify(e, "create the account"))?;
    let endpoint = &new.endpoint;
    tx.execute(
        "INSERT INTO imap_endpoints
         (account_id, imap_host, imap_port, imap_security,
          smtp_host, smtp_port, smtp_security,
          username, credential_ref, trusted_cert_sha256,
          server_saves_sent, address_mode, capabilities_json)
         VALUES (?1, '127.0.0.1', ?2, ?3, '127.0.0.1', ?4, ?5, ?6, ?7, ?8, 1, ?9, ?10)",
        params![
            new.account_id,
            endpoint.imap_port,
            endpoint.imap_security.as_str(),
            endpoint.smtp_port,
            endpoint.smtp_security.as_str(),
            endpoint.username,
            endpoint.credential_ref,
            endpoint.trusted_cert_sha256,
            endpoint.address_mode.map(AddressMode::as_str),
            endpoint.capabilities_json,
        ],
    )
    .map_err(|e| classify(e, "create the endpoint"))?;
    for identity in &new.identities {
        tx.execute(
            "INSERT INTO identities
             (identity_id, account_id, email, display_name, is_default, sendable)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![
                identity.identity_id,
                new.account_id,
                identity.email,
                identity.display_name,
                i64::from(identity.is_default),
                identity.sendable.as_str(),
            ],
        )
        .map_err(|e| classify(e, "create an identity"))?;
    }
    initialize_account_appearance(tx, &new.account_id)?;
    Ok(())
}

/// Flip an account to `needs_attention` with `reason` (spec/04's
/// health surface shows it). Never deletes the credential or any rows
/// (spec/10).
pub fn pause_account(tx: &Transaction, account_id: &str, reason: &str) -> Result<(), StoreError> {
    let changed = tx
        .execute(
            "UPDATE accounts
             SET state = 'needs_attention', state_reason = ?2, revision = revision + 1
             WHERE account_id = ?1",
            params![account_id, reason],
        )
        .map_err(|e| classify(e, "pause the account"))?;
    if changed == 0 {
        return Err(StoreError::not_found(format!("account {account_id}")));
    }
    Ok(())
}

/// Return a credential-paused account to `active` after the secret was
/// stored by its retained `credential_ref` (spec/10's retry path).
/// `Conflict` when the account is not `needs_attention`.
pub fn reactivate_account(tx: &Transaction, account_id: &str) -> Result<(), StoreError> {
    let changed = tx
        .execute(
            "UPDATE accounts
             SET state = 'active', state_reason = NULL, revision = revision + 1
             WHERE account_id = ?1 AND state = 'needs_attention'",
            params![account_id],
        )
        .map_err(|e| classify(e, "reactivate the account"))?;
    if changed == 0 {
        let exists: bool = tx
            .query_row(
                "SELECT EXISTS(SELECT 1 FROM accounts WHERE account_id = ?1)",
                params![account_id],
                |row| row.get(0),
            )
            .map_err(|e| classify(e, "check the account"))?;
        return Err(if exists {
            StoreError::conflict("the account is not awaiting its credential")
        } else {
            StoreError::not_found(format!("account {account_id}"))
        });
    }
    Ok(())
}

/// The sidebar slot after every existing account (`accounts.sort_order`).
pub fn next_account_sort_order(tx: &Transaction) -> Result<i64, StoreError> {
    tx.query_row(
        "SELECT COALESCE(MAX(sort_order), -1) + 1 FROM accounts",
        [],
        |row| row.get(0),
    )
    .map_err(|e| classify(e, "read the account order"))
}

/// The account's current `state` (spec/02).
pub fn account_state(tx: &Transaction, account_id: &str) -> Result<String, StoreError> {
    tx.query_row(
        "SELECT state FROM accounts WHERE account_id = ?1",
        params![account_id],
        |row| row.get(0),
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => {
            StoreError::not_found(format!("account {account_id}"))
        }
        other => classify(other, "read the account state"),
    })
}

/// The account's stored `credential_ref` (spec/02): the retry path
/// re-sets the secret by it after a `needs_attention` pause.
pub fn credential_ref(tx: &Transaction, account_id: &str) -> Result<String, StoreError> {
    tx.query_row(
        "SELECT credential_ref FROM imap_endpoints WHERE account_id = ?1",
        params![account_id],
        |row| row.get(0),
    )
    .map_err(|e| match e {
        rusqlite::Error::QueryReturnedNoRows => {
            StoreError::not_found(format!("account {account_id}"))
        }
        other => classify(other, "read the credential reference"),
    })
}
