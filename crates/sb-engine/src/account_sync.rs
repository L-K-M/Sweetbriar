//! Per-account sync workers (spec/01 "Process model", spec/04
//! "Connections" and "Account states and health"). Each syncable IMAP
//! account gets one worker that connects through the account's pinned
//! endpoint with a credential read fresh for every attempt, retries
//! through the transport's backoff, and drives the account's state:
//! a refused connection shows "Bridge isn't running" while retries
//! continue, a refused login or a changed certificate pauses the
//! account and ends the worker until the user acts, and a connect
//! lifts a connection pause. A live session runs sync passes (`pass`)
//! on the schedule `idle` keeps, woken by the account's IDLE watcher,
//! until the worker is stopped, the session fails, or the account
//! stops syncing.

use std::collections::HashMap;
use std::io::ErrorKind;
use std::sync::{Arc, Mutex, MutexGuard};
use std::time::{Duration, Instant};

use sb_resources::{
    CertificatePin, ConnectAttempt, Connection, ConnectionPause, Connector, CredentialPause,
    ImapEndpoint, ImapError, ImapProvider, Password, ReconnectError, ReconnectWait, Reconnector,
    SecretError, WritePriority, clear_connection_pause, connect_account, imap_endpoint,
    pause_for_connection, pause_for_credential, syncable_accounts,
};
use tokio::sync::watch;

use crate::Engine;
use crate::engine::{Spawn, Worker};
use crate::policy::Policy;
use crate::threads::{ThreadRebuilds, schedule_threads};

mod idle;
mod pass;

use idle::{IdleState, Schedule};

/// How long a worker waits before starting over after a failure the
/// transport does not retry and no account state names (a protocol
/// fault): the reconnect backoff's own ceiling.
const RESTART_DELAY: Duration = Duration::from_secs(5 * 60);

/// The first wait before reconnecting after a session that failed
/// before completing a pass; it doubles up to [`RESTART_DELAY`], the
/// reconnect backoff's own range (spec/04: 1 s to 5 min).
const UNPROVEN_SESSION_DELAY: Duration = Duration::from_secs(1);

/// The wait before reconnecting after a failed session. The transport's
/// [`Reconnector`] restarts at its first step on every connect, so a
/// server that accepts each login and then fails every pass would be
/// retried each second forever; this backoff grows across such
/// sessions and resets only once a session completes a pass.
#[derive(Debug)]
struct SessionBackoff {
    next: Duration,
}

impl SessionBackoff {
    fn new() -> Self {
        Self {
            next: UNPROVEN_SESSION_DELAY,
        }
    }

    /// A session completed a pass: the backoff starts over.
    fn proven(&mut self) {
        self.next = UNPROVEN_SESSION_DELAY;
    }

    /// The wait after a failed session: none when it completed a pass
    /// (and the backoff starts over), else the next step.
    fn after_failure(&mut self, completed_a_pass: bool) -> Duration {
        if completed_a_pass {
            self.proven();
            return Duration::ZERO;
        }
        let wait = self.next;
        self.next = wait.saturating_mul(2).min(RESTART_DELAY);
        wait
    }
}

/// The live account workers. Each holds a stop signal; `stop` raises
/// every one and refuses new workers (shutdown, and the restore
/// quiesce of plan M1.8g).
#[derive(Default)]
pub(crate) struct AccountJobs {
    state: Mutex<Jobs>,
}

#[derive(Default)]
struct Jobs {
    stopping: bool,
    live: HashMap<String, watch::Sender<bool>>,
}

impl AccountJobs {
    fn lock(&self) -> MutexGuard<'_, Jobs> {
        self.state
            .lock()
            .expect("the account workers' state poisoned")
    }

    /// Stop every account worker and start no more.
    pub(crate) fn stop(&self) {
        let mut jobs = self.lock();
        jobs.stopping = true;
        for stop in jobs.live.values() {
            let _ = stop.send(true);
        }
    }

    /// Whether a worker runs for the account.
    #[cfg(test)]
    pub(crate) fn running(&self, account_id: &str) -> bool {
        self.lock().live.contains_key(account_id)
    }

    /// Reserve the account's one worker, unless stopping or already
    /// running.
    fn claim(&self, account_id: &str) -> Option<watch::Receiver<bool>> {
        let mut jobs = self.lock();
        if jobs.stopping || jobs.live.contains_key(account_id) {
            return None;
        }
        let (stop, stopped) = watch::channel(false);
        jobs.live.insert(account_id.to_owned(), stop);
        Some(stopped)
    }

    fn release(&self, account_id: &str) {
        self.lock().live.remove(account_id);
    }
}

