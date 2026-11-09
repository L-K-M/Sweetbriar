//! Account workers over the fake Bridge server and the fake secret
//! store (spec/04 "Account states and health"): a connect keeps the
//! account healthy; a refused connection shows "Bridge isn't running"
//! while retries continue; a refused login, a changed certificate or a
//! missing credential pauses the account and ends the worker; shutdown
//! stops a worker even while it waits to retry. A live session's pass
//! syncs every selectable mailbox's headers (spec/04 steps 1-3).

use std::sync::Arc;
use std::time::{Duration, Instant};

use sb_resources::{SecretStore, WritePriority};
use sb_test_support::{
    Expect, FakeImapServer, FakeSecrets, Profile, Reply, ScratchDir, Script, Step, block_on,
    cert_der,
};

use crate::{
    AccountState, AddressMode, BridgeAccountParams, BridgeIdentity, CertificatePin,
    EndpointSecurity, Engine, IdentitySendable, Secret,
};

const PASSWORD: &str = "bridge-worker-secret-5E1B";
const DEADLINE: Duration = Duration::from_secs(20);

/// The Bridge profile's selectable mailboxes in the order a pass
/// visits them: INBOX, then by name.
const SYNC_ORDER: [&str; 10] = [
    "INBOX",
    "All Mail",
    "Archive",
    "Drafts",
    "Folders/Travel",
    "Labels/Receipts",
    "Sent",
    "Spam",
    "Starred",
    "Trash",
];

/// Where the profile's one shared message appears: `(mailbox, uid,
/// seen, flagged)`.
const MEMBERS: [(&str, i64, bool, bool); 5] = [
    ("All Mail", 9, true, true),
    ("Folders/Travel", 3, false, false),
    ("INBOX", 7, true, false),
    ("Labels/Receipts", 5, true, true),
    ("Starred", 2, false, true),
];

/// The shared message's `X-Pm-Internal-Id`.
const PROVIDER_KEY: &str = "pm-7f3a9c21";

/// Greeting to authenticated session; `login_ok` false refuses LOGIN.
fn login_steps(profile: &Profile, login_ok: bool) -> Vec<Step> {
    let login = Step::on(
        Expect::command("LOGIN"),
        vec![if login_ok {
            Reply::ok("LOGIN completed")
        } else {
            Reply::TaggedNo("LOGIN failed".to_owned())
        }],
    );
    let mut steps = vec![profile.capability_step(), login];
    if login_ok {
        steps.extend([
            profile.capability_step(),
            Step::on(
                Expect::command("ID"),
                vec![
                    Reply::untagged("ID (\"name\" \"Proton Mail Bridge\")"),
                    Reply::ok("ID completed"),
                ],
            ),
            profile.capability_step(),
        ]);
    }
    steps
}

/// The setup probe's conversation; `login_ok` false refuses LOGIN.
fn bridge_script(login_ok: bool) -> Script {
    let mut steps = login_steps(&Profile::bridge(), login_ok);
    if login_ok {
        steps.extend([
            Step::on(
                Expect::command("LIST"),
                vec![
                    Reply::untagged("LIST () \"/\" INBOX"),
                    Reply::ok("LIST completed"),
                ],
            ),
            Step::on(
                Expect::command("LOGOUT"),
                vec![Reply::untagged("BYE"), Reply::ok("LOGOUT completed")],
            ),
        ]);
    }
    Script::new(steps).implicit_tls()
}

/// What INBOX shows a worker's session.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Inbox {
    /// The shared message at UID 7.
    Listed,
    /// Bridge has not materialized it yet: no UIDs, UIDNEXT 8 all
    /// the same.
    Late,
    /// Its header fetch stalls for `STALL` before answering.
    Stalled,
    /// A second message at UID 8 whose header section alone exceeds a
    /// batch's write budget.
    Oversized,
}

/// The header fields the sync loop asks for, as the server echoes them.
const HEADER_FIELDS: &str = "DATE FROM REPLY-TO TO CC SUBJECT MESSAGE-ID IN-REPLY-TO \
     REFERENCES LIST-ID DELIVERED-TO X-ORIGINAL-TO X-PM-INTERNAL-ID";

