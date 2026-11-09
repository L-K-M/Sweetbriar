//! `add_bridge_account` (spec/07; rows spec/02, credentials and TLS
//! spec/10): probe the pinned loopback endpoint, commit the rows with
//! the command's receipt, then persist the credential by reference. A
//! replayed idempotency key returns the stored receipt before any probe
//! or secret write. A failed probe creates nothing; a secret-store
//! failure pauses the new account rather than deleting rows or
//! credentials — the stored `credential_ref` is the retry path.
//!
//! The secret store is synchronous (spec/07: a sync callback interface)
//! and can wait on keychain IPC or a Secret Service unlock prompt, so
//! these async flows never call it on their own polling thread. The
//! caller passes a `store_secret` step that runs `SecretStore::set`
//! where blocking is allowed — the engine's counted blocking boundary
//! (spec/01) — and the flow awaits it.

use std::fmt;
use std::future::Future;
use std::net::{IpAddr, Ipv4Addr};

use sb_imap::{
    CertificatePin, ConnectOptions, Connection, HostPolicy, ImapError, MailboxSummary, Password,
};
use sb_platform::{Secret, SecretError, valid_credential_ref};
use sb_store::accounts::{
    AddressMode, CredentialPause, EndpointSecurity, IdentitySendable, NewBridgeAccount,
    NewIdentity, NewImapEndpoint, credential_pause, credential_ref, insert_bridge_account,
    pause_for_credential, reactivate_after_credential,
};
use sb_store::{Receipt, StoreError, Transaction, WritePriority, replay, run_once};

use crate::Resources;

/// The command name `add_bridge_account` receipts are stored under.
pub const ADD_BRIDGE_ACCOUNT: &str = "add_bridge_account";

/// The receipt field naming the account (`{"account_id": ...}`).
const RECEIPT_ACCOUNT_ID: &str = "account_id";

/// The Bridge preset's peer (spec/10): the literal `127.0.0.1` only —
/// `::1` is refused because Bridge's certificate names only the IPv4
/// address.
const BRIDGE_HOST: IpAddr = IpAddr::V4(Ipv4Addr::LOCALHOST);

/// Everything the onboarding sheet hands `add_bridge_account` for the
/// Proton Bridge preset. `password` is the Bridge-generated credential
/// (not the Proton password), moved in once and never copied out.
pub struct BridgeAccountParams {
    pub display_name: String,
    pub username: String,
    pub password: Secret,
    pub imap_port: u16,
    pub imap_security: EndpointSecurity,
    pub smtp_port: u16,
    pub smtp_security: EndpointSecurity,
    /// The confirmed leaf-certificate pin (spec/10): from an imported
    /// `cert.pem` or a fingerprint the user confirmed.
    pub pin: CertificatePin,
    /// Proton Bridge only; `None` stays NULL.
    pub address_mode: Option<AddressMode>,
    /// The from-addresses. At least one; at most one `is_default`
    /// (the first becomes it when none is marked).
    pub identities: Vec<BridgeIdentity>,
}

/// One offered from-address (spec/02 `identities`).
pub struct BridgeIdentity {
    pub email: String,
    pub display_name: String,
    pub is_default: bool,
    /// User-provisioned at setup, so `Provisional` unless the caller
    /// has backend confirmation.
    pub sendable: IdentitySendable,
}

/// The outcome of [`Resources::add_bridge_account`], first run or
/// replay alike.
#[derive(Debug)]
pub struct AddedAccount {
    pub account_id: String,
    /// The stored receipt: `{"account_id": ...}` (spec/07).
    pub receipt: Receipt,
    /// `Some(pause)` when the account waits for its credential: the
    /// pause the row actually holds (the precise one, or `NotStored`
    /// when recording it or the activation failed), and
    /// [`Resources::store_account_credential`] retries by the stored
    /// reference. A replay reports the account's current pause.
    pub needs_attention: Option<CredentialPause>,
}

/// `add_bridge_account`'s typed failures, grouped by what setup can do
/// about them. A `Secret` never appears in any variant, message or log
/// (spec/10).
#[derive(Debug)]
pub enum BridgeAccountError {
    /// Input the sheet can fix; `field` names it (spec/07).
    InvalidInput {
        field: &'static str,
        message: String,
    },
    /// The certificate Bridge presents is not the pinned one;
    /// `presented_sha256` is its DER SHA-256 in lowercase hex, the form
    /// the pin is stored in (spec/10). Nothing was stored.
    PinMismatch { presented_sha256: String },
    /// Bridge refused the username or the Bridge password. Nothing was
    /// stored.
    LoginRefused,
    /// Nothing answered in time or the connection dropped: Bridge not
    /// running, or not on this port (spec/04). Nothing was stored.
    Unreachable(ImapError),
    /// The endpoint answered but not as Bridge does: a protocol fault,
    /// a refused upgrade or an exceeded budget. Nothing was stored.
    Protocol(ImapError),
    /// The credential store refused the retry's `set`, with any
    /// `SecretError` kind; the account keeps its credential pause
    /// (spec/10's kinds only).
    SecretStore(SecretError),
    /// A durable read or write failed, or the key was spent on another
    /// command (`Conflict`).
    Store(StoreError),
}

