//! Actual pinned Dovecot: a command deadline feeds the owned reconnect
//! driver; its delayed fresh session preserves the epoch and unseen message.

use std::future::{Future, poll_fn};
use std::io;
use std::pin::Pin;
use std::sync::{
    Arc,
    atomic::{AtomicBool, AtomicUsize, Ordering},
};
use std::task::{Context, Poll};
use std::time::Duration;

use sb_imap::{
    BodyValue, CertificatePin, ConnectAttempt, ConnectOptions, Connection, Connector, FlagCoverage,
    HostPolicy, ImapError, Password, ReconnectWait, Reconnector,
};
use sb_test_support::cert_der;
use tokio::io::AsyncWrite;

struct BlockedSink {
    entered: Arc<AtomicBool>,
    drops: Arc<AtomicUsize>,
}
impl Drop for BlockedSink {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}
impl AsyncWrite for BlockedSink {
    fn poll_write(self: Pin<&mut Self>, _: &mut Context<'_>, _: &[u8]) -> Poll<io::Result<usize>> {
        self.entered.store(true, Ordering::Release);
        Poll::Pending
    }
    fn poll_flush(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
    fn poll_shutdown(self: Pin<&mut Self>, _: &mut Context<'_>) -> Poll<io::Result<()>> {
        Poll::Ready(Ok(()))
    }
}

struct FreshConnector {
    host: std::net::IpAddr,
    port: u16,
    lookups: Arc<AtomicUsize>,
}
impl Connector for FreshConnector {
    fn connect(&mut self) -> ConnectAttempt<'_> {
        Box::pin(async move {
            self.lookups.fetch_add(1, Ordering::SeqCst);
            // Retrieve fresh synthetic container credentials per attempt.
            Connection::connect_starttls(ConnectOptions {
                host: self.host,
                port: self.port,
                host_policy: HostPolicy::LoopbackOnly,
                pin: CertificatePin::of_der(cert_der().as_ref()),
                user: std::env::var("SB_IMAP_DOVECOT_USER").unwrap(),
                password: Password::new(std::env::var("SB_IMAP_DOVECOT_PASSWORD").unwrap()),
            })
            .await
        })
    }
}

#[tokio::test]
async fn dovecot_retry_after_timeout_preserves_unseen() {
    let Ok(address) = std::env::var("SB_IMAP_DOVECOT") else {
        eprintln!("SB_IMAP_DOVECOT unset; not asked to run the Dovecot suite");
        return;
    };
    let (host, port) = address.split_once(':').expect("Dovecot host:port");
    let options = || ConnectOptions {
        host: host.parse().unwrap(),
        port: port.parse().unwrap(),
        host_policy: HostPolicy::LoopbackOnly,
        pin: CertificatePin::of_der(cert_der().as_ref()),
        user: std::env::var("SB_IMAP_DOVECOT_USER").unwrap(),
        password: Password::new(std::env::var("SB_IMAP_DOVECOT_PASSWORD").unwrap()),
    };
    let mut client = Connection::connect_starttls(options()).await.unwrap();
    let before_epoch = client
        .examine("INBOX")
        .await
        .unwrap()
        .observation
        .uid_validity;
    let before = client.flag_sweep().await.unwrap().observation;
    assert_eq!(before.coverage, FlagCoverage::Complete);
    assert_eq!(before.rows.len(), 1);
    assert!(!before.rows[0].flags.iter().any(|flag| flag == "\\Seen"));
    let uid = before.rows[0].uid;
    let entered = Arc::new(AtomicBool::new(false));
    let drops = Arc::new(AtomicUsize::new(0));
    let mut operation = Box::pin(client.fetch_body(
        uid,
        BlockedSink {
            entered: entered.clone(),
            drops: drops.clone(),
        },
    ));
    // Let real TCP/TLS and the server finish delivering the payload before
    // advancing the test clock; paused-time auto-advance can outrun OS I/O.
    tokio::select! {
        result = &mut operation => panic!("body finished before the blocked sink deadline: {result:?}"),
        ready = tokio::time::timeout(Duration::from_secs(3), async {
            while !entered.load(Ordering::Acquire) { tokio::task::yield_now().await; }
        }) => ready.unwrap(),
    }
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(121)).await;
    let result = tokio::time::timeout(Duration::from_secs(1), &mut operation).await;
    drop(operation);
    tokio::time::resume();
    let failure = match result {
        Ok(Err(
            error @ ImapError::TimedOut {
                operation: "UID FETCH",
            },
        )) => error,
        _ => panic!("the old session must fail at the intended body deadline"),
    };
    assert_eq!(drops.load(Ordering::SeqCst), 1);
    assert!(matches!(
        client.fetch_body(uid, Vec::<u8>::new()).await,
        Err(ImapError::Closed)
    ));

    let lookups = Arc::new(AtomicUsize::new(0));
    let mut connector = FreshConnector {
        host: host.parse().unwrap(),
        port: port.parse().unwrap(),
        lookups: Arc::clone(&lookups),
    };
    let mut reconnector = Reconnector::new();
    let mut delays = Vec::new();
    let mut retry = Box::pin(reconnector.retry(
        failure,
        &mut connector,
        std::future::pending(),
        |wait: ReconnectWait<'_>| delays.push(wait.delay),
    ));
    assert!(poll_fn(|cx| Poll::Ready(retry.as_mut().poll(cx).is_pending())).await);
    assert_eq!(
        lookups.load(Ordering::SeqCst),
        0,
        "no lookup before backoff"
    );
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(1)).await;
    tokio::time::resume();
    let mut fresh = tokio::time::timeout(Duration::from_secs(3), &mut retry)
        .await
        .unwrap()
        .unwrap();
    drop(retry);
    assert_eq!(lookups.load(Ordering::SeqCst), 1);
    assert_eq!(delays, vec![Duration::from_secs(1)]);
    let selected = fresh.examine("INBOX").await.unwrap().observation;
    assert_eq!(selected.uid_validity, before_epoch);
    let after = fresh.flag_sweep().await.unwrap().observation;
    assert_eq!(after.coverage, FlagCoverage::Complete);
    assert_eq!(after.inventory.uids, before.inventory.uids);
    assert!(!after.rows[0].flags.iter().any(|flag| flag == "\\Seen"));
    let body = fresh
        .fetch_body(uid, Vec::<u8>::new())
        .await
        .unwrap()
        .observation;
    assert_eq!(body.uid, uid);
    let BodyValue::Complete { sink, bytes } = body.body else {
        panic!("fresh connection lost the body");
    };
    assert_eq!(bytes, sink.len() as u64);
    assert!(bytes > 0);
    let persisted = fresh.flag_sweep().await.unwrap().observation;
    assert_eq!(persisted.coverage, FlagCoverage::Complete);
    assert!(!persisted.rows[0].flags.iter().any(|flag| flag == "\\Seen"));
    fresh.logout().await.unwrap();
}
