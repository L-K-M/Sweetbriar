//! The fake Proton API server (spec/11 "The fake Proton API server"):
//! an in-process HTTPS server on 127.0.0.1 that models SRP sign-in,
//! 2FA, human verification, two-password mode, sessions and refresh,
//! with fault knobs and a request log. It uses the same committed test
//! certificate as the IMAP fake (IP SAN 127.0.0.1), so a test client
//! trusts it through [`Trust::Roots`](sb_proton::Trust::Roots).
//!
//! Written from the protocol facts in research/proton-api; no Proton or
//! Bridge code. Every account and token here is synthetic.

use std::collections::{HashMap, HashSet};
use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use base64::Engine as _;
use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use pgp::composed::{
    ArmorOptions, CleartextSignedMessage, Deserializable, EncryptionCaps, KeyType,
    SecretKeyParamsBuilder, SignedPublicKey, SubkeyParamsBuilder,
};
use pgp::types::Password;
use proton_crypto::ProtonPGP;
use proton_crypto::crypto::{
    DataEncoding, Encryptor, EncryptorSync, KeyGeneratorAlgorithm, PGPProviderSync, SigningMode,
    WritingMode,
};
use proton_crypto_account::keys::{
    ArmoredPrivateKey, DecryptedAddressKey, KeyFlag, KeyId, LocalAddressKey, LocalUserKey,
    LockedKey, UserKeys,
};
use proton_crypto_account::salts::{KeySalt, Salt};
pub use proton_srp::{ModulusSignatureVerifier, ModulusVerifyError};
use proton_srp::{
    RawSRPModulus, SRPAuth, ServerClientProof, ServerClientVerifier, ServerInteraction,
};
use sb_types::random::Random;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;

use crate::SeededRandom;
use crate::imap_cert::server_config;

mod send;

pub use send::{
    CLEAR_SCHEME, INTERNAL_SCHEME, RecipientKey, RecordedPackage, RecordedRecipient, RecordedSend,
    RecordedSessionKey, StoredAttachment,
};

/// Encrypts a message body to the account's primary address key.
type EncryptBody = Arc<dyn Fn(&[u8]) -> Vec<u8> + Send + Sync>;
/// Encrypts an attachment, returning its key packets and data packet.
type EncryptAttachment = Arc<dyn Fn(&[u8]) -> (Vec<u8>, Vec<u8>) + Send + Sync>;

/// The synthetic plaintext body the fake encrypts for its message
/// fixture; tests compare a decryption against it.
pub const MESSAGE_PLAINTEXT: &str = "Sweetbriar synthetic body: none of this is real mail.";
/// The synthetic attachment plaintext the fake encrypts.
pub const ATTACHMENT_PLAINTEXT: &[u8] = b"Sweetbriar synthetic attachment bytes.";

/// A valid 2048-bit SRP group modulus (the one proton-srp's own test
/// suite uses), base64-encoded. Synthetic: no live Proton data.
pub const MODULUS_B64: &str = "W2z5HBi8RvsfYzZTS7qBaUxxPhsfHJFZpu3Kd6s1JafNrCCH9rfvPLrfuqocxWPgWDH2R8neK7PkNvjxto9TStuY5z7jAzWRvFWN9cQhAKkdWgy0JY6ywVn22+HFpF4cYesHrqFIKUPDMSSIlWjBVmEJZ/MusD44ZT29xcPrOqeZvwtCffKtGAIjLYPZIEbZKnDM1Dm3q2K/xS5h+xdhjnndhsrkwm9U9oyA2wxzSXFL+pdfj2fOdRwuR5nW0J2NFrq3kJjkRmpO/Genq1UW+TEknIWAb6VzJJJA244K/H8cnSx2+nSNZO3bbo6Ys228ruV9A8m6DhxmS+bihN3ttQ==";

/// Which generated keys the fake damages, for the unlock-failure
/// proofs (M2.12).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum KeyFault {
    /// Every key unlocks.
    #[default]
    None,
    /// The legacy address key is mislocked, so it fails to unlock.
    OneAddressKey,
    /// Every address key is mislocked, so none unlocks.
    AllAddressKeys,
    /// The legacy address key is mislocked and marked inactive, as
    /// Proton marks a key it knows can't be decrypted.
    RetiredAddressKey,
}

/// A synthetic test account. The password and TOTP are test values,
/// never an owner's.
#[derive(Debug, Clone)]
pub struct TestAccount {
    pub username: String,
    pub password: String,
    /// The password that unlocks the keys: the mailbox password in
    /// two-password mode, else the login password.
    pub mailbox_password: Option<String>,
    pub totp: Option<String>,
    /// 1 = one-password, 2 = separate mailbox password.
    pub password_mode: u8,
    pub hv_required: bool,
    /// A registered security key; alone it is FIDO2-only, beside
    /// `totp` it reports `2FA.Enabled` 3.
    pub fido2_only: bool,
    pub key_fault: KeyFault,
}

impl TestAccount {
    pub fn new(username: &str, password: &str) -> Self {
        Self {
            username: username.to_owned(),
            password: password.to_owned(),
            mailbox_password: None,
            totp: None,
            password_mode: 1,
            hv_required: false,
            fido2_only: false,
            key_fault: KeyFault::None,
        }
    }

    pub fn with_totp(mut self, code: &str) -> Self {
        self.totp = Some(code.to_owned());
        self
    }

    pub fn with_mailbox_password(mut self, password: &str) -> Self {
        self.password_mode = 2;
        self.mailbox_password = Some(password.to_owned());
        self
    }

    pub fn with_human_verification(mut self) -> Self {
        self.hv_required = true;
        self
    }

    pub fn fido2_only(mut self) -> Self {
        self.fido2_only = true;
        self
    }

    pub fn with_key_fault(mut self, fault: KeyFault) -> Self {
        self.key_fault = fault;
        self
    }

    /// The password the key passphrase derives from (spec/04 "Keys").
    pub fn key_password(&self) -> &str {
        self.mailbox_password.as_deref().unwrap_or(&self.password)
    }
}

/// One address the fake serves, with its locked keys. Synthetic
/// addresses on `example.test` only.
#[derive(Debug, Clone)]
struct FakeAddress {
    id: String,
    email: String,
    display_name: String,
    send: bool,
    receive: bool,
    status: u8,
    order: u32,
    keys: Vec<LockedKey>,
    /// Armored public keys `GET /core/v4/keys` serves for the address
    /// (research `keys-crypto.md` §5).
    public_keys: Vec<String>,
}

/// A label the fake serves (research `mail-sync.md` §1). `kind` is
/// Proton's `Type`: 1 user label, 2 contact group, 3 folder, 4 system.
#[derive(Debug, Clone)]
pub struct FakeLabel {
    pub id: String,
    pub parent_id: Option<String>,
    pub name: String,
    pub path: String,
    pub color: String,
    pub kind: u8,
}

impl FakeLabel {
    /// A label with the given Proton `Type` (1 user, 3 folder, 4
    /// system); `path` defaults to the name.
    pub fn new(id: &str, kind: u8, name: &str) -> Self {
        Self {
            id: id.to_owned(),
            parent_id: None,
            name: name.to_owned(),
            path: name.to_owned(),
            color: "#000000".to_owned(),
            kind,
        }
    }

    pub fn with_parent(mut self, parent: &str) -> Self {
        self.parent_id = Some(parent.to_owned());
        self
    }

    /// The `/`-joined display path (research `mail-sync.md` §1).
    pub fn with_path(mut self, path: &str) -> Self {
        self.path = path.to_owned();
        self
    }

    fn to_json(&self) -> Value {
        json!({
            "ID": self.id,
            "ParentID": self.parent_id,
            "Name": self.name,
            "Path": self.path,
            "Color": self.color,
            "Type": self.kind,
        })
    }
}

/// One attachment of a fake message: the plaintext the fake encrypts
/// to the account's primary address key, and the metadata it serves.
#[derive(Debug, Clone)]
pub struct FakeAttachment {
    pub name: String,
    pub mime_type: String,
    pub inline: bool,
    /// `Content-ID` values; one serializes as a string, several as an
    /// array (research `mail-sync.md` §2's `Headers`).
    pub content_ids: Vec<String>,
    pub plaintext: Vec<u8>,
}

impl FakeAttachment {
    /// An attachment with the given name, type and bytes.
    pub fn new(name: &str, mime_type: &str, bytes: &[u8]) -> Self {
        Self {
            name: name.to_owned(),
            mime_type: mime_type.to_owned(),
            inline: false,
            content_ids: Vec::new(),
            plaintext: bytes.to_vec(),
        }
    }

    /// Makes it inline with one `Content-ID`.
    pub fn inline(mut self, content_id: &str) -> Self {
        self.inline = true;
        self.content_ids = vec![content_id.to_owned()];
        self
    }

    /// Makes it inline with several `Content-ID` values, which the fake
    /// serves as an array.
    pub fn inline_with_ids(mut self, content_ids: &[&str]) -> Self {
        self.inline = true;
        self.content_ids = content_ids.iter().map(|id| (*id).to_owned()).collect();
        self
    }
}

/// One message as the fake serves it (research `mail-sync.md` §2, §4).
/// The default is a small synthetic `text/plain` body and a header block
/// consistent with the metadata. The server fills the encrypted fields
/// when the message is added.
#[derive(Debug, Clone)]
pub struct FakeMessage {
    pub id: String,
    pub label_ids: Vec<String>,
    pub time: i64,
    pub unread: bool,
    header: String,
    mime_type: String,
    body: Vec<u8>,
    attachments: Vec<FakeAttachment>,
    encrypted_body: Option<String>,
    encrypted_attachments: Vec<(String, Vec<u8>, Vec<u8>)>,
    /// The subject, sender and recipient lists a server-created draft
    /// carries (M4.3c); the fixtures keep the defaults below.
    subject: String,
    sender: Value,
    to_list: Vec<Value>,
    cc_list: Vec<Value>,
    bcc_list: Vec<Value>,
    flags: u32,
    address_id: String,
}

impl FakeMessage {
    /// A synthetic message in Inbox, unread, from `sender@example.test`.
    pub fn new(id: &str) -> Self {
        Self {
            id: id.to_owned(),
            label_ids: vec!["0".to_owned()],
            time: 1_700_000_000,
            unread: true,
            header: default_header(id),
            mime_type: "text/plain".to_owned(),
            body: MESSAGE_PLAINTEXT.as_bytes().to_vec(),
            attachments: Vec::new(),
            encrypted_body: None,
            encrypted_attachments: Vec::new(),
            subject: format!("synthetic subject {}", id),
            sender: json!({"Name": "Sender", "Address": "sender@example.test"}),
            to_list: Vec::new(),
            cc_list: Vec::new(),
            bcc_list: Vec::new(),
            flags: 1,
            address_id: "owner-primary".to_owned(),
        }
    }

