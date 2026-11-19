//! The send-transport HTTP calls and their failure classes (spec/05
//! "Proton submission" steps 1-4; plan M4.3d2). The crypto and the
//! packages are `super::send` (M4.3d1); this module carries them over
//! the existing `Client` session and types the failures the M4.3e
//! worker maps (spec/05 step 5): a refusal keeps Proton's code and
//! text, a connection never established is distinct from one lost
//! after the request began, and pre-request failures keep today's
//! form.

use std::fmt;
use std::sync::atomic::{AtomicU64, Ordering};
use std::time::{SystemTime, UNIX_EPOCH};

use bytes::Bytes;
use hyper::header::{HeaderName, HeaderValue};
use hyper::{Method, StatusCode};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use crate::auth::Session;
use crate::client::{
    Client, Priority, Replies, TransportFailure, code_of, error_message, read_response,
};
use crate::error::ProtonError;
use crate::mail::{ATTACHMENT_PATH, MESSAGES_PATH, validate_path_id};
use crate::send::{EncryptedAttachment, RecipientKeys, SendPackages};

/// `GET /core/v4/keys` (research `keys-crypto.md` §5).
pub const KEYS_PATH: &str = "/core/v4/keys";

/// Which persistence a Proton refusal has (spec/05 step 5).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefusalKind {
    /// 2064 (unknown product) or 5003 (unsupported version): Proton
    /// refused Sweetbriar's identity; the account needs attention
    /// (research `auth-session.md` §2).
    Identity,
    /// 10013: Proton refused the session; the account needs attention
    /// (research `auth-session.md` §6).
    Session,
    /// Any other refusal.
    Other,
}

/// Proton's refusal of a send call: its numeric `Code` and `Error`
/// text, with the identity and session refusals recognisable
/// (spec/05 step 5). The text is Proton's own message, never a body
/// or key (spec/10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SendRefusal {
    /// Proton's numeric `Code`.
    pub code: i64,
    /// Proton's `Error` text.
    pub message: String,
    /// Which persistence the refusal is.
    pub kind: RefusalKind,
}

impl SendRefusal {
    fn from_reply(code: i64, message: String) -> Self {
        let kind = match code {
            2064 | 5003 => RefusalKind::Identity,
            10013 => RefusalKind::Session,
            _ => RefusalKind::Other,
        };
        Self {
            code,
            message,
            kind,
        }
    }
}

/// A send-transport failure, typed so the M4.3e worker maps it without
/// string matching (spec/05 step 5). `Debug` and `Display` carry no
/// body text, key material or credentials (spec/10).
#[derive(Debug)]
pub enum SendTransportError {
    /// Proton answered with a numeric code other than success; its
    /// code and text are kept for the Out Box (`final_reply`). On the
    /// send call this alone proves nothing: only a qualified
    /// pre-submission class is `failed`, and the worker treats every
    /// other reply as an unknown outcome (spec/05 step 5).
    Refused(SendRefusal),
    /// The connection was never established (refused, DNS, connect
    /// timeout, failed TLS handshake): nothing was sent, so the
    /// attempt stays queued and retries (spec/05 step 5).
    NeverEstablished {
        /// A static description of the connect failure.
        message: String,
    },
    /// The request may have reached Proton and its outcome is unknown
    /// (a connection lost, a deadline, or an unusable reply after the
    /// request began).
    OutcomeUnknown {
        /// A static description.
        message: String,
    },
    /// A caller-supplied value was unusable (an address or id that
    /// cannot be a query value or path segment).
    InvalidInput { message: String },
    /// A pre-request failure with today's typed form: 429/503 backoff
    /// exhausted, 401 refresh needed, the client closed.
    PreRequest(ProtonError),
}

impl SendTransportError {
    /// Maps the client's classified failure. A 5xx the client gave up
    /// on and a lost connection both mean the request reached Proton,
    /// so they are unknown outcomes; a connect failure and a rate limit
    /// are not. A reply returned to `classify_reply` is typed there.
    fn from_transport(failure: TransportFailure) -> Self {
        match failure {
            TransportFailure::NeverEstablished { message } => Self::NeverEstablished { message },
            TransportFailure::OutcomeUnknown { message } => Self::OutcomeUnknown { message },
            TransportFailure::Timeout => Self::OutcomeUnknown {
                message: "the request exceeded its deadline".to_owned(),
            },
            TransportFailure::RateLimited { message } => {
                Self::PreRequest(ProtonError::Unavailable { message })
            }
            TransportFailure::ServerRefused { status, message } => Self::OutcomeUnknown {
                message: format!("Proton answered {status}: {message}"),
            },
            TransportFailure::Malformed { message } => Self::InvalidInput { message },
            TransportFailure::Closed { message } => {
                Self::PreRequest(ProtonError::Unavailable { message })
            }
        }
    }
}

impl fmt::Display for SendTransportError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Refused(refusal) => write!(f, "Proton refused the request: {}", refusal.message),
            Self::NeverEstablished { message } => {
                write!(f, "the connection was never established: {message}")
            }
            Self::OutcomeUnknown { message } => {
                write!(f, "the request's outcome is unknown: {message}")
            }
            Self::InvalidInput { message } => write!(f, "{message}"),
            Self::PreRequest(error) => write!(f, "{error}"),
        }
    }
}

impl std::error::Error for SendTransportError {}

/// One address of a draft's sender or recipient lists
/// (`go-proton-api/message_types.go`'s `mail.Address`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct DraftAddress {
    #[serde(default, skip_serializing_if = "String::is_empty")]
    pub name: String,
    pub address: String,
}

