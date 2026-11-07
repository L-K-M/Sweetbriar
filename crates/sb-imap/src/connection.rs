//! `Connection`: the IMAP setup (spec/04 "Connections") and the
//! coroutine pump over the strict gate. Setup order: greeting over
//! implicit TLS, CAPABILITY;
//! refuse LOGIN when `LOGINDISABLED`; LOGIN; CAPABILITY unless the
//! tagged OK carried it; ID. No credential is sent before TLS. LOGOUT
//! is best effort.

mod reads;
pub use reads::{MailboxContext, MailboxNotice, ReadObservation};

use std::fmt;
use std::net::{IpAddr, Ipv4Addr};

use io_imap::codec::{CommandCodec, ResponseCodec, encode::Encoder, fragmentizer::Fragmentizer};
use io_imap::coroutine::{ImapCoroutine, ImapYield};
use io_imap::rfc2971::id::{ImapServerId, ImapServerIdOptions};
use io_imap::rfc3501::capability::ImapCapabilityGet;
use io_imap::rfc3501::greeting::{ImapGreetingGet, ImapGreetingGetOptions};
use io_imap::rfc3501::login::{ImapLogin, ImapLoginOptions};
use io_imap::rfc3501::logout::{ImapLogout, ImapLogoutError};
use io_imap::types::command::{Command, CommandBody};
use io_imap::types::core::TagGenerator;
use io_imap::types::response::{Capability, Response, Status, StatusKind};
use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use crate::error::ImapError;
use crate::gate::{Gate, MAX_MESSAGE_SIZE};
use crate::secret::Password;
use crate::tls::{CertificatePin, pinned_client_config};

/// Which peer addresses a connection may open.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum HostPolicy {
    /// Only the literal `127.0.0.1` (the Bridge preset, spec/10). `::1`
    /// is refused too: Bridge's certificate names only the IPv4
    /// address.
    LoopbackOnly,
    /// Any address. Generic IMAP later.
    Any,
}

/// Everything [`Connection::connect`] needs. The password is held by
/// [`Password`], which never prints or clones it.
#[derive(Debug)]
pub struct ConnectOptions {
    /// The peer address. The Bridge preset passes `127.0.0.1`.
    pub host: IpAddr,
    /// The peer port.
    pub port: u16,
    /// The address policy, checked before any socket opens.
    pub host_policy: HostPolicy,
    /// The SHA-256 pin of the leaf certificate's DER.
    pub pin: CertificatePin,
    /// The account's IMAP user name.
    pub user: String,
    /// The account's IMAP password.
    pub password: Password,
}

impl fmt::Display for ConnectOptions {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "IMAP connection to {}:{}", self.host, self.port)
    }
}

/// A connected, authenticated IMAP session. The setup is complete when
/// `connect` returns; [`Connection::capability`] and later read
/// operations run over the same strict gate.
pub struct Connection {
    host: IpAddr,
    port: u16,
    wire: Option<Wire>,
    gate: Gate,
    fragmentizer: Fragmentizer,
    capabilities: Vec<Capability<'static>>,
    selection: Option<MailboxContext>,
    pending_notices: Vec<MailboxNotice>,
}

impl fmt::Debug for Connection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Connection")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("closed", &self.wire.is_none())
            .field("capabilities", &self.capabilities)
            .finish()
    }
}

impl fmt::Display for Connection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "IMAP connection to {}:{}", self.host, self.port)
    }
}

pub(crate) type Wire = TlsStream<TcpStream>;

impl Connection {
    /// Opens the connection, checks the host policy, and runs the
    /// setup. The host policy is checked before any socket opens.
    pub async fn connect(options: ConnectOptions) -> Result<Self, ImapError> {
        if options.host_policy == HostPolicy::LoopbackOnly
            && options.host != IpAddr::V4(Ipv4Addr::LOCALHOST)
        {
            return Err(ImapError::NotLoopback);
        }

        let tcp = TcpStream::connect((options.host, options.port))
            .await
            .map_err(ImapError::from_io)?;
        let wire = connect_tls(tcp, options.pin, options.host).await?;
        let mut connection = Self {
            host: options.host,
            port: options.port,
            wire: Some(wire),
            gate: Gate::new(),
            fragmentizer: Fragmentizer::new(MAX_MESSAGE_SIZE),
            capabilities: Vec::new(),
            selection: None,
            pending_notices: Vec::new(),
        };
        connection.setup(&options).await?;
        Ok(connection)
    }

