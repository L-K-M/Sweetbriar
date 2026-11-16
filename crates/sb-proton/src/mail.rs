//! Mail observations: labels, message metadata pages, per-label counts
//! and the events stream (spec/04 "Mailbox mapping", "Sync loop";
//! research `mail-sync.md` §§1-3, 5). Every call is a paced background
//! sync call and every response is decoded whole: a response that
//! doesn't decode is a failed request, never a partial observation
//! (spec/04 "Fault handling").

use std::fmt;

use serde::{Deserialize, Deserializer};
use serde_json::Value;

use crate::auth::Session;
use crate::client::{Client, Priority};
use crate::error::ProtonError;

/// `GET /core/v4/labels` (research `mail-sync.md` §1).
pub const LABELS_PATH: &str = "/core/v4/labels";
/// `GET /mail/v4/messages` (research `mail-sync.md` §3).
pub const MESSAGES_PATH: &str = "/mail/v4/messages";
/// `GET /mail/v4/messages/count` (research `mail-sync.md` §3).
pub const MESSAGE_COUNTS_PATH: &str = "/mail/v4/messages/count";
/// `GET /core/v4/events/latest` (research `mail-sync.md` §5).
pub const EVENTS_LATEST_PATH: &str = "/core/v4/events/latest";
/// `GET /core/v4/events/{id}` (research `mail-sync.md` §5).
pub const EVENTS_PATH: &str = "/core/v4/events";
/// The largest page the transport asks for (research `mail-sync.md`
/// §3: go-proton-api's client page size).
pub const MAX_PAGE_SIZE: u32 = 150;
/// The `Refresh` bit that invalidates local mail state
/// (research `mail-sync.md` §5).
pub const REFRESH_MAIL: i64 = 1;

/// A label's type (`go-proton-api/label_types.go`): user labels, folders
/// and system labels. Contact groups (2) are never requested; any
/// other type is refused as a decode failure (spec/04 "Fault
/// handling").
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LabelKind {
    Label,
    Folder,
    System,
}

impl<'de> Deserialize<'de> for LabelKind {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error as _;
        let kind = u8::deserialize(deserializer)?;
        match kind {
            1 => Ok(Self::Label),
            3 => Ok(Self::Folder),
            4 => Ok(Self::System),
            other => Err(D::Error::custom(format!(
                "unknown Proton label type {other}"
            ))),
        }
    }
}

/// One label from `GET /core/v4/labels` (research `mail-sync.md` §1).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct Label {
    #[serde(rename = "ID")]
    pub id: String,
    #[serde(rename = "ParentID", default)]
    pub parent_id: Option<String>,
    #[serde(rename = "Name")]
    pub name: String,
    /// Proton joins the path with `/` (research `mail-sync.md` §1).
    #[serde(rename = "Path")]
    pub path: String,
    #[serde(rename = "Color")]
    pub color: String,
    #[serde(rename = "Type")]
    pub kind: LabelKind,
}

#[derive(Debug, Deserialize)]
struct LabelsResponse {
    #[serde(rename = "Labels")]
    labels: Vec<Label>,
}

/// One address on a message (`go-proton-api/message_types.go`).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct MailAddress {
    #[serde(rename = "Name", default)]
    pub name: String,
    #[serde(rename = "Address")]
    pub address: String,
}

