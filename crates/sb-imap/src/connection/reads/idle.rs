//! Correlated selected-mailbox IDLE (RFC 2177). Only reconciliation hints
//! escape; the wire remains owned until graceful DONE and matching OK.

use std::future::{Future, poll_fn};
use std::pin::Pin;
use std::task::Poll;
use std::time::Duration;

use io_imap::codec::{ResponseCodec, decode::Decoder};
use io_imap::types::{
    command::{Command, CommandBody},
    core::TagGenerator,
    response::{Capability, Response, Status, StatusKind},
};
use tokio::io::{AsyncRead, AsyncWrite};
use tokio::time::Sleep;

use super::{
    Connection, MAX_METADATA_BYTES, MailboxContext, MailboxNotice, ReadKind, charge,
    check_selected_epoch, notice, write_command,
};
use crate::ImapError;
use crate::gate::{Frame, FrameKind, Gate};

const RENEW_AFTER: Duration = Duration::from_secs(25 * 60);

/// Borrowed watcher events; repeated hints reuse a bounded cache rather than
/// copying the selected mailbox's name on every unsolicited update.
/// Retain an owned hint only when needed, in bounded coalesced state:
/// ```no_run
/// # use sb_imap::{Connection, IdleEvent, MailboxNotice, ImapError};
/// # async fn retain(connection: &mut Connection) -> Result<(), ImapError> {
/// let mut last: Option<MailboxNotice> = None;
/// connection.watch_idle(std::future::pending(), |event| {
///     if let IdleEvent::Notice(hint) = event { last = Some(hint.clone()); }
/// }).await?;
/// # Ok(()) }
/// ```
/// A borrowed hint cannot escape the callback:
/// ```compile_fail,E0521
/// # use sb_imap::{Connection, IdleEvent, MailboxNotice, ImapError};
/// # async fn retain(connection: &mut Connection) -> Result<(), ImapError> {
/// let mut last: Option<&MailboxNotice> = None;
/// connection.watch_idle(std::future::pending(), |event| {
///     if let IdleEvent::Notice(hint) = event { last = Some(hint); }
/// }).await?;
/// # Ok(()) }
/// ```
#[derive(Debug)]
pub enum IdleEvent<'a> {
    /// A real continuation and the complete received tail were validated.
    /// Emitted once for the first IDLE and once for each accepted renewal.
    Started {
        /// The read-only selection whose IDLE continuation was accepted.
        mailbox: &'a MailboxContext,
    },
    /// A reconciliation hint, never authoritative flags or disappearance.
    Notice(&'a MailboxNotice),
}

