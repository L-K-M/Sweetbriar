//! Actual pinned Dovecot witness, run after xtask makes its seed unseen.
use std::num::NonZeroU32;

use sb_imap::{
    CertificatePin, ConnectOptions, Connection, FlagCoverage, HeaderBytes, HeaderCoverage,
    HeaderFetchMode, HeaderRequest, HostPolicy, MailboxNotice, Password,
};
use sb_test_support::cert_der;

// The spike's synthetic APPEND fixture, including its body. This independent
// expectation pins RFC822.SIZE as well as the filtered header section.
const SEED: &[u8] = b"From: spike@example.org\r\nSubject: Dovecot seed\r\nMessage-ID: <dovecot-seed@example.org>\r\nX-Pm-Internal-Id: pm-7f3a9c21\r\n\r\nseed body\r\n";
const HEADERS: &[u8] = b"From: spike@example.org\r\nSubject: Dovecot seed\r\nMessage-ID: <dovecot-seed@example.org>\r\nX-Pm-Internal-Id: pm-7f3a9c21\r\n\r\n";

#[tokio::test]
async fn dovecot_header_peek_keeps_an_unseen_message_unseen() {
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
    let selection = client.examine("INBOX").await.unwrap().observation;
    assert_eq!(selection.exists, 1);
    let before = client.flag_sweep().await.unwrap().observation;
    assert_eq!(before.coverage, FlagCoverage::Complete);
    assert_eq!(before.rows.len(), 1);
    assert!(!before.rows[0].flags.iter().any(|flag| flag == "\\Seen"));
    let fields = [
        "DATE",
        "FROM",
        "REPLY-TO",
        "TO",
        "CC",
        "SUBJECT",
        "MESSAGE-ID",
        "IN-REPLY-TO",
        "REFERENCES",
        "LIST-ID",
        "DELIVERED-TO",
        "X-ORIGINAL-TO",
        "X-PM-INTERNAL-ID",
    ];
    let request = |mode| {
        HeaderRequest::new(
            before.inventory.uids.clone(),
            fields.iter().map(|name| (*name).to_string()).collect(),
            mode,
        )
        .unwrap()
    };
    let fetched = client
        .fetch_headers(&request(HeaderFetchMode::Full))
        .await
        .unwrap();
    assert_eq!(fetched.observation.coverage, HeaderCoverage::Complete);
    assert_eq!(
        fetched.observation.mailbox.uid_validity,
        selection.uid_validity
    );
    assert!(fetched.observation.missing_uids.is_empty());
    assert_eq!(fetched.observation.rows.len(), 1);
    let row = &fetched.observation.rows[0];
    assert_eq!(row.uid, before.rows[0].uid);
    assert_eq!(row.rfc822_size, SEED.len() as u32);
    assert!(row.internal_date_ms > 0);
    assert!(!row.flags.iter().any(|flag| flag == "\\Seen"));
    assert_eq!(row.headers, HeaderBytes::Full(HEADERS.to_vec()));
    let limit = NonZeroU32::new(16).unwrap();
    let prefix = client
        .fetch_headers(&request(HeaderFetchMode::Prefix { max_bytes: limit }))
        .await
        .unwrap();
    assert_eq!(prefix.observation.coverage, HeaderCoverage::Incomplete);
    assert_eq!(
        prefix.observation.rows[0].headers,
        HeaderBytes::Prefix {
            bytes: HEADERS[..16].to_vec(),
            limit,
        }
    );
    assert!(
        !prefix
            .notices
            .iter()
            .any(|notice| matches!(notice, MailboxNotice::Changed { .. }))
    );
    client.logout().await.unwrap();

    // Reconnect so the final assertion observes the persisted server flags.
    let mut client = Connection::connect_starttls(options()).await.unwrap();
    client.examine("INBOX").await.unwrap();
    let after = client.flag_sweep().await.unwrap().observation;
    assert_eq!(after.coverage, FlagCoverage::Complete);
    assert_eq!(after.inventory.uids, before.inventory.uids);
    assert_eq!(after.rows.len(), 1);
    assert!(!after.rows[0].flags.iter().any(|flag| flag == "\\Seen"));
    client.logout().await.unwrap();
}