/// Message metadata as Proton lists it (`go-proton-api/message_types.go`).
/// `raw` keeps the object exactly as received, since spec/02 stores the
/// canonical metadata JSON until a body arrives.
#[derive(Debug, Clone, Deserialize)]
pub struct MessageMetadata {
    #[serde(rename = "ID")]
    pub id: String,
    #[serde(rename = "AddressID")]
    pub address_id: String,
    #[serde(rename = "LabelIDs")]
    pub label_ids: Vec<String>,
    #[serde(rename = "ExternalID", default)]
    pub external_id: Option<String>,
    #[serde(rename = "Subject")]
    pub subject: String,
    #[serde(rename = "Sender", default)]
    pub sender: Option<MailAddress>,
    #[serde(rename = "ToList", default)]
    pub to_list: Vec<MailAddress>,
    #[serde(rename = "CCList", default)]
    pub cc_list: Vec<MailAddress>,
    #[serde(rename = "BCCList", default)]
    pub bcc_list: Vec<MailAddress>,
    #[serde(rename = "ReplyTos", default)]
    pub reply_tos: Vec<MailAddress>,
    /// Seconds since the epoch.
    #[serde(rename = "Time")]
    pub time: i64,
    #[serde(rename = "Size")]
    pub size: i64,
    #[serde(rename = "Unread", deserialize_with = "proton_bool")]
    pub unread: bool,
    #[serde(rename = "Flags")]
    pub flags: u32,
    #[serde(rename = "NumAttachments")]
    pub num_attachments: u32,
    /// The metadata object as received (spec/02's `proton_metadata`).
    #[serde(skip)]
    pub raw: Value,
}

impl MessageMetadata {
    /// Decodes one metadata object and keeps its raw form. Any missing
    /// required field or wrong type fails the whole response.
    fn from_value(value: Value, path: &str) -> Result<Self, ProtonError> {
        let mut metadata: Self =
            serde_json::from_value(value.clone()).map_err(|e| ProtonError::decode(path, e))?;
        metadata.raw = value;
        Ok(metadata)
    }
}

/// An event's optional `Message`, decoded like a listed one: with its
/// raw object kept (spec/02's `proton_metadata`).
fn metadata_with_raw<'de, D: Deserializer<'de>>(
    deserializer: D,
) -> Result<Option<MessageMetadata>, D::Error> {
    let Some(value) = Option::<Value>::deserialize(deserializer)? else {
        return Ok(None);
    };
    let mut metadata: MessageMetadata =
        serde_json::from_value(value.clone()).map_err(serde::de::Error::custom)?;
    metadata.raw = value;
    Ok(Some(metadata))
}

/// One page of message metadata, newest first (spec/04 "Sync loop"
/// step 1.3).
#[derive(Debug, Clone)]
pub struct MessagePage {
    pub messages: Vec<MessageMetadata>,
    /// Proton's total across every page, for coverage (spec/04).
    pub total: u32,
    /// Proton asked for a retry of the listing (research
    /// `mail-sync.md` §3).
    pub stale: bool,
}

#[derive(Debug, Deserialize)]
struct MessagesResponse {
    #[serde(rename = "Messages")]
    messages: Vec<Value>,
    #[serde(rename = "Total")]
    total: u32,
    #[serde(rename = "Stale", deserialize_with = "proton_bool")]
    stale: bool,
}

/// One per-label total from `GET /mail/v4/messages/count`
/// (research `mail-sync.md` §3).
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct LabelCount {
    #[serde(rename = "LabelID")]
    pub label_id: String,
    #[serde(rename = "Total")]
    pub total: u32,
    #[serde(rename = "Unread")]
    pub unread: u32,
}

#[derive(Debug, Deserialize)]
struct CountsResponse {
    #[serde(rename = "Counts")]
    counts: Vec<LabelCount>,
}

/// The event cursor (`GET /core/v4/events/latest`).
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EventId(String);

impl EventId {
    pub fn as_str(&self) -> &str {
        &self.0
    }
}

impl fmt::Display for EventId {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

#[derive(Debug, Deserialize)]
struct LatestEventResponse {
    #[serde(rename = "EventID")]
    event_id: String,
}

/// What an event did to one item (research `mail-sync.md` §5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum EventAction {
    Delete,
    Create,
    Update,
    UpdateFlags,
}

impl<'de> Deserialize<'de> for EventAction {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        use serde::de::Error as _;
        let action = u8::deserialize(deserializer)?;
        match action {
            0 => Ok(Self::Delete),
            1 => Ok(Self::Create),
            2 => Ok(Self::Update),
            3 => Ok(Self::UpdateFlags),
            other => Err(D::Error::custom(format!(
                "unknown Proton event action {other}"
            ))),
        }
    }
}

