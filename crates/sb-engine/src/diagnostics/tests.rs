//! `diagnostics_package()` (spec/10): the store facts on a fresh store,
//! and the versions the caller supplies reaching the document.

use sb_test_support::{ScratchDir, block_on};

use super::*;
use crate::AccountKind;

#[test]
fn valid_json_without_a_refusal_key_contributes_no_refusal() {
    // Unparseable JSON and JSON holding none of spec/02's refusal
    // keys contribute no refusal, as the parser's doc says.
    assert_eq!(parse_refusal("not json"), None);
    assert_eq!(parse_refusal("{}"), None);
    assert_eq!(parse_refusal(r#"{"v":1}"#), None);

    let refusal = parse_refusal(
        r#"{"v":1,"code":2064,
            "message":"Proton refused this version for owner@example.test",
            "at":1735689600000}"#,
    )
    .expect("the refusal keys are present");
    assert_eq!(refusal.code, Some(2064));
    assert_eq!(
        refusal.message.as_deref(),
        Some("Proton refused this version for <address>")
    );
    assert_eq!(refusal.at_ms, Some(1735689600000));

    // One key alone is still a refusal; an absent field stays absent.
    let partial = parse_refusal(r#"{"code":2064}"#).expect("code is a refusal key");
    assert_eq!(partial.code, Some(2064));
    assert_eq!(partial.message, None);
    assert_eq!(partial.at_ms, None);
}

#[test]
fn a_fresh_store_reports_its_schema_and_the_shells_versions() {
    let scratch = ScratchDir::new("engine-diagnostics-fresh");
    let engine = block_on(Engine::open(scratch.path())).unwrap();

    let package = block_on(engine.diagnostics_package("app-1.2.3", "macOS 15.1")).unwrap();
    assert_eq!(package.app_version, "app-1.2.3");
    assert_eq!(package.os_version, "macOS 15.1");
    assert_eq!(package.core_version, env!("CARGO_PKG_VERSION"));
    assert!(package.schema_version > 0, "the migrated store version");
    assert_eq!(package.integrity, DiagnosticsIntegrity::Sound);
    assert_eq!(package.accounts.len(), 1, "the local archive");
    let archive = &package.accounts[0];
    assert_eq!(archive.kind, AccountKind::Local);
    assert!(archive.proton.is_none());
    assert_eq!(archive.parked_intents, 0);
    assert_eq!(archive.quarantined_intents, 0);
    assert_eq!(archive.coverage.total_messages, 0);

    block_on(engine.shutdown()).unwrap();
}
