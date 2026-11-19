//! Proton accounts' sync workers (spec/04-sync-proton "Sync loop"
//! steps 1 and 2). An account with a session in memory gets one
//! worker: it reads the event cursor before listing, reconciles the
//! labels through the `proton_api` policy, commits the metadata listing
//! page by page with its checkpoint, and finishes with Proton's
//! per-label totals. A crash or restart resumes from the last committed
//! page. Then it follows the events from that cursor, each response
//! applied with its new cursor, until a refresh asks for the bootstrap
//! again.
//!
//! The worker takes the account's session from the engine's map before
//! every request, so signing in again hands it the new session and a
//! disconnect, which removes the session, ends it.

use crate::account_health::{LiveCondition, LiveStatus};
use crate::account_sync::AccountJobs;
use crate::engine::{Engine, Spawn, Worker, validate_idempotency_key};
use crate::policy::proton_api;
use crate::proton::{Sessions, mark_session_refused, restore_proton_account};
use sb_resources::proton_bodies::{
    BodyFailure, MAX_PENDING_BODIES, PendingBody, acquire_proton_body, bump_key_generation,
    pending_proton_bodies, record_body_failure,
};
use sb_resources::proton_sync::{
    BreakerResolution, EventChunk, ListedPage, ProtonChange, ProtonListingState, RereadMessage,
    anchor, apply_event_chunk, apply_reread_message, begin_listing, checkpoint, commit_listed_page,
    finish_listing, listed_message, reconcile_proton_labels, record_refusal,
    refresh_proton_identities, resolve_breaker_event, withhold_unlisted,
};
use sb_resources::{
    APP_IDENTITY_REFUSED_REASON, AccountState, ApiCode, BackoffKind, EventAction, EventBatch,
    EventId, HUMAN_VERIFICATION_REASON, KeyStore, ListingAnchor, PROTON_MAX_PAGE_SIZE,
    ProtonClient, ProtonError, Receipt, Secret, Session, StoreError, StoredAccount, Transaction,
    WritePriority, accounts, pause_for_reason, proton_account, proton_identities, run_once,
};

use std::collections::{BTreeSet, HashSet};
use std::future::Future;
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;
use tokio::sync::watch;

/// Messages per listed page: the most the transport asks for. A page
/// shorter than this ends the listing, so it must not exceed the
/// transport's limit.
const PAGE_SIZE: u32 = PROTON_MAX_PAGE_SIZE;
/// `resolve_breaker_event`'s name in `command_receipts` (spec/07).
const RESOLVE_BREAKER_EVENT: &str = "resolve_breaker_event";
/// The first wait after a failed attempt; it doubles up to
/// [`MAX_RETRY_DELAY`] (spec/04 "Backoff": 1 s to 5 min).
const FIRST_RETRY_DELAY: Duration = Duration::from_secs(1);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(5 * 60);
/// How long the worker waits between event polls while the app is
/// active (spec/04 "Sync loop" step 2).
const ACTIVE_POLL_INTERVAL: Duration = Duration::from_secs(30);
/// How long it waits while the app is inactive, the cadence it starts
/// on (spec/04 "Sync loop" step 2; `set_app_active`, spec/07).
const INACTIVE_POLL_INTERVAL: Duration = Duration::from_secs(2 * 60);
/// Message changes per write: a listed page's worth, which fits the
/// store's write budget. A longer response is applied in chunks, its
/// cursor moving with the last; each change carries the message's full
/// state, so replaying a chunk after a crash is harmless.
const CHANGES_PER_WRITE: usize = PAGE_SIZE as usize;
/// Bodies per queue round: small enough that an event poll follows
/// promptly (spec/04 "Sync loop" step 3: events stay timely).
const BODY_BATCH: usize = 20;

/// The provider keys the user opened, per Proton account, most recent
/// first and bounded (spec/04 "Acquire bodies": opened first, then
/// newest). In memory only.
#[derive(Default)]
pub(crate) struct ProtonOpened {
    state: Mutex<std::collections::HashMap<String, Vec<String>>>,
}

impl ProtonOpened {
    /// Note an open, making `provider_key` the account's most recent.
    pub(crate) fn note(&self, account_id: &str, provider_key: String) {
        let mut state = self.state.lock().expect("the opened-body lock poisoned");
        let opened = state.entry(account_id.to_owned()).or_default();
        opened.retain(|key| key != &provider_key);
        opened.insert(0, provider_key);
        opened.truncate(MAX_PENDING_BODIES);
    }

    /// The account's opened keys, most recent first.
    pub(crate) fn keys(&self, account_id: &str) -> Vec<String> {
        self.state
            .lock()
            .expect("the opened-body lock poisoned")
            .get(account_id)
            .cloned()
            .unwrap_or_default()
    }
}

/// Why an attempt stopped short of finishing the listing.
#[derive(Debug)]
enum Halt {
    /// The worker was told to stop (shutdown or a restore's quiesce).
    Stopped,
    /// Proton asked for the listing again (spec/04 "Refresh").
    Refresh,
    /// The account has no session any more: disconnected, or its
    /// refresh was refused and it needs attention.
    Ended,
    /// A failure worth another attempt from the checkpoint: Proton
    /// unavailable, a stale or non-advancing listing, an undecodable
    /// response, or a store conflict with a concurrent change.
    Retry,
}

impl From<StoreError> for Halt {
    fn from(_: StoreError) -> Self {
        Self::Retry
    }
}

/// Why a body's fetch stopped (spec/04 "Sync loop" step 3).
enum FetchError {
    /// The response named another message than the one requested.
    UnexpectedMessage,
    /// An attachment data packet exceeded the transport's bound.
    AttachmentTooLarge,
    /// The account has no session any more.
    Ended,
    /// Proton or the network refused; the attempt retries.
    Retry,
}

