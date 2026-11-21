//! Proton flag intents on the account worker (spec/04-sync-proton
//! "Running an intent"; plan M3.1c). After each poll the worker claims
//! the account's runnable intents one at a time and, for each, checks
//! the generation, reads the message by id (the live target check),
//! makes the call and confirms it with a second read.
//!
//! Every transition is its own transaction, so a crash leaves either a
//! `pending` intent or an `in_flight` one, which the next attempt
//! returns to `pending`. Its live check then reconciles: a change
//! Proton already holds finishes the intent without a call, so no
//! effect is applied twice and none is marked `done` unconfirmed.

use sb_resources::{
    ClaimedIntent, FailureOutcome, FlagChange, IntentFlag, MessageMetadata, MessageMutation,
    MutationError, ProtonError, QuarantineReason, STARRED_LABEL_ID, StoreError, Transaction,
    WaitReason, WritePriority, claim_next_intent, clear_account_intents_waiting,
    finish_intent_done, park_intent, quarantine_if_reconfigured, quarantine_intent,
    record_intent_failure, recover_in_flight_intents, return_intent_to_pending,
};

use super::{Attempt, FetchError, Halt};

/// At most this many intents run between two polls, so a long queue
/// doesn't hold up events, sends and bodies.
const MAX_INTENTS_PER_PASS: usize = 25;

/// The waits a pass clears before it claims: the poll that just
/// succeeded proves a connection and a session, and a throttled intent
/// keeps its `next_attempt_at` (spec/04-sync-proton "Waiting").
const CLEARED_WAITS: [WaitReason; 3] =
    [WaitReason::Offline, WaitReason::Auth, WaitReason::Throttled];

/// How one intent's run ended for the pass.
enum Ran {
    /// The intent reached a state of its own (done, a counted failure,
    /// parked or quarantined): claim the next.
    Next,
    /// Proton can't be reached or the session is gone: the intent
    /// waits without an attempt counted, and the pass halts.
    Halt(Halt),
}

/// What the by-id read found (spec/04-sync-proton "Before the call").
enum Read {
    /// The message, with its flags as Proton holds them.
    Found(Box<MessageMetadata>),
    /// Proton has no message with that id.
    Missing,
    /// The answer named another message.
    Unexpected,
    /// The read failed; the halt says why.
    Failed(Halt),
}

/// A crash-cut point of an intent's run (spec/11): after the claim
/// commits, around the call, and around the write that ends the run.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(crate) enum IntentCut {
    Claimed,
    BeforeCall,
    AfterCall,
    BeforeFinish,
    AfterFinish,
}

/// The crash-cut suite's hook. Its child is its own process and the
/// only test in it, so a process-wide hook is enough; no in-process
/// test sets one.
#[cfg(test)]
pub(crate) type IntentCutHook = std::sync::Arc<dyn Fn(IntentCut) + Send + Sync>;

#[cfg(test)]
static CUT_HOOK: std::sync::Mutex<Option<IntentCutHook>> = std::sync::Mutex::new(None);

#[cfg(test)]
pub(crate) fn set_cut_hook(hook: Option<IntentCutHook>) {
    *CUT_HOOK.lock().unwrap_or_else(|e| e.into_inner()) = hook;
}

/// Reach a cut point: the hook runs in test builds, nothing otherwise.
fn cut(point: IntentCut) {
    #[cfg(test)]
    {
        let hook = CUT_HOOK.lock().unwrap_or_else(|e| e.into_inner()).clone();
        if let Some(hook) = hook {
            hook(point);
        }
    }
    #[cfg(not(test))]
    let _ = point;
}