/// INBOX's answers when it also holds the oversized message.
fn oversized_inbox(profile: &Profile) -> Vec<Step> {
    let mut references = String::from("References:");
    while references.len() < 3 * 1024 * 1024 {
        references.push_str(" <ref-0123456789abcdef@lists.example.org>\r\n");
    }
    let literal = format!(
        "Subject: a very long thread\r\nMessage-ID: <long@example.org>\r\n{references}\
         X-Pm-Internal-Id: pm-oversized\r\n\r\n"
    );
    let mut rows = profile.header_rows_step("INBOX");
    let ok = rows.send.pop().expect("the tagged OK");
    rows.send.push(Reply::UntaggedLiteral {
        head: format!(
            "2 FETCH (UID 8 FLAGS () INTERNALDATE \"02-Nov-2026 10:00:00 +0000\" \
             RFC822.SIZE 4096 BODY[HEADER.FIELDS ({HEADER_FIELDS})]"
        ),
        bytes: literal.into_bytes(),
        tail: ")".to_owned(),
    });
    rows.send.push(ok);
    vec![
        Step::on(
            Expect::command("EXAMINE INBOX"),
            vec![
                Reply::untagged("FLAGS (\\Seen \\Answered \\Flagged \\Deleted \\Draft)"),
                Reply::untagged("2 EXISTS"),
                Reply::untagged("0 RECENT"),
                Reply::untagged("OK [UIDVALIDITY 4100] UIDs valid"),
                Reply::untagged("OK [UIDNEXT 9] Predicted next UID"),
                Reply::ok("[READ-ONLY] EXAMINE completed"),
            ],
        ),
        Step::on(
            Expect::command("UID SEARCH ALL"),
            vec![
                Reply::untagged("SEARCH 7 8"),
                Reply::ok("UID SEARCH completed"),
            ],
        ),
        rows,
    ]
}

const STALL: Duration = Duration::from_secs(8);

/// A worker's session: login, then one pass over the Bridge profile
/// (LIST, then EXAMINE, UID SEARCH and the header rows per mailbox in
/// pass order, UNSELECT between selections).
fn sync_script(inbox: Inbox) -> Script {
    refusing_script(inbox, None)
}

/// `sync_script`, but Bridge answers EXAMINE of `refused` with a tagged
/// NO (a mailbox it cannot serve right now).
fn refusing_script(inbox: Inbox, refused: Option<&str>) -> Script {
    let profile = Profile::bridge();
    let mut steps = login_steps(&profile, true);
    steps.push(profile.list_step());
    for (index, mailbox) in SYNC_ORDER.into_iter().enumerate() {
        if index > 0 {
            steps.push(Step::on(
                Expect::command("UNSELECT"),
                vec![Reply::ok("UNSELECT completed")],
            ));
        }
        if refused == Some(mailbox) {
            steps.push(Step::on(
                Expect::command(format!("EXAMINE {mailbox}")),
                vec![Reply::TaggedNo("mailbox unavailable".to_owned())],
            ));
            continue;
        }
        if mailbox == "INBOX" && inbox == Inbox::Oversized {
            steps.extend(oversized_inbox(&profile));
            continue;
        }
        steps.push(profile.examine_step(mailbox));
        if mailbox == "INBOX" && inbox == Inbox::Late {
            steps.push(Step::on(
                Expect::command("UID SEARCH ALL"),
                vec![Reply::untagged("SEARCH"), Reply::ok("UID SEARCH completed")],
            ));
            continue;
        }
        steps.push(profile.uid_search_step(mailbox));
        if MEMBERS.iter().any(|member| member.0 == mailbox) {
            let mut rows = profile.header_rows_step(mailbox);
            if mailbox == "INBOX" && inbox == Inbox::Stalled {
                rows.send.insert(0, Reply::Delay(STALL));
            }
            steps.push(rows);
        }
    }
    // The IDLE watcher's session, started after the pass, idles on
    // INBOX without a hint.
    steps.push(Step::on(
        Expect::command("IDLE"),
        vec![Reply::Continuation("idling".to_owned())],
    ));
    Script::new(steps).implicit_tls()
}

/// How long the watcher's IDLE runs before Bridge reports news.
const HINT_DELAY: Duration = Duration::from_millis(500);

