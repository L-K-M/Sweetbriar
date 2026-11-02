//! M2.1 spike evidence, kept: `io-imap` 0.7.1 (spec/01's chosen IMAP
//! client) runs the spec/04 read-only operation sequence against the
//! `sb-test-support` fake Proton Bridge, and against a real Dovecot
//! container when `SB_IMAP_DOVECOT` points at one (spec/11).
//!
//! Malformed responses follow spec/04: close and apply nothing after
//! the fault. io-imap's send loop instead skips an undecodable
//! untagged response with a debug log, so every stream here runs the
//! strict gate: a `Fragmentizer` + `ResponseCodec` adapter releasing
//! only complete, decodable messages and failing at the first
//! undecodable one. Test-local; M2.2 moves it into `sb-imap`.

use std::any::Any;
use std::collections::{BTreeMap, VecDeque};
use std::env;
use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::num::NonZeroU32;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::Duration;

use io_imap::client::{ImapClient, ImapClientError, ImapClientStd, ImapStream};
use io_imap::codec::{ResponseCodec, fragmentizer::Fragmentizer};
use io_imap::coroutine::{ImapCoroutine, ImapCoroutineState};
use io_imap::rfc2177::idle::{ImapIdle, ImapIdleOptions, ImapIdleYield};
use io_imap::rfc2971::id::ImapServerIdOptions;
use io_imap::rfc3501::append::ImapMessageAppendOptions;
use io_imap::rfc3501::copy::ImapMessageCopyOptions;
use io_imap::rfc3501::examine::ImapMailboxExamineOptions;
use io_imap::rfc3501::fetch::ImapMessageFetchOptions;
use io_imap::rfc3501::login::ImapLoginOptions;
use io_imap::rfc3501::logout::ImapLogoutError;
use io_imap::types::core::Vec1;
use io_imap::types::fetch::{MessageDataItem, MessageDataItemName, Section};
use io_imap::types::flag::{Flag, FlagFetch};
use io_imap::types::mailbox::{ListMailbox, Mailbox};
use io_imap::types::sequence::SequenceSet;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::{WebPkiSupportedAlgorithms, verify_tls12_signature, verify_tls13_signature};
use rustls::{
    CertificateError, ClientConfig, ClientConnection, DigitallySignedStruct, RootCertStore,
    SignatureScheme,
    pki_types::{CertificateDer, ServerName, UnixTime},
    server::ParsedCertificate,
};
use sb_test_support::{
    Expect, FakeImapServer, Profile, RAW_UTF8_ENVELOPE, Reply, Script, Step, TlsIo, cert_der,
    client_stream_with,
};

/// Read budget for one exchange against loopback or the container.
const IO_TIMEOUT: Duration = Duration::from_secs(10);

/// One message's framing cap — what a real client's fragmentizer
/// allows; the fake server uses the same.
const MAX_MESSAGE_SIZE: u32 = 50 * 1024 * 1024;

/// Puts the unsolicited `EXISTS` on the wire while the client idles.
const IDLE_EVENT_DELAY: Duration = Duration::from_millis(50);

/// LOGOUT `BYE` alone on the wire: `ImapSend` finishes on the BYE and
/// the tagged OK goes unseen — why "LOGOUT is best effort".
const LOGOUT_BYE_DELAY: Duration = Duration::from_millis(50);

/// The wire transport under the gate: cleartext, or TLS via the
/// crate's `TlsIo` — not `rustls::StreamOwned`, whose read can trap
/// records in its plaintext buffer.
trait Wire: Read + Write + Send {
    fn sock(&self) -> &TcpStream;
}

impl Wire for TcpStream {
    fn sock(&self) -> &TcpStream {
        self
    }
}

impl Wire for TlsIo<ClientConnection> {
    fn sock(&self) -> &TcpStream {
        self.sock()
    }
}

/// The strict decode gate (spec/04 "Fault handling", the M2.1b
/// decision): reads feed a private `Fragmentizer`; only the bytes of
/// complete messages `ResponseCodec` decodes are released. The first
/// undecodable message fails every read from then on, so nothing past
/// the fault reaches the client. `gated` is false only for the run
/// documenting io-imap's skip-the-line defect.
struct GateStream {
    inner: Box<dyn Wire>,
    gated: bool,
    fragmentizer: Fragmentizer,
    codec: ResponseCodec,
    /// Verified message bytes waiting for the client's next read.
    released: VecDeque<u8>,
    wire_buf: Box<[u8; 8192]>,
    /// The peer hung up; reported once `released` drains.
    eof: bool,
    /// A message failed decode; every read fails from then on.
    poisoned: bool,
}

