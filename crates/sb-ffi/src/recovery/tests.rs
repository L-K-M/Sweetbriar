//! The FFI boundary for `recovery(account_id?)`: a fresh core answers
//! an empty page, a vanished message crosses field by field, an unknown
//! account is `NotFound`, and a cancelled token is `Cancelled`.

use super::*;
use crate::api::{CoreConfig, open_core};
use sb_test_support::ScratchDir;

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
fn the_recovery_query_round_trips_over_the_exported_path() {
    let scratch = ScratchDir::new("ffi-recovery");
    let store = scratch.path().join("store");
    let core = sb_test_support::block_on(open_core(token(), config(&scratch))).unwrap();
    let archive = sb_test_support::block_on(core.accounts(token())).unwrap()[0]
        .account_id
        .clone();
    sb_test_support::block_on(core.shutdown());

    sb_test_support::execute_sql(
        &store.join("state.sqlite"),
        &format!(
            r#"INSERT INTO mailboxes (account_id, remote_name, role, kind, selectable, sync_state)
             VALUES ('{archive}', 'Inbox', 'inbox', 'folder', 1, 'synced');
             INSERT INTO logical_messages (message_id, account_id, created_at)
             VALUES (1, '{archive}', 0);
             INSERT INTO occurrences (account_id, mailbox_id, message_id, state, first_seen_at,
               last_seen_at)
             VALUES ('{archive}', 1, 1, 'vanished', 100, 100);"#
        ),
    );
    let core = sb_test_support::block_on(open_core(token(), config(&scratch))).unwrap();

    let page =
        sb_test_support::block_on(core.recovery(token(), None, RecoveryAnchor::Top, 10)).unwrap();
    assert_eq!(page.rows.len(), 1);
    assert_eq!(
        page.rows[0],
        Recovery {
            message_id: 1,
            account_id: archive,
            mailbox_id: 1,
            mailbox_name: "Inbox".to_owned(),
            state: RecoveryState::Vanished,
            last_seen_at_ms: 100,
            occurrence_id: 1,
        }
    );

    let error = sb_test_support::block_on(core.recovery(
        token(),
        Some("no-such-account".to_owned()),
        RecoveryAnchor::Top,
        10,
    ))
    .unwrap_err();
    assert!(matches!(error, CoreError::NotFound { .. }));

    let cancelled = token();
    cancelled.cancel();
    let error = sb_test_support::block_on(core.recovery(cancelled, None, RecoveryAnchor::Top, 10))
        .unwrap_err();
    assert!(matches!(error, CoreError::Cancelled));

    sb_test_support::block_on(core.shutdown());
}
