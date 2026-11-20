//! Proton send submission on the account worker (spec/05 "Outbox
//! state machine" step 4; plan M4.3e2, M4.3e3). One claimed attempt
//! at a time: the recipient key lookups, the draft create or update,
//! the attachment uploads, the packages, `send_requested_at`, the send
//! and the reply's interpretation. A failure before the send request
//! requeues the attempt under an in-memory backoff; a lost send reply
//! becomes `outcome_unknown` and is never retried.
//!
//! Nothing here persists a backoff or a key: the backoff lives in the
//! worker's memory (spec/05 step 7), and the sender key stays in the
//! session's unlocked key set.

use std::collections::{HashMap, HashSet};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use sb_resources::{
    APP_IDENTITY_REFUSED_REASON, AttachmentUpload, ClaimedRecipient, ClaimedSend, DraftAction,
    DraftAddress, DraftCreate, DraftTemplate, FrozenAttachment, FrozenSend, KeyStore, ProtonClient,
    ProtonDraftAttachment, ProtonUpload, RECIPIENT_TYPE_EXTERNAL, RECIPIENT_TYPE_INTERNAL,
    RecipientKeys, RecipientKind, RefusalKind, SESSION_REFUSED_REASON, SendBody, SendError,
    SendFailure, SendFailureRejection, SendPackages, SendParentKind, SendRecipient, SendRefusal,
    SendTransportError, Session, StoreError, Timestamp, WritePriority, claim_send_attempt,
    commit_send_requested, mark_outcome_unknown, mark_send_accepted, mark_send_failed,
    pause_for_reason, pause_for_session, record_proton_draft, record_proton_upload,
    requeue_before_request,
};

/// Proton's Sent and Drafts system labels (spec/05 step 6): the
/// evidence check reads a draft's metadata and treats Sent (`7`) with
/// no Drafts (`8`) as Proton's own record that the message was sent.
pub(crate) const SENT_LABEL: &str = "7";
pub(crate) const DRAFTS_LABEL: &str = "8";

/// The latest Proton evidence observed per unresolved attempt, in
/// memory only (spec/05 step 6, spec/07 `out_box()`). A relaunch
/// starts empty; the account worker fills it as it reads drafts.
#[derive(Default)]
pub(crate) struct SendEvidence {
    latest: Mutex<HashMap<String, Vec<String>>>,
    /// Test seam: fired at each submission store transition, so a
    /// crash-cut child can die around it (spec/11). `None` outside
    /// tests.
    #[cfg(test)]
    hook: Mutex<Option<SendWriteHook>>,
}

/// A submission's store transition (spec/11 crash cuts): the writes
/// `proton_send` makes, each with a before and an after phase.
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SendTransition {
    Claim,
    RecordDraft,
    RecordUpload,
    SendRequested,
    Accepted,
    Failed,
    Unknown,
    Requeue,
}

/// Which side of a store write a crash cut lands on (spec/11: before,
/// inside and after each transaction).
#[cfg(test)]
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(crate) enum SendPhase {
    Before,
    After,
}

/// The crash-cut hook's signature: the transition and its phase.
#[cfg(test)]
pub(crate) type SendWriteHook = Arc<dyn Fn(SendTransition, SendPhase) + Send + Sync>;

impl SendEvidence {
    /// Record the latest observation for one attempt, replacing an
    /// earlier one: the Out Box shows what the worker last saw.
    pub(crate) fn record(&self, attempt_id: &str, observation: String) {
        self.latest
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .insert(attempt_id.to_owned(), vec![observation]);
    }

    /// Every attempt's latest observation, for `out_box()`.
    pub(crate) fn snapshot(&self) -> HashMap<String, Vec<String>> {
        self.latest
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .clone()
    }

    /// Install the test crash-cut hook.
    #[cfg(test)]
    pub(crate) fn set_hook(&self, hook: Option<SendWriteHook>) {
        *self.hook.lock().unwrap_or_else(|e| e.into_inner()) = hook;
    }

