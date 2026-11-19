//! The fake's send calls (spec/11 "The fake Proton API server", plan
//! M4.3c): recipient key lookup, draft create and update, attachment
//! upload, and send, with the faults 05's `outcome_unknown` and retry
//! paths need. Written from the protocol facts in research/proton-api
//! (`keys-crypto.md` §4-5, `mail-sync.md` §4, §7); no Proton or Bridge
//! code.

use std::collections::HashMap;
use std::io::{self, Cursor, Read};
use std::sync::{Arc, Mutex};

use base64::Engine as _;
use bytes::Bytes;
use http_body_util::Full;
use hyper::Response;
use pgp::composed::{Message, PlainSessionKey, RawSessionKey};
use pgp::crypto::sym::SymmetricKeyAlgorithm;
use proton_crypto::crypto::{
    DataEncoding, Decryptor, DecryptorSync, PGPProviderSync, SessionKey, VerifiedData,
};
use serde_json::{Value, json};

use super::{
    FakeAttachment, FakeEvent, FakeMessage, FakeProtonServer, State, attachment_json,
    authenticated, header_str, json_response, message_body_json, query_value, unauthorized,
};

/// Proton's internal scheme: session keys are encrypted to the
/// recipient's key and carried as `BodyKeyPacket` (research
/// `keys-crypto.md` §4).
pub const INTERNAL_SCHEME: u8 = 1;
/// Proton's clear scheme: the session key is carried in `BodyKey`.
pub const CLEAR_SCHEME: u8 = 4;

const DRAFTS_LABEL: &str = "8";
const SENT_LABEL: &str = "7";
const ALL_MAIL_LABEL: &str = "5";

/// One applied send: the draft it sent and the packages it carried.
#[derive(Debug, Clone)]
pub struct RecordedSend {
    pub draft_id: String,
    pub packages: Vec<RecordedPackage>,
}

/// One package of a send: its MIME type, scheme, body data packet and,
/// for the clear scheme, the session key.
#[derive(Debug, Clone)]
pub struct RecordedPackage {
    pub mime_type: String,
    pub scheme: u8,
    pub body: Vec<u8>,
    pub body_key: Option<RecordedSessionKey>,
    pub recipients: Vec<RecordedRecipient>,
}

/// A clear session key as the package carried it.
#[derive(Debug, Clone)]
pub struct RecordedSessionKey {
    pub key: Vec<u8>,
    pub algorithm: String,
}

/// One recipient's entry in a package: its scheme and body key packet.
#[derive(Debug, Clone)]
pub struct RecordedRecipient {
    pub address: String,
    pub scheme: u8,
    pub body_key_packet: Vec<u8>,
}

/// An internal recipient's unlocked address key, for tests that
/// decrypt what a send package or an uploaded attachment carries
/// (spec/11 M4.3c).
pub trait RecipientKey: Send + Sync {
    /// The armored public key `GET /core/v4/keys` serves for it.
    fn public_key(&self) -> &str;
    /// Decrypts a recipient's `BodyKeyPacket` to the session key bytes.
    fn decrypt_session_key(&self, key_packets: &[u8]) -> Option<Vec<u8>>;
    /// Decrypts a body data packet for the internal scheme.
    fn decrypt_body(&self, key_packets: &[u8], data_packet: &[u8]) -> Option<Vec<u8>>;
    /// Decrypts a clear body data packet with the package's `BodyKey`.
    fn decrypt_clear_body(
        &self,
        algorithm: &str,
        key: &[u8],
        data_packet: &[u8],
    ) -> Option<Vec<u8>>;
}

/// Builds a [`RecipientKey`] for a generated address key.
pub(crate) fn recipient_key<P: PGPProviderSync>(
    provider: &P,
    public_key: String,
    private_key: &P::PrivateKey,
) -> Arc<dyn RecipientKey> {
    Arc::new(RecipientKeyImpl {
        provider: provider.clone(),
        private_key: private_key.clone(),
        public_key,
    })
}

struct RecipientKeyImpl<P: PGPProviderSync> {
    provider: P,
    private_key: P::PrivateKey,
    public_key: String,
}

impl<P: PGPProviderSync> RecipientKeyImpl<P> {
    fn session_key(&self, key_packets: &[u8]) -> Option<P::SessionKey> {
        self.provider
            .new_decryptor()
            .with_decryption_key(&self.private_key)
            .decrypt_session_key(key_packets)
            .ok()
    }
}

impl<P: PGPProviderSync> RecipientKey for RecipientKeyImpl<P> {
    fn public_key(&self) -> &str {
        &self.public_key
    }

    fn decrypt_session_key(&self, key_packets: &[u8]) -> Option<Vec<u8>> {
        Some(self.session_key(key_packets)?.export().as_ref().to_vec())
    }

    fn decrypt_body(&self, key_packets: &[u8], data_packet: &[u8]) -> Option<Vec<u8>> {
        let session = self.session_key(key_packets)?;
        Some(
            self.provider
                .new_decryptor()
                .with_session_key(session)
                .decrypt(data_packet, DataEncoding::Bytes)
                .ok()?
                .to_vec(),
        )
    }