    pub fn with_time(mut self, time: i64) -> Self {
        self.time = time;
        self
    }

    pub fn with_labels(mut self, label_ids: &[&str]) -> Self {
        self.label_ids = label_ids.iter().map(|id| (*id).to_owned()).collect();
        self
    }

    pub fn read(mut self) -> Self {
        self.unread = false;
        self
    }

    /// Replaces the stored header block (M2.16a).
    pub fn with_header(mut self, block: &str) -> Self {
        self.header = block.to_owned();
        self
    }

    /// Replaces the decrypted body and its `MIMEType` (M2.16a).
    pub fn with_body(mut self, mime_type: &str, plaintext: &[u8]) -> Self {
        self.mime_type = mime_type.to_owned();
        self.body = plaintext.to_vec();
        self
    }

    /// Adds an attachment (M2.16a).
    pub fn with_attachment(mut self, attachment: FakeAttachment) -> Self {
        self.attachments.push(attachment);
        self
    }

    fn to_json(&self) -> Value {
        json!({
            "ID": self.id,
            "AddressID": self.address_id,
            "LabelIDs": self.label_ids,
            "ExternalID": format!("<{}@example.test>", self.id),
            "Subject": self.subject,
            "Sender": self.sender,
            "ToList": self.to_list,
            "CCList": self.cc_list,
            "BCCList": self.bcc_list,
            "ReplyTos": [],
            "Time": self.time,
            "Size": self.body.len(),
            "Unread": self.unread,
            "Flags": self.flags,
            "NumAttachments": self.attachments.len(),
        })
    }
}

/// A synthetic header block consistent with the message's metadata.
fn default_header(id: &str) -> String {
    format!(
        "Subject: synthetic subject {id}\r\n\
         From: Sender <sender@example.test>\r\n\
         To: To <to@example.test>\r\n\
         Date: Tue, 14 Nov 2023 22:13:20 +0000\r\n\
         Message-ID: <{id}@example.test>\r\n"
    )
}

/// The attachment JSON the API serves: metadata plus the base64 key
/// packets that match the downloadable data packet.
fn attachment_json(id: &str, attachment: &FakeAttachment, key_packets: &[u8]) -> Value {
    let headers = match attachment.content_ids.len() {
        0 => json!({}),
        1 => json!({"content-id": attachment.content_ids[0]}),
        _ => json!({"content-id": attachment.content_ids}),
    };
    json!({
        "ID": id,
        "Name": attachment.name,
        "Size": attachment.plaintext.len(),
        "MIMEType": attachment.mime_type,
        "Disposition": if attachment.inline { "inline" } else { "attachment" },
        "Headers": headers,
        "KeyPackets": base64::engine::general_purpose::STANDARD.encode(key_packets),
        "Signature": Value::Null,
    })
}

/// Proton's event actions for an item.
const ADDRESS_DELETED: u8 = 0;
const ADDRESS_CREATED: u8 = 1;
const ADDRESS_UPDATED: u8 = 2;

/// One entry of the account's event log. `number` is assigned when it
/// is recorded; the wire id is `evt-<number>`. The item arrays hold
/// the JSON objects the events response will carry.
#[derive(Default)]
struct FakeEvent {
    number: u64,
    refresh: bool,
    user: Option<Value>,
    messages: Vec<Value>,
    labels: Vec<Value>,
    addresses: Vec<Value>,
}

impl FakeEvent {
    fn label(id: String, action: u8, label: Option<FakeLabel>) -> Self {
        Self {
            labels: vec![with_optional(
                json!({"ID": id, "Action": action}),
                "Label",
                label.map(|label| label.to_json()),
            )],
            ..Self::default()
        }
    }

    fn message(id: String, action: u8, message: Option<FakeMessage>) -> Self {
        Self {
            messages: vec![with_optional(
                json!({"ID": id, "Action": action}),
                "Message",
                message.map(|message| message.to_json()),
            )],
            ..Self::default()
        }
    }

    fn user(user: Value) -> Self {
        Self {
            user: Some(user),
            ..Self::default()
        }
    }

    /// An address event carries only the id and the action
    /// ([`ADDRESS_UPDATED`] for a change).
    fn address(id: String, action: u8) -> Self {
        Self {
            addresses: vec![json!({"ID": id, "Action": action})],
            ..Self::default()
        }
    }
}

/// One logged request. The log deliberately records only header
/// presence and the honest identity values: never the UID, the bearer
/// token, the TOTP or the human-verification token (spec/10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    /// The raw query string, so tests can assert the paging and label
    /// type parameters the transport sent.
    pub query: Option<String>,
    pub app_version: Option<String>,
    pub user_agent: Option<String>,
    pub content_type: Option<String>,
    pub has_uid: bool,
    pub has_authorization: bool,
    pub hv_token_present: bool,
}

/// A fault knob. `Drop` closes the connection; `Stall` delays every
/// response; `RateLimit` answers 429/503 with an optional
/// `Retry-After`; `Code` answers a status and Proton code;
/// `DropAfterApply` applies a send call then drops the reply;
/// `ApplyThenRefuseConnection` applies it, closes the connection and
/// refuses every later one; `RefuseConnection` closes the next
/// accepted connection before any TLS byte; `RefuseConnections` closes
/// every one; `OnceSend` waits for the next send; `StallHandshake`
/// sends nothing for a duration before the TLS handshake; a bad server
/// proof makes `/auth/v4` return a proof that cannot match.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Fault {
    Drop,
    Stall(Duration),
    RateLimit {
        status: u16,
        retry_after: Option<u16>,
    },
    Code {
        status: u16,
        code: i64,
    },
    BadServerProof,
    /// Applies to the next request only, then clears itself.
    Once(Box<Fault>),
    /// Applies to the next request for `path` only, then clears itself;
    /// other requests pass meanwhile.
    OnceAt {
        path: String,
        fault: Box<Fault>,
    },
    /// Applies to the next send (`POST /mail/v4/messages/{id}`) only,
    /// then clears itself; a draft creation (`POST
    /// /mail/v4/messages`) passes meanwhile, so a test can fault the
    /// send without naming the draft id the server will choose.
    OnceSend(Box<Fault>),
    /// Applies a draft creation, attachment upload or send, then drops
    /// the reply: the effect stays, the client sees a lost reply
    /// (05's `outcome_unknown`). One-shot through [`Fault::Once`].
    DropAfterApply,
    /// Applies a draft creation, attachment upload or send, serves the
    /// reply with `connection: close`, and refuses every later accepted
    /// connection. The client's pool discards the closed connection, so
    /// its next request finds one that was never established (05's
    /// pre-request retry), and the transport's own connect retries all
    /// fail. One-shot through [`Fault::Once`] or [`Fault::OnceAt`].
    ApplyThenRefuseConnection,
    /// Closes the next accepted TCP connection before any TLS byte, so
    /// a client's handshake fails: it can tell the connection was never
    /// established from one lost after the request began. One-shot.
    RefuseConnection,
    /// Closes every accepted TCP connection before any TLS byte, until
    /// replaced: the client's connect retries all fail.
    RefuseConnections,
    /// Accepts the next TCP connection and sends nothing for `Duration`
    /// before the TLS handshake, so a client with a deadline sees a
    /// connect timeout. One-shot.
    StallHandshake(Duration),
}

/// Takes a connection-level fault ([`Fault::RefuseConnection`] or
/// [`Fault::StallHandshake`]), including one wrapped in
/// [`Fault::Once`], so the accept loop can act before the TLS
/// handshake. Other faults stay in the slot for the request handlers.
fn take_connection_fault(faults: &Mutex<Option<Fault>>) -> Option<Fault> {
    let mut slot = faults.lock().unwrap_or_else(|e| e.into_inner());
    match slot.take()? {
        Fault::RefuseConnection => Some(Fault::RefuseConnection),
        fault @ Fault::RefuseConnections => {
            *slot = Some(fault.clone());
            Some(fault)
        }
        fault @ Fault::StallHandshake(_) => Some(fault),
        Fault::Once(inner) => match *inner {
            Fault::RefuseConnection => Some(Fault::RefuseConnection),
            fault @ Fault::RefuseConnections => {
                *slot = Some(fault.clone());
                Some(fault)
            }
            fault @ Fault::StallHandshake(_) => Some(fault),
            other => {
                *slot = Some(Fault::Once(Box::new(other)));
                None
            }
        },
        other => {
            *slot = Some(other);
            None
        }
    }
}

struct Account {
    verifier: ServerClientVerifier,
    salt_b64: String,
    totp: Option<String>,
    password_mode: u8,
    hv_required: bool,
    fido2_only: bool,
    key_salts: Vec<Salt>,
    user_keys: UserKeys,
    addresses: Vec<FakeAddress>,
    /// Addresses `delete_address` took away, for `restore_address`.
    deleted_addresses: Vec<FakeAddress>,
    message_body: String,
    damaged_body: String,
    attachment_key_packets: Vec<u8>,
    attachment_data_packet: Vec<u8>,
    attachment_plaintext: Vec<u8>,
    /// Encrypts a message body to the primary address key, armored.
    encrypt_body: EncryptBody,
    /// Encrypts an attachment to the primary address key, returning the
    /// key packets and the data packet.
    encrypt_attachment: EncryptAttachment,
    /// Unlocked address keys by email, for tests that decrypt as an
    /// internal recipient (spec/11 M4.3c).
    recipient_keys: HashMap<String, Arc<dyn send::RecipientKey>>,
    labels: Vec<FakeLabel>,
    messages: Vec<FakeMessage>,
    events: Vec<FakeEvent>,
    event_counter: u64,
    next_refresh: bool,
    malformed_messages: bool,
    wrong_message_id: bool,
    /// Answers a by-id metadata read with another message's id, which
    /// the transport must refuse as `UnexpectedMessage` (plan M2.16d).
    wrong_metadata_id: bool,
    /// Message ids whose full-message request answers with the
    /// account's damaged body fixture (plan M2.16c).
    damaged_bodies: HashSet<String>,
    /// Message ids a by-id metadata read (`ID` filter) answers none
    /// for, while the unfiltered listing and the full-message read
    /// still serve them (plan M2.16d's empty re-read).
    metadata_hidden: HashSet<String>,
    /// Message ids whose by-id metadata answer carries these labels
    /// instead of the stored ones, as metadata changed since the
    /// listing (plan M2.16d's applied re-read).
    metadata_answer_labels: HashMap<String, Vec<String>>,
}

