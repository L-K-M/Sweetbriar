//! The per-account HTTPS client: honest identity, pacing, timeouts and
//! backoff (spec/04 "Connections and pacing", spec/10 "The Proton
//! API"). One client is one account; the connection pool keeps at most
//! one idle HTTP/1.1 connection per account.

use std::fmt;
use std::future::Future;
use std::pin::Pin;
use std::sync::Arc;
use std::task::{Context, Poll};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::header::{HeaderName, HeaderValue};
use hyper::{Method, Request, Response, StatusCode, Uri};
use hyper_util::client::legacy::Client as LegacyClient;
use hyper_util::client::legacy::connect::{Connected, Connection};
use hyper_util::rt::{TokioExecutor, TokioIo};
use proton_srp::{ModulusSignatureVerifier, ModulusVerifyError, RPGPVerifier};
use rustls::pki_types::{CertificateDer, ServerName};
use rustls::{ClientConfig, RootCertStore};
use rustls_platform_verifier::BuilderVerifierExt;
use tokio::net::TcpStream;
use tokio::sync::{OwnedSemaphorePermit, Semaphore};
use tokio_rustls::TlsConnector;
use tower_service::Service;

use crate::error::ProtonError;

/// The honest product identity (spec/04, spec/10): `Other` is accepted
/// by Proton; Sweetbriar never sends an official Proton client's string.
pub const APP_VERSION: &str = "Other";

/// One request's deadline (spec/04 "Connections and pacing").
pub const REQUEST_TIMEOUT: Duration = Duration::from_secs(30);
/// At most two requests in flight per account: the worker and one body
/// fetch (spec/04).
pub const MAX_IN_FLIGHT: usize = 2;
/// Background requests are paced to at most four per second (spec/04).
pub const BACKGROUND_INTERVAL: Duration = Duration::from_millis(250);
/// `Retry-After` is clamped to [1 s, 1 h] (spec/04).
pub const RETRY_AFTER_MIN: Duration = Duration::from_secs(1);
pub const RETRY_AFTER_MAX: Duration = Duration::from_secs(3_600);
/// Network errors back off exponentially from 1 s to 5 min (spec/04).
pub const BACKOFF_MIN: Duration = Duration::from_secs(1);
pub const BACKOFF_MAX: Duration = Duration::from_secs(300);
/// How many times a request retries a network error or a 5xx.
pub const MAX_NETWORK_RETRIES: u32 = 5;
/// How many times a request waits out a 429/503.
pub const MAX_RATE_RETRIES: u32 = 3;

/// What made the client wait most recently (spec/04's table); cleared
/// at the start of every send, so it describes the finished request.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BackoffKind {
    /// A 429 or 503 carried `Retry-After` and the client waited it.
    RateLimit,
    /// A 5xx answer was retried with the exponential backoff.
    ServerError,
    /// A connect failure, drop or timeout was retried with the
    /// exponential backoff.
    Network,
}

/// Told before each retry sleep, so the sync worker can show the
/// account's condition and next attempt while the wait runs (spec/07
/// `account_health`).
pub type BackoffObserver = Arc<dyn Fn(BackoffKind, Duration) + Send + Sync>;

/// How a request is queued. Interactive requests (opening a message) go
/// first: they skip the background rate limit; background requests are
/// paced to four per second.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    Interactive,
    Background,
}

/// The trust anchors for the API host. Production uses the platform
/// verifier (spec/10); tests pass explicit roots.
#[derive(Clone)]
pub enum Trust {
    /// The system trust store, hostname checked (spec/10).
    Platform,
    /// Explicit DER roots; used only by tests.
    Roots(Vec<CertificateDer<'static>>),
}

impl fmt::Debug for Trust {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            Self::Platform => f.write_str("Platform"),
            Self::Roots(roots) => f.debug_tuple("Roots").field(&roots.len()).finish(),
        }
    }
}

/// A client configuration: the account's API host, the macOS version
/// for the honest `User-Agent`, the trust anchors and the modulus
/// verifier (spec/10).
pub struct Config {
    pub base_url: String,
    pub macos_version: String,
    pub trust: Trust,
    pub modulus_verifier: Arc<dyn ModulusSignatureVerifier + Send + Sync>,
}

