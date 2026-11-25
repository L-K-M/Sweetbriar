//! The recipient check through `Core`'s public API (spec/05 "Recipient
//! check", spec/07): the service DTOs, `send_draft`'s refusal and the
//! allowance commands.

use std::path::Path;

use sb_test_support::{ScratchDir, block_on, execute_sql};

use crate::{
    BlockedRecipient, ComposeFormat, Core, CoreConfig, CoreError, DraftContent, DraftRecipient,
    DraftSource, RecipientAllowance, RecipientFence, open_core,
};

fn state_db(scratch: &ScratchDir) -> std::path::PathBuf {
    scratch.path().join("store/state.sqlite")
}

fn open(scratch: &ScratchDir) -> Core {
    block_on(open_core(CoreConfig {
        data_root: scratch.path().join("store"),
        cache_root: scratch.path().join("cache"),
    }))
    .unwrap()
}

/// Two Proton accounts; a sender row saying `them@example.test` wrote
/// only to acct-2, so acct-1's draft to that address is blocked
/// (spec/05). The received-address sweep is marked done, so the check
/// is ready (`account_senders_from` at 0).
fn seed(state_db: &Path) {
    execute_sql(
        state_db,
        "INSERT INTO accounts
         (account_id, kind, provider, display_name, state, sort_order, created_at)
         VALUES ('acct-1', 'proton', 'proton_api', 'One', 'active', 1, 0),
                ('acct-2', 'proton', 'proton_api', 'Two', 'active', 2, 0);
         INSERT INTO identities
         (identity_id, account_id, email, display_name, is_default, sendable)
         VALUES ('ident-1', 'acct-1', 'me@example.test', 'Me', 1, 'validated'),
                ('ident-2', 'acct-2', 'two@example.test', 'Two', 1, 'validated');
         INSERT INTO d_account_senders (canonical, account_id, first_seen_at)
         VALUES ('them@example.test', 'acct-2', 0),
                ('mine@example.test', 'acct-1', 0);
         UPDATE store_meta SET value = '0' WHERE key = 'account_senders_from';",
    );
}

/// One routed save with `to` in To.
fn content(to: &[&str]) -> DraftContent {
    DraftContent {
        identity_id: Some("ident-1".to_owned()),
        format: ComposeFormat::Plain,
        plain_flowed: true,
        subject: "hello".to_owned(),
        to: to
            .iter()
            .map(|address| DraftRecipient {
                address: (*address).to_owned(),
                name: None,
            })
            .collect(),
        cc: vec![],
        bcc: vec![],
        body_source: "the source text".to_owned(),
        attachments: vec![],
    }
}

/// The fence for a draft addressed to `them@Example.Test` from acct-1.
fn blocked() -> RecipientFence {
    RecipientFence {
        ready: true,
        blocked: vec![BlockedRecipient {
            address: "them@Example.Test".to_owned(),
            account_ids: vec!["acct-2".to_owned()],
        }],
    }
}

/// An address that wrote only to another account is blocked from
/// acct-1, whatever the `acks`; an allowance clears the block, lists in
/// Settings and replays its receipt; revoking brings the block back
/// (spec/05, spec/07).
#[test]
fn a_blocked_recipient_refuses_a_send_until_it_is_allowed() {
    let scratch = ScratchDir::new("service-send-recipient-fence");
    let core = open(&scratch);
    block_on(core.shutdown());
    seed(&state_db(&scratch));
    let core = open(&scratch);

    let draft = block_on(core.create_draft(&core.new_idempotency_key(), DraftSource::New)).unwrap();
    let saved = block_on(core.save_draft(
        &core.new_idempotency_key(),
        &draft.draft_id,
        None,
        content(&["them@Example.Test"]),
    ))
    .unwrap();
    assert_eq!(
        block_on(core.recipient_fence(&draft.draft_id, &saved.revision_id)).unwrap(),
        blocked()
    );

    // A blocked recipient refuses the send whatever the acks (spec/05).
    for acks in [vec![], vec!["cross_account".to_owned()]] {
        match block_on(core.send_draft(
            &core.new_idempotency_key(),
            &draft.draft_id,
            &saved.revision_id,
            acks,
        ))
        .unwrap_err()
        {
            CoreError::Conflict { detail } => {
                assert!(detail.contains("recipient_fence"), "{detail}");
                assert!(detail.contains("them@Example.Test"), "{detail}");
            }
            other => panic!("a blocked recipient must refuse the send: {other:?}"),
        }
    }

    let key = core.new_idempotency_key();
    // The draft spells `them@Example.Test`; only the domain folds, so
    // this spelling is the same canonical address (spec/02).
    let receipt = block_on(core.allow_recipient(&key, "acct-1", "them@example.test")).unwrap();
    assert_eq!(receipt.command, "allow_recipient");
    // A replayed key returns the same receipt and writes nothing.
    let replayed = block_on(core.allow_recipient(&key, "acct-1", "them@Example.Test")).unwrap();
    assert_eq!(replayed, receipt);

    assert_eq!(
        block_on(core.recipient_fence(&draft.draft_id, &saved.revision_id)).unwrap(),
        RecipientFence {
            ready: true,
            blocked: Vec::new(),
        }
    );
    assert_eq!(
        block_on(core.recipient_allowances()).unwrap(),
        vec![RecipientAllowance {
            account_id: "acct-1".to_owned(),
            address: "them@example.test".to_owned(),
            created_at_ms: receipt.created_at_ms,
        }]
    );
    block_on(core.send_draft(
        &core.new_idempotency_key(),
        &draft.draft_id,
        &saved.revision_id,
        vec![],
    ))
    .unwrap();

    // Revoking brings the block back for another draft (spec/05).
    let removed = block_on(core.remove_recipient_allowance(
        &core.new_idempotency_key(),
        "acct-1",
        "them@example.TEST",
    ))
    .unwrap();
    assert_eq!(removed.command, "remove_recipient_allowance");
    assert!(block_on(core.recipient_allowances()).unwrap().is_empty());

    let later = block_on(core.create_draft(&core.new_idempotency_key(), DraftSource::New)).unwrap();
    let saved_later = block_on(core.save_draft(
        &core.new_idempotency_key(),
        &later.draft_id,
        None,
        content(&["them@Example.Test"]),
    ))
    .unwrap();
    assert_eq!(
        block_on(core.recipient_fence(&later.draft_id, &saved_later.revision_id)).unwrap(),
        blocked()
    );
    block_on(core.shutdown());
}
