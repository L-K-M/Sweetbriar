//! Identities and From resolution (spec/05 "Identities and From
//! resolution", spec/07 "Identities"): the two identity commands, the
//! pure resolver that picks a draft's sending identity from the compose
//! context, a source occurrence's headers and the recorded identities,
//! the reply-recipient rule, and the cross-account check M4.3's
//! `send_draft` will call.
//!
//! The resolver does no I/O. [`Engine::resolve_identity`] gathers its
//! inputs header-only, then calls the pure [`resolve_identity`] here, so
//! the rules are table-testable without a store.

use std::collections::BTreeSet;
use std::sync::{Arc, Mutex};

use sb_resources::{
    AccountKind, AddressOperand, DraftRecipient, IdentityError, IdentityResolutionInputs,
    IdentityRoute, IdentitySendable, IdentityWrite, ReaderAddress, Receipt, SourceIdentityInputs,
    SourceMessageHeaders, StoreError, WritePriority, identity_resolution_inputs, run_once,
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

/// The compose context a draft was opened in (spec/05). Reply All shares
/// Reply's identity rule; only its recipient list differs.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ComposeContext {
    New,
    Reply,
    ReplyAll,
    Forward,
}

/// The resolver's answer: a concrete identity, or the shell's
/// "Choose a sending identity" state with Send disabled (spec/05).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ResolvedIdentity {
    Chosen(String),
    ChooseSendingIdentity,
}

/// The comparison key matching a header address to an identity's
/// address (spec/06's received-address policy: written local, lowercased
/// domain). An unparseable spelling matches nothing.
pub(crate) fn address_key(address: &str) -> Option<String> {
    AddressOperand::parse(address)
        .ok()
        .map(|parsed| parsed.key().to_owned())
}

/// Whether the identity can send now: not the local archive, and
/// `validated` or `provisional` (spec/02, spec/05).
fn sends(route: &IdentityRoute) -> bool {
    route.account_kind != AccountKind::Local && route.sendable.can_send()
}

fn key_matches(route: &IdentityRoute, key: Option<&str>) -> bool {
    match (address_key(&route.email), key) {
        (Some(left), Some(right)) => left == right,
        _ => false,
    }
}

/// The sending identity at `key`, preferring the receiving account's:
/// two accounts may hold the same address, and answering from the one
/// that received the message needs no cross-account banner.
fn identity_for_key<'a>(
    identities: &'a [IdentityRoute],
    account_id: &str,
    key: Option<&str>,
) -> Option<&'a IdentityRoute> {
    let matching = |route: &&IdentityRoute| sends(route) && key_matches(route, key);
    identities
        .iter()
        .filter(matching)
        .find(|route| route.account_id == account_id)
        .or_else(|| identities.iter().find(matching))
}

fn default_of<'a>(identities: &'a [IdentityRoute], account_id: &str) -> Option<&'a IdentityRoute> {
    identities
        .iter()
        .find(|route| route.account_id == account_id && route.is_default && sends(route))
}

/// Resolve a draft's sending identity (spec/05 "Default identity for a
/// new draft"):
///
/// - **New:** the selected account's default, otherwise the global
///   `identity.default` setting.
/// - **Reply, Reply All, Forward:** the identity recorded on the source
///   message's send attempt; without one, the identity matching its
///   From. Received mail falls through to the identity whose address is
///   in its To, Cc or `Delivered-To` for that exact occurrence, then to
///   that account's default.
/// - Nothing valid: [`ResolvedIdentity::ChooseSendingIdentity`].
///
/// A recorded attempt whose identity is gone or can't send refuses
/// rather than silently substituting another sender (research 15).
pub fn resolve_identity(
    context: ComposeContext,
    inputs: &IdentityResolutionInputs,
) -> ResolvedIdentity {
    let chosen = match context {
        ComposeContext::New => inputs
            .selected_account_id
            .as_deref()
            .and_then(|account| default_of(&inputs.identities, account))
            .or_else(|| {
                inputs.global_default_identity_id.as_deref().and_then(|id| {
                    inputs
                        .identities
                        .iter()
                        .find(|route| route.identity_id == id && sends(route))
                })
            })
            .map(|route| route.identity_id.clone()),
        ComposeContext::Reply | ComposeContext::ReplyAll | ComposeContext::Forward => {
            resolve_reply_like(inputs)
        }
    };
    match chosen {
        Some(identity_id) => ResolvedIdentity::Chosen(identity_id),
        None => ResolvedIdentity::ChooseSendingIdentity,
    }
}

