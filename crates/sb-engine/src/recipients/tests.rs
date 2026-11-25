//! The recipient check through the engine (spec/05 "Recipient check",
//! spec/07): the fence for new messages, replies and forwards in To, Cc
//! and Bcc, readiness, `send_draft`'s refusal and the ways through it,
//! and the allowance commands with their receipts.

use sb_resources::{StoreError, WritePriority};
use sb_test_support::{ScratchDir, block_on};

use super::*;
use crate::{ComposeFormat, DraftContent, DraftRecipient, DraftSource};

fn scratch_engine(label: &str) -> (ScratchDir, Engine) {
    let scratch = ScratchDir::new(label);
    let engine = block_on(Engine::open(scratch.path().join("store"))).expect("the engine opens");
    (scratch, engine)
}

fn sql(engine: &Engine, statement: &str) {
    let statement = statement.to_owned();
    block_on(engine.resources().write(WritePriority::User, move |tx| {
        tx.execute_batch(&statement)
            .map_err(|e| StoreError::internal(e.to_string()))?;
        Ok(())
    }))
    .unwrap();
}

/// Two Proton accounts, each with one validated identity; a message
/// acct-2 received from `them@example.test`, addressed to acct-1's
/// address so a reply routes through acct-1; and the senders: `them`
/// wrote only to acct-2, `both` to both, `mine` only to acct-1. The
/// check is ready unless `ready` is false.
fn seed(engine: &Engine, ready: bool) {
    let from = if ready { 0 } else { i64::MAX };
    sql(
        engine,
        &format!(
            r#"INSERT INTO accounts (account_id, kind, provider, display_name, state, sort_order, created_at) VALUES
              ('acct-1', 'proton', 'proton_api', 'One', 'active', 1, 0),
              ('acct-2', 'proton', 'proton_api', 'Two', 'active', 2, 0);
            INSERT INTO identities (identity_id, account_id, email, display_name, is_default, sendable) VALUES
              ('ident-1', 'acct-1', 'me@example.test', 'Me', 1, 'validated'),
              ('ident-2', 'acct-2', 'two@example.test', 'Two', 1, 'validated');
            INSERT INTO mailboxes (mailbox_id, account_id, remote_name, kind, selectable, sync_state) VALUES (8, 'acct-2', 'Inbox', 'folder', 1, 'synced');
            INSERT INTO logical_messages (message_id, account_id, created_at) VALUES (21, 'acct-2', 0);
            INSERT INTO occurrences (occurrence_id, account_id, mailbox_id, message_id, first_seen_at, last_seen_at, state) VALUES (22, 'acct-2', 8, 21, 0, 0, 'present');
            INSERT INTO message_headers (message_id, raw_fields, subject, from_json, to_json, cc_json, date_header, fetched_at)
            VALUES (21, x'', 'hello', '[{{"address":"them@example.test","name":null}}]',
               '[{{"address":"me@example.test","name":null}}]', NULL, 1700000000000, 0);
            INSERT INTO d_account_senders (canonical, account_id, first_seen_at) VALUES
              ('them@example.test', 'acct-2', 0),
              ('both@example.test', 'acct-1', 0),
              ('both@example.test', 'acct-2', 0),
              ('mine@example.test', 'acct-1', 0);
            UPDATE store_meta SET value = '{from}' WHERE key = 'account_senders_from';"#
        ),
    );
}

fn recipients(addresses: &[&str]) -> Vec<DraftRecipient> {
    addresses
        .iter()
        .map(|address| DraftRecipient {
            address: (*address).to_owned(),
            name: None,
        })
        .collect()
}

fn content(identity: &str, to: &[&str], cc: &[&str], bcc: &[&str]) -> DraftContent {
    DraftContent {
        identity_id: Some(identity.to_owned()),
        format: ComposeFormat::Plain,
        plain_flowed: true,
        subject: "hello".to_owned(),
        to: recipients(to),
        cc: recipients(cc),
        bcc: recipients(bcc),
        body_source: "the source text".to_owned(),
        attachments: vec![],
    }
}

