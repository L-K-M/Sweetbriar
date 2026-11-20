//! `diagnostics_package()` over the service boundary (spec/10): the
//! document's shape and THE log-scan test, seeded with every secret,
//! address, display name and subject the fixtures know.

use serde_json::Value;

use std::sync::Arc;

use sb_test_support::{
    FakeProtonServer, FakeSecrets, ScratchDir, block_on, cert_der, execute_sql, system_clock,
};

use super::*;
use crate::{CoreConfig, open_core};

/// Strings the package must never carry (spec/10): a display name, two
/// identity addresses, a refusal quoting one, a subject, a Proton
/// username and a token-like credential reference.
const SECRET_DISPLAY_NAME: &str = "Owner Display Name";
const SECRET_IDENTITY_NAME: &str = "Owner Identity";
const SECRET_ADDRESS: &str = "owner@example.test";
const SECRET_OTHER_ADDRESS: &str = "alias@example.test";
const SECRET_SUBJECT: &str = "Quarterly Secret Subject";
const SECRET_USERNAME: &str = "owner-user";
const SECRET_CREDENTIAL: &str = "tok-session-SECRET-abcdef123456";
/// A valid account UUID (a scope needs one).
const ACCOUNT: &str = "11111111-1111-1111-1111-111111111111";
/// The refusal once both quoted addresses are masked.
const MASKED_REFUSAL: &str = "Proton refused this version for <address> and <address>";

fn config(scratch: &ScratchDir) -> CoreConfig {
    CoreConfig {
        data_root: scratch.path().join("store"),
        cache_root: scratch.path().join("cache"),
    }
}

fn open(scratch: &ScratchDir) -> Core {
    block_on(open_core(config(scratch))).unwrap()
}

/// The core over an in-memory secret store, so the seeded Proton
/// account's restore sees "no session" on every host. The system store
/// answers `Unavailable` on Linux CI, and that pause would replace the
/// seeded refusal reason. The fake server is never called.
async fn open_with_fake_secrets(scratch: &ScratchDir) -> Core {
    let server = FakeProtonServer::serve(Vec::new()).await.unwrap();
    let engine = sb_engine::Engine::open_with_proton_and_cache(
        &scratch.path().join("store"),
        &scratch.path().join("cache"),
        Arc::new(FakeSecrets::new()),
        server.base_url().to_owned(),
        vec![cert_der()],
        server.verifier(),
        system_clock(),
    )
    .await
    .unwrap();
    Core::around(engine)
}

#[test]
fn the_document_holds_exactly_the_listed_top_level_fields() {
    let scratch = ScratchDir::new("service-diagnostics-shape");
    let core = open(&scratch);

    let text = block_on(core.diagnostics_package("app-1.2.3", "macOS 15.1")).unwrap();
    let json: Value = serde_json::from_str(&text).expect("one UTF-8 JSON document");
    let keys = json.as_object().unwrap();
    let mut names = keys.keys().cloned().collect::<Vec<_>>();
    names.sort_unstable();
    assert_eq!(
        names,
        vec![
            "accounts",
            "app_version",
            "core_version",
            "integrity",
            "os_version",
            "schema_version",
        ]
    );
    assert_eq!(keys["core_version"], env!("CARGO_PKG_VERSION"));
    assert_eq!(keys["app_version"], "app-1.2.3");
    assert_eq!(keys["os_version"], "macOS 15.1");
    assert!(keys["schema_version"].as_i64().unwrap() > 0);
    assert_eq!(keys["integrity"], serde_json::json!({"state": "sound"}));

    let accounts = keys["accounts"].as_array().unwrap();
    assert_eq!(accounts.len(), 1, "the local archive");
    let archive = &accounts[0];
    assert_eq!(archive["kind"], "local");
    assert_eq!(archive["state"], "active");
    assert_eq!(archive["condition"], "healthy");
    assert!(archive.get("reason").is_none());
    assert_eq!(archive["breaker_events"], serde_json::json!([]));
    assert_eq!(archive["parked_intents"], 0);
    assert_eq!(archive["quarantined_intents"], 0);
    assert!(archive.get("proton").is_none());
    let mut coverage_keys = archive["coverage"]
        .as_object()
        .unwrap()
        .keys()
        .cloned()
        .collect::<Vec<_>>();
    coverage_keys.sort_unstable();
    assert_eq!(
        coverage_keys,
        vec![
            "bodies_acquired",
            "failed",
            "headers_synced",
            "indexed",
            "partial",
            "pending",
            "queue_length",
            "total_messages",
        ]
    );

    block_on(core.shutdown());
}

