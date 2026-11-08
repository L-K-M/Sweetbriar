//! Full-body command correlation. The future owns the socket and unpublished
//! sink until the first matched OK and all already-received tail checks pass.

use super::{
    Connection, MAX_METADATA_BYTES, MailboxContext, MailboxNotice, ReadKind, ReadObservation,
    changed, charge, check_selected_epoch, notice, write_command,
};
use crate::body_framing::{BodyEvidence, BodyReader};
use crate::gate::{FrameKind, Gate};
use crate::timeout::BodySizeHint;
use crate::{BodyUnavailableReason, BodyValue, FetchedBody, ImapError};
use io_imap::codec::{ResponseCodec, decode::Decoder};
use io_imap::types::{
    command::{Command, CommandBody},
    core::TagGenerator,
    fetch::MessageDataItemName,
    response::{Response, Status, StatusKind},
    sequence::{SeqOrUid, Sequence},
};
use std::num::NonZeroU32;
use tokio::io::{AsyncRead, AsyncWrite};

impl Connection {
    /// Stream one explicit UID's full BODY.PEEK[] into an owned private target.
    /// Only a matched successful completion returns the target. Errors and
    /// cancellation drop it and close the connection. The caller must keep
    /// it unpublished and owns flushing, verification and durable publication.
    /// NIL and a missing frame remain distinct from a genuine empty body.
    pub async fn fetch_body<S: AsyncWrite + Unpin>(
        &mut self,
        uid: NonZeroU32,
        sink: S,
    ) -> Result<ReadObservation<FetchedBody<S>>, ImapError> {
        let context = self.uid_context()?;
        let mut wire = self.wire.take().ok_or(ImapError::Closed)?;
        let mut notices = std::mem::take(&mut self.pending_notices);
        let size_hint = BodySizeHint::default();
        let (body, observed) = crate::timeout::body(
            size_hint.clone(),
            body_exchange(
                &mut self.gate,
                &context,
                &mut wire,
                uid,
                sink,
                MAX_METADATA_BYTES,
                size_hint,
            ),
        )
        .await?;
        for notice in observed {
            if !notices.contains(&notice) {
                notices.push(notice);
            }
        }
        self.wire = Some(wire);
        Ok(ReadObservation {
            observation: FetchedBody {
                mailbox: context,
                uid,
                body,
            },
            notices,
        })
    }
}