fn resolve_reply_like(inputs: &IdentityResolutionInputs) -> Option<String> {
    let source = inputs.source.as_ref()?;
    if let Some(sent) = source.sent_identity_id.as_deref() {
        return match inputs
            .identities
            .iter()
            .find(|route| route.identity_id == sent)
        {
            Some(route) if sends(route) => Some(route.identity_id.clone()),
            // Gone or unavailable: require a visible choice, never a
            // silent substitution.
            _ => None,
        };
    }
    // No recorded attempt (sent before attempts existed, or imported):
    // the identity matching its From, within the source account.
    for address in &source.headers.from {
        let Some(key) = address_key(address.address.as_deref().unwrap_or_default()) else {
            continue;
        };
        if let Some(route) = inputs.identities.iter().find(|route| {
            route.account_id == source.account_id && sends(route) && key_matches(route, Some(&key))
        }) {
            return Some(route.identity_id.clone());
        }
    }
    // Received mail: the identity this exact copy was addressed to.
    for address in source.headers.to.iter().chain(&source.headers.cc) {
        if let Some(route) = identity_for_key(
            &inputs.identities,
            &source.account_id,
            address_key(address.address.as_deref().unwrap_or_default()).as_deref(),
        ) {
            return Some(route.identity_id.clone());
        }
    }
    for delivered in &source.headers.delivered_to {
        if let Some(route) = identity_for_key(
            &inputs.identities,
            &source.account_id,
            address_key(delivered).as_deref(),
        ) {
            return Some(route.identity_id.clone());
        }
    }
    default_of(&inputs.identities, &source.account_id).map(|route| route.identity_id.clone())
}

fn address_list(addresses: &[ReaderAddress]) -> Vec<String> {
    addresses
        .iter()
        .filter_map(|address| address_key(address.address.as_deref().unwrap_or_default()))
        .collect()
}

/// Reply's recipients (spec/05): the original's `Reply-To` when it
/// differs from its From, else the From. Offline, from `message_headers`.
pub fn reply_recipients(headers: &SourceMessageHeaders) -> Vec<ReaderAddress> {
    if !headers.reply_to.is_empty()
        && address_list(&headers.reply_to) != address_list(&headers.from)
    {
        headers.reply_to.clone()
    } else {
        headers.from.clone()
    }
}

/// Reply All's recipients: [`reply_recipients`] plus the original's To
/// and Cc, minus the chosen identity's address and any repeat (spec/05).
pub fn reply_all_recipients(headers: &SourceMessageHeaders, exclude: &str) -> Vec<ReaderAddress> {
    let excluded = address_key(exclude);
    let mut recipients = reply_recipients(headers);
    // Reply All to a message the user sent has the chosen identity as
    // its From (or a Reply-To can name it): it leaves the base list too.
    if let Some(excluded) = excluded.as_deref() {
        recipients.retain(|present| {
            address_key(present.address.as_deref().unwrap_or_default()).as_deref() != Some(excluded)
        });
    }
    for original in headers.to.iter().chain(&headers.cc) {
        let Some(key) = address_key(original.address.as_deref().unwrap_or_default()) else {
            // An unparseable entry has no key; an identical spelling
            // still counts as a repeat.
            if !recipients.contains(original) {
                recipients.push(original.clone());
            }
            continue;
        };
        if excluded.as_deref() == Some(key.as_str())
            || recipients.iter().any(|present| {
                address_key(present.address.as_deref().unwrap_or_default()).as_deref()
                    == Some(key.as_str())
            })
        {
            continue;
        }
        recipients.push(original.clone());
    }
    recipients
}

/// A reply whose route identity belongs to a different account than the
/// receiving occurrence (spec/05). Local-archive sources are exempt:
/// that account can't send, so answering from elsewhere is expected.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CrossAccountWarning {
    pub route_account_id: String,
    pub source_account_id: String,
}

/// Whether a cross-account warning applies. Only replies and reply-alls
/// carry a receiving occurrence; a forward starts a new thread.
pub fn cross_account_warning(
    context: ComposeContext,
    route: Option<&IdentityRoute>,
    source: Option<&SourceIdentityInputs>,
) -> Option<CrossAccountWarning> {
    if !matches!(context, ComposeContext::Reply | ComposeContext::ReplyAll) {
        return None;
    }
    let route = route?;
    let source = source?;
    if source.account_kind == AccountKind::Local || route.account_id == source.account_id {
        return None;
    }
    Some(CrossAccountWarning {
        route_account_id: route.account_id.clone(),
        source_account_id: source.account_id.clone(),
    })
}

