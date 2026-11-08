//! `add_bridge_account` through the native façade (spec/07; spec/02's
//! rows; spec/10's credential + pin rules) against the scripted fake
//! server and the fake secret store: the real transport does the
//! probe, the real SQLite writer commits the rows, and the only
//! credential material the database ever sees is `credential_ref`.

use std::future::{Ready, ready};

use sb_imap::{CertificatePin, ImapError};
use sb_platform::{Secret, SecretError, SecretStore};
use sb_resources::{
    AddressMode, BridgeAccountError, BridgeAccountParams, BridgeIdentity, CredentialPause,
    EndpointSecurity, IdentitySendable, Resources, StoreError, WritePriority,
};
use sb_store::accounts::reactivate_after_credential;
use sb_test_support::{
    Expect, FakeImapServer, FakeSecrets, Profile, Reply, ScratchDir, Script, SecretOp, Step,
    cert_der,
};

const PASSWORD: &str = "bridge-setup-secret-7F3A";

fn pin() -> CertificatePin {
    CertificatePin::of_der(&cert_der())
}

/// The setup conversation: CAPABILITY before and after LOGIN per the
/// crate's setup sequence, ID, then the probe's own CAPABILITY, LIST
/// and LOGOUT.
fn bridge_script(login_ok: bool) -> Script {
    let profile = Profile::bridge();
    let mut steps = vec![
        profile.capability_step(),
        Step::on(
            Expect::command("LOGIN"),
            vec![if login_ok {
                Reply::ok("LOGIN completed")
            } else {
                Reply::TaggedNo("LOGIN failed".to_owned())
            }],
        ),
    ];
    if login_ok {
        steps.extend([
            profile.capability_step(),
            Step::on(
                Expect::command("ID"),
                vec![
                    Reply::untagged("ID (\"name\" \"Proton Mail Bridge\" \"version\" \"3.27.1\")"),
                    Reply::ok("ID completed"),
                ],
            ),
            profile.capability_step(),
            Step::on(
                Expect::command("LIST"),
                vec![
                    Reply::untagged("LIST () \"/\" INBOX"),
                    Reply::untagged("LIST (\\Sent) \"/\" Sent"),
                    Reply::ok("LIST completed"),
                ],
            ),
            Step::on(
                Expect::command("LOGOUT"),
                vec![
                    Reply::untagged("BYE Logging out"),
                    Reply::ok("LOGOUT completed"),
                ],
            ),
        ]);
    }
    Script::new(steps).implicit_tls()
}

fn params(port: u16, secret: &str) -> BridgeAccountParams {
    BridgeAccountParams {
        display_name: "Owner Proton".to_owned(),
        username: "owner@example.org".to_owned(),
        password: Secret::new(secret.as_bytes().to_vec()),
        imap_port: port,
        imap_security: EndpointSecurity::Tls,
        smtp_port: 1025,
        smtp_security: EndpointSecurity::StartTls,
        pin: pin(),
        address_mode: Some(AddressMode::Combined),
        identities: vec![
            BridgeIdentity {
                email: "owner@example.org".to_owned(),
                display_name: "Owner".to_owned(),
                is_default: true,
                sendable: IdentitySendable::Provisional,
            },
            BridgeIdentity {
                // Padding is trimmed before storage.
                email: " alias@example.org ".to_owned(),
                display_name: "Alias".to_owned(),
                is_default: false,
                sendable: IdentitySendable::Provisional,
            },
        ],
    }
}

fn resources(label: &str) -> (ScratchDir, Resources) {
    let scratch = ScratchDir::new(label);
    let resources = Resources::open(&scratch.path().join("store")).unwrap();
    (scratch, resources)
}

/// The flow's secret step, run inline: the fake store is in-memory, so
/// there is no blocking call to move off the test's runtime.
fn inline(secrets: &FakeSecrets) -> impl FnOnce(String, Secret) -> Ready<Result<(), SecretError>> {
    move |reference, secret| ready(secrets.set(&reference, &secret))
}