impl Attempt<'_> {
    /// Startup recovery for this attempt (spec/04-sync-proton
    /// "Claim"): only this worker runs the account's intents, so an
    /// `in_flight` one is left from a crash or a halted attempt and
    /// returns to `pending`.
    pub(super) async fn recover_intents(&self) -> Result<(), Halt> {
        let id = self.account_id.to_owned();
        let now = self.worker.resources().now().as_millis();
        let moved = self
            .quiet_write(move |tx| recover_in_flight_intents(tx, &id, now))
            .await?;
        if moved > 0 {
            self.worker.statuses().rows_changed();
        }
        Ok(())
    }

    /// A write that doesn't wake the health subscription: the pass's
    /// bookkeeping runs after every poll, and only a run that moves an
    /// intent changes what the Activity window shows.
    async fn quiet_write<T, F>(&self, closure: F) -> Result<T, Halt>
    where
        T: Send + 'static,
        F: FnOnce(&Transaction) -> Result<T, StoreError> + Send + 'static,
    {
        Ok(self
            .worker
            .resources()
            .write(WritePriority::Sync, closure)
            .await?)
    }

    /// Run the account's due intents after a poll, one at a time, at
    /// most [`MAX_INTENTS_PER_PASS`] of them.
    pub(super) async fn run_intents(&self) -> Result<(), Halt> {
        let id = self.account_id.to_owned();
        let now = self.worker.resources().now().as_millis();
        self.quiet_write(move |tx| {
            for reason in CLEARED_WAITS {
                clear_account_intents_waiting(tx, &id, reason, now)?;
            }
            Ok(())
        })
        .await?;
        for _ in 0..MAX_INTENTS_PER_PASS {
            if *self.stop.borrow() {
                return Err(Halt::Stopped);
            }
            let id = self.account_id.to_owned();
            let now = self.worker.resources().now().as_millis();
            let Some(claimed) = self
                .quiet_write(move |tx| claim_next_intent(tx, &id, now))
                .await?
            else {
                return Ok(());
            };
            cut(IntentCut::Claimed);
            if let Ran::Halt(halt) = self.run_intent(&claimed).await? {
                return Err(halt);
            }
        }
        Ok(())
    }

    /// One claimed intent, from the generation check to its end state.
    async fn run_intent(&self, claimed: &ClaimedIntent) -> Result<Ran, Halt> {
        let check = claimed.clone();
        let now = self.worker.resources().now().as_millis();
        if self
            .write(move |tx| quarantine_if_reconfigured(tx, &check, now))
            .await?
        {
            return Ok(Ran::Next);
        }

        // The live target check, which is also the reconcile before a
        // retry: a change Proton already holds is never sent again.
        match self.read(&claimed.provider_key).await {
            Read::Found(metadata) if holds(&metadata, claimed.change) => {
                return self.finish(claimed, End::Done).await;
            }
            Read::Found(_) => {}
            Read::Missing => {
                return self
                    .finish(claimed, End::Quarantine(QuarantineReason::AmbiguousMatch))
                    .await;
            }
            Read::Unexpected => {
                return self.finish(claimed, End::Failure(UNEXPECTED)).await;
            }
            Read::Failed(halt) => return self.wait(claimed, halt).await,
        }

        cut(IntentCut::BeforeCall);
        let called = self.mutate(claimed).await;
        cut(IntentCut::AfterCall);
        match called {
            Ok(()) => {}
            Err(Called::Refused { status, message }) if is_terminal_refusal(status) => {
                return self.finish(claimed, End::Park(message)).await;
            }
            Err(Called::Refused { message, .. } | Called::Failed(message)) => {
                return self.finish(claimed, End::Failure(&message)).await;
            }
            Err(Called::Unsendable(message)) => {
                return self.finish(claimed, End::Park(message)).await;
            }
            Err(Called::Wait(halt, reason)) => {
                return self.wait_for(claimed, halt, reason).await;
            }
        }

        // Confirm with a second read: only the requested state makes
        // the intent `done`.
        match self.read(&claimed.provider_key).await {
            Read::Found(metadata) if holds(&metadata, claimed.change) => {
                self.finish(claimed, End::Done).await
            }
            Read::Found(_) => self.finish(claimed, End::Failure(NOT_CONFIRMED)).await,
            Read::Missing => self.finish(claimed, End::Failure(GONE_AFTER_CALL)).await,
            Read::Unexpected => self.finish(claimed, End::Failure(UNEXPECTED)).await,
            // The change may hold; the next attempt's live check sees.
            Read::Failed(halt) => self.wait(claimed, halt).await,
        }
    }

    /// `GET /mail/v4/messages?ID=` through the attempt's session, with
    /// one refresh after a 401.
    async fn read(&self, provider_key: &str) -> Read {
        let key = provider_key.to_owned();
        match self
            .call_classified(|client, session| {
                let key = key.clone();
                async move { client.message_metadata(&session, &key).await }
            })
            .await
        {
            Ok(Some(metadata)) => Read::Found(Box::new(metadata)),
            Ok(None) => Read::Missing,
            Err(FetchError::UnexpectedMessage) => Read::Unexpected,
            Err(FetchError::Ended) => Read::Failed(Halt::Ended),
            Err(FetchError::AttachmentTooLarge | FetchError::Retry) => Read::Failed(Halt::Retry),
        }
    }

    /// The call, sent once (the transport never replays it), with one
    /// refresh after a 401: Proton refused the request before applying
    /// it, so sending it again is safe.
    async fn mutate(&self, claimed: &ClaimedIntent) -> Result<(), Called> {
        let mut refreshed = false;
        loop {
            let Ok(session) = self.session().await else {
                return Err(Called::Wait(Halt::Ended, Some(WaitReason::Auth)));
            };
            let answer = self
                .client
                .mutate_message(&session, &claimed.provider_key, mutation(claimed.change))
                .await;
            return match answer {
                Ok(()) => Ok(()),
                Err(MutationError::PreRequest(ProtonError::Unauthorized)) if !refreshed => {
                    self.refresh(&session).await;
                    refreshed = true;
                    continue;
                }
                Err(MutationError::PreRequest(_)) => Err(Called::Wait(Halt::Retry, None)),
                Err(MutationError::Refused {
                    status, message, ..
                }) => Err(Called::Refused { status, message }),
                Err(error @ MutationError::OutcomeUnknown { .. }) => {
                    Err(Called::Failed(error.to_string()))
                }
                Err(MutationError::NeverEstablished { .. }) => {
                    Err(Called::Wait(Halt::Retry, Some(WaitReason::Offline)))
                }
                Err(MutationError::Throttled { .. }) => {
                    Err(Called::Wait(Halt::Retry, Some(WaitReason::Throttled)))
                }
                Err(error @ MutationError::InvalidInput { .. }) => {
                    Err(Called::Unsendable(error.to_string()))
                }
            };
        }
    }

    /// Return the intent to `pending` without counting an attempt, the
    /// wait named after the halt, and halt the pass.
    async fn wait(&self, claimed: &ClaimedIntent, halt: Halt) -> Result<Ran, Halt> {
        let reason = matches!(halt, Halt::Ended).then_some(WaitReason::Auth);
        self.wait_for(claimed, halt, reason).await
    }

    async fn wait_for(
        &self,
        claimed: &ClaimedIntent,
        halt: Halt,
        reason: Option<WaitReason>,
    ) -> Result<Ran, Halt> {
        let id = claimed.intent_id.clone();
        let now = self.worker.resources().now().as_millis();
        cut(IntentCut::BeforeFinish);
        self.write(move |tx| return_intent_to_pending(tx, &id, reason, None, now))
            .await?;
        cut(IntentCut::AfterFinish);
        Ok(Ran::Halt(halt))
    }

    /// The write that ends the run (spec/04-sync-proton "Failures").
    async fn finish(&self, claimed: &ClaimedIntent, end: End<'_>) -> Result<Ran, Halt> {
        let id = claimed.intent_id.clone();
        let now = self.worker.resources().now().as_millis();
        cut(IntentCut::BeforeFinish);
        match end {
            End::Done => {
                self.write(move |tx| finish_intent_done(tx, &id, now))
                    .await?;
            }
            End::Failure(error) => {
                let error = error.to_owned();
                let _: FailureOutcome = self
                    .write(move |tx| record_intent_failure(tx, &id, &error, now))
                    .await?;
            }
            End::Park(error) => {
                self.write(move |tx| park_intent(tx, &id, &error, now))
                    .await?;
            }
            End::Quarantine(reason) => {
                self.write(move |tx| quarantine_intent(tx, &id, reason, now))
                    .await?;
            }
        }
        cut(IntentCut::AfterFinish);
        Ok(Ran::Next)
    }
}

