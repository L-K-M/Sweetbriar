//! `add_bridge_account` (spec/07 "Commands"; plan M2.3): the Proton
//! Bridge preset's setup as one engine command. The ordering — replay
//! check, probe, rows with the receipt, credential — lives in
//! `sb-resources`; the engine runs it as one command worker, supplies
//! the secret step on its counted blocking boundary (spec/01: the
//! secret store blocks), and maps the setup failures onto spec/07's
//! errors.

use sb_resources::{
    AddedAccount, BridgeAccountError, BridgeAccountParams, CertificatePin, EndpointSecurity,
    Secret, SecretError, certificate_pin_from_pem,
};

use crate::EngineError;
use crate::engine::{Engine, validate_idempotency_key};

impl Engine {
    /// `add_bridge_account(params)` (spec/07) under `idempotency_key`.
    /// A repeated key answers the stored receipt `{"account_id": ...}`
    /// and does nothing else — no probe, no secret write. A refused
    /// login or a pin mismatch stores nothing (`InvalidInput` on
    /// `password` or `pin`); Bridge not answering is `Unavailable`. A
    /// secret-store failure still commits: the account waits for its
    /// credential, as `needs_attention` reports (spec/10).
    pub async fn add_bridge_account(
        &self,
        idempotency_key: &str,
        params: BridgeAccountParams,
    ) -> Result<AddedAccount, EngineError> {
        validate_idempotency_key(idempotency_key)?;
        let key = idempotency_key.to_owned();
        let added = self
            .command(move |worker| async move {
                let setter = worker.clone();
                let store_secret = move |credential_ref: String, secret: Secret| async move {
                    // A failed join (the store's call panicked, or the pool
                    // shut down) is a store that did not keep the secret: the
                    // committed account keeps its pause and the retry path
                    // stays. The engine has no logger yet (spec/07 lists one
                    // as a platform service), so nothing records the cause.
                    setter
                        .blocking(move |resources| resources.set_secret(&credential_ref, &secret))
                        .await
                        .unwrap_or(Err(SecretError::Unavailable))
                };
                worker
                    .resources()
                    .add_bridge_account(&key, params, store_secret)
                    .await
                    .map_err(EngineError::from)
            })
            .await?;
        // The new account syncs from now on, once its credential is
        // stored; a credential pause waits for the retry (spec/10).
        if added.needs_attention.is_none() {
            self.start_account(added.account_id.clone());
        }
        Ok(added)
    }
}

impl Engine {
    /// spec/10's fingerprint fallback: the pin of the certificate
    /// Bridge presents on `port`, for the user to confirm. It runs on
    /// the engine's runtime, where the probe's sockets live, and stores
    /// nothing; Bridge not answering is `Unavailable`. A read-only
    /// store is `Corrupt` before any connection: no remote effect
    /// follows an integrity finding (spec/03), and no account can be
    /// added with the answer.
    pub async fn bridge_certificate(
        &self,
        port: u16,
        security: EndpointSecurity,
    ) -> Result<CertificatePin, EngineError> {
        self.command(move |worker| async move {
            if let Some(latch) = worker.resources().integrity_latch() {
                return Err(EngineError::Corrupt {
                    detail: format!(
                        "the store is read-only after an integrity finding, \
                         so it can't ask Bridge for its certificate: {}",
                        latch.detail
                    ),
                });
            }
            worker
                .resources()
                .bridge_certificate(port, security)
                .await
                .map_err(EngineError::from)
        })
        .await
    }

    /// The pin of the one certificate in Bridge's exported `cert.pem`
    /// (spec/10's preferred path); any other file is `InvalidInput` on
    /// `certificate`. Pure: parses the given bytes only.
    pub fn certificate_pin_from_pem(pem: &[u8]) -> Result<CertificatePin, EngineError> {
        certificate_pin_from_pem(pem).map_err(EngineError::from)
    }
}

impl From<BridgeAccountError> for EngineError {
    fn from(error: BridgeAccountError) -> Self {
        let message = error.to_string();
        match error {
            BridgeAccountError::InvalidInput { field, message } => {
                Self::invalid_input(field, message)
            }
            // The presented fingerprint is in the message, so the sheet
            // can show it for confirmation (spec/10's fallback).
            BridgeAccountError::PinMismatch { .. } => Self::invalid_input("pin", message),
            BridgeAccountError::LoginRefused => Self::invalid_input("password", message),
            // spec/07: Unavailable covers "Bridge not running".
            BridgeAccountError::Unreachable(_)
            | BridgeAccountError::Protocol(_)
            | BridgeAccountError::SecretStore(_) => Self::unavailable(message),
            BridgeAccountError::Store(error) => error.into(),
        }
    }
}

#[cfg(test)]
mod tests;