impl Account {
    /// Encrypts a message's body and attachments to the primary address
    /// key, as an API message carries them.
    fn encrypt_message(&self, message: &mut FakeMessage) {
        let armored = (self.encrypt_body)(&message.body);
        message.encrypted_body =
            Some(String::from_utf8(armored).expect("the armored body is ASCII"));
        message.encrypted_attachments = message
            .attachments
            .iter()
            .enumerate()
            .map(|(index, attachment)| {
                let (key_packets, data_packet) = (self.encrypt_attachment)(&attachment.plaintext);
                (
                    format!("{}-att-{index}", message.id),
                    key_packets,
                    data_packet,
                )
            })
            .collect();
    }

    /// Appends an event, assigning its id and consuming the one-shot
    /// refresh marker (spec/04 "Refresh").
    fn record(&mut self, mut event: FakeEvent) {
        self.event_counter += 1;
        event.number = self.event_counter;
        if self.next_refresh {
            event.refresh = true;
            self.next_refresh = false;
        }
        self.events.push(event);
    }
}

struct Session {
    user_id: String,
    access_token: String,
    refresh_token: String,
    password_mode: u8,
}

struct State {
    accounts: HashMap<String, Account>,
    srp: HashMap<String, ServerInteraction>,
    sessions: HashMap<String, Session>,
    hv_tokens: HashSet<String>,
    counter: u64,
    signed_modulus: String,
    public_key: SignedPublicKey,
    /// Armored public keys registered for external addresses, so a
    /// client's "external with keys" refusal can be tested (M4.3c).
    external_keys: HashMap<String, Vec<String>>,
    /// Every applied send, in order, with the packages it carried.
    recorded_sends: Vec<send::RecordedSend>,
    /// The fake's own page-size cap; the transport clamps to the same
    /// 150 (research `mail-sync.md` §3).
    max_page_size: u32,
    /// At most this many events per `More` batch; 0 means no split.
    max_events_per_response: usize,
    /// Makes the next listing answer `Stale`.
    stale: bool,
}

/// The in-process fake. Drop aborts the accept loop.
pub struct FakeProtonServer {
    base_url: String,
    log: Arc<Mutex<Vec<RecordedRequest>>>,
    faults: Arc<Mutex<Option<Fault>>>,
    state: Arc<Mutex<State>>,
    max_in_flight: Arc<AtomicUsize>,
    handle: JoinHandle<()>,
}

impl std::fmt::Debug for FakeProtonServer {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("FakeProtonServer")
            .field("base_url", &self.base_url)
            .finish_non_exhaustive()
    }
}

impl FakeProtonServer {
    /// Binds `127.0.0.1:0`, provisions the accounts and serves HTTPS.
    pub async fn serve(accounts: Vec<TestAccount>) -> io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();
        let (signed_modulus, public_key) = test_identity();
        let mut state = State {
            accounts: HashMap::new(),
            srp: HashMap::new(),
            sessions: HashMap::new(),
            hv_tokens: HashSet::new(),
            counter: 0,
            signed_modulus,
            public_key,
            external_keys: HashMap::new(),
            recorded_sends: Vec::new(),
            max_page_size: 150,
            max_events_per_response: 0,
            stale: false,
        };
        let provider = ProtonPGP::new_rust_sync();
        let srp = proton_crypto::ProtonSRP::new_sync();
        for spec in accounts {
            let random = SeededRandom::new(seed(&spec.username));
            let mut salt = [0u8; 10];
            random.fill(&mut salt).expect("salt");
            let salt_b64 = base64::engine::general_purpose::STANDARD.encode(salt);
            let verifier = SRPAuth::generate_verifier(
                &TestVerifier(state.public_key.clone()),
                &spec.password,
                Some(&salt_b64),
                &state.signed_modulus,
            )
            .expect("the test modulus verifies with the test key");

            // The keys and fixtures are generated in code at start-up
            // (spec/11); the salts are seeded from the username, the key
            // material from the OS RNG. No binary fixtures.
            let mut key_salt_bytes = [0u8; 16];
            random.fill(&mut key_salt_bytes).expect("key salt");
            let key_salt = KeySalt::from_bytes(&key_salt_bytes);
            let key_secret = key_salt
                .salted_key_passphrase(&srp, spec.key_password().as_bytes())
                .expect("the key passphrase derives");
            let user_local =
                LocalUserKey::generate(&provider, KeyGeneratorAlgorithm::default(), &key_secret)
                    .expect("the user key generates");
            let user_keys = UserKeys::new(vec![user_locked_key(
                &format!("user-key-{}", spec.username),
                user_local,
            )]);
            let user_unlocked = user_keys
                .unlock(&provider, &key_secret)
                .unlocked_keys
                .first()
                .expect("the fresh user key unlocks")
                .clone();
            let local = spec.username.split('@').next().unwrap_or("user");
            let (primary_email, alias_email, disabled_email) = (
                format!("{local}@example.test"),
                format!("{local}-alias@example.test"),
                format!("{local}-disabled@example.test"),
            );
            let mut addresses = Vec::new();
            let primary_local = LocalAddressKey::generate(
                &provider,
                &primary_email,
                KeyGeneratorAlgorithm::default(),
                KeyFlag::default(),
                true,
                &user_unlocked,
            )
            .expect("the primary address key generates");
            let primary_unlocked = primary_local
                .unlock_and_assign_key_id(
                    &provider,
                    KeyId::from(format!("{local}-primary-key")),
                    &user_unlocked,
                )
                .expect("the primary address key unlocks");
            let mut primary = address_locked_key(&format!("{local}-primary-key"), primary_local);
            let legacy_token = LocalAddressKey::generate(
                &provider,
                &alias_email,
                KeyGeneratorAlgorithm::default(),
                KeyFlag::default(),
                false,
                &user_unlocked,
            )
            .expect("the legacy address key generates");
            let legacy_unlocked = legacy_token
                .unlock_and_assign_key_id(
                    &provider,
                    KeyId::from(format!("{local}-legacy-key")),
                    &user_unlocked,
                )
                .expect("the legacy address key unlocks");
            let mut legacy = address_locked_key(
                &format!("{local}-legacy-key"),
                LocalAddressKey::relock_address_key_legacy(
                    &provider,
                    &legacy_unlocked,
                    &key_secret,
                )
                .expect("the legacy address key relocks"),
            );
            let disabled_local = LocalAddressKey::generate(
                &provider,
                &disabled_email,
                KeyGeneratorAlgorithm::default(),
                KeyFlag::default(),
                false,
                &user_unlocked,
            )
            .expect("the disabled address key generates");
            let disabled_unlocked = disabled_local
                .unlock_and_assign_key_id(
                    &provider,
                    KeyId::from(format!("{local}-disabled-key")),
                    &user_unlocked,
                )
                .expect("the disabled address key unlocks");
            let mut disabled = address_locked_key(&format!("{local}-disabled-key"), disabled_local);
            let primary_public = public_key_armor(&provider, &primary_unlocked.public_key);
            let alias_public = public_key_armor(&provider, &legacy_unlocked.public_key);
            let disabled_public = public_key_armor(&provider, &disabled_unlocked.public_key);
            if spec.key_fault != KeyFault::None {
                mislock_key(&provider, &mut legacy, &legacy_unlocked);
            }
            if spec.key_fault == KeyFault::RetiredAddressKey {
                legacy.active = false;
            }
            if spec.key_fault == KeyFault::AllAddressKeys {
                mislock_key(&provider, &mut primary, &primary_unlocked);
                mislock_key(&provider, &mut disabled, &disabled_unlocked);
            }
            addresses.push(FakeAddress {
                id: format!("{local}-primary"),
                email: primary_email.clone(),
                display_name: format!("{local} primary"),
                send: true,
                receive: true,
                status: 1,
                order: 1,
                keys: vec![primary],
                public_keys: vec![primary_public.clone()],
            });
            addresses.push(FakeAddress {
                id: format!("{local}-alias"),
                email: alias_email.clone(),
                display_name: format!("{local} alias"),
                send: false,
                receive: true,
                status: 1,
                order: 2,
                keys: vec![legacy],
                public_keys: vec![alias_public.clone()],
            });
            addresses.push(FakeAddress {
                id: format!("{local}-disabled"),
                email: disabled_email.clone(),
                display_name: format!("{local} disabled"),
                send: true,
                receive: true,
                status: 0,
                order: 3,
                keys: vec![disabled],
                public_keys: vec![disabled_public.clone()],
            });
            let recipient_keys = HashMap::from([
                (
                    primary_email,
                    send::recipient_key(&provider, primary_public, &primary_unlocked.private_key),
                ),
                (
                    alias_email,
                    send::recipient_key(&provider, alias_public, &legacy_unlocked.private_key),
                ),
                (
                    disabled_email,
                    send::recipient_key(&provider, disabled_public, &disabled_unlocked.private_key),
                ),
            ]);

            // Every message and attachment body is encrypted to the
            // primary address key when it is added (M2.16a).
            let (encrypt_body, encrypt_attachment) = encryptors(&provider, &primary_unlocked);

            // The fixtures are encrypted to the primary address key.
            let mut body = Vec::new();
            provider
                .new_encryptor()
                .with_encryption_key(&primary_unlocked.public_key)
                .with_signing_key(&primary_unlocked.private_key)
                .encrypt_to_writer(
                    MESSAGE_PLAINTEXT.as_bytes(),
                    DataEncoding::Armor,
                    SigningMode::Inline,
                    WritingMode::All,
                    &mut body,
                )
                .expect("the message body encrypts");
            let message_body = String::from_utf8(body).expect("the armored body is ASCII");
            let damaged_body = damage_armor(&message_body);
            let mut data_packet = Vec::new();
            let detached = provider
                .new_encryptor()
                .with_encryption_key(&primary_unlocked.public_key)
                .encrypt_to_writer(
                    ATTACHMENT_PLAINTEXT,
                    DataEncoding::Bytes,
                    SigningMode::Inline,
                    WritingMode::SplitKeyPackets,
                    &mut data_packet,
                )
                .expect("the attachment encrypts");
            let attachment_key_packets = detached
                .key_packets
                .expect("split mode returns the key packets");

            state.accounts.insert(
                spec.username.clone(),
                Account {
                    verifier: ServerClientVerifier::from(&verifier),
                    salt_b64,
                    totp: spec.totp,
                    password_mode: spec.password_mode,
                    hv_required: spec.hv_required,
                    fido2_only: spec.fido2_only,
                    key_salts: vec![Salt {
                        id: KeyId::from(format!("user-key-{}", spec.username)),
                        key_salt: Some(key_salt),
                    }],
                    user_keys,
                    addresses,
                    recipient_keys,
                    deleted_addresses: Vec::new(),
                    message_body,
                    damaged_body,
                    attachment_key_packets,
                    attachment_data_packet: data_packet,
                    attachment_plaintext: ATTACHMENT_PLAINTEXT.to_vec(),
                    encrypt_body,
                    encrypt_attachment,
                    labels: system_labels(),
                    messages: Vec::new(),
                    events: Vec::new(),
                    event_counter: 0,
                    next_refresh: false,
                    malformed_messages: false,
                    wrong_message_id: false,
                    wrong_metadata_id: false,
                    damaged_bodies: HashSet::new(),
                    metadata_hidden: HashSet::new(),
                    metadata_answer_labels: HashMap::new(),
                },
            );
        }

