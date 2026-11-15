//! Acceptance tests for M2.11a's transport slice against the in-process
//! fake Proton API server (spec/11). No test contacts a live Proton
//! endpoint.

use std::io::{Read, Write};
use std::time::Duration;

use sb_proton::{ApiCode, Client, Config, ProtonError, Trust};
use sb_test_support::{FakeProtonServer, Fault, cert_der, client_stream};

const MACOS_VERSION: &str = "15.1";

fn client(server: &FakeProtonServer) -> Client {
    let config = Config::new(
        server.base_url(),
        MACOS_VERSION,
        Trust::Roots(vec![cert_der()]),
    );
    Client::new(config).expect("the test client configures")
}

async fn server() -> FakeProtonServer {
    FakeProtonServer::serve().await.expect("serve")
}

#[tokio::test]
async fn every_request_carries_the_honest_identity() {
    let server = server().await;
    let client = client(&server);
    client.ping().await.expect("ping");
    client.ping().await.expect("ping");

    let requests = server.requests();
    assert_eq!(requests.len(), 2);
    for request in &requests {
        assert_eq!(request.app_version.as_deref(), Some("Other"));
        assert_eq!(
            request.user_agent.as_deref(),
            Some("Sweetbriar/0.0.1 (macOS 15.1)")
        );
        assert_eq!(request.path, "/tests/ping");
    }
}

#[tokio::test]
async fn retry_after_is_honoured() {
    // Real time: a paused clock does not drive the real socket I/O the
    // fake server needs, so the retry wait is measured on the wall clock.
    let server = server().await;
    server.set_fault(Some(Fault::Once(Box::new(Fault::RateLimit {
        status: 429,
        retry_after: Some(2),
    }))));
    let client = client(&server);
    let start = std::time::Instant::now();
    client.ping().await.expect("the retry succeeds");
    let elapsed = start.elapsed();
    assert!(elapsed >= Duration::from_secs(2), "elapsed {elapsed:?}");
    assert_eq!(server.requests().len(), 2, "one 429 and one success");
}

#[tokio::test]
async fn retry_after_is_clamped_before_it_reaches_the_wire() {
    // The pure clamp is unit-tested; here a 0-second Retry-After still
    // waits the 1 s floor before the retry.
    let server = server().await;
    server.set_fault(Some(Fault::Once(Box::new(Fault::RateLimit {
        status: 503,
        retry_after: Some(0),
    }))));
    let client = client(&server);
    let start = std::time::Instant::now();
    client.ping().await.expect("the retry succeeds");
    assert!(
        start.elapsed() >= Duration::from_secs(1),
        "a 0 s Retry-After must wait the 1 s floor"
    );
}

#[tokio::test(start_paused = true)]
async fn a_stalled_request_times_out_at_30s() {
    let server = server().await;
    server.set_fault(Some(Fault::Stall(Duration::from_secs(60))));
    let client = client(&server);
    let start = tokio::time::Instant::now();
    let error = client.ping().await.expect_err("the stall times out");
    assert!(matches!(error, ProtonError::Timeout));
    assert!(
        start.elapsed() >= Duration::from_secs(30),
        "elapsed {:?}",
        start.elapsed()
    );
}

#[tokio::test]
async fn never_more_than_two_requests_are_in_flight() {
    let server = server().await;
    // Real time, a short stall: paused time would let the runtime jump
    // straight to the client's 30 s deadline before the stall begins.
    server.set_fault(Some(Fault::Stall(Duration::from_millis(50))));
    let client = client(&server);
    let mut handles = Vec::new();
    for _ in 0..6 {
        let client = client.clone();
        handles.push(tokio::spawn(async move { client.ping().await }));
    }
    for handle in handles {
        handle.await.expect("the ping task joins").expect("ping");
    }
    assert!(
        server.max_in_flight() <= 2,
        "max in flight was {}",
        server.max_in_flight()
    );
    assert_eq!(server.requests().len(), 6);
}

#[tokio::test]
async fn response_codes_map_to_typed_refusals() {
    let server = server().await;
    let client = client(&server);

    for (status, code, expected) in [
        (400u16, 2064i64, "app identity refused"),
        (400, 5003, "unsupported version"),
        (401, 2001, "access token rejected"),
        (422, 10013, "refresh refused"),
    ] {
        server.set_fault(Some(Fault::Code { status, code }));
        let error = client.ping().await.expect_err("the fault refuses");
        let matched = match expected {
            "app identity refused" => matches!(error, ProtonError::Unavailable { .. }),
            "unsupported version" => matches!(error, ProtonError::Unavailable { .. }),
            "access token rejected" => matches!(error, ProtonError::Unauthorized),
            "refresh refused" => matches!(error, ProtonError::RefreshRefused { .. }),
            _ => unreachable!(),
        };
        assert!(matched, "{expected}: {error:?}");
    }
    server.set_fault(None);
}

#[test]
fn the_response_code_table_covers_the_research_codes() {
    for code in [
        1000, 1001, 2001, 2028, 2064, 5001, 5002, 5003, 6003, 8002, 9001, 9002, 9101, 10004, 10013,
        12087,
    ] {
        assert!(ApiCode::from_i64(code).is_some(), "{code} is unclassified");
    }
    assert_eq!(ApiCode::from_i64(1000), Some(ApiCode::Success));
    assert_eq!(ApiCode::from_i64(10013), Some(ApiCode::RefreshTokenInvalid));
    assert_eq!(ApiCode::from_i64(42), None);
}

/// The request log records header presence, never a credential's value:
/// the fake saw an `Authorization: Bearer` header and its `Debug`
/// output must not carry the synthetic token.
#[tokio::test]
async fn the_request_log_records_presence_but_never_a_token() {
    const TOKEN: &str = "synthetic-bearer-token";
    let server = server().await;
    let base_url = server.base_url().to_owned();
    // Blocking socket I/O on its own thread: the fake server runs on
    // this test's async runtime.
    let answered = tokio::task::spawn_blocking(move || {
        let addr = base_url
            .strip_prefix("https://")
            .expect("an https base url");
        let stream = std::net::TcpStream::connect(addr).expect("the fake accepts");
        let mut tls =
            client_stream(stream, "127.0.0.1").expect("the test identity trusts the fake");
        let request = format!(
            "GET /tests/ping HTTP/1.1\r\nhost: 127.0.0.1\r\n\
             authorization: Bearer {TOKEN}\r\nconnection: close\r\n\r\n"
        );
        tls.write_all(request.as_bytes())
            .expect("the request writes");
        let mut response = Vec::new();
        tls.read_to_end(&mut response).expect("the response reads");
        response.starts_with(b"HTTP/1.1 200")
    })
    .await
    .expect("the blocking client joins");
    assert!(answered, "the fake answered");

    let recorded = server.requests();
    assert_eq!(recorded.len(), 1, "one request reached the fake");
    assert!(recorded[0].has_authorization, "the header was seen");
    let debug = format!("{recorded:?}");
    assert!(
        !debug.contains(TOKEN),
        "the log rendered the bearer token: {debug}"
    );
    assert!(
        !debug.contains("Bearer"),
        "the log rendered the header: {debug}"
    );
}