/// A session for either of the account's connections, as their order
/// is not fixed: the pass, an IDLE that reports a second INBOX message
/// after `HINT_DELAY`, and the INBOX pass that hint wakes.
fn hinting_script() -> Script {
    let mut script = sync_script(Inbox::Listed);
    let idle = script.steps.pop().expect("the quiet IDLE");
    assert_eq!(idle.send.len(), 1);
    let literal = "Subject: gate change\r\nMessage-ID: <msg-8@example.org>\r\n\
                   X-Pm-Internal-Id: pm-8b2d4e60\r\n\r\n";
    script.steps.extend([
        Step::on(
            Expect::command("IDLE"),
            vec![
                Reply::Continuation("idling".to_owned()),
                Reply::Delay(HINT_DELAY),
                Reply::untagged("2 EXISTS"),
            ],
        ),
        Step::on(
            Expect::command("UNSELECT"),
            vec![Reply::ok("UNSELECT completed")],
        ),
        Step::on(
            Expect::command("EXAMINE INBOX"),
            vec![
                Reply::untagged("FLAGS (\\Seen \\Answered \\Flagged \\Deleted \\Draft)"),
                Reply::untagged("2 EXISTS"),
                Reply::untagged("0 RECENT"),
                Reply::untagged("OK [UIDVALIDITY 4100] UIDs valid"),
                Reply::untagged("OK [UIDNEXT 9] Predicted next UID"),
                Reply::ok("[READ-ONLY] EXAMINE completed"),
            ],
        ),
        Step::on(
            Expect::command("UID SEARCH ALL"),
            vec![
                Reply::untagged("SEARCH 7 8"),
                Reply::ok("UID SEARCH completed"),
            ],
        ),
        Step::on(
            Expect::command("UID FETCH"),
            vec![
                Reply::UntaggedLiteral {
                    head: format!(
                        "2 FETCH (UID 8 FLAGS () INTERNALDATE \"02-Nov-2026 10:00:00 +0000\" \
                         RFC822.SIZE 1024 BODY[HEADER.FIELDS ({HEADER_FIELDS})]"
                    ),
                    bytes: literal.as_bytes().to_vec(),
                    tail: ")".to_owned(),
                },
                Reply::ok("UID FETCH completed"),
            ],
        ),
    ]);
    script
}

/// `(remote_name, sync_state, uidnext)` of the account's mailboxes, by
/// name.
fn mailboxes(engine: &Engine, account: &str) -> Vec<(String, String, Option<i64>)> {
    let account = account.to_owned();
    engine
        .resources()
        .read(move |tx| {
            let mut statement = tx
                .prepare(
                    "SELECT remote_name, sync_state, uidnext FROM mailboxes
                     WHERE account_id = ?1 ORDER BY remote_name",
                )
                .unwrap();
            let rows = statement
                .query_map([account], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
                .unwrap()
                .map(Result::unwrap)
                .collect();
            Ok(rows)
        })
        .unwrap()
}

/// `(mailbox, uid, seen, flagged, provider_key)` of the account's
/// current occurrences, by mailbox name.
fn occurrences(engine: &Engine, account: &str) -> Vec<(String, i64, bool, bool, String)> {
    let account = account.to_owned();
    engine
        .resources()
        .read(move |tx| {
            let mut statement = tx
                .prepare(
                    "SELECT m.remote_name, o.uid, o.f_seen, o.f_flagged, l.provider_key
                     FROM occurrences o
                     JOIN mailboxes m ON m.mailbox_id = o.mailbox_id
                     JOIN logical_messages l ON l.message_id = o.message_id
                     WHERE o.account_id = ?1 AND o.state = 'present'
                     ORDER BY m.remote_name",
                )
                .unwrap();
            let rows = statement
                .query_map([account], |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                    ))
                })
                .unwrap()
                .map(Result::unwrap)
                .collect();
            Ok(rows)
        })
        .unwrap()
}