impl fmt::Display for BridgeAccountError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput { field, message } => {
                write!(f, "invalid account parameter {field}: {message}")
            }
            Self::PinMismatch { presented_sha256 } => write!(
                f,
                "the certificate Bridge presents (SHA-256 {presented_sha256}) is not the pinned one"
            ),
            Self::LoginRefused => f.write_str("Bridge refused the username or the Bridge password"),
            Self::Unreachable(error) => write!(f, "Bridge did not answer: {error}"),
            Self::Protocol(error) => {
                write!(
                    f,
                    "the endpoint does not behave like Proton Bridge: {error}"
                )
            }
            Self::SecretStore(error) => {
                write!(
                    f,
                    "the credential store failed: {}",
                    pause_for(*error).reason()
                )
            }
            Self::Store(error) => write!(f, "the account store failed: {error}"),
        }
    }
}

impl std::error::Error for BridgeAccountError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Unreachable(error) | Self::Protocol(error) => Some(error),
            Self::SecretStore(error) => Some(error),
            Self::Store(error) => Some(error),
            Self::InvalidInput { .. } | Self::PinMismatch { .. } | Self::LoginRefused => None,
        }
    }
}

/// Group a probe failure by what setup can do about it.
fn classify_probe(error: ImapError) -> BridgeAccountError {
    match error {
        ImapError::PinMismatch { presented_sha256 } => {
            BridgeAccountError::PinMismatch { presented_sha256 }
        }
        ImapError::LoginRefused => BridgeAccountError::LoginRefused,
        ImapError::StartTlsMissing => BridgeAccountError::InvalidInput {
            field: "imap_security",
            message: "the endpoint does not offer STARTTLS; choose SSL (implicit TLS)".to_owned(),
        },
        ImapError::Io { .. } | ImapError::TimedOut { .. } | ImapError::Closed => {
            BridgeAccountError::Unreachable(error)
        }
        other => BridgeAccountError::Protocol(other),
    }
}

fn invalid(field: &'static str, message: &str) -> BridgeAccountError {
    BridgeAccountError::InvalidInput {
        field,
        message: message.to_owned(),
    }
}

/// The credential pause for a `SecretError` kind (spec/10: kinds only,
/// never vendor status text).
fn pause_for(error: SecretError) -> CredentialPause {
    match error {
        SecretError::Unavailable => CredentialPause::StoreUnavailable,
        SecretError::Refused => CredentialPause::StoreRefused,
        SecretError::NotFound | SecretError::InvalidRef => CredentialPause::StoreFailed,
    }
}

fn valid(params: &BridgeAccountParams) -> Result<(), BridgeAccountError> {
    if params.display_name.trim().is_empty() {
        return Err(invalid("display_name", "a display name is required"));
    }
    if params.username.trim().is_empty() {
        return Err(invalid("username", "a username is required"));
    }
    // The SMTP endpoint is recorded, not probed: sending (and its probe)
    // arrives with sb-smtp in M4.3. A zero port is still never valid.
    if params.imap_port == 0 {
        return Err(invalid("imap_port", "the IMAP port must be nonzero"));
    }
    if params.smtp_port == 0 {
        return Err(invalid("smtp_port", "the SMTP port must be nonzero"));
    }
    if params.identities.is_empty() {
        return Err(invalid("identities", "at least one identity is required"));
    }
    if params.identities.iter().filter(|i| i.is_default).count() > 1 {
        return Err(invalid(
            "identities",
            "at most one identity can be the default",
        ));
    }
    if params
        .identities
        .iter()
        .any(|i| i.email.trim().is_empty() || !i.email.contains('@'))
    {
        return Err(invalid(
            "identities",
            "every identity needs an email address",
        ));
    }
    let mut seen = std::collections::BTreeSet::new();
    if params
        .identities
        .iter()
        .any(|i| !seen.insert(i.email.trim().to_ascii_lowercase()))
    {
        return Err(invalid(
            "identities",
            "identity email addresses must be distinct",
        ));
    }
    Ok(())
}

/// The account a stored receipt names. Malformed JSON or a missing id
/// is `Corrupt`, never an invented id.
fn receipt_account_id(receipt: &Receipt) -> Result<String, StoreError> {
    let parsed: serde_json::Value = serde_json::from_str(receipt.result_json())
        .map_err(|_| corrupt("a stored command receipt is not JSON"))?;
    match parsed[RECEIPT_ACCOUNT_ID].as_str() {
        Some(id) if !id.is_empty() => Ok(id.to_owned()),
        _ => Err(corrupt(
            "a stored add_bridge_account receipt names no account_id",
        )),
    }
}