/// A draft from `source` saved with `content`; its id and revision id.
fn draft_with(engine: &Engine, source: DraftSource, content: DraftContent) -> (String, String) {
    let created = block_on(engine.create_draft(&engine.new_idempotency_key(), source)).unwrap();
    let draft_id = created.draft_id().to_owned();
    let base = block_on(engine.draft(&draft_id))
        .unwrap()
        .revision
        .map(|revision| revision.revision_id);
    let saved = block_on(engine.save_draft(
        &engine.new_idempotency_key(),
        &draft_id,
        base.as_deref(),
        content,
    ))
    .unwrap();
    (draft_id, saved.revision_id().to_owned())
}

fn save(engine: &Engine, draft_id: &str, base: &str, content: DraftContent) -> String {
    block_on(engine.save_draft(&engine.new_idempotency_key(), draft_id, Some(base), content))
        .unwrap()
        .revision_id()
        .to_owned()
}

fn fence(engine: &Engine, draft_id: &str, revision_id: &str) -> RecipientFence {
    block_on(engine.recipient_fence(draft_id, revision_id)).unwrap()
}

fn blocked(addresses: &[(&str, &[&str])]) -> Vec<BlockedRecipient> {
    addresses
        .iter()
        .map(|(address, accounts)| BlockedRecipient {
            address: (*address).to_owned(),
            account_ids: accounts.iter().map(|id| (*id).to_owned()).collect(),
        })
        .collect()
}

fn send(
    engine: &Engine,
    draft_id: &str,
    revision_id: &str,
    acks: &[String],
) -> Result<(), EngineError> {
    block_on(engine.send_draft(&engine.new_idempotency_key(), draft_id, revision_id, acks))
        .map(|_| ())
}

fn assert_refused(result: Result<(), EngineError>, address: &str) {
    match result {
        Err(EngineError::Conflict { detail }) => {
            assert!(detail.contains("recipient_fence"), "{detail}");
            assert!(detail.contains(address), "{detail}");
        }
        other => panic!("a blocked recipient must refuse the send: {other:?}"),
    }
}

/// An address that wrote only to acct-2 is blocked from acct-1 in To,
/// Cc and Bcc; one that wrote to acct-1, or to no account, passes.
#[test]
fn an_address_known_only_to_another_account_is_blocked_in_every_field() {
    let (_scratch, engine) = scratch_engine("engine-fence-fields");
    seed(&engine, true);
    for (to, cc, bcc) in [
        (&["them@example.test"][..], &[][..], &[][..]),
        (&[][..], &["them@example.test"][..], &[][..]),
        (&[][..], &[][..], &["them@example.test"][..]),
    ] {
        let (draft_id, revision_id) =
            draft_with(&engine, DraftSource::New, content("ident-1", to, cc, bcc));
        let answer = fence(&engine, &draft_id, &revision_id);
        assert!(answer.ready);
        assert_eq!(
            answer.blocked,
            blocked(&[("them@example.test", &["acct-2"])])
        );
    }

    let passing = [
        "mine@example.test",
        "both@example.test",
        "nobody@example.test",
    ];
    let (draft_id, revision_id) = draft_with(
        &engine,
        DraftSource::New,
        content("ident-1", &passing, &[], &[]),
    );
    assert!(fence(&engine, &draft_id, &revision_id).blocked.is_empty());
    send(&engine, &draft_id, &revision_id, &[]).unwrap();
    block_on(engine.shutdown()).unwrap();
}

/// Replies and forwards are checked alike: the reply to acct-2's mail
/// through acct-1 is blocked for the sender who wrote only to acct-2,
/// and a forward to that sender too.
#[test]
fn replies_and_forwards_are_checked_like_new_messages() {
    let (_scratch, engine) = scratch_engine("engine-fence-reply");
    seed(&engine, true);
    let created = block_on(engine.create_draft(
        &engine.new_idempotency_key(),
        DraftSource::Reply { occurrence_id: 22 },
    ))
    .unwrap();
    let draft_id = created.draft_id().to_owned();
    let revision = block_on(engine.draft(&draft_id)).unwrap().revision.unwrap();
    assert_eq!(revision.identity_id.as_deref(), Some("ident-1"));
    let answer = fence(&engine, &draft_id, &revision.revision_id);
    assert_eq!(
        answer.blocked,
        blocked(&[("them@example.test", &["acct-2"])])
    );
    // The reply crosses accounts; its acknowledgement gets past that
    // check, and the recipient check still refuses.
    let notice = block_on(engine.cross_account_notice(&draft_id, &revision.revision_id))
        .unwrap()
        .expect("the reply sends from another account");
    let acks = [notice.ack, "recipient_fence".to_owned()];
    assert_refused(
        send(&engine, &draft_id, &revision.revision_id, &acks),
        "them@example.test",
    );

    let (draft_id, revision_id) = draft_with(
        &engine,
        DraftSource::Forward { occurrence_id: 22 },
        content("ident-1", &["them@example.test"], &[], &[]),
    );
    let answer = fence(&engine, &draft_id, &revision_id);
    assert_eq!(
        answer.blocked,
        blocked(&[("them@example.test", &["acct-2"])])
    );
    assert_refused(
        send(&engine, &draft_id, &revision_id, &[]),
        "them@example.test",
    );
    block_on(engine.shutdown()).unwrap();
}

