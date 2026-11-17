//! Proton metadata onto the store's listed message (plan M2.15b).

use sb_proton::{ListingAnchor, MessageMetadata};
use serde_json::json;

use super::{anchor, checkpoint, listed_message};

fn metadata(value: serde_json::Value) -> MessageMetadata {
    let mut metadata: MessageMetadata = serde_json::from_value(value.clone()).expect("it decodes");
    metadata.raw = value;
    metadata
}

#[test]
fn metadata_maps_to_columns_flags_and_the_canonical_json() {
    let value = json!({
        "ID": "msg-1", "AddressID": "addr-1", "LabelIDs": ["0", "5", "10"],
        "Subject": "Hello", "Sender": {"Name": "Alice", "Address": "alice@example.test"},
        "ToList": [{"Name": "", "Address": "owner@example.test"}], "CCList": [], "BCCList": [],
        "ReplyTos": [], "Time": 1_700_000_000, "Size": 2048, "Unread": 0,
        "Flags": (1 << 5) | (1 << 7), "NumAttachments": 0
    });
    let listed = listed_message(&metadata(value.clone()));

    assert_eq!(listed.provider_key, "msg-1");
    assert_eq!(listed.label_ids, ["0", "5", "10"]);
    let raw: serde_json::Value = serde_json::from_slice(&listed.raw_metadata).unwrap();
    assert_eq!(raw, json!({"v": 1, "proton_metadata": value}));
    assert!(
        listed
            .raw_metadata
            .starts_with(br#"{"v":1,"proton_metadata":"#)
    );

    assert_eq!(listed.columns.subject.as_deref(), Some("Hello"));
    assert_eq!(
        listed.columns.from_json.as_deref(),
        Some(r#"[{"address":"alice@example.test","name":"Alice"}]"#)
    );
    assert_eq!(
        listed.columns.to_json.as_deref(),
        Some(r#"[{"address":"owner@example.test","name":null}]"#)
    );
    assert_eq!(listed.columns.cc_json, None);
    assert_eq!(listed.columns.date_header, Some(1_700_000_000_000));
    assert_eq!(listed.columns.in_reply_to, None);
    assert_eq!(
        listed.internal_date.map(|t| t.as_millis()),
        Some(1_700_000_000_000)
    );
    assert_eq!(listed.size, Some(2048));

    let flags = &listed.flags;
    assert!(flags.seen && flags.flagged && flags.answered && !flags.draft && !flags.deleted);
    assert_eq!(flags.keywords, ["$Forwarded"]);
}

#[test]
fn a_draft_unread_message_reads_as_such() {
    let listed = listed_message(&metadata(json!({
        "ID": "msg-2", "AddressID": "addr-1", "LabelIDs": ["1", "8"], "Subject": "",
        "ToList": [], "CCList": [], "BCCList": [], "ReplyTos": [], "Time": 1, "Size": 1,
        "Unread": 1, "Flags": 0, "NumAttachments": 0
    })));
    assert!(
        !listed.flags.seen && listed.flags.draft && !listed.flags.flagged && !listed.flags.answered
    );
    assert!(listed.flags.keywords.is_empty());
    assert_eq!(listed.columns.subject, None);
    assert_eq!(listed.columns.from_json, None);
}

#[test]
fn a_checkpoint_round_trips_its_anchor() {
    let position = ListingAnchor {
        time: 1_700_000_000,
        id: "abc-_=".to_owned(),
    };
    assert_eq!(anchor(&checkpoint(&position)), Some(position));
    for unreadable in ["", "x:abc", "12", "12:"] {
        assert_eq!(anchor(unreadable), None, "{unreadable}");
    }
}
