//! The fake IMAP server's own tests (spec/11): a raw client drives
//! scripts end-to-end — steps, literals, STARTTLS, the Bridge profile
//! (spec/04) and every fault knob — proof independent of any client
//! crate.

use std::io::{BufRead, BufReader, Read, Write};
use std::net::TcpStream;
use std::time::{Duration, Instant};

use imap_codec::ResponseCodec;
use imap_codec::decode::Decoder;
use sb_test_support::{
    Expect, FakeImapServer, Profile, RAW_UTF8_ENVELOPE, Reply, Script, Step, cert_der,
    client_stream,
};

/// Half a second is generous for a loopback exchange; reads must
/// never hang a test on a misbehaving fake.
const IO_TIMEOUT: Duration = Duration::from_millis(500);

/// A minimal IMAP client over any duplex stream (`TlsIo` after TLS).
struct RawClient<S> {
    io: BufReader<S>,
}

impl RawClient<TcpStream> {
    fn connect(port: u16) -> Self {
        let stream = TcpStream::connect(("127.0.0.1", port)).unwrap();
        stream.set_read_timeout(Some(IO_TIMEOUT)).unwrap();
        Self {
            io: BufReader::new(stream),
        }
    }
}

impl<S: Read + Write> RawClient<S> {
    fn read_line(&mut self) -> String {
        let mut line = String::new();
        self.io.read_line(&mut line).unwrap();
        assert!(line.ends_with("\r\n"), "line without CRLF: {line:?}");
        line.trim_end().to_string()
    }

    /// A response line that announces a literal, then the literal.
    fn read_literal(&mut self) -> (String, Vec<u8>) {
        let head = self.read_line();
        let len: usize = head
            .rsplit('{')
            .next()
            .and_then(|rest| rest.strip_suffix('}'))
            .and_then(|digits| digits.parse().ok())
            .expect("the line announces a literal");
        (head, self.read_exact(len))
    }

    fn read_exact(&mut self, n: usize) -> Vec<u8> {
        let mut buf = vec![0u8; n];
        self.io.read_exact(&mut buf).unwrap();
        buf
    }

    /// One nonblocking read — `0` proves the peer closed.
    fn read_some(&mut self, buf: &mut [u8]) -> usize {
        self.io.get_mut().read(buf).unwrap()
    }

    fn write(&mut self, bytes: &[u8]) {
        self.io.get_mut().write_all(bytes).unwrap();
        self.io.get_mut().flush().unwrap();
    }

    fn command(&mut self, command: &str) {
        self.write(format!("{command}\r\n").as_bytes());
    }
}

/// Serves `scripts`, connects, and swallows the greeting.
fn started(scripts: Vec<Script>) -> (FakeImapServer, RawClient<TcpStream>) {
    let server = FakeImapServer::serve(scripts).unwrap();
    let mut client = RawClient::connect(server.port());
    assert!(client.read_line().starts_with("* OK"));
    (server, client)
}

/// Sends `command`; returns the untagged lines before its tagged reply.
fn ask<S: Read + Write>(client: &mut RawClient<S>, command: &str) -> (Vec<String>, String) {
    client.command(command);
    let mut untagged = Vec::new();
    loop {
        let line = client.read_line();
        if line.starts_with('*') || line.starts_with('+') {
            untagged.push(line);
        } else {
            return (untagged, line);
        }
    }
}

/// A `* CAPABILITY` exchange step, so the mechanism tests don't lean
/// on the Bridge profile.
fn capability_step() -> Step {
    Step::on(
        Expect::command("CAPABILITY"),
        vec![
            Reply::untagged("CAPABILITY IMAP4rev1 UIDPLUS"),
            Reply::ok("CAPABILITY completed"),
        ],
    )
}