/// A draft's fields (research `mail-sync.md` §7 `DraftTemplate`).
/// `Debug` redacts the encrypted body (spec/10).
#[derive(Clone, Serialize)]
#[serde(rename_all = "PascalCase")]
pub struct DraftTemplate {
    pub sender: DraftAddress,
    #[serde(rename = "ToList", default, skip_serializing_if = "Vec::is_empty")]
    pub to: Vec<DraftAddress>,
    #[serde(rename = "CCList", default, skip_serializing_if = "Vec::is_empty")]
    pub cc: Vec<DraftAddress>,
    #[serde(rename = "BCCList", default, skip_serializing_if = "Vec::is_empty")]
    pub bcc: Vec<DraftAddress>,
    pub subject: String,
    /// `text/plain` or `text/html` (research `mail-sync.md` §7).
    #[serde(rename = "MIMEType")]
    pub mime_type: String,
    /// The armored body `encrypt_draft_body` produced.
    pub body: String,
}

impl fmt::Debug for DraftTemplate {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("DraftTemplate")
            .field("sender", &self.sender)
            .field("to", &self.to)
            .field("cc", &self.cc)
            .field("bcc", &self.bcc)
            .field("subject", &self.subject)
            .field("mime_type", &self.mime_type)
            .field("body", &"[redacted]")
            .finish()
    }
}

/// Proton's draft action for a reply or forward (research
/// `mail-sync.md` §7 `CreateDraftReq`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum DraftAction {
    Reply,
    ReplyAll,
    Forward,
    AutoResponse,
    ReadReceipt,
}

impl DraftAction {
    /// Proton's numeric action.
    pub fn code(self) -> u8 {
        match self {
            Self::Reply => 0,
            Self::ReplyAll => 1,
            Self::Forward => 2,
            Self::AutoResponse => 3,
            Self::ReadReceipt => 4,
        }
    }
}

/// A draft creation (research `mail-sync.md` §7 `CreateDraftReq`):
/// the template and, for a reply or forward of a message in the same
/// Proton account, its parent and action (spec/05 step 2).
#[derive(Debug, Clone)]
pub struct DraftCreate {
    pub template: DraftTemplate,
    /// The parent message's id, for a reply or forward.
    pub parent_id: Option<String>,
    /// Proton's action, for a reply or forward.
    pub action: Option<DraftAction>,
}

/// The draft Proton created or updated (spec/05 step 2).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Draft {
    /// Proton's draft id; it becomes the sent message's id
    /// (spec/05 step 5).
    pub id: String,
    /// The draft's `AddressID`: the sender's address Proton resolved.
    pub address_id: String,
}

#[derive(Serialize)]
struct CreateDraftRequest<'a> {
    #[serde(rename = "Message")]
    message: &'a DraftTemplate,
    #[serde(rename = "ParentID", skip_serializing_if = "Option::is_none")]
    parent_id: Option<&'a str>,
    #[serde(rename = "Action", skip_serializing_if = "Option::is_none")]
    action: Option<u8>,
}

#[derive(Serialize)]
struct UpdateDraftRequest<'a> {
    #[serde(rename = "Message")]
    message: &'a DraftTemplate,
}

#[derive(Deserialize)]
struct DraftResponse {
    #[serde(rename = "Message")]
    message: DraftMessageResponse,
}

#[derive(Deserialize)]
struct DraftMessageResponse {
    #[serde(rename = "ID")]
    id: String,
    #[serde(rename = "AddressID", default)]
    address_id: String,
}

/// One attachment to upload (spec/05 step 3): the packets
/// `encrypt_attachment` produced, with the draft metadata Proton
/// reports back.
#[derive(Debug, Clone)]
pub struct AttachmentUpload {
    /// The file name.
    pub filename: String,
    /// Its MIME type.
    pub mime_type: String,
    /// The encrypted packets.
    pub encrypted: EncryptedAttachment,
    /// Whether it is inline (`multipart/related`).
    pub inline: bool,
    /// Its `Content-ID` when inline (research `mail-sync.md` §4).
    pub content_id: Option<String>,
}

/// The uploaded attachment's id and size (spec/02's
/// `proton_uploads_json`): the draft reports the size as the
/// decrypted data's length, the measure 05's retry matching uses.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct UploadedAttachment {
    #[serde(rename = "ID")]
    pub id: String,
    pub size: i64,
}

#[derive(Deserialize)]
struct UploadResponse {
    #[serde(rename = "Attachment")]
    attachment: UploadedAttachment,
}

/// The accepted reply of a send (spec/05 step 5): the message
/// Proton made of the draft, whose id is the draft id.
#[derive(Debug, Clone, PartialEq, Eq, Deserialize)]
pub struct SentMessage {
    /// The sent message's id (the draft id).
    #[serde(rename = "ID")]
    pub id: String,
}

#[derive(Deserialize)]
struct SendResponse {
    #[serde(rename = "Sent")]
    sent: SentMessage,
}

impl Client {
    /// One authenticated send-transport call (spec/05 steps 1-4):
    /// pacing, the 30 s deadline and backoff, then the failure classes
    /// the send worker maps (spec/05 step 5).
    async fn send_request(
        &self,
        method: Method,
        path: &str,
        body: Option<Bytes>,
        content_type: Option<String>,
        session: &Session,
        replies: Replies,
    ) -> Result<Bytes, SendTransportError> {
        let extra = match content_type {
            Some(value) => {
                let value = HeaderValue::from_str(&value).map_err(|_| {
                    SendTransportError::InvalidInput {
                        message: "the content type is not a header value".to_owned(),
                    }
                })?;
                vec![(HeaderName::from_static("content-type"), value)]
            }
            None => Vec::new(),
        };
        let answer = self
            .send_classified(
                method,
                path,
                body,
                Priority::Background,
                Some((session.uid(), session.expose_access_token())),
                &extra,
                replies,
            )
            .await
            .map_err(SendTransportError::from_transport)?;
        let (status, bytes) = read_response(answer).await.map_err(|error| {
            // The request reached Proton and the reply began; a body
            // that doesn't read is an unknown outcome, never a
            // partial observation (spec/04 "Fault handling").
            SendTransportError::OutcomeUnknown {
                message: error.to_string(),
            }
        })?;
        classify_reply(status, bytes)
    }

