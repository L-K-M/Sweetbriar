//! Proton accounts' sync workers (spec/04-sync-proton "Sync loop" step
//! 1). An account with a session in memory gets one worker: it reads
//! the event cursor before listing, reconciles the labels through the
//! `proton_api` policy, commits the metadata listing page by page with
//! its checkpoint, and finishes with Proton's per-label totals. A
//! crash or restart resumes from the last committed page. Events
//! (step 2) follow in M2.15c; until then a finished account's worker
//! ends.
//!
//! The worker takes the account's session from the engine's map before
//! every request, so signing in again hands it the new session and a
//! disconnect, which removes the session, ends it.

use crate::account_sync::AccountJobs;
use crate::engine::{Engine, Spawn, Worker};
use crate::policy::proton_api;
use crate::proton::{Sessions, restore_proton_account};
use sb_resources::proton_sync::{
    ListedPage, ProtonListingState, anchor, begin_listing, checkpoint, commit_listed_page,
    finish_listing, listed_message, reconcile_proton_labels,
};
use sb_resources::{
    ListingAnchor, ProtonClient, ProtonError, Session, StoreError, Transaction, WritePriority,
    proton_account,
};

use std::future::Future;
use std::sync::Arc;
use std::time::Duration;
use tokio::sync::watch;

/// Messages per listed page: the most the transport asks for.
const PAGE_SIZE: u32 = 150;
/// The first wait after a failed attempt; it doubles up to
/// [`MAX_RETRY_DELAY`] (spec/04 "Backoff": 1 s to 5 min).
const FIRST_RETRY_DELAY: Duration = Duration::from_secs(1);
const MAX_RETRY_DELAY: Duration = Duration::from_secs(5 * 60);

/// Why an attempt stopped short of finishing the listing.
#[derive(Debug)]
enum Halt {
    /// The worker was told to stop (shutdown or a restore's quiesce).
    Stopped,
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

impl Engine {
    /// Start a worker for every Proton account with a session: after
    /// open's restore, or after a backup restore resumes the workers.
    pub(crate) fn schedule_proton_sync(&self) {
        let jobs = Arc::clone(self.account_jobs());
        let sessions = Arc::clone(&self.proton_sessions);
        let _ = self.spawn_worker(move |worker| async move {
            start_all(&worker, &jobs, &sessions).await;
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
}

/// Start a worker for every account whose session is in memory.
pub(crate) async fn start_all(
    spawner: &impl Spawn,
    jobs: &Arc<AccountJobs>,
    sessions: &Arc<Sessions>,
) {
    let accounts: Vec<String> = sessions.lock().await.keys().cloned().collect();
    for account_id in accounts {
        start(spawner, jobs, sessions, account_id);
    }
}

/// Start the account's worker unless one runs.
pub(crate) fn start(
    spawner: &impl Spawn,
    jobs: &Arc<AccountJobs>,
    sessions: &Arc<Sessions>,
    account_id: String,
) {
    let Some(stop) = jobs.claim(&account_id) else {
        return;
    };
    let owner = Arc::clone(jobs);
    let sessions = Arc::clone(sessions);
    let id = account_id.clone();
    let spawned = spawner.spawn_worker(move |worker| async move {
        run(&worker, &sessions, &id, stop).await;
        owner.release(&id);
    });
    if spawned.is_err() {
        jobs.release(&account_id);
    }
}

async fn run(worker: &Worker, sessions: &Sessions, account_id: &str, stop: watch::Receiver<bool>) {
    let Ok(client) = worker.resources().proton_client() else {
        return;
    };
    let mut delay = FIRST_RETRY_DELAY;
    loop {
        let attempt = Attempt {
            worker,
            sessions,
            client: &client,
            account_id,
            stop: &stop,
        };
        // A stop doesn't wait for a request in flight: a write already
        // submitted commits or not as a whole, and the next attempt
        // starts from what the store says.
        let mut wait = stop.clone();
        let outcome = tokio::select! {
            outcome = attempt.bootstrap() => outcome,
            () = stopped(&mut wait) => return,
        };
        match outcome {
            Ok(()) | Err(Halt::Stopped | Halt::Ended) => return,
            Err(Halt::Retry) => {}
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
    client: &'a ProtonClient,
    account_id: &'a str,
    stop: &'a watch::Receiver<bool>,
}

impl Attempt<'_> {
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

        // 1.4: totals, and the pending cursor becomes the event cursor.
        let counts = self
            .call(|client, session| async move { client.message_counts(&session).await })
            .await?;
        let totals: Vec<(String, u64)> = counts
            .into_iter()
            .map(|count| (count.label_id, u64::from(count.total)))
            .collect();
        let id = self.account_id.to_owned();
        self.write(move |tx| finish_listing(tx, &id, &totals, revision).map(drop))
            .await
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
        let session = self.session().await?;
        match request(self.client, session.clone()).await {
            Ok(value) => return Ok(value),
            Err(ProtonError::Unauthorized) => {}
            Err(_) => return Err(Halt::Retry),
        }
        self.refresh(&session).await;
        let session = self.session().await?;
        request(self.client, session).await.map_err(|_| Halt::Retry)
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
            let current = sessions.get(self.account_id);
            if current.is_none_or(|restored| {
                restored.session.expose_access_token() != refused.expose_access_token()
            }) {
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
        Ok(self
            .worker
            .resources()
            .write(WritePriority::Sync, closure)
            .await?)
    }
}