    /// Fire the test hook at one transition's phase.
    #[cfg(test)]
    pub(crate) fn fire(&self, transition: SendTransition, phase: SendPhase) {
        if let Some(hook) = self.hook.lock().unwrap_or_else(|e| e.into_inner()).as_ref() {
            hook(transition, phase);
        }
    }
}

/// What one draft read observed (spec/05 step 6). Only [`Sent`] acts:
/// the attempt becomes accepted through the store's guarded
/// transition. Every other observation is reported and changes
/// nothing.
pub(crate) enum Evidence {
    /// Proton shows the draft sent: it carries Sent and no Drafts.
    Sent,
    /// The draft still carries Drafts, so it wasn't sent.
    Drafts,
    /// Neither Sent-without-Drafts nor Drafts: proves nothing.
    Other,
    /// Proton has no such draft.
    NotFound,
    /// The read failed; nothing is known either way.
    ReadFailed,
}

impl Evidence {
    /// The line the Out Box shows for this observation. It names the
    /// draft and the labels, never a body or key (spec/10).
    pub(crate) fn line(&self, draft_id: &str) -> String {
        match self {
            Self::Sent => {
                format!("Proton shows draft {draft_id} sent (Sent, no Drafts)")
            }
            Self::Drafts => format!("Proton still shows draft {draft_id} in Drafts"),
            Self::Other => format!("Proton shows draft {draft_id} neither sent nor in Drafts"),
            Self::NotFound => format!("Proton has no draft {draft_id}"),
            Self::ReadFailed => format!("Reading draft {draft_id} failed"),
        }
    }
}

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
    evidence: &SendEvidence,
) -> Result<(), Halt> {
    let mut attempted = HashSet::new();
    for _ in 0..MAX_SUBMITS_PER_PASS {
        let now = worker.resources().now();
        let id = account_id.to_owned();
        #[cfg(test)]
        evidence.fire(SendTransition::Claim, SendPhase::Before);
        let claimed = worker
            .resources()
            .write(WritePriority::Sync, move |tx| {
                claim_send_attempt(tx, &id, now)
            })
            .await?;
        #[cfg(test)]
        evidence.fire(SendTransition::Claim, SendPhase::After);
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
            #[cfg(test)]
            evidence.fire(SendTransition::Requeue, SendPhase::Before);
            worker
                .resources()
                .write(WritePriority::Sync, move |tx| {
                    requeue_before_request(tx, &id, now)
                })
                .await?;
            #[cfg(test)]
            evidence.fire(SendTransition::Requeue, SendPhase::After);
            return Ok(());
        }
        if matches!(
            submit_one(
                worker, sessions, client, account_id, claimed, backoff, evidence
            )
            .await?,
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
    /// The attempt is back in `queued`, waiting out its backoff.
    Requeued,
}

async fn submit_one(
    worker: &crate::engine::Worker,
    sessions: &Sessions,
    client: &ProtonClient,
    account_id: &str,
    claimed: ClaimedSend,
    backoff: &mut SendBackoff,
    evidence: &SendEvidence,
) -> Result<SubmitOutcome, Halt> {
    let sha = claimed.mime_sha256.clone();
    let bytes = match worker
        .blocking(move |resources| resources.read_object_by_sha256(&sha))
        .await
    {
        Ok(Ok(bytes)) => bytes,
        // The frozen bytes are gone or damaged, so the attempt can never
        // be sent as frozen: it fails, rather than halting the account's
        // worker on every pass. Any other read failure is a retry.
        Ok(Err(StoreError::NotFound { .. } | StoreError::Corrupt { .. })) => {
            let failure = SendFailure {
                code: None,
                message: "the frozen message can't be read from the store".to_owned(),
                rejected: None,
            };
            let now = worker.resources().now();
            let id = claimed.attempt_id.clone();
            fail(worker, evidence, &id, &failure, now).await?;
            backoff.record_success(&claimed.attempt_id);
            return Ok(SubmitOutcome::Done);
        }
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
            fail(worker, evidence, &id, &failure, now).await?;
            backoff.record_success(&claimed.attempt_id);
            return Ok(SubmitOutcome::Done);
        }
    };
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
                return apply_action(
                    worker, account_id, &claimed, mapped, false, backoff, evidence,
                )
                .await;
            }
        };
        match plan_recipient(recipient, keys) {
            Ok(recipient) => recipients.push(recipient),
            Err(failure) => {
                let now = worker.resources().now();
                let id = claimed.attempt_id.clone();
                fail(worker, evidence, &id, &failure, now).await?;
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
            return apply_action(
                worker, account_id, &claimed, action, false, backoff, evidence,
            )
            .await;
        }
    };
    let body = match String::from_utf8(encrypted) {
        Ok(body) => body,
        Err(_) => {
            let action = sender_error_action(SendError::Crypto);
            return apply_action(
                worker, account_id, &claimed, action, false, backoff, evidence,
            )
            .await;
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
                    return apply_action(
                        worker, account_id, &claimed, mapped, false, backoff, evidence,
                    )
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
                    #[cfg(test)]
                    evidence.fire(SendTransition::RecordDraft, SendPhase::Before);
                    let applied = worker
                        .resources()
                        .write(WritePriority::Sync, move |tx| {
                            record_proton_draft(tx, &id, &created)
                        })
                        .await?;
                    #[cfg(test)]
                    evidence.fire(SendTransition::RecordDraft, SendPhase::After);
                    if !applied {
                        return Ok(SubmitOutcome::Done);
                    }
                    draft.id
                }
                Err(error) => {
                    record_diagnostics(worker, account_id, &error).await;
                    let mapped = interpret(&error, false);
                    return apply_action(
                        worker, account_id, &claimed, mapped, false, backoff, evidence,
                    )
                    .await;
                }
            }
        }
    };

    // Step 3: upload the frozen attachments to the draft, in the
    // frozen MIME's order, recording each before the next request. A
    // retry with a recorded draft reads it first: an attachment whose
    // reply was lost is adopted from the draft rather than uploaded
    // again (spec/05 step 3).
    let mut uploads = claimed.uploads.clone();
    let mut key_packets: HashMap<String, Vec<u8>> = HashMap::new();
    if let Some(recorded_draft) = claimed.proton_draft_id.clone() {
        // Every retry reads its recorded draft, even with no frozen
        // attachments: an attachment on the draft that the frozen
        // message doesn't carry must fail rather than ride along.
        let draft_attachments = match client.read_draft(&session, &recorded_draft).await {
            Ok(attachments) => attachments,
            Err(error) => {
                record_diagnostics(worker, account_id, &error).await;
                let mapped = interpret(&error, false);
                return apply_action(
                    worker, account_id, &claimed, mapped, false, backoff, evidence,
                )
                .await;
            }
        };
        match reconcile_draft_uploads(
            &recorded_draft,
            &draft_attachments,
            &frozen.attachments,
            &uploads,
        ) {
            Ok(recovered) => {
                for upload in recovered {
                    #[cfg(test)]
                    evidence.fire(SendTransition::RecordUpload, SendPhase::Before);
                    let applied = worker
                        .resources()
                        .write(WritePriority::Sync, {
                            let id = claimed.attempt_id.clone();
                            let upload = upload.clone();
                            move |tx| record_proton_upload(tx, &id, &upload)
                        })
                        .await?;
                    #[cfg(test)]
                    evidence.fire(SendTransition::RecordUpload, SendPhase::After);
                    if !applied {
                        return Ok(SubmitOutcome::Done);
                    }
                    uploads.push(upload);
                }
            }
            Err(message) => {
                fail_definitely(worker, backoff, evidence, &claimed, message).await?;
                return Ok(SubmitOutcome::Done);
            }
        }
        for attachment in draft_attachments {
            key_packets.insert(attachment.id, attachment.key_packets);
        }
    }
    while uploads.len() < frozen.attachments.len() {
        let index = uploads.len();
        let attachment = &frozen.attachments[index];
        // The bytes come from the frozen MIME; one that can't be read is
        // a definite failure, never a send without it (spec/05 step 3).
        let data = match attachment.extract(&bytes) {
            Ok(data) => data,
            Err(message) => {
                fail_definitely(worker, backoff, evidence, &claimed, message).await?;
                return Ok(SubmitOutcome::Done);
            }
        };
        let encrypted = match keys.encrypt_attachment(&claimed.envelope_from, &data) {
            Ok(encrypted) => encrypted,
            Err(error) => {
                let action = sender_error_action(error);
                return apply_action(
                    worker, account_id, &claimed, action, false, backoff, evidence,
                )
                .await;
            }
        };
        let upload = AttachmentUpload {
            filename: attachment.name.clone(),
            mime_type: attachment.media_type.clone(),
            encrypted,
            inline: false,
            content_id: None,
        };
        let uploaded = match client.upload_attachment(&session, &draft_id, &upload).await {
            Ok(uploaded) => uploaded,
            Err(error) => {
                record_diagnostics(worker, account_id, &error).await;
                let mapped = interpret(&error, false);
                return apply_action(
                    worker, account_id, &claimed, mapped, false, backoff, evidence,
                )
                .await;
            }
        };
        let recorded = ProtonUpload {
            index,
            id: uploaded.id.clone(),
            name: attachment.name.clone(),
            mime_type: attachment.media_type.clone(),
            size: u64::try_from(uploaded.size).unwrap_or(data.len() as u64),
        };
        #[cfg(test)]
        evidence.fire(SendTransition::RecordUpload, SendPhase::Before);
        let applied = worker
            .resources()
            .write(WritePriority::Sync, {
                let id = claimed.attempt_id.clone();
                let recorded = recorded.clone();
                move |tx| record_proton_upload(tx, &id, &recorded)
            })
            .await?;
        #[cfg(test)]
        evidence.fire(SendTransition::RecordUpload, SendPhase::After);
        if !applied {
            return Ok(SubmitOutcome::Done);
        }
        key_packets.insert(uploaded.id, upload.encrypted.key_packets);
        uploads.push(recorded);
    }

    // Step 4: the packages, then `send_requested_at` in its own write.
    let bodies = [SendBody {
        mime_type: &frozen.body.mime_type,
        data: &frozen.body.data,
    }];
    let mut send_attachments = Vec::with_capacity(uploads.len());
    for upload in &uploads {
        let Some(packets) = key_packets.get(&upload.id) else {
            fail_definitely(
                worker,
                backoff,
                evidence,
                &claimed,
                format!(
                    "the draft didn't carry attachment {}'s key packets",
                    upload.id
                ),
            )
            .await?;
            return Ok(SubmitOutcome::Done);
        };
        match keys.send_attachment(&claimed.envelope_from, &upload.id, packets) {
            Ok(send) => send_attachments.push(send),
            Err(error) => {
                let action = sender_error_action(error);
                return apply_action(
                    worker, account_id, &claimed, action, false, backoff, evidence,
                )
                .await;
            }
        }
    }
    let packages: SendPackages = match keys.build_packages(
        &claimed.envelope_from,
        &recipients,
        &bodies,
        &send_attachments,
    ) {
        Ok(packages) => packages,
        Err(error) => {
            let action = sender_error_action(error);
            return apply_action(
                worker, account_id, &claimed, action, false, backoff, evidence,
            )
            .await;
        }
    };
    let now = worker.resources().now();
    let id = claimed.attempt_id.clone();
    #[cfg(test)]
    evidence.fire(SendTransition::SendRequested, SendPhase::Before);
    let committed = worker
        .resources()
        .write(WritePriority::Sync, move |tx| {
            commit_send_requested(tx, &id, now)
        })
        .await?;
    #[cfg(test)]
    evidence.fire(SendTransition::SendRequested, SendPhase::After);
    if !committed {
        return Ok(SubmitOutcome::Done);
    }

    // Step 5: the send and its reply's interpretation.
    match client.send_packages(&session, &draft_id, &packages).await {
        Ok(_) => {
            let now = worker.resources().now();
            let id = claimed.attempt_id.clone();
            #[cfg(test)]
            evidence.fire(SendTransition::Accepted, SendPhase::Before);
            let applied = worker
                .resources()
                .write(WritePriority::Sync, move |tx| {
                    mark_send_accepted(tx, &id, now)
                })
                .await?;
            #[cfg(test)]
            evidence.fire(SendTransition::Accepted, SendPhase::After);
            if applied {
                backoff.record_success(&claimed.attempt_id);
            }
            Ok(SubmitOutcome::Done)
        }
        Err(error) => {
            record_diagnostics(worker, account_id, &error).await;
            let mapped = interpret(&error, true);
            apply_action(
                worker, account_id, &claimed, mapped, true, backoff, evidence,
            )
            .await
        }
    }
}

