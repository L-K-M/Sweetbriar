//! Address and user events (plan M2.15c4a): the keys unlock again with
//! the passphrase kept in memory and the identities follow, and keys it
//! no longer unlocks need the owner to sign in again.

use super::events::until;
use super::*;

use sb_test_support::FakeMessage;

const ALIAS: &str = "owner-alias";

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
    until(
        &h,
        &format!("identities WHERE identity_id = '{ALIAS}' AND display_name = 'Owner on holiday'"),
        1,
    )
    .await;
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
    until(
        &h,
        &format!("identities WHERE identity_id = '{ALIAS}' AND display_name = 'Owner on holiday'"),
        1,
    )
    .await;
}
