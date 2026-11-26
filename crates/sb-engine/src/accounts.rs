//! `add_bridge_account` (spec/07 "Commands"; plan M2.3): the Proton
//! Bridge preset's setup as one engine command. The ordering — replay
//! check, probe, rows with the receipt, credential — lives in
//! `sb-resources`; the engine runs it as one command worker, supplies
//! the secret step on its counted blocking boundary (spec/01: the
//! secret store blocks), and maps the setup failures onto spec/07's
//! errors.

use sb_resources::{
    AccountKind, AccountState, AddedAccount, BridgeAccountError, BridgeAccountParams,
    CertificatePin, EndpointSecurity, HUMAN_VERIFICATION_REASON, Receipt, Secret, SecretError,
    WritePriority, accounts, activate_after_verification, certificate_pin_from_pem, pause_account,
    replay, resume_account, run_once, update_account,
};

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::EngineError;
use crate::ProtonVerificationAnswer;
use crate::ProtonVerificationChallenge;
use crate::account_sync::AccountJobs;
use crate::engine::{Engine, Worker, validate_idempotency_key};
use crate::proton::Sessions;
use crate::proton_sync::ProtonOpened;
use crate::threads::ThreadRebuilds;

/// The commands' names in `command_receipts` (spec/07).
const UPDATE_ACCOUNT: &str = "update_account";
const PAUSE_ACCOUNT: &str = "pause_account";
const RESUME_ACCOUNT: &str = "resume_account";
/// `verify_proton_account`'s name in `command_receipts` (spec/07).
const VERIFY_PROTON_ACCOUNT: &str = "verify_proton_account";

/// The stored byte bound on a display name (spec/02, 07).
const MAX_DISPLAY_NAME_BYTES: usize = 128;

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
        self.worker_statuses().rows_changed();
        Ok(added)
    }
}

impl Engine {
    /// `update_account(id, params)` (spec/07) under `idempotency_key`:
    /// the display name only, trimmed and validated, with `revision`
    /// bumped and `config_generation` untouched. A repeated key
    /// answers its stored receipt and writes nothing.
    pub async fn update_account(
        &self,
        idempotency_key: &str,
        account_id: &str,
        display_name: &str,
    ) -> Result<Receipt, EngineError> {
        validate_idempotency_key(idempotency_key)?;
        let name = valid_display_name(display_name)?;
        let (key, id, name) = (idempotency_key.to_owned(), account_id.to_owned(), name);
        self.command(move |worker| async move {
            let now = worker.resources().now();
            worker
                .resources()
                .write(WritePriority::User, move |tx| {
                    run_once(tx, &key, UPDATE_ACCOUNT, now, |tx| {
                        update_account(tx, &id, &name)?;
                        Ok("{}".to_owned())
                    })
                })
                .await
                .map_err(EngineError::from)
        })
        .await
    }

