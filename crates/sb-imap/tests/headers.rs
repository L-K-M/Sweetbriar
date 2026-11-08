//! M2.2b b2b1b header observations over the fake server's Bridge
//! profile: `Connection::fetch_headers` takes one validated explicit
//! UID batch and returns UID-keyed rows built from whole FETCH frames,
//! honest incomplete coverage for anything less, and no partial batch
//! when the exchange fails. Every fixture is synthetic and scripted;
//! the pinned Dovecot suite is owned elsewhere.

use std::net::{IpAddr, Ipv4Addr};
use std::num::NonZeroU32;
use std::time::Duration;

use sb_imap::{
    CertificatePin, ConnectOptions, Connection, HeaderBatch, HeaderBytes, HeaderCoverage,
    HeaderFetchMode, HeaderRequest, HeaderRequestError, HeaderRow, HostPolicy, ImapError,
    MailboxContext, MailboxNotice, Password, ReadObservation,
};
use sb_test_support::{Expect, FakeImapServer, Profile, Reply, Script, Step, cert_der};

/// One synthetic observed date: 2020-01-01 00:00:00 +0200 is
/// 2019-12-31 22:00:00 UTC, so `DATE_MS` pins the UTC conversion.
const DATE: &str = "01-Jan-2020 00:00:00 +0200";
const DATE_MS: i64 = 1_577_829_600_000;
/// A raw UTF-8 header line; `é` is two octets on the wire.
const SUBJECT_LINE: &str = "Subject: café\r\n";
/// The epoch the Bridge profile assigns to INBOX.
const EPOCH: u32 = 4100;

fn options(port: u16) -> ConnectOptions {
    ConnectOptions {
        host: IpAddr::V4(Ipv4Addr::LOCALHOST),
        port,
        host_policy: HostPolicy::LoopbackOnly,
        pin: CertificatePin::of_der(cert_der().as_ref()),
        user: "synthetic-user".into(),
        password: Password::new("synthetic-password"),
    }
}

async fn connected(mut steps: Vec<Step>) -> (FakeImapServer, Connection) {
    let p = Profile::bridge();
    let mut setup = vec![
        p.capability_step(),
        Step::on(Expect::command("LOGIN"), vec![Reply::ok("authenticated")]),
        p.capability_step(),
        Step::on(
            Expect::command("ID"),
            vec![Reply::untagged("ID NIL"), Reply::ok("identified")],
        ),
    ];
    setup.append(&mut steps);
    let server = FakeImapServer::serve(vec![Script::new(setup).implicit_tls()]).unwrap();
    let client = Connection::connect(options(server.port())).await.unwrap();
    (server, client)
}

fn examine() -> Vec<Reply> {
    Profile::bridge().examine_step("INBOX").send
}

async fn selected_with(replies: Vec<Reply>, mut steps: Vec<Step>) -> (FakeImapServer, Connection) {
    steps.insert(0, Step::on(Expect::command("EXAMINE"), replies));
    let (server, mut client) = connected(steps).await;
    client.examine("INBOX").await.unwrap();
    (server, client)
}

async fn selected(steps: Vec<Step>) -> (FakeImapServer, Connection) {
    selected_with(examine(), steps).await
}

/// A UID FETCH step whose replies end in the tagged completion.
fn fetch(replies: Vec<Reply>) -> Step {
    let mut sends = replies;
    sends.push(Reply::ok("UID FETCH completed"));
    Step::on(Expect::command("UID FETCH"), sends)
}

/// One scripted header fetch against a fresh selected session.
async fn run_fetch(
    sends: Vec<Reply>,
) -> (
    FakeImapServer,
    Connection,
    Result<ReadObservation<HeaderBatch>, ImapError>,
) {
    let (server, mut client) = selected(vec![Step::on(Expect::command("UID FETCH"), sends)]).await;
    let request = full(&[5], &["SUBJECT"]);
    let result = client.fetch_headers(&request).await;
    (server, client, result)
}

