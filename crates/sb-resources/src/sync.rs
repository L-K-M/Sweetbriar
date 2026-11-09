//! IMAP sync's composition (spec/04 "Sync loop"): the store's sync
//! writes re-exported for the engine (spec/01 rule 3), and the mapping
//! of one fetched header-fields literal onto `message_headers`
//! columns through the MIME leaf, in the shapes the import path gives
//! `d_message_summary` (one encoding for both).

use sb_mime::{Limits, MailboxAddress};
pub use sb_store::sync::{
    EpochCheck, HeaderBatch, HeaderBatchStats, HeaderColumns, MAX_HEADER_BATCH,
    MAX_HEADER_BATCH_BYTES, ObservedMailbox, Reconciled, SyncedFlags, SyncedHeader, check_epoch,
    commit_header_batch, reconcile_mailboxes,
};

use crate::acquire::{addresses, strings};

/// The `message_headers` columns of one fetched header-fields literal
/// (spec/04 step 3). The literal is parsed as a message with no body;
/// one the parser cannot read yields no columns, and the caller still
/// stores the literal itself. `Delivered-To` and `X-Original-To` both
/// land in `delivered_to_json` (spec/02).
pub fn header_columns(raw_fields: &[u8]) -> HeaderColumns {
    let outcome = sb_mime::parse(&with_blank_line(raw_fields), &Limits::default());
    let Some(summary) = outcome.summary() else {
        return HeaderColumns::default();
    };
    let owned = |text: Option<&str>| text.map(str::to_owned);
    HeaderColumns {
        header_message_id: owned(summary.message_id()),
        subject: owned(summary.subject()),
        from_json: summary
            .from()
            .and_then(|from| addresses(std::slice::from_ref::<MailboxAddress>(from))),
        reply_to_json: addresses(summary.reply_to()),
        to_json: addresses(summary.to()),
        cc_json: addresses(summary.cc()),
        delivered_to_json: strings(summary.delivered_to()),
        date_header: summary.date().map(|date| date.as_millis()),
        in_reply_to: owned(summary.in_reply_to()),
        references_json: strings(summary.references()),
        list_id: owned(summary.list_id()),
    }
}

/// A fetched `HEADER.FIELDS` literal normally ends with the blank line
/// that closes a header section; add one when it does not, so the
/// parser never reads a header as body.
fn with_blank_line(raw_fields: &[u8]) -> Vec<u8> {
    let mut message = raw_fields.to_vec();
    if !(message.ends_with(b"\r\n\r\n") || message.ends_with(b"\n\n")) {
        if !message.is_empty() && !message.ends_with(b"\n") {
            message.extend_from_slice(b"\r\n");
        }
        message.extend_from_slice(b"\r\n");
    }
    message
}

#[cfg(test)]
mod tests;