impl From<Halt> for FetchError {
    /// A refusal's halt: an account-state refusal ends the worker,
    /// every other one retries.
    fn from(halt: Halt) -> Self {
        match halt {
            Halt::Ended => Self::Ended,
            _ => Self::Retry,
        }
    }
}

/// The identities' address ids and the failed key ids, which a key
/// change is a change to (spec/02 `key_generation`).
fn key_state(keys: &dyn KeyStore) -> (BTreeSet<String>, BTreeSet<String>) {
    (
        keys.identities()
            .iter()
            .map(|identity| identity.id.clone())
            .collect(),
        keys.failed().iter().cloned().collect(),
    )
}

impl Engine {
    /// Start a worker for every Proton account with a session: after
    /// open's restore, or after a backup restore resumes the workers.
    pub(crate) fn schedule_proton_sync(&self) {
        let jobs = Arc::clone(self.account_jobs());
        let sessions = Arc::clone(&self.proton_sessions);
        let opened = Arc::clone(&self.proton_opened);
        let _ = self.spawn_worker(move |worker| async move {
            let accounts = worker.blocking(|resources| resources.read(accounts)).await;
            if let Ok(Ok(accounts)) = accounts {
                start_all(&worker, &jobs, &sessions, &opened, &accounts).await;
            }
        });
    }
}

impl Engine {
    /// Test seam: the account's coverage (spec/04-sync-proton
    /// "Coverage"); the service exposes it with the health window.
    #[cfg(any(test, feature = "test-seams"))]
    #[doc(hidden)]
    pub async fn proton_coverage(
        &self,
        account_id: &str,
    ) -> Result<sb_resources::proton_sync::ProtonCoverage, crate::EngineError> {
        let id = account_id.to_owned();
        self.read_through(move |resources| {
            resources.read(|tx| sb_resources::proton_sync::proton_coverage(tx, &id))
        })
        .await
    }

    /// Test seam: make every account worker poll its events now rather
    /// than at its next poll (spec/04 polls after a local mutation too,
    /// M3).
    #[cfg(any(test, feature = "test-seams"))]
    #[doc(hidden)]
    pub fn poll_proton_events(&self) {
        self.account_jobs().poll_now();
    }

    /// Test seam: wake a Proton poll wait so it re-reads the injected
    /// clock (spec/11), the way a test advances the maintenance
    /// scheduler's clock. It changes no poll's schedule itself.
    #[cfg(any(test, feature = "test-seams"))]
    #[doc(hidden)]
    pub fn wake_proton_poll(&self) {
        self.account_jobs().poll_wait().notify_one();
    }

    /// `set_app_active(active)` (spec/07 "Account management"): a
    /// lifecycle signal, not a command. No key, no receipt, nothing
    /// stored. A Proton worker waits 30 s between event polls while
    /// the app is active and 2 min otherwise (spec/04 "Sync loop"
    /// step 2); it starts inactive, and the wait recomputes its
    /// deadline when the signal changes, so a switch takes effect
    /// without waiting out a running 2-min wait.
    pub fn set_app_active(&self, active: bool) {
        self.account_jobs().set_app_active(active);
    }

    /// `resolve_breaker_event(key, id, resolution)` (spec/07 "Safety",
    /// spec/04-sync-proton "The breaker"): Accept applies a Proton
    /// event's withheld deletes and Keep leaves them withheld. Either
    /// lifts the account's pause, classifying the deletes it held. A
    /// repeated key answers its stored receipt and resolves nothing
    /// again; an unknown event is `NotFound`.
    pub async fn resolve_breaker_event(
        &self,
        idempotency_key: &str,
        event_id: &str,
        resolution: BreakerResolution,
    ) -> Result<Receipt, crate::EngineError> {
        validate_idempotency_key(idempotency_key)?;
        let (key, id) = (idempotency_key.to_owned(), event_id.to_owned());
        let receipt = self
            .command(move |worker| async move {
                let now = worker.resources().now();
                worker
                    .resources()
                    .write(WritePriority::User, move |tx| {
                        run_once(tx, &key, RESOLVE_BREAKER_EVENT, now, |tx| {
                            resolve_breaker_event(tx, &id, resolution, now)?;
                            Ok("{}".to_owned())
                        })
                    })
                    .await
                    .map_err(crate::EngineError::from)
            })
            .await?;
        // The event was accepted or kept: the health row changed
        // (plan M2.7b2).
        self.worker_statuses().rows_changed();
        Ok(receipt)
    }
}

/// Start a worker for every account whose session is in memory, except
/// a paused or disconnected one: launch starts neither (spec/04
/// "Account states and health", 07 `pause_account`).
pub(crate) async fn start_all(
    spawner: &impl Spawn,
    jobs: &Arc<AccountJobs>,
    sessions: &Arc<Sessions>,
    opened: &Arc<ProtonOpened>,
    accounts: &[StoredAccount],
) {
    let skipping: HashSet<&str> = accounts
        .iter()
        .filter(|account| {
            matches!(
                account.state,
                AccountState::Paused | AccountState::Disconnected
            )
        })
        .map(|account| account.account_id.as_str())
        .collect();
    let ids: Vec<String> = sessions.lock().await.keys().cloned().collect();
    for account_id in ids {
        if skipping.contains(account_id.as_str()) {
            continue;
        }
        start(spawner, jobs, sessions, opened, account_id);
    }
}

