//! Proton sign-in conversations (spec/07); a replayed key answers
//! without contacting Proton.

use crate::EngineError;
use crate::engine::{Engine, Spawn, Worker, validate_idempotency_key};
use sb_resources::{
    AccountKind, AccountState, BEGIN_PROTON_SIGN_IN, CredentialPause, FinishedSignIn, ProtonError,
    ProtonRefusal, ProtonSignInParams, RestoredProtonSession, Secret, SecretError, Session, SignIn,
    SignInStep, WritePriority, accounts, decode_proton_session, disconnect_proton_account,
    pause_for_credential, pause_for_session, proton_account,
};

use std::collections::HashMap;
use std::sync::Arc;
use std::time::Duration;

const CONVERSATION_TTL: Duration = Duration::from_secs(10 * 60);

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ProtonSignInState {
    NeedsSecondFactor,
    NeedsMailboxPassword,
    NeedsHumanVerification { url: String, methods: Vec<String> },
    Complete { account_id: String },
}

pub enum BeginProtonSignIn {
    Open {
        sign_in_id: String,
        state: ProtonSignInState,
    },
    Complete {
        receipt: sb_resources::Receipt,
    },
}

#[derive(Debug)]
pub enum ProtonSignInStep {
    SecondFactor { code: String },
    MailboxPassword { bytes: Secret },
    HumanVerification { token: String, token_type: String },
}

struct Conversation {
    idempotency_key: String,
    account_id: String,
    display_name: String,
    username: String,
    sign_in: SignIn,
    state: ProtonSignInState,
    started_ms: i64,
}

#[derive(Default)]
pub(crate) struct SignIns {
    by_id: HashMap<String, Conversation>,
    by_key: HashMap<String, String>,
}

/// A session restored from the secret store at open and kept in memory
/// with its unlocked keys (spec/04 "Restore on start"). The keys stay
/// in memory only (spec/10).
pub(crate) struct RestoredSession {
    pub session: Session,
    /// Unlocked for the account's lifetime; the sync steps (M2.15+)
    /// read them, and the test seam observes them.
    #[cfg_attr(not(any(test, feature = "test-seams")), allow(dead_code))]
    pub keys: Box<dyn sb_resources::KeyStore>,
}

/// The restored sessions by account. Its lock also serializes every
/// write of an account's secret item (sign-in, restore, disconnect),
/// so a slower one never stores a session another has replaced.
pub(crate) type Sessions = tokio::sync::Mutex<HashMap<String, RestoredSession>>;

