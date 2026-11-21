//! The FFI boundary for `intents(filter)`: a fresh core answers an
//! empty page, populated rows cross field by field, and a cancelled
//! token is `Cancelled`.

use super::*;
use crate::api::{CoreConfig, open_core};
use sb_test_support::ScratchDir;

const ACCOUNT: &str = "00000000-0000-4000-8000-0000000000a0";

fn token() -> Arc<CancelToken> {
    CancelToken::new()
}

fn config(scratch: &ScratchDir) -> CoreConfig {
    CoreConfig {
        data_root: scratch.path().join("store").to_string_lossy().into_owned(),
        cache_root: scratch.path().join("cache").to_string_lossy().into_owned(),
    }
}

#[test]
fn the_intent_query_round_trips_over_the_exported_path() {
    let scratch = ScratchDir::new("ffi-intents");
    let store = scratch.path().join("store");
    let core = sb_test_support::block_on(open_core(token(), config(&scratch))).unwrap();
    let archive = sb_test_support::block_on(core.accounts(token())).unwrap()[0]
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
               ('i1', '{archive}', 1, 'set_flags', '{{"v":1,"occurrence_ids":[7,9]}}',
                '{{"v":1,"add":["\\Seen"],"remove":[]}}', '{{}}', 'parked', NULL, 'no',
                'i1', 'user', 1, 1),
               ('i2', '{ACCOUNT}', 1, 'move', '{{"v":1,"occurrence_ids":[8]}}',
                '{{"v":1,"to_mailbox_id":42}}', '{{}}', 'quarantined', 'epoch_change', NULL,
                'i2', 'user', 2, 2);"#
        ),
    );
    let core = sb_test_support::block_on(open_core(token(), config(&scratch))).unwrap();

    let page = sb_test_support::block_on(core.intents(
        token(),
        IntentFilter::default(),
        IntentAnchor::Top,
        10,
    ))
    .unwrap();
    assert_eq!(page.rows.len(), 2);
    assert_eq!(
        page.rows[0],
        Intent {
            intent_id: "i1".to_owned(),
            account_id: archive,
            kind: IntentKind::SetFlags,
            state: IntentState::Parked,
            target_occurrence_ids: vec![7, 9],
            quarantine_reason: None,
            last_error: Some("no".to_owned()),
            effect: IntentEffect::SetFlags {
                add: vec!["\\Seen".to_owned()],
                remove: vec![],
            },
            created_at_ms: 1,
        }
    );
    assert_eq!(page.rows[1].kind, IntentKind::Move);
    assert_eq!(page.rows[1].state, IntentState::Quarantined);
    assert_eq!(
        page.rows[1].quarantine_reason,
        Some(QuarantineReason::EpochChange)
    );
    assert_eq!(
        page.rows[1].effect,
        IntentEffect::ToMailbox { mailbox_id: 42 }
    );

    let filtered = IntentFilter {
        account_id: Some(ACCOUNT.to_owned()),
        state: Some(IntentState::Quarantined),
    };
    let page =
        sb_test_support::block_on(core.intents(token(), filtered, IntentAnchor::Top, 10)).unwrap();
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].intent_id, "i2");

    let cancelled = token();
    cancelled.cancel();
    let error = sb_test_support::block_on(core.intents(
        cancelled,
        IntentFilter::default(),
        IntentAnchor::Top,
        10,
    ))
    .unwrap_err();
    assert!(matches!(error, CoreError::Cancelled));

    sb_test_support::block_on(core.shutdown());
}

/// A reserved operation's raw args survive the boundary.
#[test]
fn a_reserved_effect_keeps_its_raw_args() {
    let effect = IntentEffect::from_service(sb_service::IntentEffect::Reserved {
        args_json: r#"{"v":1,"mailbox_id":4}"#.to_owned(),
    });
    assert_eq!(
        effect,
        IntentEffect::Reserved {
            args_json: r#"{"v":1,"mailbox_id":4}"#.to_owned()
        }
    );
}

/// `release_intents` and `cancel_intents` cross the export: release
/// resets a parked intent, cancel ends a pending star and reverts its
/// flag, a repeated key answers the stored receipt, a terminal id is
/// `Conflict`, a missing one `NotFound`, and a cancelled token is
/// `Cancelled`.
#[test]
fn intent_health_actions_cross_the_ffi() {
    let scratch = ScratchDir::new("ffi-intents-health");
    let store = scratch.path().join("store");
    let core = sb_test_support::block_on(open_core(token(), config(&scratch))).unwrap();
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
    let core = sb_test_support::block_on(open_core(token(), config(&scratch))).unwrap();
    let db = store.join("state.sqlite");

    let receipt = sb_test_support::block_on(core.release_intents(
        token(),
        "release-ffi".to_owned(),
        vec!["i1".to_owned()],
    ))
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
    let replayed = sb_test_support::block_on(core.release_intents(
        token(),
        "release-ffi".to_owned(),
        vec!["i1".to_owned()],
    ))
    .unwrap();
    assert_eq!(replayed, receipt);

    let receipt = sb_test_support::block_on(core.cancel_intents(
        token(),
        "cancel-ffi".to_owned(),
        vec!["i2".to_owned()],
    ))
    .unwrap();
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

    let conflict = sb_test_support::block_on(core.cancel_intents(
        token(),
        "cancel-done-ffi".to_owned(),
        vec!["i3".to_owned()],
    ))
    .unwrap_err();
    assert!(
        matches!(conflict, CoreError::Conflict { .. }),
        "{conflict:?}"
    );
    let missing = sb_test_support::block_on(core.cancel_intents(
        token(),
        "cancel-missing-ffi".to_owned(),
        vec!["nope".to_owned()],
    ))
    .unwrap_err();
    assert!(matches!(missing, CoreError::NotFound { .. }), "{missing:?}");
    let empty = sb_test_support::block_on(core.cancel_intents(
        token(),
        "cancel-empty-ffi".to_owned(),
        vec![],
    ))
    .unwrap_err();
    assert!(
        matches!(empty, CoreError::InvalidInput { ref field, .. } if field == "intent_ids"),
        "{empty:?}"
    );

    let cancelled = token();
    cancelled.cancel();
    let error = sb_test_support::block_on(core.release_intents(
        cancelled,
        "release-cancelled-ffi".to_owned(),
        vec!["i1".to_owned()],
    ))
    .unwrap_err();
    assert!(matches!(error, CoreError::Cancelled), "{error:?}");

    sb_test_support::block_on(core.shutdown());
}
