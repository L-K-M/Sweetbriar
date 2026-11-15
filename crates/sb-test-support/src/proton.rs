//! The fake Proton API server (spec/11 "The fake Proton API server"):
//! an in-process HTTPS server on 127.0.0.1 that models the transport
//! surface Sweetbriar's client drives: `/tests/ping`, fault knobs and a
//! request log. It uses the same committed test certificate as the IMAP
//! fake (IP SAN 127.0.0.1), so a test client trusts it through
//! [`Trust::Roots`](sb_proton::Trust::Roots).
//!
//! This slice is the generic machinery only: the auth half (SRP sign-in,
//! sessions, refresh) arrives with the SRP step. Written from the
//! protocol facts in research/proton-api; no Proton or Bridge code. Every
//! account and token here is synthetic.

use std::io;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use bytes::Bytes;
use http_body_util::{BodyExt, Full};
use hyper::body::Incoming;
use hyper::{Request, Response, StatusCode};
use hyper_util::rt::TokioIo;
use serde_json::{Value, json};
use tokio::net::TcpListener;
use tokio::task::JoinHandle;
use tokio_rustls::TlsAcceptor;

use crate::imap_cert::server_config;

/// A synthetic test account. The password is a test value, never an
/// owner's.
#[derive(Debug, Clone)]
pub struct TestAccount {
    pub username: String,
    pub password: String,
}

impl TestAccount {
    pub fn new(username: &str, password: &str) -> Self {
        Self {
            username: username.to_owned(),
            password: password.to_owned(),
        }
    }
}

/// One logged request. The log deliberately records only header
/// presence and the honest identity values: never the UID or the bearer
/// token (spec/10).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecordedRequest {
    pub method: String,
    pub path: String,
    pub app_version: Option<String>,
    pub user_agent: Option<String>,
    pub content_type: Option<String>,
    pub has_uid: bool,
    pub has_authorization: bool,
}

/// A fault knob. `Drop` closes the connection; `Stall` delays every
/// response; `RateLimit` answers 429/503 with an optional
/// `Retry-After`; `Code` answers any status and Proton code (including
/// 401); `Once` applies one request only.
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
    /// Applies to the next request only, then clears itself.
    Once(Box<Fault>),
}

/// The in-process fake. Drop aborts the accept loop.
pub struct FakeProtonServer {
    base_url: String,
    log: Arc<Mutex<Vec<RecordedRequest>>>,
    faults: Arc<Mutex<Option<Fault>>>,
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
    /// Binds `127.0.0.1:0` and serves HTTPS.
    pub async fn serve() -> io::Result<Self> {
        let listener = TcpListener::bind(("127.0.0.1", 0)).await?;
        let port = listener.local_addr()?.port();

        let log = Arc::new(Mutex::new(Vec::new()));
        let faults = Arc::new(Mutex::new(None));
        let in_flight = Arc::new(AtomicUsize::new(0));
        let max_in_flight = Arc::new(AtomicUsize::new(0));
        let acceptor = TlsAcceptor::from(server_config());
        let handle = tokio::spawn({
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
                    let acceptor = acceptor.clone();
                    let log = Arc::clone(&log);
                    let faults = Arc::clone(&faults);
                    let in_flight = Arc::clone(&in_flight);
                    let max_in_flight = Arc::clone(&max_in_flight);
                    tokio::spawn(async move {
                        let tls = match acceptor.accept(tcp).await {
                            Ok(tls) => tls,
                            Err(_) => return,
                        };
                        let service = hyper::service::service_fn(move |request| {
                            handle_request(
                                request,
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
            max_in_flight,
            handle,
        })
    }

    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    /// Every request the server has seen, in order.
    pub fn requests(&self) -> Vec<RecordedRequest> {
        self.log.lock().unwrap_or_else(|e| e.into_inner()).clone()
    }

    pub fn set_fault(&self, fault: Option<Fault>) {
        *self.faults.lock().unwrap_or_else(|e| e.into_inner()) = fault;
    }

    /// The largest number of requests being handled at once.
    pub fn max_in_flight(&self) -> usize {
        self.max_in_flight.load(Ordering::SeqCst)
    }
}

impl Drop for FakeProtonServer {
    fn drop(&mut self) {
        self.handle.abort();
    }
}

async fn handle_request(
    request: Request<Incoming>,
    log: Arc<Mutex<Vec<RecordedRequest>>>,
    faults: Arc<Mutex<Option<Fault>>>,
    in_flight: Arc<AtomicUsize>,
    max_in_flight: Arc<AtomicUsize>,
) -> Result<Response<Full<Bytes>>, io::Error> {
    let method = request.method().clone();
    let path = request.uri().path().to_owned();
    let headers = request.headers().clone();
    log.lock()
        .unwrap_or_else(|e| e.into_inner())
        .push(RecordedRequest {
            method: method.as_str().to_owned(),
            path: path.clone(),
            app_version: header_str(&headers, "x-pm-appversion"),
            user_agent: header_str(&headers, "user-agent"),
            content_type: header_str(&headers, "content-type"),
            has_uid: headers.contains_key("x-pm-uid"),
            has_authorization: headers.contains_key("authorization"),
        });
    let now = in_flight.fetch_add(1, Ordering::SeqCst) + 1;
    max_in_flight.fetch_max(now, Ordering::SeqCst);
    let outcome = dispatch(request, &faults, &method, &path).await;
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
    faults: &Arc<Mutex<Option<Fault>>>,
    method: &hyper::Method,
    path: &str,
) -> Result<Response<Full<Bytes>>, io::Error> {
    let fault = faults.lock().unwrap_or_else(|e| e.into_inner()).clone();
    let fault = match fault {
        Some(Fault::Once(inner)) => {
            *faults.lock().unwrap_or_else(|e| e.into_inner()) = None;
            Some(*inner)
        }
        other => other,
    };
    if let Some(fault) = fault {
        match fault {
            Fault::Drop => {
                return Err(io::Error::new(io::ErrorKind::ConnectionAborted, "drop"));
            }
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
            // `Once` is unwrapped into its inner fault before the match.
            Fault::Once(_) => {}
        }
    }

    let _body = request
        .into_body()
        .collect()
        .await
        .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, e))?
        .to_bytes();
    match (method.as_str(), path) {
        ("GET", "/tests/ping") => Ok(json_response(200, json!({"Code": 1000}))),
        _ => Ok(json_response(
            404,
            json!({"Code": 2001, "Error": "not found"}),
        )),
    }
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