    fn decrypt_clear_body(
        &self,
        algorithm: &str,
        key: &[u8],
        data_packet: &[u8],
    ) -> Option<Vec<u8>> {
        let sym_alg = match algorithm {
            "aes128" => SymmetricKeyAlgorithm::AES128,
            "aes256" => SymmetricKeyAlgorithm::AES256,
            _ => return None,
        };
        // A clear body is a bare data packet; `pgp` (already a
        // dependency for the test modulus) builds a session key from
        // its exported bytes.
        let plain = Message::from_bytes(Cursor::new(data_packet))
            .ok()?
            .decrypt_with_session_key(PlainSessionKey::V3_4 {
                sym_alg,
                key: RawSessionKey::from(key.to_vec()),
            })
            .ok()?;
        let mut out = Vec::new();
        match plain {
            Message::Literal { mut reader, .. } => {
                reader.read_to_end(&mut out).ok()?;
            }
            Message::Compressed { mut reader, .. } => {
                reader.read_to_end(&mut out).ok()?;
            }
            _ => return None,
        }
        Some(out)
    }
}

/// `GET /core/v4/keys?Email=`: an internal address answers 1 with its
/// public keys; an external address answers 2, with keys only when a
/// test registered them (research `keys-crypto.md` §5).
pub(super) fn handle_keys(
    state: &Arc<Mutex<State>>,
    headers: &hyper::HeaderMap,
    query: Option<&str>,
) -> Result<Response<Full<Bytes>>, io::Error> {
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    let Some(username) = authenticated(&mut state, headers) else {
        return Ok(unauthorized());
    };
    let email = query_value(query, "Email").unwrap_or_default();
    let internal = state
        .accounts
        .get(&username)
        .expect("the session's account exists")
        .addresses
        .iter()
        .find(|address| address.email.eq_ignore_ascii_case(&email));
    if let Some(address) = internal {
        let keys: Vec<Value> = address
            .public_keys
            .iter()
            .map(|key| json!({"Flags": 3, "PublicKey": key}))
            .collect();
        return Ok(json_response(
            200,
            json!({"Code": 1000, "Keys": keys, "RecipientType": 1}),
        ));
    }
    let keys: Vec<Value> = state
        .external_keys
        .get(&email.to_lowercase())
        .map(|keys| {
            keys.iter()
                .map(|key| json!({"Flags": 3, "PublicKey": key}))
                .collect()
        })
        .unwrap_or_default();
    Ok(json_response(
        200,
        json!({"Code": 1000, "Keys": keys, "RecipientType": 2}),
    ))
}

/// The fields a draft create or update requires (research
/// `mail-sync.md` §7: `Sender`, `Sender.Address` and a `MIMEType` of
/// `text/plain` or `text/html`).
struct DraftFields {
    subject: String,
    sender: Value,
    sender_address: String,
    to_list: Vec<Value>,
    cc_list: Vec<Value>,
    bcc_list: Vec<Value>,
    body: String,
    mime_type: String,
}

fn draft_fields(message: &Value) -> Option<DraftFields> {
    let sender = message.get("Sender")?.clone();
    let sender_address = sender.get("Address")?.as_str()?.to_owned();
    let mime_type = message
        .get("MIMEType")
        .and_then(Value::as_str)
        .unwrap_or_default();
    if sender_address.is_empty() || !matches!(mime_type, "text/plain" | "text/html") {
        return None;
    }
    let list = |key: &str| {
        message
            .get(key)
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default()
    };
    Some(DraftFields {
        subject: message
            .get("Subject")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        sender,
        sender_address,
        to_list: list("ToList"),
        cc_list: list("CCList"),
        bcc_list: list("BCCList"),
        body: message
            .get("Body")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        mime_type: mime_type.to_owned(),
    })
}

fn apply_draft_fields(draft: &mut FakeMessage, id: &str, fields: DraftFields) {
    draft.subject = fields.subject.clone();
    draft.sender = fields.sender;
    draft.to_list = fields.to_list;
    draft.cc_list = fields.cc_list;
    draft.bcc_list = fields.bcc_list;
    draft.mime_type = fields.mime_type;
    draft.header = format!(
        "Subject: {subject}\r\nFrom: {sender}\r\nDate: Tue, 14 Nov 2023 22:13:20 +0000\r\nMessage-ID: <{id}@example.test>\r\n",
        subject = fields.subject,
        sender = fields.sender_address,
    );
    draft.body = fields.body.as_bytes().to_vec();
    draft.encrypted_body = Some(fields.body);
}

/// Proton's `InvalidValue` (2001) with the given HTTP status.
fn error(status: u16, message: &str) -> Response<Full<Bytes>> {
    json_response(status, json!({"Code": 2001, "Error": message}))
}

/// A draft is a message carrying Drafts `8` and not Sent `7`.
fn is_draft(message: &FakeMessage) -> bool {
    message.label_ids.iter().any(|label| label == DRAFTS_LABEL)
        && !message.label_ids.iter().any(|label| label == SENT_LABEL)
}

