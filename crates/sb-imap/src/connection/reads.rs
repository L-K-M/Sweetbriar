//! Complete metadata observations. The setup coroutines are deliberately
//! not used: LIST must not apply their lossy name conversion, and a new
//! selection cannot inherit buffered values from an older mailbox.

mod uids;

use std::collections::BTreeSet;
use std::num::NonZeroU32;

use io_imap::codec::{CommandCodec, ResponseCodec, decode::Decoder, encode::Encoder};
use io_imap::types::ToStatic;
use io_imap::types::command::{Command, CommandBody};
use io_imap::types::core::TagGenerator;
use io_imap::types::response::{Capability, Code, Data, Response, Status, StatusKind};
use tokio::io::{AsyncRead, AsyncWrite};

use super::{Connection, write_command};
use crate::error::ImapError;
use crate::gate::{Frame, FrameKind, Gate};
use crate::mailbox::{ExamineBuilder, MailboxSelection, MailboxSummary, list_row};

// Metadata remains bounded independently of b3's larger body-literal budget.
const MAX_METADATA_BYTES: usize = 50 * 1024 * 1024;

/// The exact selection to which an unsolicited update belongs.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailboxContext {
    /// Strictly decoded mailbox name; INBOX uses its canonical spelling.
    pub name: String,
    /// UIDVALIDITY established by a complete EXAMINE.
    pub uid_validity: NonZeroU32,
}

/// Coalesced reconciliation triggers. These are never a row journal or
/// evidence that an occurrence disappeared (spec/04).
#[derive(Clone, Debug, PartialEq, Eq)]
pub enum MailboxNotice {
    /// Refresh this selection. None means the update could not safely be
    /// attributed, for example while switching mailboxes.
    Changed {
        /// The previous selection, or None for an unattributed update.
        mailbox: Option<MailboxContext>,
    },
    /// Refresh mailbox discovery, including roles and delimiters.
    MailboxListChanged,
    /// Refresh capabilities before evaluating a provider policy.
    CapabilitiesChanged,
    /// The server reported an alert. Raw server text is not propagated.
    Alert,
    /// The server reported a nonfatal untagged NO or BAD status.
    Warning,
}

/// A complete command observation and the unsolicited reconciliation
/// triggers encountered before its first matched tagged completion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReadObservation<T> {
    /// Data belonging to this command, never a buffered predecessor.
    pub observation: T,
    /// Coalesced notices, with the previous selection context where known.
    pub notices: Vec<MailboxNotice>,
}

#[derive(Clone, Copy)]
enum ReadKind {
    List,
    Examine,
    Unselect,
    UidSearch,
    FlagSweep,
    SelectedCapability,
}

impl ReadKind {
    fn operation(self) -> &'static str {
        match self {
            Self::List => "LIST",
            Self::Examine => "EXAMINE",
            Self::Unselect => "UNSELECT",
            Self::UidSearch => "UID SEARCH",
            Self::FlagSweep => "UID FETCH",
            Self::SelectedCapability => "CAPABILITY",
        }
    }

    fn needs_selection(self) -> bool {
        matches!(
            self,
            Self::UidSearch | Self::FlagSweep | Self::SelectedCapability
        )
    }
}