impl Engine {
    /// `begin_proton_sign_in(params)` (spec/07): a repeated key answers
    /// the open conversation, or its stored receipt, with no contact.
    pub async fn begin_proton_sign_in(
        &self,
        idempotency_key: &str,
        params: ProtonSignInParams,
    ) -> Result<BeginProtonSignIn, EngineError> {
        validate_idempotency_key(idempotency_key)?;
        let key = idempotency_key.to_owned();
        let conversations = Arc::clone(&self.proton_sign_ins);
        let sessions = Arc::clone(&self.proton_sessions);
        self.command(move |worker| async move {
            let now = worker.resources().now().as_millis();
            let mut table = conversations.lock().await;
            prune(&mut table, now);
            if let Some(id) = table.by_key.get(&key).cloned() {
                if let Some(conversation) = table.by_id.get(&id) {
                    return Ok(BeginProtonSignIn::Open {
                        sign_in_id: id,
                        state: conversation.state.clone(),
                    });
                }
                table.by_key.remove(&key);
            }
            let read_key = key.clone();
            let receipt = worker
                .blocking(move |resources| {
                    resources.read(|tx| sb_resources::replay(tx, &read_key, BEGIN_PROTON_SIGN_IN))
                })
                .await??;
            if let Some(receipt) = receipt {
                return Ok(BeginProtonSignIn::Complete { receipt });
            }
            // Signing an existing account in again names it; an unknown
            // or non-Proton account is refused before Proton is
            // contacted (spec/07).
            if let Some(account_id) = params.account_id.as_deref()
                && account_kind(&worker, account_id).await? != Some(AccountKind::Proton)
            {
                return Err(EngineError::invalid_input(
                    "account",
                    format!("account {account_id} is not a Proton account"),
                ));
            }
            let account_id = match &params.account_id {
                Some(account_id) => account_id.clone(),
                None => worker.resources().new_uuid_string(),
            };
            let display_name = params.display_name.clone();
            let username = params.username.clone();
            let started = worker
                .resources()
                .begin_proton_sign_in(params)
                .await
                .map_err(EngineError::from)?;
            let sign_in_id = worker.resources().new_uuid_string();
            let (mut sign_in, state) = started;
            match state {
                SignInStep::Complete(complete) => {
                    let finished = sign_in.finish_keys(complete).await.map_err(|error| {
                        EngineError::from(ProtonRefusal::from_transport(error, false))
                    })?;
                    let completed = complete_conversation(
                        &worker,
                        &sessions,
                        &key,
                        &account_id,
                        &display_name,
                        &username,
                        finished,
                    )
                    .await?;
                    Ok(BeginProtonSignIn::Open {
                        sign_in_id,
                        state: ProtonSignInState::Complete {
                            account_id: completed,
                        },
                    })
                }
                state => {
                    let answer = state_of(&state);
                    table.by_id.insert(
                        sign_in_id.clone(),
                        Conversation {
                            idempotency_key: key.clone(),
                            account_id,
                            display_name,
                            username,
                            sign_in,
                            state: answer.clone(),
                            started_ms: now,
                        },
                    );
                    table.by_key.insert(key, sign_in_id.clone());
                    Ok(BeginProtonSignIn::Open {
                        sign_in_id,
                        state: answer,
                    })
                }
            }
        })
        .await
    }
    /// `continue_proton_sign_in` (spec/07): a wrong code or mailbox
    /// password keeps the conversation answerable.
    pub async fn continue_proton_sign_in(
        &self,
        sign_in_id: &str,
        step: ProtonSignInStep,
    ) -> Result<ProtonSignInState, EngineError> {
        let id = sign_in_id.to_owned();
        let conversations = Arc::clone(&self.proton_sign_ins);
        let sessions = Arc::clone(&self.proton_sessions);
        self.command(move |worker| async move {
            let now = worker.resources().now().as_millis();
            let mut table = conversations.lock().await;
            prune(&mut table, now);
            let Some(conversation) = table.by_id.get_mut(&id) else {
                return Err(EngineError::NotFound {
                    what: format!("sign-in conversation {id}"),
                });
            };
            let mailbox_password = matches!(step, ProtonSignInStep::MailboxPassword { .. });
            let challenge = to_challenge(step)?;
            let next = conversation
                .sign_in
                .continue_sign_in(challenge)
                .await
                .map_err(|error| {
                    EngineError::from(ProtonRefusal::from_transport(error, mailbox_password))
                })?;
            match next {
                SignInStep::Complete(complete) => {
                    let finished =
                        conversation
                            .sign_in
                            .finish_keys(complete)
                            .await
                            .map_err(|error| {
                                EngineError::from(ProtonRefusal::from_transport(
                                    error,
                                    mailbox_password,
                                ))
                            })?;
                    // The conversation ends here whatever the commit
                    // answers; the password drops with its handle.
                    let Some(ended) = table.by_id.remove(&id) else {
                        unreachable!("the conversation was looked up above");
                    };
                    table.by_key.remove(&ended.idempotency_key);
                    let completed = complete_conversation(
                        &worker,
                        &sessions,
                        &ended.idempotency_key,
                        &ended.account_id,
                        &ended.display_name,
                        &ended.username,
                        finished,
                    )
                    .await?;
                    Ok(ProtonSignInState::Complete {
                        account_id: completed,
                    })
                }
                state => {
                    let answer = state_of(&state);
                    conversation.state = answer.clone();
                    Ok(answer)
                }
            }
        })
        .await
    }