impl Engine {
    /// Start a worker for every syncable account (spec/04), off the
    /// caller's thread.
    pub(crate) fn schedule_accounts(&self) {
        let jobs = Arc::clone(self.account_jobs());
        let threads = Arc::clone(self.imports().threads());
        let _ = self.spawn_worker(move |worker| async move {
            let accounts = worker
                .blocking(|resources| resources.read(syncable_accounts))
                .await;
            let Ok(Ok(accounts)) = accounts else {
                return;
            };
            for account_id in accounts {
                start_account(&worker, &jobs, &threads, account_id);
            }
        });
    }

    /// Start the account's worker unless one runs (a newly added
    /// account).
    pub(crate) fn start_account(&self, account_id: String) {
        start_account(
            self,
            self.account_jobs(),
            self.imports().threads(),
            account_id,
        );
    }
}

fn start_account(
    spawner: &impl Spawn,
    jobs: &Arc<AccountJobs>,
    threads: &Arc<ThreadRebuilds>,
    account_id: String,
) {
    let Some(stop) = jobs.claim(&account_id) else {
        return;
    };
    let owner = Arc::clone(jobs);
    let threads = Arc::clone(threads);
    let id = account_id.clone();
    let spawned = spawner.spawn_worker(move |worker| async move {
        run_account(&worker, &id, stop, &threads).await;
        owner.release(&id);
    });
    if spawned.is_err() {
        jobs.release(&account_id);
    }
}

/// What the store records for an account after a connection attempt.
enum Transition {
    Connected,
    Connection(ConnectionPause),
    Credential(CredentialPause),
}

/// How a live session ended.
enum Served {
    /// The worker was told to stop.
    Stopped,
    /// The account no longer syncs (paused, disconnected or removed).
    Ended,
    /// The session failed; recovery reconnects. `completed_a_pass` says
    /// whether it proved healthy first (see [`SessionBackoff`]).
    Failed {
        error: ImapError,
        completed_a_pass: bool,
    },
    /// The server refused one mailbox's command and the session is
    /// gone; the worker reconnects at once, leaving that mailbox out.
    /// `completed_a_pass` as for `Failed`.
    Refused { completed_a_pass: bool },
}

/// How a recovery ended.
enum Recovery {
    /// Boxed: a live session is large and moves once.
    Connected(Box<Connection>),
    Stopped,
    Restart,
}

/// A failure before any IMAP exchange, recorded by the connector: the
/// reconnect policy sees only a non-retryable `ImapError`.
enum SetupFailure {
    Secret(SecretError),
    UnusableSecret,
}