impl Connection {
    pub(super) async fn selected_capability(
        &mut self,
    ) -> Result<Vec<Capability<'static>>, ImapError> {
        let mut capabilities = None;
        let notices = self
            .read_metadata(
                CommandBody::Capability,
                ReadKind::SelectedCapability,
                |response| {
                    if let Response::Data(Data::Capability(values)) = response {
                        if capabilities.is_some() {
                            return Err(ImapError::command(
                                "CAPABILITY",
                                "duplicate capability response",
                            ));
                        }
                        capabilities =
                            Some(values.as_ref().iter().map(ToStatic::to_static).collect());
                    }
                    Ok(())
                },
            )
            .await?;
        let Some(capabilities) = capabilities else {
            self.wire = None;
            return Err(ImapError::command(
                "CAPABILITY",
                "missing capability response",
            ));
        };
        self.pending_notices = notices;
        Ok(capabilities)
    }

    /// LIST all mailbox names. No provider roles are interpreted here.
    pub async fn list_mailboxes(
        &mut self,
    ) -> Result<ReadObservation<Vec<MailboxSummary>>, ImapError> {
        let body = CommandBody::list(String::new(), "*".to_string()).expect("static LIST");
        let mut rows = Vec::new();
        let mut names = BTreeSet::new();
        let notices = self
            .read_metadata(body, ReadKind::List, |response| {
                let Response::Data(data) = response else {
                    return Ok(());
                };
                let Some(row) = list_row(data)? else {
                    return Ok(());
                };
                if !names.insert(row.name.clone()) {
                    return Err(ImapError::command("LIST", "duplicate mailbox name"));
                }
                rows.push(row);
                Ok(())
            })
            .await?;
        Ok(ReadObservation {
            observation: rows,
            notices,
        })
    }

    /// Establish a read-only selection with its own checked epoch. Counts
    /// describe this observation; only a later complete UID inventory can
    /// supply disappearance evidence. Missing newer codes remain unknown.
    /// Selecting again uses UNSELECT. When it is unavailable, this closes
    /// the session and returns [`ImapError::FreshConnectionRequired`].
    pub async fn examine(
        &mut self,
        name: &str,
    ) -> Result<ReadObservation<MailboxSelection>, ImapError> {
        if name.is_empty() || name.len() > MAX_METADATA_BYTES {
            return Err(ImapError::command("EXAMINE", "invalid mailbox name length"));
        }
        let encoded = crate::mailbox_codec::encode(name);
        if encoded.len() > MAX_METADATA_BYTES {
            return Err(ImapError::command(
                "EXAMINE",
                "mailbox name exceeds metadata budget",
            ));
        }
        let body = CommandBody::examine(encoded)
            .map_err(|_| ImapError::command("EXAMINE", "invalid mailbox name"))?;
        let mut notices = Vec::new();
        if self.selection.is_some() {
            // A drain cannot identify old bytes still in flight. Returning
            // the server to authenticated state gives the stream an explicit
            // boundary before the next selection's epoch fields. CLOSE would
            // risk expunging messages and is never a fallback.
            if !self
                .capabilities
                .iter()
                .any(|cap| matches!(cap, Capability::Unselect))
            {
                self.wire = None;
                self.selection = None;
                return Err(ImapError::FreshConnectionRequired);
            }
            notices = self
                .read_metadata(CommandBody::Unselect, ReadKind::Unselect, |_| Ok(()))
                .await?;
            self.selection = None;
        }
        let mut builder = ExamineBuilder::default();
        let selection_notices = self
            .read_metadata(body, ReadKind::Examine, |response| builder.push(response))
            .await?;
        for notice in selection_notices {
            if !notices.contains(&notice) {
                notices.push(notice);
            }
        }
        let selection = match builder.finish(if name.eq_ignore_ascii_case("INBOX") {
            "INBOX".to_string()
        } else {
            name.to_string()
        }) {
            Ok(selection) => selection,
            Err(error) => {
                self.wire = None;
                self.selection = None;
                return Err(error);
            }
        };
        self.selection = Some(MailboxContext {
            name: selection.mailbox.clone(),
            uid_validity: selection.uid_validity,
        });
        Ok(ReadObservation {
            observation: selection,
            notices,
        })
    }

    async fn read_metadata(
        &mut self,
        body: CommandBody<'static>,
        kind: ReadKind,
        collect: impl FnMut(&Response<'_>) -> Result<(), ImapError>,
    ) -> Result<Vec<MailboxNotice>, ImapError> {
        // The future owns the socket, including while completing an old
        // partial frame and before the first write. Cancel closes it.
        let mut wire = self.wire.take().ok_or(ImapError::Closed)?;
        let mut notices = std::mem::take(&mut self.pending_notices);
        let observed = metadata_exchange(
            &mut self.gate,
            &mut self.selection,
            &mut wire,
            body,
            kind,
            collect,
        )
        .await?;
        for notice in observed {
            if !notices.contains(&notice) {
                notices.push(notice);
            }
        }
        self.wire = Some(wire);
        Ok(notices)
    }
}