        let log = Arc::new(Mutex::new(Vec::new()));
        let faults = Arc::new(Mutex::new(None));
        let in_flight = Arc::new(AtomicUsize::new(0));
        let max_in_flight = Arc::new(AtomicUsize::new(0));
        let state = Arc::new(Mutex::new(state));
        let acceptor = TlsAcceptor::from(server_config());
        let handle = tokio::spawn({
            let state = Arc::clone(&state);
            let log = Arc::clone(&log);
            let faults = Arc::clone(&faults);
            let in_flight = Arc::clone(&in_flight);
            let max_in_flight = Arc::clone(&max_in_flight);
            async move {
                loop {
                    let (tcp, _) = match listener.accept().await {
                        Ok(accepted) => accepted,
                        Err(_) => break,
                    };
                    let connection_fault = take_connection_fault(&faults);
                    let acceptor = acceptor.clone();
                    let state = Arc::clone(&state);
                    let log = Arc::clone(&log);
                    let faults = Arc::clone(&faults);
                    let in_flight = Arc::clone(&in_flight);
                    let max_in_flight = Arc::clone(&max_in_flight);
                    tokio::spawn(async move {
                        match connection_fault {
                            None => {}
                            // Dropping `tcp` closes the connection before
                            // any TLS byte, so the client's handshake
                            // fails as a connect error.
                            Some(Fault::RefuseConnection) => return,
                            Some(Fault::RefuseConnections) => return,
                            Some(Fault::StallHandshake(delay)) => {
                                tokio::time::sleep(delay).await;
                            }
                            // `take_connection_fault` only returns the
                            // two connection faults.
                            Some(_) => {}
                        }
                        let tls = match acceptor.accept(tcp).await {
                            Ok(tls) => tls,
                            Err(_) => return,
                        };
                        let service = hyper::service::service_fn(move |request| {
                            handle_request(
                                request,
                                Arc::clone(&state),
                                Arc::clone(&log),
                                Arc::clone(&faults),
                                Arc::clone(&in_flight),
                                Arc::clone(&max_in_flight),
                            )
                        });
                        let _ = hyper::server::conn::http1::Builder::new()
                            .serve_connection(TokioIo::new(tls), service)
                            .await;
                    });
                }
            }
        });
        Ok(Self {
            base_url: format!("https://127.0.0.1:{port}"),
            log,
            faults,
            state,
            max_in_flight,
            handle,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// A verifier that trusts the fake's test key, for the client under
    /// test's injectable modulus verifier.
    pub fn verifier(&self) -> Arc<dyn ModulusSignatureVerifier + Send + Sync> {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        Arc::new(TestVerifier(state.public_key.clone()))
    }

    /// The armored test public key its signed modulus carries, so a
    /// separate process (a crash-cut child) can build the same verifier
    /// from [`fake_modulus_verifier`].
    pub fn verifier_public_key(&self) -> String {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state
            .public_key
            .to_armored_string(ArmorOptions::default())
            .expect("the fake's public key armors")
    }

    /// Every request the server has seen, in order.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.log.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set_fault(&self, fault: Option<Fault>) {
        *self.faults.lock().unwrap_or_else(|e| e.into_inner()) = fault;
    }

    /// How many sessions the server has issued and not revoked.
    pub fn session_count(&self) -> usize {
        self.state
            .lock()
            .unwrap_or_else(|e| e.into_inner())
            .sessions
            .len()
    }

    /// The largest number of requests being handled at once.
    pub fn max_in_flight(&self) -> usize {
        self.max_in_flight.load(Ordering::SeqCst)
    }

    /// Adds a label and appends a create event.
    pub fn add_label(&self, username: &str, label: FakeLabel) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let account = account_mut(&mut state, username);
        account.record(FakeEvent::label(label.id.clone(), 1, Some(label.clone())));
        account.labels.push(label);
    }

    /// Replaces a label and appends an update event.
    pub fn update_label(&self, username: &str, label: FakeLabel) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let account = account_mut(&mut state, username);
        let existing = account
            .labels
            .iter_mut()
            .find(|existing| existing.id == label.id)
            .expect("the label exists");
        *existing = label.clone();
        account.record(FakeEvent::label(label.id.clone(), 2, Some(label)));
    }

    /// Removes a label and appends a delete event.
    pub fn delete_label(&self, username: &str, id: &str) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let account = account_mut(&mut state, username);
        account.labels.retain(|label| label.id != id);
        account.record(FakeEvent::label(id.to_owned(), 0, None));
    }

    /// Adds a message and appends a create event; returns its id. The
    /// body and attachments are encrypted to the account's primary
    /// address key.
    pub fn add_message(&self, username: &str, mut message: FakeMessage) -> String {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let account = account_mut(&mut state, username);
        let id = message.id.clone();
        account.encrypt_message(&mut message);
        account.record(FakeEvent::message(id.clone(), 1, Some(message.clone())));
        account.messages.push(message);
        id
    }

    /// Replaces a message's metadata and body and appends an update
    /// event.
    pub fn update_message(&self, username: &str, mut message: FakeMessage) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let account = account_mut(&mut state, username);
        assert!(
            account
                .messages
                .iter()
                .any(|existing| existing.id == message.id),
            "the message exists"
        );
        let id = message.id.clone();
        account.encrypt_message(&mut message);
        account.record(FakeEvent::message(id, 2, Some(message.clone())));
        let existing = account
            .messages
            .iter_mut()
            .find(|existing| existing.id == message.id)
            .expect("the message exists");
        *existing = message;
    }

    /// Removes a message and appends a delete event.
    pub fn delete_message(&self, username: &str, id: &str) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let account = account_mut(&mut state, username);
        account.messages.retain(|message| message.id != id);
        account.record(FakeEvent::message(id.to_owned(), 0, None));
    }

    /// Appends an event carrying a `User` object.
    pub fn touch_user(&self, username: &str) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let account = account_mut(&mut state, username);
        let user = json!({"ID": format!("user-{username}"), "Name": username});
        account.record(FakeEvent::user(user));
    }

    /// Renames an address and appends an address update event.
    pub fn rename_address(&self, username: &str, id: &str, display_name: &str) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let account = account_mut(&mut state, username);
        account
            .addresses
            .iter_mut()
            .find(|address| address.id == id)
            .expect("the address exists")
            .display_name = display_name.to_owned();
        account.record(FakeEvent::address(id.to_owned(), ADDRESS_UPDATED));
    }

    /// Removes an address and appends an address delete event.
    pub fn delete_address(&self, username: &str, id: &str) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let account = account_mut(&mut state, username);
        let index = account
            .addresses
            .iter()
            .position(|address| address.id == id)
            .expect("the address exists");
        let address = account.addresses.remove(index);
        account.deleted_addresses.push(address);
        account.record(FakeEvent::address(id.to_owned(), ADDRESS_DELETED));
    }

    /// Brings back an address `delete_address` removed, in its place by
    /// `order`, and appends an address create event.
    pub fn restore_address(&self, username: &str, id: &str) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let account = account_mut(&mut state, username);
        let index = account
            .deleted_addresses
            .iter()
            .position(|address| address.id == id)
            .expect("the address was deleted");
        let address = account.deleted_addresses.remove(index);
        account.addresses.push(address);
        account.addresses.sort_by_key(|address| address.order);
        account.record(FakeEvent::address(id.to_owned(), ADDRESS_CREATED));
    }

    /// Marks every address key inactive and appends a user event. A
    /// password change made elsewhere leaves the keys active but
    /// encrypted under another passphrase; either way no address key
    /// unlocks, and `KeySet::unlock_with_secret` fails the same way
    /// (`NoAddressKey`).
    pub fn retire_address_keys(&self, username: &str) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let account = account_mut(&mut state, username);
        for address in &mut account.addresses {
            for key in &mut address.keys {
                key.active = false;
            }
        }
        let user = json!({"ID": format!("user-{username}"), "Name": username});
        account.record(FakeEvent::user(user));
    }

    /// Makes every address key active again, as the keys a sign-in
    /// again unlocks; records no event.
    pub fn reactivate_address_keys(&self, username: &str) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let account = account_mut(&mut state, username);
        for address in &mut account.addresses {
            for key in &mut address.keys {
                key.active = true;
            }
        }
    }

    /// The next recorded event carries the mail `Refresh` bit.
    pub fn set_refresh_mail(&self, username: &str) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        account_mut(&mut state, username).next_refresh = true;
    }

    /// At most `max` events per response; the rest follow under `More`.
    /// 0 (the default) never splits.
    pub fn set_max_events_per_response(&self, max: usize) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.max_events_per_response = max;
    }

    /// Makes message listings answer `Stale`.
    pub fn set_stale_pages(&self, stale: bool) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        state.stale = stale;
    }

    /// Serves listed messages with a numeric `ID`, which the transport
    /// must refuse as a decode failure.
    pub fn set_undecodable_message(&self, username: &str, on: bool) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        account_mut(&mut state, username).malformed_messages = on;
    }

    /// Answers a full-message request with another message's id, which
    /// the transport must refuse (spec/04 step 3).
    pub fn set_wrong_message_id(&self, username: &str, on: bool) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        account_mut(&mut state, username).wrong_message_id = on;
    }

    /// Answers a by-id metadata read with another message's id, which
    /// the transport must refuse as `UnexpectedMessage` (plan M2.16d).
    pub fn set_wrong_metadata_id(&self, username: &str, on: bool) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        account_mut(&mut state, username).wrong_metadata_id = on;
    }

    /// Makes a by-id metadata read answer none for `id`, or answer it
    /// again (plan M2.16d). The unfiltered listing and the
    /// full-message read still serve it.
    pub fn set_metadata_hidden(&self, username: &str, id: &str, on: bool) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let account = account_mut(&mut state, username);
        if on {
            account.metadata_hidden.insert(id.to_owned());
        } else {
            account.metadata_hidden.remove(id);
        }
    }

    /// Makes a by-id metadata read answer `labels` for `id`, as
    /// metadata that moved since the listing (plan M2.16d). The
    /// unfiltered listing is unaffected.
    pub fn set_metadata_answer_labels(&self, username: &str, id: &str, labels: &[&str]) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let account = account_mut(&mut state, username);
        account.metadata_answer_labels.insert(
            id.to_owned(),
            labels.iter().map(|label| (*label).to_owned()).collect(),
        );
    }

    /// Makes `id`'s full-message request answer with the account's
    /// damaged body fixture, or serve its real body again (plan
    /// M2.16c). The fixture matches the default message body.
    pub fn set_damaged_body(&self, username: &str, id: &str, on: bool) {
        let mut state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let account = account_mut(&mut state, username);
        if on {
            account.damaged_bodies.insert(id.to_owned());
        } else {
            account.damaged_bodies.remove(id);
        }
    }

    /// The encrypted message body fixture and a damaged copy. Taken
    /// from the server's first account; M2.12's servers hold one.
    pub fn message_fixtures(&self) -> (String, String) {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let account = state
            .accounts
            .values()
            .next()
            .expect("the server has an account");
        (account.message_body.clone(), account.damaged_body.clone())
    }

    /// The attachment fixture: key packets, data packet and plaintext.
    pub fn attachment_fixture(&self) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
        let state = self.state.lock().unwrap_or_else(|e| e.into_inner());
        let account = state
            .accounts
            .values()
            .next()
            .expect("the server has an account");
        (
            account.attachment_key_packets.clone(),
            account.attachment_data_packet.clone(),
            account.attachment_plaintext.clone(),
        )
    }
}

