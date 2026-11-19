//! M4.3g: the send suite on the `go-proton-api` lane (spec/05
//! "Proton submission", spec/11 "The fake Proton API server: the
//! lane").
//!
//! Drives `sb-proton`'s submission transport against Proton's own
//! development server the way that crate's fake-server tests do: a
//! recipient key lookup, a draft with an encrypted body, an attachment
//! upload, packages and a send. The driver's send log supplies each
//! call's HTTP status, Proton's `Code`/`Error` and whether the
//! recipient's inbox grew, so a refusal that sent nothing is told
//! apart from one that delivered. Those records are the evidence for
//! spec/05's qualified pre-submission list.

use crate::{BeginProtonSignIn, Core, ProtonSignInParams};

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use sb_engine::{
    AttachmentUpload, DecryptedAddressKey, DraftAddress, DraftCreate, DraftTemplate, KeyFlag,
    KeyGenerator, KeyGeneratorSync, KeyId, PGPProviderSync, ProtonClient, ProtonPGP, RecipientKeys,
    SendAttachment, SendBody, SendRecipient, SendTransportError, Session, UnlockedAddressKey,
    build_packages, encrypt_attachment, encrypt_draft_body,
};

/// How long to wait for the driver's send-log record of a call. The
/// proxy writes the record before it answers, so this is only a
/// safeguard against a missing record.
const RECORD_TIMEOUT: Duration = Duration::from_secs(10);
/// An external address the development server has no keys for, so the
/// pilot's clear scheme applies.
const EXTERNAL: &str = "stranger@example.test";
/// A recipient key packet that is not base64, so the server refuses
/// the package. (The transport base64-encodes `SendPackage::body`
/// itself, so the body can't carry an invalid value; the key packet is
/// already base64 text on the wire.)
const NOT_BASE64: &str = "not base64";
/// Proton's `MessageFlagSent` bit (`go-proton-api/message_types.go`).
const MESSAGE_FLAG_SENT: u32 = 1 << 1;

/// One record from the driver's send log.
#[derive(Debug, Clone, Deserialize)]
struct DriverRecord {
    sequence: u64,
    method: String,
    path: String,
    status: u16,
    code: Option<i64>,
    error: String,
    inbox: i64,
    sent: bool,
    #[allow(dead_code)]
    read_back: String,
}

/// One provoked call, as the lane records it for spec/05's qualified
/// pre-submission evidence.
#[derive(Debug, Serialize)]
pub(super) struct SendRecord {
    pub case: String,
    pub method: String,
    pub path: String,
    pub http_status: u16,
    pub code: Option<i64>,
    pub error: String,
    /// The transport's own outcome: `ok`, or the error's debug form.
    pub transport: String,
    /// Whether the recipient's inbox grew, from the driver's read-back.
    pub sent: bool,
    pub inbox: i64,
    /// For a send against a real draft: whether the draft carries
    /// Proton's Sent flag afterwards.
    pub draft_sent: Option<bool>,
    pub note: String,
}

/// The internal recipient round trip's observations.
#[derive(Debug, Serialize)]
pub(super) struct RoundTrip {
    pub recipient_type: u8,
    pub recipient_keys: usize,
    pub draft_id: String,
    pub attachment_id: String,
    pub attachment_size: i64,
    pub sent_id: String,
    pub delivered: bool,
    pub inbox: i64,
    pub statuses: Vec<u16>,
}

/// The external recipient without keys (clear scheme).
#[derive(Debug, Serialize)]
pub(super) struct ExternalClear {
    pub recipient_type: u8,
    pub recipient_keys: usize,
    pub draft_id: String,
    pub sent_id: String,
    pub http_status: u16,
    pub note: String,
}

/// The whole send suite's report, written into the lane artifact's
/// `send` section.
#[derive(Debug, Serialize)]
pub(super) struct SendSuite {
    pub sender: String,
    pub internal_recipient: String,
    pub round_trip: RoundTrip,
    pub external_clear: ExternalClear,
    pub refusals: Vec<SendRecord>,
}

/// The lane-line fields the send suite needs, copied so the suite
/// doesn't name the read-only suite's private line type.
#[derive(Debug)]
pub(super) struct SendLaneParams {
    pub send_username: String,
    pub password: String,
    pub send_email: String,
    pub peer_email: String,
    pub send_log: String,
}