    /// `pause_account(id)` (spec/07) under `idempotency_key`: an active
    /// account becomes paused and its worker stops before the command
    /// answers, so no request for the account starts after the receipt.
    /// A paused account answers a receipt and changes nothing; a
    /// `needs_attention` or `disconnected` one is `Conflict`; the local
    /// archive is `InvalidInput` on `account`.
    pub async fn pause_account(
        &self,
        idempotency_key: &str,
        account_id: &str,
    ) -> Result<Receipt, EngineError> {
        validate_idempotency_key(idempotency_key)?;
        let jobs = Arc::clone(self.account_jobs());
        let sessions = Arc::clone(&self.proton_sessions);
        let opened = Arc::clone(self.proton_opened());
        let evidence = Arc::clone(&self.send_evidence);
        let verifications = Arc::clone(&self.proton_verifications);
        let threads = Arc::clone(self.imports().threads());
        let statuses = Arc::clone(self.worker_statuses());
        let (key, id) = (idempotency_key.to_owned(), account_id.to_owned());
        self.command(move |worker| async move {
            let _lifecycle = jobs.lifecycle().await;
            let (kind, state) = match account_kind_and_state(&worker, &id).await? {
                Some((AccountKind::Local, _)) => {
                    return Err(EngineError::invalid_input(
                        "account",
                        format!("account {id} is the local archive, which has no server"),
                    ));
                }
                Some(found) => found,
                None => {
                    return Err(EngineError::NotFound {
                        what: format!("account {id}"),
                    });
                }
            };
            // A repeated key answers its stored receipt and does nothing
            // else: no worker stop, no write.
            let replayed = worker
                .blocking({
                    let key = key.clone();
                    move |resources| resources.read(|tx| replay(tx, &key, PAUSE_ACCOUNT))
                })
                .await??;
            if let Some(receipt) = replayed {
                return Ok(receipt);
            }
            // Only an active account's worker is stopped, before the
            // state change, so no request for the account starts after
            // the receipt. A paused account's write is a no-op and a
            // `needs_attention` or `disconnected` one's is `Conflict`;
            // neither stops a worker.
            let stopping = state == AccountState::Active;
            if stopping {
                jobs.stop_account(&id).await;
            }
            let now = worker.resources().now();
            let write_id = id.clone();
            let result = worker
                .resources()
                .write(WritePriority::User, move |tx| {
                    run_once(tx, &key, PAUSE_ACCOUNT, now, |tx| {
                        pause_account(tx, &write_id)?;
                        Ok("{}".to_owned())
                    })
                })
                .await;
            if result.is_err() && stopping {
                // The account stays as it was, so its worker runs again.
                jobs.unpause(&id);
                start_worker(
                    &worker,
                    &jobs,
                    &sessions,
                    &opened,
                    &evidence,
                    &verifications,
                    &threads,
                    kind,
                    id,
                )
                .await;
            }
            let receipt = result.map_err(EngineError::from);
            if receipt.is_ok() {
                // The account's state changed: its health row did too
                // (plan M2.7b2).
                statuses.rows_changed();
            }
            receipt
        })
        .await
    }

    /// `resume_account(id)` (spec/07) under `idempotency_key`: a paused
    /// account becomes active and its worker starts. An account a
    /// restore paused reconciles first, in the same transaction as the
    /// state change (03 "Restore" step 5). An active account answers a
    /// receipt and changes nothing; a `needs_attention` or
    /// `disconnected` one is `Conflict`; the local archive is
    /// `InvalidInput` on `account`.
    pub async fn resume_account(
        &self,
        idempotency_key: &str,
        account_id: &str,
    ) -> Result<Receipt, EngineError> {
        validate_idempotency_key(idempotency_key)?;
        let jobs = Arc::clone(self.account_jobs());
        let sessions = Arc::clone(&self.proton_sessions);
        let opened = Arc::clone(self.proton_opened());
        let evidence = Arc::clone(&self.send_evidence);
        let verifications = Arc::clone(&self.proton_verifications);
        let threads = Arc::clone(self.imports().threads());
        let statuses = Arc::clone(self.worker_statuses());
        let (key, id) = (idempotency_key.to_owned(), account_id.to_owned());
        self.command(move |worker| async move {
            let _lifecycle = jobs.lifecycle().await;
            let kind = match crate::proton::account_kind(&worker, &id).await? {
                Some(kind) => kind,
                None => {
                    return Err(EngineError::NotFound {
                        what: format!("account {id}"),
                    });
                }
            };
            if kind == AccountKind::Local {
                return Err(EngineError::invalid_input(
                    "account",
                    format!("account {id} is the local archive, which has no server"),
                ));
            }
            // A repeated key answers its stored receipt and does nothing
            // else: no worker start, no write.
            let replayed = worker
                .blocking({
                    let key = key.clone();
                    move |resources| resources.read(|tx| replay(tx, &key, RESUME_ACCOUNT))
                })
                .await??;
            if let Some(receipt) = replayed {
                return Ok(receipt);
            }
            let now = worker.resources().now();
            let write_id = id.clone();
            let activated = Arc::new(AtomicBool::new(false));
            let write_activated = Arc::clone(&activated);
            let receipt = worker
                .resources()
                .write(WritePriority::User, move |tx| {
                    run_once(tx, &key, RESUME_ACCOUNT, now, |tx| {
                        let changed = resume_account(tx, &write_id)?;
                        write_activated.store(changed, Ordering::Relaxed);
                        Ok("{}".to_owned())
                    })
                })
                .await?;
            // Resuming an active account changes nothing: no worker
            // start, no session restore.
            if !activated.load(Ordering::Relaxed) {
                return Ok(receipt);
            }
            jobs.unpause(&id);
            start_worker(
                &worker,
                &jobs,
                &sessions,
                &opened,
                &evidence,
                &verifications,
                &threads,
                kind,
                id,
            )
            .await;
            statuses.rows_changed();
            Ok(receipt)
        })
        .await
    }

