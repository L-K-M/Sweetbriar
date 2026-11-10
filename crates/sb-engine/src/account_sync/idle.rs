//! The account's IDLE watcher (spec/04 "Connections" and step 8): a
//! second session that idles on INBOX and wakes the sync worker on
//! every hint. Hints are triggers, never a journal: the worker's pass
//! reads what changed. The watcher records no account state; the sync
//! worker's own session reports Bridge's health, and a watcher that
//! cannot connect only leaves the worker polling.

use std::future::pending;
use std::time::{Duration, Instant};

use sb_resources::{Connector, IdleEvent, ImapError, MailboxNotice, Reconnector};
use tokio::sync::watch;

use super::{AccountConnector, stopped};

/// The mailbox the watcher idles on (spec/04: one IDLE on INBOX).
pub(super) const INBOX: &str = "INBOX";

/// Whether a stored mailbox name is INBOX, which IMAP matches
/// case-insensitively (RFC 9051 section 5.1): a server may list it as
/// `Inbox`, and the policy gives that name the inbox role.
pub(super) fn is_inbox(remote_name: &str) -> bool {
    remote_name.eq_ignore_ascii_case(INBOX)
}

/// How long a live session rests between INBOX passes while IDLE is
/// not running (spec/04 "Pace").
pub(super) const POLL_INTERVAL: Duration = Duration::from_secs(60);

/// How often a pass covers every mailbox, not only INBOX: spec/04's
/// interval for the mailboxes IDLE does not watch.
pub(super) const FULL_INTERVAL: Duration = Duration::from_secs(15 * 60);

/// What the watcher tells the sync worker.
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub(super) struct IdleState {
    /// Whether IDLE runs now; while it doesn't, the worker polls INBOX.
    pub live: bool,
    /// Counts the hints about INBOX.
    pub inbox_hints: u64,
    /// Counts the hints that the mailbox list changed.
    pub list_hints: u64,
}

/// Which mailboxes a pass covers.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub(super) enum Scope {
    /// LIST, then every selectable mailbox.
    All,
    /// INBOX alone, after a hint or a poll.
    Inbox,
}

/// When the worker's passes run: a full pass first and every
/// `FULL_INTERVAL` or when the mailbox list changed, an INBOX pass
/// on each hint, or every `POLL_INTERVAL` while IDLE is not live.
pub(super) struct Schedule {
    next_full: Instant,
    inbox_seen: u64,
    list_seen: u64,
}

impl Schedule {
    pub(super) fn new(now: Instant) -> Self {
        Self {
            next_full: now,
            inbox_seen: 0,
            list_seen: 0,
        }
    }

    /// Whether a hint arrived since the last pass began. IDLE starting
    /// or stopping alone is no reason for a pass; it only changes the
    /// rest.
    pub(super) fn hinted(&self, idle: IdleState) -> bool {
        idle.inbox_hints != self.inbox_seen || idle.list_hints != self.list_seen
    }

    /// The scope of the pass starting now; the hints seen so far are
    /// covered by it.
    pub(super) fn scope(&mut self, now: Instant, idle: IdleState) -> Scope {
        let list_changed = idle.list_hints != self.list_seen;
        self.inbox_seen = idle.inbox_hints;
        self.list_seen = idle.list_hints;
        if now < self.next_full && !list_changed {
            return Scope::Inbox;
        }
        self.next_full = now + FULL_INTERVAL;
        Scope::All
    }

    /// How long to rest after a pass, unless a hint arrives first.
    pub(super) fn rest(&self, now: Instant, idle_live: bool) -> Duration {
        let full = self.next_full.saturating_duration_since(now);
        if idle_live {
            full
        } else {
            full.min(POLL_INTERVAL)
        }
    }
}

/// Idle on INBOX until the worker stops or drops its receiver. A
/// failed session reconnects through the backoff; a failure the
/// backoff will not retry, or a server without IDLE, ends the watcher
/// and leaves the worker polling.
pub(super) async fn watch_inbox(
    mut connector: AccountConnector,
    stop: watch::Receiver<bool>,
    state: watch::Sender<IdleState>,
) {
    let mut reconnector = Reconnector::new();
    let mut failure: Option<ImapError> = None;
    loop {
        let connected = match failure.take() {
            None => tokio::select! {
                result = connector.connect() => result,
                () = ended(&stop, &state) => return,
            },
            Some(failure) => {
                let retried = reconnector
                    .retry(failure, &mut connector, ended(&stop, &state), |_| {})
                    .await;
                // Cancelled, or a pause the sync worker records itself.
                let Ok(connection) = retried else {
                    return;
                };
                Ok(connection)
            }
        };
        let mut connection = match connected {
            Ok(connection) => connection,
            Err(error) => {
                failure = Some(error);
                continue;
            }
        };
        let selected = tokio::select! {
            result = connection.examine(INBOX) => result,
            () = ended(&stop, &state) => return,
        };
        if let Err(error) = selected {
            failure = Some(error);
            continue;
        }
        // Dropping the IDLE future on stop closes the socket at once;
        // a graceful DONE could hold shutdown for its 30 s deadline.
        let watched = tokio::select! {
            result = connection.watch_idle(pending(), |event| hint(&state, &event)) => result,
            () = ended(&stop, &state) => return,
        };
        state.send_if_modified(|current| std::mem::replace(&mut current.live, false));
        match watched {
            Err(ImapError::IdleUnavailable) => return,
            Err(error) => failure = Some(error),
            // IDLE never ends by itself while its stop is pending.
            Ok(()) => failure = Some(ImapError::Closed),
        }
    }
}

/// Pass one IDLE event on, coalesced: the worker reads counts, not
/// events.
fn hint(state: &watch::Sender<IdleState>, event: &IdleEvent<'_>) {
    match event {
        IdleEvent::Started { .. } => {
            state.send_if_modified(|current| !std::mem::replace(&mut current.live, true));
        }
        IdleEvent::Notice(MailboxNotice::Changed { .. }) => {
            state.send_modify(|current| current.inbox_hints += 1);
        }
        IdleEvent::Notice(MailboxNotice::MailboxListChanged) => {
            state.send_modify(|current| current.list_hints += 1);
        }
        IdleEvent::Notice(_) => {}
    }
}

/// Resolves when the worker is told to stop or has ended.
async fn ended(stop: &watch::Receiver<bool>, state: &watch::Sender<IdleState>) {
    tokio::select! {
        () = stopped(stop.clone()) => {}
        () = state.closed() => {}
    }
}

#[cfg(test)]
mod tests;