/// Before the first pass after migration 38 completes, the check
/// reports not ready and blocks nothing, even with acct-2's rows.
#[test]
fn a_check_that_isn_t_ready_blocks_nothing() {
    let (_scratch, engine) = scratch_engine("engine-fence-not-ready");
    seed(&engine, false);
    let (draft_id, revision_id) = draft_with(
        &engine,
        DraftSource::New,
        content("ident-1", &["them@example.test"], &[], &[]),
    );
    assert_eq!(
        fence(&engine, &draft_id, &revision_id),
        RecipientFence {
            ready: false,
            blocked: Vec::new(),
        }
    );
    send(&engine, &draft_id, &revision_id, &[]).unwrap();
    block_on(engine.shutdown()).unwrap();
}

/// `send_draft` refuses whatever the `acks`, and sends after an
/// allowance, a route change or removing the recipient. Revoking the
/// allowance blocks again.
#[test]
fn send_refuses_until_allowed_rerouted_or_removed() {
    let (_scratch, engine) = scratch_engine("engine-fence-send");
    seed(&engine, true);
    let blocked_content = || content("ident-1", &["them@Example.Test"], &[], &[]);
    let acks = ["cross_account".to_owned(), "recipient_fence".to_owned()];

    // A route change: From acct-2, which `them` wrote to.
    let (draft_id, first) = draft_with(&engine, DraftSource::New, blocked_content());
    assert_refused(send(&engine, &draft_id, &first, &acks), "them@Example.Test");
    let rerouted = save(
        &engine,
        &draft_id,
        &first,
        content("ident-2", &["them@Example.Test"], &[], &[]),
    );
    assert!(fence(&engine, &draft_id, &rerouted).blocked.is_empty());
    send(&engine, &draft_id, &rerouted, &[]).unwrap();

    // Removing the recipient.
    let (draft_id, first) = draft_with(&engine, DraftSource::New, blocked_content());
    assert_refused(send(&engine, &draft_id, &first, &[]), "them@Example.Test");
    let removed = save(
        &engine,
        &draft_id,
        &first,
        content("ident-1", &["mine@example.test"], &[], &[]),
    );
    send(&engine, &draft_id, &removed, &[]).unwrap();

    // An allowance for acct-1; another account's allowance doesn't count.
    let (draft_id, revision_id) = draft_with(&engine, DraftSource::New, blocked_content());
    block_on(engine.allow_recipient(&engine.new_idempotency_key(), "acct-2", "them@example.test"))
        .unwrap();
    assert_refused(
        send(&engine, &draft_id, &revision_id, &[]),
        "them@Example.Test",
    );
    block_on(engine.allow_recipient(&engine.new_idempotency_key(), "acct-1", "them@example.test"))
        .unwrap();
    assert!(fence(&engine, &draft_id, &revision_id).blocked.is_empty());
    send(&engine, &draft_id, &revision_id, &[]).unwrap();

    // Revoking the allowance.
    block_on(engine.remove_recipient_allowance(
        &engine.new_idempotency_key(),
        "acct-1",
        "them@example.test",
    ))
    .unwrap();
    let (draft_id, revision_id) = draft_with(&engine, DraftSource::New, blocked_content());
    assert_eq!(
        fence(&engine, &draft_id, &revision_id).blocked,
        blocked(&[("them@Example.Test", &["acct-2"])])
    );
    assert_refused(
        send(&engine, &draft_id, &revision_id, &[]),
        "them@Example.Test",
    );
    block_on(engine.shutdown()).unwrap();
}

