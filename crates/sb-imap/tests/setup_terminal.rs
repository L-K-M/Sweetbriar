//! A constructor must not return a connection already closed by setup.
use sb_imap::{CertificatePin, ConnectOptions, Connection, HostPolicy, Password};
use sb_test_support::{Expect, FakeImapServer, Profile, Reply, Script, Step, cert_der};
use std::net::{IpAddr, Ipv4Addr};

#[tokio::test]
async fn a_terminal_response_after_id_cannot_return_a_live_connection() {
    for trailing in [
        Reply::ok("duplicate completion"),
        Reply::untagged("BYE closing"),
        Reply::Raw(b"not an IMAP response\r\n".to_vec()),
    ] {
        let profile = Profile::bridge();
        let script = Script::new(vec![
            profile.capability_step(),
            Step::on(Expect::command("LOGIN"), vec![Reply::ok("authenticated")]),
            profile.capability_step(),
            Step::on(
                Expect::command("ID"),
                vec![Reply::untagged("ID NIL"), Reply::ok("identified"), trailing],
            ),
        ])
        .implicit_tls();
        let server = FakeImapServer::serve(vec![script]).unwrap();
        let result = Connection::connect(ConnectOptions {
            host: IpAddr::V4(Ipv4Addr::LOCALHOST),
            port: server.port(),
            host_policy: HostPolicy::LoopbackOnly,
            pin: CertificatePin::of_der(cert_der().as_ref()),
            user: "synthetic-user".into(),
            password: Password::new("synthetic-password"),
        })
        .await;
        assert!(
            result.is_err(),
            "setup returned an already closed connection: {result:?}"
        );
        assert_eq!(server.received().len(), 4);
        assert_eq!(server.unexpected_count(), 0);
    }
}
