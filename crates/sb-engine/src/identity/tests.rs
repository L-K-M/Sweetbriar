//! Identity command tests (spec/07 "Identities"): the two commands'
//! rows, receipts and refusals.

use sb_resources::{IdentitySendable, StoreError, WritePriority};
use sb_test_support::{ScratchDir, block_on};

use super::*;
use crate::{Engine, EngineError};

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
