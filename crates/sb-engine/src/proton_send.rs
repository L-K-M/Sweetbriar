//! Proton send submission on the account worker (spec/05 "Outbox
//! state machine" step 4; plan M4.3e2). One claimed attempt at a
//! time: the recipient key lookups, the draft create or update, the
//! packages, `send_requested_at`, the send and the reply's
//! interpretation. A failure before the send request requeues the
//! attempt under an in-memory backoff; a lost send reply becomes
//! `outcome_unknown` and is never retried.
//!
//! Nothing here persists a backoff or a key: the backoff lives in the
//! worker's memory (spec/05 step 7), and the sender key stays in the
//! session's unlocked key set.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Duration;

use sb_resources::{
    APP_IDENTITY_REFUSED_REASON, ClaimedRecipient, ClaimedSend, DraftAction, DraftAddress,
    DraftCreate, DraftTemplate, FrozenSend, KeyStore, ProtonClient, RECIPIENT_TYPE_EXTERNAL,
    RECIPIENT_TYPE_INTERNAL, RecipientKeys, RecipientKind, RefusalKind, SESSION_REFUSED_REASON,
    SendAttachment, SendBody, SendError, SendFailure, SendFailureRejection, SendPackages,
    SendParentKind, SendRecipient, SendRefusal, SendTransportError, Session, Timestamp,
    WritePriority, claim_send_attempt, commit_send_requested, mark_outcome_unknown,
    mark_send_accepted, mark_send_failed, pause_for_reason, pause_for_session, record_proton_draft,
    requeue_before_request,
};

use crate::proton::Sessions;
use crate::proton_sync::Halt;
use sb_resources::proton_sync::record_refusal;

/// How many queued attempts one worker pass submits, so a large outbox
/// can't starve the account's events (plan M4.3e2).
const MAX_SUBMITS_PER_PASS: usize = 4;
/// The first wait after a failure before the send request; it doubles
/// up to [`BACKOFF_MAX`] (spec/05 step 7: nothing to persist).
const BACKOFF_FIRST: Duration = Duration::from_secs(2);
const BACKOFF_MAX: Duration = Duration::from_secs(5 * 60);
/// Proton codes that prove a send was refused before anything was sent
/// (spec/05 step 5). The list starts empty and grows only with the
/// `go-proton-api` lane's evidence (M4.3g).
pub const QUALIFIED_PRE_SUBMISSION_CODES: &[i64] = &[];
/// Why the pilot refuses an external recipient whose key lookup
/// returned keys (spec/05 step 1): Sweetbriar never downgrades a key
/// the endpoint knows to clear text.
const EXTERNAL_KEYS_REASON: &str =
    "Sweetbriar's pilot doesn't send to external recipients with keys";

/// The in-memory pre-request backoff, per attempt (spec/05 step 7).
/// Lost on a restart, reset when an attempt reaches a terminal state.
#[derive(Default)]
pub(crate) struct SendBackoff {
    /// Each waiting attempt's deadline and its delay for the next wait.
    next: HashMap<String, (Timestamp, Duration)>,
}

impl SendBackoff {
    /// Whether the attempt's backoff has elapsed at `now`.
    fn ready(&self, attempt_id: &str, now: Timestamp) -> bool {
        self.next
            .get(attempt_id)
            .is_none_or(|(deadline, _)| *deadline <= now)
    }

    /// Record a failure: wait this attempt's current delay, then double
    /// it (to [`BACKOFF_MAX`]) for the next.
    fn record_failure(&mut self, attempt_id: &str, now: Timestamp) {
        let delay = self
            .next
            .get(attempt_id)
            .map(|(_, delay)| *delay)
            .unwrap_or(BACKOFF_FIRST);
        let deadline = Timestamp::from_millis(now.as_millis() + delay.as_millis() as i64);
        self.next.insert(
            attempt_id.to_owned(),
            (deadline, (delay * 2).min(BACKOFF_MAX)),
        );
    }

    /// The attempt reached a terminal state: its backoff is over.
    fn record_success(&mut self, attempt_id: &str) {
        self.next.remove(attempt_id);
    }

    /// The earliest backoff deadline, so the worker's poll wait wakes
    /// for it (spec/05 step 7).
    pub(crate) fn earliest_deadline(&self) -> Option<Timestamp> {
        self.next.values().map(|(deadline, _)| *deadline).min()
    }
}