impl Connection {
    /// Watch the selected mailbox, renewing IDLE every 25 minutes. The
    /// callback must return promptly, for example by coalescing work to queue.
    /// No raw server text or authoritative row observations are delivered.
    ///
    /// `stop` requests graceful DONE and a checked completion, bounded by a
    /// 30-second exit deadline. An outstanding entry can take up to its own
    /// 30-second deadline before DONE is permitted. Dropping this future
    /// closes the socket immediately; errors also close it. Only successful
    /// graceful stopping leaves the selected connection reusable.
    pub async fn watch_idle<F, N>(&mut self, stop: F, mut notify: N) -> Result<(), ImapError>
    where
        F: Future<Output = ()>,
        N: FnMut(IdleEvent<'_>),
    {
        let context = self.uid_context()?;
        if !self
            .capabilities
            .iter()
            .any(|cap| matches!(cap, Capability::Idle))
        {
            return Err(ImapError::IdleUnavailable);
        }
        let mut stop = std::pin::pin!(stop);
        if poll_fn(|cx| Poll::Ready(stop.as_mut().poll(cx).is_ready())).await {
            return Ok(());
        }
        let mut wire = self.wire.take().ok_or(ImapError::Closed)?;
        let pending = std::mem::take(&mut self.pending_notices);
        for hint in &pending {
            notify(IdleEvent::Notice(hint));
        }
        drop(pending);
        watch(
            &mut self.gate,
            &mut wire,
            &context,
            stop,
            &mut notify,
            MAX_METADATA_BYTES,
        )
        .await?;
        self.wire = Some(wire);
        Ok(())
    }
}

async fn watch<W, F, N>(
    gate: &mut Gate,
    wire: &mut W,
    context: &MailboxContext,
    mut stop: Pin<&mut F>,
    notify: &mut N,
    budget: usize,
) -> Result<(), ImapError>
where
    W: AsyncRead + AsyncWrite + Unpin,
    F: Future<Output = ()>,
    N: FnMut(IdleEvent<'_>),
{
    let mut tags = TagGenerator::new();
    let mut cache = Vec::new();
    loop {
        let command = Command {
            tag: tags.generate(),
            body: CommandBody::Idle,
        };
        let mut remaining = budget;
        crate::timeout::command(
            "IDLE",
            enter(
                gate,
                wire,
                context,
                &command,
                &mut remaining,
                &mut cache,
                notify,
            ),
        )
        .await?;
        notify(IdleEvent::Started { mailbox: context });
        let mut renewal = std::pin::pin!(tokio::time::sleep(RENEW_AFTER));
        let stopped = loop {
            match next_event(gate, wire, &mut remaining, stop.as_mut(), renewal.as_mut()).await? {
                Wake::Frame(frame) => observe(&frame, context, &mut cache, notify)?,
                Wake::Stop => break true,
                Wake::Renew => break false,
            }
        };
        crate::timeout::command(
            "IDLE",
            finish(
                gate,
                wire,
                context,
                &command,
                &mut remaining,
                &mut cache,
                notify,
            ),
        )
        .await?;
        if stopped {
            return Ok(());
        }
    }
}

async fn read<W: AsyncRead + Unpin>(
    gate: &mut Gate,
    wire: &mut W,
    remaining: &mut usize,
) -> Result<Frame, ImapError> {
    let frame = gate.read_frame(wire).await?.ok_or(ImapError::Closed)?;
    if gate.poisoned() {
        return Err(ImapError::Malformed);
    }
    charge(remaining, frame.bytes.len(), ReadKind::Idle)?;
    Ok(frame)
}

fn checked<'a>(frame: &'a Frame, context: &MailboxContext) -> Result<Response<'a>, ImapError> {
    let (rest, response) = ResponseCodec::new()
        .decode(&frame.bytes)
        .map_err(|_| ImapError::Malformed)?;
    if !rest.is_empty() {
        return Err(ImapError::Malformed);
    }
    check_selected_epoch(&response, Some(context), ReadKind::Idle)?;
    Ok(response)
}

fn observe<N: FnMut(IdleEvent<'_>)>(
    frame: &Frame,
    context: &MailboxContext,
    cache: &mut Vec<MailboxNotice>,
    notify: &mut N,
) -> Result<(), ImapError> {
    if !matches!(frame.kind, FrameKind::Untagged) {
        return Err(ImapError::UnexpectedCompletion);
    }
    let response = checked(frame, context)?;
    if let Some(index) = notice(&response, Some(context), cache)? {
        notify(IdleEvent::Notice(&cache[index]));
    }
    Ok(())
}