fn recipient_keys(recipients: &[DraftRecipient]) -> BTreeSet<String> {
    recipients
        .iter()
        .filter_map(|recipient| address_key(&recipient.address))
        .collect()
}

/// The acknowledgement token for a cross-account send: bound to the
/// revision's identity and recipients, so editing either invalidates it.
/// Recipients are deduplicated by their comparison key and sorted, so
/// To/Cc order and repeats don't matter. The parts are a JSON array, so
/// the token's shape never depends on what a key may contain.
pub fn cross_account_ack(
    identity_id: &str,
    to: &[DraftRecipient],
    cc: &[DraftRecipient],
    bcc: &[DraftRecipient],
) -> String {
    let mut keys = recipient_keys(to);
    keys.extend(recipient_keys(cc));
    keys.extend(recipient_keys(bcc));
    let keys: Vec<String> = keys.into_iter().collect();
    let parts =
        serde_json::to_string(&(identity_id, keys)).expect("strings always serialise as JSON");
    format!("cross_account:{parts}")
}

/// The warning and the acknowledgement `send_draft` requires for one
/// revision's identity and recipients. [`Engine::cross_account_notice`]
/// and `send_draft` share it, so the notice's `ack` and the send's
/// check cannot drift (spec/05, 07).
pub(crate) struct CrossAccountDecision {
    pub warning: CrossAccountWarning,
    pub ack: String,
}

/// The warning and acknowledgement for one revision's route identity
/// and recipients; `None` when none applies (spec/05).
pub(crate) fn cross_account_decision(
    context: ComposeContext,
    route: Option<&IdentityRoute>,
    source: Option<&SourceIdentityInputs>,
    identity_id: &str,
    to: &[DraftRecipient],
    cc: &[DraftRecipient],
    bcc: &[DraftRecipient],
) -> Option<CrossAccountDecision> {
    let warning = cross_account_warning(context, route, source)?;
    Some(CrossAccountDecision {
        warning,
        ack: cross_account_ack(identity_id, to, cc, bcc),
    })
}

/// Whether `acks` carries the acknowledgement the warning needs. M4.3's
/// `send_draft` calls this inside its revision check: a missing one is
/// `Conflict` naming `cross_account`, and the core never stores it.
pub fn cross_account_acknowledged(
    warning: Option<&CrossAccountWarning>,
    acks: &[String],
    identity_id: &str,
    to: &[DraftRecipient],
    cc: &[DraftRecipient],
    bcc: &[DraftRecipient],
) -> bool {
    if warning.is_none() {
        return true;
    }
    let expected = cross_account_ack(identity_id, to, cc, bcc);
    acks.iter().any(|ack| ack == &expected)
}

/// Which account context a new draft was opened from.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ComposeSelection {
    Account(String),
    Mailbox(i64),
}

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

    /// Gather a source occurrence's resolver inputs and resolve them
    /// offline: [`resolve_identity`] over the header-only read. A reply
    /// or forward needs its source occurrence; a new message takes the
    /// selected account or mailbox instead.
    pub async fn resolve_identity(
        &self,
        context: ComposeContext,
        source_occurrence_id: Option<i64>,
        selection: Option<ComposeSelection>,
    ) -> Result<ResolvedIdentity, EngineError> {
        if matches!(context, ComposeContext::New) != source_occurrence_id.is_none() {
            return Err(EngineError::invalid_input(
                "source_occurrence_id",
                "a new message takes no source; a reply or forward takes one",
            ));
        }
        let (selected_account, selected_mailbox) = match selection {
            Some(ComposeSelection::Account(account)) => (Some(account), None),
            Some(ComposeSelection::Mailbox(mailbox)) => (None, Some(mailbox)),
            None => (None, None),
        };
        let inputs = self
            .read_through(move |resources| {
                resources.read(|tx| {
                    identity_resolution_inputs(
                        tx,
                        source_occurrence_id,
                        selected_account.as_deref(),
                        selected_mailbox,
                    )
                })
            })
            .await?;
        Ok(resolve_identity(context, &inputs))
    }
}

#[cfg(test)]
mod tests;