fn nz(value: u32) -> NonZeroU32 {
    NonZeroU32::new(value).expect("fixture UID is nonzero")
}

fn request(uids: &[u32], fields: &[&str], mode: HeaderFetchMode) -> HeaderRequest {
    HeaderRequest::new(
        uids.iter().copied().map(nz).collect(),
        fields.iter().map(|name| name.to_string()).collect(),
        mode,
    )
    .expect("fixture request validates")
}

fn full(uids: &[u32], fields: &[&str]) -> HeaderRequest {
    request(uids, fields, HeaderFetchMode::Full)
}

/// One `BODY[HEADER.FIELDS (...)]` echo item name.
fn echo(fields: &str) -> String {
    format!("BODY[HEADER.FIELDS ({fields})]")
}

fn uid(value: u32) -> Vec<u8> {
    format!("UID {value}").into_bytes()
}

fn flags(names: &str) -> Vec<u8> {
    format!("FLAGS ({names})").into_bytes()
}

fn date() -> Vec<u8> {
    format!("INTERNALDATE \"{DATE}\"").into_bytes()
}

fn size(bytes: u32) -> Vec<u8> {
    format!("RFC822.SIZE {bytes}").into_bytes()
}

/// One BODY item carrying `content` as a raw literal.
fn body(echo: &str, content: &[u8]) -> Vec<u8> {
    let mut item = format!("{echo} {{{}}}\r\n", content.len()).into_bytes();
    item.extend_from_slice(content);
    item
}

/// One BODY item carrying NIL: no literal was observed.
fn body_nil(echo: &str) -> Vec<u8> {
    format!("{echo} NIL").into_bytes()
}

/// One BODY item echoing `<0>`, the first-prefix origin.
fn origin() -> Vec<u8> {
    body("BODY[HEADER.FIELDS (SUBJECT)]<0>", b"Sub")
}

fn raw(bytes: &[u8]) -> Reply {
    Reply::Raw(bytes.to_vec())
}

/// The five required items of one row with caller-chosen flags, size
/// and section bytes.
fn row_at(value: u32, names: &str, bytes: u32, fields: &str, content: &[u8]) -> Vec<Vec<u8>> {
    vec![
        uid(value),
        flags(names),
        date(),
        size(bytes),
        body(&echo(fields), content),
    ]
}

/// The common row shape: known `\\Seen` and the 4242 size hint.
fn row(value: u32, fields: &str, content: &[u8]) -> Vec<Vec<u8>> {
    row_at(value, "\\Seen", 4242, fields, content)
}

/// One row whose BODY item is NIL: no literal was observed.
fn row_nil(value: u32, fields: &str) -> Vec<Vec<u8>> {
    vec![
        uid(value),
        flags("\\Seen"),
        date(),
        size(0),
        body_nil(&echo(fields)),
    ]
}

/// One untagged FETCH frame's exact wire bytes.
fn frame_bytes(seq: u32, items: Vec<Vec<u8>>) -> Vec<u8> {
    let mut bytes = format!("* {seq} FETCH (").into_bytes();
    for (index, item) in items.iter().enumerate() {
        if index > 0 {
            bytes.push(b' ');
        }
        bytes.extend_from_slice(item);
    }
    bytes.extend_from_slice(b")\r\n");
    bytes
}

fn frame(seq: u32, items: Vec<Vec<u8>>) -> Reply {
    Reply::Raw(frame_bytes(seq, items))
}

/// How many coalesced change notices the observation carries.
fn changed(read: &ReadObservation<HeaderBatch>) -> usize {
    read.notices
        .iter()
        .filter(|notice| matches!(notice, MailboxNotice::Changed { .. }))
        .count()
}

/// Whether a change notice names the checked INBOX selection.
fn changed_inbox(read: &ReadObservation<HeaderBatch>) -> bool {
    read.notices.iter().any(|notice| {
        matches!(
            notice,
            MailboxNotice::Changed { mailbox: Some(context) }
                if context.name == "INBOX" && context.uid_validity.get() == EPOCH
        )
    })
}

