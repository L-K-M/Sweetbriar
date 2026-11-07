//! Owned, dialect-free mailbox observations. Missing epoch evidence never
//! defaults to zero, and raw names pass through the strict private codec.
use std::num::NonZeroU32;

use io_imap::types::core::Atom;
use io_imap::types::flag::FlagPerm;
use io_imap::types::mailbox::Mailbox;
use io_imap::types::response::{Code, Data, Response, Status, StatusKind};

use crate::{ImapError, mailbox_codec};

/// One complete LIST row, without provider-specific role interpretation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailboxSummary {
    /// Strictly decoded Unicode name, with INBOX's special canonical spelling.
    pub name: String,
    /// Hierarchy delimiter; None is the protocol's flat namespace.
    pub delimiter: Option<char>,
    /// Protocol attribute spellings, including extension attributes.
    pub attributes: Vec<String>,
}

/// A complete read-only EXAMINE observation. Counts are informational;
/// disappearance reconciliation requires a complete UID inventory.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct MailboxSelection {
    /// The strictly encoded command's original Unicode mailbox name.
    pub mailbox: String,
    /// UIDVALIDITY supplied by this selection, never inherited or zero.
    pub uid_validity: NonZeroU32,
    /// Predicted next UID, unknown if the newer response code was absent.
    pub uid_next: Option<NonZeroU32>,
    /// Last EXISTS count before the matched completion.
    pub exists: u32,
    /// Last RECENT count before the matched completion.
    pub recent: u32,
    /// Supported flags supplied by this selection, including an explicit empty list.
    pub flags: Vec<String>,
    /// Permanent flags, unknown when omitted; no mutation permission is inferred.
    pub permanent_flags: Option<Vec<String>>,
    /// First unseen sequence number, unknown when omitted.
    pub unseen: Option<NonZeroU32>,
}

pub(crate) fn list_row(data: &Data<'_>) -> Result<Option<MailboxSummary>, ImapError> {
    let Data::List {
        items,
        delimiter,
        mailbox,
    } = data
    else {
        return Ok(None);
    };
    let name = match mailbox {
        Mailbox::Inbox => "INBOX".to_string(),
        Mailbox::Other(name) => mailbox_codec::decode(name.as_ref())
            .map_err(|_| ImapError::command("LIST", "invalid mailbox name encoding"))?,
    };
    Ok(Some(MailboxSummary {
        name,
        delimiter: delimiter.as_ref().map(|value| value.inner()),
        attributes: items.iter().map(ToString::to_string).collect(),
    }))
}

#[derive(Default)]
pub(crate) struct ExamineBuilder {
    uid_validity: Option<NonZeroU32>,
    uid_next: Option<NonZeroU32>,
    exists: Option<u32>,
    recent: Option<u32>,
    flags: Option<Vec<String>>,
    permanent_flags: Option<Vec<String>>,
    unseen: Option<NonZeroU32>,
    read_only: bool,
}

impl ExamineBuilder {
    /// Only the matched first completion is passed here by the exchange.
    pub(crate) fn push(&mut self, response: &Response<'_>) -> Result<(), ImapError> {
        match response {
            Response::Data(Data::Flags(flags)) => {
                self.flags = Some(flags.iter().map(ToString::to_string).collect())
            }
            Response::Data(Data::Exists(count)) => self.exists = Some(*count),
            Response::Data(Data::Recent(count)) => self.recent = Some(*count),
            Response::Status(Status::Tagged(reply)) => {
                self.read_only = reply.body.kind == StatusKind::Ok
                    && matches!(reply.body.code, Some(Code::ReadOnly));
            }
            Response::Status(Status::Untagged(body)) if body.kind == StatusKind::Ok => {
                match &body.code {
                    Some(Code::UidValidity(value)) => {
                        if self.uid_validity.is_some_and(|old| old != *value) {
                            return Err(invalid("conflicting UIDVALIDITY"));
                        }
                        self.uid_validity = Some(*value);
                    }
                    Some(Code::UidNext(value)) => {
                        if self.uid_next.is_some_and(|old| old > *value) {
                            return Err(invalid("decreasing UIDNEXT"));
                        }
                        self.uid_next = Some(*value);
                    }
                    Some(Code::PermanentFlags(flags)) => {
                        self.permanent_flags = Some(
                            flags
                                .iter()
                                .map(|flag| match flag {
                                    FlagPerm::Flag(flag) => flag.to_string(),
                                    FlagPerm::Asterisk => "\\*".to_string(),
                                })
                                .collect(),
                        )
                    }
                    Some(Code::Unseen(value)) => self.unseen = Some(*value),
                    Some(Code::Other(code)) => {
                        check_other_selection_code(code.inner()).map_err(invalid)?;
                    }
                    _ => {}
                }
            }
            _ => {}
        }
        Ok(())
    }

