//! `MessageSummary` — the owned, upstream-independent summary of one
//! message, shaped so `d_message_summary` (spec/02) can be written from
//! it without re-parsing. Fields are private; accessors are read-only,
//! matching `sb_store::settings::StoredSetting`.
//!
//! Typed collections (`Vec<MailboxAddress>`, `Vec<String>`) stand in for
//! the schema's `*_json` columns; serializing to canonical JSON is the
//! persistence consumer's job, not this crate's (no serde dependency).

use sb_types::time::Timestamp;

/// `d_message_summary.parser_version` — bump when extraction semantics
/// change so derived rows get rebuilt.
pub const PARSER_VERSION: u32 = 1;

/// One mailbox in an address header: display name and address, each
/// optional (headers like `From: "Name"` or bare groups produce partial
/// values upstream; we keep what was there).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct MailboxAddress {
    name: Option<String>,
    address: Option<String>,
}

impl MailboxAddress {
    pub(crate) fn new(name: Option<String>, address: Option<String>) -> Self {
        Self { name, address }
    }

    /// Display name, decoded (RFC 2047), without quotes.
    pub fn name(&self) -> Option<&str> {
        self.name.as_deref()
    }

    /// The `addr-spec` (or URL for RFC 2369 lists), as written.
    pub fn address(&self) -> Option<&str> {
        self.address.as_deref()
    }
}

/// The bounded summary of one message: `d_message_summary` row values
/// minus `acquisition_id` (caller-supplied) and `parse_state` (carried
/// by `ParseOutcome`).
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct MessageSummary {
    message_id: Option<String>,
    subject: Option<String>,
    from: Option<MailboxAddress>,
    reply_to: Vec<MailboxAddress>,
    to: Vec<MailboxAddress>,
    cc: Vec<MailboxAddress>,
    date: Option<Timestamp>,
    in_reply_to: Option<String>,
    references: Vec<String>,
    list_id: Option<String>,
    /// `Delivered-To` and `X-Original-To` address values, in header
    /// order, for reply identity (spec/05).
    delivered_to: Vec<String>,
    has_attachments: bool,
    /// Bounded plain-text preview of the body (≤ `SNIPPET_BYTES`),
    /// already charset-decoded and HTML-reduced where applicable.
    snippet: Option<String>,
}

impl MessageSummary {
    /// The extraction rules version (`parser_version` column).
    pub fn parser_version(&self) -> u32 {
        PARSER_VERSION
    }

    /// `header_message_id`: first `Message-ID` value.
    pub fn message_id(&self) -> Option<&str> {
        self.message_id.as_deref()
    }

    pub fn subject(&self) -> Option<&str> {
        self.subject.as_deref()
    }

    /// First `From` mailbox.
    pub fn from(&self) -> Option<&MailboxAddress> {
        self.from.as_ref()
    }

    pub fn reply_to(&self) -> &[MailboxAddress] {
        &self.reply_to
    }

    pub fn to(&self) -> &[MailboxAddress] {
        &self.to
    }

    pub fn cc(&self) -> &[MailboxAddress] {
        &self.cc
    }

    /// `date_header`: the `Date` field as UTC milliseconds, present only
    /// when the field parsed to a valid date.
    pub fn date(&self) -> Option<Timestamp> {
        self.date
    }

    /// `in_reply_to`: first `In-Reply-To` id.
    pub fn in_reply_to(&self) -> Option<&str> {
        self.in_reply_to.as_deref()
    }

    /// `references`: all `References` ids in order.
    pub fn references(&self) -> &[String] {
        &self.references
    }

    /// `list_id`: the `List-Id` value.
    pub fn list_id(&self) -> Option<&str> {
        self.list_id.as_deref()
    }

    /// `delivered_to`: `Delivered-To` and `X-Original-To` values.
    pub fn delivered_to(&self) -> &[String] {
        &self.delivered_to
    }

    pub fn has_attachments(&self) -> bool {
        self.has_attachments
    }

    /// `snippet`: bounded plain text, `None` when no body text exists.
    pub fn snippet(&self) -> Option<&str> {
        self.snippet.as_deref()
    }

    pub(crate) fn set_message_id(&mut self, v: Option<String>) {
        self.message_id = v;
    }
    pub(crate) fn set_subject(&mut self, v: Option<String>) {
        self.subject = v;
    }
    pub(crate) fn set_from(&mut self, v: Option<MailboxAddress>) {
        self.from = v;
    }
    pub(crate) fn set_reply_to(&mut self, v: Vec<MailboxAddress>) {
        self.reply_to = v;
    }
    pub(crate) fn set_to(&mut self, v: Vec<MailboxAddress>) {
        self.to = v;
    }
    pub(crate) fn set_cc(&mut self, v: Vec<MailboxAddress>) {
        self.cc = v;
    }
    pub(crate) fn set_date(&mut self, v: Option<Timestamp>) {
        self.date = v;
    }
    pub(crate) fn set_in_reply_to(&mut self, v: Option<String>) {
        self.in_reply_to = v;
    }
    pub(crate) fn set_references(&mut self, v: Vec<String>) {
        self.references = v;
    }
    pub(crate) fn set_list_id(&mut self, v: Option<String>) {
        self.list_id = v;
    }
    pub(crate) fn set_delivered_to(&mut self, v: Vec<String>) {
        self.delivered_to = v;
    }
    pub(crate) fn set_has_attachments(&mut self, v: bool) {
        self.has_attachments = v;
    }
    pub(crate) fn set_snippet(&mut self, v: Option<String>) {
        self.snippet = v;
    }
}