fn row_uids(batch: &HeaderBatch) -> Vec<u32> {
    batch.rows.iter().map(|row| row.uid.get()).collect()
}

/// The command is the exact wire form: numeric requested UIDs with
/// `u32::MAX` preserved, the selected PEEK fields and no ENVELOPE.
#[tokio::test]
async fn a_fetch_requests_exact_numeric_uids_and_only_the_selected_peek_fields() {
    let request = full(&[7, u32::MAX], &["subject", "From"]);
    let (server, mut client) = selected(vec![fetch(vec![
        frame(1, row(7, "FROM SUBJECT", b"Subject: seven\r\n")),
        frame(2, row(u32::MAX, "FROM SUBJECT", b"Subject: top\r\n")),
    ])])
    .await;
    let read = client.fetch_headers(&request).await.unwrap();
    let line = server
        .received()
        .last()
        .expect("the fetch command")
        .line
        .clone();
    assert!(
        line.ends_with(
            "UID FETCH 7,4294967295 (UID FLAGS INTERNALDATE RFC822.SIZE \
             BODY.PEEK[HEADER.FIELDS (FROM SUBJECT)])"
        ),
        "exact numeric request wire form: {line}"
    );
    assert!(line.contains("BODY.PEEK"), "{line}");
    assert!(!line.contains("ENVELOPE"), "{line}");
    assert_eq!(read.observation.requested_uids, [nz(7), nz(u32::MAX)]);
    assert_eq!(row_uids(&read.observation), [7, u32::MAX]);
    assert!(read.observation.missing_uids.is_empty());
    assert_eq!(read.observation.coverage, HeaderCoverage::Complete);
    assert_eq!(
        read.observation.mailbox,
        MailboxContext {
            name: "INBOX".into(),
            uid_validity: nz(EPOCH)
        }
    );
    assert!(read.notices.is_empty());
    assert_eq!(server.unexpected_count(), 0);
}

/// One row takes all required metadata from one frame even when the
/// UID item follows the literal: raw UTF-8 bytes, UTC milliseconds, the
/// size hint and a known empty flag list survive, and the frame's
/// sequence number is never a key.
#[tokio::test]
async fn one_whole_frame_carries_the_row_even_when_the_uid_item_follows_the_literal() {
    let request = full(&[5, 6], &["From", "subject"]);
    let mut items = vec![body(&echo("FROM SUBJECT"), SUBJECT_LINE.as_bytes())];
    items.extend([size(4242), date(), flags("\\Seen"), uid(5)]);
    let (server, mut client) = selected(vec![fetch(vec![
        frame(9, items),
        frame(1, row_at(6, "", 13, "FROM SUBJECT", b"Subject: six\r\n")),
    ])])
    .await;
    let read = client.fetch_headers(&request).await.unwrap();
    assert_eq!(
        read.observation.rows[0],
        HeaderRow {
            uid: nz(5),
            flags: vec!["\\Seen".to_string()],
            internal_date_ms: DATE_MS,
            rfc822_size: 4242,
            headers: HeaderBytes::Full(SUBJECT_LINE.as_bytes().to_vec()),
        }
    );
    assert_eq!(read.observation.rows[1].uid, nz(6));
    assert_eq!(read.observation.rows[1].internal_date_ms, DATE_MS);
    assert!(read.observation.rows[1].flags.is_empty(), "known empty");
    assert_eq!(
        read.observation.rows[1].headers,
        HeaderBytes::Full(b"Subject: six\r\n".to_vec())
    );
    assert_eq!(read.observation.coverage, HeaderCoverage::Complete);
    assert!(read.notices.is_empty());
    assert_eq!(server.unexpected_count(), 0);
}