/// The sender's transport, provider and generated key, grouped so the
/// draft and key-lookup helpers don't carry them argument by argument.
struct Sender<'a, P: PGPProviderSync> {
    client: &'a ProtonClient,
    session: &'a Session,
    provider: &'a P,
    key: &'a UnlockedAddressKey<P>,
}

impl<P: PGPProviderSync> Sender<'_, P> {
    /// Creates a draft whose body is encrypted to the sender's key
    /// (spec/05 step 2) and returns its id.
    async fn create_draft(
        &self,
        sender_email: &str,
        to: &[DraftAddress],
        subject: &str,
        body: &[u8],
    ) -> Result<String, SendTransportError> {
        let armored =
            encrypt_draft_body(self.provider, self.key, body).expect("the draft body encrypts");
        let template = DraftTemplate {
            sender: DraftAddress {
                name: "Lane Sender".to_owned(),
                address: sender_email.to_owned(),
            },
            to: to.to_vec(),
            cc: Vec::new(),
            bcc: Vec::new(),
            subject: subject.to_owned(),
            mime_type: "text/plain".to_owned(),
            body: String::from_utf8(armored).expect("the armored body is ASCII"),
        };
        let draft = self
            .client
            .create_draft(
                self.session,
                &DraftCreate {
                    template,
                    parent_id: None,
                    action: None,
                },
            )
            .await?;
        Ok(draft.id)
    }

    /// `GET /core/v4/keys?Email=` for a recipient.
    async fn recipient_keys(&self, email: &str) -> RecipientKeys {
        self.client
            .recipient_keys(self.session, email)
            .await
            .expect("the recipient key lookup")
    }
}