impl Config {
    /// Production defaults: the platform verifier and Proton's embedded
    /// modulus key (spec/10).
    pub fn new(
        base_url: impl Into<String>,
        macos_version: impl Into<String>,
        trust: Trust,
    ) -> Self {
        Self {
            base_url: base_url.into(),
            macos_version: macos_version.into(),
            trust,
            modulus_verifier: Arc::new(RPGPVerifier::default()),
        }
    }

    /// Replaces the modulus verifier. Tests inject one that trusts the
    /// fake server's test key; production never calls this.
    pub fn with_modulus_verifier(
        mut self,
        verifier: Arc<dyn ModulusSignatureVerifier + Send + Sync>,
    ) -> Self {
        self.modulus_verifier = verifier;
        self
    }

    /// The honest `User-Agent` (spec/10): the crate version and the
    /// caller-supplied macOS version, nothing else.
    fn user_agent(&self) -> String {
        format!(
            "Sweetbriar/{} (macOS {})",
            env!("CARGO_PKG_VERSION"),
            self.macos_version
        )
    }
}

impl fmt::Debug for Config {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Config")
            .field("base_url", &self.base_url)
            .field("macos_version", &self.macos_version)
            .field("trust", &self.trust)
            .finish_non_exhaustive()
    }
}

/// The exponential network backoff: 1 s, 2 s, 4 s, ... capped at 5 min
/// (spec/04). `attempt` counts from 1 (the first retry).
pub fn backoff_delay(attempt: u32) -> Duration {
    let shift = attempt.saturating_sub(1).min(9);
    let millis = BACKOFF_MIN.as_millis() as u64 * (1u64 << shift);
    Duration::from_millis(millis.min(BACKOFF_MAX.as_millis() as u64))
}

/// Clamps a `Retry-After` delta to [1 s, 1 h] (spec/04). A missing or
/// unparsable header answers the 1 s floor.
pub fn clamp_retry_after(retry_after: Option<&HeaderValue>) -> Duration {
    let seconds = retry_after
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok());
    match seconds {
        Some(secs) => {
            Duration::from_secs(secs.clamp(RETRY_AFTER_MIN.as_secs(), RETRY_AFTER_MAX.as_secs()))
        }
        None => RETRY_AFTER_MIN,
    }
}

/// `SRPAuth::new` takes `&impl ModulusSignatureVerifier` (Sized), so the
/// shared config verifier is wrapped rather than passed as a trait
/// object reference.
pub(crate) struct SharedVerifier(pub Arc<dyn ModulusSignatureVerifier + Send + Sync>);

impl ModulusSignatureVerifier for SharedVerifier {
    fn verify_and_extract_modulus(
        &self,
        modulus: &str,
        server_key: &str,
    ) -> Result<String, ModulusVerifyError> {
        self.0.verify_and_extract_modulus(modulus, server_key)
    }
}

#[derive(Default)]
struct RateState {
    last_background: Option<tokio::time::Instant>,
}

/// One account's client.
pub struct Client {
    inner: LegacyClient<HttpsConnector, Full<Bytes>>,
    pub(crate) config: Arc<Config>,
    semaphore: Arc<Semaphore>,
    rate: Arc<tokio::sync::Mutex<RateState>>,
    /// What the last send waited for; read by the worker after a
    /// failure to classify the retry.
    last_backoff: Arc<std::sync::Mutex<Option<BackoffKind>>>,
    /// Told before each retry sleep.
    backoff_observer: Arc<std::sync::Mutex<Option<BackoffObserver>>>,
}

impl fmt::Debug for Client {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.debug_struct("Client")
            .field("config", &self.config)
            .finish_non_exhaustive()
    }
}

impl Clone for Client {
    fn clone(&self) -> Self {
        Self {
            inner: self.inner.clone(),
            config: self.config.clone(),
            semaphore: self.semaphore.clone(),
            rate: self.rate.clone(),
            last_backoff: Arc::clone(&self.last_backoff),
            backoff_observer: Arc::clone(&self.backoff_observer),
        }
    }
}

impl Client {
    /// Builds the client: the TLS configuration (platform or explicit
    /// roots) and the pooling HTTP/1.1 transport.
    pub fn new(config: Config) -> Result<Self, ProtonError> {
        let tls = tls_config(&config.trust)?;
        let connector = HttpsConnector {
            tls: TlsConnector::from(tls),
        };
        let inner = LegacyClient::builder(TokioExecutor::new())
            .pool_max_idle_per_host(1)
            .build(connector);
        Ok(Self {
            inner,
            config: Arc::new(config),
            semaphore: Arc::new(Semaphore::new(MAX_IN_FLIGHT)),
            rate: Arc::new(tokio::sync::Mutex::new(RateState::default())),
            last_backoff: Arc::new(std::sync::Mutex::new(None)),
            backoff_observer: Arc::new(std::sync::Mutex::new(None)),
        })
    }