fn corrupt(detail: &str) -> StoreError {
    StoreError::Corrupt {
        detail: detail.to_owned(),
    }
}

/// A replay's answer: the stored receipt and the account's current
/// credential pause (`None` when it holds none, or no longer exists).
fn replayed(tx: &Transaction, receipt: Receipt) -> Result<AddedAccount, StoreError> {
    let account_id = receipt_account_id(&receipt)?;
    let needs_attention = match credential_pause(tx, &account_id) {
        Ok(pause) => pause,
        Err(StoreError::NotFound { .. }) => None,
        Err(error) => return Err(error),
    };
    Ok(AddedAccount {
        account_id,
        receipt,
        needs_attention,
    })
}

/// The probe: connect over the pinned transport, log in, record the
/// capabilities and mailbox roles spec/04's compatibility ledger
/// wants. The server `ID` is not captured by the transport yet, so the
/// ledger records it `null` until that capture lands.
async fn probe_bridge(
    params: &BridgeAccountParams,
    password: Password,
) -> Result<String, ImapError> {
    let options = ConnectOptions {
        host: BRIDGE_HOST,
        port: params.imap_port,
        host_policy: HostPolicy::LoopbackOnly,
        pin: params.pin,
        user: params.username.clone(),
        password,
    };
    let mut connection = match params.imap_security {
        EndpointSecurity::Tls => Connection::connect(options).await?,
        EndpointSecurity::StartTls => Connection::connect_starttls(options).await?,
    };
    let capabilities = connection.capability().await?;
    let mailboxes: Vec<MailboxSummary> = connection.list_mailboxes().await?.observation;
    // Best effort: a refused BYE must not fail a completed setup.
    let _ = connection.logout().await;
    Ok(serde_json::json!({
        "capabilities": capabilities.iter().map(ToString::to_string).collect::<Vec<_>>(),
        "id": null,
        "mailboxes": mailboxes.iter().map(|row| serde_json::json!({
            "name": row.name,
            "delimiter": row.delimiter,
            "attributes": row.attributes,
        })).collect::<Vec<_>>(),
    })
    .to_string())
}