/// `tag UID FETCH <uid>` on the provider-id field — spec/04's move
/// verification. Returns the header literal as UTF-8 text.
fn fetch_id(client: &mut RawClient<TcpStream>, tag: &str, uid: u32) -> String {
    let command = format!("{tag} UID FETCH {uid} (BODY.PEEK[HEADER.FIELDS (X-PM-INTERNAL-ID)])");
    client.command(&command);
    let (head, literal) = client.read_literal();
    assert_eq!(client.read_line(), ")");
    let tagged = client.read_line();
    assert!(head.starts_with(&format!("* 1 FETCH (UID {uid}")), "{head}");
    assert!(tagged.ends_with("OK UID FETCH completed"), "{tagged}");
    String::from_utf8(literal).expect("the headers are UTF-8")
}

#[test]
fn script_replays_commands_and_records_the_trace() {
    let script = Script::new(vec![
        capability_step(),
        Step::on(Expect::command("LOGIN"), vec![Reply::ok("LOGIN completed")]),
    ])
    .greeting("* OK [CAPABILITY IMAP4rev1] test ready\r\n");

    let server = FakeImapServer::serve(vec![script]).unwrap();
    let mut client = RawClient::connect(server.port());
    assert_eq!(client.read_line(), "* OK [CAPABILITY IMAP4rev1] test ready");

    let (untagged, ok) = ask(&mut client, "a1 CAPABILITY");
    assert_eq!(untagged, ["* CAPABILITY IMAP4rev1 UIDPLUS"]);
    assert_eq!(ok, "a1 OK CAPABILITY completed");

    let (_, ok) = ask(&mut client, "a2 LOGIN \"owner@example.org\" \"pw\"");
    assert_eq!(ok, "a2 OK LOGIN completed");

    // The trace keeps normalized command lines, quotes dropped.
    let received: Vec<String> = server
        .received()
        .into_iter()
        .map(|entry| entry.line)
        .collect();
    assert_eq!(received, ["a1 CAPABILITY", "a2 LOGIN owner@example.org pw"]);
    assert_eq!(server.unexpected_count(), 0);
}

/// An expectation written as a client sends it, quotes and all,
/// matches that command: `LIST "" *` and a quoted mailbox name.
#[test]
fn quoted_expectations_match_what_clients_send() {
    let script = Script::new(vec![
        Step::on(
            Expect::command("LIST \"\" *"),
            vec![Reply::ok("LIST completed")],
        ),
        Step::on(
            Expect::command("EXAMINE \"All Mail\""),
            vec![Reply::ok("EXAMINE completed")],
        ),
    ]);
    let (server, mut client) = started(vec![script]);

    assert_eq!(ask(&mut client, "a1 LIST \"\" *").1, "a1 OK LIST completed");
    assert_eq!(
        ask(&mut client, "a2 EXAMINE \"All Mail\"").1,
        "a2 OK EXAMINE completed"
    );
    assert_eq!(server.unexpected_count(), 0);
}

#[test]
fn literals_are_framed_in_both_directions() {
    // `{5}` is a synchronizing literal: the server must offer `+`
    // before its bytes. An untagged literal is framed with its length.
    let script = Script::new(vec![
        Step::on(Expect::command("LOGIN"), vec![Reply::ok("LOGIN completed")]),
        Step::on(
            Expect::command("UID FETCH"),
            vec![
                Reply::UntaggedLiteral {
                    head: "1 FETCH (UID 1 BODY[HEADER.FIELDS (SUBJECT)]".to_string(),
                    bytes: b"Subject: sample\r\n\r\n".to_vec(),
                    tail: ")".to_string(),
                },
                Reply::ok("FETCH completed"),
            ],
        ),
    ]);
    let (server, mut client) = started(vec![script]);

    client.write(b"a1 LOGIN {5}\r\n");
    assert!(client.read_line().starts_with('+'));
    client.write(b"owner {2}\r\n");
    assert!(client.read_line().starts_with('+'));
    client.write(b"pw\r\n");
    assert_eq!(client.read_line(), "a1 OK LOGIN completed");

    client.command("a2 UID FETCH 1 (FLAGS)");
    let (_, literal) = client.read_literal();
    assert_eq!(literal, b"Subject: sample\r\n\r\n");
    assert_eq!(client.read_line(), ")");
    assert_eq!(client.read_line(), "a2 OK FETCH completed");
    assert_eq!(server.unexpected_count(), 0);
}

