//! Pure FETCH-frame validation for one header request. Each row is
//! built from a single decoded `Data::Fetch` frame; no I/O, no MIME
//! parsing and no size policy enter this leaf.

use std::collections::{BTreeMap, BTreeSet};
use std::num::NonZeroU32;

use io_imap::types::core::NString;
use io_imap::types::fetch::{MessageDataItem, Section};
use io_imap::types::response::{Data, Response};

use super::{HeaderBatch, HeaderBytes, HeaderCoverage, HeaderFetchMode, HeaderRequest, HeaderRow};
use crate::uid_observations::flag_name;
use crate::{ImapError, MailboxContext};

/// Collects one validated row per requested UID from `UID FETCH`
/// frames. A frame is complete only with exactly one UID, FLAGS,
/// INTERNALDATE, RFC822.SIZE and matching BODY.PEEK[HEADER.FIELDS]
/// item; anything else is incomplete evidence or a static failure.
pub(crate) struct HeaderBuilder<'a> {
    request: &'a HeaderRequest,
    rows: BTreeMap<NonZeroU32, HeaderRow>,
    seen: BTreeSet<NonZeroU32>,
    ambiguous: bool,
    failed: bool,
}

impl<'a> HeaderBuilder<'a> {
    /// Starts a builder for an already validated request.
    pub(crate) fn new(request: &'a HeaderRequest) -> Self {
        Self {
            request,
            rows: BTreeMap::new(),
            seen: BTreeSet::new(),
            ambiguous: false,
            failed: false,
        }
    }

    /// Validates one decoded response. Non-FETCH responses are ignored.
    /// A protocol violation marks the builder failed, so an ignored
    /// error still prevents `finish` from returning partial rows.
    pub(crate) fn push(&mut self, response: &Response<'_>) -> Result<(), ImapError> {
        let Response::Data(Data::Fetch { items, .. }) = response else {
            return Ok(());
        };
        let mut uid = None;
        let mut flags = None;
        let mut date_ms = None;
        let mut size = None;
        let mut headers = None;
        for item in items.as_ref() {
            match item {
                MessageDataItem::Uid(value) => {
                    if uid.replace(*value).is_some() {
                        return Err(self.fail("duplicate UID attribute"));
                    }
                }
                MessageDataItem::Flags(values) => {
                    if flags
                        .replace(values.iter().map(flag_name).collect())
                        .is_some()
                    {
                        return Err(self.fail("duplicate FLAGS attribute"));
                    }
                }
                MessageDataItem::InternalDate(value) => {
                    if date_ms.replace(value.as_ref().timestamp_millis()).is_some() {
                        return Err(self.fail("duplicate INTERNALDATE attribute"));
                    }
                }
                MessageDataItem::Rfc822Size(value) => {
                    if size.replace(*value).is_some() {
                        return Err(self.fail("duplicate RFC822.SIZE attribute"));
                    }
                }
                MessageDataItem::BodyExt {
                    section,
                    origin,
                    data,
                } => {
                    if headers.is_some() {
                        return Err(self.fail("duplicate BODY attribute"));
                    }
                    let validated = validate_body(
                        self.request.mode(),
                        self.request.fields(),
                        section,
                        *origin,
                        data,
                    );
                    headers = Some(validated.map_err(|message| self.fail(message))?);
                }
                _ => {}
            }
        }
        let Some(uid) = uid else {
            self.ambiguous = true;
            return Ok(());
        };
        if !self.request.uids().contains(&uid) {
            self.ambiguous = true;
            return Ok(());
        }
        let (Some(flags), Some(date_ms), Some(size), Some(headers)) =
            (flags, date_ms, size, headers)
        else {
            self.rows.remove(&uid);
            self.seen.insert(uid);
            self.ambiguous = true;
            return Ok(());
        };
        if !self.seen.insert(uid) {
            self.ambiguous = true;
        }
        self.rows.insert(
            uid,
            HeaderRow {
                uid,
                flags,
                internal_date_ms: date_ms,
                rfc822_size: size,
                headers,
            },
        );
        Ok(())
    }

