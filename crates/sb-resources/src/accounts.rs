//! `add_bridge_account` (spec/07; rows spec/02, credentials and TLS
//! spec/10): probe the pinned loopback endpoint, commit the rows, then
//! persist the credential by reference. A failed probe creates
//! nothing; a secret-store failure pauses the new account rather than
//! deleting rows or credentials — the stored `credential_ref` is the
//! retry path.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr};

use sb_imap::{
    CertificatePin, ConnectOptions, Connection, HostPolicy, ImapError, MailboxSummary, Password,
};
use sb_platform::{Secret, SecretError, SecretStore, valid_credential_ref};
use sb_store::accounts::{
    AddressMode, EndpointSecurity, IdentitySendable, NewBridgeAccount, NewIdentity,
    NewImapEndpoint, account_state, credential_ref, insert_bridge_account, pause_account,
    reactivate_account,
};
use sb_store::{StoreError, WritePriority};

use crate::Resources;

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

/// The outcome of [`Resources::add_bridge_account`].
#[derive(Debug)]
pub struct AddedAccount {
    pub account_id: String,
    /// `Some(reason)` when the rows committed but the credential could
    /// not be persisted — the account is `needs_attention` and
    /// [`Resources::store_account_credential`] retries by the stored
    /// reference.
    pub needs_attention: Option<&'static str>,
}

/// `add_bridge_account`'s typed failures. A `Secret` never appears in
/// any variant, message or log (spec/10).
#[derive(Debug)]
pub enum BridgeAccountError {
    /// Input the sheet can fix.
    InvalidInput(String),
    /// The credential cannot be a LOGIN argument (not UTF-8).
    NotUtf8Credential,
    /// The endpoint probe failed before anything was stored: socket,
    /// pin, login or protocol — the typed `ImapError` carries it.
    Probe(ImapError),
    /// The credential store refused the retry's `set`; the account
    /// stays `needs_attention` (spec/10's kinds only).
    SecretStore(SecretError),
    /// A durable write failed.
    Store(StoreError),
    /// The rows committed but a later step failed. The account exists
    /// in `needs_attention` and the credential retry repairs it — the
    /// id is carried for exactly that recovery.
    Committed {
        account_id: String,
        error: StoreError,
    },
}

impl fmt::Display for BridgeAccountError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::InvalidInput(what) => write!(f, "invalid account parameters: {what}"),
            Self::NotUtf8Credential => {
                write!(f, "the Bridge password is not valid UTF-8")
            }
            Self::Probe(error) => write!(f, "the Bridge endpoint probe failed: {error}"),
            Self::SecretStore(error) => {
                write!(
                    f,
                    "the credential store failed: {}",
                    attention_reason(*error)
                )
            }
            Self::Store(error) => write!(f, "the account write failed: {error}"),
            Self::Committed { account_id, error } => write!(
                f,
                "account {account_id} is committed but needs attention: {error}"
            ),
        }
    }
}

impl std::error::Error for BridgeAccountError {}

/// The `needs_attention` reason for a `SecretError` kind (spec/10:
/// kinds only, never vendor status text).
fn attention_reason(error: SecretError) -> &'static str {
    match error {
        SecretError::Unavailable => "the credential store is unavailable",
        SecretError::Refused => "the credential store refused the password",
        SecretError::NotFound | SecretError::InvalidRef => {
            "the credential store could not keep the password"
        }
    }
}