async fn metadata_exchange<W>(
    gate: &mut Gate,
    selection: &mut Option<MailboxContext>,
    wire: &mut W,
    body: CommandBody<'static>,
    kind: ReadKind,
    mut collect: impl FnMut(&Response<'_>) -> Result<(), ImapError>,
) -> Result<Vec<MailboxNotice>, ImapError>
where
    W: AsyncRead + AsyncWrite + Unpin,
{
    let mut notices = Vec::new();
    let mut remaining = MAX_METADATA_BYTES;
    let codec = ResponseCodec::new();
    if gate.poisoned() {
        return Err(ImapError::Malformed);
    }

    // Complete all bytes already received before issuing the command.
    // This includes partial frames, so an old UIDVALIDITY or LIST row
    // cannot become authority merely because its last byte arrived late.
    while gate.has_buffered() {
        let frame = gate.read_frame(wire).await?.ok_or(ImapError::Closed)?;
        if gate.poisoned() {
            return Err(ImapError::Malformed);
        }
        charge(&mut remaining, &frame, kind)?;
        if !matches!(frame.kind, FrameKind::Untagged) {
            return Err(ImapError::UnexpectedCompletion);
        }
        let (_, response) = codec
            .decode(&frame.bytes)
            .map_err(|_| ImapError::Malformed)?;
        check_selected_epoch(&response, selection.as_ref(), kind)?;
        notice(&response, selection.as_ref(), &mut notices)?;
    }
    if matches!(kind, ReadKind::Examine) {
        *selection = None;
    }
    let command = Command {
        tag: TagGenerator::new().generate(),
        body,
    };
    let bytes = CommandCodec::new().encode(&command).dump();
    if bytes.len() > MAX_METADATA_BYTES {
        return Err(ImapError::command(
            kind.operation(),
            "command exceeds metadata budget",
        ));
    }
    write_command(wire, &bytes)
        .await
        .map_err(ImapError::from_io)?;
    loop {
        let frame = gate.read_frame(wire).await?.ok_or_else(|| {
            ImapError::command(kind.operation(), "connection ended before completion")
        })?;
        if gate.poisoned() {
            return Err(ImapError::Malformed);
        }
        charge(&mut remaining, &frame, kind)?;
        let (_, response) = codec
            .decode(&frame.bytes)
            .map_err(|_| ImapError::Malformed)?;
        check_selected_epoch(&response, selection.as_ref(), kind)?;
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
                collect(&response)?;
                if !gate.reusable() {
                    return Err(ImapError::UnexpectedCompletion);
                }
                return Ok(notices);
            }
            Response::Status(Status::Bye(_)) | Response::CommandContinuationRequest(_) => {
                return Err(ImapError::UnexpectedCompletion);
            }
            _ => {
                collect(&response)?;
                let owned = match (&kind, &response) {
                    (ReadKind::List, Response::Data(Data::List { .. })) => true,
                    (ReadKind::UidSearch, Response::Data(Data::Search(..))) => true,
                    (ReadKind::FlagSweep, Response::Data(Data::Fetch { .. })) => true,
                    (ReadKind::SelectedCapability, Response::Data(Data::Capability(_))) => true,
                    (
                        ReadKind::Examine,
                        Response::Data(Data::Flags(_) | Data::Exists(_) | Data::Recent(_)),
                    ) => true,
                    (ReadKind::Examine, Response::Status(Status::Untagged(body))) => {
                        body.kind == StatusKind::Ok
                            && matches!(
                                body.code,
                                Some(
                                    Code::UidValidity(_)
                                        | Code::UidNext(_)
                                        | Code::PermanentFlags(_)
                                        | Code::Unseen(_)
                                )
                            )
                    }
                    _ => false,
                };
                if !owned {
                    notice(&response, selection.as_ref(), &mut notices)?;
                }
            }
        }
    }
}

fn charge(remaining: &mut usize, frame: &Frame, kind: ReadKind) -> Result<(), ImapError> {
    *remaining = remaining
        .checked_sub(frame.bytes.len())
        .ok_or_else(|| ImapError::command(kind.operation(), "metadata response budget exceeded"))?;
    Ok(())
}

