//! What an IMAP account's sync worker connects with, and the account
//! states its connection drives (spec/04 "Account states and health").
//! Connection pauses sit beside the credential pauses (spec/10): each
//! kind is lifted only by its own recovery, so a successful connect
//! never clears a credential pause and a stored secret never clears a
//! connection pause.

use rusqlite::{OptionalExtension, Transaction, params};

use super::EndpointSecurity;
use crate::StoreError;
use crate::schema::classify;

/// Why an IMAP account cannot connect (spec/04's table), shown as
/// `state_reason` on the health surface. The worker keeps retrying a
/// refused connection; the other two wait for the user.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ConnectionPause {
    /// The connection was refused: Bridge is not running.
    BridgeNotRunning,
    /// Bridge refused the login.
    LoginRejected,
    /// The certificate Bridge presents is not the pinned one; the
    /// re-trust flow (spec/10) resolves it.
    CertificateChanged,
}

impl ConnectionPause {
    const ALL: [Self; 3] = [
        Self::BridgeNotRunning,
        Self::LoginRejected,
        Self::CertificateChanged,
    ];

    /// spec/04's "User sees" text. It is persisted and matched back, so
    /// rewording one needs a migration of the stored rows; the tests
    /// pin the exact strings.
    pub fn reason(self) -> &'static str {
        match self {
            Self::BridgeNotRunning => "Bridge isn't running",
            Self::LoginRejected => "Bridge password changed or account signed out in Bridge",
            Self::CertificateChanged => "Bridge's certificate changed",
        }
    }

    fn from_reason(reason: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|pause| pause.reason() == reason)
    }
}

/// An IMAP account's provider, which selects its policy (spec/04).
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum ImapProvider {
    ProtonBridge,
    GenericImap,
}

/// What a sync worker connects with (spec/02 `imap_endpoints`). The
/// credential itself stays in the secret store, named by
/// `credential_ref` (spec/10).
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ImapEndpoint {
    pub provider: ImapProvider,
    pub port: u16,
    pub security: EndpointSecurity,
    pub username: String,
    pub credential_ref: String,
    /// The pinned leaf certificate's DER SHA-256 in hex; Bridge always
    /// has one (spec/10).
    pub trusted_cert_sha256: Option<String>,
}

/// The account's endpoint. `NotFound` for an account without one (the
/// local archive, or none); `Corrupt` for a stored value outside its
/// range.
pub fn imap_endpoint(tx: &Transaction, account_id: &str) -> Result<ImapEndpoint, StoreError> {
    let row: Option<(String, i64, String, String, String, Option<String>)> = tx
        .query_row(
            "SELECT a.provider, e.imap_port, e.imap_security, e.username, e.credential_ref,
                    e.trusted_cert_sha256
             FROM imap_endpoints e JOIN accounts a ON a.account_id = e.account_id
             WHERE e.account_id = ?1",
            params![account_id],
            |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                    row.get(5)?,
                ))
            },
        )
        .optional()
        .map_err(|e| classify(e, "read an IMAP endpoint"))?;
    let Some((provider, port, security, username, credential_ref, pin)) = row else {
        return Err(StoreError::not_found(format!(
            "the IMAP endpoint of account {account_id}"
        )));
    };
    let provider = match provider.as_str() {
        "proton_bridge" => ImapProvider::ProtonBridge,
        "generic_imap" => ImapProvider::GenericImap,
        _ => {
            return Err(StoreError::corrupt(
                "an IMAP endpoint of a non-IMAP provider",
            ));
        }
    };
    let port = u16::try_from(port)
        .ok()
        .filter(|port| *port != 0)
        .ok_or_else(|| StoreError::corrupt("a stored IMAP port is out of range"))?;
    let security = match security.as_str() {
        "tls" => EndpointSecurity::Tls,
        "starttls" => EndpointSecurity::StartTls,
        _ => return Err(StoreError::corrupt("a stored IMAP security is unknown")),
    };
    Ok(ImapEndpoint {
        provider,
        port,
        security,
        username,
        credential_ref,
        trusted_cert_sha256: pin,
    })
}

