//! Proton sign-in conversations (spec/07); a replayed key answers
//! without contacting Proton.

use crate::EngineError;
use crate::engine::{Engine, Worker, validate_idempotency_key};
use sb_resources::{
    BEGIN_PROTON_SIGN_IN, FinishedSignIn, ProtonRefusal, ProtonSignInParams, Secret, SecretError,
    SignIn, SignInStep,
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
            let account_id = worker.resources().new_uuid_string();
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
    worker
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
        .map_err(EngineError::from)
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