impl Drop for FakeProtonServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

fn seed(username: &str) -> u64 {
    let mut hash = 0xcbf2_9ce4_8422_2325u64;
    for byte in username.bytes() {
        hash ^= u64::from(byte);
        hash = hash.wrapping_mul(0x0000_0100_0000_01b3);
    }
    hash
}

async fn handle_request(
    request: Request<Incoming>,
    state: Arc<Mutex<State>>,
    log: Arc<Mutex<Vec<RecordedRequest>>>,
    faults: Arc<Mutex<Option<Fault>>>,
    in_flight: Arc<AtomicUsize>,
    max_in_flight: Arc<AtomicUsize>,
) -> Result<Response<Full<Bytes>>, io::Error> {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let query = request.uri().query().map(str::to_owned);
    let headers = request.headers().clone();
    log.lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(RecordedRequest {
            method: method.as_str().to_owned(),
            path: path.clone(),
            query: query.clone(),
            app_version: header_str(&headers, "x-pm-appversion"),
            user_agent: header_str(&headers, "user-agent"),
            content_type: header_str(&headers, "content-type"),
            has_uid: headers.contains_key("x-pm-uid"),
            has_authorization: headers.contains_key("authorization"),
            hv_token_present: headers.contains_key("x-pm-human-verification-token"),
        });
    let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
    max_in_flight.fetch_max(now, Ordering::SeqCst);
    let outcome = dispatch(
        request,
        &state,
        &faults,
        &headers,
        &method,
        &path,
        query.as_deref(),
    )
    .await;
    in_flight.fetch_sub(1, Ordering::SeqCst);
    outcome
}

fn header_str(headers: &hyper::HeaderMap, name: &str) -> Option<String> {
    headers
        .get(name)
        .and_then(|value| value.to_str().ok())
        .map(str::to_owned)
}

async fn dispatch(
    request: Request<Incoming>,
    state: &Arc<Mutex<State>>,
    faults: &Arc<Mutex<Option<Fault>>>,
    headers: &hyper::HeaderMap,
    method: &hyper::Method,
    path: &str,
    query: Option<&str>,
) -> Result<Response<Full<Bytes>>, io::Error> {
    // Read and clear a one-shot fault under one lock, so two concurrent
    // requests can't both take it.
    let fault = {
        let mut slot = faults.lock().unwrap_or_else(|e| e.into_inner());
        let is_send = method == "POST" && path.starts_with("/mail/v4/messages/");
        match slot.take() {
            Some(Fault::Once(inner)) => Some(*inner),
            Some(Fault::OnceAt { path: at, fault }) if at == path => Some(*fault),
            Some(waiting @ Fault::OnceAt { .. }) => {
                *slot = Some(waiting);
                None
            }
            Some(waiting @ Fault::OnceSend(_)) if !is_send => {
                *slot = Some(waiting);
                None
            }
            Some(Fault::OnceSend(inner)) => Some(*inner),
            other => {
                slot.clone_from(&other);
                other
            }
        }
    };
    let mut drop_reply_after_apply = false;
    let mut close_and_refuse_next = false;
    if let Some(fault) = fault {
        match fault {
            Fault::Drop => {
                return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "drop"));
            }
            Fault::DropAfterApply => drop_reply_after_apply = true,
            Fault::ApplyThenRefuseConnection => close_and_refuse_next = true,
            Fault::Stall(delay) => tokio::time::sleep(delay).await,
            Fault::RateLimit {
                status,
                retry_after,
            } => {
                let mut builder = Response::builder().status(status);
                if let Some(seconds) = retry_after {
                    builder = builder.header("retry-after", seconds.to_string());
                }
                let code = if status == 429 { 2028 } else { 5003 };
                return Ok(builder
                    .body(json_bytes(json!({"Code": code, "Error": "slow down"})))
                    .unwrap());
            }
            Fault::Code { status, code } => {
                return Ok(json_response(
                    status,
                    json!({"Code": code, "Error": "fault injection"}),
                ));
            }
            Fault::BadServerProof => {}
            // Connection-level faults are consumed by the accept loop
            // before the handshake; if one reaches here it was never
            // triggered, so let the request through.
            Fault::RefuseConnection | Fault::RefuseConnections | Fault::StallHandshake(_) => {}
            // One-shot faults are unwrapped into their inner fault
            // before the match.
            Fault::Once(_) | Fault::OnceAt { .. } | Fault::OnceSend(_) => {}
        }
    }

    let body = request
        .into_body()
        .collect()
        .await
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
        .to_bytes();
    let outcome = match (method.as_str(), path) {
        ("POST", "/auth/v4/info") => handle_info(&body, state),
        ("POST", "/auth/v4") => handle_auth(&body, state, headers, faults),
        ("POST", "/auth/v4/2fa") => handle_2fa(&body, state, headers),
        ("POST", "/auth/v4/refresh") => handle_refresh(&body, state),
        ("DELETE", "/auth/v4") => handle_logout(state, headers),
        ("GET", "/core/v4/keys/salts") => handle_salts(state, headers),
        ("GET", "/core/v4/users") => handle_user(state, headers),
        ("GET", "/core/v4/addresses") => handle_addresses(state, headers),
        ("GET", "/core/v4/labels") => handle_labels(state, headers, query),
        ("GET", "/core/v4/keys") => send::handle_keys(state, headers, query),
        ("GET", "/mail/v4/messages") => handle_messages(state, headers, query),
        ("GET", "/mail/v4/messages/count") => handle_message_counts(state, headers),
        ("POST", "/mail/v4/messages") => send::handle_draft_write(state, headers, None, &body),
        ("POST", "/mail/v4/attachments") => send::handle_upload_attachment(state, headers, &body),
        ("GET", "/core/v4/events/latest") => handle_latest_event(state, headers),
        ("GET", "/tests/ping") => Ok(json_response(200, json!({"Code": 1000}))),
        _ => {
            if let Some(id) = path.strip_prefix("/mail/v4/messages/") {
                match method.as_str() {
                    "PUT" => send::handle_draft_write(state, headers, Some(id), &body),
                    "POST" => send::handle_send(state, headers, id, &body),
                    "GET" => handle_message(state, headers, id),
                    _ => Ok(json_response(
                        404,
                        json!({"Code": 2001, "Error": "not found"}),
                    )),
                }
            } else if method.as_str() != "GET" {
                Ok(json_response(
                    404,
                    json!({"Code": 2001, "Error": "not found"}),
                ))
            } else if let Some(id) = path.strip_prefix("/mail/v4/attachments/") {
                handle_attachment(state, headers, id)
            } else if let Some(id) = path.strip_prefix("/core/v4/events/") {
                handle_events(state, headers, id)
            } else {
                Ok(json_response(
                    404,
                    json!({"Code": 2001, "Error": "not found"}),
                ))
            }
        }
    };
    if drop_reply_after_apply
        && is_apply_call(method.as_str(), path)
        && outcome
            .as_ref()
            .map(|response| response.status().is_success())
            .unwrap_or(false)
    {
        return Err(io::Error::new(
            io::ErrorKind::ConnectionAborted,
            "drop after apply",
        ));
    }
    if close_and_refuse_next
        && is_apply_call(method.as_str(), path)
        && outcome
            .as_ref()
            .map(|response| response.status().is_success())
            .unwrap_or(false)
    {
        // The reply is served, then the connection closes; the next
        // accepted connection is refused, so the client's next request
        // sees a connection that was never established.
        *faults.lock().unwrap_or_else(|e| e.into_inner()) = Some(Fault::RefuseConnections);
        let mut response = outcome?;
        response.headers_mut().insert(
            hyper::header::CONNECTION,
            hyper::header::HeaderValue::from_static("close"),
        );
        return Ok(response);
    }
    outcome
}

/// Whether a call is one whose effect [`Fault::DropAfterApply`] covers:
/// a draft creation, an attachment upload or a send.
fn is_apply_call(method: &str, path: &str) -> bool {
    if method != "POST" {
        return false;
    }
    path == "/mail/v4/messages"
        || path == "/mail/v4/attachments"
        || path.starts_with("/mail/v4/messages/")
}

