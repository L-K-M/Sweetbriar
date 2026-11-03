//! `Connection`: the IMAP setup (spec/04 "Connections") and the
//! coroutine pump over the strict gate. Setup order: greeting over
//! implicit TLS, CAPABILITY;
//! refuse LOGIN when `LOGINDISABLED`; LOGIN; CAPABILITY unless the
//! tagged OK carried it; ID. No credential is sent before TLS. LOGOUT
//! is best effort.

use std::fmt;
use std::net::{IpAddr, Ipv4Addr};

use io_imap::codec::fragmentizer::Fragmentizer;
use io_imap::coroutine::{ImapCoroutine, ImapCoroutineState, ImapYield};
use io_imap::rfc2971::id::{ImapServerId, ImapServerIdOptions};
use io_imap::rfc3501::capability::ImapCapabilityGet;
use io_imap::rfc3501::greeting::{ImapGreetingGet, ImapGreetingGetOptions};
use io_imap::rfc3501::login::{ImapLogin, ImapLoginOptions};
use io_imap::rfc3501::logout::{ImapLogout, ImapLogoutError};
use io_imap::types::response::Capability;
use rustls::pki_types::ServerName;
use tokio::io::{AsyncWrite, AsyncWriteExt};
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
    /// The connection-wide parser the io-imap coroutines share.
    fragmentizer: Fragmentizer,
    capabilities: Vec<Capability<'static>>,
    closed: bool,
}

impl fmt::Debug for Connection {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Connection")
            .field("host", &self.host)
            .field("port", &self.port)
            .field("closed", &self.closed)
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
            closed: false,
        };
        connection.setup(&options).await?;
        Ok(connection)
    }

    /// The setup order from spec/04 "Connections".
    async fn setup(&mut self, options: &ConnectOptions) -> Result<(), ImapError> {
        self.greeting().await?;

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

    /// Reads and checks the server greeting through the gate.
    async fn greeting(&mut self) -> Result<(), ImapError> {
        self.run(ImapGreetingGet::new(ImapGreetingGetOptions {
            ensure_capabilities: false,
        }))
        .await?;
        Ok(())
    }

    /// `CAPABILITY`.
    pub async fn capability(&mut self) -> Result<Vec<Capability<'static>>, ImapError> {
        self.run(ImapCapabilityGet::new()).await
    }

    /// `LOGIN` over the pinned-TLS channel.
    async fn login(
        &mut self,
        user: &str,
        password: &Password,
    ) -> Result<Vec<Capability<'static>>, ImapError> {
        let coroutine = ImapLogin::new(user, password.as_str(), ImapLoginOptions::default())
            .map_err(|_| ImapError::command("LOGIN", "invalid credentials"))?;
        self.run(coroutine).await
    }

    /// `ID ("name" "Sweetbriar" "version" "<version>")`.
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
        self.close().await;
        match result? {
            Ok(()) | Err(ImapLogoutError::MissingTagged) => Ok(()),
            Err(err) => Err(err.into()),
        }
    }

    /// The capabilities from the last refresh.
    pub fn capabilities(&self) -> &[Capability<'static>] {
        &self.capabilities
    }

    /// Runs one standard-shape coroutine to completion over the gate.
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
        if self.closed {
            return Err(ImapError::Closed);
        }
        let mut coroutine = coroutine;
        let mut arg: Option<Vec<u8>> = None;
        loop {
            match coroutine.resume(&mut self.fragmentizer, arg.as_deref()) {
                ImapCoroutineState::Complete(result) => {
                    if self.gate.poisoned() {
                        self.close().await;
                    }
                    return Ok(result);
                }
                ImapCoroutineState::Yielded(ImapYield::WantsRead) => {
                    let wire = self.wire.as_mut().ok_or(ImapError::Closed)?;
                    let bytes = match self.gate.read(wire).await {
                        Ok(bytes) => bytes,
                        Err(err) => {
                            self.close().await;
                            return Err(err);
                        }
                    };
                    arg = Some(bytes);
                }
                ImapCoroutineState::Yielded(ImapYield::WantsWrite(bytes)) => {
                    let wire = self.wire.as_mut().ok_or(ImapError::Closed)?;
                    if let Err(err) = write_command(wire, &bytes).await {
                        self.close().await;
                        return Err(ImapError::from_io(err));
                    }
                    arg = None;
                }
            }
        }
    }

    /// Marks the connection unusable and shuts the socket down.
    async fn close(&mut self) {
        self.closed = true;
        if let Some(wire) = self.wire.as_mut() {
            let _ = wire.shutdown().await;
        }
    }
}

async fn write_command(wire: &mut (impl AsyncWrite + Unpin), bytes: &[u8]) -> std::io::Result<()> {
    wire.write_all(bytes).await?;
    wire.flush().await
}

/// Whether the capability list advertises `wanted`.
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
