//! Header-field extraction shared by the fully-admitted path (upstream
//! `Message` headers) and the partial path (a bounded re-parse of the
//! root header block), plus the bounded snippet decoder for partial
//! input. All upstream types stay private to this crate: everything
//! leaves here as `MessageSummary` fields.

use mail_parser::decoders::base64::base64_decode;
use mail_parser::decoders::charsets::map::charset_decoder;
use mail_parser::decoders::html::html_to_text;
use mail_parser::decoders::quoted_printable::quoted_printable_decode;
use mail_parser::parsers::preview::preview_text;
use mail_parser::{Addr, Address, Header, HeaderName, HeaderValue, MessageParser};

use sb_types::time::Timestamp;

use crate::limits::SNIPPET_BYTES;
use crate::summary::{MailboxAddress, MessageSummary};
use crate::walk::{Cte, Region, TextExtent};

/// `X-Original-To`: not a `HeaderName` variant, so matched via `Other`.
fn x_original_to() -> HeaderName<'static> {
    HeaderName::from("x-original-to")
}

/// Parser for the summary's header fields (and only those). Used on the
/// bounded root header block in the partial path; the admitted path's
/// upstream `MessageParser` is configured identically so both paths
/// extract the same values.
pub(crate) fn summary_parser() -> MessageParser {
    MessageParser::new()
        .with_minimal_headers()
        .with_message_ids()
        .header_id(HeaderName::ListId)
        .header_address(HeaderName::DeliveredTo)
        .header_address(x_original_to())
        .default_header_ignore()
}

/// The first header value named `name`.
fn field<'h>(headers: &'h [Header<'h>], name: HeaderName) -> Option<&'h HeaderValue<'h>> {
    headers.iter().find(|h| h.name == name).map(|h| &h.value)
}

/// All values of headers named `name`.
fn fields<'h>(
    headers: &'h [Header<'h>],
    name: HeaderName,
) -> impl Iterator<Item = &'h HeaderValue<'h>> {
    headers
        .iter()
        .filter(move |h| h.name == name)
        .map(|h| &h.value)
}

/// First text value of a header, owned.
fn text_of(headers: &[Header<'_>], name: HeaderName) -> Option<String> {
    field(headers, name)
        .and_then(HeaderValue::as_text)
        .map(str::to_string)
}

/// First id of an id-list header (`as_text_list`), owned.
fn id_of(headers: &[Header<'_>], name: HeaderName) -> Option<String> {
    field(headers, name)
        .and_then(HeaderValue::as_text_list)
        .and_then(|l| l.first().map(|s| s.to_string()))
}

/// Every `Addr` in an `Address`, flattening groups.
fn flatten<'a>(address: &'a Address<'a>) -> Vec<&'a Addr<'a>> {
    match address {
        Address::List(list) => list.iter().collect(),
        Address::Group(groups) => groups.iter().flat_map(|g| g.addresses.iter()).collect(),
    }
}

/// All mailboxes across every occurrence of an address header.
fn mailboxes(headers: &[Header<'_>], name: HeaderName) -> Vec<MailboxAddress> {
    let mut out = Vec::new();
    for value in fields(headers, name) {
        if let Some(address) = value.as_address() {
            out.extend(flatten(address).iter().map(|a| {
                MailboxAddress::new(
                    a.name().map(str::to_string),
                    a.address().map(str::to_string),
                )
            }));
        }
    }
    out
}

/// `Date` → UTC milliseconds. An out-of-range `DateTime` is not a date.
fn date_of(headers: &[Header<'_>]) -> Option<Timestamp> {
    let dt = field(headers, HeaderName::Date)?.as_datetime()?;
    if !dt.is_valid() {
        return None;
    }
    Some(Timestamp::from_millis(
        dt.to_timestamp().checked_mul(1_000)?,
    ))
}

/// Fill a summary's header fields from any parsed header block —
/// `message.parts[0].headers` on the admitted path, or a bounded
/// re-parse of the root block on the partial path.
pub(crate) fn extract_headers(headers: &[Header<'_>], summary: &mut MessageSummary) {
    summary.set_message_id(id_of(headers, HeaderName::MessageId));
    summary.set_subject(text_of(headers, HeaderName::Subject));
    summary.set_from(mailboxes(headers, HeaderName::From).into_iter().next());
    summary.set_reply_to(mailboxes(headers, HeaderName::ReplyTo));
    summary.set_to(mailboxes(headers, HeaderName::To));
    summary.set_cc(mailboxes(headers, HeaderName::Cc));
    summary.set_date(date_of(headers));
    summary.set_in_reply_to(id_of(headers, HeaderName::InReplyTo));
    summary.set_references(
        fields(headers, HeaderName::References)
            .filter_map(HeaderValue::as_text_list)
            .flatten()
            .map(|s| s.to_string())
            .collect(),
    );
    summary.set_list_id(id_of(headers, HeaderName::ListId));
    let mut delivered = Vec::new();
    for name in [HeaderName::DeliveredTo, x_original_to()] {
        for value in fields(headers, name) {
            if let Some(address) = value.as_address() {
                delivered.extend(
                    flatten(address)
                        .iter()
                        .filter_map(|a| a.address().map(str::to_string)),
                );
            }
        }
    }
    summary.set_delivered_to(delivered);
}

/// Bounded plain-text snippet for the partial path: decode the extent
/// (its decoded size was budget-checked at admission), reduce HTML,
/// truncate on a UTF-8 boundary via upstream `preview_text`.
pub(crate) fn extent_snippet(regions: &[Region<'_>], extent: &TextExtent) -> Option<String> {
    let bytes = regions[extent.region].bytes().get(extent.range.clone())?;
    let decoded = match extent.cte {
        Cte::Identity => std::borrow::Cow::Borrowed(bytes),
        Cte::Base64 => base64_decode(bytes)?.into(),
        Cte::QuotedPrintable => quoted_printable_decode(bytes)?.into(),
    };
    let text = match extent.charset.as_deref().and_then(charset_decoder) {
        Some(decode) => decode(&decoded),
        None => String::from_utf8_lossy(&decoded).into_owned(),
    };
    let snippet = if extent.html {
        preview_text(html_to_text(&text).into(), SNIPPET_BYTES)
    } else {
        preview_text(text.into(), SNIPPET_BYTES)
    };
    (!snippet.is_empty()).then(|| snippet.into_owned())
}