    /// Whether any frame failed to yield one complete new row for a
    /// requested UID. Prefix and NIL evidence alone do not set this.
    pub(crate) fn ambiguous(&self) -> bool {
        self.ambiguous
    }

    /// Returns the owned batch. Coverage is complete only for a full,
    /// unchanged, unambiguous request where every requested UID has one
    /// full row.
    pub(crate) fn finish(
        self,
        mailbox: MailboxContext,
        changed: bool,
    ) -> Result<HeaderBatch, ImapError> {
        if self.failed {
            return Err(fetch_error("invalid UID FETCH frame"));
        }
        let requested_uids = self.request.uids().to_vec();
        let missing_uids: Vec<NonZeroU32> = requested_uids
            .iter()
            .copied()
            .filter(|uid| !self.rows.contains_key(uid))
            .collect();
        let rows: Vec<HeaderRow> = self.rows.into_values().collect();
        let complete = matches!(self.request.mode(), HeaderFetchMode::Full)
            && !changed
            && !self.ambiguous
            && rows.len() == requested_uids.len()
            && rows
                .iter()
                .all(|row| matches!(&row.headers, HeaderBytes::Full(_)));
        Ok(HeaderBatch {
            mailbox,
            requested_uids,
            rows,
            missing_uids,
            coverage: if complete {
                HeaderCoverage::Complete
            } else {
                HeaderCoverage::Incomplete
            },
        })
    }

    /// Marks the builder failed and returns the static command error.
    fn fail(&mut self, message: &'static str) -> ImapError {
        self.failed = true;
        fetch_error(message)
    }
}

/// Validates one echoed body item against the request. The section must
/// be the exact header-field subset, the origin must match the mode and
/// a prefix must not exceed its requested maximum.
fn validate_body(
    mode: HeaderFetchMode,
    requested: &[String],
    section: &Option<Section<'_>>,
    origin: Option<u32>,
    data: &NString<'_>,
) -> Result<HeaderBytes, &'static str> {
    let Some(Section::HeaderFields(part, echo)) = section else {
        return Err("unexpected body section");
    };
    if part.is_some() {
        return Err("unexpected body section part");
    }
    if echo.as_ref().len() != requested.len() {
        return Err("header field echo mismatch");
    }
    let mut echoed: Vec<Vec<u8>> = echo
        .as_ref()
        .iter()
        .map(|name| name.as_ref().iter().map(u8::to_ascii_uppercase).collect())
        .collect();
    echoed.sort_unstable();
    echoed.dedup();
    let requested_bytes: Vec<&[u8]> = requested.iter().map(String::as_bytes).collect();
    if echoed.len() != requested_bytes.len()
        || echoed
            .iter()
            .map(Vec::as_slice)
            .ne(requested_bytes.iter().copied())
    {
        return Err("header field echo mismatch");
    }
    let expected_origin = match mode {
        HeaderFetchMode::Full => None,
        HeaderFetchMode::Prefix { .. } => Some(0),
    };
    if origin != expected_origin {
        return Err("unexpected body origin");
    }
    let bytes = match data.0.as_ref() {
        Some(value) => value.as_ref().to_vec(),
        None => return Ok(HeaderBytes::Unavailable),
    };
    match mode {
        HeaderFetchMode::Full => Ok(HeaderBytes::Full(bytes)),
        HeaderFetchMode::Prefix { max_bytes } => {
            if bytes.len() > max_bytes.get() as usize {
                return Err("prefix exceeds requested maximum");
            }
            Ok(HeaderBytes::Prefix {
                bytes,
                limit: max_bytes,
            })
        }
    }
}

/// A static `UID FETCH` failure; no input text is formatted in.
fn fetch_error(message: &'static str) -> ImapError {
    ImapError::command("UID FETCH", message)
}