async fn run_account(
    worker: &Worker,
    account_id: &str,
    stop: watch::Receiver<bool>,
    threads: &Arc<ThreadRebuilds>,
) {
    // However the worker was asked for (a replayed setup, a pause that
    // raced the start), an account that no longer syncs gets no session.
    let id = account_id.to_owned();
    let syncable = worker
        .blocking(|resources| resources.read(syncable_accounts))
        .await;
    if !matches!(syncable, Ok(Ok(accounts)) if accounts.contains(&id)) {
        return;
    }
    let endpoint = worker
        .blocking(move |resources| resources.read(|tx| imap_endpoint(tx, &id)))
        .await;
    let Ok(Ok(endpoint)) = endpoint else {
        return;
    };
    // The pilot sets up Bridge accounts only (spec/04).
    if endpoint.provider != ImapProvider::ProtonBridge {
        return;
    }
    let policy = Policy::ProtonBridge;
    // Without a usable pin there is nothing to trust: the re-trust
    // flow (spec/10) gives the account one.
    let Some(pin) = endpoint
        .trusted_cert_sha256
        .as_deref()
        .and_then(CertificatePin::from_hex)
    else {
        record(
            worker,
            account_id,
            Transition::Connection(ConnectionPause::CertificateChanged),
        )
        .await;
        return;
    };
    let mut connector = AccountConnector {
        worker: worker.clone(),
        endpoint,
        pin,
        setup_failure: None,
    };
    let mut reconnector = Reconnector::new();
    let mut sessions = SessionBackoff::new();
    let (watched_endpoint, watcher_stop) = (connector.endpoint.clone(), stop.clone());
    let start_idle = move || start_watcher(worker, &watched_endpoint, pin, &watcher_stop);
    let mut carried = Carried {
        refusals: pass::Refusals::default(),
        idle: None,
        start_idle: &start_idle,
        threads,
    };
    let mut failure: Option<ImapError> = None;
    loop {
        let recovery = match failure.take() {
            None => establish(worker, account_id, &mut reconnector, &mut connector, &stop).await,
            Some(failure) => {
                recover(
                    worker,
                    account_id,
                    &mut reconnector,
                    &mut connector,
                    failure,
                    &stop,
                )
                .await
            }
        };
        let mut connection = match recovery {
            Recovery::Connected(connection) => *connection,
            Recovery::Stopped => return,
            Recovery::Restart => {
                tokio::select! {
                    () = tokio::time::sleep(RESTART_DELAY) => continue,
                    () = stopped(stop.clone()) => return,
                }
            }
        };
        record(worker, account_id, Transition::Connected).await;
        let served = serve(
            worker,
            account_id,
            policy,
            &mut connection,
            &stop,
            &mut carried,
        )
        .await;
        match served {
            // A session stopped mid-command is unusable; dropping it
            // closes the socket.
            Served::Stopped => return,
            Served::Ended => {
                tokio::select! {
                    _ = connection.logout() => {}
                    () = stopped(stop.clone()) => {}
                }
                return;
            }
            Served::Failed {
                error,
                completed_a_pass,
            } => {
                let wait = sessions.after_failure(completed_a_pass);
                if !wait.is_zero() {
                    tokio::select! {
                        () = tokio::time::sleep(wait) => {}
                        () = stopped(stop.clone()) => return,
                    }
                }
                failure = Some(error);
            }
            // A refused mailbox ends a session that may have proved
            // the account; the next session starts at once.
            Served::Refused { completed_a_pass } => {
                if completed_a_pass {
                    sessions.proven();
                }
            }
        }
    }
}

/// Start the account's IDLE watcher on its own session. It ends with
/// the worker: on the stop signal, or once the returned receiver is
/// dropped. If it cannot start, the receiver reports it gone and the
/// worker polls.
fn start_watcher(
    worker: &Worker,
    endpoint: &ImapEndpoint,
    pin: CertificatePin,
    stop: &watch::Receiver<bool>,
) -> watch::Receiver<IdleState> {
    let (state, idle) = watch::channel(IdleState::default());
    let endpoint = endpoint.clone();
    let stop = stop.clone();
    let _ = worker.spawn_worker(move |worker| {
        let connector = AccountConnector {
            worker,
            endpoint,
            pin,
            setup_failure: None,
        };
        idle::watch_inbox(connector, stop, state)
    });
    idle
}

/// The first attempt of a connection, raced against the stop signal,
/// then the backoff's retries.
async fn establish(
    worker: &Worker,
    account_id: &str,
    reconnector: &mut Reconnector,
    connector: &mut AccountConnector,
    stop: &watch::Receiver<bool>,
) -> Recovery {
    let first = tokio::select! {
        result = connector.connect() => result,
        () = stopped(stop.clone()) => return Recovery::Stopped,
    };
    match first {
        Ok(connection) => Recovery::Connected(Box::new(connection)),
        Err(failure) => recover(worker, account_id, reconnector, connector, failure, stop).await,
    }
}