    /// Opens the connection, checks the host policy, and runs the
    /// STARTTLS setup: greeting and pre-TLS CAPABILITY in cleartext,
    /// a checked STARTTLS command, a refusal of any buffered trailing
    /// plaintext, then the pinned TLS handshake and the authenticated
    /// setup. No credential is sent before TLS, and no second greeting
    /// is read after the upgrade.
    pub async fn connect_starttls(options: ConnectOptions) -> Result<Self, ImapError> {
        if options.host_policy == HostPolicy::LoopbackOnly
            && options.host != IpAddr::V4(Ipv4Addr::LOCALHOST)
        {
            return Err(ImapError::NotLoopback);
        }

        // The plain socket is owned by this future: an error or a
        // cancel at any await drops it, so no half-open peer is left
        // holding a command.
        let mut tcp = TcpStream::connect((options.host, options.port))
            .await
            .map_err(ImapError::from_io)?;
        let mut gate = Gate::new();
        let mut fragmentizer = Fragmentizer::new(MAX_MESSAGE_SIZE);

        run_plain(
            &mut gate,
            &mut fragmentizer,
            &mut tcp,
            ImapGreetingGet::new(ImapGreetingGetOptions {
                ensure_capabilities: false,
            }),
        )
        .await?;

        // Pre-TLS LOGINDISABLED is normal; only STARTTLS must be
        // advertised before the upgrade (spec/04).
        let pre_tls = run_plain(
            &mut gate,
            &mut fragmentizer,
            &mut tcp,
            ImapCapabilityGet::new(),
        )
        .await?;
        if !advertises(&pre_tls, |cap| matches!(cap, Capability::StartTls)) {
            return Err(ImapError::StartTlsMissing);
        }

        request_starttls(&mut gate, &mut fragmentizer, &mut tcp).await?;

        let wire = connect_tls(tcp, options.pin, options.host).await?;
        let mut connection = Self {
            host: options.host,
            port: options.port,
            wire: Some(wire),
            gate,
            fragmentizer,
            capabilities: Vec::new(),
            selection: None,
            pending_notices: Vec::new(),
        };
        connection.setup_authenticated(&options).await?;
        Ok(connection)
    }

    async fn setup(&mut self, options: &ConnectOptions) -> Result<(), ImapError> {
        self.greeting().await?;
        self.setup_authenticated(options).await
    }

    /// The setup after the greeting (implicit TLS) or after the
    /// upgrade (STARTTLS): refresh capabilities, refuse a server that
    /// still disables LOGIN, LOGIN, refresh again unless the tagged OK
    /// carried capabilities, then ID. No second greeting is read.
    async fn setup_authenticated(&mut self, options: &ConnectOptions) -> Result<(), ImapError> {
        self.capabilities = self.capability().await?;
        if advertises(&self.capabilities, |cap| {
            matches!(cap, Capability::LoginDisabled)
        }) {
            return Err(ImapError::LoginDisabled);
        }

        // The login's tagged OK may carry the capabilities; only a
        // round-trip is skipped when it did (spec/04).
        let login_capabilities = self.login(&options.user, &options.password).await?;
        self.capabilities = if login_capabilities.is_empty() {
            self.capability().await?
        } else {
            login_capabilities
        };

        self.id().await?;
        Ok(())
    }

    async fn greeting(&mut self) -> Result<(), ImapError> {
        self.run(ImapGreetingGet::new(ImapGreetingGetOptions {
            ensure_capabilities: false,
        }))
        .await?;
        Ok(())
    }

