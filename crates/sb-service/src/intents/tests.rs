//! `Core::intents` (spec/07): the query's page, order, filters and
//! typed DTOs across the service boundary.

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
                '{{"v":1}}', '{{}}', 'parked', NULL, NULL, 'i3', 'user', 3, 3);"#
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
    assert_eq!(next.rows.len(), 1);
    assert_eq!(next.rows[0].intent_id, "i3");
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
        vec!["i1", "i3"]
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