/// Every column as text (NULL as ""), with bound parameters.
fn rows(res: &Resources, sql: &str, params: &[&str]) -> Vec<Vec<String>> {
    res.read(|tx| {
        let mut statement = tx.prepare(sql).unwrap();
        for (index, value) in params.iter().enumerate() {
            statement.raw_bind_parameter(index + 1, value).unwrap();
        }
        let count = statement.column_count();
        let mut found = statement.raw_query();
        let mut out = Vec::new();
        while let Some(row) = found.next().unwrap() {
            out.push(
                (0..count)
                    .map(|index| {
                        row.get::<_, Option<String>>(index)
                            .unwrap()
                            .unwrap_or_default()
                    })
                    .collect(),
            );
        }
        Ok(out)
    })
    .unwrap()
}

#[tokio::test(flavor = "current_thread")]
async fn setup_stores_rows_and_the_credential_by_reference() {
    let server = FakeImapServer::serve(vec![bridge_script(true)]).unwrap();
    let secrets = FakeSecrets::new();
    let (scratch, res) = resources("account-setup");

    let added = res
        .add_bridge_account(params(server.port(), PASSWORD), inline(&secrets))
        .await
        .expect("setup");

    assert!(added.needs_attention.is_none());
    let credential_ref = format!("account/{}/bridge-password", added.account_id);
    let account = rows(
        &res,
        "SELECT kind, provider, display_name, state, CAST(sort_order AS TEXT) FROM accounts
             WHERE account_id = ?1",
        &[added.account_id.as_str()],
    );
    assert_eq!(
        account,
        vec![vec![
            "imap".to_owned(),
            "proton_bridge".to_owned(),
            "Owner Proton".to_owned(),
            "active".to_owned(),
            "1".to_owned() // the local archive takes slot 0
        ]]
    );
    let endpoint = rows(
        &res,
        "SELECT imap_host, CAST(imap_port AS TEXT), imap_security, smtp_host,
                    CAST(smtp_port AS TEXT), smtp_security, username, credential_ref,
                    trusted_cert_sha256, CAST(server_saves_sent AS TEXT), address_mode
             FROM imap_endpoints
             WHERE account_id = ?1",
        &[added.account_id.as_str()],
    );
    assert_eq!(
        endpoint[0],
        vec![
            "127.0.0.1".to_owned(),
            server.port().to_string(),
            "tls".to_owned(),
            "127.0.0.1".to_owned(),
            "1025".to_owned(),
            "starttls".to_owned(),
            "owner@example.org".to_owned(),
            credential_ref.clone(),
            pin().hex(),
            "1".to_owned(),
            "combined".to_owned()
        ]
    );
    let identities = rows(
        &res,
        "SELECT email, CAST(is_default AS TEXT), sendable FROM identities
             WHERE account_id = ?1 ORDER BY email",
        &[added.account_id.as_str()],
    );
    assert_eq!(
        identities,
        vec![
            vec![
                "alias@example.org".to_owned(),
                "0".to_owned(),
                "provisional".to_owned()
            ],
            vec![
                "owner@example.org".to_owned(),
                "1".to_owned(),
                "provisional".to_owned()
            ]
        ]
    );

    // The compatibility ledger recorded capabilities and mailboxes.
    let ledger: String = res
        .read(|tx| {
            Ok(tx
                .query_row(
                    "SELECT capabilities_json FROM imap_endpoints WHERE account_id = ?1",
                    [&added.account_id],
                    |row| row.get(0),
                )
                .unwrap())
        })
        .unwrap();
    assert!(ledger.contains("IMAP4REV1"), "{ledger}");
    assert!(ledger.contains("INBOX"), "{ledger}");

    // The credential landed in the secret store under the row's ref.
    assert_eq!(
        secrets.recorded(),
        vec![(SecretOp::Set, credential_ref.clone())]
    );
    let stored = secrets.get(&credential_ref).unwrap();
    assert_eq!(stored.expose(), PASSWORD.as_bytes());

    // spec/10: no secret bytes anywhere in the store files — the
    // database, its WAL/SHM and every companion — raw or hexed.
    let hexed: String = PASSWORD.bytes().map(|b| format!("{b:02x}")).collect();
    for entry in std::fs::read_dir(scratch.path().join("store")).unwrap() {
        let path = entry.unwrap().path();
        if !path.is_file() {
            continue;
        }
        let bytes = std::fs::read(&path).unwrap();
        assert!(
            !bytes
                .windows(PASSWORD.len())
                .any(|window| window == PASSWORD.as_bytes()),
            "secret bytes in {}",
            path.display()
        );
        let text = String::from_utf8_lossy(&bytes);
        assert!(
            !text.contains(&hexed) && !text.contains(&hexed.to_uppercase()),
            "hexed secret in {}",
            path.display()
        );
    }
}