/// `POST /mail/v4/messages` creates a draft and `PUT
/// /mail/v4/messages/{id}` updates one (research `mail-sync.md` §7).
/// The body is the client's armored body; the fake stores it as served.
pub(super) fn handle_draft_write(
    state: &Arc<Mutex<State>>,
    headers: &hyper::HeaderMap,
    id: Option<&str>,
    body: &[u8],
) -> Result<Response<Full<Bytes>>, io::Error> {
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    let Some(username) = authenticated(&mut state, headers) else {
        return Ok(unauthorized());
    };
    let value: Value = serde_json::from_slice(body).unwrap_or(json!({}));
    let message = value.get("Message").cloned().unwrap_or(json!({}));
    let Some(fields) = draft_fields(&message) else {
        return Ok(error(422, "InvalidValue"));
    };
    let created = id.is_none();
    let new_id = created.then(|| {
        state.counter += 1;
        format!("draft-{}", state.counter)
    });
    let id = id
        .map(str::to_owned)
        .or_else(|| new_id.clone())
        .expect("an id or a creation");
    let account = state
        .accounts
        .get_mut(&username)
        .expect("the session's account exists");
    let existing = account.messages.iter_mut().find(|message| message.id == id);
    let Some(draft) = existing else {
        if !created {
            return Ok(error(404, "message not found"));
        }
        let address_id = account
            .addresses
            .iter()
            .find(|address| address.email.eq_ignore_ascii_case(&fields.sender_address))
            .map(|address| address.id.clone())
            .unwrap_or_else(|| "owner-primary".to_owned());
        let mut draft = FakeMessage::new(&id).with_labels(&[DRAFTS_LABEL, ALL_MAIL_LABEL]);
        draft.flags = 0;
        draft.unread = false;
        draft.address_id = address_id;
        apply_draft_fields(&mut draft, &id, fields);
        let json = message_body_json(&draft);
        account.record(FakeEvent::message(id.clone(), 1, Some(draft.clone())));
        account.messages.push(draft);
        return Ok(json_response(200, json!({"Code": 1000, "Message": json})));
    };
    if !is_draft(draft) {
        return Ok(error(400, "message is not a draft"));
    }
    apply_draft_fields(draft, &id, fields);
    let clone = draft.clone();
    let json = message_body_json(&clone);
    account.record(FakeEvent::message(id, 2, Some(clone)));
    Ok(json_response(200, json!({"Code": 1000, "Message": json})))
}

/// `POST /mail/v4/attachments`: multipart fields plus the key packets,
/// data packet and detached signature blobs (research `mail-sync.md`
/// §4). The attachment joins the named draft; its reported size is the
/// decrypted data's length, the measure 05's retry matching uses.
pub(super) fn handle_upload_attachment(
    state: &Arc<Mutex<State>>,
    headers: &hyper::HeaderMap,
    body: &[u8],
) -> Result<Response<Full<Bytes>>, io::Error> {
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    let Some(username) = authenticated(&mut state, headers) else {
        return Ok(unauthorized());
    };
    let content_type = header_str(headers, "content-type").unwrap_or_default();
    let Some(form) = parse_multipart(&content_type, body) else {
        return Ok(error(400, "invalid multipart body"));
    };
    let field = |name: &str| form.field(name).map(str::to_owned);
    let (Some(message_id), Some(filename), Some(mime_type)) =
        (field("MessageID"), field("Filename"), field("MIMEType"))
    else {
        return Ok(error(400, "MessageID, Filename and MIMEType are required"));
    };
    let disposition = form.field("Disposition").unwrap_or("attachment");
    let content_id = form.field("ContentID").filter(|value| !value.is_empty());
    let key_packets = form.blob("KeyPackets").unwrap_or_default();
    let data_packet = form.blob("DataPacket").unwrap_or_default();

    let account = state
        .accounts
        .get_mut(&username)
        .expect("the session's account exists");
    let Some(draft) = account
        .messages
        .iter_mut()
        .find(|message| message.id == message_id)
    else {
        return Ok(error(404, "message not found"));
    };
    if !is_draft(draft) {
        return Ok(error(400, "message is not a draft"));
    }
    let sender = draft
        .sender
        .get("Address")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_lowercase();
    let recipient = account.recipient_keys.get(&sender).cloned();
    let Some(plaintext) = recipient
        .as_ref()
        .and_then(|key| key.decrypt_body(key_packets, data_packet))
    else {
        return Ok(json_response(
            422,
            json!({"Code": 2001, "Error": "the attachment cannot be decrypted"}),
        ));
    };
    let attachment_id = format!("{}-att-{}", message_id, draft.attachments.len());
    draft.attachments.push(FakeAttachment {
        name: filename,
        mime_type,
        inline: disposition == "inline",
        content_ids: content_id.map(|id| vec![id.to_owned()]).unwrap_or_default(),
        plaintext,
    });
    draft.encrypted_attachments.push((
        attachment_id.clone(),
        key_packets.to_vec(),
        data_packet.to_vec(),
    ));
    let clone = draft.clone();
    let json = attachment_json(
        &attachment_id,
        clone.attachments.last().expect("the attachment was pushed"),
        key_packets,
    );
    account.record(FakeEvent::message(message_id, 2, Some(clone)));
    Ok(json_response(
        200,
        json!({"Code": 1000, "Attachment": json}),
    ))
}