    /// `GET /core/v4/keys?Email=` (spec/05 step 1; research
    /// `keys-crypto.md` §5): the public keys an address lookup
    /// returned, and whether Proton considers it internal.
    pub async fn recipient_keys(
        &self,
        session: &Session,
        email: &str,
    ) -> Result<RecipientKeys, SendTransportError> {
        validate_query_email(email)?;
        let path = format!("{KEYS_PATH}?Email={}", encode_query_value(email));
        let bytes = self
            .send_request(Method::GET, &path, None, None, session, Replies::Retry)
            .await?;
        // `RecipientKeys` reads the `Keys` and `RecipientType` fields
        // and ignores the response's `Code`.
        decode_reply(&path, &bytes)
    }

    /// `POST /mail/v4/messages` (spec/05 step 2; research
    /// `mail-sync.md` §7): creates a draft from the encrypted body.
    pub async fn create_draft(
        &self,
        session: &Session,
        draft: &DraftCreate,
    ) -> Result<Draft, SendTransportError> {
        let body = serde_json::to_vec(&CreateDraftRequest {
            message: &draft.template,
            parent_id: draft.parent_id.as_deref(),
            action: draft.action.map(|action| action.code()),
        })
        .expect("a draft serializes");
        let bytes = self
            .send_request(
                Method::POST,
                MESSAGES_PATH,
                Some(Bytes::from(body)),
                None,
                session,
                Replies::Retry,
            )
            .await?;
        let response: DraftResponse = decode_reply(MESSAGES_PATH, &bytes)?;
        Ok(Draft {
            id: response.message.id,
            address_id: response.message.address_id,
        })
    }

    /// `PUT /mail/v4/messages/{id}` (spec/05 step 2; research
    /// `mail-sync.md` §7): updates a draft a recorded id names, rather
    /// than creating another.
    pub async fn update_draft(
        &self,
        session: &Session,
        draft_id: &str,
        template: &DraftTemplate,
    ) -> Result<Draft, SendTransportError> {
        validate_path_id(draft_id).map_err(|error| SendTransportError::InvalidInput {
            message: error.to_string(),
        })?;
        let body = serde_json::to_vec(&UpdateDraftRequest { message: template })
            .expect("a draft serializes");
        let path = format!("{MESSAGES_PATH}/{draft_id}");
        let bytes = self
            .send_request(
                Method::PUT,
                &path,
                Some(Bytes::from(body)),
                None,
                session,
                Replies::Retry,
            )
            .await?;
        let response: DraftResponse = decode_reply(&path, &bytes)?;
        Ok(Draft {
            id: response.message.id,
            address_id: response.message.address_id,
        })
    }

    /// `POST /mail/v4/attachments` (spec/05 step 3; research
    /// `mail-sync.md` §4): multipart `KeyPackets`, `DataPacket` and
    /// `Signature` with the draft id, filename and MIME type.
    pub async fn upload_attachment(
        &self,
        session: &Session,
        draft_id: &str,
        attachment: &AttachmentUpload,
    ) -> Result<UploadedAttachment, SendTransportError> {
        validate_path_id(draft_id).map_err(|error| SendTransportError::InvalidInput {
            message: error.to_string(),
        })?;
        let boundary = multipart_boundary();
        let body = multipart_upload(&boundary, draft_id, attachment);
        let content_type = format!("multipart/form-data; boundary={boundary}");
        let bytes = self
            .send_request(
                Method::POST,
                ATTACHMENT_PATH,
                Some(Bytes::from(body)),
                Some(content_type),
                session,
                Replies::Retry,
            )
            .await?;
        let response: UploadResponse = decode_reply(ATTACHMENT_PATH, &bytes)?;
        Ok(response.attachment)
    }

    /// `POST /mail/v4/messages/{id}` (spec/05 step 4; research
    /// `mail-sync.md` §7): sends the draft's packages and returns the
    /// accepted reply.
    pub async fn send_packages(
        &self,
        session: &Session,
        draft_id: &str,
        packages: &SendPackages,
    ) -> Result<SentMessage, SendTransportError> {
        validate_path_id(draft_id).map_err(|error| SendTransportError::InvalidInput {
            message: error.to_string(),
        })?;
        let body = serde_json::to_vec(packages).expect("the packages serialize");
        let path = format!("{MESSAGES_PATH}/{draft_id}");
        // Every reply, a 429 or 503 included, is returned for the
        // worker to interpret (spec/05 step 5): waiting one out and
        // sending again would replay the send.
        let bytes = self
            .send_request(
                Method::POST,
                &path,
                Some(Bytes::from(body)),
                None,
                session,
                Replies::Return,
            )
            .await?;
        let response: SendResponse = decode_reply(&path, &bytes)?;
        Ok(response.sent)
    }
}