/// Start the account's worker unless one runs.
pub(crate) fn start(
    spawner: &impl Spawn,
    jobs: &Arc<AccountJobs>,
    sessions: &Arc<Sessions>,
    opened: &Arc<ProtonOpened>,
    account_id: String,
) {
    let Some(stop) = jobs.claim(&account_id) else {
        return;
    };
    let owner = Arc::clone(jobs);
    let sessions = Arc::clone(sessions);
    let opened = Arc::clone(opened);
    let id = account_id.clone();
    let signals = PollSignals {
        poll: jobs.poll_signal(),
        active: jobs.app_active(),
        wake: jobs.poll_wait(),
    };
    let spawned = spawner.spawn_worker(move |worker| async move {
        run(&worker, &sessions, &opened, &id, stop, signals).await;
        owner.release(&id);
    });
    if spawned.is_err() {
        jobs.release(&account_id);
    }
}

/// What a worker's wait between event polls listens to: the poll hint,
/// the app-active signal for the cadence (spec/04 "Sync loop" step 2)
/// and the test wake that re-reads the clock.
struct PollSignals {
    poll: watch::Receiver<()>,
    active: watch::Receiver<bool>,
    wake: Arc<tokio::sync::Notify>,
}

async fn run(
    worker: &Worker,
    sessions: &Sessions,
    opened: &ProtonOpened,
    account_id: &str,
    stop: watch::Receiver<bool>,
    signals: PollSignals,
) {
    run_loop(worker, sessions, opened, account_id, stop, signals).await;
    // The worker is done (paused, disconnected, a refusal that ended
    // it, or shutdown): the account's live status goes with it and its
    // condition comes from the stored state.
    worker.statuses().clear(account_id);
}

async fn run_loop(
    worker: &Worker,
    sessions: &Sessions,
    opened: &ProtonOpened,
    account_id: &str,
    stop: watch::Receiver<bool>,
    signals: PollSignals,
) {
    let PollSignals { poll, active, wake } = signals;
    let Ok(client) = worker.resources().proton_client() else {
        return;
    };
    // Show the account's condition and next attempt while the client
    // waits out a retry (spec/07); its retry behaviour is unchanged.
    let statuses = Arc::clone(worker.statuses());
    let observer_statuses = Arc::clone(&statuses);
    let observer_id = account_id.to_owned();
    let observer_worker = worker.clone();
    client.set_backoff_observer(Some(Arc::new(move |kind, delay| {
        let at_ms = observer_worker.resources().now().as_millis() + delay.as_millis() as i64;
        let condition = match kind {
            BackoffKind::Network => LiveCondition::Offline {
                next_retry_ms: at_ms,
            },
            BackoffKind::ServerError => LiveCondition::NotAnswering {
                next_retry_ms: at_ms,
            },
            BackoffKind::RateLimit => LiveCondition::RateLimited { until_ms: at_ms },
        };
        observer_statuses.set(
            &observer_id,
            LiveStatus {
                last_sync_ms: observer_statuses
                    .get(&observer_id)
                    .and_then(|status| status.last_sync_ms),
                condition,
            },
        );
    })));
    let mut delay = FIRST_RETRY_DELAY;
    // The messages this run refused as another message's response
    // (spec/04 step 3); a restart re-reads them once.
    let mut skipped = HashSet::new();
    loop {
        let mut attempt = Attempt {
            worker,
            sessions,
            opened,
            client: &client,
            account_id,
            stop: &stop,
            poll: poll.clone(),
            active: active.clone(),
            wake: Arc::clone(&wake),
            advanced: AtomicBool::new(false),
            skipped: &mut skipped,
        };
        // A stop doesn't wait for a request in flight: a write already
        // submitted commits or not as a whole, and the next attempt
        // starts from what the store says.
        let mut wait = stop.clone();
        let outcome = tokio::select! {
            outcome = attempt.sync() => outcome,
            () = stopped(&mut wait) => return,
        };
        match outcome {
            Halt::Stopped | Halt::Ended => return,
            Halt::Retry => {
                let now_ms = worker.resources().now().as_millis();
                record_retry(&statuses, account_id, client.last_backoff(), now_ms, delay);
            }
            Halt::Refresh => {}
        }
        // Events applied since the last failure: this one is new.
        if attempt.advanced.load(Ordering::Relaxed) {
            delay = FIRST_RETRY_DELAY;
        }
        tokio::select! {
            () = tokio::time::sleep(delay) => {}
            () = stopped(&mut wait) => return,
        }
        delay = (delay * 2).min(MAX_RETRY_DELAY);
    }
}

async fn stopped(stop: &mut watch::Receiver<bool>) {
    while !*stop.borrow_and_update() {
        if stop.changed().await.is_err() {
            return;
        }
    }
}

/// One bootstrap attempt's context.
struct Attempt<'a> {
    worker: &'a Worker,
    sessions: &'a Sessions,
    opened: &'a ProtonOpened,
    client: &'a ProtonClient,
    account_id: &'a str,
    stop: &'a watch::Receiver<bool>,
    poll: watch::Receiver<()>,
    /// Whether the app is active, for the poll cadence (spec/04).
    active: watch::Receiver<bool>,
    /// Woken to re-read the clock between polls (the test seam).
    wake: Arc<tokio::sync::Notify>,
    /// Set once an events response is applied.
    advanced: AtomicBool,
    /// Provider keys this run refused as another message's response.
    skipped: &'a mut HashSet<String>,
}