    /// Replace the retry observer (spec/07 `account_health`); one
    /// client belongs to one account's worker, which sets it once.
    pub fn set_backoff_observer(&self, observer: Option<BackoffObserver>) {
        *self
            .backoff_observer
            .lock()
            .expect("the backoff observer lock poisoned") = observer;
    }

    /// What the last send waited for, or `None` when it never waited
    /// (or succeeded at once). The worker classifies its retry from it.
    pub fn last_backoff(&self) -> Option<BackoffKind> {
        *self.last_backoff.lock().expect("the backoff lock poisoned")
    }

    /// `GET /tests/ping`: the reachability probe (research
    /// `auth-session.md` §1).
    pub async fn ping(&self) -> Result<(), ProtonError> {
        let response = self
            .send(
                Method::GET,
                "/tests/ping",
                None,
                Priority::Interactive,
                None,
                &[],
            )
            .await?;
        let (status, body) = read_response(response).await?;
        expect_success(status, &body)
    }

    /// Sends one request with pacing, the 30 s deadline, `Retry-After`
    /// and exponential backoff. `auth` adds the session headers; the
    /// `extra` headers are the human-verification retry headers.
    pub(crate) async fn send(
        &self,
        method: Method,
        path: &str,
        body: Option<Bytes>,
        priority: Priority,
        auth: Option<(&str, &str)>,
        extra: &[(HeaderName, HeaderValue)],
    ) -> Result<Answer, ProtonError> {
        if priority == Priority::Background {
            self.wait_background_slot().await;
        }
        let permit = Arc::clone(&self.semaphore)
            .acquire_owned()
            .await
            .map_err(|_| ProtonError::Unavailable {
                message: "the client is closed".to_owned(),
            })?;

        let request = self.build_request(&method, path, body, auth, extra)?;
        // The classification describes this send only.
        *self.last_backoff.lock().expect("the backoff lock poisoned") = None;
        let mut network_attempt = 0;
        let mut rate_attempt = 0;
        loop {
            match tokio::time::timeout(REQUEST_TIMEOUT, self.inner.request(request.clone())).await {
                Ok(Ok(response)) => {
                    let status = response.status();
                    if status == StatusCode::TOO_MANY_REQUESTS
                        || status == StatusCode::SERVICE_UNAVAILABLE
                    {
                        let retry_after = clamp_retry_after(response.headers().get("retry-after"));
                        let (_, body) = read_body(response).await?;
                        if rate_attempt >= MAX_RATE_RETRIES {
                            return Err(ProtonError::Unavailable {
                                message: error_message(&body),
                            });
                        }
                        rate_attempt += 1;
                        log::debug!(
                            "Proton asked us to slow down ({}); waiting {:?} (retry {}/{})",
                            status,
                            retry_after,
                            rate_attempt,
                            MAX_RATE_RETRIES
                        );
                        self.wait_backoff(BackoffKind::RateLimit, retry_after);
                        tokio::time::sleep(retry_after).await;
                        continue;
                    }
                    if status.is_server_error() {
                        let (_, body) = read_body(response).await?;
                        // A server error answered a request that reached
                        // Proton, so only an idempotent one is sent again.
                        if !may_retry(&method, true) || network_attempt >= MAX_NETWORK_RETRIES {
                            return Err(ProtonError::Unavailable {
                                message: error_message(&body),
                            });
                        }
                        network_attempt += 1;
                        let delay = backoff_delay(network_attempt);
                        self.wait_backoff(BackoffKind::ServerError, delay);
                        log::debug!(
                            "Proton returned {}; backing off {:?} (retry {}/{})",
                            status,
                            delay,
                            network_attempt,
                            MAX_NETWORK_RETRIES
                        );
                        tokio::time::sleep(delay).await;
                        continue;
                    }
                    return Ok(Answer {
                        response,
                        _permit: permit,
                    });
                }
                Ok(Err(error)) => {
                    if !may_retry(&method, !error.is_connect())
                        || network_attempt >= MAX_NETWORK_RETRIES
                    {
                        return Err(ProtonError::Unavailable {
                            message: error.to_string(),
                        });
                    }
                    network_attempt += 1;
                    let delay = backoff_delay(network_attempt);
                    self.wait_backoff(BackoffKind::Network, delay);
                    tokio::time::sleep(delay).await;
                }
                Err(_) => {
                    if !may_retry(&method, true) || network_attempt >= MAX_NETWORK_RETRIES {
                        return Err(ProtonError::Timeout);
                    }
                    network_attempt += 1;
                    let delay = backoff_delay(network_attempt);
                    self.wait_backoff(BackoffKind::Network, delay);
                    tokio::time::sleep(delay).await;
                }
            }
        }
    }

