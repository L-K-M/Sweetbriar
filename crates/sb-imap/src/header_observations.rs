//! UID-keyed header evidence from one FETCH frame per row. Prefixes
//! remain partial transport evidence, never complete persisted headers.
use crate::MailboxContext;
use io_imap::types::{
    command::CommandBody,
    core::{AString, Vec1},
    fetch::{MessageDataItemName, Section},
    sequence::{SeqOrUid, Sequence},
};
use std::fmt;
use std::num::NonZeroU32;

/// Which portion of the selected header fields to request.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderFetchMode {
    /// The entire selected header section, with no partial origin.
    Full,
    /// Only its first bytes; never combine successive reads into one row.
    Prefix {
        /// The nonzero maximum requested length, starting at offset zero.
        max_bytes: NonZeroU32,
    },
}

/// A local request validation failure. No network operation has begun.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderRequestError {
    /// At least one explicit UID is required.
    EmptyUids,
    /// The input batch exceeds the spec/04 limit of 250 UIDs.
    TooManyUids,
    /// At least one header field name is required.
    EmptyFields,
    /// A field name is empty or contains a colon, control or non-ASCII byte.
    InvalidFieldName,
}
impl fmt::Display for HeaderRequestError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(match self {
            Self::EmptyUids => "a header request requires at least one UID",
            Self::TooManyUids => "a header request accepts at most 250 UIDs",
            Self::EmptyFields => "a header request requires at least one field name",
            Self::InvalidFieldName => "header field names must be nonempty ASCII field names",
        })
    }
}
impl std::error::Error for HeaderRequestError {}

/// A validated explicit batch. Fields are ASCII upper-case, sorted and
/// deduplicated; UIDs are sorted and deduplicated without range expansion.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeaderRequest {
    uids: Vec<NonZeroU32>,
    fields: Vec<String>,
    mode: HeaderFetchMode,
}
impl HeaderRequest {
    /// The maximum number of UIDs in an input batch (spec/04).
    pub const MAX_UIDS: usize = 250;
    /// Validate the batch and RFC 5322 field-name syntax before any I/O.
    pub fn new(
        mut uids: Vec<NonZeroU32>,
        mut fields: Vec<String>,
        mode: HeaderFetchMode,
    ) -> Result<Self, HeaderRequestError> {
        if uids.is_empty() {
            return Err(HeaderRequestError::EmptyUids);
        }
        if uids.len() > Self::MAX_UIDS {
            return Err(HeaderRequestError::TooManyUids);
        }
        if fields.is_empty() {
            return Err(HeaderRequestError::EmptyFields);
        }
        if fields.iter().any(|name| {
            name.is_empty() || !name.bytes().all(|b| (33..=126).contains(&b) && b != b':')
        }) {
            return Err(HeaderRequestError::InvalidFieldName);
        }
        uids.sort_unstable();
        uids.dedup();
        for field in &mut fields {
            field.make_ascii_uppercase();
        }
        fields.sort_unstable();
        fields.dedup();
        Ok(Self { uids, fields, mode })
    }
    /// The explicit requested UIDs, with no sequence-number interpretation.
    pub fn uids(&self) -> &[NonZeroU32] {
        &self.uids
    }
    /// The normalized requested field names.
    pub fn fields(&self) -> &[String] {
        &self.fields
    }
    /// The full or first-prefix request mode.
    pub fn mode(&self) -> HeaderFetchMode {
        self.mode
    }

    pub(crate) fn command(&self) -> CommandBody<'static> {
        let fields: Vec<AString<'static>> = self
            .fields
            .iter()
            .cloned()
            .map(|name| name.try_into().expect("validated ASCII field name"))
            .collect();
        let fields = Vec1::try_from(fields).expect("validated nonempty fields");
        let partial = match self.mode {
            HeaderFetchMode::Full => None,
            HeaderFetchMode::Prefix { max_bytes } => Some((0, max_bytes)),
        };
        // The pinned codec maps NonZeroU32::MAX's convenience conversion
        // to '*'. Explicit Value preserves the actual requested UID.
        let sequence: Vec<Sequence> = self
            .uids
            .iter()
            .map(|uid| Sequence::Single(SeqOrUid::Value(*uid)))
            .collect();
        CommandBody::fetch(
            sequence,
            vec![
                MessageDataItemName::Uid,
                MessageDataItemName::Flags,
                MessageDataItemName::InternalDate,
                MessageDataItemName::Rfc822Size,
                MessageDataItemName::BodyExt {
                    section: Some(Section::HeaderFields(None, fields)),
                    partial,
                    peek: true,
                },
            ],
            true,
        )
        .expect("validated UID header request")
    }
}

/// Raw section bytes, distinguishing unavailable data from real empty data.
/// Debug output reports lengths instead of message content.
#[derive(Clone, PartialEq, Eq)]
pub enum HeaderBytes {
    /// A complete requested section, including an actually empty section.
    Full(Vec<u8>),
    /// A first prefix, conservatively partial even when shorter than its limit.
    Prefix {
        /// The exact bytes returned in this one response.
        bytes: Vec<u8>,
        /// The requested maximum, not a claim about total section length.
        limit: NonZeroU32,
    },
    /// The server returned NIL; no literal was observed.
    Unavailable,
}
impl fmt::Debug for HeaderBytes {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Full(bytes) => f.debug_struct("Full").field("len", &bytes.len()).finish(),
            Self::Prefix { bytes, limit } => f
                .debug_struct("Prefix")
                .field("len", &bytes.len())
                .field("limit", limit)
                .finish(),
            Self::Unavailable => f.write_str("Unavailable"),
        }
    }
}

/// One frame's required metadata and section, keyed only by its UID item.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeaderRow {
    /// The explicit UID item, which may follow the literal on the wire.
    pub uid: NonZeroU32,
    /// Known flags; an empty list is distinct from an absent attribute.
    pub flags: Vec<String>,
    /// The observed INTERNALDATE expressed as UTC Unix milliseconds.
    pub internal_date_ms: i64,
    /// The server's size hint, without imposing a body budget on metadata.
    pub rfc822_size: u32,
    /// Exact section evidence, never stitched from separate FETCH frames.
    pub headers: HeaderBytes,
}

/// Coverage of this requested batch, not archive or MIME-parse coverage.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HeaderCoverage {
    /// Every requested UID has one full row without ambiguity/change hints.
    Complete,
    /// Missing, unavailable, repeated, unkeyed or intentionally partial data.
    Incomplete,
}

/// Owned rows from one checked selection and request. No deletion or
/// checkpoint authority follows from this transport observation.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct HeaderBatch {
    /// The exact read-only selection under which the batch was observed.
    pub mailbox: MailboxContext,
    /// The normalized UIDs this request attempted to observe.
    pub requested_uids: Vec<NonZeroU32>,
    /// At most one row per requested UID, sorted by UID.
    pub rows: Vec<HeaderRow>,
    /// Requested UIDs without a row containing all required metadata.
    /// NIL rows are not missing here; their coverage remains incomplete.
    pub missing_uids: Vec<NonZeroU32>,
    /// Full only when all requested full sections were observed unambiguously.
    pub coverage: HeaderCoverage,
}

mod builder;
#[cfg(test)]
use builder::HeaderBuilder;

#[cfg(test)]
mod tests;