#[test]
fn starttls_wraps_the_socket_and_keeps_serving() {
    let script = Script::new(vec![
        Step::on(
            Expect::command("STARTTLS"),
            vec![Reply::ok("Begin TLS negotiation now"), Reply::UpgradeTls],
        ),
        capability_step(),
    ]);
    let server = FakeImapServer::serve(vec![script]).unwrap();
    let mut plain = RawClient::connect(server.port());
    assert!(plain.read_line().starts_with("* OK"));
    plain.command("a1 STARTTLS");
    assert_eq!(plain.read_line(), "a1 OK Begin TLS negotiation now");

    // The pinning client verifies the committed fixture certificate.
    let io = BufReader::new(client_stream(plain.io.into_inner(), "127.0.0.1").unwrap());
    let mut tls = RawClient { io };
    let (untagged, ok) = ask(&mut tls, "a2 CAPABILITY");
    assert_eq!(untagged, ["* CAPABILITY IMAP4rev1 UIDPLUS"]);
    assert_eq!(ok, "a2 OK CAPABILITY completed");
    let conn = &tls.io.get_mut().conn;
    assert_eq!(conn.peer_certificates().unwrap()[0], cert_der());
}

#[test]
fn fault_knobs_fallback_no_and_drop_in_both_positions() {
    // `Reply::Drop` after the tagged OK it follows on one connection,
    // and after command two with no reply at all on the next.
    let script = Script::new(vec![
        Step::on(
            Expect::command("CLOSE"),
            vec![Reply::TaggedNo("cannot CLOSE here".to_string())],
        ),
        Step::on(
            Expect::command("LOGIN"),
            vec![Reply::ok("LOGIN completed"), Reply::Drop],
        ),
    ]);
    let next = Script::new(vec![
        capability_step(),
        Step::on(Expect::command("LOGIN"), vec![Reply::Drop]),
    ]);
    let (server, mut client) = started(vec![script, next]);
    let mut buf = [0u8; 16];

    // An unscripted command hits the fallback and counts.
    client.command("a9 SELECT INBOX");
    assert_eq!(
        client.read_line(),
        "a9 BAD no scripted answer for this command"
    );
    assert_eq!(server.unexpected_count(), 1);

    // A NO reply carries the command's own tag.
    assert_eq!(ask(&mut client, "a1 CLOSE").1, "a1 NO cannot CLOSE here");

    client.command("a2 LOGIN u p");
    assert_eq!(client.read_line(), "a2 OK LOGIN completed");
    assert_eq!(client.read_some(&mut buf), 0, "drop after the tagged OK");

    let mut client = RawClient::connect(server.port());
    client.read_line();
    let (_, ok) = ask(&mut client, "b1 CAPABILITY");
    assert_eq!(ok, "b1 OK CAPABILITY completed");
    client.command("b2 LOGIN u p");
    assert_eq!(client.read_some(&mut buf), 0, "no reply, just EOF");
    assert_eq!(server.received().len(), 5, "all commands arrived first");
}

#[test]
fn uidvalidity_changes_between_sessions() {
    // Each connection takes the next script, so the second EXAMINE
    // advertises the epoch a repair reassigned (spec/11 knob).
    let examine = |uidvalidity| {
        Step::on(
            Expect::command("EXAMINE INBOX"),
            vec![
                Reply::untagged("0 EXISTS"),
                Reply::untagged(format!("OK [UIDVALIDITY {uidvalidity}] UIDs valid")),
                Reply::ok("EXAMINE completed"),
            ],
        )
    };
    let server = FakeImapServer::serve(vec![
        Script::new(vec![examine(4100)]),
        Script::new(vec![examine(5100)]),
    ])
    .unwrap();

    for want in [4100, 5100] {
        let mut client = RawClient::connect(server.port());
        client.read_line();
        let (untagged, ok) = ask(&mut client, "a1 EXAMINE INBOX");
        assert_eq!(untagged[1], format!("* OK [UIDVALIDITY {want}] UIDs valid"));
        assert_eq!(ok, "a1 OK EXAMINE completed");
    }
    let conns: Vec<usize> = server.received().iter().map(|r| r.conn).collect();
    assert_eq!(conns, [0, 1], "each connection got its own script run");
}