/// The IMAP accounts a sync worker runs for, in sidebar order: active
/// ones, and ones paused for their connection (the worker keeps
/// retrying, spec/04). A user-paused or disconnected account, and one
/// waiting for its credential, has no worker.
pub fn syncable_accounts(tx: &Transaction) -> Result<Vec<String>, StoreError> {
    let read = |e| classify(e, "read the syncable accounts");
    let mut statement = tx
        .prepare(
            "SELECT account_id, state, state_reason FROM accounts
             WHERE kind = 'imap' AND state IN ('active', 'needs_attention')
             ORDER BY sort_order",
        )
        .map_err(read)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, Option<String>>(2)?,
            ))
        })
        .map_err(read)?;
    let mut accounts = Vec::new();
    for row in rows {
        let (account_id, state, reason) = row.map_err(read)?;
        let connection_paused = reason
            .as_deref()
            .and_then(ConnectionPause::from_reason)
            .is_some();
        if state == "active" || connection_paused {
            accounts.push(account_id);
        }
    }
    Ok(accounts)
}

/// Record a connection pause (spec/04). Only an active or
/// connection-paused account takes it: a credential pause, a user
/// pause or a disconnect stays, so the account's real blocker is the
/// one shown (`Conflict`). Repeating the current pause writes nothing.
/// `NotFound` for an unknown account.
pub fn pause_for_connection(
    tx: &Transaction,
    account_id: &str,
    pause: ConnectionPause,
) -> Result<(), StoreError> {
    match current(tx, account_id)? {
        Current::Active => {}
        Current::Connection(current) if current == pause => return Ok(()),
        Current::Connection(_) => {}
        Current::Other => {
            return Err(StoreError::conflict(
                "the account is paused for another reason",
            ));
        }
    }
    tx.execute(
        "UPDATE accounts SET state = 'needs_attention', state_reason = ?2,
             revision = revision + 1
         WHERE account_id = ?1",
        params![account_id, pause.reason()],
    )
    .map_err(|e| classify(e, "pause the account's connection"))?;
    Ok(())
}

/// A connection succeeded: lift a connection pause back to `active`.
/// Answers whether the account changed. An active account is left as
/// it is, and any other state (a credential pause, a user pause, a
/// disconnect) too: only its own recovery lifts it. `NotFound` for an
/// unknown account.
pub fn clear_connection_pause(tx: &Transaction, account_id: &str) -> Result<bool, StoreError> {
    let Current::Connection(_) = current(tx, account_id)? else {
        return Ok(false);
    };
    tx.execute(
        "UPDATE accounts SET state = 'active', state_reason = NULL, revision = revision + 1
         WHERE account_id = ?1",
        params![account_id],
    )
    .map_err(|e| classify(e, "lift the account's connection pause"))?;
    Ok(true)
}

/// The account's state as the connection transitions see it.
enum Current {
    Active,
    Connection(ConnectionPause),
    /// A credential pause, a user pause, a disconnect, or an unknown
    /// `needs_attention` reason.
    Other,
}

fn current(tx: &Transaction, account_id: &str) -> Result<Current, StoreError> {
    let (state, reason): (String, Option<String>) = tx
        .query_row(
            "SELECT state, state_reason FROM accounts WHERE account_id = ?1",
            params![account_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )
        .optional()
        .map_err(|e| classify(e, "read the account state"))?
        .ok_or_else(|| StoreError::not_found(format!("account {account_id}")))?;
    if state == "active" {
        return Ok(Current::Active);
    }
    if state != "needs_attention" {
        return Ok(Current::Other);
    }
    // A credential pause is "other" here: only its retry lifts it.
    Ok(reason
        .as_deref()
        .and_then(ConnectionPause::from_reason)
        .map_or(Current::Other, Current::Connection))
}

#[cfg(test)]
mod tests;
