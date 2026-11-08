//! Actual pinned Dovecot witness after the unchanged six suites. The
//! header witness leaves the seed unseen; full-body PEEK must preserve it.

use sb_imap::{
    BodyValue, CertificatePin, ConnectOptions, Connection, FlagCoverage, HostPolicy, MailboxNotice,
    Password,
};
use sb_test_support::cert_der;

const SEED: &[u8] = b"From: spike@example.org\r\nSubject: Dovecot seed\r\nMessage-ID: <dovecot-seed@example.org>\r\nX-Pm-Internal-Id: pm-7f3a9c21\r\n\r\nseed body\r\n";

#[tokio::test]
async fn dovecot_body_peek_keeps_an_unseen_message_unseen() {
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
    let selected = client.examine("INBOX").await.unwrap().observation;
    assert_eq!(selected.exists, 1);
    let before = client.flag_sweep().await.unwrap().observation;
    assert_eq!(before.coverage, FlagCoverage::Complete);
    assert_eq!(before.rows.len(), 1);
    assert!(!before.rows[0].flags.iter().any(|flag| flag == "\\Seen"));
    let uid = before.rows[0].uid;
    let fetched = client.fetch_body(uid, Vec::<u8>::new()).await.unwrap();
    assert_eq!(fetched.observation.mailbox.name, "INBOX");
    assert_eq!(
        fetched.observation.mailbox.uid_validity,
        selected.uid_validity
    );
    assert_eq!(fetched.observation.uid, uid);
    assert!(
        !fetched
            .notices
            .iter()
            .any(|notice| matches!(notice, MailboxNotice::Changed { .. }))
    );
    let BodyValue::Complete { sink, bytes } = fetched.observation.body else {
        panic!("Dovecot returned no complete body");
    };
    assert_eq!(bytes, SEED.len() as u64);
    assert_eq!(sink, SEED);
    let after = client.flag_sweep().await.unwrap().observation;
    assert_eq!(after.coverage, FlagCoverage::Complete);
    assert_eq!(after.inventory.uids, before.inventory.uids);
    assert!(!after.rows[0].flags.iter().any(|flag| flag == "\\Seen"));
    client.logout().await.unwrap();

    let mut client = Connection::connect_starttls(options()).await.unwrap();
    client.examine("INBOX").await.unwrap();
    let persisted = client.flag_sweep().await.unwrap().observation;
    assert_eq!(persisted.coverage, FlagCoverage::Complete);
    assert_eq!(persisted.inventory.uids, before.inventory.uids);
    assert_eq!(persisted.rows.len(), 1);
    assert!(!persisted.rows[0].flags.iter().any(|flag| flag == "\\Seen"));
    client.logout().await.unwrap();
}