/// `POST /mail/v4/messages/{id}`: send a draft. Records each package
/// and recipient, moves the draft to Sent, emits the update event a
/// real server would and counts the send.
pub(super) fn handle_send(
    state: &Arc<Mutex<State>>,
    headers: &hyper::HeaderMap,
    id: &str,
    body: &[u8],
) -> Result<Response<Full<Bytes>>, io::Error> {
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    let Some(username) = authenticated(&mut state, headers) else {
        return Ok(unauthorized());
    };
    let value: Value = serde_json::from_slice(body).unwrap_or(json!({}));
    let packages = value
        .get("Packages")
        .and_then(Value::as_array)
        .cloned()
        .unwrap_or_default();
    let recorded: Vec<RecordedPackage> = packages.iter().map(record_package).collect();
    let sent = {
        let account = state
            .accounts
            .get_mut(&username)
            .expect("the session's account exists");
        let Some(draft) = account.messages.iter_mut().find(|message| message.id == id) else {
            return Ok(error(404, "message not found"));
        };
        if !is_draft(draft) {
            return Ok(error(400, "message is not a draft"));
        }
        draft.label_ids.retain(|label| label != DRAFTS_LABEL);
        if !draft.label_ids.iter().any(|label| label == SENT_LABEL) {
            draft.label_ids.push(SENT_LABEL.to_owned());
        }
        draft.flags |= 2;
        let clone = draft.clone();
        let json = message_body_json(&clone);
        account.record(FakeEvent::message(id.to_owned(), 2, Some(clone)));
        json
    };
    state.recorded_sends.push(RecordedSend {
        draft_id: id.to_owned(),
        packages: recorded,
    });
    Ok(json_response(200, json!({"Code": 1000, "Sent": sent})))
}

/// Records one package: its MIME type, scheme, body data packet, clear
/// body session key and each recipient's scheme and body key packet.
fn record_package(package: &Value) -> RecordedPackage {
    let recipients = package
        .get("Addresses")
        .and_then(Value::as_object)
        .map(|addresses| {
            addresses
                .iter()
                .map(|(address, value)| RecordedRecipient {
                    address: address.clone(),
                    scheme: value.get("Type").and_then(Value::as_u64).unwrap_or(0) as u8,
                    body_key_packet: decode_base64(
                        value
                            .get("BodyKeyPacket")
                            .and_then(Value::as_str)
                            .unwrap_or_default(),
                    ),
                })
                .collect()
        })
        .unwrap_or_default();
    RecordedPackage {
        mime_type: package
            .get("MIMEType")
            .and_then(Value::as_str)
            .unwrap_or_default()
            .to_owned(),
        scheme: package.get("Type").and_then(Value::as_u64).unwrap_or(0) as u8,
        body: decode_base64(
            package
                .get("Body")
                .and_then(Value::as_str)
                .unwrap_or_default(),
        ),
        body_key: package.get("BodyKey").and_then(|key| {
            Some(RecordedSessionKey {
                key: decode_base64(key.get("Key").and_then(Value::as_str)?),
                algorithm: key.get("Algorithm").and_then(Value::as_str)?.to_owned(),
            })
        }),
        recipients,
    }
}

fn decode_base64(value: &str) -> Vec<u8> {
    base64::engine::general_purpose::STANDARD
        .decode(value)
        .unwrap_or_default()
}

/// A parsed `multipart/form-data` body: text fields and binary blobs.
#[derive(Default)]
struct MultipartForm {
    fields: HashMap<String, String>,
    blobs: HashMap<String, Vec<u8>>,
}

impl MultipartForm {
    fn field(&self, name: &str) -> Option<&str> {
        self.fields.get(name).map(String::as_str)
    }

    fn blob(&self, name: &str) -> Option<&[u8]> {
        self.blobs.get(name).map(Vec::as_slice)
    }
}

fn parse_multipart(content_type: &str, body: &[u8]) -> Option<MultipartForm> {
    let boundary = content_type.split(';').map(str::trim).find_map(|part| {
        let (name, value) = part.split_once('=')?;
        name.eq_ignore_ascii_case("boundary")
            .then(|| value.trim_matches('"').to_owned())
    })?;
    let delimiter = format!("--{boundary}");
    let mut form = MultipartForm::default();
    for segment in split_by(body, delimiter.as_bytes()).iter().skip(1) {
        if segment.starts_with(b"--") {
            break;
        }
        let part = segment.strip_prefix(b"\r\n").unwrap_or(segment);
        let part = part.strip_suffix(b"\r\n").unwrap_or(part);
        let Some(split) = find_subslice(part, b"\r\n\r\n") else {
            continue;
        };
        let Some(name) = content_disposition_name(&part[..split]) else {
            continue;
        };
        let content = &part[split + 4..];
        if matches!(name.as_str(), "KeyPackets" | "DataPacket" | "Signature") {
            form.blobs.insert(name, content.to_vec());
        } else {
            form.fields
                .insert(name, String::from_utf8_lossy(content).into_owned());
        }
    }
    Some(form)
}