#[test]
fn expunge_and_move_arrive_behind_the_clients_back() {
    // The before/after inventory distinguishes a move from a plain
    // expunge: the provider id leaves All Mail and reappears in
    // Labels/Receipts while the client idles.
    let profile = Profile::bridge();
    let script = Script::new(vec![
        profile.examine_step("All Mail"),
        profile.fetch_headers_step("All Mail"),
        Step::on(
            Expect::command("IDLE"),
            vec![Reply::Continuation("idling".into())],
        ),
        Step::on(
            Expect::command("DONE"),
            vec![
                Reply::untagged("1 EXPUNGE"),
                Reply::untagged("OK [COPYUID 4100 9 5] moved"),
                Reply::ok("IDLE terminated"),
            ],
        ),
        Step::on(
            Expect::command("EXAMINE All Mail"),
            vec![Reply::untagged("0 EXISTS"), Reply::ok("EXAMINE completed")],
        ),
        profile.examine_step("Labels/Receipts"),
        profile.fetch_headers_step("Labels/Receipts"),
    ]);
    let (server, mut client) = started(vec![script]);

    // Before: the provider id sits at UID 9 in All Mail.
    let (untagged, _) = ask(&mut client, "a1 EXAMINE \"All Mail\"");
    assert_eq!(untagged[0], "* 1 EXISTS");
    let before = fetch_id(&mut client, "a2", 9);
    assert!(before.contains("X-Pm-Internal-Id: pm-7f3a9c21"), "{before}");

    // The move lands while the client idles: the source loses its
    // only message and gets a COPYUID receipt for it.
    client.command("a3 IDLE");
    assert_eq!(client.read_line(), "+ idling");
    let (untagged, ok) = ask(&mut client, "DONE");
    assert_eq!(untagged[0], "* 1 EXPUNGE");
    assert_eq!(untagged[1], "* OK [COPYUID 4100 9 5] moved");
    assert_eq!(ok, "a3 OK IDLE terminated", "DONE takes IDLE's tag");

    // After: All Mail is empty and the same provider id answers from
    // Labels/Receipts at the UID COPYUID assigned it.
    let (untagged, _) = ask(&mut client, "a4 EXAMINE \"All Mail\"");
    assert_eq!(untagged[0], "* 0 EXISTS");
    let (untagged, _) = ask(&mut client, "a5 EXAMINE Labels/Receipts");
    assert_eq!(untagged[0], "* 1 EXISTS");
    let after = fetch_id(&mut client, "a6", 5);
    assert_eq!(before, after, "the same provider id moved mailboxes");
    assert_eq!(server.unexpected_count(), 0);
}

#[test]
fn partial_fetch_and_delay_shape_the_reply() {
    // A FETCH step may answer fewer items than the client asked for,
    // and [`Reply::Delay`] holds the rest of the answer back.
    const PAUSE: Duration = Duration::from_millis(250);
    let script = Script::new(vec![
        Step::on(
            Expect::command("EXAMINE"),
            vec![
                Reply::untagged("3 EXISTS"),
                Reply::untagged("OK [UIDVALIDITY 4100] UIDs valid"),
                Reply::ok("EXAMINE completed"),
            ],
        ),
        Step::on(
            Expect::command("UID FETCH"),
            vec![
                Reply::untagged("1 FETCH (UID 1 FLAGS (\\Seen))"),
                Reply::Delay(PAUSE),
                Reply::ok("UID FETCH completed"),
            ],
        ),
    ]);
    let (server, mut client) = started(vec![script]);

    let _ = ask(&mut client, "a1 EXAMINE INBOX");
    let started_at = Instant::now();
    let (untagged, ok) = ask(&mut client, "a2 UID FETCH 1:* (FLAGS)");
    assert_eq!(untagged.len(), 1, "one of three");
    assert_eq!(untagged[0], "* 1 FETCH (UID 1 FLAGS (\\Seen))");
    assert_eq!(ok, "a2 OK UID FETCH completed");
    assert!(started_at.elapsed() >= PAUSE, "the reply held the pause");
    assert_eq!(server.unexpected_count(), 0);
}