    /// `cancel_proton_sign_in` (spec/07): drop the conversation.
    pub async fn cancel_proton_sign_in(&self, sign_in_id: &str) -> Result<(), EngineError> {
        let id = sign_in_id.to_owned();
        let conversations = Arc::clone(&self.proton_sign_ins);
        self.command(move |worker| async move {
            let now = worker.resources().now().as_millis();
            let mut table = conversations.lock().await;
            prune(&mut table, now);
            let Some(conversation) = table.by_id.remove(&id) else {
                return Err(EngineError::NotFound {
                    what: format!("sign-in conversation {id}"),
                });
            };
            table.by_key.remove(&conversation.idempotency_key);
            Ok(())
        })
        .await
    }

    /// Restore every Proton account's session in the background after
    /// open (spec/04 "On every start"): the open itself never waits for
    /// Proton. Each account not disconnected reads its secret item,
    /// refreshes and unlocks its keys without the password; a missing or
    /// unreadable item, or a refused refresh, marks it `needs_attention`
    /// and keeps its data.
    pub(crate) fn schedule_proton_restore(&self) {
        let sessions = Arc::clone(&self.proton_sessions);
        let done = Arc::clone(&self.proton_restore_done);
        let _ = self.spawn_worker(move |worker| async move {
            let accounts = worker.blocking(|resources| resources.read(accounts)).await;
            if let Ok(Ok(accounts)) = accounts {
                for account in accounts {
                    if account.kind != AccountKind::Proton
                        || account.state == AccountState::Disconnected
                    {
                        continue;
                    }
                    restore_proton_account(&worker, &sessions, &account.account_id).await;
                }
            }
            done.notify_one();
        });
    }

    /// `disconnect_account(id)` (spec/07, 04): revoke a Proton
    /// account's session, delete its secret and mark it disconnected,
    /// keeping every row and its data. Only a Proton account; another
    /// kind is `InvalidInput` on `account`.
    pub async fn disconnect_account(&self, account_id: &str) -> Result<(), EngineError> {
        let id = account_id.to_owned();
        let sessions = Arc::clone(&self.proton_sessions);
        self.command(move |worker| async move {
            match account_kind(&worker, &id).await? {
                Some(AccountKind::Proton) => {}
                Some(_) => {
                    return Err(EngineError::invalid_input(
                        "account",
                        format!("account {id} is not a Proton account"),
                    ));
                }
                None => {
                    return Err(EngineError::NotFound {
                        what: format!("account {id}"),
                    });
                }
            }
            // Held to the end, so a restore finishing meanwhile can't
            // store the secret again.
            let mut sessions = sessions.lock().await;
            let restored = sessions.remove(&id);
            if let Some(restored) = restored {
                // Best effort: a failed revoke never blocks the local
                // disconnect (spec/04).
                let _ = worker
                    .resources()
                    .logout_proton_session(&restored.session)
                    .await;
            }
            let reference = worker
                .blocking({
                    let id = id.clone();
                    move |resources| resources.read(|tx| proton_account(tx, &id))
                })
                .await??
                .ok_or_else(|| EngineError::NotFound {
                    what: format!("account {id}"),
                })?
                .credential_ref;
            match worker
                .blocking(move |resources| resources.delete_secret(&reference))
                .await
            {
                Ok(Ok(())) | Ok(Err(SecretError::NotFound)) => {}
                Ok(Err(error)) => {
                    let pause = credential_pause(error);
                    let id = id.clone();
                    let _ = worker
                        .resources()
                        .write(WritePriority::User, move |tx| {
                            pause_for_credential(tx, &id, pause)
                        })
                        .await;
                    return Err(EngineError::unavailable(format!(
                        "the credential store could not delete the account's secret: {error}"
                    )));
                }
                Err(error) => return Err(error),
            }
            worker
                .resources()
                .write(WritePriority::User, move |tx| {
                    disconnect_proton_account(tx, &id)
                })
                .await?;
            Ok(())
        })
        .await
    }

    /// Test seam: wait for the open-time restore pass to finish.
    #[cfg(any(test, feature = "test-seams"))]
    #[doc(hidden)]
    pub async fn proton_restore_settled(&self) {
        self.proton_restore_done.notified().await;
    }

    /// Test seam: the restored session's unlocked identities, or `None`
    /// when the account has no restored session.
    #[cfg(any(test, feature = "test-seams"))]
    #[doc(hidden)]
    pub async fn proton_session(&self, account_id: &str) -> Option<Vec<sb_resources::Identity>> {
        let sessions = self.proton_sessions.lock().await;
        sessions
            .get(account_id)
            .map(|restored| restored.keys.identities().to_vec())
    }
}