/// How a run ends, as [`Attempt::finish`] writes it.
enum End<'a> {
    Done,
    Failure(&'a str),
    Park(String),
    Quarantine(QuarantineReason),
}

/// The call's failure, classified for the run (spec/04-sync-proton
/// "The call").
enum Called {
    /// Proton answered and refused, with its HTTP status.
    Refused { status: u16, message: String },
    /// The change may have been applied: a counted failure, reconciled
    /// by the next attempt's live check.
    Failed(String),
    /// The request can never be sent.
    Unsendable(String),
    /// Nothing was applied: wait without an attempt counted.
    Wait(Halt, Option<WaitReason>),
}

const UNEXPECTED: &str = "the by-id read answered another message";
const NOT_CONFIRMED: &str = "the confirming read doesn't show the change";
const GONE_AFTER_CALL: &str = "the message was gone at the confirming read";

/// A 4xx other than 429 parks the intent at once (spec/04-sync-proton
/// "Failures"); a refusing code in a 2xx answer counts as a failure.
/// The transport reports 429 as `Throttled`, never as a refusal.
fn is_terminal_refusal(status: u16) -> bool {
    (400..500).contains(&status) && status != 429
}

/// The call for a flag change (spec/04-sync-proton's mutation matrix).
fn mutation(change: FlagChange) -> MessageMutation {
    match (change.flag, change.set) {
        (IntentFlag::Seen, true) => MessageMutation::Read,
        (IntentFlag::Seen, false) => MessageMutation::Unread,
        (IntentFlag::Flagged, true) => MessageMutation::Label(STARRED_LABEL_ID.to_owned()),
        (IntentFlag::Flagged, false) => MessageMutation::Unlabel(STARRED_LABEL_ID.to_owned()),
    }
}

/// Whether Proton already holds the requested state.
fn holds(metadata: &MessageMetadata, change: FlagChange) -> bool {
    match change.flag {
        IntentFlag::Seen => metadata.unread != change.set,
        IntentFlag::Flagged => {
            metadata
                .label_ids
                .iter()
                .any(|label| label == STARRED_LABEL_ID)
                == change.set
        }
    }
}

#[cfg(test)]
mod conflict_tests;
#[cfg(test)]
mod crash_tests;
#[cfg(test)]
mod tests;