/// Submit the account's queued attempts, one at a time, at most
/// [`MAX_SUBMITS_PER_PASS`] a pass. A requeued attempt ends the pass:
/// its backoff says when it is next worth claiming.
pub(crate) async fn submit_queued_attempts(
    worker: &crate::engine::Worker,
    sessions: &Sessions,
    client: &ProtonClient,
    account_id: &str,
    backoff: &mut SendBackoff,
) -> Result<(), Halt> {
    let mut attempted = HashSet::new();
    for _ in 0..MAX_SUBMITS_PER_PASS {
        let now = worker.resources().now();
        let id = account_id.to_owned();
        let claimed = worker
            .resources()
            .write(WritePriority::Sync, move |tx| {
                claim_send_attempt(tx, &id, now)
            })
            .await?;
        let Some(claimed) = claimed else {
            return Ok(());
        };
        if !attempted.insert(claimed.attempt_id.clone()) {
            return Ok(());
        }
        if !backoff.ready(&claimed.attempt_id, worker.resources().now()) {
            // The oldest queued attempt is waiting out its backoff;
            // leave it claimed-then-requeued and try the next pass.
            let now = worker.resources().now();
            let id = claimed.attempt_id.clone();
            worker
                .resources()
                .write(WritePriority::Sync, move |tx| {
                    requeue_before_request(tx, &id, now)
                })
                .await?;
            return Ok(());
        }
        if matches!(
            submit_one(worker, sessions, client, account_id, claimed, backoff).await?,
            SubmitOutcome::Requeued
        ) {
            return Ok(());
        }
    }
    Ok(())
}

/// How one attempt's submission ended.
enum SubmitOutcome {
    /// A terminal state was committed (or another transition won).
    Done,
    /// The attempt is back in `queued`, waiting out its backoff or for
    /// M4.3e3's uploads.
    Requeued,
}