impl Attempt<'_> {
    /// Bootstrap, then follow the events, until something halts it.
    async fn sync(&mut self) -> Halt {
        loop {
            let halt = match self.bootstrap().await {
                Ok(()) => {
                    self.record_sync();
                    self.follow().await
                }
                Err(halt) => halt,
            };
            if !matches!(halt, Halt::Refresh) {
                return halt;
            }
        }
    }

    /// One successful pass over Proton: the last sync time moves and
    /// any retry condition clears (plan M2.7b2).
    fn record_sync(&self) {
        let now_ms = self.worker.resources().now().as_millis();
        self.worker.statuses().set(
            self.account_id,
            LiveStatus {
                last_sync_ms: Some(now_ms),
                condition: LiveCondition::Synced,
            },
        );
    }

    /// spec/04-sync-proton "Sync loop" step 1, from where the store
    /// says the listing stands.
    async fn bootstrap(&self) -> Result<(), Halt> {
        let id = self.account_id.to_owned();
        let account = self
            .worker
            .blocking(move |resources| resources.read(|tx| proton_account(tx, &id)))
            .await
            .map_err(|_| Halt::Stopped)??
            .ok_or(Halt::Ended)?;
        if account.listing_state == ProtonListingState::Complete {
            return Ok(());
        }
        let mut revision = account.revision;
        let mut after = None;
        match (account.listing_state, account.pending_cursor) {
            (ProtonListingState::Listing, Some(_)) => {
                after = account.listing_checkpoint.as_deref().and_then(anchor);
            }
            _ => {
                // 1.1: the cursor comes first, so a change made while
                // listing is applied from it afterwards.
                let cursor = self
                    .call(|client, session| async move { client.latest_event(&session).await })
                    .await?;
                let id = self.account_id.to_owned();
                revision = self
                    .write(move |tx| {
                        begin_listing(tx, &id, cursor.as_str(), revision)
                            .map(|account| account.revision)
                    })
                    .await?;
            }
        }

        // 1.2: labels.
        let labels = self
            .call(|client, session| async move { client.labels(&session).await })
            .await?;
        let observed = proton_api::observed_labels(&labels);
        let id = self.account_id.to_owned();
        self.write(move |tx| reconcile_proton_labels(tx, &id, &observed).map(drop))
            .await?;

        // 1.3: pages, newest first, each with its checkpoint.
        loop {
            if *self.stop.borrow() {
                return Err(Halt::Stopped);
            }
            let page = self
                .call(|client, session| {
                    let after = after.clone();
                    async move {
                        client
                            .message_page(&session, after.as_ref(), PAGE_SIZE)
                            .await
                    }
                })
                .await?;
            // Proton asks for the listing to be retried (research
            // `mail-sync.md` §3); nothing from the page is applied.
            if page.stale {
                return Err(Halt::Retry);
            }
            let Some(last) = page.messages.last() else {
                break;
            };
            let next = ListingAnchor::of(last);
            // The transport drops the anchor and anything newer, so a
            // repeated anchor is a listing that can't advance.
            if after.as_ref() == Some(&next) {
                return Err(Halt::Retry);
            }
            let messages: Vec<_> = page.messages.iter().map(listed_message).collect();
            let id = self.account_id.to_owned();
            let reached = checkpoint(&next);
            let now = self.worker.resources().now();
            revision = self
                .write(move |tx| {
                    commit_listed_page(
                        tx,
                        &ListedPage {
                            account_id: &id,
                            messages: &messages,
                            checkpoint: &reached,
                            expected_revision: revision,
                        },
                        now,
                    )?;
                    // The checkpoint moved the revision on.
                    proton_account(tx, &id)?
                        .map(|account| account.revision)
                        .ok_or_else(|| StoreError::Conflict {
                            detail: "the Proton account is gone".to_owned(),
                        })
                })
                .await?;
            let short = page.messages.len() < PAGE_SIZE as usize;
            after = Some(next);
            if short {
                break;
            }
        }

        // "Refresh" step 3: what the listing missed is withheld, a
        // budget-sized write at a time.
        loop {
            let id = self.account_id.to_owned();
            if self
                .write(move |tx| withhold_unlisted(tx, &id, revision))
                .await?
                == 0
            {
                break;
            }
        }

        // 1.4: totals, the breaker's view of what the listing missed,
        // and the pending cursor becomes the event cursor.
        let counts = self
            .call(|client, session| async move { client.message_counts(&session).await })
            .await?;
        let totals: Vec<(String, u64)> = counts
            .into_iter()
            .map(|count| (count.label_id, u64::from(count.total)))
            .collect();
        let id = self.account_id.to_owned();
        let now = self.worker.resources().now();
        self.write(move |tx| finish_listing(tx, &id, &totals, revision, now).map(drop))
            .await
    }

    /// spec/04-sync-proton "Sync loop" steps 2 and 3: poll the
    /// cursor, apply each response with its new cursor and fetch again
    /// while `More`, then drain a small batch of bodies before the next
    /// wait. Bodies between event polls keep an opened message prompt
    /// and events timely; the events run first, so a message the queue
    /// is about to fetch has its delete applied already.
    async fn follow(&mut self) -> Halt {
        let mut poll = self.poll.clone();
        poll.mark_unchanged();
        let mut stop = self.stop.clone();
        let mut active = self.active.clone();
        loop {
            match self.poll_once().await {
                Ok(true) => continue,
                Ok(false) => {}
                Err(halt) => return halt,
            }
            if let Err(halt) = self.drain_bodies().await {
                return halt;
            }
            let start = self.worker.resources().now();
            let now = || self.worker.resources().now();
            if let Some(halt) =
                wait_for_next_poll(now, &mut stop, &mut poll, &mut active, &self.wake, start).await
            {
                return halt;
            }
        }
    }

    /// spec/04 "Acquire bodies": one small batch from the store's
    /// queue, opened keys first, then newest. A content failure is
    /// recorded and the queue moves on; a transport failure halts the
    /// attempt for a retry.
    async fn drain_bodies(&mut self) -> Result<(), Halt> {
        let id = self.account_id.to_owned();
        let account = self
            .worker
            .blocking({
                let id = id.clone();
                move |resources| resources.read(|tx| proton_account(tx, &id))
            })
            .await
            .map_err(|_| Halt::Stopped)??
            .ok_or(Halt::Ended)?;
        if account.listing_state != ProtonListingState::Complete {
            return Ok(());
        }
        let opened = self.opened.keys(&id);
        // Skipped messages stay in the store's queue, so ask for that
        // many more: a run of them ahead can't fill every batch.
        let limit = (BODY_BATCH + self.skipped.len()).min(MAX_PENDING_BODIES);
        let pending = self
            .worker
            .blocking({
                let id = self.account_id.to_owned();
                move |resources| resources.read(|tx| pending_proton_bodies(tx, &id, &opened, limit))
            })
            .await
            .map_err(|_| Halt::Stopped)??;
        for body in pending {
            if *self.stop.borrow() {
                return Err(Halt::Stopped);
            }
            if self.skipped.contains(&body.provider_key) {
                continue;
            }
            self.acquire_one(&body, account.key_generation, account.revision)
                .await?;
        }
        Ok(())
    }

    /// Fetch, decrypt and commit one pending body. A failure is
    /// recorded and the queue moves on; a transport failure is the
    /// caller's `Halt`.
    async fn acquire_one(
        &mut self,
        body: &PendingBody,
        key_generation: i64,
        revision: i64,
    ) -> Result<(), Halt> {
        let fetched = match self
            .call_classified(|client, session| async move {
                client.message(&session, &body.provider_key).await
            })
            .await
        {
            Ok(fetched) => fetched,
            // spec/04 step 3: the id in the response must equal
            // provider_key. The message's metadata is re-read by id and
            // applied as an event's change would; the body isn't
            // fetched again this run.
            Err(FetchError::UnexpectedMessage) => {
                self.reread_metadata(body, revision).await?;
                self.skipped.insert(body.provider_key.clone());
                return Ok(());
            }
            Err(FetchError::Ended) => return Err(Halt::Ended),
            Err(_) => return Err(Halt::Retry),
        };
        let body_bytes = match self
            .with_keys(|keys| keys.decrypt_body(fetched.body.as_bytes()))
            .await?
        {
            Ok(bytes) => bytes,
            Err(failure) => {
                self.record_failure(body.message_id, failure.into(), key_generation)
                    .await?;
                return Ok(());
            }
        };
        let mut attachments = Vec::new();
        for attachment in &fetched.attachments {
            let packet = match self
                .call_classified({
                    let id = attachment.id.clone();
                    move |client, session| {
                        let id = id.clone();
                        async move { client.attachment(&session, &id).await }
                    }
                })
                .await
            {
                Ok(packet) => packet,
                Err(FetchError::AttachmentTooLarge) => {
                    self.record_failure(body.message_id, BodyFailure::TooLarge, key_generation)
                        .await?;
                    return Ok(());
                }
                Err(FetchError::Ended) => return Err(Halt::Ended),
                Err(_) => return Err(Halt::Retry),
            };
            let data = match self
                .with_keys(|keys| keys.decrypt_attachment(&attachment.key_packets, &packet))
                .await?
            {
                Ok(data) => data,
                Err(_) => {
                    self.record_failure(body.message_id, BodyFailure::Attachment, key_generation)
                        .await?;
                    return Ok(());
                }
            };
            attachments.push((attachment.clone(), data));
        }
        let now = self.worker.resources().now();
        let account_id = self.account_id.to_owned();
        let committed = self
            .worker
            .blocking(move |resources| {
                acquire_proton_body(
                    resources,
                    &account_id,
                    &fetched,
                    body_bytes,
                    attachments,
                    now,
                )
            })
            .await
            .map_err(|_| Halt::Stopped)?;
        match committed {
            Ok(_) => Ok(()),
            Err(failure) => match failure.reason() {
                Some(_) => {
                    self.record_failure(body.message_id, failure, key_generation)
                        .await?;
                    Ok(())
                }
                // A store failure is the caller's retry, never a
                // recorded message failure.
                None => Err(Halt::Retry),
            },
        }
    }

    /// Re-read one message's metadata by id after its body response
    /// named another message (spec/04 step 3, plan M2.16d), and apply
    /// it as an event's change would. An empty answer writes nothing,
    /// since only a delete event or a complete listing removes a
    /// message; nor does one naming another message, which is refused
    /// as the body was. A transport failure is the caller's `Halt`.
    async fn reread_metadata(&self, body: &PendingBody, revision: i64) -> Result<(), Halt> {
        let key = body.provider_key.clone();
        let metadata = match self
            .call_classified(move |client, session| {
                let key = key.clone();
                async move { client.message_metadata(&session, &key).await }
            })
            .await
        {
            Ok(metadata) => metadata,
            // Retrying would fetch the same refused body forever and
            // starve the rest of the queue.
            Err(FetchError::UnexpectedMessage) => None,
            Err(FetchError::Ended) => return Err(Halt::Ended),
            Err(_) => return Err(Halt::Retry),
        };
        let Some(metadata) = metadata else {
            return Ok(());
        };
        let listed = listed_message(&metadata);
        let id = self.account_id.to_owned();
        let now = self.worker.resources().now();
        self.write(move |tx| {
            apply_reread_message(
                tx,
                &RereadMessage {
                    account_id: &id,
                    message: &listed,
                    expected_revision: revision,
                },
                now,
            )
            .map(drop)
        })
        .await?;
        Ok(())
    }

    /// Record one body failure at the account's current key generation;
    /// the store's queue excludes the message until the generation
    /// moves (spec/02 `proton_body_failures`).
    async fn record_failure(
        &self,
        message_id: i64,
        failure: BodyFailure,
        key_generation: i64,
    ) -> Result<(), Halt> {
        let reason = failure.reason().expect("a recorded failure names a reason");
        let detail = failure.detail();
        let now = self.worker.resources().now();
        self.write(move |tx| {
            record_body_failure(tx, message_id, reason, detail, key_generation, now).map(drop)
        })
        .await
    }

    /// Run `decrypt` with the account's current keys, copied out of the
    /// session map under the lock only for the copy (spec/10: the lock
    /// is never held across an await or a decrypt).
    async fn with_keys<T>(&self, decrypt: impl FnOnce(&dyn KeyStore) -> T) -> Result<T, Halt> {
        let keys = {
            let sessions = self.sessions.lock().await;
            sessions
                .get(self.account_id)
                .map(|restored| Arc::clone(&restored.keys))
                .ok_or(Halt::Ended)?
        };
        Ok(decrypt(keys.as_ref()))
    }

    /// Fetch and apply one events response; whether more follow.
    async fn poll_once(&self) -> Result<bool, Halt> {
        let id = self.account_id.to_owned();
        let account = self
            .worker
            .blocking(move |resources| resources.read(|tx| proton_account(tx, &id)))
            .await
            .map_err(|_| Halt::Stopped)??
            .ok_or(Halt::Ended)?;
        let (ProtonListingState::Complete, Some(cursor)) =
            (account.listing_state, account.event_cursor)
        else {
            return Err(Halt::Refresh);
        };
        let from = EventId::new(cursor);
        let batch = self
            .call(|client, session| {
                let from = from.clone();
                async move { client.events_since(&session, &from).await }
            })
            .await?;
        if !batch.addresses.is_empty() || batch.user_changed {
            self.refresh_keys().await?;
        }
        if batch.refresh_mail {
            // spec/04 "Refresh": list again from this response's cursor;
            // the listing brings what it changed.
            let id = self.account_id.to_owned();
            let next = batch.event_id.as_str().to_owned();
            let revision = account.revision;
            self.write(move |tx| begin_listing(tx, &id, &next, revision).map(drop))
                .await?;
            return Err(Halt::Refresh);
        }
        self.apply(&batch, from.as_str(), account.revision).await?;
        self.advanced.store(true, Ordering::Relaxed);
        self.record_sync();
        Ok(batch.more)
    }

    /// Apply one response in budget-sized chunks; the labels, re-listed
    /// when it changed one, reconcile with the first and the cursor
    /// moves with the last.
    async fn apply(&self, batch: &EventBatch, from: &str, revision: i64) -> Result<(), Halt> {
        let changes = changes(batch)?;
        if changes.is_empty() && batch.labels.is_empty() && batch.event_id.as_str() == from {
            return Ok(());
        }
        let mut labels = if batch.labels.is_empty() {
            None
        } else {
            let listed = self
                .call(|client, session| async move { client.labels(&session).await })
                .await?;
            Some(proton_api::observed_labels(&listed))
        };
        let chunks: Vec<Vec<ProtonChange>> = if changes.is_empty() {
            vec![Vec::new()]
        } else {
            changes
                .chunks(CHANGES_PER_WRITE)
                .map(<[ProtonChange]>::to_vec)
                .collect()
        };
        let last = chunks.len() - 1;
        for (index, chunk) in chunks.into_iter().enumerate() {
            let id = self.account_id.to_owned();
            let from = from.to_owned();
            let next = (index == last).then(|| batch.event_id.as_str().to_owned());
            let labels = labels.take();
            let now = self.worker.resources().now();
            let applied = self
                .write(move |tx| {
                    apply_event_chunk(
                        tx,
                        &EventChunk {
                            account_id: &id,
                            from_cursor: &from,
                            labels: labels.as_deref(),
                            changes: &chunk,
                            next_cursor: next.as_deref(),
                            expected_revision: revision,
                        },
                        now,
                    )
                })
                .await?;
            // A new logical message in the account's Inbox role
            // mailbox is the only arrival that feeds new mail
            // (spec/10 "Notifications"); a bootstrap and a resync
            // report none.
            if !applied.new_inbox.is_empty() {
                self.worker
                    .windows()
                    .new_mail_arrived(self.account_id, applied.new_inbox);
            }
        }
        Ok(())
    }

    /// An address, user or key event (spec/04-sync-proton "Sync loop"
    /// step 2): unlock the keys again with the passphrase kept in
    /// memory and refresh the identities. It runs before the response's
    /// cursor moves, so a failure retries the response. Keys the
    /// passphrase no longer unlocks, as after a password change, leave
    /// the account needing the owner to sign in again.
    async fn refresh_keys(&self) -> Result<(), Halt> {
        let (passphrase, before) = {
            let sessions = self.sessions.lock().await;
            let restored = sessions.get(self.account_id).ok_or(Halt::Ended)?;
            (
                Arc::clone(&restored.key_passphrase),
                key_state(restored.keys.as_ref()),
            )
        };
        let unlocked = self
            .call(|client, session| {
                let passphrase = Arc::clone(&passphrase);
                async move {
                    match client
                        .unlock_keys_with_secret(&session, passphrase.expose())
                        .await
                    {
                        Ok(keys) => Ok(Ok((session, keys))),
                        Err(ProtonError::KeyUnlock(_)) => Ok(Err(session)),
                        Err(error) => Err(error),
                    }
                }
            })
            .await?;
        // The session map's lock is held through each write below, as a
        // sign-in again holds it through its own: a sign-in or restore
        // meanwhile brought its own session, keys and identities, so
        // what this unlock saw is stale and nothing is written.
        let mut sessions = self.sessions.lock().await;
        let (used, keys) = match unlocked {
            Ok(unlocked) => unlocked,
            Err(used) => {
                if !still_holds(&sessions, self.account_id, &used, &passphrase) {
                    return Err(Halt::Retry);
                }
                sessions.remove(self.account_id);
                mark_session_refused(self.worker, self.account_id).await;
                return Err(Halt::Ended);
            }
        };
        if !still_holds(&sessions, self.account_id, &used, &passphrase) {
            return Ok(());
        }
        let identities = proton_identities(keys.as_ref());
        // Key unlocking that changed what it unlocked (the identities'
        // address ids or the failed key ids) moves the generation, so
        // bodies that failed under the old keys are tried again
        // (spec/02 `key_generation`).
        let changed_keys = key_state(keys.as_ref()) != before;
        let id = self.account_id.to_owned();
        self.write(move |tx| -> Result<(), StoreError> {
            refresh_proton_identities(tx, &id, &identities)?;
            if changed_keys {
                bump_key_generation(tx, &id)?;
            }
            Ok(())
        })
        .await?;
        if let Some(restored) = sessions.get_mut(self.account_id) {
            restored.keys = Arc::from(keys);
        }
        Ok(())
    }

    /// One Proton request with the account's current session. A 401
    /// refreshes the session once through restore, which stores the
    /// rotated refresh token before the new session is used, and
    /// retries the request (spec/04 "Refresh"); a refused refresh
    /// leaves the account needing attention and ends the worker.
    async fn call<'c, T, F, Fut>(&'c self, request: F) -> Result<T, Halt>
    where
        F: Fn(&'c ProtonClient, Session) -> Fut,
        Fut: Future<Output = Result<T, ProtonError>>,
    {
        self.call_classified(request)
            .await
            .map_err(|error| match error {
                FetchError::Ended => Halt::Ended,
                _ => Halt::Retry,
            })
    }

    /// [`Attempt::call`], keeping the typed failures a body's fetch
    /// distinguishes: a response naming another message and an
    /// attachment past its bound (spec/04 step 3).
    async fn call_classified<'c, T, F, Fut>(&'c self, request: F) -> Result<T, FetchError>
    where
        F: Fn(&'c ProtonClient, Session) -> Fut,
        Fut: Future<Output = Result<T, ProtonError>>,
    {
        let session = self.session().await.map_err(|_| FetchError::Ended)?;
        match request(self.client, session.clone()).await {
            Ok(value) => return Ok(value),
            Err(ProtonError::UnexpectedMessage) => return Err(FetchError::UnexpectedMessage),
            Err(ProtonError::AttachmentTooLarge) => return Err(FetchError::AttachmentTooLarge),
            Err(ProtonError::Unauthorized) => {}
            Err(error) => return Err(self.refusal(&error).await.into()),
        }
        self.refresh(&session).await;
        let session = self.session().await.map_err(|_| FetchError::Ended)?;
        match request(self.client, session).await {
            Ok(value) => Ok(value),
            Err(ProtonError::UnexpectedMessage) => Err(FetchError::UnexpectedMessage),
            Err(ProtonError::AttachmentTooLarge) => Err(FetchError::AttachmentTooLarge),
            // A second 401 is the refresh's own failure, not a Proton
            // refusal with a code; retry as before, recording nothing.
            Err(ProtonError::Unauthorized) => Err(FetchError::Retry),
            Err(error) => Err(self.refusal(&error).await.into()),
        }
    }

    /// Record a mapped refusal's redacted diagnostics, then map the
    /// account-state refusals (spec/04's table): human verification
    /// and a refused app identity put the account `needs_attention`
    /// and end the worker without retrying; nothing local is deleted.
    /// Every other refusal leaves the account active and retries.
    async fn refusal(&self, error: &ProtonError) -> Halt {
        let (code, message) = refusal_diagnostics(error);
        let id = self.account_id.to_owned();
        let now = self.worker.resources().now();
        let _ = self
            .worker
            .resources()
            .write(WritePriority::Sync, move |tx| {
                record_refusal(tx, &id, code, &message, now).map(|_| ())
            })
            .await;
        // The refusal is part of `account_health()`: wake its subscription.
        self.worker.statuses().rows_changed();
        let Some(reason) = needs_attention(error) else {
            return Halt::Retry;
        };
        // The worker ends only once the pause persisted: ending without
        // it would leave an active account with nothing syncing it, so
        // a failed write retries the attempt, which records and pauses
        // again.
        let id = self.account_id.to_owned();
        let paused = self
            .worker
            .resources()
            .write(WritePriority::Sync, move |tx| {
                pause_for_reason(tx, &id, reason)
            })
            .await;
        if paused.is_err() {
            return Halt::Retry;
        }
        self.worker.statuses().rows_changed();
        Halt::Ended
    }

    async fn session(&self) -> Result<Session, Halt> {
        let sessions = self.sessions.lock().await;
        sessions
            .get(self.account_id)
            .map(|restored| restored.session.clone())
            .ok_or(Halt::Ended)
    }

    /// Replace a session Proton refused, unless another caller (a
    /// sign-in again) already did.
    async fn refresh(&self, refused: &Session) {
        {
            let mut sessions = self.sessions.lock().await;
            if !is_current(&sessions, self.account_id, refused) {
                return;
            }
            sessions.remove(self.account_id);
        }
        restore_proton_account(self.worker, self.sessions, self.account_id).await;
    }

    async fn write<T, F>(&self, closure: F) -> Result<T, Halt>
    where
        T: Send + 'static,
        F: FnOnce(&Transaction) -> Result<T, StoreError> + Send + 'static,
    {
        let written = self
            .worker
            .resources()
            .write(WritePriority::Sync, closure)
            .await?;
        // The write changed a row the health query reads: wake its
        // subscription (plan M2.7b2).
        self.worker.statuses().rows_changed();
        Ok(written)
    }
}