/// One message change. `message` is absent for a delete.
#[derive(Debug, Clone, Deserialize)]
pub struct MessageEvent {
    #[serde(rename = "ID")]
    pub id: String,
    #[serde(rename = "Action")]
    pub action: EventAction,
    #[serde(rename = "Message", default, deserialize_with = "metadata_with_raw")]
    pub message: Option<MessageMetadata>,
}

/// One label change. `label` is absent for a delete.
#[derive(Debug, Clone, Deserialize)]
pub struct LabelEvent {
    #[serde(rename = "ID")]
    pub id: String,
    #[serde(rename = "Action")]
    pub action: EventAction,
    #[serde(rename = "Label", default)]
    pub label: Option<Label>,
}

/// One address change; the event carries only the id and the action.
#[derive(Debug, Clone, Deserialize)]
pub struct AddressEvent {
    #[serde(rename = "ID")]
    pub id: String,
    #[serde(rename = "Action")]
    pub action: EventAction,
}

/// One events response: the cursor after this batch, whether more
/// follow, and the mail refresh bit (spec/04 "Sync loop" step 2).
#[derive(Debug, Clone)]
pub struct EventBatch {
    pub event_id: EventId,
    pub more: bool,
    pub refresh_mail: bool,
    pub messages: Vec<MessageEvent>,
    pub labels: Vec<LabelEvent>,
    pub addresses: Vec<AddressEvent>,
    /// Proton included a `User` object, so identities may have changed.
    pub user_changed: bool,
}

#[derive(Debug, Deserialize)]
struct EventResponse {
    #[serde(rename = "EventID")]
    event_id: String,
    #[serde(rename = "More", default, deserialize_with = "proton_bool")]
    more: bool,
    #[serde(rename = "Refresh", default)]
    refresh: i64,
    #[serde(rename = "Messages", default)]
    messages: Vec<MessageEvent>,
    #[serde(rename = "Labels", default)]
    labels: Vec<LabelEvent>,
    #[serde(rename = "Addresses", default)]
    addresses: Vec<AddressEvent>,
    #[serde(rename = "User", default)]
    user: Option<Value>,
}

/// Proton's booleans are `0`/`1` on the wire (`go-proton-api/boolean.go`),
/// so both that and JSON `true`/`false` decode.
fn proton_bool<'de, D>(deserializer: D) -> Result<bool, D::Error>
where
    D: serde::Deserializer<'de>,
{
    use serde::de::Error as _;
    match Value::deserialize(deserializer)? {
        Value::Bool(value) => Ok(value),
        Value::Number(value) => match value.as_u64() {
            Some(0) => Ok(false),
            Some(1) => Ok(true),
            _ => Err(D::Error::custom(
                "a Proton boolean must be true, false, 0 or 1",
            )),
        },
        _ => Err(D::Error::custom(
            "a Proton boolean must be true, false, 0 or 1",
        )),
    }
}

impl Client {
    /// `GET /core/v4/labels?Type=1&Type=3&Type=4` (spec/04 "Mailbox
    /// mapping"): user labels, folders and system labels. Contact
    /// groups aren't requested; an unknown type fails the whole call.
    pub async fn labels(&self, session: &Session) -> Result<Vec<Label>, ProtonError> {
        let path = format!("{LABELS_PATH}?Type=1&Type=3&Type=4");
        let response: LabelsResponse = self.get_json(&path, session, Priority::Background).await?;
        Ok(response.labels)
    }

    /// One page of message metadata, newest first by time (spec/04
    /// "Sync loop" step 1.3). A page size above 150 is clamped to 150
    /// (research `mail-sync.md` §3).
    pub async fn message_page(
        &self,
        session: &Session,
        page: u32,
        page_size: u32,
    ) -> Result<MessagePage, ProtonError> {
        let page_size = page_size.min(MAX_PAGE_SIZE);
        let path = format!("{MESSAGES_PATH}?Page={page}&PageSize={page_size}&Sort=Time&Desc=1");
        let response: MessagesResponse =
            self.get_json(&path, session, Priority::Background).await?;
        let mut messages = Vec::with_capacity(response.messages.len());
        for (index, value) in response.messages.into_iter().enumerate() {
            let at = format!("{MESSAGES_PATH} (message {index} of page {page})");
            messages.push(MessageMetadata::from_value(value, &at)?);
        }
        Ok(MessagePage {
            messages,
            total: response.total,
            stale: response.stale,
        })
    }

