//! Identity commands over the service boundary (spec/07 "Identities"):
//! `upsert_identity` and `set_default_identity`, with a service-owned
//! params record so no engine type crosses into L1 (spec/01 rule 3).

use crate::{Core, CoreError, IdentitySendable, Receipt};

/// `upsert_identity`'s parameters (spec/07): the identity's own fields.
#[derive(Clone)]
pub struct IdentityParams {
    pub identity_id: String,
    pub account_id: String,
    pub email: String,
    pub display_name: String,
    pub reply_to: Option<String>,
    pub signature_source: Option<String>,
    pub sendable: IdentitySendable,
}

impl Core {
    /// `upsert_identity(params)` (spec/07) under an idempotency key: a
    /// repeated key returns the stored receipt. A Proton account's
    /// identities come from its backend and are refused; another
    /// account's identity id, a taken address and malformed fields are
    /// refused too.
    pub async fn upsert_identity(
        &self,
        idempotency_key: &str,
        params: IdentityParams,
    ) -> Result<Receipt, CoreError> {
        let sendable = match params.sendable {
            IdentitySendable::Validated => sb_engine::IdentitySendable::Validated,
            IdentitySendable::Provisional => sb_engine::IdentitySendable::Provisional,
            IdentitySendable::Unavailable => sb_engine::IdentitySendable::Unavailable,
        };
        let receipt = self
            .engine()
            .upsert_identity(
                idempotency_key,
                sb_engine::IdentityParams {
                    identity_id: params.identity_id,
                    account_id: params.account_id,
                    email: params.email,
                    display_name: params.display_name,
                    reply_to: params.reply_to,
                    signature_source: params.signature_source,
                    sendable,
                },
            )
            .await?;
        Ok(Receipt::from_engine(&receipt))
    }

    /// `set_default_identity(identity_id)` (spec/07, spec/05) under an
    /// idempotency key: exactly one default per account afterwards. An
    /// identity that isn't `validated` or `provisional` is refused.
    pub async fn set_default_identity(
        &self,
        idempotency_key: &str,
        identity_id: &str,
    ) -> Result<Receipt, CoreError> {
        let receipt = self
            .engine()
            .set_default_identity(idempotency_key, identity_id)
            .await?;
        Ok(Receipt::from_engine(&receipt))
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CoreConfig, open_core};
    use sb_test_support::{ScratchDir, block_on, execute_sql};

    #[test]
    fn the_commands_cross_the_boundary_and_replay() {
        let s = ScratchDir::new("service-identities");
        let core = block_on(open_core(CoreConfig {
            data_root: s.path().join("data").to_str().unwrap().into(),
            cache_root: s.path().join("cache").to_str().unwrap().into(),
        }))
        .unwrap();
        execute_sql(
            &s.path().join("data/state.sqlite"),
            "INSERT INTO accounts (account_id, kind, provider, display_name, state, sort_order, created_at)
             VALUES ('acct', 'imap', 'generic_imap', 'Work', 'active', 1, 0);
             INSERT INTO identities (identity_id, account_id, email, display_name, is_default, sendable)
             VALUES ('b', 'acct', 'default@example.test', 'Default', 1, 'validated');
             INSERT INTO accounts (account_id, kind, provider, display_name, state, sort_order, created_at)
             VALUES ('proton', 'proton', 'proton_api', 'Proton', 'active', 2, 0);
             INSERT INTO identities (identity_id, account_id, email, display_name, is_default, sendable)
             VALUES ('gone', 'acct', 'gone@example.test', 'Gone', 0, 'unavailable');",
        );

        let params = |identity_id: &str, account_id: &str| IdentityParams {
            identity_id: identity_id.to_owned(),
            account_id: account_id.to_owned(),
            email: format!("{identity_id}@example.test"),
            display_name: "Alias".to_owned(),
            reply_to: None,
            signature_source: None,
            sendable: IdentitySendable::Provisional,
        };
        let key = core.new_idempotency_key();
        let created = block_on(core.upsert_identity(&key, params("a", "acct"))).unwrap();
        assert_eq!(created.command, "upsert_identity");
        let replayed = block_on(core.upsert_identity(&key, params("a", "acct"))).unwrap();
        assert_eq!(replayed.result_json, created.result_json);
        let set = block_on(core.set_default_identity(&core.new_idempotency_key(), "a")).unwrap();
        assert_eq!(set.command, "set_default_identity");

        let error =
            block_on(core.upsert_identity(&core.new_idempotency_key(), params("x", "proton")))
                .unwrap_err();
        assert!(
            matches!(error, CoreError::InvalidInput { ref field, .. } if field == "account_id"),
            "{error:?}"
        );
        let error =
            block_on(core.set_default_identity(&core.new_idempotency_key(), "gone")).unwrap_err();
        assert!(
            matches!(error, CoreError::InvalidInput { ref field, .. } if field == "identity_id"),
            "{error:?}"
        );
        block_on(core.shutdown());
    }
}