/// The read refusal a poisoned gate reports — and keeps reporting.
const GATE_REFUSAL: &str = "undecodable response message on the wire";

fn gate_error() -> io::Error {
    io::Error::new(io::ErrorKind::InvalidData, GATE_REFUSAL)
}

impl GateStream {
    fn new(inner: Box<dyn Wire>, gated: bool) -> Self {
        Self {
            inner,
            gated,
            fragmentizer: Fragmentizer::new(MAX_MESSAGE_SIZE),
            codec: ResponseCodec::new(),
            released: VecDeque::new(),
            wire_buf: Box::new([0; 8192]),
            eof: false,
            poisoned: false,
        }
    }

    fn plain(stream: TcpStream) -> io::Result<Self> {
        Self::wrap(stream, true)
    }

    /// The same stream with the gate off — the defect run.
    fn ungated(stream: TcpStream) -> io::Result<Self> {
        Self::wrap(stream, false)
    }

    fn wrap(stream: TcpStream, gated: bool) -> io::Result<Self> {
        stream.set_read_timeout(Some(IO_TIMEOUT))?;
        Ok(Self::new(Box::new(stream), gated))
    }

    /// Releases each framed message's wire bytes once it decodes;
    /// pulls from the wire only when nothing framed is pending.
    fn fill(&mut self) -> io::Result<()> {
        while let Some(_fragment) = self.fragmentizer.progress() {
            if !self.fragmentizer.is_message_complete() {
                continue;
            }
            match self.fragmentizer.decode_message(&self.codec) {
                Ok(_) => self
                    .released
                    .extend(self.fragmentizer.message_bytes().iter().copied()),
                Err(_) => {
                    self.poisoned = true;
                    return Err(gate_error());
                }
            }
        }
        if !self.released.is_empty() || self.eof {
            return Ok(());
        }
        match self.inner.read(self.wire_buf.as_mut_slice())? {
            0 => self.eof = true,
            n => self.fragmentizer.enqueue_bytes(&self.wire_buf[..n]),
        }
        Ok(())
    }
}

impl Read for GateStream {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        if !self.gated {
            return self.inner.read(buf);
        }
        loop {
            if !self.released.is_empty() {
                return self.released.read(buf);
            }
            if self.poisoned {
                return Err(gate_error());
            }
            if self.eof {
                return Ok(0);
            }
            self.fill()?;
        }
    }
}

impl Write for GateStream {
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        self.inner.write(buf)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

impl ImapStream for GateStream {
    fn as_any_mut(&mut self) -> &mut dyn Any {
        self
    }

