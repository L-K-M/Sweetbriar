//! Proton sync's composition (spec/04-sync-proton "Sync loop" step 1):
//! the store's bootstrap writes re-exported for the engine (spec/01
//! rule 3), and the mapping of Proton's listed metadata onto a
//! `ListedMessage`: the canonical metadata JSON `message_headers` keeps
//! until a body arrives, its columns, and the message's flags.

use sb_proton::{ListingAnchor, MailAddress, MessageMetadata};
use sb_store::Timestamp;

pub use sb_store::{
    ListedMessage, ListedPage, ListedPageStats, MailboxCoverage, ObservedLabel, ProtonCoverage,
    ProtonListingState, begin_listing, commit_listed_page, finish_listing, proton_coverage,
    reconcile_proton_labels,
};

use crate::sync::{HeaderColumns, SyncedFlags};

/// Proton's system labels the flags read (spec/04-sync-proton).
const ALL_DRAFTS: &str = "1";
const DRAFTS: &str = "8";
const STARRED: &str = "10";

/// `Flags` bits (go-proton-api `MessageFlag`): replied, replied to
/// all, forwarded.
const FLAG_REPLIED: u32 = 1 << 5;
const FLAG_REPLIED_ALL: u32 = 1 << 6;
const FLAG_FORWARDED: u32 = 1 << 7;

/// The keyword a forwarded message carries, as IMAP spells it.
const FORWARDED_KEYWORD: &str = "$Forwarded";

/// One listed message as the store takes it. The metadata JSON is kept
/// as received under `{"v":1,"proton_metadata":…}` (spec/02); the
/// columns In-Reply-To and References stay empty until the body's
/// header block replaces them.
pub fn listed_message(metadata: &MessageMetadata) -> ListedMessage {
    let raw = format!(r#"{{"v":1,"proton_metadata":{}}}"#, metadata.raw);
    let has = |label: &str| metadata.label_ids.iter().any(|id| id == label);
    ListedMessage {
        provider_key: metadata.id.clone(),
        label_ids: metadata.label_ids.clone(),
        raw_metadata: raw.into_bytes(),
        columns: HeaderColumns {
            subject: (!metadata.subject.is_empty()).then(|| metadata.subject.clone()),
            from_json: metadata
                .sender
                .as_ref()
                .and_then(|sender| addresses(std::slice::from_ref(sender))),
            reply_to_json: addresses(&metadata.reply_tos),
            to_json: addresses(&metadata.to_list),
            cc_json: addresses(&metadata.cc_list),
            date_header: millis(metadata.time),
            ..HeaderColumns::default()
        },
        internal_date: millis(metadata.time).map(Timestamp::from_millis),
        size: u64::try_from(metadata.size).ok(),
        flags: SyncedFlags {
            seen: !metadata.unread,
            flagged: has(STARRED),
            answered: metadata.flags & (FLAG_REPLIED | FLAG_REPLIED_ALL) != 0,
            draft: has(DRAFTS) || has(ALL_DRAFTS),
            deleted: false,
            keywords: if metadata.flags & FLAG_FORWARDED != 0 {
                vec![FORWARDED_KEYWORD.to_owned()]
            } else {
                Vec::new()
            },
        },
    }
}

/// The checkpoint a listed page leaves: the anchor's time and id
/// (`proton_accounts.listing_checkpoint`).
pub fn checkpoint(anchor: &ListingAnchor) -> String {
    format!("{}:{}", anchor.time, anchor.id)
}

/// The anchor a stored checkpoint names; `None` for one this version
/// didn't write, which restarts the listing from the newest message.
pub fn anchor(checkpoint: &str) -> Option<ListingAnchor> {
    let (time, id) = checkpoint.split_once(':')?;
    if id.is_empty() {
        return None;
    }
    Some(ListingAnchor {
        time: time.parse().ok()?,
        id: id.to_owned(),
    })
}

/// Address lists in `message_headers`' shape (`[{"address", "name"}]`).
fn addresses(list: &[MailAddress]) -> Option<String> {
    (!list.is_empty()).then(|| {
        list.iter()
            .map(|a| serde_json::json!({"address": a.address, "name": (!a.name.is_empty()).then_some(&a.name)}))
            .collect::<serde_json::Value>()
            .to_string()
    })
}

/// Proton's seconds as milliseconds; `None` outside the store's range.
fn millis(seconds: i64) -> Option<i64> {
    seconds.checked_mul(1000)
}

#[cfg(test)]
mod tests;