async fn drain<W, N>(
    gate: &mut Gate,
    wire: &mut W,
    context: &MailboxContext,
    remaining: &mut usize,
    cache: &mut Vec<MailboxNotice>,
    notify: &mut N,
) -> Result<(), ImapError>
where
    W: AsyncRead + Unpin,
    N: FnMut(IdleEvent<'_>),
{
    if gate.poisoned() {
        return Err(ImapError::Malformed);
    }
    while gate.has_buffered() {
        let frame = read(gate, wire, remaining).await?;
        observe(&frame, context, cache, notify)?;
    }
    Ok(())
}

fn tail<N: FnMut(IdleEvent<'_>)>(
    gate: &mut Gate,
    context: &MailboxContext,
    remaining: &mut usize,
    cache: &mut Vec<MailboxNotice>,
    notify: &mut N,
) -> Result<(), ImapError> {
    for frame in gate.take_received()? {
        charge(remaining, frame.bytes.len(), ReadKind::Idle)?;
        observe(&frame, context, cache, notify)?;
    }
    Ok(())
}

async fn enter<W, N>(
    gate: &mut Gate,
    wire: &mut W,
    context: &MailboxContext,
    command: &Command<'_>,
    remaining: &mut usize,
    cache: &mut Vec<MailboxNotice>,
    notify: &mut N,
) -> Result<(), ImapError>
where
    W: AsyncRead + AsyncWrite + Unpin,
    N: FnMut(IdleEvent<'_>),
{
    drain(gate, wire, context, remaining, cache, notify).await?;
    write_command(wire, &crate::metadata_encoding::encode(command)?)
        .await
        .map_err(ImapError::from_io)?;
    loop {
        let frame = read(gate, wire, remaining).await?;
        match checked(&frame, context)? {
            Response::CommandContinuationRequest(_) => {
                return tail(gate, context, remaining, cache, notify);
            }
            Response::Status(Status::Tagged(reply)) => {
                if reply.tag.inner() == command.tag.inner() && reply.body.kind != StatusKind::Ok {
                    return Err(ImapError::command("IDLE", "server refused the command"));
                }
                return Err(ImapError::UnexpectedCompletion);
            }
            _ => observe(&frame, context, cache, notify)?,
        }
    }
}

async fn finish<W, N>(
    gate: &mut Gate,
    wire: &mut W,
    context: &MailboxContext,
    command: &Command<'_>,
    remaining: &mut usize,
    cache: &mut Vec<MailboxNotice>,
    notify: &mut N,
) -> Result<(), ImapError>
where
    W: AsyncRead + AsyncWrite + Unpin,
    N: FnMut(IdleEvent<'_>),
{
    // Complete any received predecessor before writing DONE. A partial early
    // tagged reply cannot turn into the command's completion after the write.
    drain(gate, wire, context, remaining, cache, notify).await?;
    write_command(wire, b"DONE\r\n")
        .await
        .map_err(ImapError::from_io)?;
    loop {
        let frame = read(gate, wire, remaining).await?;
        match checked(&frame, context)? {
            Response::Status(Status::Tagged(reply)) => {
                if reply.tag.inner() != command.tag.inner() {
                    return Err(ImapError::UnexpectedCompletion);
                }
                if reply.body.kind != StatusKind::Ok {
                    return Err(ImapError::command("IDLE", "server refused DONE"));
                }
                return tail(gate, context, remaining, cache, notify);
            }
            _ => observe(&frame, context, cache, notify)?,
        }
    }
}

enum Wake {
    Frame(Frame),
    Stop,
    Renew,
}

async fn next_event<W, F>(
    gate: &mut Gate,
    wire: &mut W,
    remaining: &mut usize,
    mut stop: Pin<&mut F>,
    mut renewal: Pin<&mut Sleep>,
) -> Result<Wake, ImapError>
where
    W: AsyncRead + Unpin,
    F: Future<Output = ()>,
{
    let mut incoming = std::pin::pin!(read(gate, wire, remaining));
    poll_fn(|cx| {
        // Stop and renewal cannot starve under a continuous ready-frame flood.
        // The exit drain validates already-received bytes before DONE is sent.
        if stop.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Ok(Wake::Stop));
        }
        if renewal.as_mut().poll(cx).is_ready() {
            return Poll::Ready(Ok(Wake::Renew));
        }
        incoming
            .as_mut()
            .poll(cx)
            .map(|result| result.map(Wake::Frame))
    })
    .await
}

#[cfg(test)]
mod tests;