    /// Record the wait and tell the observer, before sleeping (spec/07
    /// `account_health`).
    fn wait_backoff(&self, kind: BackoffKind, delay: Duration) {
        *self.last_backoff.lock().expect("the backoff lock poisoned") = Some(kind);
        let observer = self
            .backoff_observer
            .lock()
            .expect("the backoff observer lock poisoned")
            .clone();
        if let Some(observer) = observer {
            observer(kind, delay);
        }
    }

    /// Background requests are paced to at most four per second. The
    /// rate lock is held across the wait, so concurrent background
    /// requests queue at 250 ms intervals.
    async fn wait_background_slot(&self) {
        let mut state = self.rate.lock().await;
        let now = tokio::time::Instant::now();
        let next = state
            .last_background
            .map(|last| last + BACKGROUND_INTERVAL)
            .unwrap_or(now);
        if next > now {
            tokio::time::sleep_until(next).await;
        }
        state.last_background = Some(tokio::time::Instant::now());
    }

    /// Builds a request with the honest identity headers on every call
    /// (spec/10). `x-pm-appversion` and `User-Agent` are always sent;
    /// the session headers only for authenticated calls.
    pub(crate) fn build_request(
        &self,
        method: &Method,
        path: &str,
        body: Option<Bytes>,
        auth: Option<(&str, &str)>,
        extra: &[(HeaderName, HeaderValue)],
    ) -> Result<Request<Full<Bytes>>, ProtonError> {
        let uri: Uri = format!("{}{}", self.config.base_url, path)
            .parse()
            .map_err(|e| ProtonError::Malformed {
                message: format!("bad request URI: {e}"),
            })?;
        let mut builder = Request::builder()
            .method(method.clone())
            .uri(uri)
            .header("x-pm-appversion", APP_VERSION)
            .header("user-agent", self.config.user_agent());
        if body.is_some() {
            builder = builder.header("content-type", "application/json");
        }
        if let Some((uid, access_token)) = auth {
            builder = builder
                .header("x-pm-uid", uid)
                .header("authorization", format!("Bearer {access_token}"));
        }
        for (name, value) in extra {
            builder = builder.header(name, value);
        }
        let body = body.unwrap_or_default();
        builder
            .body(Full::new(body))
            .map_err(|e| ProtonError::Malformed {
                message: format!("bad request: {e}"),
            })
    }
}

/// Reads a response body to bytes. A body that fails mid-read is a
/// failed request, never a partial observation (spec/04).
/// The in-flight permit is released once the body is read.
pub(crate) async fn read_response(answer: Answer) -> Result<(StatusCode, Bytes), ProtonError> {
    read_body(answer.response).await
}

/// Reads a response body to bytes under a hard size bound, frame by
/// frame, so an oversized body is refused before it is all buffered:
/// a truncated return is never mistaken for the whole packet (spec/04
/// step 3). The in-flight permit is released once the body is read or
/// the bound is breached.
pub(crate) async fn read_response_bounded(
    answer: Answer,
    limit: u64,
) -> Result<(StatusCode, Vec<u8>), ProtonError> {
    let status = answer.response.status();
    let mut body = answer.response.into_body();
    let mut total = 0u64;
    let mut bytes = Vec::new();
    // Each frame must arrive within the request timeout, and the whole
    // body within BOUNDED_READ_TIMEOUT, so a server dribbling frames
    // can't hold the in-flight permit forever.
    let deadline = tokio::time::Instant::now() + BOUNDED_READ_TIMEOUT;
    loop {
        let frame_deadline = (tokio::time::Instant::now() + REQUEST_TIMEOUT).min(deadline);
        let frame = match tokio::time::timeout_at(frame_deadline, body.frame()).await {
            Ok(Some(frame)) => frame,
            Ok(None) => break,
            Err(_) => return Err(ProtonError::Timeout),
        };
        let data = frame
            .map_err(|e| ProtonError::Unavailable {
                message: format!("the response body didn't read: {e}"),
            })?
            .into_data()
            .map_err(|_| ProtonError::Unavailable {
                message: "the response body carried a trailer instead of data".to_owned(),
            })?;
        total = total.saturating_add(data.len() as u64);
        if total > limit {
            return Err(ProtonError::AttachmentTooLarge);
        }
        bytes.extend_from_slice(&data);
    }
    Ok((status, bytes))
}