async fn submit_one(
    worker: &crate::engine::Worker,
    sessions: &Sessions,
    client: &ProtonClient,
    account_id: &str,
    claimed: ClaimedSend,
    backoff: &mut SendBackoff,
) -> Result<SubmitOutcome, Halt> {
    let sha = claimed.mime_sha256.clone();
    let bytes = match worker
        .blocking(move |resources| resources.read_object_by_sha256(&sha))
        .await
    {
        Ok(Ok(bytes)) => bytes,
        Ok(Err(error)) => return Err(error.into()),
        Err(_) => return Err(Halt::Stopped),
    };
    let frozen = match sb_resources::frozen_send(&bytes) {
        Ok(frozen) => frozen,
        Err(message) => {
            let failure = SendFailure {
                code: None,
                message,
                rejected: None,
            };
            let now = worker.resources().now();
            let id = claimed.attempt_id.clone();
            fail(worker, &id, &failure, now).await?;
            backoff.record_success(&claimed.attempt_id);
            return Ok(SubmitOutcome::Done);
        }
    };
    if !frozen.attachments.is_empty() {
        // M4.3e3 uploads them; until then the attempt waits in `queued`
        // and nothing reaches Proton.
        let now = worker.resources().now();
        let id = claimed.attempt_id.clone();
        worker
            .resources()
            .write(WritePriority::Sync, move |tx| {
                requeue_before_request(tx, &id, now)
            })
            .await?;
        return Ok(SubmitOutcome::Requeued);
    }
    let (session, keys) = session_and_keys(sessions, account_id).await?;

    // Step 1: each recipient's key lookup. An external address whose
    // lookup returned keys fails the attempt before any draft exists.
    let mut recipients = Vec::with_capacity(claimed.recipients.len());
    for recipient in &claimed.recipients {
        let address = recipient.canonical.clone();
        let keys = match client.recipient_keys(&session, &address).await {
            Ok(keys) => keys,
            Err(error) => {
                record_diagnostics(worker, account_id, &error).await;
                let mapped = interpret(&error, false);
                return apply_action(worker, account_id, &claimed, mapped, false, backoff).await;
            }
        };
        match plan_recipient(recipient, keys) {
            Ok(recipient) => recipients.push(recipient),
            Err(failure) => {
                let now = worker.resources().now();
                let id = claimed.attempt_id.clone();
                fail(worker, &id, &failure, now).await?;
                backoff.record_success(&claimed.attempt_id);
                return Ok(SubmitOutcome::Done);
            }
        }
    }

    // Step 2: the sender's key encrypts and signs the frozen body.
    let encrypted = match keys.encrypt_draft_body(&claimed.envelope_from, &frozen.body.data) {
        Ok(encrypted) => encrypted,
        Err(error) => {
            let action = sender_error_action(error);
            return apply_action(worker, account_id, &claimed, action, false, backoff).await;
        }
    };
    let body = match String::from_utf8(encrypted) {
        Ok(body) => body,
        Err(_) => {
            let action = sender_error_action(SendError::Crypto);
            return apply_action(worker, account_id, &claimed, action, false, backoff).await;
        }
    };
    let (template, parent_id, action) = draft_parts(&claimed, &frozen, body);
    let draft_id = match &claimed.proton_draft_id {
        // A recorded draft is updated, never recreated (spec/05 step 2).
        Some(recorded) => {
            let recorded = recorded.clone();
            match client.update_draft(&session, &recorded, &template).await {
                Ok(_) => recorded,
                Err(error) => {
                    record_diagnostics(worker, account_id, &error).await;
                    let mapped = interpret(&error, false);
                    return apply_action(worker, account_id, &claimed, mapped, false, backoff)
                        .await;
                }
            }
        }
        None => {
            let create = DraftCreate {
                template,
                parent_id,
                action,
            };
            match client.create_draft(&session, &create).await {
                Ok(draft) => {
                    // The id is committed before the next request; a
                    // crash here leaves an unnamed draft in Proton's
                    // Drafts (spec/05 step 2).
                    let id = claimed.attempt_id.clone();
                    let created = draft.id.clone();
                    let applied = worker
                        .resources()
                        .write(WritePriority::Sync, move |tx| {
                            record_proton_draft(tx, &id, &created)
                        })
                        .await?;
                    if !applied {
                        return Ok(SubmitOutcome::Done);
                    }
                    draft.id
                }
                Err(error) => {
                    record_diagnostics(worker, account_id, &error).await;
                    let mapped = interpret(&error, false);
                    return apply_action(worker, account_id, &claimed, mapped, false, backoff)
                        .await;
                }
            }
        }
    };

    // Step 4: the packages, then `send_requested_at` in its own write.
    let bodies = [SendBody {
        mime_type: &frozen.body.mime_type,
        data: &frozen.body.data,
    }];
    let attachments: [SendAttachment; 0] = [];
    let packages: SendPackages =
        match keys.build_packages(&claimed.envelope_from, &recipients, &bodies, &attachments) {
            Ok(packages) => packages,
            Err(error) => {
                let action = sender_error_action(error);
                return apply_action(worker, account_id, &claimed, action, false, backoff).await;
            }
        };
    let now = worker.resources().now();
    let id = claimed.attempt_id.clone();
    let committed = worker
        .resources()
        .write(WritePriority::Sync, move |tx| {
            commit_send_requested(tx, &id, now)
        })
        .await?;
    if !committed {
        return Ok(SubmitOutcome::Done);
    }

    // Step 5: the send and its reply's interpretation.
    match client.send_packages(&session, &draft_id, &packages).await {
        Ok(_) => {
            let now = worker.resources().now();
            let id = claimed.attempt_id.clone();
            let applied = worker
                .resources()
                .write(WritePriority::Sync, move |tx| {
                    mark_send_accepted(tx, &id, now)
                })
                .await?;
            if applied {
                backoff.record_success(&claimed.attempt_id);
            }
            Ok(SubmitOutcome::Done)
        }
        Err(error) => {
            record_diagnostics(worker, account_id, &error).await;
            let mapped = interpret(&error, true);
            apply_action(worker, account_id, &claimed, mapped, true, backoff).await
        }
    }
}

/// The account's current session and unlocked keys, copied out of the
/// session map under the lock only for the copies (spec/10).
async fn session_and_keys(
    sessions: &Sessions,
    account_id: &str,
) -> Result<(Session, Arc<dyn KeyStore>), Halt> {
    let sessions = sessions.lock().await;
    let restored = sessions.get(account_id).ok_or(Halt::Ended)?;
    Ok((restored.session.clone(), Arc::clone(&restored.keys)))
}

/// One recipient's plan from its key lookup: encrypt to its keys
/// (internal) or send it clear (external without keys). An external
/// address whose lookup returned keys is refused permanently, before
/// any draft; so is an answer that can't be sent to.
fn plan_recipient(
    recipient: &ClaimedRecipient,
    keys: RecipientKeys,
) -> Result<SendRecipient, SendFailure> {
    let send = |keys| {
        Ok(SendRecipient {
            address: recipient.canonical.clone(),
            keys,
        })
    };
    let failed = |message| SendFailure {
        code: None,
        message,
        rejected: None,
    };
    match keys.recipient_type {
        RECIPIENT_TYPE_INTERNAL if keys.encryption_key().is_some() => send(keys),
        RECIPIENT_TYPE_INTERNAL => {
            Err(failed(format!("no active key for {}", recipient.canonical)))
        }
        RECIPIENT_TYPE_EXTERNAL if keys.keys.is_empty() => send(keys),
        RECIPIENT_TYPE_EXTERNAL => Err(SendFailure {
            code: None,
            message: format!(
                "{} has keys, and the pilot doesn't send to external keys",
                recipient.canonical
            ),
            rejected: Some(SendFailureRejection {
                canonical: recipient.canonical.clone(),
                reason: EXTERNAL_KEYS_REASON.to_owned(),
            }),
        }),
        _ => Err(failed(format!(
            "{} has an unknown recipient type",
            recipient.canonical
        ))),
    }
}