fn valid(params: &BridgeAccountParams) -> Result<(), BridgeAccountError> {
    let invalid = |what: &str| Err(BridgeAccountError::InvalidInput(what.to_owned()));
    if params.display_name.trim().is_empty() {
        return invalid("a display name is required");
    }
    if params.username.trim().is_empty() {
        return invalid("a username is required");
    }
    if params.identities.is_empty() {
        return invalid("at least one identity is required");
    }
    if params.identities.iter().filter(|i| i.is_default).count() > 1 {
        return invalid("at most one identity can be the default");
    }
    if params
        .identities
        .iter()
        .any(|i| i.email.trim().is_empty() || !i.email.contains('@'))
    {
        return invalid("every identity needs an email address");
    }
    let mut seen = std::collections::BTreeSet::new();
    if params
        .identities
        .iter()
        .any(|i| !seen.insert(i.email.trim().to_ascii_lowercase()))
    {
        return invalid("identity email addresses must be distinct");
    }
    Ok(())
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
    /// spec/07's `add_bridge_account` for the Proton Bridge preset.
    /// Order (spec/10): probe first — a failed login or pin stores
    /// nothing; then the durable rows; then the credential under the
    /// rows' `credential_ref`. A secret-store failure leaves the
    /// account `needs_attention` with the reference kept for
    /// [`store_account_credential`](Self::store_account_credential) —
    /// never a delete or recreate.
    pub async fn add_bridge_account(
        &self,
        secrets: &dyn SecretStore,
        params: BridgeAccountParams,
    ) -> Result<AddedAccount, BridgeAccountError> {
        valid(&params)?;
        let password = Password::from_secret(&params.password)
            .map_err(|_| BridgeAccountError::NotUtf8Credential)?;
        let capabilities_json = probe_bridge(&params, password)
            .await
            .map_err(BridgeAccountError::Probe)?;

        let account_id = self.new_uuid_string();
        let credential_ref = format!("account/{account_id}/bridge-password");
        debug_assert!(valid_credential_ref(&credential_ref));
        let identities: Vec<NewIdentity> = params
            .identities
            .iter()
            .enumerate()
            .map(|(index, identity)| NewIdentity {
                identity_id: self.new_uuid_string(),
                email: identity.email.clone(),
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
        self.write(WritePriority::User, move |tx| {
            insert_bridge_account(tx, &new, now)
        })
        .await
        .map_err(BridgeAccountError::Store)?;

        match secrets.set(&credential_ref, &params.password) {
            Ok(()) => {
                let activated_id = account_id.clone();
                self.write(WritePriority::User, move |tx| {
                    reactivate_account(tx, &activated_id)
                })
                .await
                .map_err(|error| BridgeAccountError::Committed {
                    account_id: account_id.clone(),
                    error,
                })?;
                Ok(AddedAccount {
                    account_id,
                    needs_attention: None,
                })
            }
            Err(error) => {
                let reason = attention_reason(error);
                let paused_id = account_id.clone();
                // The account is already needs_attention; this write only
                // records the precise reason. If it fails, the generic
                // reason stands and the account is still recoverable.
                let _ = self
                    .write(WritePriority::User, move |tx| {
                        pause_account(tx, &paused_id, reason)
                    })
                    .await;
                Ok(AddedAccount {
                    account_id,
                    needs_attention: Some(reason),
                })
            }
        }
    }

    /// Retry credential persistence after a `needs_attention` pause
    /// (spec/10): re-sets the secret by the row's stored
    /// `credential_ref` — same reference, never a recreated item — and
    /// reactivates the account on success.
    pub async fn store_account_credential(
        &self,
        secrets: &dyn SecretStore,
        account_id: &str,
        password: Secret,
    ) -> Result<(), BridgeAccountError> {
        let owned_id = account_id.to_owned();
        let (reference, state) = self
            .write(WritePriority::User, move |tx| {
                Ok((
                    credential_ref(tx, &owned_id)?,
                    account_state(tx, &owned_id)?,
                ))
            })
            .await
            .map_err(BridgeAccountError::Store)?;
        if state != "needs_attention" {
            return Err(BridgeAccountError::InvalidInput(
                "the account is not awaiting its credential".to_owned(),
            ));
        }
        secrets
            .set(&reference, &password)
            .map_err(BridgeAccountError::SecretStore)?;
        let owned_id = account_id.to_owned();
        self.write(WritePriority::User, move |tx| {
            reactivate_account(tx, &owned_id)
        })
        .await
        .map_err(BridgeAccountError::Store)
    }
}