/// The `name` of a part's `Content-Disposition`.
fn content_disposition_name(headers: &[u8]) -> Option<String> {
    let headers = String::from_utf8_lossy(headers);
    let disposition = headers.lines().find_map(|line| {
        let (name, value) = line.split_once(':')?;
        name.eq_ignore_ascii_case("content-disposition")
            .then(|| value.trim())
    })?;
    disposition.split(';').find_map(|part| {
        let (key, value) = part.trim().split_once('=')?;
        key.trim()
            .eq_ignore_ascii_case("name")
            .then(|| value.trim().trim_matches('"').to_owned())
    })
}

fn split_by<'a>(body: &'a [u8], delimiter: &[u8]) -> Vec<&'a [u8]> {
    let mut segments = Vec::new();
    let mut start = 0;
    while let Some(position) = find_subslice(&body[start..], delimiter) {
        segments.push(&body[start..start + position]);
        start += position + delimiter.len();
    }
    segments.push(&body[start..]);
    segments
}

fn find_subslice(haystack: &[u8], needle: &[u8]) -> Option<usize> {
    if needle.is_empty() || haystack.len() < needle.len() {
        return None;
    }
    haystack
        .windows(needle.len())
        .position(|window| window == needle)
}

impl FakeProtonServer {
    /// Registers an armored public key for an external address, so a
    /// client's "external with keys" refusal can be tested (spec/11
    /// M4.3c).
    pub fn add_external_key(&self, email: &str, public_key: &str) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .external_keys
            .entry(email.to_lowercase())
            .or_default()
            .push(public_key.to_owned());
    }

    /// The unlocked key of an internal recipient, for tests that
    /// decrypt packages and uploads as that recipient.
    pub fn recipient_key(&self, username: &str, email: &str) -> Option<Arc<dyn RecipientKey>> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .accounts
            .get(username)?
            .recipient_keys
            .get(&email.to_lowercase())
            .cloned()
    }

    /// How many sends the fake has applied.
    pub fn sends(&self) -> usize {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.recorded_sends.len()
    }

    /// Every applied send, in order, with the packages it carried.
    pub fn recorded_sends(&self) -> Vec<RecordedSend> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.recorded_sends.clone()
    }
}

#[cfg(test)]
mod tests {
    use std::io;

    use base64::Engine as _;
    use bytes::Bytes;
    use http_body_util::{BodyExt, Full};
    use hyper::{Request, StatusCode};
    use hyper_util::rt::TokioIo;
    use proton_crypto::ProtonPGP;
    use proton_crypto::crypto::{
        DataEncoding, Encryptor, EncryptorSync, PGPProviderSync, SessionKey, SessionKeyAlgorithm,
        SigningMode, WritingMode,
    };
    use rustls::pki_types::ServerName;
    use serde_json::{Value, json};
    use tokio::net::TcpStream;
    use tokio_rustls::TlsConnector;

    use crate::{FakeProtonServer, TestAccount};

    use super::{CLEAR_SCHEME, INTERNAL_SCHEME};

    const USER: &str = "owner@example.org";
    const INTERNAL: &str = "owner@example.test";
    const CONTENT_TYPE: &str = "multipart/form-data; boundary=synthetic-boundary";
    const DRAFT_BODY: &str =
        "-----BEGIN PGP MESSAGE-----\r\n\r\nsynthetic\r\n-----END PGP MESSAGE-----\r\n";

    async fn serve() -> FakeProtonServer {
        FakeProtonServer::serve(vec![TestAccount::new(USER, "synthetic-owner-password")])
            .await
            .expect("serve")
    }

    /// Inserts a synthetic authenticated session, so the tests drive the
    /// fake's send calls rather than the SRP handshake.
    fn session(server: &FakeProtonServer) -> (String, String) {
        let (uid, access) = ("uid-test".to_owned(), "access-test".to_owned());
        server
            .state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .sessions
            .insert(
                uid.clone(),
                super::super::Session {
                    user_id: USER.to_owned(),
                    access_token: access.clone(),
                    refresh_token: "refresh-test".to_owned(),
                    password_mode: 1,
                },
            );
        (uid, access)
    }

    struct TestClient {
        base_url: String,
        uid: String,
        access: String,
    }

    impl TestClient {
        fn new(server: &FakeProtonServer) -> Self {
            let (uid, access) = session(server);
            Self {
                base_url: server.base_url().to_owned(),
                uid,
                access,
            }
        }

        async fn request(
            &self,
            method: &str,
            path: &str,
            body: Vec<u8>,
            content_type: &str,
        ) -> Result<(StatusCode, Bytes), String> {
            let stream = tls_connect(&self.base_url)
                .await
                .map_err(|e| e.to_string())?;
            let (mut sender, connection) =
                hyper::client::conn::http1::handshake(TokioIo::new(stream))
                    .await
                    .map_err(|e| e.to_string())?;
            tokio::spawn(async move {
                let _ = connection.await;
            });
            let request = Request::builder()
                .method(method)
                .uri(format!("{}{}", self.base_url, path))
                .header("x-pm-uid", &self.uid)
                .header("authorization", format!("Bearer {}", self.access))
                .header("x-pm-appversion", "Other")
                .header("user-agent", "Sweetbriar-test")
                .header("content-type", content_type)
                .body(Full::new(Bytes::from(body)))
                .map_err(|e| e.to_string())?;
            let response = sender
                .send_request(request)
                .await
                .map_err(|e| e.to_string())?;
            let status = response.status();
            let body = response
                .into_body()
                .collect()
                .await
                .map_err(|e| e.to_string())?
                .to_bytes();
            Ok((status, body))
        }