/// Run sync passes on a live session while the account syncs: a
/// full pass first and on `Schedule`'s cadence, an INBOX pass on each
/// IDLE hint or poll. Each pass races the stop signal, so shutdown
/// never waits for a long backfill. The IDLE watcher starts after the
/// first pass, so its session never competes with the initial sync.
async fn serve(
    worker: &Worker,
    account_id: &str,
    policy: Policy,
    connection: &mut Connection,
    stop: &watch::Receiver<bool>,
    carried: &mut Carried<'_>,
) -> Served {
    let mut schedule = Schedule::new(Instant::now());
    // The watcher may be gone (no IDLE, or it could not start); then
    // its last state says nothing and the worker polls.
    let mut watching = true;
    let mut completed_a_pass = false;
    loop {
        if !still_syncable(worker, account_id).await {
            return Served::Ended;
        }
        let seen = carried
            .idle
            .as_mut()
            .map_or_else(IdleState::default, |idle| *idle.borrow_and_update());
        let scope = schedule.scope(Instant::now(), seen);
        let passed = tokio::select! {
            result = pass::run(worker, account_id, policy, connection, scope, &mut carried.refusals) => result,
            () = stopped(stop.clone()) => return Served::Stopped,
        };
        // The pass has ended, however it ended: messages it stored get
        // their threads now (spec/02 "Threads"), not at the next open.
        // The rebuild does nothing when no batch stored a new message,
        // and a refusal means shutdown began.
        schedule_threads(worker, carried.threads);
        match passed {
            Ok(()) => completed_a_pass = true,
            Err(pass::PassError::Session(error)) => {
                return Served::Failed {
                    error,
                    completed_a_pass,
                };
            }
            Err(pass::PassError::Refused { mailbox_id }) => {
                carried.refusals.refuse(mailbox_id, Instant::now());
                return Served::Refused { completed_a_pass };
            }
        }
        let idle = carried.idle.get_or_insert_with(carried.start_idle);
        loop {
            let live = watching && idle.borrow().live;
            let rest = schedule.rest(Instant::now(), live);
            tokio::select! {
                () = tokio::time::sleep(rest) => break,
                changed = idle.changed(), if watching => {
                    if changed.is_err() {
                        watching = false;
                    } else if schedule.hinted(*idle.borrow()) {
                        break;
                    }
                }
                () = stopped(stop.clone()) => return Served::Stopped,
            }
        }
    }
}

/// What outlives one session of an account's worker: the refused
/// mailboxes' cooldowns, and the IDLE watcher, started once after the
/// first pass.
struct Carried<'a> {
    refusals: pass::Refusals,
    idle: Option<watch::Receiver<IdleState>>,
    start_idle: &'a (dyn Fn() -> watch::Receiver<IdleState> + Sync),
    /// The engine's thread rebuilds, asked for after each pass.
    threads: &'a Arc<ThreadRebuilds>,
}

/// Whether the account still syncs. A failed read keeps the worker
/// going: the pass's own writes fail harmlessly, and the next check
/// reads again.
async fn still_syncable(worker: &Worker, account_id: &str) -> bool {
    let id = account_id.to_owned();
    match worker
        .blocking(|resources| resources.read(syncable_accounts))
        .await
    {
        Ok(Ok(accounts)) => accounts.contains(&id),
        _ => true,
    }
}

/// Retry through the transport's backoff (spec/04: 1 s to 5 min). A
/// refused connection shows "Bridge isn't running" while it retries;
/// the state write runs beside the retry, which must not block.
async fn recover(
    worker: &Worker,
    account_id: &str,
    reconnector: &mut Reconnector,
    connector: &mut AccountConnector,
    failure: ImapError,
    stop: &watch::Receiver<bool>,
) -> Recovery {
    let (pause, mut paused) = watch::channel(None::<ConnectionPause>);
    let result = {
        let mut retry = Box::pin(reconnector.retry(
            failure,
            connector,
            stopped(stop.clone()),
            |wait: ReconnectWait<'_>| {
                if let Some(next) = retrying_pause(wait.cause) {
                    pause.send_if_modified(|current| {
                        let changed = *current != Some(next);
                        *current = Some(next);
                        changed
                    });
                }
            },
        ));
        loop {
            tokio::select! {
                result = &mut retry => break result,
                changed = paused.changed() => {
                    if changed.is_err() {
                        break retry.await;
                    }
                    let next = *paused.borrow_and_update();
                    if let Some(next) = next {
                        record(worker, account_id, Transition::Connection(next)).await;
                    }
                }
            }
        }
    };
    match result {
        Ok(connection) => Recovery::Connected(Box::new(connection)),
        Err(ReconnectError::Cancelled) => Recovery::Stopped,
        Err(ReconnectError::Paused { cause }) => {
            // A stop that raced the attempt must not record a pause: a
            // secret read cut short by the engine stopping would read
            // as an unavailable secret store.
            if *stop.borrow() {
                return Recovery::Stopped;
            }
            let Some(transition) = paused_transition(connector, &cause) else {
                return Recovery::Restart;
            };
            record(worker, account_id, transition).await;
            Recovery::Stopped
        }
    }
}