/// NIL, a real empty section and a first prefix are three distinct
/// kinds of evidence. An unavailable row reports reconciliation; a
/// requested prefix alone never does, and a prefix is never complete.
#[tokio::test]
async fn nil_real_empty_and_first_prefix_sections_stay_distinct_evidence() {
    let (server, mut client) = selected(vec![
        fetch(vec![
            frame(1, row_nil(5, "SUBJECT")),
            frame(2, row_at(6, "", 0, "SUBJECT", b"")),
        ]),
        fetch(vec![frame(1, row_at(6, "", 0, "SUBJECT", b""))]),
        fetch(vec![frame(
            1,
            vec![uid(5), flags(""), date(), size(3), origin()],
        )]),
    ])
    .await;

    let pair = full(&[5, 6], &["SUBJECT"]);
    let read = client.fetch_headers(&pair).await.unwrap();
    assert_eq!(read.observation.rows[0].flags, ["\\Seen"]);
    assert_eq!(read.observation.rows[0].internal_date_ms, DATE_MS);
    assert_eq!(read.observation.rows[0].headers, HeaderBytes::Unavailable);
    assert!(read.observation.rows[1].flags.is_empty());
    assert_eq!(
        read.observation.rows[1].headers,
        HeaderBytes::Full(Vec::new())
    );
    assert_ne!(HeaderBytes::Unavailable, HeaderBytes::Full(Vec::new()));
    assert!(
        read.observation.missing_uids.is_empty(),
        "NIL is not missing"
    );
    assert_eq!(read.observation.coverage, HeaderCoverage::Incomplete);
    assert!(changed_inbox(&read), "unavailable rows reconcile");

    let single = full(&[6], &["SUBJECT"]);
    let read = client.fetch_headers(&single).await.unwrap();
    assert_eq!(
        read.observation.rows[0].headers,
        HeaderBytes::Full(Vec::new())
    );
    assert_eq!(read.observation.coverage, HeaderCoverage::Complete);
    assert_eq!(changed(&read), 0);

    let mode = HeaderFetchMode::Prefix { max_bytes: nz(4) };
    let prefix = request(&[5], &["SUBJECT"], mode);
    let read = client.fetch_headers(&prefix).await.unwrap();
    let line = server
        .received()
        .last()
        .expect("the prefix command")
        .line
        .clone();
    assert!(
        line.ends_with(
            "UID FETCH 5 (UID FLAGS INTERNALDATE RFC822.SIZE \
             BODY.PEEK[HEADER.FIELDS (SUBJECT)]<0.4>)"
        ),
        "exact prefix request wire form: {line}"
    );
    assert_eq!(
        read.observation.rows[0].headers,
        HeaderBytes::Prefix {
            bytes: b"Sub".to_vec(),
            limit: nz(4),
        }
    );
    assert!(read.observation.rows[0].flags.is_empty());
    assert!(read.observation.missing_uids.is_empty());
    // Three octets under a four-octet limit prove nothing about the
    // rest of the section, so the batch stays incomplete regardless.
    assert_eq!(read.observation.coverage, HeaderCoverage::Incomplete);
    assert_eq!(changed(&read), 0, "a requested prefix is no change hint");
    assert_eq!(server.unexpected_count(), 0);
}