/// The whole-body deadline of a bounded read: enough for the largest
/// attachment over a slow link (30 MiB at 100 KiB/s takes 307 s).
pub(crate) const BOUNDED_READ_TIMEOUT: Duration = Duration::from_secs(6 * 60);

/// A response with its in-flight permit. The permit is held until the
/// body has been read or the response dropped, so a body still
/// streaming counts against [`MAX_IN_FLIGHT`] (spec/04).
pub(crate) struct Answer {
    response: Response<Incoming>,
    _permit: OwnedSemaphorePermit,
}

/// Whether a failed attempt may be sent again (spec/04 "Backoff"). One
/// that may have reached Proton (a timeout, or a connection lost after
/// it was sent) is retried only when its method is idempotent, so a
/// lost answer never replays a sign-in or refresh.
fn may_retry(method: &Method, may_have_reached: bool) -> bool {
    !may_have_reached || method.is_idempotent()
}

/// Reads a body within the request deadline, so a stalled body can't
/// hold an in-flight permit for ever.
async fn read_body(response: Response<Incoming>) -> Result<(StatusCode, Bytes), ProtonError> {
    let status = response.status();
    let collected = tokio::time::timeout(REQUEST_TIMEOUT, response.into_body().collect())
        .await
        .map_err(|_| ProtonError::Timeout)?;
    let bytes = collected
        .map_err(|e| ProtonError::Unavailable {
            message: format!("the response body didn't read: {e}"),
        })?
        .to_bytes();
    Ok((status, bytes))
}

/// Proton's `Error` text from a JSON body, or a static fallback.
pub(crate) fn error_message(body: &[u8]) -> String {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()
        .and_then(|value| {
            value
                .get("Error")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "Proton refused the request".to_owned())
}

/// The numeric `Code` from a JSON body.
pub(crate) fn code_of(body: &[u8]) -> Option<i64> {
    serde_json::from_slice::<serde_json::Value>(body)
        .ok()?
        .get("Code")?
        .as_i64()
}

/// Checks a success response: HTTP 2xx and `Code` 1000/1001.
pub(crate) fn expect_success(status: StatusCode, body: &[u8]) -> Result<(), ProtonError> {
    let code = code_of(body).unwrap_or(0);
    let message = error_message(body);
    if status == StatusCode::UNAUTHORIZED {
        return Err(ProtonError::Unauthorized);
    }
    if status.is_success() && matches!(code, 1000 | 1001) {
        return Ok(());
    }
    match crate::error::ApiCode::from_i64(code) {
        Some(api) => {
            Err(ProtonError::from_api(api, &message)
                .unwrap_or(ProtonError::Unavailable { message }))
        }
        None => Err(ProtonError::Unavailable { message }),
    }
}

fn tls_config(trust: &Trust) -> Result<Arc<ClientConfig>, ProtonError> {
    match trust {
        Trust::Platform => ClientConfig::builder()
            .with_platform_verifier()
            .map_err(|e| ProtonError::Unavailable {
                message: format!("the platform TLS verifier is unavailable: {e}"),
            })
            .map(|builder| Arc::new(builder.with_no_client_auth())),
        Trust::Roots(roots) => {
            let mut store = RootCertStore::empty();
            for root in roots {
                store
                    .add(root.clone())
                    .map_err(|e| ProtonError::Unavailable {
                        message: format!("a configured trust root is invalid: {e}"),
                    })?;
            }
            Ok(Arc::new(
                ClientConfig::builder()
                    .with_root_certificates(store)
                    .with_no_client_auth(),
            ))
        }
    }
}

/// The TLS connector: one HTTPS connection per account, SNI and
/// hostname checked by rustls.
#[derive(Clone)]
struct HttpsConnector {
    tls: TlsConnector,
}

impl Service<Uri> for HttpsConnector {
    type Response = TlsConn;
    type Error = Box<dyn std::error::Error + Send + Sync>;
    type Future = Pin<Box<dyn Future<Output = Result<Self::Response, Self::Error>> + Send>>;

    fn poll_ready(&mut self, _cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        Poll::Ready(Ok(()))
    }

    fn call(&mut self, uri: Uri) -> Self::Future {
        let tls = self.tls.clone();
        Box::pin(async move {
            let host = uri.host().ok_or("the URI has no host")?.to_string();
            let port = uri.port_u16().unwrap_or(443);
            let tcp = TcpStream::connect((host.as_str(), port)).await?;
            let _ = tcp.set_nodelay(true);
            let name = ServerName::try_from(host)?;
            let stream = tls.connect(name, tcp).await?;
            Ok(TlsConn(TokioIo::new(stream)))
        })
    }
}

/// A TLS stream that hyper's legacy client can pool.
struct TlsConn(TokioIo<tokio_rustls::client::TlsStream<TcpStream>>);

impl Connection for TlsConn {
    fn connected(&self) -> Connected {
        Connected::new()
    }
}

impl hyper::rt::Read for TlsConn {
    fn poll_read(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: hyper::rt::ReadBufCursor<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.get_mut().0).poll_read(cx, buf)
    }
}

