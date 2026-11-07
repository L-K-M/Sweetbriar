//! Correlation for the single-command setup coroutines. The pinned
//! sender ignores tags and replaces coalesced tagged replies. Supply
//! one validated frame per resume and check its tag before delivery.

use io_imap::codec::fragmentizer::Fragmentizer;
use io_imap::coroutine::{ImapCoroutine, ImapCoroutineState, ImapYield};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::connection::write_command;
use crate::error::ImapError;
use crate::gate::{FrameKind, Gate, MAX_MESSAGE_SIZE};

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
    let mut awaiting_continuation = false;
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
                let mut notices = Vec::new();
                loop {
                    let Some(frame) = gate.read_frame(wire).await? else {
                        arg = Some(Vec::new());
                        break;
                    };
                    // ImapSend also serializes its pending literal after an
                    // untagged response, even without a continuation. Keep
                    // notices together with the actual '+' (or refusal), so
                    // neither socket-read boundaries nor notices grant it
                    // permission to send literal bytes. Bound this setup-only
                    // prefix by the same budget as a setup response frame.
                    if awaiting_continuation && matches!(frame.kind, FrameKind::Untagged) {
                        if notices.len() + frame.bytes.len() > MAX_MESSAGE_SIZE as usize {
                            return Err(ImapError::command("setup", "response budget exceeded"));
                        }
                        notices.extend_from_slice(&frame.bytes);
                        continue;
                    }
                    match frame.kind {
                        FrameKind::Completion(reply_tag) => {
                            if tag.as_deref() != Some(reply_tag.as_slice()) {
                                return Err(ImapError::UnexpectedCompletion);
                            }
                            completed = true;
                        }
                        FrameKind::Bye => bye = true,
                        FrameKind::Continuation => awaiting_continuation = false,
                        _ => {}
                    }
                    notices.extend_from_slice(&frame.bytes);
                    arg = Some(notices);
                    break;
                }
            }
            ImapCoroutineState::Yielded(ImapYield::WantsWrite(bytes)) => {
                // The gate may have observed a fault after the valid
                // continuation it just released. Check every write,
                // including literal payloads, before touching the socket.
                if gate.poisoned() {
                    return Err(ImapError::Malformed);
                }
                if !gate.reusable() {
                    return Err(ImapError::UnexpectedCompletion);
                }
                if tag.is_none() {
                    // Trusted CommandCodec output: its first write starts
                    // with the tag. Further writes can be synchronizing
                    // literal continuations and retain that same tag.
                    tag = Some(bytes.split(|byte| *byte == b' ').next().unwrap().to_vec());
                }
                write_command(wire, &bytes)
                    .await
                    .map_err(ImapError::from_io)?;
                awaiting_continuation = announces_sync_literal(&bytes);
                arg = None;
            }
        }
    }
}

/// Trusted CommandCodec chunks end with `{n}\r\n` when a synchronizing
/// literal follows. Non-synchronizing `{n+}` is deliberately excluded.
fn announces_sync_literal(bytes: &[u8]) -> bool {
    let Some(prefix) = bytes.strip_suffix(b"}\r\n") else {
        return false;
    };
    let Some(open) = prefix.iter().rposition(|byte| *byte == b'{') else {
        return false;
    };
    let length = &prefix[open + 1..];
    !length.is_empty() && length.iter().all(u8::is_ascii_digit)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::gate::MAX_MESSAGE_SIZE;
    use io_imap::rfc3501::login::{ImapLogin, ImapLoginOptions};
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    #[tokio::test]
    async fn an_untagged_notice_cannot_release_a_synchronizing_literal() {
        let (mut client, server) = tokio::io::duplex(1024);
        let exchange = async {
            let mut gate = Gate::new();
            assert!(!gate.read(&mut client).await.unwrap().is_empty());
            let mut fragmentizer = Fragmentizer::new(MAX_MESSAGE_SIZE);
            let result = run(
                &mut gate,
                &mut fragmentizer,
                &mut client,
                ImapLogin::new("synthetic-界", "password", ImapLoginOptions::default()).unwrap(),
            )
            .await
            .unwrap();
            assert!(result.result.is_ok());
        };
        let peer = async {
            let mut server = BufReader::new(server);
            server.write_all(b"* OK ready\r\n").await.unwrap();
            let mut line = String::new();
            server.read_line(&mut line).await.unwrap();
            assert!(line.ends_with("{13}\r\n"), "{line:?}");
            let tag = line.split_whitespace().next().unwrap().to_string();
            server
                .write_all(b"* OK [ALERT] notice before continuation\r\n")
                .await
                .unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(50), server.read_u8())
                    .await
                    .is_err(),
                "a literal was sent before the continuation request"
            );
            server.write_all(b"+ continue\r\n").await.unwrap();
            let mut literal = vec![0; 13];
            server.read_exact(&mut literal).await.unwrap();
            assert_eq!(literal, "synthetic-界".as_bytes());
            line.clear();
            server.read_line(&mut line).await.unwrap();
            assert_eq!(line, " password\r\n");
            server
                .write_all(format!("{tag} OK authenticated\r\n").as_bytes())
                .await
                .unwrap();
        };
        tokio::join!(exchange, peer);
    }

    #[tokio::test]
    async fn a_known_malformed_frame_prevents_a_literal_write() {
        let (mut client, server) = tokio::io::duplex(1024);
        let exchange = async move {
            let mut gate = Gate::new();
            gate.read(&mut client).await.unwrap();
            let mut fragmentizer = Fragmentizer::new(MAX_MESSAGE_SIZE);
            let result = run(
                &mut gate,
                &mut fragmentizer,
                &mut client,
                ImapLogin::new("synthetic-界", "password", ImapLoginOptions::default()).unwrap(),
            )
            .await;
            assert!(matches!(result, Err(ImapError::Malformed)));
        };
        let peer = async {
            let mut server = BufReader::new(server);
            server.write_all(b"* OK ready\r\n").await.unwrap();
            let mut line = String::new();
            server.read_line(&mut line).await.unwrap();
            assert!(line.ends_with("{13}\r\n"));
            // Both messages are in the same read, so the gate knows the
            // fault before it releases the valid continuation response.
            server
                .write_all(b"+ continue\r\nnot an IMAP response\r\n")
                .await
                .unwrap();
            let mut byte = [0];
            let count = tokio::time::timeout(Duration::from_secs(2), server.read(&mut byte))
                .await
                .expect("session closes")
                .unwrap();
            assert_eq!(
                count, 0,
                "literal bytes were sent after a known framing fault"
            );
        };
        tokio::join!(exchange, peer);
    }
}
