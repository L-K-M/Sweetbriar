//! Correlation for the single-command setup coroutines. The pinned
//! sender ignores tags and replaces coalesced tagged replies. Supply
//! one validated frame per resume and check its tag before delivery.

use io_imap::codec::fragmentizer::Fragmentizer;
use io_imap::coroutine::{ImapCoroutine, ImapCoroutineState, ImapYield};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::connection::write_command;
use crate::error::ImapError;
use crate::gate::{FrameKind, Gate};

pub(crate) struct Outcome<T> {
    pub(crate) result: T,
    pub(crate) reusable: bool,
}

pub(crate) async fn run<W, C>(
    gate: &mut Gate,
    fragmentizer: &mut Fragmentizer,
    wire: &mut W,
    mut coroutine: C,
) -> Result<Outcome<C::Return>, ImapError>
where
    W: AsyncRead + AsyncWrite + Unpin,
    C: ImapCoroutine<Yield = ImapYield>,
{
    let mut arg: Option<Vec<u8>> = None;
    let mut tag: Option<Vec<u8>> = None;
    let mut completed = false;
    let mut bye = false;
    loop {
        match coroutine.resume(fragmentizer, arg.as_deref()) {
            ImapCoroutineState::Complete(result) => {
                return Ok(Outcome {
                    result,
                    reusable: !bye && (tag.is_none() || completed) && gate.reusable(),
                });
            }
            ImapCoroutineState::Yielded(_) if completed || bye => {
                return Err(ImapError::UnexpectedCompletion);
            }
            ImapCoroutineState::Yielded(ImapYield::WantsRead) => {
                let Some(frame) = gate.read_frame(wire).await? else {
                    arg = Some(Vec::new());
                    continue;
                };
                match frame.kind {
                    FrameKind::Completion(reply_tag) => {
                        if tag.as_deref() != Some(reply_tag.as_slice()) {
                            return Err(ImapError::UnexpectedCompletion);
                        }
                        completed = true;
                    }
                    FrameKind::Bye => bye = true,
                    _ => {}
                }
                arg = Some(frame.bytes);
            }
            ImapCoroutineState::Yielded(ImapYield::WantsWrite(bytes)) => {
                if tag.is_none() {
                    if gate.poisoned() {
                        return Err(ImapError::Malformed);
                    }
                    if !gate.reusable() {
                        return Err(ImapError::UnexpectedCompletion);
                    }
                    // Trusted CommandCodec output: its first write starts
                    // with the tag. Further writes can be synchronizing
                    // literal continuations and retain that same tag.
                    tag = Some(bytes.split(|byte| *byte == b' ').next().unwrap().to_vec());
                }
                write_command(wire, &bytes)
                    .await
                    .map_err(ImapError::from_io)?;
                arg = None;
            }
        }
    }
}