/// Wait out the poll interval before the next events poll; `None`
/// polls again, `Some` halts the worker. `now` reads the engine's
/// clock and `start` is the poll's start by it; the deadline is
/// `start + interval`, recomputed from `start` when the app-active
/// signal changes, so a switch to active never waits out a running
/// 2-min wait (spec/04 "Sync loop" step 2). A poll hint polls at
/// once; a test wake only re-reads the clock.
async fn wait_for_next_poll(
    now: impl Fn() -> sb_resources::Timestamp,
    stop: &mut watch::Receiver<bool>,
    poll: &mut watch::Receiver<()>,
    active: &mut watch::Receiver<bool>,
    wake: &tokio::sync::Notify,
    start: sb_resources::Timestamp,
) -> Option<Halt> {
    loop {
        let interval = if *active.borrow() {
            ACTIVE_POLL_INTERVAL
        } else {
            INACTIVE_POLL_INTERVAL
        };
        let deadline = start.as_millis() + interval.as_millis() as i64;
        let remaining = deadline - now().as_millis();
        if remaining <= 0 {
            return None;
        }
        let sleep = tokio::time::sleep(Duration::from_millis(
            u64::try_from(remaining).unwrap_or(u64::MAX),
        ));
        tokio::select! {
            () = sleep => return None,
            () = stopped(stop) => return Some(Halt::Stopped),
            changed = poll.changed() => {
                if changed.is_err() {
                    return Some(Halt::Stopped);
                }
                return None;
            }
            changed = active.changed() => {
                if changed.is_err() {
                    return Some(Halt::Stopped);
                }
                // Recompute the deadline from the wait's start with the
                // new cadence.
            }
            () = wake.notified() => {
                // Re-read the clock; the deadline may have moved.
            }
        }
    }
}