/// The pause a retried failure shows: only a refused connection names
/// one (spec/04); timeouts and dropped sessions retry silently.
fn retrying_pause(cause: &ImapError) -> Option<ConnectionPause> {
    match cause {
        ImapError::Io {
            kind: ErrorKind::ConnectionRefused,
            ..
        } => Some(ConnectionPause::BridgeNotRunning),
        _ => None,
    }
}

/// The state a failure the transport will not retry records, or `None`
/// for a protocol fault no state names (the worker starts over later).
fn paused_transition(connector: &mut AccountConnector, cause: &ImapError) -> Option<Transition> {
    if let Some(failure) = connector.setup_failure.take() {
        return Some(match failure {
            SetupFailure::Secret(error) => Transition::Credential(credential_pause(error)),
            // A stored secret unusable as a LOGIN argument needs re-entry.
            SetupFailure::UnusableSecret => Transition::Connection(ConnectionPause::LoginRejected),
        });
    }
    match cause {
        ImapError::LoginRefused | ImapError::LoginDisabled => {
            Some(Transition::Connection(ConnectionPause::LoginRejected))
        }
        ImapError::PinMismatch { .. } => {
            Some(Transition::Connection(ConnectionPause::CertificateChanged))
        }
        _ => None,
    }
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

/// Record a transition. Best effort: the state is what the health
/// surface shows, and a failed write leaves the previous one, which the
/// next attempt corrects. A pause another blocker outranks (a user
/// pause, a credential pause) stays, by the store's own guard.
async fn record(worker: &Worker, account_id: &str, transition: Transition) {
    let id = account_id.to_owned();
    let _ = worker
        .resources()
        .write(WritePriority::Sync, move |tx| match transition {
            Transition::Connected => clear_connection_pause(tx, &id).map(|_| ()),
            Transition::Connection(pause) => pause_for_connection(tx, &id, pause),
            Transition::Credential(pause) => pause_for_credential(tx, &id, pause),
        })
        .await;
}

/// Resolves once the worker is told to stop, or its registry entry is
/// gone.
async fn stopped(mut stop: watch::Receiver<bool>) {
    while !*stop.borrow_and_update() {
        if stop.changed().await.is_err() {
            return;
        }
    }
}

/// A source of fresh sessions for one account (sb-imap's `Connector`):
/// every attempt reads the credential anew on the blocking pool
/// (spec/10) and drops it with the attempt.
struct AccountConnector {
    worker: Worker,
    endpoint: ImapEndpoint,
    pin: CertificatePin,
    setup_failure: Option<SetupFailure>,
}

impl Connector for AccountConnector {
    fn connect(&mut self) -> ConnectAttempt<'_> {
        Box::pin(async move {
            let reference = self.endpoint.credential_ref.clone();
            let secret = self
                .worker
                .blocking(move |resources| resources.get_secret(&reference))
                .await;
            let secret = match secret {
                Ok(Ok(secret)) => secret,
                Ok(Err(error)) => return Err(self.fail(SetupFailure::Secret(error))),
                Err(_) => return Err(self.fail(SetupFailure::Secret(SecretError::Unavailable))),
            };
            let Ok(password) = Password::from_secret(&secret) else {
                return Err(self.fail(SetupFailure::UnusableSecret));
            };
            drop(secret);
            connect_account(&self.endpoint, self.pin, password).await
        })
    }
}

impl AccountConnector {
    /// Record a setup failure and answer the error the reconnect policy
    /// never retries, so recovery ends and reads the recorded cause.
    fn fail(&mut self, failure: SetupFailure) -> ImapError {
        self.setup_failure = Some(failure);
        ImapError::Command {
            operation: "credential",
            message: "the account's credential could not be used".to_owned(),
        }
    }
}

#[cfg(test)]
mod tests;