fn handle_info(body: &[u8], state: &Arc<Mutex<State>>) -> Result<Response<Full<Bytes>>, io::Error> {
    let value: Value = serde_json::from_slice(body).unwrap_or(json!({}));
    let username = value
        .get("Username")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    let Some(account) = state.accounts.get(&username) else {
        return Ok(json_response(
            400,
            json!({"Code": 6003, "Error": "UsernameInvalid"}),
        ));
    };
    let verifier = account.verifier.clone();
    let salt_b64 = account.salt_b64.clone();
    let signed_modulus = state.signed_modulus.clone();
    let raw = RawSRPModulus::new(MODULUS_B64).expect("the test modulus is valid");
    let mut server = ServerInteraction::new(&raw, &verifier).expect("the test verifier is valid");
    let challenge = server.generate_challenge();
    state.counter += 1;
    let session = format!("srp-{}", state.counter);
    state.srp.insert(session.clone(), server);
    Ok(json_response(
        200,
        json!({
            "Code": 1000,
            "Modulus": signed_modulus,
            "ServerEphemeral": challenge.encode_b64(),
            "Version": 4,
            "Salt": salt_b64,
            "SRPSession": session,
            "Username": username,
        }),
    ))
}

fn handle_auth(
    body: &[u8],
    state: &Arc<Mutex<State>>,
    headers: &hyper::HeaderMap,
    faults: &Arc<Mutex<Option<Fault>>>,
) -> Result<Response<Full<Bytes>>, io::Error> {
    let value: Value = serde_json::from_slice(body).unwrap_or(json!({}));
    let username = value
        .get("Username")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let srp_session = value
        .get("SRPSession")
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned();
    let client_ephemeral = value.get("ClientEphemeral").and_then(Value::as_str);
    let client_proof = value.get("ClientProof").and_then(Value::as_str);
    let (client_ephemeral, client_proof) = match (client_ephemeral, client_proof) {
        (Some(ephemeral), Some(proof)) => (ephemeral, proof),
        _ => {
            return Ok(json_response(
                422,
                json!({"Code": 2001, "Error": "InvalidValue"}),
            ));
        }
    };

    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    let Some(account) = state.accounts.get(&username) else {
        return Ok(json_response(
            400,
            json!({"Code": 6003, "Error": "UsernameInvalid"}),
        ));
    };
    let totp = account.totp.clone();
    let password_mode = account.password_mode;
    let hv_required = account.hv_required;
    let fido2_only = account.fido2_only;

    // Human verification is checked before the SRP session is consumed,
    // so the retry with the verification token reuses the same session.
    let hv_token = headers
        .get("x-pm-human-verification-token")
        .and_then(|value| value.to_str().ok());
    if hv_required {
        let Some(token) = hv_token else {
            return Ok(human_verification_response(&mut state));
        };
        if !state.hv_tokens.contains(token) {
            return Ok(json_response(
                422,
                json!({"Code": 12087, "Error": "CAPTCHA validation failed"}),
            ));
        }
    }

    let proof = {
        let Some(server) = state.srp.get_mut(&srp_session) else {
            return Ok(json_response(
                422,
                json!({"Code": 2001, "Error": "InvalidValue"}),
            ));
        };
        match server.verify_proof(
            &ServerClientProof::new(client_ephemeral, client_proof)
                .map_err(|_| io::Error::new(io::ErrorKind::InvalidData, "bad proof encoding"))?,
        ) {
            Ok(proof) => proof,
            Err(_) => {
                return Ok(json_response(
                    422,
                    json!({"Code": 8002, "Error": "PasswordWrong"}),
                ));
            }
        }
    };
    state.srp.remove(&srp_session);

    state.counter += 1;
    let uid = format!("uid-{}", state.counter);
    let access = format!("access-{}", state.counter);
    let refresh = format!("refresh-{}", state.counter);
    // `2FA.Enabled` is a bitmask: 1 = TOTP, 2 = FIDO2, 3 = both.
    let two_fa = u8::from(totp.is_some()) | (u8::from(fido2_only) << 1);
    let bad_proof = matches!(
        *faults.lock().unwrap_or_else(|e| e.into_inner()),
        Some(Fault::BadServerProof)
    );
    let server_proof = if bad_proof {
        base64::engine::general_purpose::STANDARD.encode([0u8; 256])
    } else {
        proof.encode_b64()
    };
    state.sessions.insert(
        uid.clone(),
        Session {
            user_id: username.clone(),
            access_token: access.clone(),
            refresh_token: refresh.clone(),
            password_mode,
        },
    );
    Ok(json_response(
        200,
        json!({
            "Code": 1000,
            "UID": uid,
            "AccessToken": access,
            "RefreshToken": refresh,
            "ServerProof": server_proof,
            "Scope": "full",
            "2FA": {"Enabled": two_fa},
            "PasswordMode": password_mode,
            "UserID": format!("user-{username}"),
        }),
    ))
}

fn handle_2fa(
    body: &[u8],
    state: &Arc<Mutex<State>>,
    headers: &hyper::HeaderMap,
) -> Result<Response<Full<Bytes>>, io::Error> {
    let uid = header_str(headers, "x-pm-uid").unwrap_or_default();
    let bearer = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or_default()
        .to_owned();
    let value: Value = serde_json::from_slice(body).unwrap_or(json!({}));
    let code = value
        .get("TwoFactorCode")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    let (access_ok, user_id, refresh_token, password_mode) = {
        let Some(session) = state.sessions.get(&uid) else {
            return Ok(json_response(
                401,
                json!({"Code": 401, "Error": "Invalid access token"}),
            ));
        };
        (
            session.access_token == bearer,
            session.user_id.clone(),
            session.refresh_token.clone(),
            session.password_mode,
        )
    };
    if !access_ok {
        return Ok(json_response(
            401,
            json!({"Code": 401, "Error": "Invalid access token"}),
        ));
    }
    // Human verification can also be raised by the 2FA step; the retry
    // must carry the verification headers on `/auth/v4/2fa`.
    let hv_required = state
        .accounts
        .get(&user_id)
        .map(|account| account.hv_required)
        .unwrap_or(false);
    let hv_token = headers
        .get("x-pm-human-verification-token")
        .and_then(|value| value.to_str().ok());
    if hv_required {
        let Some(token) = hv_token else {
            return Ok(human_verification_response(&mut state));
        };
        if !state.hv_tokens.contains(token) {
            return Ok(json_response(
                422,
                json!({"Code": 12087, "Error": "CAPTCHA validation failed"}),
            ));
        }
    }
    let expected = state
        .accounts
        .get(&user_id)
        .and_then(|account| account.totp.clone());
    if expected.as_deref() != Some(code) {
        return Ok(json_response(
            422,
            json!({"Code": 8002, "Error": "PasswordWrong"}),
        ));
    }
    state.counter += 1;
    let access = format!("access-{}", state.counter);
    if let Some(session) = state.sessions.get_mut(&uid) {
        session.access_token = access.clone();
    }
    Ok(json_response(
        200,
        json!({
            "Code": 1000,
            "UID": uid,
            "AccessToken": access,
            "RefreshToken": refresh_token,
            "Scope": "full",
            "2FA": {"Enabled": 0},
            "PasswordMode": password_mode,
            "UserID": user_id,
        }),
    ))
}

fn handle_refresh(
    body: &[u8],
    state: &Arc<Mutex<State>>,
) -> Result<Response<Full<Bytes>>, io::Error> {
    let value: Value = serde_json::from_slice(body).unwrap_or(json!({}));
    let uid = value.get("UID").and_then(Value::as_str).unwrap_or_default();
    let refresh = value
        .get("RefreshToken")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    let (refresh_ok, user_id, password_mode) = {
        let Some(session) = state.sessions.get(uid) else {
            return Ok(json_response(
                422,
                json!({"Code": 10013, "Error": "Invalid refresh token"}),
            ));
        };
        (
            session.refresh_token == refresh,
            session.user_id.clone(),
            session.password_mode,
        )
    };
    if !refresh_ok {
        return Ok(json_response(
            422,
            json!({"Code": 10013, "Error": "Invalid refresh token"}),
        ));
    }
    state.counter += 1;
    let access = format!("access-{}", state.counter);
    let refresh = format!("refresh-{}", state.counter);
    if let Some(session) = state.sessions.get_mut(uid) {
        session.access_token = access.clone();
        session.refresh_token = refresh.clone();
    }
    Ok(json_response(
        200,
        json!({
            "Code": 1000,
            "UID": uid,
            "AccessToken": access,
            "RefreshToken": refresh,
            "Scope": "full",
            "2FA": {"Enabled": 0},
            "PasswordMode": password_mode,
            "UserID": user_id,
        }),
    ))
}

fn handle_logout(
    state: &Arc<Mutex<State>>,
    headers: &hyper::HeaderMap,
) -> Result<Response<Full<Bytes>>, io::Error> {
    let uid = header_str(headers, "x-pm-uid").unwrap_or_default();
    let bearer = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))
        .unwrap_or_default()
        .to_owned();
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    if let Some(session) = state.sessions.get(&uid)
        && session.access_token == bearer
    {
        state.sessions.remove(&uid);
    }
    Ok(json_response(200, json!({"Code": 1000})))
}

/// The session's username, or `None` when the headers don't match a
/// live session (the fake's authenticated routes).
fn authenticated(state: &mut State, headers: &hyper::HeaderMap) -> Option<String> {
    let uid = header_str(headers, "x-pm-uid")?;
    let bearer = headers
        .get("authorization")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.strip_prefix("Bearer "))?;
    let session = state.sessions.get(&uid)?;
    (session.access_token == bearer).then(|| session.user_id.clone())
}

fn unauthorized() -> Response<Full<Bytes>> {
    json_response(401, json!({"Code": 401, "Error": "Invalid access token"}))
}

fn handle_salts(
    state: &Arc<Mutex<State>>,
    headers: &hyper::HeaderMap,
) -> Result<Response<Full<Bytes>>, io::Error> {
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    let Some(username) = authenticated(&mut state, headers) else {
        return Ok(unauthorized());
    };
    let account = state
        .accounts
        .get(&username)
        .expect("the session's account exists");
    let salts: Vec<Value> = account
        .key_salts
        .iter()
        .map(|salt| {
            json!({
                "ID": salt.id.0,
                "KeySalt": salt.key_salt.as_ref().map(|value| value.0.clone()),
            })
        })
        .collect();
    Ok(json_response(200, json!({"Code": 1000, "KeySalts": salts})))
}