/// Proton's numeric code and message for a mapped refusal (spec/04's
/// table, spec/10). The code is `None` when Proton sent none (a 5xx
/// body, an offline transport).
fn refusal_diagnostics(error: &ProtonError) -> (Option<i64>, String) {
    match error {
        ProtonError::HumanVerificationRequired => (
            Some(ApiCode::HumanVerificationRequired as i64),
            error.to_string(),
        ),
        ProtonError::Refused { code, message } => (Some(*code as i64), message.clone()),
        ProtonError::RefreshRefused { message } => {
            (Some(ApiCode::RefreshTokenInvalid as i64), message.clone())
        }
        ProtonError::HumanVerificationInvalid { message } => (
            Some(ApiCode::HumanVerificationTokenInvalid as i64),
            message.clone(),
        ),
        other => (None, other.to_string()),
    }
}

/// The `state_reason` a mapped refusal stops the worker for (spec/04's
/// table): human verification, and this app's identity refused (2064,
/// 5001, 5002, 5003). `None` when the worker retries.
fn needs_attention(error: &ProtonError) -> Option<&'static str> {
    match error {
        ProtonError::HumanVerificationRequired => Some(HUMAN_VERIFICATION_REASON),
        ProtonError::Refused { code, .. } => matches!(
            code,
            ApiCode::ProductInvalid
                | ApiCode::AppVersionMissingDev
                | ApiCode::AppVersionMissing
                | ApiCode::AppVersionUnsupported
        )
        .then_some(APP_IDENTITY_REFUSED_REASON),
        _ => None,
    }
}