/// The account's logical messages and stored header literals.
fn messages(engine: &Engine, account: &str) -> (i64, i64) {
    let account = account.to_owned();
    engine
        .resources()
        .read(move |tx| {
            Ok(tx
                .query_row(
                    "SELECT (SELECT count(*) FROM logical_messages WHERE account_id = ?1),
                            (SELECT count(*) FROM message_headers h
                             JOIN logical_messages l ON l.message_id = h.message_id
                             WHERE l.account_id = ?1 AND h.subject = 'Re: boarding pass')",
                    [account],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .unwrap())
        })
        .unwrap()
}

/// Whether every selectable mailbox finished its pass.
fn all_synced(engine: &Engine, account: &str) -> bool {
    mailboxes(engine, account)
        .iter()
        .filter(|(_, state, _)| state == "synced")
        .count()
        == SYNC_ORDER.len()
}

fn commands(server: &FakeImapServer, prefix: &str) -> usize {
    server
        .received()
        .iter()
        .filter(|command| {
            command
                .line
                .split_once(' ')
                .is_some_and(|(_, rest)| rest.starts_with(prefix))
        })
        .count()
}

fn params(port: u16) -> BridgeAccountParams {
    BridgeAccountParams {
        display_name: "Owner Proton".to_owned(),
        username: "owner@example.org".to_owned(),
        password: Secret::new(PASSWORD.as_bytes().to_vec()),
        imap_port: port,
        imap_security: EndpointSecurity::Tls,
        smtp_port: 1025,
        smtp_security: EndpointSecurity::StartTls,
        pin: CertificatePin::of_der(&cert_der()),
        address_mode: Some(AddressMode::Combined),
        identities: vec![BridgeIdentity {
            email: "owner@example.org".to_owned(),
            display_name: "Owner".to_owned(),
            is_default: true,
            sendable: IdentitySendable::Provisional,
        }],
    }
}

fn open(scratch: &ScratchDir, secrets: &Arc<FakeSecrets>) -> Engine {
    let store: Arc<dyn SecretStore> = secrets.clone();
    block_on(Engine::open_with_secrets(
        &scratch.path().join("store"),
        store,
    ))
    .expect("the engine opens")
}

/// Add the account through setup (whose probe uses the first script)
/// and answer its id.
fn add(engine: &Engine, port: u16) -> String {
    block_on(engine.add_bridge_account("setup", params(port)))
        .expect("setup")
        .account_id
}

/// `(state, reason)` of the account.
fn state(engine: &Engine, account_id: &str) -> (AccountState, Option<String>) {
    let accounts = block_on(engine.accounts()).unwrap();
    let account = accounts
        .into_iter()
        .find(|account| account.account_id == account_id)
        .expect("the account is listed");
    (account.state, account.state_reason)
}

/// Poll until `done` holds or the deadline passes.
fn eventually(what: &str, mut done: impl FnMut() -> bool) {
    let start = Instant::now();
    while !done() {
        assert!(start.elapsed() < DEADLINE, "timed out waiting: {what}");
        std::thread::sleep(Duration::from_millis(20));
    }
}

fn shutdown_promptly(engine: Engine) {
    let start = Instant::now();
    block_on(engine.shutdown()).unwrap();
    assert!(
        start.elapsed() < Duration::from_secs(5),
        "shutdown waited {:?} for the account worker",
        start.elapsed()
    );
}

fn logins(server: &FakeImapServer) -> usize {
    server
        .received()
        .iter()
        .filter(|command| command.line.contains("LOGIN"))
        .count()
}

#[test]
fn an_added_account_s_pass_syncs_every_mailbox_into_one_logical_message() {
    let scratch = ScratchDir::new("account-worker-pass");
    let secrets = Arc::new(FakeSecrets::new());
    let server =
        FakeImapServer::serve(vec![bridge_script(true), sync_script(Inbox::Listed)]).unwrap();
    let engine = open(&scratch, &secrets);

    let account = add(&engine, server.port());

    eventually("the first pass", || all_synced(&engine, &account));
    assert!(engine.account_jobs().running(&account));
    assert_eq!(state(&engine, &account), (AccountState::Active, None));
    // Every selectable mailbox checkpoints at the server's UIDNEXT; the
    // `\Noselect` parents are listed but never examined.
    let expected: Vec<(String, String, Option<i64>)> = [
        ("All Mail", "synced", Some(10)),
        ("Archive", "synced", Some(1)),
        ("Drafts", "synced", Some(1)),
        ("Folders", "new", None),
        ("Folders/Travel", "synced", Some(4)),
        ("INBOX", "synced", Some(8)),
        ("Labels", "new", None),
        ("Labels/Receipts", "synced", Some(6)),
        ("Sent", "synced", Some(1)),
        ("Spam", "synced", Some(1)),
        ("Starred", "synced", Some(3)),
        ("Trash", "synced", Some(1)),
    ]
    .map(|(name, state, uidnext)| (name.to_owned(), state.to_owned(), uidnext))
    .into();
    assert_eq!(mailboxes(&engine, &account), expected);
    // Five memberships, one logical message by X-Pm-Internal-Id, its
    // headers stored once.
    let expected: Vec<(String, i64, bool, bool, String)> = MEMBERS
        .map(|(mailbox, uid, seen, flagged)| {
            (
                mailbox.to_owned(),
                uid,
                seen,
                flagged,
                PROVIDER_KEY.to_owned(),
            )
        })
        .into();
    assert_eq!(occurrences(&engine, &account), expected);
    assert_eq!(messages(&engine, &account), (1, 1));
    assert_eq!(server.unexpected_count(), 0);
    shutdown_promptly(engine);
}

#[test]
fn a_restarted_worker_resumes_from_its_checkpoints_without_duplicates() {
    let scratch = ScratchDir::new("account-worker-resume");
    let secrets = Arc::new(FakeSecrets::new());
    let server =
        FakeImapServer::serve(vec![bridge_script(true), sync_script(Inbox::Listed)]).unwrap();
    let engine = open(&scratch, &secrets);
    let account = add(&engine, server.port());
    eventually("the first pass", || all_synced(&engine, &account));
    shutdown_promptly(engine);

    let engine = open(&scratch, &secrets);

    eventually("the second pass", || {
        commands(&server, "UID SEARCH") >= 2 * SYNC_ORDER.len()
    });
    // Nothing new: no header fetch, no second copy of anything.
    assert_eq!(commands(&server, "UID FETCH"), MEMBERS.len());
    assert_eq!(occurrences(&engine, &account).len(), MEMBERS.len());
    assert_eq!(messages(&engine, &account), (1, 1));
    assert!(all_synced(&engine, &account));
    shutdown_promptly(engine);
}

#[test]
fn a_message_bridge_materialized_late_is_backfilled_below_the_checkpoint() {
    let scratch = ScratchDir::new("account-worker-backfill");
    let secrets = Arc::new(FakeSecrets::new());
    let server = FakeImapServer::serve(vec![
        bridge_script(true),
        sync_script(Inbox::Late),
        sync_script(Inbox::Listed),
    ])
    .unwrap();
    let engine = open(&scratch, &secrets);
    let account = add(&engine, server.port());
    eventually("the first pass", || all_synced(&engine, &account));
    // INBOX checkpointed past UID 7 without it.
    let inbox = |engine: &Engine| {
        mailboxes(engine, &account)
            .into_iter()
            .find(|(name, _, _)| name == "INBOX")
            .unwrap()
    };
    assert_eq!(inbox(&engine).2, Some(8));
    assert_eq!(occurrences(&engine, &account).len(), MEMBERS.len() - 1);
    shutdown_promptly(engine);

    let engine = open(&scratch, &secrets);

    eventually("the backfill", || {
        occurrences(&engine, &account).len() == MEMBERS.len()
    });
    assert_eq!(
        inbox(&engine),
        ("INBOX".to_owned(), "synced".to_owned(), Some(8))
    );
    assert_eq!(messages(&engine, &account), (1, 1));
    shutdown_promptly(engine);
}

#[test]
fn a_header_too_large_for_any_batch_never_stalls_its_mailbox() {
    let scratch = ScratchDir::new("account-worker-oversized");
    let secrets = Arc::new(FakeSecrets::new());
    let server =
        FakeImapServer::serve(vec![bridge_script(true), sync_script(Inbox::Oversized)]).unwrap();
    let engine = open(&scratch, &secrets);
    let account = add(&engine, server.port());

    // The batch of UIDs 7 and 8 is over budget: halved, 7 commits and
    // 8 alone is left out, while the checkpoint still moves past it.
    eventually("the first pass", || {
        mailboxes(&engine, &account)
            .iter()
            .any(|row| *row == ("INBOX".to_owned(), "backfilling".to_owned(), Some(9)))
    });
    eventually("the rest of the pass", || {
        mailboxes(&engine, &account)
            .iter()
            .filter(|(_, state, _)| state == "synced")
            .count()
            == SYNC_ORDER.len() - 1
    });
    let inbox: Vec<i64> = occurrences(&engine, &account)
        .into_iter()
        .filter(|row| row.0 == "INBOX")
        .map(|row| row.1)
        .collect();
    assert_eq!(inbox, [7]);
    assert_eq!(messages(&engine, &account), (1, 1));
    shutdown_promptly(engine);
}

#[test]
fn a_mailbox_bridge_refuses_is_left_out_while_the_rest_sync() {
    let scratch = ScratchDir::new("account-worker-refused-mailbox");
    let secrets = Arc::new(FakeSecrets::new());
    let server = FakeImapServer::serve(vec![
        bridge_script(true),
        // INBOX, the first mailbox a pass visits: every later session
        // then answers exactly as a fresh one.
        refusing_script(Inbox::Listed, Some("INBOX")),
    ])
    .unwrap();
    let engine = open(&scratch, &secrets);
    let account = add(&engine, server.port());

    // The refusal ends the session; the worker reconnects at once and
    // its next pass leaves INBOX out instead of failing on it again.
    eventually("the rest of the pass", || {
        mailboxes(&engine, &account)
            .iter()
            .filter(|(_, state, _)| state == "synced")
            .count()
            == SYNC_ORDER.len() - 1
    });
    let inbox = mailboxes(&engine, &account)
        .into_iter()
        .find(|(name, _, _)| name == "INBOX")
        .unwrap();
    assert_eq!(inbox, ("INBOX".to_owned(), "new".to_owned(), None));
    // The worker's sessions are the ones that LIST after the probe's;
    // the IDLE watcher's own session also selects INBOX, refused too.
    let received = server.received();
    let worker_sessions: Vec<usize> = received
        .iter()
        .filter(|command| command.conn > 0 && command.line.contains(" LIST "))
        .map(|command| command.conn)
        .collect();
    let inbox_selections = received
        .iter()
        .filter(|command| {
            worker_sessions.contains(&command.conn) && command.line.ends_with("EXAMINE INBOX")
        })
        .count();
    assert_eq!(worker_sessions.len(), 2, "the refused session and the next");
    assert_eq!(inbox_selections, 1, "never asked again");
    assert_eq!(occurrences(&engine, &account).len(), MEMBERS.len() - 1);
    assert_eq!(state(&engine, &account), (AccountState::Active, None));
    shutdown_promptly(engine);
}

#[test]
fn an_idle_hint_wakes_an_inbox_pass() {
    let scratch = ScratchDir::new("account-worker-idle");
    let secrets = Arc::new(FakeSecrets::new());
    let server = FakeImapServer::serve(vec![bridge_script(true), hinting_script()]).unwrap();
    let engine = open(&scratch, &secrets);
    let account = add(&engine, server.port());

    eventually("the hinted message", || {
        occurrences(&engine, &account)
            .iter()
            .filter(|row| row.0 == "INBOX")
            .count()
            == 2
    });
    // The watcher idled on its own session; the hint woke INBOX alone
    // (one LIST, one more INBOX selection).
    assert!(commands(&server, "IDLE") >= 1);
    assert_eq!(commands(&server, "LIST"), 2, "the probe's and the pass's");
    let (logical, _) = messages(&engine, &account);
    assert_eq!(logical, 2);
    shutdown_promptly(engine);
}

#[test]
fn shutdown_interrupts_a_pass_waiting_on_the_server() {
    let scratch = ScratchDir::new("account-worker-stall");
    let secrets = Arc::new(FakeSecrets::new());
    let server =
        FakeImapServer::serve(vec![bridge_script(true), sync_script(Inbox::Stalled)]).unwrap();
    let engine = open(&scratch, &secrets);
    let _account = add(&engine, server.port());

    eventually("the stalled fetch", || commands(&server, "UID FETCH") >= 1);

    shutdown_promptly(engine);
}

#[test]
fn a_refused_login_pauses_the_account_and_ends_the_worker() {
    let scratch = ScratchDir::new("account-worker-login");
    let secrets = Arc::new(FakeSecrets::new());
    // The probe logs in; every later connection is refused.
    let server = FakeImapServer::serve(vec![bridge_script(true), bridge_script(false)]).unwrap();
    let engine = open(&scratch, &secrets);

    let account = add(&engine, server.port());

    eventually("the login pause", || {
        state(&engine, &account).1.as_deref()
            == Some("Bridge password changed or account signed out in Bridge")
    });
    eventually("the worker ends", || {
        !engine.account_jobs().running(&account)
    });
    shutdown_promptly(engine);
}

#[test]
fn a_changed_certificate_pauses_the_account() {
    let scratch = ScratchDir::new("account-worker-pin");
    let secrets = Arc::new(FakeSecrets::new());
    let server =
        FakeImapServer::serve(vec![bridge_script(true), sync_script(Inbox::Listed)]).unwrap();
    let engine = open(&scratch, &secrets);
    let account = add(&engine, server.port());
    eventually("the first worker logs in", || logins(&server) >= 2);
    // Bridge reinstalled: its certificate no longer matches the pin.
    let id = account.clone();
    block_on(engine.resources().write(WritePriority::User, move |tx| {
        tx.execute(
            "UPDATE imap_endpoints SET trusted_cert_sha256 = ?2 WHERE account_id = ?1",
            [id.as_str(), &"cd".repeat(32)],
        )
        .unwrap();
        Ok(())
    }))
    .unwrap();
    shutdown_promptly(engine);

    let engine = open(&scratch, &secrets);

    eventually("the certificate pause", || {
        state(&engine, &account).1.as_deref() == Some("Bridge's certificate changed")
    });
    eventually("the worker ends", || {
        !engine.account_jobs().running(&account)
    });
    shutdown_promptly(engine);
}

#[test]
fn bridge_not_running_shows_while_retries_continue_and_shutdown_still_stops() {
    let scratch = ScratchDir::new("account-worker-refused");
    let secrets = Arc::new(FakeSecrets::new());
    let server =
        FakeImapServer::serve(vec![bridge_script(true), sync_script(Inbox::Listed)]).unwrap();
    let engine = open(&scratch, &secrets);
    let account = add(&engine, server.port());
    eventually("the first worker logs in", || logins(&server) >= 2);
    shutdown_promptly(engine);
    // Bridge quits: nothing listens on its port any more.
    drop(server);

    let engine = open(&scratch, &secrets);

    eventually("the not-running pause", || {
        state(&engine, &account).1.as_deref() == Some("Bridge isn't running")
    });
    assert!(
        engine.account_jobs().running(&account),
        "the worker keeps retrying"
    );
    shutdown_promptly(engine);
}

#[test]
fn a_missing_credential_pauses_the_account_for_its_credential() {
    let scratch = ScratchDir::new("account-worker-secret");
    let secrets = Arc::new(FakeSecrets::new());
    let server =
        FakeImapServer::serve(vec![bridge_script(true), sync_script(Inbox::Listed)]).unwrap();
    let engine = open(&scratch, &secrets);
    let account = add(&engine, server.port());
    eventually("the first worker logs in", || logins(&server) >= 2);
    shutdown_promptly(engine);
    secrets
        .delete(&format!("account/{account}/bridge-password"))
        .unwrap();

    let engine = open(&scratch, &secrets);

    eventually("the credential pause", || {
        state(&engine, &account).1.as_deref() == Some("the credential is not yet stored")
    });
    eventually("the worker ends", || {
        !engine.account_jobs().running(&account)
    });
    shutdown_promptly(engine);
}

/// A worker asked for an account that no longer syncs (a replayed
/// setup, a pause that raced the start) ends without a session.
#[test]
fn a_paused_account_gets_no_session() {
    let server = FakeImapServer::serve(vec![bridge_script(true)]).unwrap();
    let secrets = Arc::new(FakeSecrets::new());
    let scratch = ScratchDir::new("account-worker-paused");
    let engine = open(&scratch, &secrets);
    let account_id = add(&engine, server.port());
    let id = account_id.clone();
    block_on(engine.resources().write(WritePriority::User, move |tx| {
        tx.execute(
            "UPDATE accounts SET state = 'paused' WHERE account_id = ?1",
            [&id],
        )
        .map(drop)
        .map_err(|error| sb_resources::StoreError::internal(error.to_string()))
    }))
    .unwrap();
    shutdown_promptly(engine);

    // The reopened engine schedules no worker for the paused account;
    // asking for one directly opens no session either.
    let engine = open(&scratch, &secrets);
    let sessions = |server: &FakeImapServer| {
        let mut conns: Vec<usize> = server.received().iter().map(|c| c.conn).collect();
        conns.dedup();
        conns.len()
    };
    let before = sessions(&server);
    engine.start_account(account_id.clone());
    eventually("the worker ends", || {
        !engine.account_jobs().running(&account_id)
    });
    assert_eq!(sessions(&server), before, "no new session");
    shutdown_promptly(engine);
}
