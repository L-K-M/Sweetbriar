//! `Connection`: the IMAP setup (spec/04 "Connections") and the
//! decoded setup exchanges over the strict gate. Setup order: greeting over
//! implicit TLS, CAPABILITY;
//! refuse LOGIN when `LOGINDISABLED`; LOGIN; CAPABILITY unless the
//! tagged OK carried it; ID. No credential is sent before TLS. LOGOUT
//! is best effort.

mod reads;
pub use reads::{MailboxContext, MailboxNotice, ReadObservation};

use std::fmt;
use std::net::{IpAddr, Ipv4Addr};

use io_imap::types::command::CommandBody;
use io_imap::types::response::{Capability, Code};
use rustls::pki_types::ServerName;
use tokio::io::{AsyncRead, AsyncWrite, AsyncWriteExt};
use tokio::net::TcpStream;
use tokio_rustls::TlsConnector;
use tokio_rustls::client::TlsStream;

use crate::error::ImapError;
use crate::exchange::{self, Authentication, Reply};
use crate::gate::Gate;
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
        if exchange::greeting(&mut gate, &mut tcp).await? != Authentication::Required {
            return Err(ImapError::StartTlsRefused);
        }
        let pre_tls = exchange::capability(&mut gate, &mut tcp).await?;
        if gate.poisoned() {
            return Err(ImapError::Malformed);
        }
        if !pre_tls.reusable {
            return Err(ImapError::Closed);
        }
        if !advertises(&pre_tls.result, |cap| matches!(cap, Capability::StartTls)) {
            return Err(ImapError::StartTlsMissing);
        }
        request_starttls(&mut gate, &mut tcp).await?;

        let wire = connect_tls(tcp, options.pin, options.host).await?;
        let mut connection = Self {
            host: options.host,
            port: options.port,
            wire: Some(wire),
            gate,
            capabilities: Vec::new(),
            selection: None,
            pending_notices: Vec::new(),
        };
        connection
            .setup_authenticated(&options, Authentication::Required)
            .await?;
        Ok(connection)
    }

    async fn setup(&mut self, options: &ConnectOptions) -> Result<(), ImapError> {
        let authentication = self.greeting().await?;
        self.setup_authenticated(options, authentication).await
    }

    /// The setup after the greeting (implicit TLS) or after the
    /// upgrade (STARTTLS): refresh capabilities, refuse a server that
    /// still disables LOGIN, LOGIN, refresh again unless the tagged OK
    /// carried capabilities, then ID. No second greeting is read.
    async fn setup_authenticated(
        &mut self,
        options: &ConnectOptions,
        authentication: Authentication,
    ) -> Result<(), ImapError> {
        self.capabilities = self.capability().await?;
        if authentication == Authentication::Required {
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
        }
        self.id().await?;
        if self.wire.is_none() {
            return Err(ImapError::Closed);
        }
        Ok(())
    }

    async fn greeting(&mut self) -> Result<Authentication, ImapError> {
        let mut wire = self.wire.take().ok_or(ImapError::Closed)?;
        let authentication = exchange::greeting(&mut self.gate, &mut wire).await?;
        if self.gate.reusable() {
            self.wire = Some(wire);
        }
        Ok(authentication)
    }

    /// Refresh capabilities. While selected, epoch changes fail closed
    /// and reconciliation notices are retained for the next read.
    pub async fn capability(&mut self) -> Result<Vec<Capability<'static>>, ImapError> {
        let capabilities = if self.selection.is_some() {
            self.selected_capability().await?
        } else {
            let mut wire = self.wire.take().ok_or(ImapError::Closed)?;
            let outcome = exchange::capability(&mut self.gate, &mut wire).await?;
            if outcome.reusable {
                self.wire = Some(wire);
            }
            outcome.result
        };
        self.capabilities = capabilities.clone();
        Ok(capabilities)
    }

    async fn login(
        &mut self,
        user: &str,
        password: &Password,
    ) -> Result<Vec<Capability<'static>>, ImapError> {
        let body = CommandBody::login(user.to_string(), password.as_str().to_string())
            .map_err(|_| ImapError::command("LOGIN", "invalid credentials"))?;
        match self.run_setup(body, "LOGIN").await? {
            Reply::Tagged(reply) if reply.body.kind == io_imap::types::response::StatusKind::Ok => {
                match reply.body.code {
                    Some(Code::Capability(values)) => Ok(values.into_inner()),
                    _ => Ok(Vec::new()),
                }
            }
            _ => Err(ImapError::LoginRefused),
        }
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
        self.run_setup(
            CommandBody::Id {
                parameters: Some(parameters),
            },
            "ID",
        )
        .await?
        .ok("ID")?;
        Ok(())
    }

    /// `LOGOUT`, best effort (the M2.1b decision): a valid BYE is
    /// sufficient even when the peer closes before its tagged reply.
    pub async fn logout(&mut self) -> Result<(), ImapError> {
        let result = self.run_setup(CommandBody::Logout, "LOGOUT").await;
        self.wire = None;
        match result? {
            Reply::Bye => Ok(()),
            _ => Err(ImapError::command("LOGOUT", "server did not report BYE")),
        }
    }

    /// The capabilities from the last refresh.
    pub fn capabilities(&self) -> &[Capability<'static>] {
        &self.capabilities
    }

    async fn run_setup(
        &mut self,
        body: CommandBody<'static>,
        operation: &'static str,
    ) -> Result<Reply, ImapError> {
        // This future owns the socket across all writes and reads. Errors
        // and cancellation cannot leave a reply for a following command.
        let mut wire = self.wire.take().ok_or(ImapError::Closed)?;
        let outcome = exchange::run(&mut self.gate, &mut wire, body, operation, |_| Ok(())).await?;
        if outcome.result.succeeded() && outcome.reusable {
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

/// Exactly one matched completion authorizes TLS, and every byte already
/// received after it remains plaintext and prevents the upgrade.
async fn request_starttls(
    gate: &mut Gate,
    wire: &mut (impl AsyncRead + AsyncWrite + Unpin),
) -> Result<(), ImapError> {
    let outcome =
        match exchange::run(gate, wire, CommandBody::StartTLS, "STARTTLS", |_| Ok(())).await {
            Err(ImapError::UnexpectedCompletion) => return Err(ImapError::StartTlsRefused),
            other => other?,
        };
    if !outcome.result.succeeded() {
        return Err(ImapError::StartTlsRefused);
    }
    if gate.poisoned() {
        return Err(ImapError::Malformed);
    }
    if gate.has_plaintext_residual() {
        return Err(ImapError::StartTlsInjection);
    }
    Ok(())
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