/// Missing, unkeyed, unrequested, split and repeated frames yield
/// honest incomplete coverage with reconciliation notices: no row is
/// ever stitched together, and a later incomplete update removes an
/// earlier whole row rather than letting stale evidence stand.
#[tokio::test]
async fn incomplete_and_repeated_frames_yield_honest_coverage_without_stitching() {
    let complete = row(5, "SUBJECT", b"Sub");
    let without = |drop: usize| -> Vec<Vec<u8>> {
        complete
            .iter()
            .enumerate()
            .filter(|(at, _)| *at != drop)
            .map(|(_, item)| item.clone())
            .collect()
    };
    let other_uid = || {
        let mut items = complete.clone();
        items[0] = uid(8);
        items
    };
    let body_only = frame_bytes(2, vec![uid(5), body(&echo("SUBJECT"), b"Sub")]);
    for (label, frames) in [
        ("no frames", vec![]),
        ("missing UID item", vec![frame_bytes(1, without(0))]),
        ("missing FLAGS item", vec![frame_bytes(1, without(1))]),
        ("missing INTERNALDATE", vec![frame_bytes(1, without(2))]),
        ("missing RFC822.SIZE", vec![frame_bytes(1, without(3))]),
        ("missing body item", vec![frame_bytes(1, without(4))]),
        ("unrequested UID", vec![frame_bytes(1, other_uid())]),
        ("split frames", vec![frame_bytes(1, without(4)), body_only]),
    ] {
        let (server, mut client) =
            selected(vec![fetch(frames.into_iter().map(Reply::Raw).collect())]).await;
        let request = full(&[5], &["SUBJECT"]);
        let read = client.fetch_headers(&request).await.unwrap();
        assert!(read.observation.rows.is_empty(), "{label}");
        assert_eq!(read.observation.requested_uids, [nz(5)], "{label}");
        assert_eq!(read.observation.missing_uids, [nz(5)], "{label}");
        assert_eq!(read.observation.coverage, HeaderCoverage::Incomplete);
        assert!(changed_inbox(&read), "{label}");
        assert_eq!(server.unexpected_count(), 0, "{label}");
    }

    // A repeat keeps only the latest whole frame.
    let (server, mut client) = selected(vec![fetch(vec![
        frame(1, row_at(5, "\\Seen", 3, "SUBJECT", b"one")),
        frame(2, row_at(5, "\\Draft", 3, "SUBJECT", b"two")),
    ])])
    .await;
    let request = full(&[5], &["SUBJECT"]);
    let read = client.fetch_headers(&request).await.unwrap();
    assert_eq!(row_uids(&read.observation), [5]);
    assert_eq!(read.observation.rows[0].flags, ["\\Draft"]);
    assert_eq!(
        read.observation.rows[0].headers,
        HeaderBytes::Full(b"two".to_vec())
    );
    assert_eq!(read.observation.coverage, HeaderCoverage::Incomplete);
    assert!(changed_inbox(&read), "a repeat can hide an update");
    assert_eq!(server.unexpected_count(), 0);

    // A later incomplete update removes the earlier whole row.
    let (server, mut client) = selected(vec![fetch(vec![
        frame(1, row_at(5, "\\Seen", 3, "SUBJECT", b"one")),
        frame(2, vec![uid(5), flags("\\Draft"), date(), size(3)]),
    ])])
    .await;
    let request = full(&[5], &["SUBJECT"]);
    let read = client.fetch_headers(&request).await.unwrap();
    assert!(read.observation.rows.is_empty(), "stale row survived");
    assert_eq!(read.observation.missing_uids, [nz(5)]);
    assert_eq!(read.observation.coverage, HeaderCoverage::Incomplete);
    assert!(changed_inbox(&read));
    assert_eq!(server.unexpected_count(), 0);
}

/// A wrong echo or a duplicate attribute fails the exchange with the
/// typed UID FETCH cause even after a complete row arrived, so no
/// partial batch escapes and the failed wire stays closed.
#[tokio::test]
async fn a_wrong_echo_or_duplicate_attribute_fails_without_partial_rows() {
    let head = vec![uid(5), flags("\\Seen"), date(), size(3)];
    let good = body(&echo("SUBJECT"), b"Sub");
    for (label, extras) in [
        ("foreign echo", vec![body(&echo("FROM"), b"Sub")]),
        ("widened echo", vec![body(&echo("SUBJECT FROM"), b"Sub")]),
        ("whole section", vec![body("BODY[HEADER]", b"Sub")]),
        ("full-read origin", vec![origin()]),
        ("duplicate UID item", vec![good.clone(), uid(6)]),
        ("duplicate FLAGS", vec![good.clone(), flags("\\Draft")]),
        ("duplicate body item", vec![good.clone(), good.clone()]),
    ] {
        let mut items = head.clone();
        items.extend(extras);
        let (server, mut client) = selected(vec![fetch(vec![
            frame(1, row(5, "SUBJECT", b"Sub")),
            frame(2, items),
        ])])
        .await;
        let request = full(&[5], &["SUBJECT"]);
        match client.fetch_headers(&request).await {
            Err(ImapError::Command {
                operation: "UID FETCH",
                ..
            }) => {}
            other => panic!("{label}: typed failure without a partial batch: {other:?}"),
        }
        assert!(
            matches!(client.fetch_headers(&request).await, Err(ImapError::Closed)),
            "{label}"
        );
        assert_eq!(server.received().len(), 6, "{label}");
        assert_eq!(server.unexpected_count(), 0, "{label}");
    }
}