/// Whether the account is disconnected; an unreadable store counts as
/// disconnected, so restore stores nothing it can't check.
async fn is_disconnected(worker: &Worker, account_id: &str) -> bool {
    let id = account_id.to_owned();
    match worker
        .blocking(move |resources| resources.read(accounts))
        .await
    {
        Ok(Ok(accounts)) => accounts
            .iter()
            .find(|account| account.account_id == id)
            .is_none_or(|account| account.state == AccountState::Disconnected),
        _ => true,
    }
}

/// The account's kind, or `None` when it doesn't exist.
async fn account_kind(
    worker: &Worker,
    account_id: &str,
) -> Result<Option<AccountKind>, EngineError> {
    let id = account_id.to_owned();
    let accounts = worker
        .blocking(move |resources| resources.read(accounts))
        .await??;
    Ok(accounts
        .into_iter()
        .find(|account| account.account_id == id)
        .map(|account| account.kind))
}

/// The credential pause a secret-store failure shows (spec/10: kinds
/// only).
fn credential_pause(error: SecretError) -> CredentialPause {
    match error {
        SecretError::NotFound => CredentialPause::NotStored,
        SecretError::Unavailable => CredentialPause::StoreUnavailable,
        SecretError::Refused => CredentialPause::StoreRefused,
        SecretError::InvalidRef => CredentialPause::StoreFailed,
    }
}

/// Restore one account's session (spec/04 "Restore on start").
async fn restore_proton_account(worker: &Worker, sessions: &Sessions, account_id: &str) {
    let id = account_id.to_owned();
    let reference = match worker
        .blocking({
            let id = id.clone();
            move |resources| resources.read(|tx| proton_account(tx, &id))
        })
        .await
    {
        Ok(Ok(Some(account))) => account.credential_ref,
        _ => return,
    };
    let get_reference = reference.clone();
    let secret = match worker
        .blocking(move |resources| resources.get_secret(&get_reference))
        .await
    {
        Ok(Ok(secret)) => secret,
        Ok(Err(SecretError::NotFound)) => {
            mark_session_refused(worker, account_id).await;
            return;
        }
        Ok(Err(error)) => {
            pause_for_secret_error(worker, account_id, error).await;
            return;
        }
        Err(_) => return,
    };
    let decoded = match decode_proton_session(secret.expose()) {
        Ok(decoded) => decoded,
        Err(_) => {
            mark_session_refused(worker, account_id).await;
            return;
        }
    };
    drop(secret);
    let restored = match worker
        .resources()
        .restore_proton_session(
            decoded.uid(),
            decoded.refresh_token(),
            decoded.key_passphrase(),
        )
        .await
    {
        Ok(restored) => restored,
        Err(ProtonError::RefreshRefused { .. }) => {
            mark_session_refused(worker, account_id).await;
            return;
        }
        // Offline and other transport failures leave the account as it
        // is; the next start tries again (spec/04).
        Err(_) => return,
    };
    // Proton rotated the refresh token: store the new item before the
    // session is used (spec/04 "Refresh").
    let RestoredProtonSession {
        session,
        keys,
        secret,
    } = restored;
    let mut sessions = sessions.lock().await;
    // A sign-in or disconnect that ran meanwhile owns the secret item
    // now: this session is surplus, so it is revoked, never stored.
    if sessions.contains_key(account_id) || is_disconnected(worker, account_id).await {
        let _ = worker.resources().logout_proton_session(&session).await;
        return;
    }
    let set_reference = reference.clone();
    let stored = worker
        .blocking(move |resources| resources.set_secret(&set_reference, &secret))
        .await;
    match stored {
        Ok(Ok(())) => {
            sessions.insert(account_id.to_owned(), RestoredSession { session, keys });
        }
        Ok(Err(error)) => pause_for_secret_error(worker, account_id, error).await,
        // The blocking join failed (shutdown): leave the account as it
        // is rather than report a store condition that never happened.
        Err(_) => {}
    }
}