        async fn json(
            &self,
            method: &str,
            path: &str,
            value: Value,
        ) -> Result<(StatusCode, Value), String> {
            let (status, body) = self
                .request(
                    method,
                    path,
                    value.to_string().into_bytes(),
                    "application/json",
                )
                .await?;
            Ok((status, serde_json::from_slice(&body).unwrap_or(Value::Null)))
        }

        async fn ok(&self, method: &str, path: &str, value: Value) -> Value {
            let (status, body) = self
                .json(method, path, value)
                .await
                .expect("the request succeeds");
            assert_eq!(status, StatusCode::OK, "{body}");
            assert_eq!(body["Code"], 1000, "{body}");
            body
        }
    }

    async fn tls_connect(base_url: &str) -> io::Result<tokio_rustls::client::TlsStream<TcpStream>> {
        let authority = base_url.strip_prefix("https://").expect("https");
        let (host, port) = authority.split_once(':').expect("port");
        let tcp = TcpStream::connect((host, port.parse::<u16>().expect("port"))).await?;
        let name =
            ServerName::try_from(host.to_owned()).map_err(|e| io::Error::other(e.to_string()))?;
        TlsConnector::from(crate::imap_cert::client_config())
            .connect(name, tcp)
            .await
    }

    fn draft_message(subject: &str, body: &str) -> Value {
        json!({
            "Subject": subject,
            "Sender": {"Name": "Owner", "Address": INTERNAL},
            "ToList": [{"Name": "To", "Address": "to@example.test"}],
            "CCList": [], "BCCList": [], "Body": body, "MIMEType": "text/plain", "Unread": 0
        })
    }

    async fn create_draft(client: &TestClient) -> String {
        client
            .ok(
                "POST",
                "/mail/v4/messages",
                json!({"Message": draft_message("synthetic draft", DRAFT_BODY)}),
            )
            .await["Message"]["ID"]
            .as_str()
            .expect("the draft id")
            .to_owned()
    }

    /// Encrypts `plaintext` to a data packet, returning the key packets
    /// (internal scheme), the data packet, the session key and its
    /// algorithm name.
    fn encrypt_body(
        provider: &impl PGPProviderSync,
        public_armored: &str,
        plaintext: &[u8],
    ) -> (Vec<u8>, Vec<u8>, Vec<u8>, String) {
        let public = provider
            .public_key_import(public_armored.as_bytes(), DataEncoding::Armor)
            .expect("the public key");
        let session = provider
            .new_encryptor()
            .with_encryption_key(&public)
            .generate_session_key()
            .expect("the session key");
        let key_packets = provider
            .new_encryptor()
            .with_encryption_key(&public)
            .encrypt_session_key(&session)
            .expect("the key packets");
        let mut data_packet = Vec::new();
        provider
            .new_encryptor()
            .with_session_key(session.clone())
            .encrypt_to_writer(
                plaintext,
                DataEncoding::Bytes,
                SigningMode::Inline,
                WritingMode::All,
                &mut data_packet,
            )
            .expect("the body");
        let algorithm = match session.algorithm() {
            SessionKeyAlgorithm::Aes128 => "aes128",
            SessionKeyAlgorithm::Aes256 => "aes256",
            SessionKeyAlgorithm::Unknown => "unknown",
        }
        .to_owned();
        (
            key_packets,
            data_packet,
            session.export().as_ref().to_vec(),
            algorithm,
        )
    }

    struct Package {
        json: Value,
        data_packet: Vec<u8>,
        key_packets: Vec<u8>,
        session_key: Vec<u8>,
    }

    /// A send package for one recipient: internal (scheme 1) or clear
    /// (scheme 4).
    fn package(
        provider: &impl PGPProviderSync,
        public_armored: &str,
        address: &str,
        scheme: u8,
        plaintext: &[u8],
    ) -> Package {
        let (key_packets, data_packet, session_key, algorithm) =
            encrypt_body(provider, public_armored, plaintext);
        let mut json = json!({
            "Addresses": {address: {"Type": scheme, "Signature": 1}},
            "MIMEType": "text/plain",
            "Type": scheme,
            "Body": base64::engine::general_purpose::STANDARD.encode(&data_packet),
        });
        if scheme == CLEAR_SCHEME {
            json["BodyKey"] = json!({
                "Key": base64::engine::general_purpose::STANDARD.encode(&session_key),
                "Algorithm": algorithm,
            });
        } else {
            json["Addresses"][address]["BodyKeyPacket"] =
                json!(base64::engine::general_purpose::STANDARD.encode(&key_packets));
        }
        Package {
            json,
            data_packet,
            key_packets,
            session_key,
        }
    }