/// Runs the send suite against the lane and returns its report. The
/// caller has already asserted the read-only suite, so this suite's
/// mutations don't disturb it.
pub(super) async fn run_send_suite(core: &Core, lane: &SendLaneParams) -> SendSuite {
    let sender_account = sign_in_sender(core, lane).await;
    let (client, session) = core
        .engine()
        .proton_lane_transport(&sender_account)
        .await
        .expect("the send-suite account has a session");
    let provider = ProtonPGP::new_rust_sync();
    let sender_key = generate_sender_key(&provider, &lane.send_email);
    let sender = Sender {
        client: &client,
        session: &session,
        provider: &provider,
        key: &sender_key,
    };
    let mut log = SendLog::new(&lane.send_log);

    // The internal recipient's keys: an internal address is encrypted
    // to them (spec/05 step 1).
    let peer_keys = sender.recipient_keys(&lane.peer_email).await;
    assert!(
        peer_keys.is_internal(),
        "{} is not an internal recipient: {peer_keys:?}",
        lane.peer_email
    );
    let peer_key_count = peer_keys.keys.len();
    assert_eq!(peer_key_count, 1, "the recipient has one active key");
    let lookup_status = log.next_record().await.status;

    // Step 2: the draft, its body encrypted to the sender's key.
    let draft_id = sender
        .create_draft(
            &lane.send_email,
            &[peer_address(&lane.peer_email)],
            "lane send suite",
            b"lane send suite body",
        )
        .await
        .expect("the draft creates");
    let create_status = log.next_record().await.status;

    // Step 3: an attachment, uploaded to the draft.
    let encrypted = encrypt_attachment(&provider, &sender_key, b"lane attachment bytes")
        .expect("the attachment encrypts");
    let attachment_session_key = encrypted.session_key.clone();
    let uploaded = client
        .upload_attachment(
            &session,
            &draft_id,
            &AttachmentUpload {
                filename: "lane-attachment.txt".to_owned(),
                mime_type: "text/plain".to_owned(),
                encrypted,
                inline: false,
                content_id: None,
            },
        )
        .await
        .expect("the attachment uploads");
    let upload_status = log.next_record().await.status;

    // Step 4: one package for the internal recipient, then the send.
    let packages = build_packages(
        &provider,
        &sender_key,
        &[SendRecipient {
            address: lane.peer_email.clone(),
            keys: peer_keys,
        }],
        &[SendBody {
            mime_type: "text/plain",
            data: b"lane send suite body",
        }],
        &[SendAttachment {
            id: uploaded.id.clone(),
            session_key: attachment_session_key,
        }],
    )
    .expect("the packages build");
    let sent = client
        .send_packages(&session, &draft_id, &packages)
        .await
        .expect("the internal send");
    assert_eq!(sent.id, draft_id, "the sent message's id is the draft id");
    let send_record = log.next_record().await;
    assert!(
        send_record.sent && send_record.inbox == 1,
        "the recipient's inbox grew by one: {send_record:?}"
    );
    let round_trip = RoundTrip {
        recipient_type: 1,
        recipient_keys: peer_key_count,
        draft_id: draft_id.clone(),
        attachment_id: uploaded.id.clone(),
        attachment_size: uploaded.size,
        sent_id: sent.id.clone(),
        delivered: true,
        inbox: send_record.inbox,
        statuses: vec![
            lookup_status,
            create_status,
            upload_status,
            send_record.status,
        ],
    };

    // An external recipient without keys gets the clear scheme; the
    // server accepts it and delivers nothing internally.
    let external_keys = sender.recipient_keys(EXTERNAL).await;
    assert!(
        !external_keys.is_internal() && external_keys.keys.is_empty(),
        "{EXTERNAL} is not an external recipient without keys"
    );
    let external_key_count = external_keys.keys.len();
    log.next_record().await;
    let external_draft = sender
        .create_draft(
            &lane.send_email,
            &[DraftAddress {
                name: "Stranger".to_owned(),
                address: EXTERNAL.to_owned(),
            }],
            "lane external clear",
            b"lane external body",
        )
        .await
        .expect("the external draft creates");
    log.next_record().await;
    let clear_packages = build_packages(
        &provider,
        &sender_key,
        &[SendRecipient {
            address: EXTERNAL.to_owned(),
            keys: external_keys,
        }],
        &[SendBody {
            mime_type: "text/plain",
            data: b"lane external body",
        }],
        &[],
    )
    .expect("the clear packages build");
    let clear_sent = client
        .send_packages(&session, &external_draft, &clear_packages)
        .await
        .expect("the external send is accepted");
    let clear_record = log.next_record().await;
    assert!(
        !clear_record.sent,
        "a clear send delivers nothing internally"
    );
    let external_clear = ExternalClear {
        recipient_type: 2,
        recipient_keys: external_key_count,
        draft_id: external_draft.clone(),
        sent_id: clear_sent.id.clone(),
        http_status: clear_record.status,
        note: format!(
            "the server accepted the clear scheme (status {}, code {:?}) and delivered nothing internally",
            clear_record.status, clear_record.code
        ),
    };

    let mut refusals = Vec::new();

    // A draft whose sender address the server doesn't know: refused
    // before anything is created.
    let unknown_sender = sender
        .create_draft(
            "nobody@proton.local",
            &[peer_address(&lane.peer_email)],
            "lane unknown sender",
            b"lane unknown sender body",
        )
        .await;
    let driver = log.next_record().await;
    assert!(unknown_sender.is_err(), "an unknown sender is refused");
    refusals.push(record_case(
        "unknown_sender_address",
        driver,
        &unknown_sender,
        None,
        "the server refuses an unknown sender address before creating a draft",
    ));

    // A send against a draft id that doesn't exist. The development
    // server panics on the nil message and its recovery answers 500;
    // production's refusal shape is unverified.
    let peer_keys = sender.recipient_keys(&lane.peer_email).await;
    log.next_record().await;
    let packages = build_packages(
        &provider,
        &sender_key,
        &[SendRecipient {
            address: lane.peer_email.clone(),
            keys: peer_keys,
        }],
        &[SendBody {
            mime_type: "text/plain",
            data: b"lane unknown draft body",
        }],
        &[],
    )
    .expect("the packages build");
    let unknown_draft = client
        .send_packages(&session, "missing-draft", &packages)
        .await;
    let driver = log.next_record().await;
    assert!(
        unknown_draft.is_err() && driver.status == 500,
        "the development server answers 500: {unknown_draft:?}"
    );
    refusals.push(record_case(
        "unknown_draft_id",
        driver,
        &unknown_draft,
        None,
        "the development server panics on an unknown draft id; its recovery answers a bare 500 with no Code/Error",
    ));

    // A malformed package: the server marks the draft sent, then
    // refuses the package body. Nothing is delivered, but the draft's
    // Sent flag changed.
    let malformed_draft = sender
        .create_draft(
            &lane.send_email,
            &[peer_address(&lane.peer_email)],
            "lane malformed package",
            b"lane malformed package body",
        )
        .await
        .expect("the malformed-package draft creates");
    log.next_record().await;
    let before = draft_flags(&client, &session, &malformed_draft).await;
    let peer_keys = sender.recipient_keys(&lane.peer_email).await;
    log.next_record().await;
    let mut malformed = build_packages(
        &provider,
        &sender_key,
        &[SendRecipient {
            address: lane.peer_email.clone(),
            keys: peer_keys,
        }],
        &[SendBody {
            mime_type: "text/plain",
            data: b"lane malformed package body",
        }],
        &[],
    )
    .expect("the packages build");
    malformed.packages[0]
        .addresses
        .get_mut(&lane.peer_email)
        .expect("the recipient entry")
        .body_key_packet = Some(NOT_BASE64.to_owned());
    let result = client
        .send_packages(&session, &malformed_draft, &malformed)
        .await;
    let driver = log.next_record().await;
    let after = draft_flags(&client, &session, &malformed_draft).await;
    assert!(result.is_err(), "a malformed package is refused");
    assert!(!driver.sent, "a malformed package delivers nothing");
    assert!(
        after && !before,
        "the server marks the draft sent before validating the package"
    );
    refusals.push(record_case(
        "malformed_package",
        driver,
        &result,
        Some(after),
        "the server refuses a non-base64 recipient key packet after marking the draft sent",
    ));

    // A package set that omits an internal recipient: the server
    // accepts it and silently sends nothing to that recipient.
    let missing_draft = sender
        .create_draft(
            &lane.send_email,
            &[
                peer_address(&lane.peer_email),
                DraftAddress {
                    name: "Stranger".to_owned(),
                    address: EXTERNAL.to_owned(),
                },
            ],
            "lane missing package",
            b"lane missing package body",
        )
        .await
        .expect("the missing-package draft creates");
    log.next_record().await;
    let external_keys = sender.recipient_keys(EXTERNAL).await;
    log.next_record().await;
    let missing = build_packages(
        &provider,
        &sender_key,
        &[SendRecipient {
            address: EXTERNAL.to_owned(),
            keys: external_keys,
        }],
        &[SendBody {
            mime_type: "text/plain",
            data: b"lane missing package body",
        }],
        &[],
    )
    .expect("the packages build");
    let result = client
        .send_packages(&session, &missing_draft, &missing)
        .await;
    let driver = log.next_record().await;
    assert!(
        result.is_ok(),
        "a package set omitting a recipient is accepted"
    );
    assert!(
        !driver.sent,
        "the omitted internal recipient is not sent to"
    );
    refusals.push(record_case(
        "missing_package_for_recipient",
        driver,
        &result,
        None,
        "the server accepts a package set that omits an internal recipient; that recipient is silently not sent to",
    ));

    // Sending the same draft twice: the server does not refuse it and
    // delivers again, which is why Sweetbriar never resends on its own.
    let twice_draft = sender
        .create_draft(
            &lane.send_email,
            &[peer_address(&lane.peer_email)],
            "lane send twice",
            b"lane send twice body",
        )
        .await
        .expect("the send-twice draft creates");
    log.next_record().await;
    let peer_keys = sender.recipient_keys(&lane.peer_email).await;
    log.next_record().await;
    let twice = build_packages(
        &provider,
        &sender_key,
        &[SendRecipient {
            address: lane.peer_email.clone(),
            keys: peer_keys,
        }],
        &[SendBody {
            mime_type: "text/plain",
            data: b"lane send twice body",
        }],
        &[],
    )
    .expect("the packages build");
    let first = client
        .send_packages(&session, &twice_draft, &twice)
        .await
        .expect("the first send");
    let first_driver = log.next_record().await;
    let second = client
        .send_packages(&session, &twice_draft, &twice)
        .await
        .expect("the second send is not refused");
    let second_driver = log.next_record().await;
    assert!(
        first_driver.sent && second_driver.sent,
        "both sends deliver"
    );
    assert_eq!(first.id, second.id, "both sends name the same draft");
    refusals.push(record_case(
        "send_twice",
        second_driver.clone(),
        &Ok(second.id.clone()),
        None,
        &format!(
            "the server does not refuse a second send (first status {}, second status {}); it delivers again",
            first_driver.status, second_driver.status
        ),
    ));

    SendSuite {
        sender: lane.send_email.clone(),
        internal_recipient: lane.peer_email.clone(),
        round_trip,
        external_clear,
        refusals,
    }
}

