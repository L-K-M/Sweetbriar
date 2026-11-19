//! The identity commands (spec/07 "Identities", spec/05 "Identities
//! and From resolution"): `upsert_identity` for accounts whose
//! addresses the user maintains, and `set_default_identity`, each under
//! an idempotency key with its receipt.

use std::sync::{Arc, Mutex};

use sb_resources::{
    IdentityError, IdentitySendable, IdentityWrite, Receipt, StoreError, WritePriority, run_once,
    set_default_identity, upsert_identity,
};

use crate::drafts::encode_receipt_id;
use crate::engine::validate_idempotency_key;
use crate::{Engine, EngineError};

/// The command names `command_receipts` records (spec/07).
const UPSERT_IDENTITY: &str = "upsert_identity";
const SET_DEFAULT_IDENTITY: &str = "set_default_identity";
/// Rolls a write back while a typed refusal travels out to the caller.
const IDENTITY_ROLLBACK: &str = "the identity command refused the write";

/// `upsert_identity`'s parameters (spec/07): the identity's own fields.
/// Defaults belong to [`Engine::set_default_identity`].
pub struct IdentityParams {
    pub identity_id: String,
    pub account_id: String,
    pub email: String,
    pub display_name: String,
    pub reply_to: Option<String>,
    pub signature_source: Option<String>,
    pub sendable: IdentitySendable,
}

fn identity_error(error: IdentityError) -> EngineError {
    match error {
        IdentityError::ProtonAccount { account_id } => EngineError::invalid_input(
            "account_id",
            format!(
                "account {account_id}'s identities come from Proton; \
                 they can't be edited here"
            ),
        ),
        IdentityError::NotSendable {
            identity_id,
            sendable,
        } => EngineError::invalid_input(
            "identity_id",
            format!("identity {identity_id} is {sendable}, so it can't be the sending default"),
        ),
        IdentityError::InvalidInput { field, message } => {
            EngineError::invalid_input(field, message)
        }
        IdentityError::Store(error) => error.into(),
    }
}

/// Record a typed refusal for [`finish_identity`] and roll the write
/// transaction back with a private marker.
fn rollback_identity(slot: &Mutex<Option<EngineError>>, error: IdentityError) -> StoreError {
    *slot.lock().expect("identity validation lock poisoned") = Some(identity_error(error));
    StoreError::internal(IDENTITY_ROLLBACK)
}

fn finish_identity(
    result: Result<Receipt, StoreError>,
    slot: &Mutex<Option<EngineError>>,
) -> Result<Receipt, EngineError> {
    let recorded = slot
        .lock()
        .expect("identity validation lock poisoned")
        .take();
    match (result, recorded) {
        (Ok(receipt), None) => Ok(receipt),
        (Err(error), None) => Err(error.into()),
        (_, Some(error)) => Err(error),
    }
}

impl Engine {
    /// One identity command's transaction: the effect and its receipt
    /// commit together, a repeated key replays, and a typed refusal
    /// rolls back without being flattened into a store failure.
    async fn identity_command<F>(
        &self,
        idempotency_key: &str,
        command: &'static str,
        receipt_id: String,
        effect: F,
    ) -> Result<Receipt, EngineError>
    where
        F: FnOnce(&sb_resources::Transaction) -> Result<(), IdentityError> + Send + 'static,
    {
        validate_idempotency_key(idempotency_key)?;
        let now = self.resources().now();
        let (key, id) = (idempotency_key.to_owned(), receipt_id);
        let validation = Arc::new(Mutex::new(None));
        let write_validation = Arc::clone(&validation);
        let receipt = self
            .resources()
            .write(WritePriority::User, move |tx| {
                let receipt = run_once(tx, &key, command, now, |tx| {
                    if let Err(error) = effect(tx) {
                        return Err(match error {
                            IdentityError::Store(error) => error,
                            refusal => rollback_identity(&write_validation, refusal),
                        });
                    }
                    Ok(encode_receipt_id("identity_id", &id))
                })?;
                Ok(receipt)
            })
            .await;
        finish_identity(receipt, &validation)
    }

    /// `upsert_identity(params)` (spec/07): insert the identity or update
    /// the row `identity_id` names, under an idempotency key. A Proton
    /// account's identities come from its backend and are refused; new
    /// identities start non-default.
    pub async fn upsert_identity(
        &self,
        idempotency_key: &str,
        params: IdentityParams,
    ) -> Result<Receipt, EngineError> {
        let identity_id = params.identity_id.clone();
        self.identity_command(idempotency_key, UPSERT_IDENTITY, identity_id, move |tx| {
            upsert_identity(
                tx,
                &IdentityWrite {
                    identity_id: params.identity_id,
                    account_id: params.account_id,
                    email: params.email,
                    display_name: params.display_name,
                    reply_to: params.reply_to,
                    signature_source: params.signature_source,
                    sendable: params.sendable,
                },
            )
        })
        .await
    }

    /// `set_default_identity(identity_id)` (spec/07, spec/05): leave the
    /// account with exactly this identity as its one default, in one
    /// transaction. An identity that isn't `validated` or `provisional`
    /// can't send and is refused.
    pub async fn set_default_identity(
        &self,
        idempotency_key: &str,
        identity_id: &str,
    ) -> Result<Receipt, EngineError> {
        let id = identity_id.to_owned();
        self.identity_command(
            idempotency_key,
            SET_DEFAULT_IDENTITY,
            id.clone(),
            move |tx| set_default_identity(tx, &id),
        )
        .await
    }
}

#[cfg(test)]
mod tests;
