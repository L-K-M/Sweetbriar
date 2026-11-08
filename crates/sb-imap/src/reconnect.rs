//! Fresh owned connection attempts after a typed operation failure.

use std::fmt;
use std::future::{Future, poll_fn};
use std::io::ErrorKind;
use std::pin::Pin;
use std::task::Poll;
use std::time::Duration;

use crate::{Connection, ImapError};

const FIRST_DELAY: Duration = Duration::from_secs(1);
const MAX_DELAY: Duration = Duration::from_secs(5 * 60);

/// One attempt owns its transport and credentials until completion or drop.
pub type ConnectAttempt<'a> =
    Pin<Box<dyn Future<Output = Result<Connection, ImapError>> + Send + 'a>>;

/// A source of fresh sessions, normally backed by the account's secret store.
/// Fetch credentials anew per attempt. The returned future must own all work:
/// dropping it must release credentials and sockets without a background task.
pub trait Connector: Send {
    /// Open a new authenticated session; never reuse a failed connection.
    fn connect(&mut self) -> ConnectAttempt<'_>;
}

/// A bounded retry wait. Borrowed causes cannot outlive this notification.
#[derive(Debug)]
pub struct ReconnectWait<'a> {
    /// Delay before the next fresh attempt, between one second and five minutes.
    pub delay: Duration,
    /// Typed failure; callers classify the variant rather than formatted text.
    pub cause: &'a ImapError,
}

/// Reconnection stopped without returning a live session.
#[derive(Debug)]
pub enum ReconnectError {
    /// The caller stopped during a wait or connection attempt.
    Cancelled,
    /// Security, configuration, protocol or unknown failure needs caller action.
    Paused {
        /// The original typed cause, without retrying it automatically.
        cause: ImapError,
    },
}

impl fmt::Display for ReconnectError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Cancelled => f.write_str("IMAP reconnection cancelled"),
            Self::Paused { cause } => write!(f, "IMAP reconnection paused: {cause}"),
        }
    }
}

impl std::error::Error for ReconnectError {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        match self {
            Self::Cancelled => None,
            Self::Paused { cause } => Some(cause),
        }
    }
}

/// Cancellable exponential backoff. This state stores no credentials, options
/// or failed transport. Initial setup still uses [`Connection::connect`].
#[derive(Debug)]
pub struct Reconnector {
    next_delay: Duration,
}

impl Default for Reconnector {
    fn default() -> Self {
        Self::new()
    }
}

impl Reconnector {
    /// Start with a one-second wait; successful connection resets this wait.
    pub fn new() -> Self {
        Self {
            next_delay: FIRST_DELAY,
        }
    }

    /// Recover from an operation failure through fresh connector attempts.
    /// Closed sessions, timeouts and transient network I/O retry. Other typed
    /// failures pause for caller action, including certificate and LOGIN errors.
    /// Every failure doubles the next delay up to five minutes; only success
    /// resets it. `notify` must return promptly and may coalesce status work.
    ///
    /// A ready `stop` prevents even a connector call. Stopping or dropping this
    /// future drops an in-flight attempt before returning; the connector must
    /// obey its ownership contract. A returned session belongs to the caller.
    pub async fn retry<C, F, N>(
        &mut self,
        mut failure: ImapError,
        connector: &mut C,
        stop: F,
        mut notify: N,
    ) -> Result<Connection, ReconnectError>
    where
        C: Connector + ?Sized,
        F: Future<Output = ()>,
        N: FnMut(ReconnectWait<'_>),
    {
        let mut stop = std::pin::pin!(stop);
        loop {
            if poll_fn(|cx| Poll::Ready(stop.as_mut().poll(cx).is_ready())).await {
                return Err(ReconnectError::Cancelled);
            }
            if !retryable(&failure) {
                return Err(ReconnectError::Paused { cause: failure });
            }
            let delay = self.next_delay;
            self.next_delay = delay.saturating_mul(2).min(MAX_DELAY);
            notify(ReconnectWait {
                delay,
                cause: &failure,
            });
            drop(failure);
            cancellable(stop.as_mut(), tokio::time::sleep(delay)).await?;
            // The scoped future releases its mutable connector borrow, socket
            // and credentials before another wait or attempt can begin.
            let result = cancellable(stop.as_mut(), async { connector.connect().await }).await?;
            match result {
                Ok(connection) => {
                    self.next_delay = FIRST_DELAY;
                    return Ok(connection);
                }
                Err(error) => failure = error,
            }
        }
    }
}

fn retryable(error: &ImapError) -> bool {
    match error {
        ImapError::Closed | ImapError::TimedOut { .. } => true,
        ImapError::Io { kind, .. } => matches!(
            kind,
            ErrorKind::ConnectionRefused
                | ErrorKind::ConnectionReset
                | ErrorKind::ConnectionAborted
                | ErrorKind::NotConnected
                | ErrorKind::AddrInUse
                | ErrorKind::AddrNotAvailable
                | ErrorKind::BrokenPipe
                | ErrorKind::WriteZero
                | ErrorKind::UnexpectedEof
                | ErrorKind::TimedOut
                | ErrorKind::Interrupted
                | ErrorKind::WouldBlock
                | ErrorKind::NetworkDown
                | ErrorKind::NetworkUnreachable
                | ErrorKind::HostUnreachable
        ),
        _ => false,
    }
}

async fn cancellable<S: Future<Output = ()>, A: Future>(
    mut stop: Pin<&mut S>,
    attempt: A,
) -> Result<A::Output, ReconnectError> {
    let mut attempt = std::pin::pin!(attempt);
    poll_fn(|cx| {
        if stop.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Err(ReconnectError::Cancelled));
        }
        attempt.as_mut().poll(cx).map(Ok)
    })
    .await
}

#[cfg(test)]
mod tests;