/// A revision that isn't the draft's current one is `Conflict`; an
/// unknown draft `NotFound`; a revision without a route blocks nothing.
#[test]
fn the_fence_answers_only_for_the_current_revision() {
    let (_scratch, engine) = scratch_engine("engine-fence-stale");
    seed(&engine, true);
    let (draft_id, first) = draft_with(
        &engine,
        DraftSource::New,
        content("ident-1", &["them@example.test"], &[], &[]),
    );
    let second = save(
        &engine,
        &draft_id,
        &first,
        content(
            "ident-1",
            &["them@example.test", "both@example.test"],
            &[],
            &[],
        ),
    );
    match block_on(engine.recipient_fence(&draft_id, &first)) {
        Err(EngineError::Conflict { detail }) => assert!(detail.contains("current revision")),
        other => panic!("a stale revision must conflict: {other:?}"),
    }
    assert_eq!(fence(&engine, &draft_id, &second).blocked.len(), 1);
    match block_on(engine.recipient_fence("no-such-draft", &second)) {
        Err(EngineError::NotFound { what }) => assert_eq!(what, "draft no-such-draft"),
        other => panic!("an unknown draft must be NotFound: {other:?}"),
    }

    let mut unrouted = content("ident-1", &["them@example.test"], &[], &[]);
    unrouted.identity_id = None;
    let third = save(&engine, &draft_id, &second, unrouted);
    assert_eq!(
        fence(&engine, &draft_id, &third),
        RecipientFence {
            ready: true,
            blocked: Vec::new(),
        }
    );
    block_on(engine.shutdown()).unwrap();
}

/// The allowance commands replay their receipts, say whether they
/// changed anything, list for Settings, and refuse the local archive.
#[test]
fn allowance_commands_replay_and_refuse_the_local_archive() {
    let (_scratch, engine) = scratch_engine("engine-fence-allowances");
    seed(&engine, true);
    let key = engine.new_idempotency_key();
    let allow = |key: &str, account: &str, address: &str| {
        block_on(engine.allow_recipient(key, account, address))
    };
    let first = allow(&key, "acct-1", "Them@Example.Test").unwrap();
    assert_eq!(first.result_json(), "{\"changed\":true}");
    let replayed = allow(&key, "acct-1", "Them@Example.Test").unwrap();
    assert_eq!(replayed.result_json(), first.result_json());
    assert_eq!(replayed.created_at(), first.created_at());
    let again = allow(&engine.new_idempotency_key(), "acct-1", "Them@EXAMPLE.test").unwrap();
    assert_eq!(again.result_json(), "{\"changed\":false}");

    let listed = block_on(engine.recipient_allowances()).unwrap();
    assert_eq!(listed.len(), 1);
    assert_eq!(listed[0].account_id, "acct-1");
    assert_eq!(listed[0].address, "Them@Example.Test");
    assert_eq!(listed[0].created_at, first.created_at());

    let local = engine
        .resources()
        .read(|tx| {
            tx.query_row(
                "SELECT account_id FROM accounts WHERE provider = 'local_archive'",
                [],
                |row| row.get::<_, String>(0),
            )
            .map_err(|e| StoreError::internal(e.to_string()))
        })
        .unwrap();
    for result in [
        allow(&engine.new_idempotency_key(), &local, "them@example.test"),
        block_on(engine.remove_recipient_allowance(
            &engine.new_idempotency_key(),
            &local,
            "them@example.test",
        )),
        allow(&engine.new_idempotency_key(), "acct-1", "not an address"),
        allow(&engine.new_idempotency_key(), "", "them@example.test"),
    ] {
        assert!(
            matches!(result, Err(EngineError::InvalidInput { .. })),
            "{result:?}"
        );
    }
    assert!(matches!(
        allow(&engine.new_idempotency_key(), "acct-9", "them@example.test"),
        Err(EngineError::NotFound { .. })
    ));

    let remove =
        |key: &str| block_on(engine.remove_recipient_allowance(key, "acct-1", "Them@example.TEST"));
    let key = engine.new_idempotency_key();
    assert_eq!(remove(&key).unwrap().result_json(), "{\"changed\":true}");
    assert_eq!(remove(&key).unwrap().result_json(), "{\"changed\":true}");
    assert!(matches!(
        remove(&engine.new_idempotency_key()),
        Err(EngineError::NotFound { .. })
    ));
    assert!(block_on(engine.recipient_allowances()).unwrap().is_empty());
    block_on(engine.shutdown()).unwrap();
}