#[test]
fn capabilities_are_advertised_or_withheld_on_request() {
    // The spec/11 knob: the profile sends whatever capabilities it
    // carries; Bridge's set holds UIDPLUS and MOVE, not CONDSTORE/QRESYNC.
    let script = Script::new(vec![
        Profile::bridge().capability_step(),
        Profile::bridge()
            .with_capabilities(&["IMAP4rev1", "UIDPLUS", "MOVE", "CONDSTORE", "QRESYNC"])
            .capability_step(),
        Profile::bridge()
            .with_capabilities(&["IMAP4rev1"])
            .capability_step(),
    ]);
    let (server, mut client) = started(vec![script]);

    // Exact answers both withhold and advertise: Bridge's set carries
    // UIDPLUS+MOVE and none of CONDSTORE/QRESYNC/NAMESPACE/LITERAL+.
    let [s] = ask(&mut client, "a1 CAPABILITY").0.try_into().unwrap();
    assert_eq!(s, "* CAPABILITY IMAP4rev1 IDLE UIDPLUS MOVE UNSELECT ID");
    let [s] = ask(&mut client, "a2 CAPABILITY").0.try_into().unwrap();
    assert_eq!(s, "* CAPABILITY IMAP4rev1 UIDPLUS MOVE CONDSTORE QRESYNC");
    let [s] = ask(&mut client, "a3 CAPABILITY").0.try_into().unwrap();
    assert_eq!(s, "* CAPABILITY IMAP4rev1");
    assert_eq!(server.unexpected_count(), 0);
}