/// Reconciles a retry's draft read with the attempt's recorded uploads
/// and the frozen attachments (spec/05 step 3). The recorded uploads
/// occupy the draft's leading positions and must still carry their
/// ids; an unrecorded draft attachment is adopted when its reported
/// name, MIME type and size match the next pending frozen attachment.
/// Any other shape fails the attempt before the send request, naming
/// the draft.
fn reconcile_draft_uploads(
    draft_id: &str,
    draft: &[ProtonDraftAttachment],
    frozen: &[FrozenAttachment],
    recorded: &[ProtonUpload],
) -> Result<Vec<ProtonUpload>, String> {
    let mismatch =
        || format!("draft {draft_id} carries an attachment that doesn't match the frozen message");
    if recorded.len() > frozen.len() {
        return Err(format!(
            "the attempt recorded more uploads to draft {draft_id} than the frozen message carries"
        ));
    }
    if draft.len() > frozen.len() {
        return Err(format!(
            "draft {draft_id} carries more attachments than the frozen message"
        ));
    }
    if draft.len() < recorded.len() {
        return Err(format!(
            "draft {draft_id} carries fewer attachments than the attempt recorded"
        ));
    }
    for (upload, attachment) in recorded.iter().zip(draft) {
        if upload.id != attachment.id {
            return Err(mismatch());
        }
    }
    let mut recovered = Vec::new();
    for (index, attachment) in draft.iter().enumerate().skip(recorded.len()) {
        let expected = &frozen[index];
        // Name and type only: Proton receives just the encrypted data
        // packet, so its Size is not the part's decoded length. Uploads
        // run in order and stop at the first failure, so the unrecorded
        // tail is this attempt's own uploads of these frozen parts.
        if attachment.name != expected.name || attachment.mime_type != expected.media_type {
            return Err(mismatch());
        }
        // Recorded as Proton reports it, like a confirmed upload.
        let size = u64::try_from(attachment.size).unwrap_or(0);
        recovered.push(ProtonUpload {
            index,
            id: attachment.id.clone(),
            name: attachment.name.clone(),
            mime_type: attachment.mime_type.clone(),
            size,
        });
    }
    Ok(recovered)
}