    fn set_read_timeout(&self, timeout: Option<Duration>) -> io::Result<()> {
        self.inner.sock().set_read_timeout(timeout)
    }
}

/// spec/10's pinning verifier: the presented leaf must match the
/// pinned DER exactly — a different certificate is refused whatever
/// it chains to. Real checks stay: the pin anchors chain/validity,
/// the provider verifies the signature.
#[derive(Debug)]
struct CertPinVerifier {
    pin: CertificateDer<'static>,
    algs: WebPkiSupportedAlgorithms,
}

impl ServerCertVerifier for CertPinVerifier {
    fn verify_server_cert(
        &self,
        end_entity: &CertificateDer<'_>,
        intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        now: UnixTime,
    ) -> Result<ServerCertVerified, rustls::Error> {
        if end_entity.as_ref() != self.pin.as_ref() {
            return Err(CertificateError::UnknownIssuer.into());
        }
        // The pin matched; validity still applies, checked as WebPKI
        // does with the leaf as its own anchor. (The server name is
        // not checked: the pin is the identity, spec/10.)
        let mut roots = RootCertStore::empty();
        roots
            .add(self.pin.clone())
            .map_err(|err| rustls::Error::General(format!("pin is no trust anchor: {err}")))?;
        let parsed = ParsedCertificate::try_from(end_entity)?;
        rustls::client::verify_server_cert_signed_by_trust_anchor(
            &parsed,
            &roots,
            intermediates,
            now,
            self.algs.all,
        )?;
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls12_signature(message, cert, dss, &self.algs)
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> Result<HandshakeSignatureValid, rustls::Error> {
        verify_tls13_signature(message, cert, dss, &self.algs)
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.algs.supported_schemes()
    }
}

/// The upgrade's TLS half: a handshake pinned to `pin`.
fn pinned_client_stream(
    stream: TcpStream,
    server: &str,
    pin: CertificateDer<'static>,
) -> io::Result<TlsIo<ClientConnection>> {
    let verifier = CertPinVerifier {
        pin,
        algs: rustls::crypto::ring::default_provider().signature_verification_algorithms,
    };
    let config = ClientConfig::builder()
        .dangerous()
        .with_custom_certificate_verifier(Arc::new(verifier))
        .with_no_client_auth();
    client_stream_with(stream, server, Arc::new(config))
}

/// Greeting read, STARTTLS sent, tagged reply checked — by hand:
/// io-imap's `ImapStartTls` discards the reply's status, so a NO
/// would still send a ClientHello. spec/04 upgrades before credentials.
fn starttls_plain(mut tcp: &TcpStream) {
    // One CRLF-terminated line, a byte at a time: a buffered read
    // could pull bytes past the tagged OK into userspace, and
    // plaintext buffered at upgrade must not cross into TLS.
    fn line(mut tcp: &TcpStream) -> String {
        let mut line = Vec::new();
        let mut byte = [0u8; 1];
        while tcp.read(&mut byte).expect("read") == 1 {
            line.push(byte[0]);
            if byte[0] == b'\n' {
                return String::from_utf8(line).expect("UTF-8 line");
            }
        }
        panic!("eof mid-line")
    }

    line(tcp);
    tcp.write_all(b"s1 STARTTLS\r\n").expect("STARTTLS write");
    let reply = line(tcp);
    let accepted = reply.split_whitespace().nth(1) == Some("OK");
    assert!(accepted, "STARTTLS refused: {reply:?}");
}

/// Connects, upgrades with the pinning verifier (spec/10), returns a
/// client on the gated TLS stream.
fn connect_starttls(host: &str, port: u16) -> ImapClientStd {
    let tcp = TcpStream::connect((host, port)).expect("connect");
    tcp.set_read_timeout(Some(IO_TIMEOUT))
        .expect("read timeout");
    starttls_plain(&tcp);
    let tls = pinned_client_stream(tcp, host, cert_der())
        .expect("pinning TLS handshake against the test certificate");
    ImapClientStd::new(GateStream::new(Box::new(tls), true))
}

/// One IDLE round: collects unsolicited notifications until `done`,
/// then DONEs out; returns the event count.
fn idle_round(client: &mut ImapClientStd) -> usize {
    let done = Arc::new(AtomicBool::new(false));
    let mut idle = ImapIdle::new(done.clone(), ImapIdleOptions::default());
    let mut events = 0;
    let mut arg: Option<Vec<u8>> = None;
    let mut buf = [0u8; 4096];
    client
        .stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .expect("idle read timeout");
    loop {
        match idle.resume(&mut client.fragmentizer, arg.take().as_deref()) {
            ImapCoroutineState::Complete(out) => {
                out.expect("IDLE completes with its tagged OK");
                break;
            }
            ImapCoroutineState::Yielded(ImapIdleYield::WantsWrite(bytes)) => {
                client.stream.write_all(&bytes).expect("IDLE write");
            }
            ImapCoroutineState::Yielded(ImapIdleYield::WantsRead) => {
                match client.stream.read(&mut buf) {
                    Ok(0) => panic!("server closed during IDLE"),
                    Ok(n) => arg = Some(buf[..n].to_vec()),
                    Err(e)
                        if e.kind() == io::ErrorKind::WouldBlock
                            || e.kind() == io::ErrorKind::TimedOut =>
                    {
                        done.store(true, Ordering::Relaxed);
                    }
                    Err(e) => panic!("IDLE read: {e}"),
                }
            }
            ImapCoroutineState::Yielded(ImapIdleYield::Event(_)) => {
                events += 1;
                done.store(true, Ordering::Relaxed);
            }
        }
    }
    client
        .stream
        .set_read_timeout(Some(IO_TIMEOUT))
        .expect("restore read timeout");
    events
}

/// `UID FETCH` for the headers spec/04's sync keys on; returns the
/// message count.
fn fetch_headers(client: &mut ImapClientStd) -> usize {
    let header_fields = Vec1::try_from(vec![
        "SUBJECT".try_into().expect("header atom"),
        "X-PM-INTERNAL-ID".try_into().expect("header atom"),
    ])
    .expect("nonempty header list");
    let items = vec![
        MessageDataItemName::Uid,
        MessageDataItemName::Flags,
        MessageDataItemName::InternalDate,
        MessageDataItemName::Rfc822Size,
        MessageDataItemName::BodyExt {
            section: Some(Section::HeaderFields(None, header_fields)),
            partial: None,
            peek: true,
        },
    ];
    let fetched = client
        .fetch(
            SequenceSet::try_from("1:*").expect("sequence set"),
            items.into(),
            ImapMessageFetchOptions {
                uid: true,
                ..Default::default()
            },
        )
        .expect("UID FETCH headers");

    for data in fetched.values() {
        let items: &[MessageDataItem] = data.as_ref();
        let header = items.iter().find_map(|item| match item {
            MessageDataItem::BodyExt { data, .. } => Some(data),
            _ => None,
        });
        let text = String::from_utf8_lossy(
            header
                .and_then(|h| h.0.as_ref())
                .expect("FETCH item carries a header literal")
                .as_ref(),
        );
        assert!(
            text.contains("X-Pm-Internal-Id: pm-7f3a9c21") && text.contains("Subject:"),
            "header literal carries the shared Bridge id: {text}"
        );
    }
    fetched.len()
}

/// `Mailbox` has no `Display`.
fn mailbox_name(mailbox: &Mailbox) -> String {
    match mailbox {
        Mailbox::Inbox => "INBOX".to_string(),
        Mailbox::Other(other) => String::from_utf8_lossy(other.inner().as_ref()).into_owned(),
    }
}

/// The commands a server saw, tags replaced by `T<n>` in order of
/// first appearance (io-imap's tag counter is process-global).
/// `DONE` is tagless.
fn received_commands(server: &FakeImapServer) -> Vec<String> {
    let mut tags: Vec<String> = Vec::new();
    server
        .received()
        .iter()
        .map(|entry| {
            let Some((tag, rest)) = entry.line.split_once(' ') else {
                return entry.line.clone();
            };
            let index = tags
                .iter()
                .position(|known| *known == tag)
                .unwrap_or_else(|| {
                    tags.push(tag.to_string());
                    tags.len() - 1
                });
            format!("T{} {rest}", index + 1)
        })
        .collect()
}

/// The spec/04 read-only session against the Bridge profile: STARTTLS
/// with pinning, capability re-reads, ID, role LIST, EXAMINE, header
/// and flag fetches, IDLE, a UIDPLUS `[COPYUID]` receipt, LOGOUT.
#[test]
fn bridge_profile_operation_sequence() {
    let profile = Profile::bridge();
    let script = Script::new(vec![
        Step::on(
            Expect::command("STARTTLS"),
            vec![Reply::ok("Begin TLS negotiation now"), Reply::UpgradeTls],
        ),
        // spec/04 re-reads capabilities after STARTTLS and after
        // login: two steps, one consumed per command.
        profile.capability_step(),
        Step::on(Expect::command("LOGIN"), vec![Reply::ok("LOGIN completed")]),
        profile.capability_step(),
        Step::on(
            Expect::command("ID"),
            vec![
                Reply::untagged("ID (\"name\" \"Proton Mail Bridge\" \"version\" \"3.27.1\")"),
                Reply::ok("ID completed"),
            ],
        ),
        profile.list_step(),
        profile.examine_step("INBOX"),
        profile.fetch_headers_step("INBOX"),
        profile.fetch_flags_step("INBOX"),
        Step::on(
            Expect::command("IDLE"),
            vec![
                Reply::Continuation("idling".into()),
                Reply::Delay(IDLE_EVENT_DELAY),
                Reply::untagged("2 EXISTS"),
            ],
        ),
        Step::on(Expect::command("DONE"), vec![Reply::ok("IDLE terminated")]),
        Step::on(
            Expect::command("UID COPY"),
            vec![Reply::ok(profile.copyuid("7", "1"))],
        ),
        // The delay puts BYE on the wire alone: `ImapSend` returns
        // once BYE is consumed, so `ImapLogout` reports MissingTagged
        // — why M2.1b records LOGOUT as best effort.
        Step::on(
            Expect::command("LOGOUT"),
            vec![
                Reply::untagged("BYE Sweetbriar logging out"),
                Reply::Delay(LOGOUT_BYE_DELAY),
                Reply::ok("LOGOUT completed"),
            ],
        ),
    ]);
    let server = FakeImapServer::serve(vec![script]).expect("serve");
    let mut client = connect_starttls("127.0.0.1", server.port());

    let caps = client.capability().expect("post-TLS capability");
    let names: Vec<String> = caps.iter().map(|cap| format!("{cap:?}")).collect();
    for want in ["Imap4Rev1", "Idle", "UidPlus", "Move", "Unselect", "Id"] {
        assert!(
            names.iter().any(|n| n == want),
            "missing {want} in {names:?}"
        );
    }
    // spec/04: Bridge does not offer these — the sync must not use them.
    for absent in [
        "Condstore",
        "Qresync",
        "Namespace",
        "LiteralPlus",
        "SpecialUse",
    ] {
        assert!(
            !names.iter().any(|n| n == absent),
            "{absent} must not appear"
        );
    }

    client
        .login(
            "owner@example.org",
            "bridge-password",
            ImapLoginOptions::default(),
        )
        .expect("LOGIN");
    client.capability().expect("post-login capability");

    let server_id = client
        .id(ImapServerIdOptions {
            parameters: Some(vec![
                (
                    "name".try_into().expect("id atom"),
                    "Sweetbriar".try_into().expect("id atom"),
                ),
                (
                    "version".try_into().expect("id atom"),
                    "0.0.0".try_into().expect("id atom"),
                ),
            ]),
        })
        .expect("ID");
    let id_text = format!("{server_id:?}");
    assert!(
        id_text.contains("Proton Mail Bridge"),
        "server id: {id_text}"
    );

    let listing = client
        .list(
            Mailbox::try_from("").expect("list root"),
            ListMailbox::try_from("*").expect("list pattern"),
        )
        .expect("LIST");
    assert_eq!(listing.len(), 12, "Bridge LIST covers every mailbox");
    let names: Vec<String> = listing.iter().map(|(m, _, _)| mailbox_name(m)).collect();
    for want in [
        "INBOX",
        "Drafts",
        "Sent",
        "Starred",
        "Archive",
        "Spam",
        "Trash",
        "All Mail",
        "Folders",
        "Folders/Travel",
        "Labels",
        "Labels/Receipts",
    ] {
        assert!(
            names.iter().any(|n| n == want),
            "LIST missing {want}: {names:?}"
        );
    }
    // spec/04: Scheduled exists but stays hidden while empty.
    assert!(
        !names.iter().any(|n| n == "Scheduled"),
        "Scheduled is hidden"
    );
    let attrs_of = |name: &str| {
        listing
            .iter()
            .find(|(m, _, _)| mailbox_name(m) == name)
            .map(|(_, _, attrs)| attrs.iter().map(|a| format!("{a:?}")).collect::<Vec<_>>())
            .unwrap_or_default()
    };
    for role in [
        "Drafts", "Sent", "Flagged", "Archive", "Junk", "Trash", "All",
    ] {
        assert!(
            attrs_of(match role {
                "Flagged" => "Starred",
                "Junk" => "Spam",
                "All" => "All Mail",
                other => other,
            })
            .iter()
            .any(|a| a.contains(role)),
            "role attribute \\{role} missing"
        );
    }
    assert!(
        attrs_of("Folders").iter().any(|a| a == "Noselect"),
        "Folders is \\Noselect"
    );
    assert!(
        attrs_of("Labels").iter().any(|a| a == "Noselect"),
        "Labels is \\Noselect"
    );

    let selected = client
        .examine(
            Mailbox::try_from("INBOX").expect("mailbox"),
            ImapMailboxExamineOptions::default(),
        )
        .expect("EXAMINE");
    assert_eq!(selected.exists, Some(1));
    assert_eq!(selected.uid_validity, NonZeroU32::new(4100));
    assert_eq!(selected.uid_next, NonZeroU32::new(8));

    assert_eq!(
        fetch_headers(&mut client),
        1,
        "header fetch returns the message"
    );

    let flags = client
        .fetch(
            SequenceSet::try_from("1:*").expect("sequence set"),
            vec![MessageDataItemName::Flags].into(),
            ImapMessageFetchOptions {
                uid: true,
                ..Default::default()
            },
        )
        .expect("UID FETCH flags");
    assert_eq!(flags.len(), 1);

    assert_eq!(
        idle_round(&mut client),
        1,
        "one unsolicited EXISTS during IDLE"
    );

    let copyuid = client
        .copy(
            SequenceSet::try_from("7").expect("sequence set"),
            Mailbox::try_from("Archive").expect("mailbox"),
            ImapMessageCopyOptions { uid: true },
        )
        .expect("UID COPY");
    assert_eq!(
        copyuid,
        Some((4100, vec![7], vec![1])),
        "UIDPLUS receipt carries the uid mapping"
    );

    // Best effort per the M2.1b decision: the tagged OK is not yet on
    // the wire when `ImapSend` returns on the BYE.
    let err = client.logout().expect_err("LOGOUT stops at BYE");
    assert!(
        matches!(err, ImapClientError::Logout(ImapLogoutError::MissingTagged)),
        "expected MissingTagged, got {err:?}"
    );

    assert_eq!(
        received_commands(&server),
        [
            "T1 STARTTLS",
            "T2 CAPABILITY",
            "T3 LOGIN owner@example.org bridge-password",
            "T4 CAPABILITY",
            "T5 ID (\"name Sweetbriar version 0.0.0\")",
            "T6 LIST *",
            "T7 EXAMINE INBOX",
            "T8 UID FETCH 1:* (UID FLAGS INTERNALDATE RFC822.SIZE BODY.PEEK[HEADER.FIELDS (SUBJECT X-PM-INTERNAL-ID)])",
            "T9 UID FETCH 1:* FLAGS",
            "T10 IDLE",
            "DONE",
            "T11 UID COPY 7 Archive",
            "T12 LOGOUT",
        ],
        "the exact client operation sequence"
    );
    assert_eq!(server.unexpected_count(), 0, "no fallback answers");
}

/// spec/10's exact-match pin, exercised negatively: different DER
/// bytes must be refused — the check is the pin, not trust anchoring.
#[test]
fn starttls_refuses_a_certificate_off_the_pin() {
    let script = Script::new(vec![Step::on(
        Expect::command("STARTTLS"),
        vec![Reply::ok("Begin TLS negotiation now"), Reply::UpgradeTls],
    )]);
    let server = FakeImapServer::serve(vec![script]).expect("serve");
    let tcp = TcpStream::connect(("127.0.0.1", server.port())).expect("connect");
    tcp.set_read_timeout(Some(IO_TIMEOUT))
        .expect("read timeout");
    starttls_plain(&tcp);

    let mut wrong = cert_der().as_ref().to_vec();
    *wrong.last_mut().expect("nonempty certificate") ^= 0xFF;
    let err = match pinned_client_stream(tcp, "127.0.0.1", wrong.into()) {
        Ok(_) => panic!("a certificate off the pin must be refused"),
        Err(err) => err,
    };
    assert!(
        err.to_string().contains("certificate"),
        "the refusal reports the certificate: {err}"
    );
    assert_eq!(server.unexpected_count(), 0, "no fallback answers");
}

/// A valid FETCH item, the undecodable raw-UTF-8 ENVELOPE (spec/04,
/// Gluon #498), then another valid item.
fn fault_script() -> Script {
    Script::new(vec![
        Step::on(Expect::command("LOGIN"), vec![Reply::ok("LOGIN completed")]),
        Step::on(
            Expect::command("EXAMINE"),
            vec![
                Reply::untagged("1 EXISTS"),
                Reply::untagged("OK [UIDVALIDITY 4100] UIDs valid"),
                Reply::ok("[READ-ONLY] EXAMINE completed"),
            ],
        ),
        Step::on(
            Expect::command("UID FETCH"),
            vec![
                Reply::untagged(
                    "1 FETCH (UID 7 ENVELOPE (\"Mon, 02 Nov 2026 09:15:00 +0000\" \"first\" NIL NIL NIL NIL NIL NIL NIL NIL))",
                ),
                Reply::Raw(RAW_UTF8_ENVELOPE.to_vec()),
                Reply::untagged(
                    "3 FETCH (UID 9 ENVELOPE (\"Mon, 02 Nov 2026 09:15:00 +0000\" \"third\" NIL NIL NIL NIL NIL NIL NIL NIL))",
                ),
                Reply::ok("FETCH completed"),
            ],
        ),
    ])
    .greeting("* OK [CAPABILITY IMAP4rev1] fault script ready\r\n")
}

/// LOGIN/EXAMINE/`UID FETCH ENVELOPE`, returning the fetch outcome.
fn fetch_envelopes(
    client: &mut ImapClientStd,
) -> Result<BTreeMap<NonZeroU32, Vec1<MessageDataItem<'static>>>, ImapClientError> {
    client
        .login("u", "p", ImapLoginOptions::default())
        .expect("LOGIN");
    client
        .examine(
            Mailbox::try_from("INBOX").expect("mailbox"),
            ImapMailboxExamineOptions::default(),
        )
        .expect("EXAMINE");
    client.fetch(
        SequenceSet::try_from("1:*").expect("sequence set"),
        vec![MessageDataItemName::Envelope].into(),
        ImapMessageFetchOptions {
            uid: true,
            ..Default::default()
        },
    )
}

/// spec/04's rule, end to end: an undecodable response closes the
/// connection and nothing after the fault is applied. The ungated run
/// proves the io-imap defect on these exact bytes — `ImapSend` skips
/// the undecodable line and applies the item after it.
#[test]
fn malformed_response_closes_before_later_data() {
    // Ungated, for contrast: the items before and after the fault
    // both reach the application — the failure spec/04 forbids.
    let server = FakeImapServer::serve(vec![fault_script()]).expect("serve");
    let tcp = TcpStream::connect(("127.0.0.1", server.port())).expect("connect");
    let mut client = ImapClientStd::new(GateStream::ungated(tcp).expect("read timeout"));
    let applied = fetch_envelopes(&mut client)
        .expect("io-imap completes the fetch despite the fault on the wire");
    assert_eq!(
        applied.len(),
        2,
        "the item after the fault reached the application"
    );
    drop(client);

    // The same wire through the gate: the read fails at the fault and
    // nothing after it can be applied.
    let server = FakeImapServer::serve(vec![fault_script()]).expect("serve");
    let tcp = TcpStream::connect(("127.0.0.1", server.port())).expect("connect");
    let mut client = ImapClientStd::new(GateStream::plain(tcp).expect("read timeout"));
    let err = fetch_envelopes(&mut client).expect_err("the gate must refuse the fault");
    let ImapClientError::Io(cause) = &err else {
        panic!("expected the gate's read refusal, got {err:?}")
    };
    assert_eq!(
        cause.kind(),
        io::ErrorKind::InvalidData,
        "the gate refuses with"
    );
    assert!(
        cause.to_string().contains(GATE_REFUSAL),
        "gate refusal: {cause}"
    );
    drop(client);
    assert_eq!(
        received_commands(&server),
        [
            "T1 LOGIN u p",
            "T2 EXAMINE INBOX",
            "T3 UID FETCH 1:* ENVELOPE"
        ],
        "no command was attempted after the fault"
    );
    assert_eq!(server.unexpected_count(), 0, "no fallback answers");
}

/// The message APPEND seeds, so the header FETCH has a literal to
/// check — it carries the Bridge profile's provider-id header.
const DOVECOT_SEED: &[u8] = b"From: spike@example.org\r\nSubject: Dovecot seed\r\nMessage-ID: <dovecot-seed@example.org>\r\nX-Pm-Internal-Id: pm-7f3a9c21\r\n\r\nseed body\r\n";

/// The mailbox `UID COPY` targets, created on the fly.
const DOVECOT_COPY_TARGET: &str = "Archive";

/// The same operation sequence against a real Dovecot — spec/11's
/// protocol suite. Runs only when `SB_IMAP_DOVECOT=<host:port>` is
/// set (`cargo xtask dovecot` sets it for the pinned container, which
/// presents the committed test certificate); an unreachable container
/// fails rather than skipping.
#[test]
fn dovecot_operation_sequence() {
    let Ok(addr) = env::var("SB_IMAP_DOVECOT") else {
        eprintln!("SB_IMAP_DOVECOT unset; not asked to run the Dovecot suite");
        return;
    };
    let user = env::var("SB_IMAP_DOVECOT_USER").unwrap_or_else(|_| "spike".to_string());
    let password = env::var("SB_IMAP_DOVECOT_PASSWORD").unwrap_or_else(|_| "dovecot".to_string());
    let (host, port) = addr.split_once(':').expect("SB_IMAP_DOVECOT is host:port");
    run_dovecot_suite(
        host,
        port.parse().expect("SB_IMAP_DOVECOT port"),
        &user,
        &password,
    );
}

/// The suite body, callable against a fake witness server.
fn run_dovecot_suite(host: &str, port: u16, user: &str, password: &str) {
    // spec/04: STARTTLS before any credential, with the pinning
    // verifier. A container without TLS fails here.
    let mut client = connect_starttls(host, port);

    let caps = client.capability().expect("post-TLS CAPABILITY");
    let names: Vec<String> = caps.iter().map(|cap| cap.to_string()).collect();
    let has = |want: &str| names.iter().any(|n| n.eq_ignore_ascii_case(want));
    assert!(has("IMAP4rev1") || has("IMAP4rev2"), "IMAP4: {names:?}");
    assert!(has("UIDPLUS"), "UIDPLUS is advertised: {names:?}");

    client
        .login(user, password, ImapLoginOptions::default())
        .expect("LOGIN against the container's static passdb");
    client.capability().expect("post-login CAPABILITY");

    client
        .id(ImapServerIdOptions { parameters: None })
        .expect("ID NIL");

    // Seed: the COPY destination, then one message in INBOX; CREATE
    // and APPEND answer with the UIDPLUS receipts Dovecot advertises.
    client
        .create(Mailbox::try_from(DOVECOT_COPY_TARGET).expect("mailbox"))
        .expect("CREATE the COPY destination");

    let listing = client
        .list(
            Mailbox::try_from("").expect("list root"),
            ListMailbox::try_from("*").expect("list pattern"),
        )
        .expect("LIST");
    let listed: Vec<String> = listing.iter().map(|(m, _, _)| mailbox_name(m)).collect();
    for want in ["INBOX", DOVECOT_COPY_TARGET] {
        assert!(
            listed.iter().any(|n| n.eq_ignore_ascii_case(want)),
            "Dovecot lists {want}: {listed:?}"
        );
    }

    let (_, appenduid) = client
        .append(
            Mailbox::try_from("INBOX").expect("mailbox"),
            DOVECOT_SEED,
            ImapMessageAppendOptions {
                flags: vec![Flag::Seen],
                ..Default::default()
            },
        )
        .expect("APPEND the seed message");
    let (_, uid) = appenduid.expect("UIDPLUS names the appended UID");

    let selected = client
        .examine(
            Mailbox::try_from("INBOX").expect("mailbox"),
            ImapMailboxExamineOptions::default(),
        )
        .expect("EXAMINE INBOX");
    assert_eq!(selected.exists, Some(1), "the seed is in INBOX");
    assert!(selected.uid_validity.is_some(), "UIDVALIDITY reported");

    assert_eq!(
        fetch_headers(&mut client),
        1,
        "header fetch returns the seeded message"
    );

    let flags = client
        .fetch(
            SequenceSet::try_from("1:*").expect("sequence set"),
            vec![MessageDataItemName::Flags].into(),
            ImapMessageFetchOptions {
                uid: true,
                ..Default::default()
            },
        )
        .expect("UID FETCH flags");
    assert_eq!(flags.len(), 1, "one message, one flags item");
    let items: &[MessageDataItem] = flags.values().next().expect("one item").as_ref();
    let flag_list = items.iter().find_map(|item| match item {
        MessageDataItem::Flags(flags) => Some(flags),
        _ => None,
    });
    assert!(
        flag_list.is_some_and(|f| f.contains(&FlagFetch::Flag(Flag::Seen))),
        "APPEND set \\Seen: {items:?}"
    );

    idle_round(&mut client);

    // INBOX is still selected, so the COPY sources come from it.
    let Some((uidvalidity, sources, destinations)) = client
        .copy(
            SequenceSet::try_from(uid).expect("sequence set"),
            Mailbox::try_from(DOVECOT_COPY_TARGET).expect("mailbox"),
            ImapMessageCopyOptions { uid: true },
        )
        .expect("UID COPY")
    else {
        panic!("Dovecot must return the UIDPLUS COPYUID receipt")
    };
    assert_eq!(sources, vec![uid], "COPYUID echoes the source UID");
    assert_eq!(destinations.len(), 1, "one copied message");

    // spec/04's copy completion: COPYUID names the destination's
    // UIDVALIDITY, and the new UID answers in that mailbox.
    let archive = client
        .examine(
            Mailbox::try_from(DOVECOT_COPY_TARGET).expect("mailbox"),
            ImapMailboxExamineOptions::default(),
        )
        .expect("EXAMINE the COPY destination");
    assert_eq!(archive.exists, Some(1), "the copy landed");
    assert_eq!(
        archive.uid_validity,
        NonZeroU32::new(uidvalidity),
        "COPYUID carries the destination's epoch"
    );
    let found = client
        .fetch(
            SequenceSet::try_from(destinations[0]).expect("sequence set"),
            vec![MessageDataItemName::Uid].into(),
            ImapMessageFetchOptions {
                uid: true,
                ..Default::default()
            },
        )
        .expect("UID FETCH in the destination");
    assert!(
        found.values().any(|items| {
            let items: &[MessageDataItem] = items.as_ref();
            items
                .iter()
                .any(|i| matches!(i, MessageDataItem::Uid(u) if u.get() == destinations[0]))
        }),
        "the COPYUID destination UID answers: {found:?}"
    );

    // LOGOUT is best effort (the M2.1b decision): `ImapSend` finishes
    // at the BYE, so Ok and MissingTagged are both correct.
    match client.logout() {
        Ok(()) | Err(ImapClientError::Logout(ImapLogoutError::MissingTagged)) => {}
        Err(err) => panic!("unexpected LOGOUT failure: {err:?}"),
    }
}

/// A cleartext-only witness satisfied the old suite. Now STARTTLS is
/// the first exchange and its tagged OK is checked.
#[test]
fn dovecot_suite_rejects_a_server_without_starttls() {
    let script = Script::new(vec![Step::on(
        Expect::command("STARTTLS"),
        vec![Reply::TaggedNo("STARTTLS disabled".into())],
    )]);
    let server = FakeImapServer::serve(vec![script]).expect("serve");
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        run_dovecot_suite("127.0.0.1", server.port(), "spike", "dovecot");
    }))
    .expect_err("the suite must refuse the witness");
    assert_eq!(received_commands(&server), ["T1 STARTTLS"]);
}
