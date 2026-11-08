//! Actual pinned Dovecot: 25-minute renewal and graceful stop preserve a
//! reusable read-only selection and an unseen message.

use sb_imap::{
    BodyValue, CertificatePin, ConnectOptions, Connection, FlagCoverage, HostPolicy, IdleEvent,
    ImapError, Password,
};
use sb_test_support::cert_der;
use std::future::Future;
use std::pin::Pin;
use std::sync::{
    Arc,
    atomic::{AtomicUsize, Ordering},
};
use std::time::Duration;
use tokio::sync::oneshot;

async fn started<F: Future<Output = Result<(), ImapError>>>(
    operation: &mut Pin<Box<F>>,
    count: &AtomicUsize,
    minimum: usize,
) {
    tokio::select! {
        result = operation.as_mut() => panic!("watcher ended before continuation: {result:?}"),
        ready = tokio::time::timeout(Duration::from_secs(3), async {
            while count.load(Ordering::Acquire) < minimum { tokio::task::yield_now().await; }
        }) => ready.unwrap(),
    }
}

#[tokio::test]
async fn dovecot_idle_renews_stops_and_keeps_the_message_unseen() {
    let Ok(address) = std::env::var("SB_IMAP_DOVECOT") else {
        eprintln!("SB_IMAP_DOVECOT unset; not asked to run the Dovecot suite");
        return;
    };
    let (host, port) = address.split_once(':').expect("Dovecot host:port");
    let mut client = Connection::connect_starttls(ConnectOptions {
        host: host.parse().unwrap(),
        port: port.parse().unwrap(),
        host_policy: HostPolicy::LoopbackOnly,
        pin: CertificatePin::of_der(cert_der().as_ref()),
        user: std::env::var("SB_IMAP_DOVECOT_USER").unwrap(),
        password: Password::new(std::env::var("SB_IMAP_DOVECOT_PASSWORD").unwrap()),
    })
    .await
    .unwrap();
    let epoch = client
        .examine("INBOX")
        .await
        .unwrap()
        .observation
        .uid_validity;
    let before = client.flag_sweep().await.unwrap().observation;
    assert_eq!(before.coverage, FlagCoverage::Complete);
    assert_eq!(before.rows.len(), 1);
    assert!(!before.rows[0].flags.iter().any(|flag| flag == "\\Seen"));
    let count = Arc::new(AtomicUsize::new(0));
    let observed = count.clone();
    let (stop_tx, stop_rx) = oneshot::channel();
    let mut operation = Box::pin(client.watch_idle(
        async {
            let _ = stop_rx.await;
        },
        move |event| {
            if let IdleEvent::Started { mailbox } = event {
                assert_eq!(mailbox.name, "INBOX");
                assert_eq!(mailbox.uid_validity, epoch);
                observed.fetch_add(1, Ordering::Release);
            }
        },
    ));
    // A callback is emitted only after the real continuation and tail checks.
    // Resume real time before waiting for Dovecot's response to DONE/new IDLE.
    started(&mut operation, &count, 1).await;
    tokio::time::pause();
    tokio::time::advance(Duration::from_secs(25 * 60 + 1)).await;
    tokio::time::resume();
    started(&mut operation, &count, 2).await;
    assert_eq!(count.load(Ordering::Acquire), 2);
    stop_tx.send(()).unwrap();
    tokio::time::timeout(Duration::from_secs(3), &mut operation)
        .await
        .unwrap()
        .unwrap();
    drop(operation);

    let after = client.flag_sweep().await.unwrap().observation;
    assert_eq!(after.coverage, FlagCoverage::Complete);
    assert_eq!(after.inventory, before.inventory);
    assert!(!after.rows[0].flags.iter().any(|flag| flag == "\\Seen"));
    let body = client
        .fetch_body(before.rows[0].uid, Vec::<u8>::new())
        .await
        .unwrap()
        .observation;
    let BodyValue::Complete { sink, bytes } = body.body else {
        panic!("IDLE lost the body");
    };
    assert_eq!(bytes, sink.len() as u64);
    assert!(bytes > 0);
    let persisted = client.flag_sweep().await.unwrap().observation;
    assert_eq!(persisted.coverage, FlagCoverage::Complete);
    assert!(!persisted.rows[0].flags.iter().any(|flag| flag == "\\Seen"));
    client.logout().await.unwrap();
}