impl hyper::rt::Write for TlsConn {
    fn poll_write(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
        buf: &[u8],
    ) -> Poll<Result<usize, std::io::Error>> {
        Pin::new(&mut self.get_mut().0).poll_write(cx, buf)
    }

    fn poll_flush(self: Pin<&mut Self>, cx: &mut Context<'_>) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.get_mut().0).poll_flush(cx)
    }

    fn poll_shutdown(
        self: Pin<&mut Self>,
        cx: &mut Context<'_>,
    ) -> Poll<Result<(), std::io::Error>> {
        Pin::new(&mut self.get_mut().0).poll_shutdown(cx)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sb_test_support::{FakeProtonServer, Fault, cert_der};

    fn test_client(server: &FakeProtonServer) -> Client {
        let config = Config::new(server.base_url(), "15.1", Trust::Roots(vec![cert_der()]));
        Client::new(config).expect("the test client configures")
    }

    #[tokio::test]
    async fn an_unread_body_keeps_its_in_flight_slot() {
        let server = FakeProtonServer::serve(Vec::new()).await.expect("serve");
        let client = test_client(&server);
        let send = || {
            client.send(
                Method::GET,
                "/tests/ping",
                None,
                Priority::Interactive,
                None,
                &[],
            )
        };
        let first = send().await.expect("the first request answers");
        let second = send().await.expect("the second request answers");
        assert_eq!(client.semaphore.available_permits(), 0);
        let third = tokio::time::timeout(Duration::from_millis(200), send()).await;
        assert!(
            third.is_err(),
            "a third request waits while two bodies are unread"
        );

        read_response(first).await.expect("the body reads");
        assert_eq!(client.semaphore.available_permits(), 1);
        drop(second);
        assert_eq!(client.semaphore.available_permits(), MAX_IN_FLIGHT);
    }

    #[tokio::test]
    async fn a_dropped_post_is_not_sent_again() {
        let server = FakeProtonServer::serve(Vec::new()).await.expect("serve");
        server.set_fault(Some(Fault::Drop));
        let client = test_client(&server);
        let result = client
            .send(
                Method::POST,
                "/tests/ping",
                Some(Bytes::from_static(b"{}")),
                Priority::Interactive,
                None,
                &[],
            )
            .await;
        assert!(matches!(result, Err(ProtonError::Unavailable { .. })));
        assert_eq!(server.requests().len(), 1, "the POST went out once");
    }

    #[tokio::test]
    async fn a_post_answered_with_a_server_error_is_not_sent_again() {
        // A 5xx answer reached Proton, so a sign-in or refresh must not be
        // replayed (spec/04 "Backoff").
        let server = FakeProtonServer::serve(Vec::new()).await.expect("serve");
        server.set_fault(Some(Fault::Code {
            status: 500,
            code: 500,
        }));
        let client = test_client(&server);
        let result = client
            .send(
                Method::POST,
                "/tests/ping",
                Some(Bytes::from_static(b"{}")),
                Priority::Interactive,
                None,
                &[],
            )
            .await;
        assert!(matches!(result, Err(ProtonError::Unavailable { .. })));
        assert_eq!(server.requests().len(), 1, "the POST went out once");
    }

    // Real time: a paused clock can auto-advance past the request
    // deadline while the fake server's task waits.
    #[tokio::test]
    async fn a_retry_is_observable_before_the_wait() {
        let server = FakeProtonServer::serve(Vec::new()).await.expect("serve");
        server.set_fault(Some(Fault::RateLimit {
            status: 429,
            retry_after: Some(1),
        }));
        let client = test_client(&server);
        let seen = Arc::new(std::sync::Mutex::new(Vec::new()));
        let recorded = Arc::clone(&seen);
        client.set_backoff_observer(Some(Arc::new(move |kind, delay| {
            recorded
                .lock()
                .expect("the observed waits lock poisoned")
                .push((kind, delay));
        })));
        let _ = client
            .send(
                Method::GET,
                "/tests/ping",
                None,
                Priority::Interactive,
                None,
                &[],
            )
            .await;
        let seen = seen.lock().expect("the observed waits lock poisoned");
        assert_eq!(seen.len(), 3, "three waits, then the refusal");
        assert!(seen.iter().all(
            |(kind, delay)| *kind == BackoffKind::RateLimit && *delay == Duration::from_secs(1)
        ));
        assert_eq!(client.last_backoff(), Some(BackoffKind::RateLimit));
    }

    #[test]
    fn only_idempotent_requests_are_retried_once_they_may_have_reached_proton() {
        assert!(may_retry(&Method::POST, false));
        assert!(!may_retry(&Method::POST, true));
        assert!(may_retry(&Method::GET, true));
        assert!(may_retry(&Method::DELETE, true));
    }

    #[test]
    fn backoff_grows_from_one_second_and_caps_at_five_minutes() {
        assert_eq!(backoff_delay(1), Duration::from_secs(1));
        assert_eq!(backoff_delay(2), Duration::from_secs(2));
        assert_eq!(backoff_delay(3), Duration::from_secs(4));
        assert_eq!(backoff_delay(4), Duration::from_secs(8));
        assert_eq!(backoff_delay(5), Duration::from_secs(16));
        assert_eq!(backoff_delay(6), Duration::from_secs(32));
        assert_eq!(backoff_delay(7), Duration::from_secs(64));
        assert_eq!(backoff_delay(8), Duration::from_secs(128));
        assert_eq!(backoff_delay(9), Duration::from_secs(256));
        assert_eq!(backoff_delay(10), BACKOFF_MAX);
        assert_eq!(backoff_delay(u32::MAX), BACKOFF_MAX);
    }

    #[test]
    fn retry_after_is_clamped_to_one_second_and_one_hour() {
        let header = |value: &str| HeaderValue::from_str(value).unwrap();
        assert_eq!(clamp_retry_after(None), Duration::from_secs(1));
        assert_eq!(
            clamp_retry_after(Some(&header("0"))),
            Duration::from_secs(1)
        );
        assert_eq!(
            clamp_retry_after(Some(&header("7"))),
            Duration::from_secs(7)
        );
        assert_eq!(
            clamp_retry_after(Some(&header("100000"))),
            Duration::from_secs(3_600)
        );
        assert_eq!(
            clamp_retry_after(Some(&header("not-a-number"))),
            Duration::from_secs(1)
        );
    }

    #[tokio::test(start_paused = true)]
    async fn background_slots_are_paced_to_four_per_second() {
        let client = Client::new(Config::new(
            "https://127.0.0.1:1",
            "15.1",
            Trust::Roots(vec![]),
        ))
        .expect("the client configures");
        let start = tokio::time::Instant::now();
        for _ in 0..4 {
            client.wait_background_slot().await;
        }
        let elapsed = start.elapsed();
        assert!(
            elapsed >= Duration::from_millis(750),
            "four background slots took {elapsed:?}"
        );
    }

    #[test]
    fn config_debug_and_user_agent_are_honest() {
        let config = Config::new("https://127.0.0.1:1", "15.1", Trust::Platform);
        let agent = config.user_agent();
        assert_eq!(agent, "Sweetbriar/0.0.1 (macOS 15.1)");
        assert!(!format!("{config:?}").contains("RPGPVerifier"));
    }
}