/// The draft's parts (spec/05 step 2): the template from the frozen
/// message and the claimed envelope, and the parent with Proton's
/// action 0 for a reply, 2 for a forward, none without a parent.
fn draft_parts(
    claimed: &ClaimedSend,
    frozen: &FrozenSend,
    body: String,
) -> (DraftTemplate, Option<String>, Option<DraftAction>) {
    let mut to = Vec::new();
    let mut cc = Vec::new();
    let mut bcc = Vec::new();
    for recipient in &claimed.recipients {
        let address = DraftAddress {
            name: String::new(),
            address: recipient.address.clone(),
        };
        match recipient.kind {
            RecipientKind::To => to.push(address),
            RecipientKind::Cc => cc.push(address),
            RecipientKind::Bcc => bcc.push(address),
        }
    }
    let (parent_id, action) = match &claimed.parent {
        Some(parent) => {
            let action = match parent.kind {
                SendParentKind::Reply => DraftAction::Reply,
                SendParentKind::Forward => DraftAction::Forward,
            };
            (Some(parent.provider_message_id.clone()), Some(action))
        }
        None => (None, None),
    };
    (
        DraftTemplate {
            sender: DraftAddress {
                name: String::new(),
                address: claimed.envelope_from.clone(),
            },
            to,
            cc,
            bcc,
            subject: frozen.subject.clone(),
            mime_type: frozen.body.mime_type.clone(),
            body,
        },
        parent_id,
        action,
    )
}

/// What a failure does (spec/05 step 5). `send_requested` says
/// whether `send_requested_at` was committed: before it, a lost reply,
/// a rate limit, a session to refresh or offline requeues with the
/// backoff; after it, only a connection never established requeues,
/// and every other reply or loss is an unknown outcome, never a
/// replay.
fn interpret(error: &SendTransportError, send_requested: bool) -> Action {
    match error {
        SendTransportError::NeverEstablished { .. } => Action::Retry,
        SendTransportError::Refused(refusal) => refusal_action(refusal, send_requested),
        SendTransportError::OutcomeUnknown { .. } | SendTransportError::PreRequest(_) => {
            if send_requested {
                Action::Unknown
            } else {
                Action::Retry
            }
        }
        SendTransportError::InvalidInput { message } => Action::failed(message.clone()),
    }
}

/// A Proton refusal's action: identity and session refusals need the
/// account; a qualified pre-submission code, or any definite refusal
/// before the send request, fails; after it, anything else is an
/// unknown outcome (spec/05 step 5).
fn refusal_action(refusal: &SendRefusal, send_requested: bool) -> Action {
    match refusal.kind {
        RefusalKind::Identity => Action::attention(APP_IDENTITY_REFUSED_REASON),
        RefusalKind::Session => Action::attention(SESSION_REFUSED_REASON),
        RefusalKind::Other if QUALIFIED_PRE_SUBMISSION_CODES.contains(&refusal.code) => {
            Action::failed_with_code(refusal.code, refusal.message.clone())
        }
        RefusalKind::Other if !send_requested => {
            Action::failed_with_code(refusal.code, refusal.message.clone())
        }
        RefusalKind::Other => Action::Unknown,
    }
}

/// A key failure: no unlocked sender key is a wait for the keys
/// (spec/05 step 7); a recipient key that can't be used, or any other
/// crypto failure, is definite.
fn sender_error_action(error: SendError) -> Action {
    match error {
        SendError::SenderKey => Action::Retry,
        other => Action::failed(other.to_string()),
    }
}

/// The action a failure maps to.
enum Action {
    /// Requeue before the send request, with the in-memory backoff.
    Retry,
    /// A definite pre-acceptance failure.
    Failed(SendFailure),
    /// The send's outcome is unknown; never retried automatically.
    Unknown,
    /// An identity or session refusal: pause the account and end the
    /// worker. Its diagnostics are recorded by the caller, which saw
    /// the refusal.
    Attention { reason: &'static str },
}

impl Action {
    fn failed(message: String) -> Self {
        Self::Failed(SendFailure {
            code: None,
            message,
            rejected: None,
        })
    }