/// Record why the worker retries and when (spec/04's table). A rate
/// limit keeps the time Proton asked for, which the observer recorded
/// while the client slept.
fn record_retry(
    statuses: &Arc<crate::account_health::WorkerStatuses>,
    account_id: &str,
    backoff: Option<BackoffKind>,
    now_ms: i64,
    delay: Duration,
) {
    let last_sync_ms = statuses
        .get(account_id)
        .and_then(|status| status.last_sync_ms);
    let next_retry_ms = now_ms + delay.as_millis() as i64;
    let condition = match backoff {
        Some(BackoffKind::Network) => LiveCondition::Offline { next_retry_ms },
        Some(BackoffKind::ServerError) | None => LiveCondition::NotAnswering { next_retry_ms },
        Some(BackoffKind::RateLimit) => match statuses.get(account_id) {
            Some(status) if matches!(status.condition, LiveCondition::RateLimited { .. }) => {
                status.condition
            }
            _ => LiveCondition::RateLimited {
                until_ms: next_retry_ms,
            },
        },
    };
    statuses.set(
        account_id,
        LiveStatus {
            last_sync_ms,
            condition,
        },
    );
}

/// Whether the account's session is still the one a request used.
fn is_current(
    sessions: &std::collections::HashMap<String, crate::proton::RestoredSession>,
    account_id: &str,
    used: &Session,
) -> bool {
    sessions.get(account_id).is_some_and(|restored| {
        restored.session.expose_access_token() == used.expose_access_token()
    })
}

/// Whether the account still holds the session and key passphrase a
/// key unlock used.
fn still_holds(
    sessions: &std::collections::HashMap<String, crate::proton::RestoredSession>,
    account_id: &str,
    used: &Session,
    passphrase: &Secret,
) -> bool {
    is_current(sessions, account_id, used)
        && sessions
            .get(account_id)
            .is_some_and(|restored| restored.key_passphrase.expose() == passphrase.expose())
}

/// A response's message changes in its order. A change without the
/// message's metadata can't be applied, so the response is retried.
fn changes(batch: &EventBatch) -> Result<Vec<ProtonChange>, Halt> {
    batch
        .messages
        .iter()
        .map(|event| match (event.action, &event.message) {
            (EventAction::Delete, _) => Ok(ProtonChange::Delete(event.id.clone())),
            (_, Some(metadata)) if metadata.id == event.id => {
                Ok(ProtonChange::Upsert(Box::new(listed_message(metadata))))
            }
            _ => Err(Halt::Retry),
        })
        .collect()
}