/// Mark the account `needs_attention` ("Sign in again") without
/// touching its data (spec/04).
async fn mark_session_refused(worker: &Worker, account_id: &str) {
    let id = account_id.to_owned();
    let _ = worker
        .resources()
        .write(WritePriority::Sync, move |tx| pause_for_session(tx, &id))
        .await;
}

/// A secret-store failure pauses the account (spec/10), never deletes.
async fn pause_for_secret_error(worker: &Worker, account_id: &str, error: SecretError) {
    let pause = credential_pause(error);
    let id = account_id.to_owned();
    let _ = worker
        .resources()
        .write(WritePriority::Sync, move |tx| {
            pause_for_credential(tx, &id, pause)
        })
        .await;
}

/// Drops conversations past their ten minutes.
fn prune(table: &mut SignIns, now_ms: i64) {
    let expired: Vec<String> = table
        .by_id
        .iter()
        .filter(|(_, conversation)| {
            now_ms.saturating_sub(conversation.started_ms) >= CONVERSATION_TTL.as_millis() as i64
        })
        .map(|(id, _)| id.clone())
        .collect();
    for id in expired {
        if let Some(conversation) = table.by_id.remove(&id) {
            table.by_key.remove(&conversation.idempotency_key);
        }
    }
}

fn state_of(step: &SignInStep) -> ProtonSignInState {
    match step {
        SignInStep::NeedsSecondFactor => ProtonSignInState::NeedsSecondFactor,
        SignInStep::NeedsMailboxPassword => ProtonSignInState::NeedsMailboxPassword,
        SignInStep::NeedsHumanVerification(challenge) => {
            ProtonSignInState::NeedsHumanVerification {
                url: challenge.url.clone(),
                methods: challenge.methods.clone(),
            }
        }
        SignInStep::Complete(_) => unreachable!("completion is handled by the caller"),
    }
}

fn to_challenge(step: ProtonSignInStep) -> Result<sb_resources::Challenge, EngineError> {
    Ok(match step {
        ProtonSignInStep::SecondFactor { code } => sb_resources::Challenge::SecondFactor {
            code: sb_resources::SecretString::from(code),
        },
        ProtonSignInStep::MailboxPassword { bytes } => {
            let text = std::str::from_utf8(bytes.expose()).map_err(|_| {
                EngineError::invalid_input("mailbox_password", "the mailbox password must be UTF-8")
            })?;
            sb_resources::Challenge::MailboxPassword {
                password: sb_resources::SecretString::from(text.to_owned()),
            }
        }
        ProtonSignInStep::HumanVerification { token, token_type } => {
            sb_resources::Challenge::HumanVerification {
                token: sb_resources::SecretString::from(token),
                token_type,
            }
        }
    })
}

/// Commits a completed conversation, then writes the secret item.
async fn complete_conversation(
    worker: &Worker,
    sessions: &Sessions,
    idempotency_key: &str,
    account_id: &str,
    display_name: &str,
    username: &str,
    finished: FinishedSignIn,
) -> Result<String, EngineError> {
    let setter = worker.clone();
    let store_secret = move |credential_ref: String, secret: Secret| async move {
        // A failed join is a store that kept nothing: the account pauses.
        setter
            .blocking(move |resources| resources.set_secret(&credential_ref, &secret))
            .await
            .unwrap_or(Err(SecretError::Unavailable))
    };
    let mut sessions = sessions.lock().await;
    let completed = worker
        .resources()
        .complete_proton_sign_in(
            idempotency_key,
            account_id,
            display_name,
            username,
            &finished,
            store_secret,
        )
        .await
        .map_err(EngineError::from)?;
    // The new session replaces one restored at open, which signing in
    // again made stale.
    let FinishedSignIn { session, keys, .. } = finished;
    sessions.insert(completed.clone(), RestoredSession { session, keys });
    Ok(completed)
}

impl From<ProtonRefusal> for EngineError {
    fn from(error: ProtonRefusal) -> Self {
        match error {
            ProtonRefusal::InvalidInput { field, message } => Self::invalid_input(field, message),
            ProtonRefusal::Unavailable { message } => Self::unavailable(message),
            ProtonRefusal::Store(error) => error.into(),
        }
    }
}