/// A local failure that is never worth retrying: the attempt ends
/// `failed`, so it can never be sent in this shape (spec/05 steps 3, 7).
async fn fail_definitely(
    worker: &crate::engine::Worker,
    backoff: &mut SendBackoff,
    evidence: &SendEvidence,
    claimed: &ClaimedSend,
    message: String,
) -> Result<(), Halt> {
    let now = worker.resources().now();
    let id = claimed.attempt_id.clone();
    let failure = SendFailure {
        code: None,
        message,
        rejected: None,
    };
    fail(worker, evidence, &id, &failure, now).await?;
    backoff.record_success(&claimed.attempt_id);
    Ok(())
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
    evidence: &SendEvidence,
) -> Result<SubmitOutcome, Halt> {
    let attempt_id = claimed.attempt_id.as_str();
    match action {
        Action::Retry => {
            let now = worker.resources().now();
            let id = claimed.attempt_id.clone();
            #[cfg(test)]
            evidence.fire(SendTransition::Requeue, SendPhase::Before);
            let applied = worker
                .resources()
                .write(WritePriority::Sync, move |tx| {
                    requeue_before_request(tx, &id, now)
                })
                .await?;
            #[cfg(test)]
            evidence.fire(SendTransition::Requeue, SendPhase::After);
            if applied {
                backoff.record_failure(&claimed.attempt_id, worker.resources().now());
                return Ok(SubmitOutcome::Requeued);
            }
            Ok(SubmitOutcome::Done)
        }
        Action::Failed(failure) => {
            let now = worker.resources().now();
            let id = claimed.attempt_id.clone();
            fail(worker, evidence, &id, &failure, now).await?;
            backoff.record_success(attempt_id);
            Ok(SubmitOutcome::Done)
        }
        Action::Unknown => {
            let now = worker.resources().now();
            let id = claimed.attempt_id.clone();
            #[cfg(test)]
            evidence.fire(SendTransition::Unknown, SendPhase::Before);
            let applied = worker
                .resources()
                .write(WritePriority::Sync, move |tx| {
                    mark_outcome_unknown(tx, &id, now)
                })
                .await?;
            #[cfg(test)]
            evidence.fire(SendTransition::Unknown, SendPhase::After);
            if applied {
                backoff.record_success(attempt_id);
            }
            Ok(SubmitOutcome::Done)
        }
        Action::Attention { reason } => {
            let now = worker.resources().now();
            let id = claimed.attempt_id.clone();
            #[cfg(test)]
            let transition = if send_requested {
                SendTransition::Unknown
            } else {
                SendTransition::Requeue
            };
            #[cfg(test)]
            evidence.fire(transition, SendPhase::Before);
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
            #[cfg(test)]
            evidence.fire(transition, SendPhase::After);
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
    evidence: &SendEvidence,
    attempt_id: &str,
    failure: &SendFailure,
    now: Timestamp,
) -> Result<(), Halt> {
    let _ = evidence;
    let id = attempt_id.to_owned();
    let failure = failure.clone();
    #[cfg(test)]
    evidence.fire(SendTransition::Failed, SendPhase::Before);
    worker
        .resources()
        .write(WritePriority::Sync, move |tx| {
            mark_send_failed(tx, &id, &failure, now)
        })
        .await?;
    #[cfg(test)]
    evidence.fire(SendTransition::Failed, SendPhase::After);
    Ok(())
}

#[cfg(test)]
mod crash_tests;

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

    fn frozen(name: &str, size: Option<u64>) -> FrozenAttachment {
        FrozenAttachment {
            name: name.to_owned(),
            media_type: "text/plain".to_owned(),
            size,
            part_id: "2".to_owned(),
        }
    }

    fn on_draft(id: &str, name: &str, size: i64) -> ProtonDraftAttachment {
        ProtonDraftAttachment {
            id: id.to_owned(),
            name: name.to_owned(),
            size,
            mime_type: "text/plain".to_owned(),
            key_packets: Vec::new(),
        }
    }

    #[test]
    fn an_unrecorded_upload_is_adopted_whatever_size_proton_reports() {
        // Proton receives only the encrypted data packet, so its Size
        // is not the frozen part's decoded length (go-proton-api's
        // server leaves it 0): adoption can't depend on it.
        for reported in [0, 57] {
            let adopted = reconcile_draft_uploads(
                "draft-1",
                &[on_draft("att-1", "notes.txt", reported)],
                &[frozen("notes.txt", Some(12))],
                &[],
            )
            .expect("the lost upload is adopted");
            assert_eq!(adopted.len(), 1);
            assert_eq!(adopted[0].id, "att-1");
            assert_eq!(adopted[0].index, 0);
        }
        let adopted = reconcile_draft_uploads(
            "draft-1",
            &[on_draft("att-1", "notes.txt", 0)],
            &[frozen("notes.txt", None)],
            &[],
        )
        .expect("a frozen part without a size is adopted too");
        assert_eq!(adopted.len(), 1);
    }

    #[test]
    fn an_unrecorded_upload_of_another_name_is_a_mismatch() {
        let refused = reconcile_draft_uploads(
            "draft-1",
            &[on_draft("att-1", "other.txt", 12)],
            &[frozen("notes.txt", Some(12))],
            &[],
        );
        assert!(refused.is_err());
    }
}
