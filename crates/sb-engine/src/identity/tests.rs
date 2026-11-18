//! Identity resolution tests (spec/05 "Identities and From
//! resolution", research 15): the pure rules as a table, reply
//! recipients, the cross-account check and its acknowledgement, the
//! header-only read across a restart, and the two commands' rows,
//! receipts and refusals.

use sb_resources::{
    AccountKind, IdentityRoute, IdentitySendable, ReaderAddress, SourceIdentityInputs,
    SourceMessageHeaders, StoreError, WritePriority,
};
use sb_test_support::{ScratchDir, block_on};

use super::*;
use crate::{DraftRecipient, Engine, EngineError};

fn scratch_engine(label: &str) -> (ScratchDir, Engine) {
    let scratch = ScratchDir::new(label);
    let engine = block_on(Engine::open(scratch.path().join("store"))).expect("the engine opens");
    (scratch, engine)
}

/// Run one seeded batch through the writer.
fn sql(engine: &Engine, batch: &'static str) {
    block_on(engine.resources().write(WritePriority::User, move |tx| {
        tx.execute_batch(batch)
            .map_err(|error| StoreError::internal(error.to_string()))?;
        Ok(())
    }))
    .unwrap();
}

fn route(id: &str, account: &str, email: &str, default: bool, kind: AccountKind) -> IdentityRoute {
    route_sendable(
        id,
        account,
        email,
        default,
        IdentitySendable::Validated,
        kind,
    )
}

fn route_sendable(
    id: &str,
    account: &str,
    email: &str,
    default: bool,
    sendable: IdentitySendable,
    kind: AccountKind,
) -> IdentityRoute {
    IdentityRoute {
        identity_id: id.to_owned(),
        account_id: account.to_owned(),
        email: email.to_owned(),
        is_default: default,
        sendable,
        account_kind: kind,
    }
}

fn source(account: &str, kind: AccountKind) -> SourceIdentityInputs {
    SourceIdentityInputs {
        account_id: account.to_owned(),
        account_kind: kind,
        headers: SourceMessageHeaders::default(),
        sent_identity_id: None,
    }
}

fn headers(
    from: &[&str],
    reply_to: &[&str],
    to: &[&str],
    cc: &[&str],
    delivered: &[&str],
) -> SourceMessageHeaders {
    let list = |values: &[&str]| {
        values
            .iter()
            .map(|value| ReaderAddress {
                address: Some((*value).to_owned()),
                name: None,
            })
            .collect()
    };
    SourceMessageHeaders {
        from: list(from),
        reply_to: list(reply_to),
        to: list(to),
        cc: list(cc),
        delivered_to: delivered.iter().map(|value| (*value).to_owned()).collect(),
    }
}

fn inputs(
    source: Option<SourceIdentityInputs>,
    identities: Vec<IdentityRoute>,
    selected: Option<&str>,
    global: Option<&str>,
) -> IdentityResolutionInputs {
    IdentityResolutionInputs {
        source,
        identities,
        selected_account_id: selected.map(str::to_owned),
        global_default_identity_id: global.map(str::to_owned),
    }
}

fn chosen(identity_id: &str) -> ResolvedIdentity {
    ResolvedIdentity::Chosen(identity_id.to_owned())
}