    pub(crate) fn finish(self, mailbox: String) -> Result<MailboxSelection, ImapError> {
        if !self.read_only {
            return Err(invalid("missing READ-ONLY completion"));
        }
        Ok(MailboxSelection {
            mailbox,
            uid_validity: self
                .uid_validity
                .ok_or_else(|| invalid("missing UIDVALIDITY"))?,
            uid_next: self.uid_next,
            exists: self.exists.ok_or_else(|| invalid("missing EXISTS"))?,
            recent: self.recent.ok_or_else(|| invalid("missing RECENT"))?,
            flags: self.flags.ok_or_else(|| invalid("missing FLAGS"))?,
            permanent_flags: self.permanent_flags,
            unseen: self.unseen,
        })
    }
}

fn invalid(message: &'static str) -> ImapError {
    ImapError::command("EXAMINE", message)
}

/// The pinned codec sends malformed known codes through its extension
/// fallback. Both EXAMINE and later UID reads must refuse those bytes.
pub(crate) fn check_other_selection_code(code: &[u8]) -> Result<(), &'static str> {
    let keyword = code.split(|byte| *byte == b' ').next().unwrap_or_default();
    Atom::try_from(keyword).map_err(|_| "invalid response code keyword")?;
    if [
        b"UIDVALIDITY".as_slice(),
        b"UIDNEXT",
        b"UNSEEN",
        b"PERMANENTFLAGS",
        b"CLOSED",
        b"READ-ONLY",
        b"READ-WRITE",
    ]
    .iter()
    .any(|known| keyword.eq_ignore_ascii_case(known))
    {
        return Err("invalid selection response code");
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use io_imap::codec::{ResponseCodec, decode::Decoder};

    const BASE: &[&str] = &[
        "* FLAGS ()\r\n",
        "* 0 EXISTS\r\n",
        "* 0 RECENT\r\n",
        "* OK [UIDVALIDITY 41] epoch\r\n",
        "T OK [READ-ONLY] done\r\n",
    ];

    fn builder(lines: &[&str]) -> Result<ExamineBuilder, ImapError> {
        let mut output = ExamineBuilder::default();
        for line in lines {
            let (rest, response) = ResponseCodec::new()
                .decode(line.as_bytes())
                .map_err(|_| ImapError::Malformed)?;
            assert!(rest.is_empty());
            output.push(&response)?;
        }
        Ok(output)
    }

    #[test]
    fn empty_and_unknown_are_distinct_and_every_required_field_is_present() {
        let selected = builder(BASE).unwrap().finish("INBOX".into()).unwrap();
        assert_eq!(selected.exists, 0);
        assert!(selected.flags.is_empty());
        assert_eq!(selected.uid_next, None);
        assert_eq!(selected.permanent_flags, None);
        assert_eq!(selected.unseen, None);
        for index in 0..BASE.len() {
            let lines: Vec<_> = BASE
                .iter()
                .copied()
                .enumerate()
                .filter_map(|(at, line)| (at != index).then_some(line))
                .collect();
            assert!(
                builder(&lines).unwrap().finish("INBOX".into()).is_err(),
                "missing {index}"
            );
        }
    }

    #[test]
    fn epochs_cannot_conflict_or_hide_zero_overflow_or_invalid_known_codes() {
        for code in [
            "UIDVALIDITY 0",
            "UIDVALIDITY 42",
            "UIDVALIDITY 4294967296",
            "UIDNEXT 0",
            "UIDNEXT bogus",
            "UNSEEN 0",
            "PERMANENTFLAGS bogus",
            " UIDVALIDITY 42",
            "\tUIDVALIDITY 0",
            "UIDNEXT\u{000b}0",
            "UIDVALIDITY\0 42",
            "UIDVALIDITYé 42",
        ] {
            let line = format!("* OK [{code}] invalid\r\n");
            let mut lines = BASE.to_vec();
            lines.insert(4, &line);
            assert!(
                builder(&lines)
                    .and_then(|b| b.finish("INBOX".into()))
                    .is_err(),
                "{code}"
            );
        }
        let mut lines = BASE.to_vec();
        lines.insert(4, "* OK [UIDVALIDITY 41] repeated\r\n");
        assert!(builder(&lines).unwrap().finish("INBOX".into()).is_ok());
    }

    #[test]
    fn updates_and_optional_codes_remain_explicit() {
        let mut lines = BASE.to_vec();
        lines.splice(
            4..4,
            [
                "* OK [UIDNEXT 7] next\r\n",
                "* OK [UIDNEXT 8] next\r\n",
                "* 3 EXISTS\r\n",
                "* 1 RECENT\r\n",
                "* FLAGS (\\Seen custom)\r\n",
                "* OK [UNSEEN 2] unseen\r\n",
                "* OK [PERMANENTFLAGS (\\Seen \\*)] permanent\r\n",
                "* OK [X-CUSTOM value] extension\r\n",
            ],
        );
        let selected = builder(&lines).unwrap().finish("Other".into()).unwrap();
        assert_eq!(selected.uid_next.unwrap().get(), 8);
        assert_eq!((selected.exists, selected.recent), (3, 1));
        assert_eq!(selected.flags, ["\\Seen", "custom"]);
        assert_eq!(selected.unseen.unwrap().get(), 2);
        assert_eq!(selected.permanent_flags.unwrap(), ["\\Seen", "\\*"]);
        lines.insert(lines.len() - 1, "* OK [UIDNEXT 7] decreased\r\n");
        assert!(builder(&lines).is_err());
    }

    #[test]
    fn only_a_successful_read_only_completion_qualifies() {
        for completion in [
            "T OK done\r\n",
            "T OK [READ-WRITE] done\r\n",
            "T NO [READ-ONLY] refused\r\n",
            "* OK [READ-ONLY] not a completion\r\n",
        ] {
            let mut lines = BASE.to_vec();
            lines[4] = completion;
            assert!(builder(&lines).unwrap().finish("Other".into()).is_err());
        }
    }

    #[test]
    fn list_uses_the_strict_name_codec_and_preserves_attributes_and_nil() {
        for (wire, name, delimiter) in [
            (
                "* LIST (\\Noselect \\HasChildren \\Sent) NIL \"&U,BTFw- &-\"\r\n",
                "台北 &",
                None,
            ),
            ("* LIST () \"/\" iNbOx\r\n", "INBOX", Some('/')),
        ] {
            let (_, Response::Data(data)) = ResponseCodec::new().decode(wire.as_bytes()).unwrap()
            else {
                panic!("LIST");
            };
            let row = list_row(&data).unwrap().unwrap();
            assert_eq!(row.name, name);
            assert_eq!(row.delimiter, delimiter);
            if delimiter.is_none() {
                assert_eq!(row.attributes, ["\\Noselect", "\\HasChildren", "\\Sent"]);
            }
        }
        for name in ["&AEE-", "&2AA-", "&missing", "台北"] {
            let wire = format!("* LIST () NIL \"{name}\"\r\n");
            if let Ok((_, Response::Data(data))) = ResponseCodec::new().decode(wire.as_bytes()) {
                let error = list_row(&data).unwrap_err();
                assert!(!format!("{error:?} {error}").contains(name));
            }
        }
    }
}