/// Classifies a send reply: success (HTTP 2xx and Proton's 1000/1001),
/// a refusal carrying Proton's code and text, or an unknown outcome
/// (spec/05 step 5).
fn classify_reply(status: StatusCode, body: Bytes) -> Result<Bytes, SendTransportError> {
    if status == StatusCode::UNAUTHORIZED {
        return Err(SendTransportError::PreRequest(ProtonError::Unauthorized));
    }
    let code = code_of(&body);
    if status.is_success() && matches!(code, Some(1000 | 1001)) {
        return Ok(body);
    }
    if let Some(code) = code {
        return Err(SendTransportError::Refused(SendRefusal::from_reply(
            code,
            error_message(&body),
        )));
    }
    Err(SendTransportError::OutcomeUnknown {
        message: format!("Proton answered {status} without a numeric code"),
    })
}

/// Decodes a send reply. A reply that doesn't decode is an unknown
/// outcome, never a partial observation (spec/04 "Fault handling"):
/// the request reached Proton, so nothing from it is applied.
fn decode_reply<T: DeserializeOwned>(path: &str, bytes: &[u8]) -> Result<T, SendTransportError> {
    serde_json::from_slice(bytes).map_err(|error| SendTransportError::OutcomeUnknown {
        // serde's text can quote a reply's values, so only its
        // category and position are kept (spec/10).
        message: format!(
            "the {path} response didn't decode ({:?} at line {}, column {})",
            error.classify(),
            error.line(),
            error.column()
        ),
    })
}

/// An address in a query must be one printable token: no whitespace
/// or control characters. Its other characters are percent-encoded
/// ([`encode_query_value`]), so `+`, `&` or `%` in a local part reach
/// Proton as written.
fn validate_query_email(email: &str) -> Result<(), SendTransportError> {
    let token = !email.is_empty() && !email.chars().any(|c| c.is_whitespace() || c.is_control());
    token
        .then_some(())
        .ok_or_else(|| SendTransportError::InvalidInput {
            message: "a recipient address must be one query token".to_owned(),
        })
}

/// Percent-encodes a query value (RFC 3986 section 2): every byte
/// outside the unreserved set, so a form decoder reads `+` as `+`, not
/// a space.
fn encode_query_value(value: &str) -> String {
    let mut encoded = String::with_capacity(value.len());
    for byte in value.bytes() {
        if byte.is_ascii_alphanumeric() || matches!(byte, b'-' | b'.' | b'_' | b'~') {
            encoded.push(char::from(byte));
        } else {
            encoded.push_str(&format!("%{byte:02X}"));
        }
    }
    encoded
}

/// A multipart boundary no attachment byte stream is expected to
/// contain: hexadecimal time and counter, so it carries no CRLF.
fn multipart_boundary() -> String {
    static COUNTER: AtomicU64 = AtomicU64::new(0);
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|elapsed| elapsed.as_nanos())
        .unwrap_or(0);
    let counter = COUNTER.fetch_add(1, Ordering::Relaxed);
    format!("----SweetbriarSendBoundary{nanos:x}{counter:x}")
}

/// Builds the multipart body of an attachment upload (research
/// `mail-sync.md` §4): text fields plus the key packets, data packet
/// and detached signature blobs.
fn multipart_upload(boundary: &str, draft_id: &str, attachment: &AttachmentUpload) -> Vec<u8> {
    let mut body = Vec::new();
    push_field(&mut body, boundary, "MessageID", draft_id.as_bytes());
    push_field(
        &mut body,
        boundary,
        "Filename",
        attachment.filename.as_bytes(),
    );
    push_field(
        &mut body,
        boundary,
        "MIMEType",
        attachment.mime_type.as_bytes(),
    );
    let disposition = if attachment.inline {
        "inline"
    } else {
        "attachment"
    };
    push_field(&mut body, boundary, "Disposition", disposition.as_bytes());
    // Proton's clients always send `ContentID`, empty when the
    // attachment isn't inline; its server reads the field
    // unconditionally and panics when it is absent. The fake server
    // defaults a missing field to empty, so only the lane caught this.
    push_field(
        &mut body,
        boundary,
        "ContentID",
        attachment.content_id.as_deref().unwrap_or("").as_bytes(),
    );
    for (name, value) in [
        ("KeyPackets", attachment.encrypted.key_packets.as_slice()),
        ("DataPacket", attachment.encrypted.data_packet.as_slice()),
        ("Signature", attachment.encrypted.signature.as_slice()),
    ] {
        push_blob(&mut body, boundary, name, value);
    }
    body.extend_from_slice(format!("--{boundary}--\r\n").as_bytes());
    body
}

fn push_field(body: &mut Vec<u8>, boundary: &str, name: &str, value: &[u8]) {
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"{name}\"\r\n\r\n").as_bytes(),
    );
    body.extend_from_slice(value);
    body.extend_from_slice(b"\r\n");
}