#[test]
fn the_identity_rules_are_table_driven() {
    let alias = || route("a", "acct", "alias@example.test", false, AccountKind::Imap);
    let fallback = || route("b", "acct", "default@example.test", true, AccountKind::Imap);
    let pair = || vec![alias(), fallback()];
    let r = |context, source, identities, selected, global| {
        resolve_identity(context, &inputs(source, identities, selected, global))
    };
    let sent = |identity: &str| {
        Some(SourceIdentityInputs {
            sent_identity_id: Some(identity.to_owned()),
            ..source("acct", AccountKind::Imap)
        })
    };
    let with_headers = |headers: SourceMessageHeaders| {
        Some(SourceIdentityInputs {
            headers,
            ..source("acct", AccountKind::Imap)
        })
    };

    // New: the selected account's default, otherwise the global setting.
    assert_eq!(
        r(ComposeContext::New, None, pair(), Some("acct"), None),
        chosen("b")
    );
    let other = route("c", "other", "other@example.test", false, AccountKind::Imap);
    assert_eq!(
        r(
            ComposeContext::New,
            None,
            vec![alias(), other],
            Some("other"),
            Some("a")
        ),
        chosen("a")
    );
    assert_eq!(
        r(
            ComposeContext::New,
            None,
            vec![alias()],
            Some("acct"),
            Some("missing")
        ),
        ResolvedIdentity::ChooseSendingIdentity
    );
    let local = route("l", "local", "l@example.test", true, AccountKind::Local);
    assert_eq!(
        r(
            ComposeContext::New,
            None,
            vec![local],
            Some("local"),
            Some("l")
        ),
        ResolvedIdentity::ChooseSendingIdentity
    );
    // A first compose with nothing selected and no global default
    // asks, rather than picking any sendable identity.
    assert_eq!(
        r(ComposeContext::New, None, pair(), None, None),
        ResolvedIdentity::ChooseSendingIdentity
    );

    // Reply and Reply All: the recorded attempt wins, even over To.
    let recorded = Some(SourceIdentityInputs {
        sent_identity_id: Some("a".to_owned()),
        headers: headers(&[], &[], &["default@example.test"], &[], &[]),
        ..source("acct", AccountKind::Imap)
    });
    assert_eq!(
        r(ComposeContext::ReplyAll, recorded, pair(), None, None),
        chosen("a")
    );
    // A recorded sender that can't send requires a visible choice.
    assert_eq!(
        r(
            ComposeContext::Reply,
            sent("gone"),
            vec![fallback()],
            None,
            None
        ),
        ResolvedIdentity::ChooseSendingIdentity
    );

    // No attempt: the From match, then To, Cc and Delivered-To.
    assert_eq!(
        r(
            ComposeContext::Reply,
            with_headers(headers(&["alias@EXAMPLE.test"], &[], &[], &[], &[])),
            pair(),
            None,
            None
        ),
        chosen("a")
    );
    for (to, cc, delivered) in [
        (vec!["alias@example.test"], vec![], vec![]),
        (vec![], vec!["alias@example.test"], vec![]),
        (vec![], vec![], vec!["alias@example.test"]),
    ] {
        let headers = with_headers(headers(&[], &[], &to, &cc, &delivered));
        assert_eq!(
            r(ComposeContext::Forward, headers, pair(), None, None),
            chosen("a")
        );
    }

    // Received mail with no matching address: the account's default;
    // an unavailable default offers nothing.
    assert_eq!(
        r(
            ComposeContext::Reply,
            Some(source("acct", AccountKind::Imap)),
            pair(),
            None,
            None
        ),
        chosen("b")
    );
    let unavailable = route_sendable(
        "u",
        "acct",
        "default@example.test",
        true,
        IdentitySendable::Unavailable,
        AccountKind::Imap,
    );
    assert_eq!(
        r(
            ComposeContext::Reply,
            Some(source("acct", AccountKind::Imap)),
            vec![unavailable],
            None,
            None
        ),
        ResolvedIdentity::ChooseSendingIdentity
    );

    // Two accounts hold the address the message was sent to: the
    // receiving account's identity answers, whatever the list order.
    let elsewhere = route("x", "other", "alias@example.test", false, AccountKind::Imap);
    assert_eq!(
        r(
            ComposeContext::Reply,
            with_headers(headers(&[], &[], &["alias@example.test"], &[], &[])),
            vec![elsewhere, alias()],
            None,
            None
        ),
        chosen("a")
    );
}

#[test]
fn reply_recipients_prefer_a_differing_reply_to_and_reply_all_excludes_the_sender() {
    let list = |values: &[&str]| {
        values
            .iter()
            .map(|value| ReaderAddress {
                address: Some((*value).to_owned()),
                name: None,
            })
            .collect::<Vec<_>>()
    };
    let differing = headers(
        &["sender@example.test"],
        &["lists@example.test"],
        &[],
        &[],
        &[],
    );
    assert_eq!(reply_recipients(&differing), list(&["lists@example.test"]));
    // Written local part, lowercased domain: this Reply-To is From.
    let same = headers(
        &["sender@example.test"],
        &["sender@EXAMPLE.TEST"],
        &[],
        &[],
        &[],
    );
    assert_eq!(reply_recipients(&same), list(&["sender@example.test"]));
    // No Reply-To, the common case: Reply goes to From.
    let plain = headers(&["sender@example.test"], &[], &[], &[], &[]);
    assert_eq!(reply_recipients(&plain), list(&["sender@example.test"]));
    let all = headers(
        &["sender@example.test"],
        &["lists@example.test"],
        &["sender@example.test", "person@example.test"],
        &["copy@example.test", "person@example.test"],
        &[],
    );
    assert_eq!(
        reply_all_recipients(&all, "sender@example.test"),
        list(&[
            "lists@example.test",
            "person@example.test",
            "copy@example.test"
        ])
    );
}