    fn failed_with_code(code: i64, message: String) -> Self {
        Self::Failed(SendFailure {
            code: Some(code),
            message,
            rejected: None,
        })
    }

    fn attention(reason: &'static str) -> Self {
        Self::Attention { reason }
    }
}

/// Apply one failure's action. `send_requested` says whether
/// `send_requested_at` was committed for this attempt, which decides
/// an identity or session refusal's transition (spec/05 step 5).
async fn apply_action(
    worker: &crate::engine::Worker,
    account_id: &str,
    claimed: &ClaimedSend,
    action: Action,
    send_requested: bool,
    backoff: &mut SendBackoff,
) -> Result<SubmitOutcome, Halt> {
    let attempt_id = claimed.attempt_id.as_str();
    match action {
        Action::Retry => {
            let now = worker.resources().now();
            let id = claimed.attempt_id.clone();
            let applied = worker
                .resources()
                .write(WritePriority::Sync, move |tx| {
                    requeue_before_request(tx, &id, now)
                })
                .await?;
            if applied {
                backoff.record_failure(&claimed.attempt_id, worker.resources().now());
                return Ok(SubmitOutcome::Requeued);
            }
            Ok(SubmitOutcome::Done)
        }
        Action::Failed(failure) => {
            let now = worker.resources().now();
            let id = claimed.attempt_id.clone();
            fail(worker, &id, &failure, now).await?;
            backoff.record_success(attempt_id);
            Ok(SubmitOutcome::Done)
        }
        Action::Unknown => {
            let now = worker.resources().now();
            let id = claimed.attempt_id.clone();
            let applied = worker
                .resources()
                .write(WritePriority::Sync, move |tx| {
                    mark_outcome_unknown(tx, &id, now)
                })
                .await?;
            if applied {
                backoff.record_success(attempt_id);
            }
            Ok(SubmitOutcome::Done)
        }
        Action::Attention { reason } => {
            let now = worker.resources().now();
            let id = claimed.attempt_id.clone();
            let applied = if send_requested {
                worker
                    .resources()
                    .write(WritePriority::Sync, move |tx| {
                        mark_outcome_unknown(tx, &id, now)
                    })
                    .await?
            } else {
                worker
                    .resources()
                    .write(WritePriority::Sync, move |tx| {
                        requeue_before_request(tx, &id, now)
                    })
                    .await?
            };
            if applied {
                backoff.record_success(attempt_id);
            }
            // The worker ends only once the pause persisted, as the
            // sync refusals do (spec/04 "Account states and health").
            let id = account_id.to_owned();
            let paused = worker
                .resources()
                .write(WritePriority::Sync, move |tx| {
                    if reason == SESSION_REFUSED_REASON {
                        pause_for_session(tx, &id)
                    } else {
                        pause_for_reason(tx, &id, reason)
                    }
                })
                .await;
            if paused.is_err() {
                return Err(Halt::Retry);
            }
            worker.statuses().rows_changed();
            Err(Halt::Ended)
        }
    }
}

/// Record a refusal's code and message in the account's diagnostics,
/// best effort (spec/04: every refusal records them, redacted).
async fn record_diagnostics(
    worker: &crate::engine::Worker,
    account_id: &str,
    error: &SendTransportError,
) {
    let SendTransportError::Refused(refusal) = error else {
        return;
    };
    let now = worker.resources().now();
    let id = account_id.to_owned();
    let code = refusal.code;
    let message = refusal.message.clone();
    let _ = worker
        .resources()
        .write(WritePriority::Sync, move |tx| {
            record_refusal(tx, &id, Some(code), &message, now).map(|_| ())
        })
        .await;
}

async fn fail(
    worker: &crate::engine::Worker,
    attempt_id: &str,
    failure: &SendFailure,
    now: Timestamp,
) -> Result<(), Halt> {
    let id = attempt_id.to_owned();
    let failure = failure.clone();
    worker
        .resources()
        .write(WritePriority::Sync, move |tx| {
            mark_send_failed(tx, &id, &failure, now)
        })
        .await?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn only_a_missing_sender_key_waits() {
        // The keys may still unlock; a recipient key that can't be
        // used never will, so it must not requeue forever.
        assert!(matches!(
            sender_error_action(SendError::SenderKey),
            Action::Retry
        ));
        for definite in [
            SendError::RecipientKey,
            SendError::Crypto,
            SendError::NoRecipients,
            SendError::ExternalKeys,
        ] {
            assert!(matches!(sender_error_action(definite), Action::Failed(_)));
        }
    }
}