impl Resources {
    /// spec/07's `add_bridge_account` for the Proton Bridge preset,
    /// under `idempotency_key`. Order (spec/10): a replayed key answers
    /// its stored receipt and does nothing else; otherwise the probe — a
    /// failed login or pin stores nothing — then the durable rows with
    /// the receipt, then the credential under the rows'
    /// `credential_ref`. A secret-store failure leaves the account
    /// paused with the reference kept for
    /// [`store_account_credential`](Self::store_account_credential) —
    /// never a delete or recreate.
    ///
    /// `store_secret(credential_ref, secret)` performs the blocking
    /// `SecretStore::set` off this future's thread (module docs).
    pub async fn add_bridge_account<S, F>(
        &self,
        idempotency_key: &str,
        params: BridgeAccountParams,
        store_secret: S,
    ) -> Result<AddedAccount, BridgeAccountError>
    where
        S: FnOnce(String, Secret) -> F,
        F: Future<Output = Result<(), SecretError>>,
    {
        let key = idempotency_key.to_owned();
        let earlier = self
            .write(WritePriority::User, move |tx| {
                replay(tx, &key, ADD_BRIDGE_ACCOUNT)?
                    .map(|receipt| replayed(tx, receipt))
                    .transpose()
            })
            .await
            .map_err(BridgeAccountError::Store)?;
        if let Some(added) = earlier {
            return Ok(added);
        }

        valid(&params)?;
        let password = Password::from_secret(&params.password).map_err(|_| {
            invalid(
                "password",
                "the Bridge password must be non-empty UTF-8 without CR, LF or NUL",
            )
        })?;
        let capabilities_json = probe_bridge(&params, password)
            .await
            .map_err(classify_probe)?;

        let account_id = self.new_uuid_string();
        let credential_ref = format!("account/{account_id}/bridge-password");
        debug_assert!(valid_credential_ref(&credential_ref));
        let identities: Vec<NewIdentity> = params
            .identities
            .iter()
            .enumerate()
            .map(|(index, identity)| NewIdentity {
                identity_id: self.new_uuid_string(),
                email: identity.email.trim().to_owned(),
                display_name: identity.display_name.clone(),
                // None marked: the first becomes the From default.
                is_default: identity.is_default
                    || (index == 0 && !params.identities.iter().any(|i| i.is_default)),
                sendable: identity.sendable,
            })
            .collect();
        let new = NewBridgeAccount {
            account_id: account_id.clone(),
            display_name: params.display_name,
            endpoint: NewImapEndpoint {
                imap_port: i64::from(params.imap_port),
                imap_security: params.imap_security,
                smtp_port: i64::from(params.smtp_port),
                smtp_security: params.smtp_security,
                username: params.username,
                credential_ref: credential_ref.clone(),
                trusted_cert_sha256: params.pin.hex(),
                address_mode: params.address_mode,
                capabilities_json,
            },
            identities,
        };
        let now = self.now();
        let key = idempotency_key.to_owned();
        let minted = account_id.clone();
        let committed = self
            .write(WritePriority::User, move |tx| {
                let mut receipt_json = serde_json::Map::new();
                receipt_json.insert(RECEIPT_ACCOUNT_ID.to_owned(), minted.clone().into());
                let receipt = run_once(tx, &key, ADD_BRIDGE_ACCOUNT, now, |tx| {
                    insert_bridge_account(tx, &new, now)?;
                    Ok(serde_json::Value::Object(receipt_json).to_string())
                })?;
                // A concurrent command under the same key committed
                // first: answer its receipt, and leave its secret alone.
                if receipt_account_id(&receipt)? != minted {
                    return replayed(tx, receipt).map(Err);
                }
                Ok(Ok(receipt))
            })
            .await
            .map_err(BridgeAccountError::Store)?;
        let receipt = match committed {
            Ok(receipt) => receipt,
            Err(added) => return Ok(added),
        };

        let needs_attention = match store_secret(credential_ref, params.password).await {
            Ok(()) => {
                let activated_id = account_id.clone();
                // The receipt already committed. A failed activation
                // reports the pause the row actually holds (a guard
                // refusal means it changed meanwhile); NotStored only
                // when even that read fails, as the insert recorded it.
                match self
                    .write(WritePriority::User, move |tx| {
                        reactivate_after_credential(tx, &activated_id)
                    })
                    .await
                {
                    Ok(()) => None,
                    Err(_) => {
                        let paused_id = account_id.clone();
                        self.write(WritePriority::User, move |tx| {
                            credential_pause(tx, &paused_id)
                        })
                        .await
                        .unwrap_or(Some(CredentialPause::NotStored))
                    }
                }
            }
            Err(error) => {
                let pause = pause_for(error);
                let paused_id = account_id.clone();
                // The account is already paused as NotStored; this write
                // only records the precise reason. If it fails, NotStored
                // stands, is what the caller is told, and the retry path
                // still lifts it.
                match self
                    .write(WritePriority::User, move |tx| {
                        pause_for_credential(tx, &paused_id, pause)
                    })
                    .await
                {
                    Ok(()) => Some(pause),
                    Err(_) => Some(CredentialPause::NotStored),
                }
            }
        };
        Ok(AddedAccount {
            account_id,
            receipt,
            needs_attention,
        })
    }

    /// Retry credential persistence after a credential pause (spec/10):
    /// re-sets the secret by the row's stored `credential_ref` — same
    /// reference, never a recreated item — and reactivates the account
    /// on success. An account that is active or paused for another
    /// reason (spec/04's connection pauses) is refused before the secret
    /// store is touched, and the reactivation re-checks the pause in its
    /// own statement, so a connection pause is never cleared here.
    /// `store_secret` as for [`add_bridge_account`](Self::add_bridge_account).
    ///
    /// The secret is not probed here: the account's next connect logs in
    /// with it, and spec/04 pauses a rejected login. Retries for one
    /// account are the caller's to run one at a time — two racing
    /// retries both pass the check across the secret-store await, and
    /// the later secret is the one stored.
    pub async fn store_account_credential<S, F>(
        &self,
        account_id: &str,
        password: Secret,
        store_secret: S,
    ) -> Result<(), BridgeAccountError>
    where
        S: FnOnce(String, Secret) -> F,
        F: Future<Output = Result<(), SecretError>>,
    {
        let owned_id = account_id.to_owned();
        let (reference, pause) = self
            .write(WritePriority::User, move |tx| {
                Ok((
                    credential_ref(tx, &owned_id)?,
                    credential_pause(tx, &owned_id)?,
                ))
            })
            .await
            .map_err(BridgeAccountError::Store)?;
        if pause.is_none() {
            return Err(invalid(
                "account_id",
                "the account is not awaiting its credential",
            ));
        }
        store_secret(reference, password)
            .await
            .map_err(BridgeAccountError::SecretStore)?;
        let owned_id = account_id.to_owned();
        self.write(WritePriority::User, move |tx| {
            reactivate_after_credential(tx, &owned_id)
        })
        .await
        .map_err(|error| match error {
            // The guard refused: the pause changed during the
            // secret-store await. A state mismatch, not a failed write.
            StoreError::Conflict { detail } => invalid("account_id", &detail),
            error => BridgeAccountError::Store(error),
        })
    }
}
