//! The static test identity for [`crate::imap`] servers offering
//! STARTTLS: a synthetic self-signed certificate with IP SAN
//! `127.0.0.1`, generated once with openssl and committed as a
//! fixture (spec/04's Bridge model). Expires 2126-10-08. Not a secret.

use std::io::{self, Read, Write};
use std::net::TcpStream;
use std::ops::{Deref, DerefMut};
use std::sync::Arc;
use std::time::Duration;

use rustls::pki_types::pem::PemObject;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, ServerName};
use rustls::{
    ClientConfig, ClientConnection, ConnectionCommon, RootCertStore, ServerConfig, SideData,
};

/// Longest a test TLS handshake may go without progress.
const HANDSHAKE_DEADLINE: Duration = Duration::from_secs(10);

/// The PEM certificate (chain of one).
pub const CERT_PEM: &str = include_str!("../certs/imap-localhost.pem");
/// The PEM private key (`BEGIN PRIVATE KEY`, PKCS#8).
pub const KEY_PEM: &str = include_str!("../certs/imap-localhost-key.pem");

/// The leaf certificate in DER form — what a pinning client compares.
pub fn cert_der() -> CertificateDer<'static> {
    CertificateDer::from_pem_slice(CERT_PEM.as_bytes())
        .expect("the committed test certificate parses")
}

/// A `rustls` server config presenting the test identity.
pub fn server_config() -> Arc<ServerConfig> {
    let certs = vec![cert_der()];
    let key =
        PrivateKeyDer::from_pem_slice(KEY_PEM.as_bytes()).expect("the committed test key parses");
    let config = ServerConfig::builder()
        .with_no_client_auth()
        .with_single_cert(certs, key)
        .expect("the committed test identity configures TLS");
    Arc::new(config)
}

/// The Bridge certificate model (spec/04, spec/10): the self-signed
/// test certificate is the single trust anchor — pinning with real
/// signature and expiry checks kept.
pub(crate) fn client_config() -> Arc<ClientConfig> {
    let mut roots = RootCertStore::empty();
    roots
        .add(cert_der())
        .expect("the committed test certificate is a trust anchor");
    let config = ClientConfig::builder()
        .with_root_certificates(roots)
        .with_no_client_auth();
    Arc::new(config)
}

/// Wraps a plain socket in the pinning client TLS — the other half of
/// a script's [`crate::Reply::UpgradeTls`]. `server` is the name
/// verified against the certificate (IP SAN `127.0.0.1`); a stalled
/// or closed peer fails instead of spinning. Caller's timeout is kept.
pub fn client_stream(stream: TcpStream, server: &str) -> io::Result<TlsIo<ClientConnection>> {
    client_stream_with(stream, server, client_config())
}

/// [`client_stream`] over a caller-built config — the verifier seam.
pub fn client_stream_with(
    stream: TcpStream,
    server: &str,
    config: Arc<ClientConfig>,
) -> io::Result<TlsIo<ClientConnection>> {
    let name = ServerName::try_from(server.to_string())
        .map_err(|e| io::Error::other(format!("bad server name {server}: {e}")))?;
    let mut conn = ClientConnection::new(config, name).map_err(io::Error::other)?;
    let caller_timeout = stream.read_timeout()?;
    stream.set_read_timeout(Some(HANDSHAKE_DEADLINE))?;
    let outcome = loop {
        if !conn.is_handshaking() {
            break Ok(());
        }
        match conn.complete_io(&mut &stream) {
            Ok((0, 0)) => break Err(io::Error::other("peer closed during the TLS handshake")),
            Ok(_) => {}
            Err(e)
                if e.kind() == io::ErrorKind::WouldBlock || e.kind() == io::ErrorKind::TimedOut =>
            {
                break Err(io::Error::new(
                    io::ErrorKind::TimedOut,
                    "test TLS handshake stalled",
                ));
            }
            Err(e) => break Err(e),
        }
    };
    stream.set_read_timeout(caller_timeout)?;
    outcome?;
    Ok(TlsIo::new(conn, stream))
}

/// A TLS session over one socket that drains decrypted plaintext
/// before touching the socket again: `rustls::Stream`'s `read` can
/// fail a `read` that already decrypted bytes, trapping them behind
/// a `WouldBlock`. Both TLS endpoints here run: buffered plaintext
/// first, one `complete_io`, buffered plaintext again, and only then
/// the socket's not-ready error.
pub struct TlsIo<C> {
    /// The rustls session, for callers inspecting peer certificates
    /// or driving `send_close_notify`.
    pub conn: C,
    sock: TcpStream,
}

impl<C> TlsIo<C> {
    pub(crate) fn new(conn: C, sock: TcpStream) -> Self {
        Self { conn, sock }
    }

    /// The wrapped socket — read timeouts and shutdowns take `&self`.
    pub fn sock(&self) -> &TcpStream {
        &self.sock
    }
}

impl<C, S> TlsIo<C>
where
    C: DerefMut + Deref<Target = ConnectionCommon<S>>,
    S: SideData,
{
    /// Buffered plaintext, or `Ok(0)` when the reader is empty —
    /// `conn.reader()` signals an empty buffer as `Err(WouldBlock)`.
    fn buffered(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        match self.conn.reader().read(buf) {
            Err(e) if e.kind() == io::ErrorKind::WouldBlock => Ok(0),
            out => out,
        }
    }
}

impl<C, S> Read for TlsIo<C>
where
    C: DerefMut + Deref<Target = ConnectionCommon<S>>,
    S: SideData,
{
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        loop {
            let read = self.buffered(buf)?;
            if read > 0 {
                return Ok(read);
            }
            match self.conn.complete_io(&mut self.sock) {
                // Nothing wanted either way: the peer sent
                // `close_notify` — a clean EOF.
                Ok((0, 0)) => return Ok(0),
                Ok(_) => {}
                Err(e)
                    if e.kind() == io::ErrorKind::WouldBlock
                        || e.kind() == io::ErrorKind::TimedOut =>
                {
                    // The failed poll may still have decrypted bytes
                    // into the reader; surface them before the error.
                    let read = self.buffered(buf)?;
                    return if read > 0 { Ok(read) } else { Err(e) };
                }
                Err(e) => return Err(e),
            }
        }
    }
}

impl<C, S> Write for TlsIo<C>
where
    C: DerefMut + Deref<Target = ConnectionCommon<S>>,
    S: SideData,
{
    fn write(&mut self, buf: &[u8]) -> io::Result<usize> {
        let written = self.conn.writer().write(buf)?;
        // `complete_io` with nothing to send waits for the peer instead.
        if self.conn.wants_write() {
            self.conn.complete_io(&mut self.sock)?;
        }
        Ok(written)
    }

    fn flush(&mut self) -> io::Result<()> {
        self.conn.writer().flush()?;
        if self.conn.wants_write() {
            self.conn.complete_io(&mut self.sock)?;
        }
        self.sock.flush()
    }
}