#[test]
fn the_cross_account_check_binds_its_ack_to_identity_and_recipients() {
    let route = route("r", "sending", "me@sending.test", true, AccountKind::Imap);
    let receiving = source("receiving", AccountKind::Imap);
    let warning = CrossAccountWarning {
        route_account_id: "sending".to_owned(),
        source_account_id: "receiving".to_owned(),
    };
    for context in [ComposeContext::Reply, ComposeContext::ReplyAll] {
        assert_eq!(
            cross_account_warning(context, Some(&route), Some(&receiving)),
            Some(warning.clone())
        );
        assert_eq!(
            cross_account_warning(
                context,
                Some(&route),
                Some(&source("local", AccountKind::Local))
            ),
            None
        );
        assert_eq!(
            cross_account_warning(
                context,
                Some(&route),
                Some(&source("sending", AccountKind::Imap))
            ),
            None
        );
    }
    assert_eq!(
        cross_account_warning(ComposeContext::Forward, Some(&route), Some(&receiving)),
        None
    );
    assert_eq!(
        cross_account_warning(ComposeContext::New, Some(&route), None),
        None
    );

    let recipient = |address: &str| DraftRecipient {
        address: address.to_owned(),
        name: None,
    };
    let acked = [cross_account_ack(
        "r",
        &[recipient("person@example.test")],
        &[],
        &[],
    )];
    let cases = [
        ("r", vec![recipient("person@example.test")], true),
        ("other", vec![recipient("person@example.test")], false),
        (
            "r",
            vec![
                recipient("person@example.test"),
                recipient("second@example.test"),
            ],
            false,
        ),
        ("r", vec![recipient("other@example.test")], false),
        ("r", vec![], false),
    ];
    for (identity, to, expected) in cases {
        assert_eq!(
            cross_account_acknowledged(Some(&warning), &acked, identity, &to, &[], &[]),
            expected,
            "{identity} {to:?}"
        );
    }
    // A hidden recipient added after the acknowledgement asks again.
    assert!(!cross_account_acknowledged(
        Some(&warning),
        &acked,
        "r",
        &[recipient("person@example.test")],
        &[],
        &[recipient("hidden@example.test")]
    ));
    // Order doesn't matter; no warning needs no acknowledgement.
    let reordered = [cross_account_ack(
        "r",
        &[recipient("b@example.test"), recipient("a@example.test")],
        &[recipient("b@example.test")],
        &[],
    )];
    assert!(cross_account_acknowledged(
        Some(&warning),
        &reordered,
        "r",
        &[recipient("a@example.test")],
        &[recipient("b@example.test")],
        &[]
    ));
    assert!(cross_account_acknowledged(None, &[], "r", &[], &[], &[]));
}

/// The alias A, the default B and a received message whose headers name
/// A in `Delivered-To`; no acquisition anywhere.
fn seed(engine: &Engine) {
    sql(
        engine,
        r#"INSERT INTO accounts (account_id, kind, provider, display_name, state, sort_order, created_at)
           VALUES ('acct', 'imap', 'generic_imap', 'Work', 'active', 1, 0);
           INSERT INTO identities (identity_id, account_id, email, display_name, is_default, sendable)
           VALUES ('a', 'acct', 'alias@example.test', 'Alias', 0, 'provisional');
           INSERT INTO identities (identity_id, account_id, email, display_name, is_default, sendable)
           VALUES ('b', 'acct', 'default@example.test', 'Default', 1, 'validated');
           INSERT INTO mailboxes (mailbox_id, account_id, remote_name, kind, selectable, sync_state)
           VALUES (7, 'acct', 'Inbox', 'folder', 1, 'synced');
           INSERT INTO logical_messages (message_id, account_id, created_at)
           VALUES (9, 'acct', 0);
           INSERT INTO occurrences (occurrence_id, account_id, mailbox_id, message_id,
               first_seen_at, last_seen_at, state)
           VALUES (11, 'acct', 7, 9, 0, 0, 'present');
           INSERT INTO message_headers (message_id, raw_fields, from_json, to_json, cc_json,
               delivered_to_json, fetched_at)
           VALUES (9, x'', '[{"address":"sender@example.test","name":null}]',
               '[{"address":"other@example.test","name":null}]',
               '[{"address":"third@example.test","name":null}]',
               '["alias@example.test"]', 0);"#,
    );
}