fn check_selected_epoch(
    response: &Response<'_>,
    selection: Option<&MailboxContext>,
    kind: ReadKind,
) -> Result<(), ImapError> {
    // UNSELECT/EXAMINE establish a new boundary with fresh epoch evidence.
    // Reads that retain a selection, including LIST, cannot hide a change
    // from a later UID read on this connection.
    if matches!(kind, ReadKind::Unselect | ReadKind::Examine) {
        return Ok(());
    }
    let Some(selection) = selection else {
        return if kind.needs_selection() {
            Err(ImapError::NotSelected)
        } else {
            Ok(())
        };
    };
    let code = match response {
        Response::Status(Status::Tagged(reply)) => reply.body.code.as_ref(),
        Response::Status(Status::Untagged(body)) => body.code.as_ref(),
        _ => None,
    };
    match code {
        Some(Code::UidValidity(epoch)) if *epoch != selection.uid_validity => {
            Err(ImapError::SelectionInvalidated)
        }
        Some(Code::Closed | Code::ReadWrite) => Err(ImapError::SelectionInvalidated),
        Some(Code::Other(code)) => crate::mailbox::check_other_selection_code(code.inner())
            .map_err(|message| ImapError::command(kind.operation(), message)),
        _ => Ok(()),
    }
}

fn notice(
    response: &Response<'_>,
    context: Option<&MailboxContext>,
    notices: &mut Vec<MailboxNotice>,
) -> Result<(), ImapError> {
    let item = match response {
        Response::Data(Data::List { .. }) => {
            if let Response::Data(data) = response {
                list_row(data)?;
            }
            MailboxNotice::MailboxListChanged
        }
        Response::Data(Data::Capability(_)) => MailboxNotice::CapabilitiesChanged,
        Response::Status(Status::Untagged(body)) => match (&body.code, &body.kind) {
            (Some(Code::Capability(_)), _) => MailboxNotice::CapabilitiesChanged,
            (Some(Code::Alert), _) => MailboxNotice::Alert,
            (_, StatusKind::No | StatusKind::Bad) => MailboxNotice::Warning,
            (
                Some(
                    Code::UidValidity(_)
                    | Code::UidNext(_)
                    | Code::PermanentFlags(_)
                    | Code::Unseen(_),
                ),
                _,
            ) => {
                changed(context, notices);
                return Ok(());
            }
            _ => return Ok(()),
        },
        Response::Data(_) => {
            changed(context, notices);
            return Ok(());
        }
        _ => return Err(ImapError::UnexpectedCompletion),
    };
    if !notices.contains(&item) {
        notices.push(item);
    }
    Ok(())
}

fn changed(context: Option<&MailboxContext>, notices: &mut Vec<MailboxNotice>) {
    // One exchange has at most its previous selection and an unattributed
    // switch window. Coalesce before cloning a possibly long mailbox name;
    // a flood of tiny updates must not repeatedly copy that name.
    if !notices.iter().any(|item| matches!(item, MailboxNotice::Changed { mailbox } if mailbox.is_some() == context.is_some())) {
        notices.push(MailboxNotice::Changed { mailbox: context.cloned() });
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};

    #[tokio::test]
    async fn a_metadata_flood_reaches_the_budget_without_a_partial_observation() {
        let (mut client, server) = tokio::io::duplex(8192);
        let exchange = async move {
            let mut gate = Gate::new();
            gate.read(&mut client).await.unwrap();
            let mut selection = None;
            let result = metadata_exchange(
                &mut gate,
                &mut selection,
                &mut client,
                CommandBody::list(String::new(), "*".to_string()).unwrap(),
                ReadKind::List,
                |_| Ok(()),
            )
            .await;
            assert!(
                matches!(result, Err(ImapError::Command { operation: "LIST", ref message }) if message.contains("budget"))
            );
        };
        let peer = async {
            let mut server = BufReader::new(server);
            server.write_all(b"* OK ready\r\n").await.unwrap();
            let mut command = String::new();
            server.read_line(&mut command).await.unwrap();
            let tag = command.split_whitespace().next().unwrap();
            let line = format!("* OK {}\r\n", "x".repeat(4089));
            assert_eq!(line.len(), 4096);
            for _ in 0..=(50 * 1024 * 1024 / 4096) {
                if server.write_all(line.as_bytes()).await.is_err() {
                    return;
                }
            }
            // Without the aggregate budget, these individually valid frames
            // would yield a successful empty LIST after this completion.
            let _ = server
                .write_all(format!("{tag} OK listed\r\n").as_bytes())
                .await;
        };
        tokio::time::timeout(std::time::Duration::from_secs(30), async {
            tokio::join!(exchange, peer);
        })
        .await
        .unwrap();
    }
}
