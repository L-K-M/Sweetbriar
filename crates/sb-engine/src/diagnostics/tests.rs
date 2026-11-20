//! `diagnostics_package()` (spec/10): the store facts on a fresh store,
//! and the versions the caller supplies reaching the document.

use sb_test_support::{ScratchDir, block_on};

use super::*;
use crate::AccountKind;

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