#[test]
fn the_bridge_profile_answers_a_sync_conversation() {
    let profile = Profile::bridge();
    let script = Script::new(vec![
        profile.capability_step(),
        profile.list_step(),
        profile.examine_step("All Mail"),
        profile.fetch_flags_step("All Mail"),
        profile.fetch_headers_step("All Mail"),
        profile.examine_step("Labels/Receipts"),
        profile.fetch_headers_step("Labels/Receipts"),
        Step::on(
            Expect::command("COPY"),
            vec![Reply::ok(profile.copyuid("7", "5"))],
        ),
        Step::on(
            Expect::command("APPEND"),
            vec![Reply::ok(profile.appenduid(8))],
        ),
    ]);
    let (server, mut client) = started(vec![script]);

    let [s] = ask(&mut client, "a1 CAPABILITY").0.try_into().unwrap();
    assert_eq!(s, "* CAPABILITY IMAP4rev1 IDLE UIDPLUS MOVE UNSELECT ID");

    // Folders, labels and the role attributes.
    let (untagged, _) = ask(&mut client, "a2 LIST \"\" \"*\"");
    assert_eq!(
        untagged,
        [
            "* LIST (\\HasNoChildren) \"/\" \"INBOX\"",
            "* LIST (\\Drafts \\HasNoChildren) \"/\" Drafts",
            "* LIST (\\Sent \\HasNoChildren) \"/\" Sent",
            "* LIST (\\Flagged \\HasNoChildren) \"/\" Starred",
            "* LIST (\\Archive \\HasNoChildren) \"/\" Archive",
            "* LIST (\\Junk \\HasNoChildren) \"/\" Spam",
            "* LIST (\\Trash \\HasNoChildren) \"/\" Trash",
            "* LIST (\\All \\HasNoChildren) \"/\" \"All Mail\"",
            "* LIST (\\Noselect \\HasChildren) \"/\" Folders",
            "* LIST (\\HasNoChildren) \"/\" \"Folders/Travel\"",
            "* LIST (\\Noselect \\HasChildren) \"/\" Labels",
            "* LIST (\\HasNoChildren) \"/\" \"Labels/Receipts\"",
        ]
    );

    let (untagged, ok) = ask(&mut client, "a3 EXAMINE \"All Mail\"");
    assert_eq!(untagged[0], "* 1 EXISTS");
    assert_eq!(untagged[1], "* OK [UIDVALIDITY 4100] UIDs valid");
    assert_eq!(untagged[2], "* OK [UIDNEXT 10] Predicted next UID");
    assert_eq!(ok, "a3 OK [READ-ONLY] EXAMINE completed");

    let (untagged, _) = ask(&mut client, "a4 UID FETCH 1:* (FLAGS)");
    assert_eq!(untagged, ["* 1 FETCH (UID 9 FLAGS (\\Seen \\Flagged))"]);

    // The same message fetched by header in both mailboxes: identical
    // bytes, one X-Pm-Internal-Id. fetch_id also proves the FETCH
    // prefix is the sequence number, not the UID (RFC 3501 6.4.8).
    let text = fetch_id(&mut client, "a5", 9);
    assert!(text.contains("X-Pm-Internal-Id: pm-7f3a9c21"), "{text}");

    let (untagged, _) = ask(&mut client, "a6 EXAMINE \"Labels/Receipts\"");
    assert_eq!(untagged[2], "* OK [UIDNEXT 6] Predicted next UID");
    let again = fetch_id(&mut client, "a7", 5);
    assert_eq!(again, text, "same message, other mailbox");

    // UIDPLUS receipts, COPY and APPEND alike.
    let (_, ok) = ask(&mut client, "a8 COPY 7 \"Labels/Receipts\"");
    assert_eq!(ok, "a8 OK [COPYUID 4100 7 5] COPY completed");
    let (_, ok) = ask(&mut client, "a9 APPEND Drafts \"note\"");
    assert_eq!(ok, "a9 OK [APPENDUID 4100 8] APPEND completed");
    assert_eq!(server.unexpected_count(), 0);
}

#[test]
fn the_raw_utf8_envelope_fault_is_undecodable() {
    // Bridge's fault (spec/04): raw UTF-8 in an ENVELOPE, which a
    // client must answer by closing, never by applying the message.
    // imap-codec accepts raw UTF-8 inside a literal on its own, so
    // the eleventh envelope slot is what keeps the bytes
    // undecodable.
    let tolerated = b"* 1 FETCH (UID 7 ENVELOPE (\"Mon, 02 Nov 2026 09:15:00 +0000\" {17}\r\nGr\xc3\xbc\xc3\x9fe aus K\xc3\xb6ln NIL NIL NIL NIL NIL NIL NIL \"<msg-7@example.org>\"))\r\n";
    let codec = ResponseCodec::default();
    // Raw UTF-8 alone decodes, which is why the fixture adds a slot;
    // the fault bytes must stay undecodable.
    assert!(codec.decode(tolerated).is_ok(), "utf-8 alone decodes");
    assert!(codec.decode(RAW_UTF8_ENVELOPE).is_err(), "undecodable");

    // And the server puts them on the wire verbatim.
    let script = Script::new(vec![Step::on(
        Expect::command("UID FETCH"),
        vec![
            Reply::Raw(RAW_UTF8_ENVELOPE.to_vec()),
            Reply::ok("UID FETCH completed"),
        ],
    )]);
    let (server, mut client) = started(vec![script]);

    client.command("a1 UID FETCH 7 (ENVELOPE)");
    assert_eq!(
        client.read_exact(RAW_UTF8_ENVELOPE.len()),
        RAW_UTF8_ENVELOPE
    );
    assert_eq!(client.read_line(), "a1 OK UID FETCH completed");
    assert_eq!(server.unexpected_count(), 0);
}
