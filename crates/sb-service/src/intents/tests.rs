//! `Core::intents` and the intent health actions (spec/07): the
//! query's page, order, filters and typed DTOs across the service
//! boundary, and `release_intents` / `cancel_intents` under a key.

use super::*;
use crate::{CoreConfig, open_core};

fn config(scratch: &sb_test_support::ScratchDir) -> CoreConfig {
    CoreConfig {
        data_root: scratch.path().join("store"),
        cache_root: scratch.path().join("cache"),
    }
}

/// A fresh core's intent page is empty, and a cursor from it is
/// refused.
#[test]
fn a_fresh_core_has_no_intents() {
    let scratch = sb_test_support::ScratchDir::new("service-intents-empty");
    let core = sb_test_support::block_on(open_core(config(&scratch))).unwrap();
    let page =
        sb_test_support::block_on(core.intents(IntentFilter::default(), IntentAnchor::Top, 100))
            .unwrap();
    assert!(page.rows.is_empty());
    assert!(!page.has_more);
    assert!(page.after.is_none());
    sb_test_support::block_on(core.shutdown());
}

/// Parked and quarantined rows cross field by field, oldest first, and
/// the state filter and a cursor page through them.
#[test]
fn intents_cross_field_by_field_and_page() {
    let scratch = sb_test_support::ScratchDir::new("service-intents-rows");
    let store = scratch.path().join("store");
    let core = sb_test_support::block_on(open_core(config(&scratch))).unwrap();
    let archive = sb_test_support::block_on(core.accounts()).unwrap()[0]
        .account_id
        .clone();
    sb_test_support::block_on(core.shutdown());

    sb_test_support::execute_sql(
        &store.join("state.sqlite"),
        &format!(
            r#"INSERT INTO accounts (account_id, kind, provider, display_name, state, sort_order,
               created_at)
             VALUES ('{ACCOUNT}', 'imap', 'proton_bridge', 'Work', 'active', 1, 0);
             INSERT INTO intents (intent_id, account_id, config_generation, op, target_json,
               args_json, preconditions_json, state, quarantine_reason, last_error,
               idempotency_key, origin, created_at, updated_at)
             VALUES
               ('i1', '{archive}', 1, 'set_flags', '{{"v":1,"occurrence_ids":[7]}}',
                '{{"v":1,"add":["\\Seen"],"remove":[]}}', '{{}}', 'parked', NULL, 'no',
                'i1', 'user', 1, 1),
               ('i2', '{archive}', 1, 'move', '{{"v":1,"occurrence_ids":[7,9]}}',
                '{{"v":1,"to_mailbox_id":42}}', '{{}}', 'quarantined', 'epoch_change', NULL,
                'i2', 'user', 2, 2),
               ('i3', '{ACCOUNT}', 1, 'delete_to_trash', '{{"v":1,"occurrence_ids":[8]}}',
                '{{"v":1}}', '{{}}', 'parked', NULL, NULL, 'i3', 'user', 3, 3),
               ('i4', '{archive}', 1, 'remove_label', '{{"v":1,"occurrence_ids":[9]}}',
                '{{"v":1,"from_mailbox_id":42}}', '{{}}', 'parked', NULL, NULL,
                'i4', 'user', 4, 4);"#
        ),
    );
    let core = sb_test_support::block_on(open_core(config(&scratch))).unwrap();

    let page =
        sb_test_support::block_on(core.intents(IntentFilter::default(), IntentAnchor::Top, 2))
            .unwrap();
    assert_eq!(page.rows.len(), 2);
    assert!(page.has_more);
    assert_eq!(page.rows[0].intent_id, "i1");
    assert_eq!(
        page.rows[0].effect,
        IntentEffect::SetFlags {
            add: vec!["\\Seen".to_owned()],
            remove: vec![],
        }
    );
    assert_eq!(page.rows[0].state, IntentState::Parked);
    assert_eq!(page.rows[0].last_error.as_deref(), Some("no"));
    assert_eq!(
        page.rows[1].quarantine_reason,
        Some(QuarantineReason::EpochChange)
    );
    assert_eq!(
        page.rows[1].effect,
        IntentEffect::ToMailbox { mailbox_id: 42 }
    );
    let after = page.after.clone().unwrap();

    let next = sb_test_support::block_on(core.intents(
        IntentFilter::default(),
        IntentAnchor::After(after),
        2,
    ))
    .unwrap();
    assert_eq!(next.rows.len(), 2);
    assert_eq!(next.rows[0].intent_id, "i3");
    assert_eq!(next.rows[1].intent_id, "i4");
    assert_eq!(next.rows[1].kind, IntentKind::RemoveLabel);
    assert_eq!(
        next.rows[1].effect,
        IntentEffect::RemoveLabel { mailbox_id: 42 }
    );
    assert!(!next.has_more);

    let parked = IntentFilter {
        account_id: None,
        state: Some(IntentState::Parked),
    };
    let page = sb_test_support::block_on(core.intents(parked, IntentAnchor::Top, 10)).unwrap();
    assert_eq!(
        page.rows
            .iter()
            .map(|row| row.intent_id.as_str())
            .collect::<Vec<_>>(),
        vec!["i1", "i3", "i4"]
    );

    let work = IntentFilter {
        account_id: Some(ACCOUNT.to_owned()),
        state: None,
    };
    let page = sb_test_support::block_on(core.intents(work, IntentAnchor::Top, 10)).unwrap();
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].intent_id, "i3");

    sb_test_support::block_on(core.shutdown());
}