    /// Refresh capabilities. While selected, epoch changes fail closed
    /// and reconciliation notices are retained for the next read.
    pub async fn capability(&mut self) -> Result<Vec<Capability<'static>>, ImapError> {
        let capabilities = if self.selection.is_some() {
            self.selected_capability().await?
        } else {
            self.run(ImapCapabilityGet::new()).await?
        };
        self.capabilities = capabilities.clone();
        Ok(capabilities)
    }

    async fn login(
        &mut self,
        user: &str,
        password: &Password,
    ) -> Result<Vec<Capability<'static>>, ImapError> {
        let coroutine = ImapLogin::new(user, password.as_str(), ImapLoginOptions::default())
            .map_err(|_| ImapError::command("LOGIN", "invalid credentials"))?;
        self.run(coroutine).await
    }

    async fn id(&mut self) -> Result<(), ImapError> {
        let parameters = vec![
            (
                "name".try_into().expect("id atom"),
                "Sweetbriar".try_into().expect("id value"),
            ),
            (
                "version".try_into().expect("id atom"),
                env!("CARGO_PKG_VERSION").try_into().expect("id value"),
            ),
        ];
        self.run(ImapServerId::new(ImapServerIdOptions {
            parameters: Some(parameters),
        }))
        .await?;
        Ok(())
    }

    /// `LOGOUT`, best effort (the M2.1b decision): `ImapLogout` stops
    /// at BYE with `MissingTagged`, which is a success for a logout.
    pub async fn logout(&mut self) -> Result<(), ImapError> {
        let result = self.run_raw(ImapLogout::new()).await;
        self.wire = None;
        match result? {
            Ok(()) | Err(ImapLogoutError::MissingTagged) => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    /// The capabilities from the last refresh.
    pub fn capabilities(&self) -> &[Capability<'static>] {
        &self.capabilities
    }

    /// Runs a single-command coroutine (or the greeting) over the gate.
    /// Multi-command flows are split at the call sites so each exchange
    /// has one expected tag, including across synchronizing literals.
    /// A poisoned gate fails the read with [`ImapError::Malformed`] and
    /// closes the connection; later runs report [`ImapError::Closed`]. A
    /// completed result before poison in the same fill is returned first.
    async fn run<C, T, E>(&mut self, coroutine: C) -> Result<T, ImapError>
    where
        C: ImapCoroutine<Yield = ImapYield, Return = Result<T, E>>,
        E: Into<ImapError>,
    {
        self.run_raw(coroutine).await?.map_err(Into::into)
    }

    async fn run_raw<C, T, E>(&mut self, coroutine: C) -> Result<Result<T, E>, ImapError>
    where
        C: ImapCoroutine<Yield = ImapYield, Return = Result<T, E>>,
    {
        // The future owns the socket during an exchange. Dropping it at
        // any await closes the transport, including after a partial write;
        // no later coroutine can consume the abandoned command's reply.
        let mut wire = self.wire.take().ok_or(ImapError::Closed)?;
        let outcome =
            crate::exchange::run(&mut self.gate, &mut self.fragmentizer, &mut wire, coroutine)
                .await?;
        if outcome.reusable {
            self.wire = Some(wire);
        }
        Ok(outcome.result)
    }
}

pub(crate) async fn write_command(
    wire: &mut (impl AsyncWrite + Unpin),
    bytes: &[u8],
) -> std::io::Result<()> {
    wire.write_all(bytes).await?;
    wire.flush().await
}

/// Read STARTTLS responses only until the first tagged completion. ImapSend
/// consumes every coalesced response and retains only its last tagged status;
/// that would let an extra OK hide a refusal, mismatched tag or duplicate reply.
async fn request_starttls(
    gate: &mut Gate,
    fragmentizer: &mut Fragmentizer,
    wire: &mut (impl AsyncRead + AsyncWrite + Unpin),
) -> Result<(), ImapError> {
    let tag = TagGenerator::new().generate();
    let command = Command {
        tag: tag.clone(),
        body: CommandBody::StartTLS,
    };
    let bytes = CommandCodec::new().encode(&command).dump();
    write_command(wire, &bytes)
        .await
        .map_err(ImapError::from_io)?;
    let codec = ResponseCodec::new();
    loop {
        let bytes = gate.read(wire).await?;
        if gate.poisoned() {
            return Err(ImapError::Malformed);
        }
        if bytes.is_empty() {
            return Err(ImapError::command(
                "STARTTLS",
                "connection ended before completion",
            ));
        }
        fragmentizer.enqueue_bytes(&bytes);
        while fragmentizer.progress().is_some() {
            if !fragmentizer.is_message_complete() {
                continue;
            }
            let response = fragmentizer
                .decode_message(&codec)
                .map_err(|_| ImapError::Malformed)?;
            match response {
                Response::Status(Status::Tagged(reply)) => {
                    if reply.tag.inner() != tag.inner() || reply.body.kind != StatusKind::Ok {
                        return Err(ImapError::StartTlsRefused);
                    }
                    if gate.has_plaintext_residual() || fragmentizer_has_residual(fragmentizer) {
                        return Err(ImapError::StartTlsInjection);
                    }
                    return Ok(());
                }
                Response::Status(Status::Bye(_)) | Response::CommandContinuationRequest(_) => {
                    return Err(ImapError::StartTlsRefused);
                }
                // Unsolicited data before the completion grants no authority.
                // All pre-TLS capabilities are discarded after the handshake.
                _ => {}
            }
        }
    }
}

/// Runs one coroutine to completion over the gate while borrowing the
/// plain socket, mirroring [`Connection::run_raw`] without taking
/// ownership: the socket stays owned by the STARTTLS setup future, so
/// a cancel closes it. Used only before the TLS upgrade.
async fn run_plain<W, C, T, E>(
    gate: &mut Gate,
    fragmentizer: &mut Fragmentizer,
    wire: &mut W,
    coroutine: C,
) -> Result<T, ImapError>
where
    W: AsyncRead + AsyncWrite + Unpin,
    C: ImapCoroutine<Yield = ImapYield, Return = Result<T, E>>,
    E: Into<ImapError>,
{
    let outcome = crate::exchange::run(gate, fragmentizer, wire, coroutine).await?;
    if gate.poisoned() {
        return Err(ImapError::Malformed);
    }
    let result = outcome.result.map_err(Into::into)?;
    if !outcome.reusable {
        return Err(ImapError::Closed);
    }
    Ok(result)
}

/// True when `fragmentizer` still holds bytes a completed coroutine
/// did not consume: a partial frame or unparsed bytes. Draining
/// `progress` is safe: a clean fragmentizer ends with an empty message
/// buffer, ready for the next command.
fn fragmentizer_has_residual(fragmentizer: &mut Fragmentizer) -> bool {
    if fragmentizer.progress().is_some() {
        return true;
    }
    !fragmentizer.message_bytes().is_empty()
}

fn advertises(
    capabilities: &[Capability<'static>],
    wanted: impl Fn(&Capability<'_>) -> bool,
) -> bool {
    capabilities.iter().any(wanted)
}

async fn connect_tls(tcp: TcpStream, pin: CertificatePin, host: IpAddr) -> Result<Wire, ImapError> {
    let (config, presented) = pinned_client_config(pin);
    let server = ServerName::try_from(host.to_string())
        .map_err(|_| ImapError::command("TLS", "invalid server name"))?;
    match TlsConnector::from(config).connect(server, tcp).await {
        Ok(tls) => Ok(tls),
        Err(err) => {
            if let Some(presented_sha256) = presented.lock().expect("verifier mutex").take() {
                return Err(ImapError::PinMismatch { presented_sha256 });
            }
            Err(ImapError::from_io(err))
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::BufWriter;

    #[tokio::test]
    async fn command_is_flushed_before_return() {
        let (client, _server) = tokio::io::duplex(64);
        let mut wire = BufWriter::new(client);
        write_command(&mut wire, b"A1 CAPABILITY\r\n")
            .await
            .unwrap();
        assert!(wire.buffer().is_empty(), "command remains buffered");
    }
}