    /// `proton_verification(id)` (spec/07, 04 "Human verification
    /// during sync"): the challenge the engine holds for a Proton
    /// account that is `needs_attention` for human verification, or
    /// none for any other state, reason or kind. An unknown account is
    /// `NotFound`. The URL carries Proton's token, so it never reaches
    /// a log or the diagnostics package (spec/10).
    pub async fn proton_verification(
        &self,
        account_id: &str,
    ) -> Result<Option<ProtonVerificationChallenge>, EngineError> {
        let id = account_id.to_owned();
        let read_id = id.clone();
        let accounts = self
            .read_through(move |resources| resources.read(accounts))
            .await?;
        let account = match accounts
            .into_iter()
            .find(|account| account.account_id == read_id)
        {
            Some(account) => account,
            None => {
                return Err(EngineError::NotFound {
                    what: format!("account {id}"),
                });
            }
        };
        if account.kind != AccountKind::Proton
            || account.state != AccountState::NeedsAttention
            || account.state_reason.as_deref() != Some(HUMAN_VERIFICATION_REASON)
        {
            return Ok(None);
        }
        Ok(self
            .proton_verifications
            .challenge(&id)
            .map(|challenge| ProtonVerificationChallenge {
                url: challenge.url,
                methods: challenge.methods,
            }))
    }

    /// `verify_proton_account(key, id, answer)` (spec/07, 04 "Human
    /// verification during sync"): a `needs_attention` Proton account
    /// paused for human verification becomes `active` and its worker
    /// starts, carrying the answer's token on its first request, once.
    /// Without an answer this is "Try Again": the worker meets
    /// Proton's challenge again if Proton still asks. Any other state
    /// or stored reason is `Conflict`; another kind is `InvalidInput`
    /// on `account`; an unknown account is `NotFound`. A repeated key
    /// answers its stored receipt and changes nothing. The token is
    /// never logged, stored or put in a receipt (spec/10).
    pub async fn verify_proton_account(
        &self,
        idempotency_key: &str,
        account_id: &str,
        answer: Option<ProtonVerificationAnswer>,
    ) -> Result<Receipt, EngineError> {
        validate_idempotency_key(idempotency_key)?;
        if let Some(answer) = answer.as_ref()
            && (answer.token.is_empty() || answer.token_type.is_empty())
        {
            return Err(EngineError::invalid_input(
                "answer",
                "the human-verification answer needs both a token and its type",
            ));
        }
        let jobs = Arc::clone(self.account_jobs());
        let sessions = Arc::clone(&self.proton_sessions);
        let opened = Arc::clone(self.proton_opened());
        let evidence = Arc::clone(&self.send_evidence);
        let verifications = Arc::clone(&self.proton_verifications);
        let threads = Arc::clone(self.imports().threads());
        let statuses = Arc::clone(self.worker_statuses());
        let (key, id) = (idempotency_key.to_owned(), account_id.to_owned());
        self.command(move |worker| async move {
            let _lifecycle = jobs.lifecycle().await;
            let kind = match crate::proton::account_kind(&worker, &id).await? {
                Some(kind) => kind,
                None => {
                    return Err(EngineError::NotFound {
                        what: format!("account {id}"),
                    });
                }
            };
            if kind != AccountKind::Proton {
                return Err(EngineError::invalid_input(
                    "account",
                    format!("account {id} is not a Proton account"),
                ));
            }
            // A repeated key answers its stored receipt and does nothing
            // else: no activation, no answer held, no worker start.
            let replayed = worker
                .blocking({
                    let key = key.clone();
                    move |resources| resources.read(|tx| replay(tx, &key, VERIFY_PROTON_ACCOUNT))
                })
                .await??;
            if let Some(receipt) = replayed {
                return Ok(receipt);
            }
            let now = worker.resources().now();
            let write_id = id.clone();
            let receipt = worker
                .resources()
                .write(WritePriority::User, move |tx| {
                    run_once(tx, &key, VERIFY_PROTON_ACCOUNT, now, |tx| {
                        activate_after_verification(tx, &write_id)?;
                        Ok("{}".to_owned())
                    })
                })
                .await?;
            // The challenge was answered (or "Try Again" drops it):
            // its token must never outlive the request it rode.
            verifications.forget(&id);
            if let Some(answer) = answer {
                verifications.answer(
                    &id,
                    sb_resources::SecretString::from(answer.token),
                    answer.token_type,
                );
            }
            jobs.unpause(&id);
            // A worker the 9001 ended may still be releasing its claim;
            // wait so this activation's worker is the one that rides
            // the answer.
            jobs.wait_for_release(&id).await;
            start_worker(
                &worker,
                &jobs,
                &sessions,
                &opened,
                &evidence,
                &verifications,
                &threads,
                kind,
                id,
            )
            .await;
            statuses.rows_changed();
            Ok(receipt)
        })
        .await
    }
}