async fn body_exchange<W, S>(
    gate: &mut Gate,
    context: &MailboxContext,
    wire: &mut W,
    uid: NonZeroU32,
    sink: S,
    mut remaining: usize,
    size_hint: BodySizeHint,
) -> Result<(BodyValue<S>, Vec<MailboxNotice>), ImapError>
where
    W: AsyncRead + AsyncWrite + Unpin,
    S: AsyncWrite + Unpin,
{
    let kind = ReadKind::BodyFetch;
    let mut notices = Vec::new();
    let codec = ResponseCodec::new();
    if gate.poisoned() {
        return Err(ImapError::Malformed);
    }
    // Predecessors are reconciliation hints only, including a partial
    // frame completed after this future starts. Never stream them to sink.
    while gate.has_buffered() {
        let frame = gate.read_frame(wire).await?.ok_or(ImapError::Closed)?;
        if gate.poisoned() {
            return Err(ImapError::Malformed);
        }
        charge(&mut remaining, frame.bytes.len(), kind)?;
        if !matches!(frame.kind, FrameKind::Untagged) {
            return Err(ImapError::UnexpectedCompletion);
        }
        let (_, response) = codec
            .decode(&frame.bytes)
            .map_err(|_| ImapError::Malformed)?;
        check_selected_epoch(&response, Some(context), kind)?;
        notice(&response, Some(context), &mut notices)?;
    }
    let command = Command {
        tag: TagGenerator::new().generate(),
        body: CommandBody::fetch(
            vec![Sequence::Single(SeqOrUid::Value(uid))],
            vec![
                MessageDataItemName::Uid,
                MessageDataItemName::BodyExt {
                    section: None,
                    partial: None,
                    peek: true,
                },
            ],
            true,
        )
        .expect("one explicit UID and full BODY.PEEK[]"),
    };
    let bytes = crate::metadata_encoding::encode(&command)?;
    write_command(wire, &bytes)
        .await
        .map_err(ImapError::from_io)?;
    let mut reader = BodyReader::with_size_hint(uid, sink, size_hint);
    let mut evidence = None;
    loop {
        let frame = reader.next(wire).await?.ok_or_else(|| {
            ImapError::command(kind.operation(), "connection ended before completion")
        })?;
        // BodyReader validates a short placeholder, but quotas count the
        // original announcement/quotes and exclude only the actual payload.
        charge(&mut remaining, frame.metadata_bytes, kind)?;
        let (_, response) = codec
            .decode(&frame.frame.bytes)
            .map_err(|_| ImapError::Malformed)?;
        check_selected_epoch(&response, Some(context), kind)?;
        match &response {
            Response::Status(Status::Tagged(reply)) => {
                if reply.tag.inner() != command.tag.inner() {
                    return Err(ImapError::UnexpectedCompletion);
                }
                if reply.body.kind != StatusKind::Ok {
                    return Err(ImapError::command(
                        kind.operation(),
                        "server refused the command",
                    ));
                }
                break;
            }
            Response::Status(Status::Bye(_)) | Response::CommandContinuationRequest(_) => {
                return Err(ImapError::UnexpectedCompletion);
            }
            _ => {
                if frame.body.is_some() {
                    evidence = frame.body;
                } else {
                    notice(&response, Some(context), &mut notices)?;
                }
            }
        }
    }
    let (sink, tail) = reader.into_parts()?;
    // Do not wait for an incomplete tail after OK. Retain it for the next
    // pre-write drain, while checking every complete received frame now.
    for frame in gate.ingest_received(&tail)? {
        charge(&mut remaining, frame.bytes.len(), kind)?;
        let (_, response) = codec
            .decode(&frame.bytes)
            .map_err(|_| ImapError::Malformed)?;
        check_selected_epoch(&response, Some(context), kind)?;
        notice(&response, Some(context), &mut notices)?;
    }
    let body = match evidence {
        Some(BodyEvidence::Bytes(bytes)) => BodyValue::Complete { sink, bytes },
        unavailable => {
            drop(sink);
            changed(Some(context), &mut notices);
            BodyValue::Unavailable {
                reason: match unavailable {
                    Some(BodyEvidence::Unavailable) => BodyUnavailableReason::ServerNil,
                    None => BodyUnavailableReason::NoFrame,
                    Some(BodyEvidence::Bytes(_)) => unreachable!(),
                },
            }
        }
    };
    Ok((body, notices))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    #[tokio::test]
    async fn original_framing_exhausts_the_command_quota_before_tag_or_eof() {
        let (mut client, server) = tokio::io::duplex(8192);
        let mut frame = b"* 1 FETCH (BODY[] {123+}\r\n".to_vec();
        frame.extend_from_slice(&[b'x'; 123]);
        frame.extend_from_slice(b" UID 7)\r\n");
        // The {0} codec placeholder would fit, but the actual {123+}
        // announcement must not. The peer holds the stream open with no OK.
        let budget = frame.len() - 123 - 1;
        let peer = tokio::spawn(async move {
            let mut server = BufReader::new(server);
            server.write_all(b"* OK ready\r\n").await.unwrap();
            let mut command = String::new();
            server.read_line(&mut command).await.unwrap();
            assert!(command.contains("UID FETCH 7 (UID BODY.PEEK[])"));
            server.write_all(&frame).await.unwrap();
            std::future::pending::<()>().await;
        });
        let mut gate = Gate::new();
        gate.read(&mut client).await.unwrap();
        let context = MailboxContext {
            name: "INBOX".into(),
            uid_validity: NonZeroU32::new(4100).unwrap(),
        };
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(2),
            body_exchange(
                &mut gate,
                &context,
                &mut client,
                NonZeroU32::new(7).unwrap(),
                Vec::<u8>::new(),
                budget,
                BodySizeHint::default(),
            ),
        )
        .await;
        peer.abort();
        assert!(matches!(
            result,
            Ok(Err(ImapError::ResponseBudgetExceeded {
                operation: "UID FETCH"
            }))
        ));
    }
}