    /// `GET /mail/v4/messages/count`: per-label totals and unread counts
    /// (spec/04 "Sync loop" step 1.4).
    pub async fn message_counts(&self, session: &Session) -> Result<Vec<LabelCount>, ProtonError> {
        let response: CountsResponse = self
            .get_json(MESSAGE_COUNTS_PATH, session, Priority::Background)
            .await?;
        Ok(response.counts)
    }

    /// `GET /core/v4/events/latest`: the sync checkpoint to read before
    /// listing (spec/04 "Sync loop" step 1.1).
    pub async fn latest_event(&self, session: &Session) -> Result<EventId, ProtonError> {
        let response: LatestEventResponse = self
            .get_json(EVENTS_LATEST_PATH, session, Priority::Background)
            .await?;
        Ok(EventId(response.event_id))
    }

    /// `GET /core/v4/events/{id}`: the changes since a cursor, with
    /// `More` when the caller must poll again (spec/04 "Sync loop"
    /// step 2).
    pub async fn events_since(
        &self,
        session: &Session,
        cursor: &EventId,
    ) -> Result<EventBatch, ProtonError> {
        let path = format!("{EVENTS_PATH}/{}", cursor.as_str());
        let response: EventResponse = self.get_json(&path, session, Priority::Background).await?;
        require_payloads(&response, &path)?;
        Ok(EventBatch {
            event_id: EventId(response.event_id),
            more: response.more,
            refresh_mail: response.refresh & REFRESH_MAIL != 0,
            messages: response.messages,
            labels: response.labels,
            addresses: response.addresses,
            user_changed: response.user.is_some(),
        })
    }
}