/// Signs the send suite in as its own synthetic sender, so the
/// read-only suite's account is never mutated.
async fn sign_in_sender(core: &Core, lane: &SendLaneParams) -> String {
    let state = match core
        .begin_proton_sign_in(
            "lane-send",
            ProtonSignInParams {
                display_name: "Lane Send".to_owned(),
                username: lane.send_username.clone(),
                password: lane.password.clone().into_bytes(),
                account_id: None,
            },
        )
        .await
        .expect("the send-suite sign-in begins")
    {
        BeginProtonSignIn::Open { state, .. } => state,
        BeginProtonSignIn::Complete { receipt } => {
            panic!("an unused key replayed a receipt: {receipt:?}")
        }
    };
    super::account_id(&state)
}

/// A generated sender address key, as the fake-server tests use: the
/// development server never reads the draft's sender key back.
fn generate_sender_key<P: PGPProviderSync>(provider: &P, email: &str) -> UnlockedAddressKey<P> {
    let private = provider
        .new_key_generator()
        .with_user_id("Lane Sender", email)
        .generate()
        .expect("the sender key generates");
    let public = provider
        .private_key_to_public_key(&private)
        .expect("the sender public key");
    DecryptedAddressKey {
        id: KeyId::from("lane-sender-key".to_owned()),
        flags: KeyFlag::default(),
        primary: true,
        is_v6: false,
        private_key: private,
        public_key: public,
    }
}