/// A missing selection and invalid local requests fail before any byte
/// reaches the wire and without closing the authenticated session.
#[tokio::test]
async fn invalid_requests_and_a_missing_selection_stay_local() {
    let (server, mut client) = connected(vec![
        Step::on(Expect::command("EXAMINE"), examine()),
        fetch(vec![frame(1, row(5, "SUBJECT", b"Sub"))]),
    ])
    .await;
    let request = full(&[5], &["SUBJECT"]);
    assert!(matches!(
        client.fetch_headers(&request).await,
        Err(ImapError::NotSelected)
    ));
    assert_eq!(server.received().len(), 4, "no command was sent");

    let names =
        |names: &[&str]| -> Vec<String> { names.iter().map(|name| name.to_string()).collect() };
    for (want, got) in [
        HeaderRequestError::EmptyUids,
        HeaderRequestError::TooManyUids,
        HeaderRequestError::EmptyFields,
        HeaderRequestError::InvalidFieldName,
    ]
    .into_iter()
    .zip([
        HeaderRequest::new(Vec::new(), names(&["SUBJECT"]), HeaderFetchMode::Full),
        HeaderRequest::new(
            (1..=251).map(nz).collect(),
            names(&["SUBJECT"]),
            HeaderFetchMode::Full,
        ),
        HeaderRequest::new(vec![nz(5)], Vec::new(), HeaderFetchMode::Full),
        HeaderRequest::new(vec![nz(5)], names(&["SUB JECT"]), HeaderFetchMode::Full),
    ]) {
        assert_eq!(got.unwrap_err(), want);
    }
    assert_eq!(server.received().len(), 4, "local failures sent nothing");

    // The local precondition failure did not close the session.
    client.examine("INBOX").await.unwrap();
    let read = client.fetch_headers(&request).await.unwrap();
    assert_eq!(row_uids(&read.observation), [5]);
    assert_eq!(server.unexpected_count(), 0);
}

/// Only the first matched tagged completion can finish a fetch: a
/// wrong tag or a refusal fails it, a contradicted epoch fails it with
/// its typed cause, and terminal tails leave no live read behind (the
/// same tails setup_terminal.rs keeps out of a live connection).
#[tokio::test]
async fn wrong_tags_epoch_changes_and_terminal_tails_never_complete_a_fetch() {
    let row_reply = || frame(1, row(5, "SUBJECT", b"Sub"));
    let fetched = || Reply::ok("fetched");
    let no = Reply::TaggedNo("refused".into());
    let bad = Reply::TaggedBad("refused".into());
    let wrong = Reply::Raw(b"WRONG OK stale\r\n".to_vec());
    let bye = Reply::untagged("BYE closing");
    let garbage = raw(b"not IMAP\r\n");
    let stale_epoch = Reply::untagged("OK [UIDVALIDITY 42] stale");
    let tagged_epoch = Reply::ok("[UIDVALIDITY 42] stale");
    for (label, epoch_invalidated, first, tail) in [
        ("tagged NO", false, no, fetched()),
        ("tagged BAD", false, bad, fetched()),
        ("wrong tag", false, wrong, fetched()),
        ("duplicate tail", false, fetched(), fetched()),
        ("BYE tail", false, fetched(), bye),
        ("malformed tail", false, fetched(), garbage),
        ("epoch change", true, stale_epoch, fetched()),
        ("tagged epoch", true, tagged_epoch, fetched()),
    ] {
        let sends = vec![row_reply(), first, tail];
        let (server, mut client, result) = run_fetch(sends).await;
        if epoch_invalidated {
            assert!(
                matches!(&result, Err(ImapError::SelectionInvalidated)),
                "{label}: {result:?}"
            );
        } else {
            assert!(result.is_err(), "{label} completed a fetch: {result:?}");
        }
        let request = full(&[5], &["SUBJECT"]);
        assert!(
            matches!(client.fetch_headers(&request).await, Err(ImapError::Closed)),
            "{label}"
        );
        assert_eq!(server.received().len(), 6, "{label}");
        assert_eq!(server.unexpected_count(), 0, "{label}");
    }
}