/// A create or update event must carry its object; only a delete may
/// omit it.
fn require_payloads(response: &EventResponse, path: &str) -> Result<(), ProtonError> {
    for event in &response.messages {
        if event.message.is_none() && event.action != EventAction::Delete {
            return Err(ProtonError::Malformed {
                message: format!(
                    "the {path} response omitted the message of a {:?} event",
                    event.action
                ),
            });
        }
    }
    for event in &response.labels {
        if event.label.is_none() && event.action != EventAction::Delete {
            return Err(ProtonError::Malformed {
                message: format!(
                    "the {path} response omitted the label of a {:?} event",
                    event.action
                ),
            });
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    fn metadata_json() -> Value {
        json!({
            "ID": "msg-1",
            "AddressID": "addr-1",
            "LabelIDs": ["0", "5"],
            "ExternalID": "<msg-1@example.test>",
            "Subject": "synthetic subject",
            "Sender": {"Name": "Sender", "Address": "sender@example.test"},
            "ToList": [{"Name": "To", "Address": "to@example.test"}],
            "CCList": [{"Name": "Cc", "Address": "cc@example.test"}],
            "BCCList": [{"Name": "Bcc", "Address": "bcc@example.test"}],
            "ReplyTos": [{"Name": "Reply", "Address": "reply@example.test"}],
            "Time": 1_700_000_000i64,
            "Size": 128,
            "Unread": 1,
            "Flags": 1,
            "NumAttachments": 2,
        })
    }

    #[derive(Deserialize)]
    struct BoolProbe {
        #[serde(deserialize_with = "proton_bool")]
        value: bool,
    }

    #[test]
    fn message_metadata_decodes_recipients_and_keeps_the_raw_object() {
        let metadata =
            MessageMetadata::from_value(metadata_json(), MESSAGES_PATH).expect("it decodes");
        assert_eq!(metadata.label_ids, ["0", "5"]);
        assert_eq!(metadata.to_list.len(), 1);
        assert_eq!(metadata.cc_list[0].address, "cc@example.test");
        assert_eq!(metadata.bcc_list[0].name, "Bcc");
        assert_eq!(metadata.reply_tos[0].address, "reply@example.test");
        assert!(metadata.unread);
        assert_eq!(metadata.raw["ID"], "msg-1");
    }

    #[test]
    fn proton_booleans_accept_true_false_and_zero_one() {
        for (wire, expected) in [
            (json!(true), true),
            (json!(false), false),
            (json!(1), true),
            (json!(0), false),
        ] {
            let probe: BoolProbe =
                serde_json::from_value(json!({"value": wire})).expect("it decodes");
            assert_eq!(probe.value, expected);
        }
        assert!(serde_json::from_value::<BoolProbe>(json!({"value": 2})).is_err());
    }

    #[test]
    fn a_message_with_a_missing_or_mistyped_field_is_a_decode_failure() {
        let missing = MessageMetadata::from_value(json!({"AddressID": "addr-1"}), MESSAGES_PATH)
            .expect_err("an ID is required");
        assert!(matches!(missing, ProtonError::Malformed { .. }));

        let mut mistyped = metadata_json();
        mistyped["Time"] = json!("not-a-number");
        let mistyped =
            MessageMetadata::from_value(mistyped, MESSAGES_PATH).expect_err("Time must be seconds");
        assert!(matches!(mistyped, ProtonError::Malformed { .. }));
    }

    #[test]
    fn an_unknown_label_type_is_a_decode_failure() {
        let error = serde_json::from_value::<LabelsResponse>(json!({"Labels": [
            {"ID": "contact-1", "Name": "Contacts", "Path": "Contacts", "Color": "", "Type": 2}
        ]}))
        .expect_err("a contact group isn't a mapped label");
        assert!(error.to_string().contains("label type"));
    }

    #[test]
    fn event_actions_decode_and_unknown_actions_fail() {
        #[derive(Debug, Deserialize)]
        struct Probe {
            #[serde(rename = "Action")]
            action: EventAction,
        }
        for (wire, expected) in [
            (json!(0), EventAction::Delete),
            (json!(1), EventAction::Create),
            (json!(2), EventAction::Update),
            (json!(3), EventAction::UpdateFlags),
        ] {
            let probe: Probe = serde_json::from_value(json!({"Action": wire})).expect("it decodes");
            assert_eq!(probe.action, expected);
        }
        assert!(serde_json::from_value::<Probe>(json!({"Action": 4})).is_err());
    }

    #[test]
    fn an_event_message_keeps_its_raw_object_and_must_decode() {
        let event: MessageEvent =
            serde_json::from_value(json!({"ID": "msg-1", "Action": 1, "Message": metadata_json()}))
                .expect("it decodes");
        let message = event.message.expect("a create carries its message");
        assert_eq!(message.raw, metadata_json());

        let delete: MessageEvent =
            serde_json::from_value(json!({"ID": "msg-1", "Action": 0, "Message": null}))
                .expect("a delete may carry none");
        assert!(delete.message.is_none());

        let mistyped = json!({"ID": "msg-1", "Action": 1, "Message": {"ID": 7}});
        assert!(serde_json::from_value::<MessageEvent>(mistyped).is_err());
    }

    #[test]
    fn create_and_update_events_must_carry_their_object() {
        let response = |messages: Value, labels: Value| -> EventResponse {
            serde_json::from_value(json!({
                "EventID": "evt-1", "Messages": messages, "Labels": labels
            }))
            .expect("it decodes")
        };
        let deletes = response(
            json!([{"ID": "msg-1", "Action": 0}]),
            json!([{"ID": "label-1", "Action": 0}]),
        );
        assert!(require_payloads(&deletes, EVENTS_PATH).is_ok());

        let bare_message = response(json!([{"ID": "msg-1", "Action": 2}]), json!([]));
        let error = require_payloads(&bare_message, EVENTS_PATH).expect_err("an update needs it");
        assert!(error.to_string().contains("omitted the message"));

        let bare_label = response(json!([]), json!([{"ID": "label-1", "Action": 1}]));
        let error = require_payloads(&bare_label, EVENTS_PATH).expect_err("a create needs it");
        assert!(error.to_string().contains("omitted the label"));
    }
}
