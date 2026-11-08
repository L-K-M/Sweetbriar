//! `add_bridge_account` (spec/07; rows spec/02, credentials and TLS
//! spec/10): probe the pinned loopback endpoint, commit the rows, then
//! persist the credential by reference. A failed probe creates
//! nothing; a secret-store failure pauses the new account rather than
//! deleting rows or credentials — the stored `credential_ref` is the
//! retry path.
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
    /// `Some(pause)` when the rows committed but the credential could
    /// not be persisted: the pause the account actually holds (the
    /// precise one, or `NotStored` when recording it failed), and
    /// [`Resources::store_account_credential`] retries by the stored
    /// reference.
    pub needs_attention: Option<CredentialPause>,
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
    /// The retry's `set` failed, with any `SecretError` kind; the
    /// account keeps its credential pause unchanged (spec/10's kinds
    /// only).
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
                    pause_for(*error).reason()
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

impl std::error::Error for BridgeAccountError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Probe(error) => Some(error),
            Self::SecretStore(error) => Some(error),
            Self::Store(error) | Self::Committed { error, .. } => Some(error),
            Self::InvalidInput(_) | Self::NotUtf8Credential => None,
        }
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
    let invalid = |what: &str| Err(BridgeAccountError::InvalidInput(what.to_owned()));
    if params.display_name.trim().is_empty() {
        return invalid("a display name is required");
    }
    if params.username.trim().is_empty() {
        return invalid("a username is required");
    }
    // The SMTP endpoint is recorded, not probed: sending (and its probe)
    // arrives with sb-smtp in M4.3. A zero port is still never valid.
    if params.imap_port == 0 || params.smtp_port == 0 {
        return invalid("the IMAP and SMTP ports must be nonzero");
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
    ///
    /// `store_secret(credential_ref, secret)` performs the blocking
    /// `SecretStore::set` off this future's thread (module docs).
    pub async fn add_bridge_account<S, F>(
        &self,
        params: BridgeAccountParams,
        store_secret: S,
    ) -> Result<AddedAccount, BridgeAccountError>
    where
        S: FnOnce(String, Secret) -> F,
        F: Future<Output = Result<(), SecretError>>,
    {
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
        self.write(WritePriority::User, move |tx| {
            insert_bridge_account(tx, &new, now)
        })
        .await
        .map_err(BridgeAccountError::Store)?;

        match store_secret(credential_ref, params.password).await {
            Ok(()) => {
                let activated_id = account_id.clone();
                self.write(WritePriority::User, move |tx| {
                    reactivate_after_credential(tx, &activated_id)
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
                let pause = pause_for(error);
                let paused_id = account_id.clone();
                // The account is already paused as NotStored; this write
                // only records the precise reason. If it fails, NotStored
                // stands, is what the caller is told, and the retry path
                // still lifts it.
                let recorded = match self
                    .write(WritePriority::User, move |tx| {
                        pause_for_credential(tx, &paused_id, pause)
                    })
                    .await
                {
                    Ok(()) => pause,
                    Err(_) => CredentialPause::NotStored,
                };
                Ok(AddedAccount {
                    account_id,
                    needs_attention: Some(recorded),
                })
            }
        }
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
            return Err(BridgeAccountError::InvalidInput(
                "the account is not awaiting its credential".to_owned(),
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
        .map_err(BridgeAccountError::Store)
    }
}