fn handle_user(
    state: &Arc<Mutex<State>>,
    headers: &hyper::HeaderMap,
) -> Result<Response<Full<Bytes>>, io::Error> {
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    let Some(username) = authenticated(&mut state, headers) else {
        return Ok(unauthorized());
    };
    let account = state
        .accounts
        .get(&username)
        .expect("the session's account exists");
    let keys: Vec<Value> = account
        .user_keys
        .0
        .iter()
        .map(|key| serde_json::to_value(key).expect("a locked key serializes"))
        .collect();
    Ok(json_response(
        200,
        json!({
            "Code": 1000,
            "User": {
                "ID": format!("user-{username}"),
                "Name": username,
                "DisplayName": username,
                "Email": account.addresses.first().map(|a| a.email.clone()).unwrap_or_default(),
                "Keys": keys,
            }
        }),
    ))
}

fn handle_addresses(
    state: &Arc<Mutex<State>>,
    headers: &hyper::HeaderMap,
) -> Result<Response<Full<Bytes>>, io::Error> {
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    let Some(username) = authenticated(&mut state, headers) else {
        return Ok(unauthorized());
    };
    let account = state
        .accounts
        .get(&username)
        .expect("the session's account exists");
    let addresses: Vec<Value> = account
        .addresses
        .iter()
        .map(|address| {
            let keys: Vec<Value> = address
                .keys
                .iter()
                .map(|key| serde_json::to_value(key).expect("a locked key serializes"))
                .collect();
            json!({
                "ID": address.id,
                "Email": address.email,
                "DisplayName": address.display_name,
                // Production's `0`/`1` booleans (`go-proton-api` `Bool`).
                "Send": u8::from(address.send),
                "Receive": u8::from(address.receive),
                "Status": address.status,
                "Order": address.order,
                "Keys": keys,
            })
        })
        .collect();
    Ok(json_response(
        200,
        json!({"Code": 1000, "Addresses": addresses}),
    ))
}

/// The system labels every account starts with (research
/// `mail-sync.md` §1).
fn system_labels() -> Vec<FakeLabel> {
    [
        ("0", "Inbox"),
        ("1", "AllDrafts"),
        ("2", "AllSent"),
        ("3", "Trash"),
        ("4", "Spam"),
        ("5", "All Mail"),
        ("6", "Archive"),
        ("7", "Sent"),
        ("8", "Drafts"),
        ("9", "Outbox"),
        ("10", "Starred"),
        ("12", "Scheduled"),
    ]
    .into_iter()
    .map(|(id, name)| FakeLabel::new(id, 4, name))
    .collect()
}

fn account_mut<'a>(state: &'a mut State, username: &str) -> &'a mut Account {
    state
        .accounts
        .get_mut(username)
        .expect("the account exists")
}

fn event_id(number: u64) -> String {
    format!("evt-{number}")
}

/// Every value of a query parameter, in order. Proton's query values
/// are plain (`Page`, `PageSize`, `Type`, `Desc`), so no form decoding
/// is needed.
fn query_values(query: Option<&str>, name: &str) -> Vec<String> {
    let Some(query) = query else {
        return Vec::new();
    };
    query
        .split('&')
        .filter_map(|pair| pair.split_once('='))
        .filter(|(key, _)| *key == name)
        .map(|(_, value)| value.to_owned())
        .collect()
}

fn query_value(query: Option<&str>, name: &str) -> Option<String> {
    query_values(query, name).into_iter().next()
}

/// Adds an optional field to an event's JSON object.
fn with_optional(mut value: Value, key: &str, field: Option<Value>) -> Value {
    if let Some(field) = field {
        value[key] = field;
    }
    value
}

fn handle_labels(
    state: &Arc<Mutex<State>>,
    headers: &hyper::HeaderMap,
    query: Option<&str>,
) -> Result<Response<Full<Bytes>>, io::Error> {
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    let Some(username) = authenticated(&mut state, headers) else {
        return Ok(unauthorized());
    };
    let account = state
        .accounts
        .get(&username)
        .expect("the session's account exists");
    let types: HashSet<u8> = query_values(query, "Type")
        .iter()
        .filter_map(|value| value.parse().ok())
        .collect();
    let labels: Vec<Value> = account
        .labels
        .iter()
        .filter(|label| types.is_empty() || types.contains(&label.kind))
        .map(|label| label.to_json())
        .collect();
    Ok(json_response(200, json!({"Code": 1000, "Labels": labels})))
}

fn handle_messages(
    state: &Arc<Mutex<State>>,
    headers: &hyper::HeaderMap,
    query: Option<&str>,
) -> Result<Response<Full<Bytes>>, io::Error> {
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    let Some(username) = authenticated(&mut state, headers) else {
        return Ok(unauthorized());
    };
    let page: usize = query_value(query, "Page")
        .and_then(|v| v.parse().ok())
        .unwrap_or(0);
    let page_size: usize = query_value(query, "PageSize")
        .and_then(|v| v.parse::<u32>().ok())
        .unwrap_or(100)
        .min(state.max_page_size) as usize;
    let desc = query_value(query, "Desc")
        .map(|value| value != "0")
        .unwrap_or(true);
    let end: Option<i64> = query_value(query, "End").and_then(|value| value.parse().ok());
    let end_id: Option<String> = query_value(query, "EndID");
    let id_filter = query_value(query, "ID");
    let stale = state.stale;
    let account = state
        .accounts
        .get(&username)
        .expect("the session's account exists");
    let mut ordered: Vec<&FakeMessage> = account.messages.iter().collect();
    // The by-id read (research `mail-sync.md` §3): only the asked
    // message, or none.
    if let Some(id) = &id_filter {
        ordered
            .retain(|message| &message.id == id && !account.metadata_hidden.contains(&message.id));
    }
    ordered.sort_by(|a, b| b.time.cmp(&a.time).then_with(|| b.id.cmp(&a.id)));
    if !desc {
        ordered.reverse();
    }
    // Keyset paging (the web indexer's `End`/`EndID`): drop everything
    // at or below the anchor before `Page` skips, and count what
    // remains as Proton's `Total` for the remaining range.
    if let Some(end) = end {
        let end_id = end_id.as_deref();
        ordered.retain(|message| {
            message.time < end
                || (message.time == end && end_id.is_none_or(|end_id| message.id.as_str() < end_id))
        });
    }
    let total = u32::try_from(ordered.len()).unwrap_or(u32::MAX);
    // A probe's huge `Page` is an empty page, not an overflow.
    let start = page.saturating_mul(page_size);
    let messages: Vec<Value> = ordered
        .into_iter()
        .skip(start)
        .take(page_size)
        .map(|message| {
            let mut value = message.to_json();
            if account.malformed_messages {
                value["ID"] = json!(7);
            }
            // The by-id read's changed labels (plan M2.16d): metadata
            // that moved since the listing.
            if id_filter.is_some()
                && let Some(labels) = account.metadata_answer_labels.get(&message.id)
            {
                value["LabelIDs"] = json!(labels);
            }
            // The by-id read's wrong-id answer (plan M2.16d): a
            // well-typed id that differs from the asked one.
            if let (Some(asked), true) = (&id_filter, account.wrong_metadata_id)
                && message.id == *asked
            {
                value["ID"] = json!(
                    account
                        .messages
                        .iter()
                        .find(|other| other.id != message.id)
                        .map(|other| other.id.clone())
                        .unwrap_or_else(|| format!("{asked}-other"))
                );
            }
            value
        })
        .collect();
    Ok(json_response(
        200,
        json!({
            "Code": 1000,
            "Messages": messages,
            "Total": total,
            "Stale": stale,
        }),
    ))
}

fn handle_message_counts(
    state: &Arc<Mutex<State>>,
    headers: &hyper::HeaderMap,
) -> Result<Response<Full<Bytes>>, io::Error> {
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    let Some(username) = authenticated(&mut state, headers) else {
        return Ok(unauthorized());
    };
    let account = state
        .accounts
        .get(&username)
        .expect("the session's account exists");
    let counts: Vec<Value> = account
        .labels
        .iter()
        .map(|label| {
            let (total, unread) = account
                .messages
                .iter()
                .filter(|message| message.label_ids.contains(&label.id))
                .fold((0u32, 0u32), |(total, unread), message| {
                    (total + 1, unread + u32::from(message.unread))
                });
            json!({
                "LabelID": label.id,
                "Total": total,
                "Unread": unread,
            })
        })
        .collect();
    Ok(json_response(200, json!({"Code": 1000, "Counts": counts})))
}

/// `GET /mail/v4/messages/{id}`: the full message (research
/// `mail-sync.md` §4). An unknown id is the fake's not-found error.
fn handle_message(
    state: &Arc<Mutex<State>>,
    headers: &hyper::HeaderMap,
    id: &str,
) -> Result<Response<Full<Bytes>>, io::Error> {
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    let Some(username) = authenticated(&mut state, headers) else {
        return Ok(unauthorized());
    };
    let account = state
        .accounts
        .get(&username)
        .expect("the session's account exists");
    let Some(stored) = account.messages.iter().find(|message| message.id == id) else {
        return Ok(json_response(
            404,
            json!({"Code": 2001, "Error": "message not found"}),
        ));
    };
    let mut value = message_body_json(stored);
    if account.damaged_bodies.contains(id) {
        value["Body"] = json!(&account.damaged_body);
    }
    if account.wrong_message_id {
        let other = account
            .messages
            .iter()
            .find(|message| message.id != id)
            .map(|message| message.id.clone())
            .unwrap_or_else(|| format!("{id}-other"));
        value["ID"] = json!(other);
    }
    Ok(json_response(200, json!({"Code": 1000, "Message": value})))
}

/// `GET /mail/v4/attachments/{id}`: the raw data packet, not JSON
/// (research `mail-sync.md` §4).
fn handle_attachment(
    state: &Arc<Mutex<State>>,
    headers: &hyper::HeaderMap,
    id: &str,
) -> Result<Response<Full<Bytes>>, io::Error> {
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    let Some(username) = authenticated(&mut state, headers) else {
        return Ok(unauthorized());
    };
    let account = state
        .accounts
        .get(&username)
        .expect("the session's account exists");
    let Some((_, _, data_packet)) = account
        .messages
        .iter()
        .flat_map(|message| message.encrypted_attachments.iter())
        .find(|(attachment_id, _, _)| attachment_id == id)
    else {
        return Ok(json_response(
            404,
            json!({"Code": 2001, "Error": "attachment not found"}),
        ));
    };
    Ok(Response::builder()
        .status(StatusCode::OK)
        .header("content-type", "application/octet-stream")
        .body(Full::new(Bytes::from(data_packet.clone())))
        .expect("a raw response builds"))
}