#[tokio::test]
async fn the_log_scan_finds_no_seeded_secret_address_name_or_subject() {
    let scratch = ScratchDir::new("service-diagnostics-log-scan");
    let core = open_core(config(&scratch)).await.unwrap();
    core.shutdown().await;

    execute_sql(
        &scratch.path().join("store/state.sqlite"),
        &format!(
            r#"INSERT INTO accounts
                 (account_id, kind, provider, display_name, state, state_reason, sort_order, created_at)
               VALUES ('{ACCOUNT}', 'proton', 'proton_api', '{SECRET_DISPLAY_NAME}', 'needs_attention',
                       'Proton refused this version of Sweetbriar', 1, 0);
               INSERT INTO proton_accounts
                 (account_id, api_base_url, username, proton_user_id, credential_ref, password_mode,
                  listing_state, refresh_count, diagnostics_json)
               VALUES ('{ACCOUNT}', 'https://mail.proton.me/api', '{SECRET_USERNAME}', 'user-1',
                       '{SECRET_CREDENTIAL}', 2, 'complete', 4,
                       '{{"v":1,"code":2064,
                         "message":"Proton refused this version for {SECRET_ADDRESS} and {SECRET_OTHER_ADDRESS}",
                         "at":1735689600000}}');
               INSERT INTO identities
                 (identity_id, account_id, email, display_name, is_default, sendable)
               VALUES ('id-1', '{ACCOUNT}', '{SECRET_ADDRESS}', '{SECRET_IDENTITY_NAME}', 1, 'validated');
               INSERT INTO logical_messages (message_id, account_id, created_at)
               VALUES (1, '{ACCOUNT}', 0);
               INSERT INTO message_headers (message_id, raw_fields, subject, fetched_at)
               VALUES (1, x'', '{SECRET_SUBJECT}', 0);
               INSERT INTO breaker_events
                 (event_id, account_id, mailbox_id, kind, details_json, state, created_at)
               VALUES ('evt-1', '{ACCOUNT}', NULL, 'bulk_disappearance', '{{"disappeared":3}}', 'open', 5);
               INSERT INTO intents
                 (intent_id, account_id, config_generation, op, target_json, args_json,
                  preconditions_json, state, quarantine_reason, last_error, idempotency_key,
                  origin, created_at, updated_at)
               VALUES
                 ('i1', '{ACCOUNT}', 1, 'set_flags', '{{"v":1,"occurrence_ids":[7]}}',
                  '{{"v":1,"add":["\\Seen"],"remove":[]}}', '{{}}', 'parked', NULL, NULL,
                  'i1', 'user', 1, 1),
                 ('i2', '{ACCOUNT}', 1, 'move', '{{"v":1,"occurrence_ids":[7]}}',
                  '{{"v":1,"to_mailbox_id":42}}', '{{}}', 'quarantined', 'epoch_change', NULL,
                  'i2', 'user', 2, 2);"#
        ),
    );

    let core = open_with_fake_secrets(&scratch).await;
    let text = core
        .diagnostics_package("app-test", "os-test")
        .await
        .unwrap();

    for forbidden in [
        SECRET_DISPLAY_NAME,
        SECRET_IDENTITY_NAME,
        SECRET_ADDRESS,
        SECRET_OTHER_ADDRESS,
        SECRET_SUBJECT,
        SECRET_USERNAME,
        SECRET_CREDENTIAL,
    ] {
        assert!(!text.contains(forbidden), "the package holds {forbidden:?}");
    }
    assert!(
        text.contains("<address>"),
        "the refusal's address is masked"
    );

    let json: Value = serde_json::from_str(&text).unwrap();
    let accounts = json["accounts"].as_array().unwrap();
    let proton = accounts
        .iter()
        .find(|account| account["account_id"] == ACCOUNT)
        .expect("the Proton account");
    assert_eq!(proton["kind"], "proton");
    assert_eq!(proton["state"], "needs_attention");
    assert_eq!(
        proton["reason"],
        "Proton refused this version of Sweetbriar"
    );
    assert_eq!(
        proton["condition"],
        serde_json::json!({"version_refused": {"message": MASKED_REFUSAL}})
    );
    assert_eq!(
        proton["breaker_events"],
        serde_json::json!([{
            "id": "evt-1",
            "mailbox_id": Value::Null,
            "count": 3,
            "detected_at_ms": 5,
        }])
    );
    assert_eq!(proton["parked_intents"], 1);
    assert_eq!(proton["quarantined_intents"], 1);
    assert_eq!(proton["proton"]["listing_state"], "complete");
    assert_eq!(proton["proton"]["refresh_count"], 4);
    assert_eq!(
        proton["proton"]["last_refusal"],
        serde_json::json!({
            "code": 2064,
            "message": MASKED_REFUSAL,
            "at_ms": 1735689600000i64,
        })
    );

    core.shutdown().await;
}