/// The account's kind and state, or `None` for an unknown account.
async fn account_kind_and_state(
    worker: &Worker,
    account_id: &str,
) -> Result<Option<(AccountKind, AccountState)>, EngineError> {
    let accounts = worker
        .blocking(move |resources| resources.read(accounts))
        .await??;
    Ok(accounts
        .into_iter()
        .find(|account| account.account_id == account_id)
        .map(|account| (account.kind, account.state)))
}

/// Start a server account's worker. Launch skips a paused account's
/// Proton session, so one without a session restores it first.
#[allow(clippy::too_many_arguments)]
async fn start_worker(
    worker: &Worker,
    jobs: &Arc<AccountJobs>,
    sessions: &Arc<Sessions>,
    opened: &Arc<ProtonOpened>,
    evidence: &Arc<crate::proton_send::SendEvidence>,
    verifications: &Arc<crate::proton_verification::ProtonVerifications>,
    threads: &Arc<ThreadRebuilds>,
    kind: AccountKind,
    id: String,
) {
    match kind {
        AccountKind::Proton => {
            if sessions.lock().await.get(&id).is_none() {
                // A failure is the worker's to retry (spec/04).
                let _ = crate::proton::restore_proton_account(worker, sessions, &id).await;
            }
            crate::proton_sync::start(worker, jobs, sessions, opened, evidence, verifications, id);
        }
        AccountKind::Imap => crate::account_sync::start_account(worker, jobs, threads, id),
        AccountKind::Local => {}
    }
}

/// A display name is trimmed, 1–128 UTF-8 bytes and control-free
/// (spec/07); the trimmed name is what stores.
fn valid_display_name(name: &str) -> Result<String, EngineError> {
    let trimmed = name.trim();
    if trimmed.is_empty() || trimmed.len() > MAX_DISPLAY_NAME_BYTES {
        return Err(EngineError::invalid_input(
            "display_name",
            format!(
                "a display name is 1 through {MAX_DISPLAY_NAME_BYTES} UTF-8 bytes once trimmed"
            ),
        ));
    }
    if trimmed.chars().any(char::is_control) {
        return Err(EngineError::invalid_input(
            "display_name",
            "a display name has no control characters",
        ));
    }
    Ok(trimmed.to_owned())
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