#[test]
fn after_a_restart_a_header_only_reply_resolves_from_message_headers() {
    let scratch = ScratchDir::new("engine-identity-restart");
    let root = scratch.path().join("store");
    let engine = block_on(Engine::open(&root)).expect("the engine opens");
    seed(&engine);
    block_on(engine.shutdown()).unwrap();

    // Reopen offline: the body was never acquired, and the identity
    // still resolves from the stored headers (spec/02 "Headers before
    // bodies").
    let engine = block_on(Engine::open(&root)).expect("the reopened engine");
    let resolved =
        block_on(engine.resolve_identity(ComposeContext::Reply, Some(11), None)).unwrap();
    assert_eq!(resolved, chosen("a"));

    // A mailbox selection resolves through its account for a new draft.
    for selection in [
        Some(ComposeSelection::Mailbox(7)),
        Some(ComposeSelection::Account("acct".to_owned())),
    ] {
        let new = block_on(engine.resolve_identity(ComposeContext::New, None, selection)).unwrap();
        assert_eq!(new, chosen("b"));
    }

    // The context and its source must agree.
    for (context, source) in [
        (ComposeContext::New, Some(11)),
        (ComposeContext::Reply, None),
    ] {
        assert!(
            matches!(
                block_on(engine.resolve_identity(context, source, None)),
                Err(EngineError::InvalidInput { ref field, .. }) if field == "source_occurrence_id"
            ),
            "{context:?} {source:?}"
        );
    }
    block_on(engine.shutdown()).unwrap();
}

#[test]
fn identity_commands_write_once_and_replay_with_typed_refusals() {
    let (_scratch, engine) = scratch_engine("engine-identity-commands");
    sql(
        &engine,
        r#"INSERT INTO accounts (account_id, kind, provider, display_name, state, sort_order, created_at)
           VALUES ('acct', 'imap', 'generic_imap', 'Work', 'active', 1, 0);
           INSERT INTO identities (identity_id, account_id, email, display_name, is_default, sendable)
           VALUES ('b', 'acct', 'default@example.test', 'Default', 1, 'validated');
           INSERT INTO accounts (account_id, kind, provider, display_name, state, sort_order, created_at)
           VALUES ('proton', 'proton', 'proton_api', 'Proton', 'active', 2, 0);
           INSERT INTO identities (identity_id, account_id, email, display_name, is_default, sendable)
           VALUES ('gone', 'acct', 'gone@example.test', 'Gone', 0, 'unavailable');"#,
    );
    let params = |id: &str| IdentityParams {
        identity_id: id.to_owned(),
        account_id: "acct".to_owned(),
        email: format!("{id}@example.test"),
        display_name: "Alias".to_owned(),
        reply_to: None,
        signature_source: None,
        sendable: IdentitySendable::Provisional,
    };
    let key = engine.new_idempotency_key();
    let created = block_on(engine.upsert_identity(&key, params("a"))).unwrap();
    assert_eq!(created.command(), "upsert_identity");
    let replayed = block_on(engine.upsert_identity(&key, params("a"))).unwrap();
    assert_eq!(replayed.result_json(), created.result_json());

    // The replay names the same receipt; the store test pins the
    // exactly-one-default invariant.
    let default_key = engine.new_idempotency_key();
    let set = block_on(engine.set_default_identity(&default_key, "a")).unwrap();
    let again = block_on(engine.set_default_identity(&default_key, "a")).unwrap();
    assert_eq!(again.result_json(), set.result_json());

    // Typed refusals: an unavailable identity can't be the default, and
    // a Proton account's identities come from its backend.
    let error =
        block_on(engine.set_default_identity(&engine.new_idempotency_key(), "gone")).unwrap_err();
    assert!(
        matches!(&error, EngineError::InvalidInput { field, message }
            if field == "identity_id" && message.contains("unavailable")),
        "{error:?}"
    );
    let mut proton = params("x");
    proton.account_id = "proton".to_owned();
    let error =
        block_on(engine.upsert_identity(&engine.new_idempotency_key(), proton)).unwrap_err();
    assert!(
        matches!(&error, EngineError::InvalidInput { field, .. } if field == "account_id"),
        "{error:?}"
    );
    block_on(engine.shutdown()).unwrap();
}
