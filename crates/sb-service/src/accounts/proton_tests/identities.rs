//! Address and user events (plan M2.15c4a): the keys unlock again with
//! the passphrase kept in memory and the identities follow, and keys it
//! no longer unlocks need the owner to sign in again.

use super::events::{event_requests, until};
use super::*;

use sb_test_support::FakeMessage;

const PRIMARY: &str = "owner-primary";
const ALIAS: &str = "owner-alias";
const RENAMED_ALIAS: &str = "identities WHERE identity_id = 'owner-alias'
     AND display_name = 'Owner on holiday'";

fn refreshes(h: &Harness) -> usize {
    h.server
        .requests()
        .iter()
        .filter(|request| request.method == "POST" && request.path == "/auth/v4/refresh")
        .count()
}

async fn synced(h: &Harness) -> String {
    h.server.add_message(
        USER,
        FakeMessage::new("m1")
            .with_time(100)
            .with_labels(&["0", "5"]),
    );
    let (_id, state) = begin(h, "identities").await;
    let id = account_id(&state);
    listed(&h.core, &id).await;
    id
}

#[tokio::test]
async fn an_address_event_refreshes_the_identities_and_a_user_event_changes_nothing() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let id = synced(&h).await;
    let generation = account_column(&h, &id, "config_generation");

    h.server.rename_address(USER, ALIAS, "Owner on holiday");
    until(&h, RENAMED_ALIAS, 1).await;
    assert_eq!(account_column(&h, &id, "config_generation"), generation + 1);
    let unlocked = h.core.engine().proton_session(&id).await.unwrap();
    assert!(
        unlocked
            .iter()
            .any(|identity| identity.id == ALIAS && identity.display_name == "Owner on holiday")
    );

    // A user event for something else unlocks again and changes nothing.
    h.server.touch_user(USER);
    h.server.add_message(
        USER,
        FakeMessage::new("m2")
            .with_time(200)
            .with_labels(&["0", "5"]),
    );
    until(&h, "logical_messages WHERE provider_key = 'm2'", 1).await;
    assert_eq!(account_column(&h, &id, "config_generation"), generation + 1);
    assert_eq!(count(&h.scratch, "identities"), 2);
}

#[tokio::test]
async fn keys_the_passphrase_no_longer_unlocks_need_a_sign_in_again() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let id = synced(&h).await;

    h.server.retire_address_keys(USER);
    h.server.add_message(
        USER,
        FakeMessage::new("m2")
            .with_time(200)
            .with_labels(&["0", "5"]),
    );
    until(
        &h,
        &format!(
            "accounts WHERE account_id = '{id}' AND state = 'needs_attention'
             AND state_reason = 'Sign in again'"
        ),
        1,
    )
    .await;
    // The response wasn't applied, and the data stays.
    assert!(h.core.engine().proton_session(&id).await.is_none());
    assert_eq!(
        count(&h.scratch, "logical_messages WHERE provider_key = 'm2'"),
        0
    );
    assert_eq!(
        count(&h.scratch, "logical_messages WHERE provider_key = 'm1'"),
        1
    );
    assert_eq!(count(&h.scratch, "identities"), 2);
}

#[tokio::test]
async fn an_address_event_in_a_refresh_response_still_refreshes_the_identities() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let _id = synced(&h).await;

    h.server.set_refresh_mail(USER);
    h.server.rename_address(USER, ALIAS, "Owner on holiday");
    until(&h, RENAMED_ALIAS, 1).await;
}

#[tokio::test]
async fn a_response_that_changes_an_identity_applies_its_messages_at_once() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let _id = synced(&h).await;
    let polled = event_requests(&h.server).len();

    // One response: the identities' write bumps the account row's
    // revision, not the Proton row's its message changes are written
    // under, so they apply without a retry.
    h.server.rename_address(USER, ALIAS, "Owner on holiday");
    h.server.add_message(
        USER,
        FakeMessage::new("m2")
            .with_time(200)
            .with_labels(&["0", "5"]),
    );
    until(&h, "logical_messages WHERE provider_key = 'm2'", 1).await;
    assert_eq!(count(&h.scratch, RENAMED_ALIAS), 1);
    let mut since = event_requests(&h.server).split_off(polled);
    let fetched = since.len();
    since.dedup();
    assert_eq!(
        since.len(),
        fetched,
        "a response was fetched again: {since:?}"
    );
}

#[tokio::test]
async fn a_refused_token_during_the_key_refresh_refreshes_once_and_goes_on() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let id = synced(&h).await;
    let before = refreshes(&h);

    // The key refresh's addresses request is refused as an expired token.
    h.server.set_fault(Some(Fault::OnceAt {
        path: "/core/v4/addresses".to_owned(),
        fault: Box::new(Fault::Code {
            status: 401,
            code: 401,
        }),
    }));
    h.server.rename_address(USER, ALIAS, "Owner on holiday");
    until(&h, RENAMED_ALIAS, 1).await;
    assert_eq!(refreshes(&h), before + 1);
    assert_eq!(account(&h.core, &id).await.state, AccountState::Active);
    assert!(h.core.engine().proton_session(&id).await.is_some());
}

#[tokio::test]
async fn signing_in_again_after_the_keys_stopped_unlocking_resumes_the_events() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let id = synced(&h).await;
    h.server.retire_address_keys(USER);
    h.server.add_message(
        USER,
        FakeMessage::new("m2")
            .with_time(200)
            .with_labels(&["0", "5"]),
    );
    until(
        &h,
        &format!("accounts WHERE account_id = '{id}' AND state = 'needs_attention'"),
        1,
    )
    .await;

    // The owner fixes the keys elsewhere and signs in again.
    h.server.reactivate_address_keys(USER);
    let mut again = params();
    again.account_id = Some(id.clone());
    let (_sign_in_id, state) = begin_with(&h, "again", again).await;
    assert_eq!(account_id(&state), id);
    until(&h, "logical_messages WHERE provider_key = 'm2'", 1).await;
    assert_eq!(account(&h.core, &id).await.state, AccountState::Active);
}

#[tokio::test]
async fn a_deleted_address_keeps_its_identity_unsendable_until_it_comes_back() {
    let h = setup(vec![TestAccount::new(USER, PASSWORD)]).await;
    let id = synced(&h).await;
    let sending_default = format!(
        "identities WHERE identity_id = '{PRIMARY}' AND is_default = 1
         AND sendable <> 'unavailable'"
    );
    assert_eq!(count(&h.scratch, &sending_default), 1);
    let generation = account_column(&h, &id, "config_generation");

    // spec/02: an address no longer returned keeps its identity row,
    // which can't send; the next address becomes the default.
    h.server.delete_address(USER, PRIMARY);
    until(
        &h,
        &format!(
            "identities WHERE identity_id = '{PRIMARY}' AND is_default = 0
             AND sendable = 'unavailable'"
        ),
        1,
    )
    .await;
    assert_eq!(
        count(
            &h.scratch,
            &format!("identities WHERE identity_id = '{ALIAS}' AND is_default = 1")
        ),
        1
    );
    assert_eq!(count(&h.scratch, "identities"), 2);
    assert_eq!(account_column(&h, &id, "config_generation"), generation + 1);

    // The same address coming back is the same identity, sendable and
    // the default again.
    h.server.restore_address(USER, PRIMARY);
    until(&h, &sending_default, 1).await;
    assert_eq!(count(&h.scratch, "identities"), 2);
    assert_eq!(account_column(&h, &id, "config_generation"), generation + 2);
}