fn peer_address(email: &str) -> DraftAddress {
    DraftAddress {
        name: "Lane Peer".to_owned(),
        address: email.to_owned(),
    }
}

/// The draft's Proton `Flags`, for the before/after of a refusal.
async fn draft_flags(client: &ProtonClient, session: &Session, draft_id: &str) -> bool {
    client
        .message_metadata(session, draft_id)
        .await
        .expect("the draft metadata reads")
        .expect("the draft exists")
        .flags
        & MESSAGE_FLAG_SENT
        != 0
}

/// One recorded case: the driver's observation plus the transport's
/// own outcome.
fn record_case<T>(
    case: &str,
    driver: DriverRecord,
    result: &Result<T, SendTransportError>,
    draft_sent: Option<bool>,
    note: &str,
) -> SendRecord {
    SendRecord {
        case: case.to_owned(),
        method: driver.method,
        path: driver.path,
        http_status: driver.status,
        code: driver.code,
        error: driver.error,
        transport: match result {
            Ok(_) => "ok".to_owned(),
            Err(error) => format!("{error:?}"),
        },
        sent: driver.sent,
        inbox: driver.inbox,
        draft_sent,
        note: note.to_owned(),
    }
}

/// The driver's append-only send log: one JSON record per send-suite
/// call, written before the call's reply reaches the lane.
struct SendLog {
    path: PathBuf,
    next: u64,
}

impl SendLog {
    fn new(path: &str) -> Self {
        assert!(
            !path.is_empty(),
            "the lane needs the driver's -send-log path to read back what was sent"
        );
        Self {
            path: PathBuf::from(path),
            next: 1,
        }
    }

    /// Waits for the record of the next call.
    async fn next_record(&mut self) -> DriverRecord {
        let deadline = tokio::time::Instant::now() + RECORD_TIMEOUT;
        loop {
            if let Some(record) = self.read() {
                self.next += 1;
                return record;
            }
            assert!(
                tokio::time::Instant::now() < deadline,
                "the driver's send log {} has no record {}",
                self.path.display(),
                self.next
            );
            tokio::time::sleep(Duration::from_millis(20)).await;
        }
    }

    /// The record whose sequence is `self.next`, if it is already in
    /// the file.
    fn read(&self) -> Option<DriverRecord> {
        let text = std::fs::read_to_string(&self.path).ok()?;
        text.lines()
            .filter_map(|line| serde_json::from_str::<DriverRecord>(line).ok())
            .find(|record| record.sequence == self.next)
    }
}