/// Several change triggers in one exchange coalesce into exactly one
/// reconciliation notice while the observed row stands on its own.
#[tokio::test]
async fn change_hints_coalesce_into_one_reconciliation_notice() {
    let (server, mut client) = selected(vec![fetch(vec![
        frame(1, row(5, "SUBJECT", b"Sub")),
        Reply::untagged("3 EXISTS"),
        Reply::untagged("9 EXPUNGE"),
        Reply::untagged("OK [UIDNEXT 9] advanced"),
    ])])
    .await;
    let request = full(&[5], &["SUBJECT"]);
    let read = client.fetch_headers(&request).await.unwrap();
    assert_eq!(
        row_uids(&read.observation),
        [5],
        "the row is still observed"
    );
    assert_eq!(read.observation.coverage, HeaderCoverage::Incomplete);
    assert_eq!(changed(&read), 1, "notices coalesce: {:?}", read.notices);
    assert!(changed_inbox(&read));
    assert_eq!(server.unexpected_count(), 0);
}

/// Dropping the operation while its command is pending closes the
/// wire before any other command can run.
#[tokio::test]
async fn cancelling_a_pending_header_fetch_closes_the_connection() {
    let (server, mut client) = selected(vec![Step::on(
        Expect::command("UID FETCH"),
        vec![Reply::Delay(Duration::from_millis(500)), Reply::ok("late")],
    )])
    .await;
    let request = full(&[5], &["SUBJECT"]);
    let mut operation = Box::pin(client.fetch_headers(&request));
    tokio::select! {
        result = &mut operation => panic!("completed before cancellation: {result:?}"),
        seen = tokio::time::timeout(Duration::from_secs(2), async {
            while server.received().len() != 6 { tokio::task::yield_now().await; }
        }) => seen.unwrap(),
    }
    drop(operation);
    assert!(matches!(
        client.fetch_headers(&request).await,
        Err(ImapError::Closed)
    ));
    assert_eq!(server.received().len(), 6);
}