const ACCOUNT: &str = "00000000-0000-4000-8000-0000000000a0";

/// `release_intents` resets a parked intent, `cancel_intents` ends a
/// pending star and reverts its flag, a repeated key answers the stored
/// receipt, and a bad id changes nothing.
#[test]
fn intent_health_actions_cross_the_service() {
    let scratch = sb_test_support::ScratchDir::new("service-intents-health");
    let store = scratch.path().join("store");
    let core = sb_test_support::block_on(open_core(config(&scratch))).unwrap();
    sb_test_support::block_on(core.shutdown());

    sb_test_support::execute_sql(
        &store.join("state.sqlite"),
        r#"
        INSERT INTO accounts (account_id, kind, provider, display_name, state,
         config_generation, sort_order, created_at)
         VALUES ('acct-1', 'proton', 'proton_api', 'Owner', 'active', 1, 1, 0);
        INSERT INTO mailboxes (mailbox_id, account_id, remote_name, role, kind, selectable,
         sync_state) VALUES (1, 'acct-1', 'Inbox', 'inbox', 'folder', 1, 'synced');
        INSERT INTO logical_messages (message_id, account_id, provider_key, created_at)
         VALUES (1, 'acct-1', 'msg-1', 0);
        INSERT INTO occurrences (occurrence_id, account_id, mailbox_id, message_id, state,
         first_seen_at, last_seen_at, f_flagged)
         VALUES (11, 'acct-1', 1, 1, 'present', 0, 0, 1);
        INSERT INTO intents (intent_id, account_id, config_generation, op, target_json,
         args_json, preconditions_json, state, quarantine_reason, last_error,
         idempotency_key, origin, created_at, updated_at)
         VALUES
           ('i1', 'acct-1', 1, 'set_flags',
            '{"v":1,"occurrence_ids":[11],"provider_key":"msg-1"}',
            '{"v":1,"add":["\\Flagged"],"remove":[]}', '{}', 'parked',
            'config_change', 'no', 'i1', 'user', 1, 1),
           ('i2', 'acct-1', 1, 'set_flags',
            '{"v":1,"occurrence_ids":[11],"provider_key":"msg-1"}',
            '{"v":1,"add":["\\Flagged"],"remove":[]}', '{}', 'pending',
            NULL, NULL, 'i2', 'user', 2, 2),
           ('i3', 'acct-1', 1, 'set_flags',
            '{"v":1,"occurrence_ids":[11],"provider_key":"msg-1"}',
            '{"v":1,"add":["\\Flagged"],"remove":[]}', '{}', 'done',
            NULL, NULL, 'i3', 'user', 3, 3);
        "#,
    );
    let core = sb_test_support::block_on(open_core(config(&scratch))).unwrap();
    let db = store.join("state.sqlite");

    let receipt =
        sb_test_support::block_on(core.release_intents("release-1", vec!["i1".to_owned()]))
            .unwrap();
    assert_eq!(receipt.command, "release_intents");
    assert_eq!(
        sb_test_support::count_rows(
            &db,
            "SELECT COUNT(*) FROM intents WHERE intent_id = 'i1' AND state = 'pending'
             AND attempts = 0 AND quarantine_reason IS NULL",
        ),
        1
    );
    let replayed =
        sb_test_support::block_on(core.release_intents("release-1", vec!["i1".to_owned()]))
            .unwrap();
    assert_eq!(replayed, receipt);

    let receipt =
        sb_test_support::block_on(core.cancel_intents("cancel-1", vec!["i2".to_owned()])).unwrap();
    assert_eq!(receipt.command, "cancel_intents");
    assert_eq!(
        sb_test_support::count_rows(
            &db,
            "SELECT COUNT(*) FROM intents WHERE intent_id = 'i2' AND state = 'cancelled'",
        ),
        1
    );
    assert_eq!(
        sb_test_support::count_rows(
            &db,
            "SELECT COUNT(*) FROM occurrences WHERE occurrence_id = 11 AND f_flagged = 0",
        ),
        1,
        "cancelling reverts the pending star's local effect"
    );

    // A terminal id is a Conflict and changes nothing; an empty list is
    // InvalidInput.
    let error = sb_test_support::block_on(core.cancel_intents("cancel-2", vec!["i3".to_owned()]))
        .unwrap_err();
    assert!(matches!(error, CoreError::Conflict { .. }), "{error:?}");
    assert_eq!(
        sb_test_support::count_rows(
            &db,
            "SELECT COUNT(*) FROM intents WHERE intent_id = 'i3' AND state = 'done'",
        ),
        1
    );
    let error = sb_test_support::block_on(core.cancel_intents("cancel-3", Vec::new())).unwrap_err();
    assert!(
        matches!(error, CoreError::InvalidInput { ref field, .. } if field == "intent_ids"),
        "{error:?}"
    );

    sb_test_support::block_on(core.shutdown());
}