fn handle_latest_event(
    state: &Arc<Mutex<State>>,
    headers: &hyper::HeaderMap,
) -> Result<Response<Full<Bytes>>, io::Error> {
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    let Some(username) = authenticated(&mut state, headers) else {
        return Ok(unauthorized());
    };
    let account = state
        .accounts
        .get(&username)
        .expect("the session's account exists");
    let id = account
        .events
        .last()
        .map(|event| event_id(event.number))
        .unwrap_or_else(|| "0".to_owned());
    Ok(json_response(200, json!({"Code": 1000, "EventID": id})))
}

fn handle_events(
    state: &Arc<Mutex<State>>,
    headers: &hyper::HeaderMap,
    id: &str,
) -> Result<Response<Full<Bytes>>, io::Error> {
    let mut state = state.lock().unwrap_or_else(|e| e.into_inner());
    let Some(username) = authenticated(&mut state, headers) else {
        return Ok(unauthorized());
    };
    let after = id
        .strip_prefix("evt-")
        .and_then(|number| number.parse::<u64>().ok())
        .unwrap_or(0);
    let max = state.max_events_per_response;
    let account = state
        .accounts
        .get(&username)
        .expect("the session's account exists");
    let events: Vec<&FakeEvent> = account
        .events
        .iter()
        .filter(|event| event.number > after)
        .collect();
    let (batch, more) = if max > 0 && events.len() > max {
        (&events[..max], true)
    } else {
        (&events[..], false)
    };
    let event_id = batch
        .last()
        .map(|event| event_id(event.number))
        .unwrap_or_else(|| id.to_owned());
    let refresh = batch.iter().any(|event| event.refresh);
    let user = batch.iter().find_map(|event| event.user.clone());
    let messages: Vec<Value> = batch
        .iter()
        .flat_map(|event| event.messages.iter().cloned())
        .collect();
    let labels: Vec<Value> = batch
        .iter()
        .flat_map(|event| event.labels.iter().cloned())
        .collect();
    let addresses: Vec<Value> = batch
        .iter()
        .flat_map(|event| event.addresses.iter().cloned())
        .collect();
    let mut response = json!({
        "Code": 1000,
        "EventID": event_id,
        "More": more,
        "Refresh": if refresh { 1 } else { 0 },
        "Messages": messages,
        "Labels": labels,
        "Addresses": addresses,
    });
    if let Some(user) = user {
        response["User"] = user;
    }
    Ok(json_response(200, response))
}

/// Exports a public key armored, as `GET /core/v4/keys` serves it.
fn public_key_armor<P: PGPProviderSync>(provider: &P, key: &P::PublicKey) -> String {
    let armored = provider
        .public_key_export(key, DataEncoding::Armor)
        .expect("the public key exports");
    String::from_utf8(armored.as_ref().to_vec()).expect("the armored public key is ASCII")
}

/// The full-message JSON shared by a read and a send reply: metadata
/// plus header, body, MIME type and attachments (research
/// `mail-sync.md` §4).
fn message_body_json(stored: &FakeMessage) -> Value {
    let mut value = stored.to_json();
    value["Header"] = json!(&stored.header);
    value["Body"] = json!(
        stored
            .encrypted_body
            .as_ref()
            .expect("the body is encrypted")
    );
    value["MIMEType"] = json!(&stored.mime_type);
    value["Attachments"] = Value::Array(
        stored
            .attachments
            .iter()
            .zip(&stored.encrypted_attachments)
            .map(|(a, (id, key_packets, _))| attachment_json(id, a, key_packets))
            .collect(),
    );
    value
}

/// The per-account encryptors: an armored, signed message body and a
/// split attachment (key packets plus data packet), both to the primary
/// address key.
fn encryptors<P>(
    provider: &P,
    key: &DecryptedAddressKey<P::PrivateKey, P::PublicKey>,
) -> (EncryptBody, EncryptAttachment)
where
    P: PGPProviderSync,
{
    let encrypt_body: EncryptBody = {
        let provider = provider.clone();
        let public = key.public_key.clone();
        let private = key.private_key.clone();
        Arc::new(move |plaintext: &[u8]| {
            let mut armored = Vec::new();
            provider
                .new_encryptor()
                .with_encryption_key(&public)
                .with_signing_key(&private)
                .encrypt_to_writer(
                    plaintext,
                    DataEncoding::Armor,
                    SigningMode::Inline,
                    WritingMode::All,
                    &mut armored,
                )
                .expect("the body encrypts");
            armored
        })
    };
    let encrypt_attachment: EncryptAttachment = {
        let provider = provider.clone();
        let public = key.public_key.clone();
        Arc::new(move |plaintext: &[u8]| {
            let mut data_packet = Vec::new();
            let detached = provider
                .new_encryptor()
                .with_encryption_key(&public)
                .encrypt_to_writer(
                    plaintext,
                    DataEncoding::Bytes,
                    SigningMode::Inline,
                    WritingMode::SplitKeyPackets,
                    &mut data_packet,
                )
                .expect("the attachment encrypts");
            let key_packets = detached
                .key_packets
                .expect("split mode returns the key packets");
            (key_packets, data_packet)
        })
    };
    (encrypt_body, encrypt_attachment)
}

/// Builds the `LockedKey` the API stores for a generated user key.
fn user_locked_key(id: &str, local: LocalUserKey) -> LockedKey {
    LockedKey {
        id: KeyId::from(id.to_owned()),
        version: 4,
        private_key: local.private_key,
        token: None,
        signature: None,
        activation: None,
        primary: true,
        active: true,
        flags: None,
        recovery_secret: None,
        recovery_secret_signature: None,
        address_forwarding_id: None,
    }
}

/// Builds the `LockedKey` the API stores for a generated address key.
fn address_locked_key(id: &str, local: LocalAddressKey) -> LockedKey {
    LockedKey {
        id: KeyId::from(id.to_owned()),
        version: 4,
        private_key: local.private_key,
        token: local.token,
        signature: local.signature,
        activation: None,
        primary: local.primary,
        active: true,
        flags: Some(local.flags),
        recovery_secret: None,
        recovery_secret_signature: None,
        address_forwarding_id: None,
    }
}

/// Re-locks a generated address key with a passphrase that matches
/// neither its token nor the key secret, so unlocking it fails while
/// the rest of the account stays valid.
fn mislock_key<P: PGPProviderSync>(
    provider: &P,
    key: &mut LockedKey,
    unlocked: &DecryptedAddressKey<P::PrivateKey, P::PublicKey>,
) {
    let wrong = provider
        .private_key_export(
            &unlocked.private_key,
            b"sweetbriar-wrong-passphrase",
            DataEncoding::Armor,
        )
        .expect("the key exports");
    key.private_key = ArmoredPrivateKey(
        String::from_utf8(wrong.as_ref().to_vec()).expect("the armored key is ASCII"),
    );
}

/// Flips a run of characters in the middle of an armored message,
/// which is enough to make the ciphertext undecodeable.
fn damage_armor(armored: &str) -> String {
    let mut chars: Vec<char> = armored.chars().collect();
    let middle = chars.len() / 2;
    for offset in 0..8 {
        let index = (middle + offset).min(chars.len() - 1);
        chars[index] = if chars[index] == 'A' { 'B' } else { 'A' };
    }
    chars.into_iter().collect()
}

/// Issues a human-verification token and answers 9001 with Proton's
/// verification details (research `auth-session.md` §5).
fn human_verification_response(state: &mut State) -> Response<Full<Bytes>> {
    state.counter += 1;
    let token = format!("hv-token-{}", state.counter);
    state.hv_tokens.insert(token.clone());
    json_response(
        422,
        json!({
            "Code": 9001,
            "Error": "Human verification required",
            "Details": {
                "HumanVerificationMethods": ["captcha"],
                "HumanVerificationToken": token,
                "WebUrl": format!("https://verify.proton.me/?methods=captcha&token={token}"),
            }
        }),
    )
}

fn json_bytes(value: Value) -> Full<Bytes> {
    Full::new(Bytes::from(value.to_string()))
}

fn json_response(status: u16, value: Value) -> Response<Full<Bytes>> {
    Response::builder()
        .status(StatusCode::from_u16(status).unwrap_or(StatusCode::INTERNAL_SERVER_ERROR))
        .header("content-type", "application/json")
        .body(json_bytes(value))
        .expect("a JSON response builds")
}

/// Generates the fake's test identity: an Ed25519 OpenPGP key and a
/// cleartext-signed copy of the test modulus. Synthetic, generated per
/// server; no live Proton key.
fn test_identity() -> (String, SignedPublicKey) {
    let mut rng = rand::thread_rng();
    let mut signkey = SubkeyParamsBuilder::default();
    signkey
        .key_type(KeyType::Ed25519Legacy)
        .can_sign(true)
        .can_encrypt(EncryptionCaps::None)
        .can_authenticate(false);
    let mut key_params = SecretKeyParamsBuilder::default();
    key_params
        .key_type(KeyType::Ed25519Legacy)
        .can_certify(true)
        .can_sign(false)
        .can_encrypt(EncryptionCaps::None)
        .primary_user_id("sweetbriar-proton-test@localhost".into())
        .subkeys(vec![signkey.build().expect("the signing subkey builds")]);
    let params = key_params.build().expect("the test key parameters build");
    let key = params.generate(&mut rng).expect("the test key generates");
    let public = SignedPublicKey::from(key.clone());
    let signed = CleartextSignedMessage::sign(&mut rng, MODULUS_B64, &*key, &Password::empty())
        .expect("the test modulus signs");
    let armored = signed
        .to_armored_string(ArmorOptions::default())
        .expect("the signed modulus armors");
    (armored, public)
}

/// Builds the fake's modulus verifier from the armored test public key
/// alone. A crash-cut child in another process uses it to trust the
/// parent process's fake server (spec/11).
pub fn fake_modulus_verifier(armored: &str) -> Arc<dyn ModulusSignatureVerifier + Send + Sync> {
    let (public, _) = SignedPublicKey::from_string(armored).expect("the fake's public key parses");
    Arc::new(TestVerifier(public))
}

/// Verifies the fake's signed modulus with the fake's test key.
struct TestVerifier(SignedPublicKey);

impl ModulusSignatureVerifier for TestVerifier {
    fn verify_and_extract_modulus(
        &self,
        modulus: &str,
        _server_key: &str,
    ) -> Result<String, ModulusVerifyError> {
        let (message, _) = CleartextSignedMessage::from_string(modulus)
            .map_err(|e| ModulusVerifyError::CleartextParse(e.to_string()))?;
        message
            .verify(&self.0)
            .map_err(|e| ModulusVerifyError::SignatureVerification(e.to_string()))?;
        Ok(message.text().to_owned())
    }
}