    fn upload_body(draft_id: &str, key_packets: &[u8], data_packet: &[u8]) -> Vec<u8> {
        let boundary = "synthetic-boundary";
        let mut body = Vec::new();
        for (name, value) in [
            ("MessageID", draft_id),
            ("Filename", "notes.txt"),
            ("MIMEType", "text/plain"),
            ("Disposition", "attachment"),
            ("ContentID", ""),
        ] {
            body.extend_from_slice(
                format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"\r\n\r\n{value}\r\n")
                    .as_bytes(),
            );
        }
        for (name, value) in [
            ("KeyPackets", key_packets),
            ("DataPacket", data_packet),
            ("Signature", b"signature".as_slice()),
        ] {
            body.extend_from_slice(
                format!("--{boundary}\r\nContent-Disposition: form-data; name=\"{name}\"; filename=\"blob\"\r\nContent-Type: application/octet-stream\r\n\r\n")
                    .as_bytes(),
            );
            body.extend_from_slice(value);
            body.extend_from_slice(b"\r\n");
        }
        body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
        body
    }

    #[tokio::test]
    async fn recorded_effects_cover_lookup_drafts_uploads_and_sends() {
        let server = serve().await;
        let client = TestClient::new(&server);
        let recipient = server.recipient_key(USER, INTERNAL).expect("the key");
        let provider = ProtonPGP::new_rust_sync();

        let body = client
            .ok("GET", "/core/v4/keys?Email=owner@example.test", json!({}))
            .await;
        assert_eq!(body["RecipientType"], 1);
        assert_eq!(body["Keys"][0]["PublicKey"], json!(recipient.public_key()));
        let body = client
            .ok(
                "GET",
                "/core/v4/keys?Email=stranger@example.test",
                json!({}),
            )
            .await;
        assert_eq!(body["RecipientType"], 2);
        assert_eq!(body["Keys"], json!([]));
        server.add_external_key("external@example.test", recipient.public_key());
        let body = client
            .ok(
                "GET",
                "/core/v4/keys?Email=external@example.test",
                json!({}),
            )
            .await;
        assert_eq!(body["RecipientType"], 2);
        assert_eq!(body["Keys"][0]["PublicKey"], json!(recipient.public_key()));

        let cursor = client.ok("GET", "/core/v4/events/latest", json!({})).await["EventID"]
            .as_str()
            .expect("the cursor")
            .to_owned();
        let id = create_draft(&client).await;
        let full = client
            .ok("GET", &format!("/mail/v4/messages/{id}"), json!({}))
            .await;
        assert_eq!(full["Message"]["Subject"], json!("synthetic draft"));
        assert_eq!(full["Message"]["MIMEType"], json!("text/plain"));
        assert_eq!(full["Message"]["Body"], json!(DRAFT_BODY));
        assert_eq!(full["Message"]["LabelIDs"], json!(["8", "5"]));
        assert_eq!(full["Message"]["Attachments"], json!([]));

        let body = client
            .ok(
                "PUT",
                &format!("/mail/v4/messages/{id}"),
                json!({"Message": draft_message("updated subject", "updated body")}),
            )
            .await;
        assert_eq!(body["Message"]["Subject"], json!("updated subject"));
        let full = client
            .ok("GET", &format!("/mail/v4/messages/{id}"), json!({}))
            .await;
        assert_eq!(full["Message"]["Body"], json!("updated body"));

        let plaintext = b"Sweetbriar synthetic upload bytes.";
        let (key_packets, data_packet) = {
            let (key_packets, data_packet, _, _) =
                encrypt_body(&provider, recipient.public_key(), plaintext);
            (key_packets, data_packet)
        };
        let (status, bytes) = client
            .request(
                "POST",
                "/mail/v4/attachments",
                upload_body(&id, &key_packets, &data_packet),
                CONTENT_TYPE,
            )
            .await
            .expect("the upload");
        assert_eq!(status, StatusCode::OK);
        let json: Value = serde_json::from_slice(&bytes).unwrap();
        assert_eq!(json["Code"], 1000);
        assert_eq!(json["Attachment"]["Name"], json!("notes.txt"));
        assert_eq!(json["Attachment"]["Size"], json!(plaintext.len()));
        let full = client
            .ok("GET", &format!("/mail/v4/messages/{id}"), json!({}))
            .await;
        assert_eq!(
            full["Message"]["Attachments"][0]["Size"],
            json!(plaintext.len())
        );

        let parts = package(
            &provider,
            recipient.public_key(),
            INTERNAL,
            INTERNAL_SCHEME,
            b"hello",
        );
        let body = client
            .ok(
                "POST",
                &format!("/mail/v4/messages/{id}"),
                json!({"Packages": [parts.json]}),
            )
            .await;
        assert_eq!(body["Sent"]["ID"], json!(id));
        assert_eq!(body["Sent"]["LabelIDs"], json!(["5", "7"]));
        assert_eq!(server.sends(), 1);
        let recorded = server.recorded_sends();
        assert_eq!(recorded[0].draft_id, id);
        let recorded = &recorded[0].packages[0];
        assert_eq!(
            (recorded.mime_type.as_str(), recorded.scheme),
            ("text/plain", INTERNAL_SCHEME)
        );
        assert_eq!(recorded.body, parts.data_packet);
        assert!(recorded.body_key.is_none());
        assert_eq!(recorded.recipients[0].address, INTERNAL);
        assert_eq!(recorded.recipients[0].scheme, INTERNAL_SCHEME);
        assert_eq!(recorded.recipients[0].body_key_packet, parts.key_packets);

        let events = client
            .ok("GET", &format!("/core/v4/events/{cursor}"), json!({}))
            .await;
        let update = events["Messages"]
            .as_array()
            .expect("messages")
            .iter()
            .rfind(|event| event["ID"] == json!(id) && event["Action"] == json!(2))
            .expect("the update event");
        assert_eq!(update["Message"]["LabelIDs"], json!(["5", "7"]));
    }

    #[tokio::test]
    async fn package_bodies_decrypt_with_the_recipient_key_and_clear_body_key() {
        let server = serve().await;
        let client = TestClient::new(&server);
        let id = create_draft(&client).await;
        let provider = ProtonPGP::new_rust_sync();
        let recipient = server.recipient_key(USER, INTERNAL).expect("the key");
        let internal = package(
            &provider,
            recipient.public_key(),
            INTERNAL,
            INTERNAL_SCHEME,
            b"internal body",
        );
        let clear = package(
            &provider,
            recipient.public_key(),
            "to@example.test",
            CLEAR_SCHEME,
            b"clear body",
        );
        client
            .ok(
                "POST",
                &format!("/mail/v4/messages/{id}"),
                json!({"Packages": [internal.json, clear.json]}),
            )
            .await;

        let recorded = server.recorded_sends();
        let internal_record = &recorded[0].packages[0];
        let entry = &internal_record.recipients[0];
        assert_eq!(
            recipient
                .decrypt_body(&entry.body_key_packet, &internal_record.body)
                .expect("the internal body"),
            b"internal body"
        );
        assert_eq!(
            recipient
                .decrypt_session_key(&entry.body_key_packet)
                .expect("the session key"),
            internal.session_key
        );

        let clear_record = &recorded[0].packages[1];
        let body_key = clear_record.body_key.as_ref().expect("the body key");
        assert_eq!(body_key.key, clear.session_key);
        assert!(clear_record.recipients[0].body_key_packet.is_empty());
        assert_eq!(
            recipient
                .decrypt_clear_body(&body_key.algorithm, &body_key.key, &clear_record.body)
                .expect("the clear body"),
            b"clear body"
        );
    }

    #[tokio::test]
    async fn refusals_cover_draft_upload_and_send() {
        let server = serve().await;
        let client = TestClient::new(&server);
        let provider = ProtonPGP::new_rust_sync();
        let recipient = server.recipient_key(USER, INTERNAL).expect("the key");

        let mut no_address = draft_message("x", "");
        no_address["Sender"] = json!({"Name": "No address"});
        let mut bad_mime = draft_message("x", "");
        bad_mime["MIMEType"] = json!("application/json");
        for message in [
            json!({"Subject": "x", "MIMEType": "text/plain", "Body": ""}),
            no_address,
            bad_mime,
        ] {
            let (status, body) = client
                .json("POST", "/mail/v4/messages", json!({"Message": message}))
                .await
                .expect("the request");
            assert_eq!(status, StatusCode::UNPROCESSABLE_ENTITY);
            assert_eq!(body["Code"], 2001);
        }

        let (key_packets, data_packet) = {
            let (key_packets, data_packet, _, _) =
                encrypt_body(&provider, recipient.public_key(), b"bytes");
            (key_packets, data_packet)
        };
        let (status, bytes) = client
            .request(
                "POST",
                "/mail/v4/attachments",
                upload_body("missing", &key_packets, &data_packet),
                CONTENT_TYPE,
            )
            .await
            .expect("the request");
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap()["Code"],
            2001
        );

        let id = create_draft(&client).await;
        client
            .ok(
                "POST",
                &format!("/mail/v4/messages/{id}"),
                json!({"Packages": []}),
            )
            .await;
        let (status, bytes) = client
            .request(
                "POST",
                "/mail/v4/attachments",
                upload_body(&id, &key_packets, &data_packet),
                CONTENT_TYPE,
            )
            .await
            .expect("the request");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(
            serde_json::from_slice::<Value>(&bytes).unwrap()["Code"],
            2001
        );

        let (status, body) = client
            .json("POST", "/mail/v4/messages/missing", json!({"Packages": []}))
            .await
            .expect("the request");
        assert_eq!(status, StatusCode::NOT_FOUND);
        assert_eq!(body["Code"], 2001);
        let (status, body) = client
            .json(
                "POST",
                &format!("/mail/v4/messages/{id}"),
                json!({"Packages": []}),
            )
            .await
            .expect("the request");
        assert_eq!(status, StatusCode::BAD_REQUEST);
        assert_eq!(body["Code"], 2001);
        assert_eq!(server.sends(), 1);
    }
}
