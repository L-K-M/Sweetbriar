//! Setup uses the gate's validated message boundaries directly. Encoding
//! still supplies the literal fragments: only a real continuation permits
//! a synchronizing payload, and the first tagged reply ends the exchange.
use io_imap::codec::{
    CommandCodec, GreetingCodec, ResponseCodec,
    decode::Decoder,
    encode::{Encoder, Fragment},
};
use io_imap::types::{
    ToStatic,
    command::{Command, CommandBody},
    core::{LiteralMode, Tag, TagGenerator},
    response::{Capability, Data, GreetingKind, Response, Status, StatusKind, Tagged},
};
use tokio::io::{AsyncRead, AsyncWrite};

use crate::connection::write_command;
use crate::error::ImapError;
use crate::gate::{FrameKind, Gate, MAX_MESSAGE_SIZE};

pub(crate) struct Outcome<T> {
    pub(crate) result: T,
    pub(crate) reusable: bool,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(crate) enum Authentication {
    Required,
    Preauthenticated,
}

pub(crate) enum Reply {
    Tagged(Tagged<'static>),
    Bye,
    Continuation,
}

impl Reply {
    pub(crate) fn ok(self, operation: &'static str) -> Result<Tagged<'static>, ImapError> {
        match self {
            Self::Tagged(reply) if reply.body.kind == StatusKind::Ok => Ok(reply),
            _ => Err(ImapError::command(operation, "server exchange failed")),
        }
    }

    pub(crate) fn succeeded(&self) -> bool {
        matches!(self, Self::Tagged(reply) if reply.body.kind == StatusKind::Ok)
    }
}

pub(crate) async fn greeting<W: AsyncRead + Unpin>(
    gate: &mut Gate,
    wire: &mut W,
) -> Result<Authentication, ImapError> {
    let frame = gate
        .read_frame(wire)
        .await?
        .ok_or_else(|| ImapError::command("greeting", "connection ended"))?;
    let (rest, greeting) = GreetingCodec::new()
        .decode(&frame.bytes)
        .map_err(|_| ImapError::Malformed)?;
    if !rest.is_empty() {
        return Err(ImapError::Malformed);
    }
    match greeting.kind {
        GreetingKind::Bye => Err(ImapError::command(
            "greeting",
            "server refused the connection",
        )),
        GreetingKind::PreAuth => Ok(Authentication::Preauthenticated),
        GreetingKind::Ok => Ok(Authentication::Required),
    }
}

pub(crate) async fn capability<W: AsyncRead + AsyncWrite + Unpin>(
    gate: &mut Gate,
    wire: &mut W,
) -> Result<Outcome<Vec<Capability<'static>>>, ImapError> {
    let mut capabilities = None;
    let outcome = run(
        gate,
        wire,
        CommandBody::Capability,
        "CAPABILITY",
        |response| {
            if let Response::Data(Data::Capability(values)) = response {
                if capabilities.is_some() {
                    return Err(ImapError::command(
                        "CAPABILITY",
                        "duplicate capability response",
                    ));
                }
                capabilities = Some(values.as_ref().iter().map(ToStatic::to_static).collect());
            }
            Ok(())
        },
    )
    .await?;
    outcome.result.ok("CAPABILITY")?;
    let result = capabilities
        .ok_or_else(|| ImapError::command("CAPABILITY", "missing capability response"))?;
    Ok(Outcome {
        result,
        reusable: outcome.reusable,
    })
}

pub(crate) async fn run<W, F>(
    gate: &mut Gate,
    wire: &mut W,
    body: CommandBody<'static>,
    operation: &'static str,
    mut observe: F,
) -> Result<Outcome<Reply>, ImapError>
where
    W: AsyncRead + AsyncWrite + Unpin,
    F: FnMut(&Response<'_>) -> Result<(), ImapError>,
{
    let mut remaining = MAX_MESSAGE_SIZE as usize;
    // Predecessor frames cannot satisfy a new command. A partial predecessor
    // is finished under the same budget before any command bytes are sent.
    while gate.has_buffered() {
        let frame = gate
            .read_frame(wire)
            .await?
            .ok_or_else(|| ImapError::command(operation, "connection ended"))?;
        charge(&mut remaining, frame.bytes.len(), operation)?;
        if !matches!(frame.kind, FrameKind::Untagged) {
            return Err(ImapError::UnexpectedCompletion);
        }
    }
    let command = Command {
        tag: TagGenerator::new().generate(),
        body,
    };
    for fragment in CommandCodec::new().encode(&command) {
        let bytes = match fragment {
            Fragment::Line { data } => data,
            Fragment::Literal { data, mode } => {
                if mode == LiteralMode::Sync {
                    let reply = receive(
                        gate,
                        wire,
                        &command.tag,
                        operation,
                        &mut remaining,
                        &mut observe,
                    )
                    .await?;
                    match reply {
                        Reply::Continuation => {}
                        Reply::Tagged(ref tagged) if tagged.body.kind == StatusKind::Ok => {
                            return Err(ImapError::UnexpectedCompletion);
                        }
                        result => {
                            return Ok(Outcome {
                                result,
                                reusable: false,
                            });
                        }
                    }
                }
                data
            }
        };
        // Check even after a valid continuation: fill may already know a
        // following fault or terminal reply. No credential crosses it.
        if gate.poisoned() {
            return Err(ImapError::Malformed);
        }
        if !gate.reusable() {
            return Err(ImapError::UnexpectedCompletion);
        }
        write_command(wire, &bytes)
            .await
            .map_err(ImapError::from_io)?;
    }
    let result = receive(
        gate,
        wire,
        &command.tag,
        operation,
        &mut remaining,
        &mut observe,
    )
    .await?;
    if matches!(result, Reply::Continuation) {
        return Err(ImapError::UnexpectedCompletion);
    }
    Ok(Outcome {
        result,
        reusable: gate.reusable(),
    })
}

async fn receive<W, F>(
    gate: &mut Gate,
    wire: &mut W,
    tag: &Tag<'_>,
    operation: &'static str,
    remaining: &mut usize,
    observe: &mut F,
) -> Result<Reply, ImapError>
where
    W: AsyncRead + AsyncWrite + Unpin,
    F: FnMut(&Response<'_>) -> Result<(), ImapError>,
{
    loop {
        let frame = gate
            .read_frame(wire)
            .await?
            .ok_or_else(|| ImapError::command(operation, "connection ended before completion"))?;
        charge(remaining, frame.bytes.len(), operation)?;
        let (rest, response) = ResponseCodec::new()
            .decode(&frame.bytes)
            .map_err(|_| ImapError::Malformed)?;
        if !rest.is_empty() {
            return Err(ImapError::Malformed);
        }
        match response {
            Response::Status(Status::Tagged(reply)) => {
                if reply.tag.inner() != tag.inner() {
                    return Err(ImapError::UnexpectedCompletion);
                }
                return Ok(Reply::Tagged(reply.to_static()));
            }
            Response::Status(Status::Bye(_)) => return Ok(Reply::Bye),
            Response::CommandContinuationRequest(_) => return Ok(Reply::Continuation),
            _ => observe(&response)?,
        }
    }
}

fn charge(remaining: &mut usize, bytes: usize, operation: &'static str) -> Result<(), ImapError> {
    *remaining = remaining
        .checked_sub(bytes)
        .ok_or(ImapError::ResponseBudgetExceeded { operation })?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::time::Duration;
    use tokio::io::{AsyncBufReadExt, AsyncReadExt, AsyncWriteExt, BufReader};

    #[tokio::test]
    async fn an_untagged_notice_cannot_release_a_synchronizing_literal() {
        let (mut client, server) = tokio::io::duplex(1024);
        let exchange = async {
            let mut gate = Gate::new();
            assert!(!gate.read(&mut client).await.unwrap().is_empty());
            let result = run(
                &mut gate,
                &mut client,
                CommandBody::login("synthetic-界".to_string(), "password-界".to_string()).unwrap(),
                "LOGIN",
                |_| Ok(()),
            )
            .await
            .unwrap();
            assert!(result.result.succeeded());
        };
        let peer = async {
            let mut server = BufReader::new(server);
            server.write_all(b"* OK ready\r\n").await.unwrap();
            let mut line = String::new();
            server.read_line(&mut line).await.unwrap();
            assert!(line.ends_with("{13}\r\n"), "{line:?}");
            let tag = line.split_whitespace().next().unwrap().to_string();
            server
                .write_all(b"* OK [ALERT] notice before continuation {3}\r\n")
                .await
                .unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(50), server.read_u8())
                    .await
                    .is_err(),
                "a literal was sent before the continuation request"
            );
            server.write_all(b"+ continue {3}\r\n").await.unwrap();
            let mut literal = vec![0; 13];
            server.read_exact(&mut literal).await.unwrap();
            assert_eq!(literal, "synthetic-界".as_bytes());
            line.clear();
            server.read_line(&mut line).await.unwrap();
            assert_eq!(line, " {12}\r\n");
            server
                .write_all(b"* OK another notice {2}\r\n")
                .await
                .unwrap();
            assert!(
                tokio::time::timeout(Duration::from_millis(50), server.read_u8())
                    .await
                    .is_err(),
                "second literal needs separate permission"
            );
            server.write_all(b"+ password {4}\r\n").await.unwrap();
            let mut password = [0; 12];
            server.read_exact(&mut password).await.unwrap();
            assert_eq!(&password, "password-界".as_bytes());
            line.clear();
            server.read_line(&mut line).await.unwrap();
            assert_eq!(line, "\r\n");
            server
                .write_all(format!("{tag} OK authenticated {{4}}\r\n").as_bytes())
                .await
                .unwrap();
        };
        tokio::time::timeout(Duration::from_secs(2), async {
            tokio::join!(exchange, peer);
        })
        .await
        .expect("both literal permissions and completion arrive");
    }

    #[tokio::test]
    async fn a_known_malformed_frame_prevents_a_literal_write() {
        let (mut client, server) = tokio::io::duplex(1024);
        let exchange = async move {
            let mut gate = Gate::new();
            gate.read(&mut client).await.unwrap();
            let result = run(
                &mut gate,
                &mut client,
                CommandBody::login("synthetic-界".to_string(), "password".to_string()).unwrap(),
                "LOGIN",
                |_| Ok(()),
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
    #[tokio::test]
    async fn setup_response_budget_stops_without_completion_or_eof() {
        let (mut client, server) = tokio::io::duplex(8192);
        let reader = async move {
            let mut gate = Gate::new();
            greeting(&mut gate, &mut client).await.unwrap();
            let result = run(
                &mut gate,
                &mut client,
                CommandBody::Capability,
                "CAPABILITY",
                |_| Ok(()),
            )
            .await;
            assert!(matches!(
                result,
                Err(ImapError::ResponseBudgetExceeded {
                    operation: "CAPABILITY"
                })
            ));
        };
        let peer = async move {
            let mut server = BufReader::new(server);
            server.write_all(b"* OK ready\r\n").await.unwrap();
            let mut command = String::new();
            server.read_line(&mut command).await.unwrap();
            assert!(command.ends_with(" CAPABILITY\r\n"));
            let line = format!("* OK {}\r\n", "x".repeat(8185));
            assert_eq!(line.len(), 8192);
            for _ in 0..(MAX_MESSAGE_SIZE as usize / line.len() + 1) {
                server.write_all(line.as_bytes()).await.unwrap();
            }
            assert_eq!(server.read(&mut [0]).await.unwrap(), 0);
        };
        tokio::time::timeout(Duration::from_secs(30), async {
            tokio::join!(reader, peer);
        })
        .await
        .expect("setup resource fault closes without completion/EOF");
    }
    #[tokio::test]
    async fn capability_needs_one_current_response_not_a_buffered_predecessor() {
        for stale in [false, true] {
            let (mut client, server) = tokio::io::duplex(1024);
            let reader = async move {
                let mut gate = Gate::new();
                greeting(&mut gate, &mut client).await.unwrap();
                let result = capability(&mut gate, &mut client).await;
                assert!(matches!(
                    result,
                    Err(ImapError::Command {
                        operation: "CAPABILITY",
                        ..
                    })
                ));
            };
            let peer = async move {
                let mut server = BufReader::new(server);
                let greeting = if stale {
                    b"* OK ready\r\n* CAPABILITY IMAP4rev1\r\n".as_slice()
                } else {
                    b"* OK ready\r\n".as_slice()
                };
                server.write_all(greeting).await.unwrap();
                let mut command = String::new();
                server.read_line(&mut command).await.unwrap();
                assert!(command.ends_with(" CAPABILITY\r\n"));
                let tag = command.split_whitespace().next().unwrap();
                let rows = if stale {
                    ""
                } else {
                    "* CAPABILITY IMAP4rev1\r\n* CAPABILITY IMAP4rev1 ID\r\n"
                };
                server
                    .write_all(format!("{rows}{tag} OK complete\r\n").as_bytes())
                    .await
                    .unwrap();
                assert_eq!(server.read(&mut [0]).await.unwrap(), 0);
            };
            tokio::time::timeout(Duration::from_secs(2), async {
                tokio::join!(reader, peer);
            })
            .await
            .expect("invalid capability evidence closes");
        }
    }
}