fn push_blob(body: &mut Vec<u8>, boundary: &str, name: &str, value: &[u8]) {
    body.extend_from_slice(format!("--{boundary}\r\n").as_bytes());
    body.extend_from_slice(
        format!("Content-Disposition: form-data; name=\"{name}\"; filename=\"blob\"\r\n")
            .as_bytes(),
    );
    body.extend_from_slice(b"Content-Type: application/octet-stream\r\n\r\n");
    body.extend_from_slice(value);
    body.extend_from_slice(b"\r\n");
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;
    use std::time::Duration;

    use proton_crypto::ProtonPGP;
    use proton_crypto::crypto::{
        DataEncoding, Encryptor, EncryptorSync, KeyGenerator, KeyGeneratorSync, PGPProviderSync,
        SessionKey, SessionKeyAlgorithm, Signer, SignerSync, SigningMode, WritingMode,
    };
    use proton_crypto_account::keys::{DecryptedAddressKey, KeyFlag, KeyId, UnlockedAddressKey};
    use sb_test_support::{FakeProtonServer, Fault, TestAccount, cert_der};
    use secrecy::SecretString;

    use super::*;
    use crate::Trust;
    use crate::auth::SignInStep;
    use crate::client::Config;
    use crate::send::{
        INTERNAL_SCHEME, RECIPIENT_TYPE_EXTERNAL, RECIPIENT_TYPE_INTERNAL, SendAttachment,
        SendBody, SendPackages, SendRecipient, SessionKey as SendSessionKey, build_packages,
        encrypt_draft_body,
    };

    const USER: &str = "owner@example.org";
    const PASSWORD: &str = "synthetic-owner-password";
    const INTERNAL: &str = "owner@example.test";
    const EXTERNAL: &str = "stranger@example.test";

    async fn server() -> FakeProtonServer {
        FakeProtonServer::serve(vec![TestAccount::new(USER, PASSWORD)])
            .await
            .expect("serve")
    }

    fn test_client(server: &FakeProtonServer) -> Client {
        let config = Config::new(server.base_url(), "15.1", Trust::Roots(vec![cert_der()]))
            .with_modulus_verifier(server.verifier());
        Client::new(config).expect("the test client configures")
    }

    async fn session(client: &Client) -> Session {
        let (_sign_in, step) = client
            .begin_sign_in(USER, SecretString::from(PASSWORD))
            .await
            .expect("sign-in");
        match step {
            SignInStep::Complete(complete) => complete.session,
            other => panic!("unexpected first step: {other:?}"),
        }
    }

    /// A generated sender address key, standing in for the unlocked key
    /// the engine holds for the identity.
    fn sender_key<P: PGPProviderSync>(provider: &P) -> UnlockedAddressKey<P> {
        let private = provider
            .new_key_generator()
            .with_user_id("Owner", "sender@example.test")
            .generate()
            .expect("the sender key generates");
        let public = provider
            .private_key_to_public_key(&private)
            .expect("the sender public key");
        DecryptedAddressKey {
            id: KeyId::from("sender-key".to_owned()),
            flags: KeyFlag::default(),
            primary: true,
            is_v6: false,
            private_key: private,
            public_key: public,
        }
    }

    fn draft_template(body: String) -> DraftTemplate {
        DraftTemplate {
            sender: DraftAddress {
                name: "Owner".to_owned(),
                address: INTERNAL.to_owned(),
            },
            to: vec![DraftAddress {
                name: "To".to_owned(),
                address: INTERNAL.to_owned(),
            }],
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: "synthetic subject".to_owned(),
            mime_type: "text/plain".to_owned(),
            body,
        }
    }

    fn new_draft(body: String) -> DraftCreate {
        DraftCreate {
            template: draft_template(body),
            parent_id: None,
            action: None,
        }
    }

    /// Upload packets the fake's account key decrypts: a fresh session
    /// key encrypted to `recipient_public`, the data packet signed
    /// with `sender`, and a real detached signature. The fake's
    /// upload handler checks the decryption, not the signature.
    fn upload_packets<P: PGPProviderSync>(
        provider: &P,
        sender: &UnlockedAddressKey<P>,
        recipient_public: &str,
        data: &[u8],
    ) -> EncryptedAttachment {
        let public = provider
            .public_key_import(recipient_public.as_bytes(), DataEncoding::Armor)
            .expect("the recipient public key imports");
        let session = provider
            .session_key_generate(SessionKeyAlgorithm::Aes256)
            .expect("the session key generates");
        let key_packets = provider
            .new_encryptor()
            .with_encryption_key(&public)
            .encrypt_session_key(&session)
            .expect("the key packets encrypt");
        let mut data_packet = Vec::new();
        provider
            .new_encryptor()
            .with_session_key(session.clone())
            .with_signing_key(&sender.private_key)
            .encrypt_to_writer(
                data,
                DataEncoding::Bytes,
                SigningMode::Inline,
                WritingMode::All,
                &mut data_packet,
            )
            .expect("the data packet encrypts");
        let signature = provider
            .new_signer()
            .with_signing_key(&sender.private_key)
            .sign_detached(data, DataEncoding::Bytes)
            .expect("the signature");
        EncryptedAttachment {
            key_packets,
            data_packet,
            signature,
            session_key: SendSessionKey::new(
                session.export().as_ref().to_vec(),
                session.algorithm(),
            ),
        }
    }

    fn encrypted_draft_body<P: PGPProviderSync>(
        provider: &P,
        sender: &UnlockedAddressKey<P>,
    ) -> String {
        let body = encrypt_draft_body(provider, sender, b"draft body").expect("the body");
        String::from_utf8(body).expect("the armored body is ASCII")
    }

    async fn create_draft<P: PGPProviderSync>(
        client: &Client,
        session: &Session,
        provider: &P,
        sender: &UnlockedAddressKey<P>,
    ) -> Draft {
        client
            .create_draft(session, &new_draft(encrypted_draft_body(provider, sender)))
            .await
            .expect("the draft")
    }

    /// One clear-scheme package for the external address, as a plain
    /// draft needs.
    fn clear_packages<P: PGPProviderSync>(
        provider: &P,
        sender: &UnlockedAddressKey<P>,
    ) -> SendPackages {
        build_packages(
            provider,
            sender,
            &[SendRecipient {
                address: EXTERNAL.to_owned(),
                keys: crate::RecipientKeys {
                    keys: Vec::new(),
                    recipient_type: crate::RECIPIENT_TYPE_EXTERNAL,
                },
            }],
            &[SendBody {
                mime_type: "text/plain",
                data: b"hello",
            }],
            &[],
        )
        .expect("the packages")
    }

    #[tokio::test]
    async fn an_internal_recipient_round_trips_through_the_send_calls() {
        let server = server().await;
        let client = test_client(&server);
        let session = session(&client).await;
        let provider = ProtonPGP::new_rust_sync();
        let sender = sender_key(&provider);
        let recipient = server
            .recipient_key(USER, INTERNAL)
            .expect("the recipient key");

        // Step 1: the recipient key lookup.
        let keys = client
            .recipient_keys(&session, INTERNAL)
            .await
            .expect("the keys");
        assert_eq!(keys.recipient_type, RECIPIENT_TYPE_INTERNAL);
        assert_eq!(keys.encryption_key(), Some(recipient.public_key()));

        // Step 2: create the draft with an encrypted body.
        let draft = create_draft(&client, &session, &provider, &sender).await;
        assert!(!draft.id.is_empty());

        // Step 3: upload an attachment.
        let encrypted = upload_packets(
            &provider,
            &sender,
            recipient.public_key(),
            b"attachment bytes",
        );
        let session_key = encrypted.session_key.clone();
        let attachment = AttachmentUpload {
            filename: "notes.txt".to_owned(),
            mime_type: "text/plain".to_owned(),
            encrypted,
            inline: false,
            content_id: None,
        };
        let uploaded = client
            .upload_attachment(&session, &draft.id, &attachment)
            .await
            .expect("the upload");
        assert!(!uploaded.id.is_empty());
        assert_eq!(uploaded.size, b"attachment bytes".len() as i64);

        // Step 4: build the packages and send.
        let packages = build_packages(
            &provider,
            &sender,
            &[SendRecipient {
                address: INTERNAL.to_owned(),
                keys,
            }],
            &[SendBody {
                mime_type: "text/plain",
                data: b"hello",
            }],
            &[SendAttachment {
                id: uploaded.id.clone(),
                session_key: session_key.clone(),
            }],
        )
        .expect("the packages");
        let sent = client
            .send_packages(&session, &draft.id, &packages)
            .await
            .expect("the send");
        assert_eq!(sent.id, draft.id);

        // The fake recorded the send once; the recipient's key decrypts
        // the body and the attachment key packet.
        assert_eq!(server.sends(), 1);
        let recorded = server.recorded_sends();
        assert_eq!(recorded.len(), 1);
        assert_eq!(recorded[0].draft_id, draft.id);
        assert_eq!(recorded[0].packages.len(), 1);
        let package = &recorded[0].packages[0];
        assert_eq!(package.mime_type, "text/plain");
        assert_eq!(package.scheme, INTERNAL_SCHEME);
        let entry = package
            .recipients
            .iter()
            .find(|entry| entry.address == INTERNAL)
            .expect("the recipient");
        assert_eq!(
            recipient
                .decrypt_body(&entry.body_key_packet, &package.body)
                .expect("the body decrypts"),
            b"hello"
        );
        let attachment_packet = entry
            .attachment_key_packets
            .get(&uploaded.id)
            .expect("the attachment key packet");
        assert_eq!(
            recipient
                .decrypt_session_key(attachment_packet)
                .expect("the attachment session key decrypts"),
            session_key.key()
        );
    }

    #[tokio::test]
    async fn a_recorded_draft_is_updated_rather_than_recreated() {
        let server = server().await;
        let client = test_client(&server);
        let session = session(&client).await;
        let provider = ProtonPGP::new_rust_sync();
        let sender = sender_key(&provider);
        let draft = create_draft(&client, &session, &provider, &sender).await;

        let mut template = draft_template(encrypted_draft_body(&provider, &sender));
        template.subject = "updated subject".to_owned();
        let updated = client
            .update_draft(&session, &draft.id, &template)
            .await
            .expect("the update");
        assert_eq!(updated.id, draft.id);
        let full = client
            .message(&session, &draft.id)
            .await
            .expect("the draft");
        assert_eq!(full.metadata.subject, "updated subject");
        let creates = server
            .requests()
            .into_iter()
            .filter(|request| request.method == "POST" && request.path == MESSAGES_PATH)
            .count();
        assert_eq!(creates, 1, "the update used PUT, not a second POST");
    }

    #[tokio::test]
    async fn an_address_with_a_plus_is_looked_up_as_written() {
        let server = server().await;
        let client = test_client(&server);
        let session = session(&client).await;
        let recipient = server
            .recipient_key(USER, INTERNAL)
            .expect("a fake key to register");
        // A form decoder reads an unescaped `+` as a space, which would
        // look up another address.
        server.add_external_key("ext+tag@example.test", recipient.public_key());
        let keys = client
            .recipient_keys(&session, "ext+tag@example.test")
            .await
            .expect("the keys");
        assert_eq!(keys.recipient_type, RECIPIENT_TYPE_EXTERNAL);
        assert_eq!(keys.keys.len(), 1, "the registered key came back");
    }

    #[tokio::test]
    async fn an_external_recipient_without_keys_gets_the_clear_scheme() {
        let server = server().await;
        let client = test_client(&server);
        let session = session(&client).await;
        let provider = ProtonPGP::new_rust_sync();
        let sender = sender_key(&provider);
        let recipient = server
            .recipient_key(USER, INTERNAL)
            .expect("a fake key to decrypt clear");

        let keys = client
            .recipient_keys(&session, EXTERNAL)
            .await
            .expect("the keys");
        assert_eq!(keys.recipient_type, RECIPIENT_TYPE_EXTERNAL);
        assert!(keys.keys.is_empty());

        let draft = create_draft(&client, &session, &provider, &sender).await;
        let packages = clear_packages(&provider, &sender);
        client
            .send_packages(&session, &draft.id, &packages)
            .await
            .expect("the send");

        let recorded = server.recorded_sends();
        assert_eq!(recorded.len(), 1);
        let package = &recorded[0].packages[0];
        assert_eq!(package.scheme, crate::send::CLEAR_SCHEME);
        let body_key = package.body_key.as_ref().expect("the clear body key");
        assert_eq!(
            recipient
                .decrypt_clear_body(&body_key.algorithm, &body_key.key, &package.body)
                .expect("the clear body decrypts"),
            b"hello"
        );
        assert!(package.recipients[0].body_key_packet.is_empty());
    }

    #[tokio::test]
    async fn a_dropped_send_reply_is_outcome_unknown_and_not_replayed() {
        let server = server().await;
        let client = test_client(&server);
        let session = session(&client).await;
        let provider = ProtonPGP::new_rust_sync();
        let sender = sender_key(&provider);
        let draft = create_draft(&client, &session, &provider, &sender).await;
        let packages = clear_packages(&provider, &sender);

        server.set_fault(Some(Fault::Once(Box::new(Fault::DropAfterApply))));
        let error = client
            .send_packages(&session, &draft.id, &packages)
            .await
            .expect_err("the reply is dropped after the send is applied");
        assert!(
            matches!(error, SendTransportError::OutcomeUnknown { .. }),
            "{error:?}"
        );

        // The fake applied the send once and the client did not replay
        // the POST (spec/05 step 5: a dropped send reply is unknown).
        assert_eq!(server.sends(), 1);
        let send_path = format!("{MESSAGES_PATH}/{}", draft.id);
        let send_requests = server
            .requests()
            .into_iter()
            .filter(|request| request.method == "POST" && request.path == send_path)
            .count();
        assert_eq!(send_requests, 1, "the send POST went out once");
    }

    #[tokio::test(start_paused = true)]
    async fn a_rate_limited_send_is_not_waited_out_and_replayed() {
        let server = server().await;
        let client = test_client(&server);
        let session = session(&client).await;
        let provider = ProtonPGP::new_rust_sync();
        let sender = sender_key(&provider);
        let draft = create_draft(&client, &session, &provider, &sender).await;
        let packages = clear_packages(&provider, &sender);

        // Once `send_requested_at` is committed, any reply is the
        // worker's to interpret (spec/05 step 5): waiting out a 429 and
        // sending again would replay the POST.
        server.set_fault(Some(Fault::Once(Box::new(Fault::RateLimit {
            status: 429,
            retry_after: Some(1),
        }))));
        let error = client
            .send_packages(&session, &draft.id, &packages)
            .await
            .expect_err("the 429 is returned, not waited out");
        assert!(
            matches!(
                &error,
                SendTransportError::Refused(SendRefusal { code: 2028, .. })
            ),
            "{error:?}"
        );
        let send_path = format!("{MESSAGES_PATH}/{}", draft.id);
        let send_requests = server
            .requests()
            .into_iter()
            .filter(|request| request.method == "POST" && request.path == send_path)
            .count();
        assert_eq!(send_requests, 1, "the send POST went out once");
        assert_eq!(server.sends(), 0);
    }

    #[tokio::test]
    async fn a_dropped_draft_create_is_not_replayed() {
        let server = server().await;
        let client = test_client(&server);
        let session = session(&client).await;
        let provider = ProtonPGP::new_rust_sync();
        let sender = sender_key(&provider);

        server.set_fault(Some(Fault::Drop));
        let error = client
            .create_draft(
                &session,
                &new_draft(encrypted_draft_body(&provider, &sender)),
            )
            .await
            .expect_err("the reply is dropped");
        assert!(
            matches!(error, SendTransportError::OutcomeUnknown { .. }),
            "{error:?}"
        );
        let creates = server
            .requests()
            .into_iter()
            .filter(|request| request.method == "POST" && request.path == MESSAGES_PATH)
            .count();
        assert_eq!(creates, 1, "the draft POST went out once");
    }

    #[tokio::test(start_paused = true)]
    async fn a_refused_connection_is_never_established_and_records_nothing() {
        let server = Arc::new(server().await);
        let session = session(&test_client(&server)).await;
        // A fresh client, so no pooled sign-in connection bypasses the
        // connection-level fault.
        let client = test_client(&server);

        // `Fault::RefuseConnection` is one-shot, and the client retries
        // a connect failure (nothing was sent), so keep it armed until
        // the retries are exhausted.
        let refusals = tokio::spawn({
            let server = Arc::clone(&server);
            async move {
                loop {
                    server.set_fault(Some(Fault::RefuseConnection));
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        });
        let error = client
            .recipient_keys(&session, INTERNAL)
            .await
            .expect_err("every connection is refused");
        refusals.abort();
        let _ = refusals.await;

        assert!(
            matches!(error, SendTransportError::NeverEstablished { .. }),
            "{error:?}"
        );
        assert!(
            !server
                .requests()
                .iter()
                .any(|request| request.path == KEYS_PATH),
            "no key lookup reached the server"
        );
    }

    #[tokio::test(start_paused = true)]
    async fn a_stalled_handshake_is_a_connect_timeout_never_established() {
        let server = Arc::new(server().await);
        let session = session(&test_client(&server)).await;
        // A fresh client, so no pooled sign-in connection bypasses the
        // connection-level fault.
        let client = test_client(&server);

        let stalls = tokio::spawn({
            let server = Arc::clone(&server);
            async move {
                loop {
                    server.set_fault(Some(Fault::StallHandshake(Duration::from_secs(60))));
                    tokio::time::sleep(Duration::from_millis(10)).await;
                }
            }
        });
        let error = client
            .recipient_keys(&session, INTERNAL)
            .await
            .expect_err("the handshake never completes");
        stalls.abort();
        let _ = stalls.await;

        assert!(
            matches!(error, SendTransportError::NeverEstablished { .. }),
            "{error:?}"
        );
        assert!(
            !server
                .requests()
                .iter()
                .any(|request| request.path == KEYS_PATH),
            "no key lookup reached the server"
        );
    }

    #[tokio::test]
    async fn refusals_keep_proton_code_and_text() {
        let server = server().await;
        let client = test_client(&server);
        let session = session(&client).await;

        for (status, code, expected) in [
            (400u16, 2064i64, RefusalKind::Identity),
            (400, 5003, RefusalKind::Identity),
            (422, 10013, RefusalKind::Session),
            (422, 2001, RefusalKind::Other),
        ] {
            server.set_fault(Some(Fault::Code { status, code }));
            let error = client
                .recipient_keys(&session, INTERNAL)
                .await
                .expect_err("the fault refuses");
            let SendTransportError::Refused(refusal) = error else {
                panic!("{code} is a refusal: {error:?}");
            };
            assert_eq!(refusal.code, code);
            assert_eq!(refusal.kind, expected);
            assert!(!refusal.message.is_empty());
        }

        // The send route refuses the same way and applies nothing.
        let provider = ProtonPGP::new_rust_sync();
        let sender = sender_key(&provider);
        let packages = clear_packages(&provider, &sender);
        server.set_fault(Some(Fault::OnceAt {
            path: format!("{MESSAGES_PATH}/draft-refused"),
            fault: Box::new(Fault::Code {
                status: 422,
                code: 2001,
            }),
        }));
        let error = client
            .send_packages(&session, "draft-refused", &packages)
            .await
            .expect_err("the send is refused");
        let SendTransportError::Refused(refusal) = error else {
            panic!("the send refusal is typed: {error:?}");
        };
        assert_eq!(refusal.code, 2001);
        assert_eq!(refusal.kind, RefusalKind::Other);
        assert_eq!(server.sends(), 0);
    }

    #[tokio::test]
    async fn a_rate_limit_keeps_its_pre_request_form() {
        let server = server().await;
        let client = test_client(&server);
        let session = session(&client).await;
        server.set_fault(Some(Fault::RateLimit {
            status: 429,
            retry_after: None,
        }));
        let error = client
            .recipient_keys(&session, INTERNAL)
            .await
            .expect_err("the backoff is exhausted");
        assert!(
            matches!(
                error,
                SendTransportError::PreRequest(ProtonError::Unavailable { .. })
            ),
            "{error:?}"
        );
    }

    #[test]
    fn the_upload_always_carries_a_content_id_field() {
        let attachment = AttachmentUpload {
            filename: "notes.txt".to_owned(),
            mime_type: "text/plain".to_owned(),
            encrypted: EncryptedAttachment {
                key_packets: Vec::new(),
                data_packet: Vec::new(),
                signature: Vec::new(),
                session_key: SendSessionKey::new(Vec::new(), SessionKeyAlgorithm::Aes256),
            },
            inline: false,
            content_id: None,
        };
        let body = multipart_upload("boundary", "draft-1", &attachment);
        let text = String::from_utf8_lossy(&body);
        assert!(
            text.contains("name=\"ContentID\""),
            "a non-inline upload still carries the field Proton reads: {text}"
        );
    }

    #[test]
    fn a_query_email_must_be_one_token() {
        for bad in ["", "a b", "a\tb", "a\u{7f}b"] {
            assert!(
                matches!(
                    validate_query_email(bad),
                    Err(SendTransportError::InvalidInput { .. })
                ),
                "{bad} is not one query token"
            );
        }
        assert!(validate_query_email("a+b@example.test").is_ok());
        assert!(validate_query_email("a%b@example.test").is_ok());
        assert_eq!(
            encode_query_value("a+b&c=d%e@example.test"),
            "a%2Bb%26c%3Dd%25e%40example.test"
        );
    }

    #[test]
    fn debug_redacts_bodies_and_packets() {
        let template = draft_template(
            "-----BEGIN PGP MESSAGE-----\r\nsecret\r\n-----END PGP MESSAGE-----".to_owned(),
        );
        let debug = format!("{template:?}");
        assert!(!debug.contains("secret"), "the body is redacted: {debug}");
        assert!(debug.contains("synthetic subject"));

        let provider = ProtonPGP::new_rust_sync();
        let sender = sender_key(&provider);
        let public = provider
            .public_key_export(&sender.public_key, DataEncoding::Armor)
            .expect("the public key exports");
        let public = String::from_utf8(public.as_ref().to_vec()).expect("the armored key is ASCII");
        let encrypted = upload_packets(&provider, &sender, &public, b"attachment");
        let debug = format!("{encrypted:?}");
        assert!(
            !debug.contains("attachment"),
            "the packets are redacted: {debug}"
        );
        // The ciphertext never holds the ASCII plaintext, so check the
        // byte fields' own renderings and the session key's too.
        for bytes in [
            &encrypted.key_packets,
            &encrypted.data_packet,
            &encrypted.signature,
        ] {
            assert!(
                !debug.contains(&format!("{bytes:?}")),
                "a packet's bytes are printed: {debug}"
            );
        }
        assert_eq!(
            debug.matches("[redacted]").count(),
            4,
            "three packets and the session key are redacted: {debug}"
        );
    }
}
