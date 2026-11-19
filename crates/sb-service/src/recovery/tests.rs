//! `Core::recovery` (spec/07): the Recovery list's rows, order,
//! account filter and `NotFound` across the service boundary.

use super::*;
use crate::{CoreConfig, open_core};

const ACCOUNT: &str = "00000000-0000-4000-8000-0000000000a0";

fn config(scratch: &sb_test_support::ScratchDir) -> CoreConfig {
    CoreConfig {
        data_root: scratch.path().join("store"),
        cache_root: scratch.path().join("cache"),
    }
}

/// A fresh core's Recovery list is empty.
#[test]
fn a_fresh_core_has_no_recovery_rows() {
    let scratch = sb_test_support::ScratchDir::new("service-recovery-empty");
    let core = sb_test_support::block_on(open_core(config(&scratch))).unwrap();
    let page = sb_test_support::block_on(core.recovery(None, RecoveryAnchor::Top, 100)).unwrap();
    assert!(page.rows.is_empty());
    assert!(!page.has_more);
    sb_test_support::block_on(core.shutdown());
}

/// A vanished message crosses with its mailbox, state and time,
/// newest first; the account filter narrows it and an unknown account
/// is `NotFound`.
#[test]
fn recovery_rows_cross_and_filter() {
    let scratch = sb_test_support::ScratchDir::new("service-recovery-rows");
    let store = scratch.path().join("store");
    let core = sb_test_support::block_on(open_core(config(&scratch))).unwrap();
    let archive = sb_test_support::block_on(core.accounts()).unwrap()[0]
        .account_id
        .clone();
    sb_test_support::block_on(core.shutdown());

    sb_test_support::execute_sql(
        &store.join("state.sqlite"),
        &format!(
            "INSERT INTO accounts (account_id, kind, provider, display_name, state, sort_order,
               created_at)
             VALUES ('{ACCOUNT}', 'imap', 'proton_bridge', 'Work', 'active', 1, 0);
             INSERT INTO mailboxes (account_id, remote_name, role, kind, selectable, sync_state)
             VALUES ('{archive}', 'Inbox', 'inbox', 'folder', 1, 'synced'),
                    ('{ACCOUNT}', 'Inbox', 'inbox', 'folder', 1, 'synced');
             INSERT INTO logical_messages (message_id, account_id, created_at)
             VALUES (1, '{archive}', 0), (2, '{archive}', 0), (3, '{ACCOUNT}', 0);
             INSERT INTO occurrences (account_id, mailbox_id, message_id, state, first_seen_at,
               last_seen_at)
             VALUES
               ('{archive}', 1, 1, 'vanished', 100, 100),
               ('{archive}', 1, 2, 'epoch_stale', 200, 200),
               ('{ACCOUNT}', 2, 3, 'superseded', 300, 300);"
        ),
    );
    let core = sb_test_support::block_on(open_core(config(&scratch))).unwrap();

    let page = sb_test_support::block_on(core.recovery(None, RecoveryAnchor::Top, 10)).unwrap();
    assert_eq!(
        page.rows
            .iter()
            .map(|row| row.message_id)
            .collect::<Vec<_>>(),
        vec![3, 2, 1]
    );
    assert_eq!(page.rows[2].mailbox_name, "Inbox");
    assert_eq!(page.rows[2].state, RecoveryState::Vanished);
    assert_eq!(page.rows[2].last_seen_at_ms, 100);
    assert_eq!(page.rows[0].state, RecoveryState::Superseded);

    let page =
        sb_test_support::block_on(core.recovery(Some(ACCOUNT.to_owned()), RecoveryAnchor::Top, 10))
            .unwrap();
    assert_eq!(page.rows.len(), 1);
    assert_eq!(page.rows[0].message_id, 3);

    let error = sb_test_support::block_on(core.recovery(
        Some("no-such-account".to_owned()),
        RecoveryAnchor::Top,
        10,
    ))
    .unwrap_err();
    assert!(matches!(error, crate::CoreError::NotFound { .. }));

    sb_test_support::block_on(core.shutdown());
}