/// Stale frames left over from a predecessor boundary, complete or
/// still partial, stay reconciliation evidence and never fill the
/// rows of the request that follows.
#[tokio::test]
async fn stale_predecessor_frames_never_fill_a_new_request() {
    for partial in [false, true] {
        let stale = frame_bytes(2, row(5, "SUBJECT", b"Subject: stale\r\n"));
        let mut tail = Vec::new();
        if partial {
            let split = stale.len() / 2;
            tail.push(Reply::Raw(stale[..split].to_vec()));
            tail.push(Reply::Delay(Duration::from_millis(30)));
            tail.push(Reply::Raw(stale[split..].to_vec()));
        } else {
            tail.push(Reply::Raw(stale));
        }

        // Stale data trailing the predecessor's completion.
        let mut first = vec![Reply::Raw(frame_bytes(
            1,
            row(5, "SUBJECT", b"Subject: fresh\r\n"),
        ))];
        first.push(Reply::ok("fetched"));
        first.extend(tail.clone());
        let (server, mut client) = selected(vec![
            Step::on(Expect::command("UID FETCH"), first),
            fetch(vec![]),
        ])
        .await;
        let request = full(&[5], &["SUBJECT"]);
        let batch = client.fetch_headers(&request).await.unwrap().observation;
        assert_eq!(row_uids(&batch), [5]);
        assert_eq!(
            batch.rows[0].headers,
            HeaderBytes::Full(b"Subject: fresh\r\n".to_vec())
        );
        let read = client.fetch_headers(&request).await.unwrap();
        assert!(read.observation.rows.is_empty(), "stale data counted");
        assert_eq!(read.observation.missing_uids, [nz(5)]);
        assert_eq!(read.observation.coverage, HeaderCoverage::Incomplete);
        assert!(changed_inbox(&read));
        assert_eq!(server.received().len(), 7);
        assert_eq!(server.unexpected_count(), 0);

        // Stale data buffered before the first command.
        let mut replies = examine();
        replies.extend(tail);
        let (server, mut client) = selected_with(replies, vec![fetch(vec![])]).await;
        let request = full(&[5], &["SUBJECT"]);
        let read = client.fetch_headers(&request).await.unwrap();
        assert!(read.observation.rows.is_empty(), "pre-command data counted");
        assert_eq!(read.observation.missing_uids, [nz(5)]);
        assert_eq!(read.observation.coverage, HeaderCoverage::Incomplete);
        assert!(changed_inbox(&read));
        assert_eq!(server.received().len(), 6);
        assert_eq!(server.unexpected_count(), 0);
    }
}

/// The typed frame guard refuses an oversized literal announcement
/// before its bytes or EOF arrive, and the failed exchange closes.
#[tokio::test]
async fn an_oversized_literal_announcement_is_refused_before_its_bytes_arrive() {
    let (server, mut client) = selected(vec![Step::on(
        Expect::command("UID FETCH"),
        vec![Reply::Raw(
            format!(
                "* 1 FETCH (UID 7 BODY[HEADER.FIELDS (SUBJECT)] {{{}}}\r\n",
                50 * 1024 * 1024 + 1
            )
            .into_bytes(),
        )],
    )])
    .await;
    let request = full(&[7], &["SUBJECT"]);
    let result = tokio::time::timeout(Duration::from_secs(2), client.fetch_headers(&request))
        .await
        .expect("the typed frame guard must fail without literal bytes or EOF");
    assert!(
        matches!(&result, Err(ImapError::FrameBudgetExceeded)),
        "{result:?}"
    );
    assert!(matches!(
        client.fetch_headers(&request).await,
        Err(ImapError::Closed)
    ));
    assert_eq!(server.received().len(), 6);
}

/// The profile's own header answer carries only the UID item and the
/// literal. Required metadata cannot be fabricated, so none of its
/// frames is a row and the batch reports honest incomplete coverage.
#[tokio::test]
async fn bridge_profile_header_frames_without_metadata_yield_no_fabricated_rows() {
    let fields = "DATE FROM REPLY-TO TO CC SUBJECT MESSAGE-ID IN-REPLY-TO REFERENCES \
                  LIST-ID DELIVERED-TO X-ORIGINAL-TO X-PM-INTERNAL-ID";
    let names: Vec<&str> = fields.split_whitespace().collect();
    let request = full(&[7], &names);
    let (server, mut client) = selected(vec![Profile::bridge().fetch_headers_step("INBOX")]).await;
    let read = client.fetch_headers(&request).await.unwrap();
    assert!(
        read.observation.rows.is_empty(),
        "metadata never fabricated"
    );
    assert_eq!(read.observation.requested_uids, [nz(7)]);
    assert_eq!(read.observation.missing_uids, [nz(7)]);
    assert_eq!(read.observation.coverage, HeaderCoverage::Incomplete);
    assert!(changed_inbox(&read));
    assert_eq!(server.unexpected_count(), 0);
}