#[tokio::test(flavor = "current_thread")]
async fn a_refused_login_stores_nothing() {
    let server = FakeImapServer::serve(vec![bridge_script(false)]).unwrap();
    let secrets = FakeSecrets::new();
    let (_scratch, res) = resources("account-login-refused");

    let error = res
        .add_bridge_account(params(server.port(), PASSWORD), inline(&secrets))
        .await
        .expect_err("login refused");

    assert!(matches!(error, BridgeAccountError::Probe(_)), "{error:?}");
    assert_eq!(
        rows(
            &res,
            "SELECT account_id FROM accounts WHERE kind = 'imap'",
            &[]
        )
        .len(),
        0
    );
    assert_eq!(secrets.calls(SecretOp::Set), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn a_pin_mismatch_stores_nothing() {
    let server = FakeImapServer::serve(vec![bridge_script(true)]).unwrap();
    let secrets = FakeSecrets::new();
    let (_scratch, res) = resources("account-pin-mismatch");

    let mut bad = params(server.port(), PASSWORD);
    bad.pin = CertificatePin::from_bytes([0xab; 32]);
    let error = res
        .add_bridge_account(bad, inline(&secrets))
        .await
        .expect_err("pin mismatch");

    // The presented fingerprint surfaces, in the stored pin's hex form,
    // for the re-trust flow (spec/10).
    let BridgeAccountError::Probe(ImapError::PinMismatch { presented_sha256 }) = error else {
        panic!("{error:?}");
    };
    assert_eq!(presented_sha256, pin().hex());
    assert_eq!(
        rows(
            &res,
            "SELECT account_id FROM accounts WHERE kind = 'imap'",
            &[]
        )
        .len(),
        0
    );
    assert_eq!(secrets.calls(SecretOp::Set), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn a_secret_store_failure_pauses_and_the_retry_heals() {
    let server = FakeImapServer::serve(vec![bridge_script(true)]).unwrap();
    let secrets = FakeSecrets::new();
    secrets.fail_next(SecretOp::Set, SecretError::Unavailable);
    let (_scratch, res) = resources("account-secret-fails");

    let added = res
        .add_bridge_account(params(server.port(), PASSWORD), inline(&secrets))
        .await
        .expect("rows still committed");

    assert_eq!(
        added.needs_attention,
        Some(CredentialPause::StoreUnavailable)
    );
    let state = rows(
        &res,
        "SELECT state, state_reason FROM accounts WHERE account_id = ?1",
        &[added.account_id.as_str()],
    );
    assert_eq!(
        state[0],
        vec![
            "needs_attention".to_owned(),
            "the credential store is unavailable".to_owned()
        ]
    );
    // The ref stays for retry; nothing was deleted or recreated.
    let credential_ref = format!("account/{}/bridge-password", added.account_id);
    assert_eq!(
        secrets.recorded(),
        vec![(SecretOp::Set, credential_ref.clone())]
    );

    res.store_account_credential(
        &added.account_id,
        Secret::new(PASSWORD.as_bytes().to_vec()),
        inline(&secrets),
    )
    .await
    .expect("retry");
    let state = rows(
        &res,
        "SELECT state, state_reason FROM accounts WHERE account_id = ?1",
        &[added.account_id.as_str()],
    );
    // Active with the reason cleared, not active-but-still-warning.
    assert_eq!(state[0], vec!["active".to_owned(), String::new()]);
    let stored = secrets.get(&credential_ref).unwrap();
    assert_eq!(stored.expose(), PASSWORD.as_bytes());
}

#[tokio::test(flavor = "current_thread")]
async fn bad_input_creates_nothing() {
    let server = FakeImapServer::serve(vec![bridge_script(true)]).unwrap();
    let secrets = FakeSecrets::new();
    let (_scratch, res) = resources("account-bad-input");

    let mut bad = params(server.port(), PASSWORD);
    bad.identities.clear();
    let error = res
        .add_bridge_account(bad, inline(&secrets))
        .await
        .expect_err("no identities");
    assert!(matches!(error, BridgeAccountError::InvalidInput(_)));

    let mut non_utf8 = params(server.port(), PASSWORD);
    non_utf8.password = Secret::new(vec![0xff, 0x80, 0xfe]);
    let error = res
        .add_bridge_account(non_utf8, inline(&secrets))
        .await
        .expect_err("non-UTF-8 password");
    assert!(matches!(error, BridgeAccountError::NotUtf8Credential));

    for (imap_port, smtp_port) in [(0, 1025), (server.port(), 0)] {
        let mut zero = params(server.port(), PASSWORD);
        zero.imap_port = imap_port;
        zero.smtp_port = smtp_port;
        let error = res
            .add_bridge_account(zero, inline(&secrets))
            .await
            .expect_err("zero port");
        assert!(
            matches!(error, BridgeAccountError::InvalidInput(_)),
            "{error:?}"
        );
    }

    let mut duplicate = params(server.port(), PASSWORD);
    duplicate.identities[1].email = " OWNER@example.org ".to_owned();
    let error = res
        .add_bridge_account(duplicate, inline(&secrets))
        .await
        .expect_err("duplicate after trimming");
    assert!(
        matches!(error, BridgeAccountError::InvalidInput(_)),
        "{error:?}"
    );
    assert_eq!(
        rows(
            &res,
            "SELECT account_id FROM accounts WHERE kind = 'imap'",
            &[]
        )
        .len(),
        0
    );
    assert_eq!(secrets.calls(SecretOp::Set), 0);
}

#[tokio::test(flavor = "current_thread")]
async fn a_credential_retry_never_lifts_a_connection_pause() {
    let server = FakeImapServer::serve(vec![bridge_script(true)]).unwrap();
    let secrets = FakeSecrets::new();
    let (_scratch, res) = resources("account-connection-pause");
    let added = res
        .add_bridge_account(params(server.port(), PASSWORD), inline(&secrets))
        .await
        .expect("setup");

    // spec/04: a pin mismatch pauses the account until a connect
    // succeeds; a bare secret write must not clear it.
    let paused_id = added.account_id.clone();
    res.write(WritePriority::User, move |tx| {
        tx.execute(
            "UPDATE accounts SET state = 'needs_attention',
                    state_reason = 'Bridge''s certificate changed'
             WHERE account_id = ?1",
            [&paused_id],
        )
        .unwrap();
        Ok(())
    })
    .await
    .unwrap();

    let error = res
        .store_account_credential(
            &added.account_id,
            Secret::new(b"replacement".to_vec()),
            inline(&secrets),
        )
        .await
        .expect_err("not awaiting its credential");
    assert!(
        matches!(error, BridgeAccountError::InvalidInput(_)),
        "{error:?}"
    );
    // The working credential was not replaced.
    assert_eq!(secrets.calls(SecretOp::Set), 1);

    // The reactivation's own guard holds even past the caller's check
    // (a pause recorded between the check and the flip).
    let checked_id = added.account_id.clone();
    let error = res
        .write(WritePriority::User, move |tx| {
            reactivate_after_credential(tx, &checked_id)
        })
        .await
        .expect_err("guarded");
    assert!(matches!(error, StoreError::Conflict { .. }), "{error:?}");
    let state = rows(
        &res,
        "SELECT state, state_reason FROM accounts WHERE account_id = ?1",
        &[added.account_id.as_str()],
    );
    assert_eq!(
        state[0],
        vec![
            "needs_attention".to_owned(),
            "Bridge's certificate changed".to_owned()
        ]
    );
}
