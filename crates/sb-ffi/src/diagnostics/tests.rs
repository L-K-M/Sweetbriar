//! `diagnostics_package()` across the FFI boundary (plan M2.7c4): the
//! text reaches Swift, and a cancelled query answers `Cancelled`
//! without moving anything.

use std::sync::Arc;

use sb_test_support::{ScratchDir, block_on};

use super::*;
use crate::api::{CoreConfig, open_core};

fn scratch_core(label: &str) -> (ScratchDir, Arc<Core>) {
    let scratch = ScratchDir::new(label);
    let config = CoreConfig {
        data_root: scratch.path().join("store").to_string_lossy().into_owned(),
        cache_root: scratch.path().join("cache").to_string_lossy().into_owned(),
    };
    let core = block_on(open_core(token(), config)).unwrap();
    (scratch, core)
}

fn token() -> Arc<CancelToken> {
    CancelToken::new()
}

#[test]
fn the_package_text_crosses_the_boundary() {
    let (_scratch, core) = scratch_core("ffi-diagnostics-round-trip");
    let text = block_on(core.diagnostics_package(
        token(),
        "app-1.2.3".to_owned(),
        "macOS 15.1".to_owned(),
    ))
    .expect("the diagnostics package");
    assert!(text.starts_with('{') && text.ends_with('}'), "{text}");
    for field in [
        "\"core_version\"",
        "\"app_version\"",
        "\"os_version\"",
        "\"schema_version\"",
        "\"integrity\"",
        "\"accounts\"",
        "\"parked_intents\"",
        "\"quarantined_intents\"",
        "\"coverage\"",
    ] {
        assert!(text.contains(field), "missing {field}: {text}");
    }
    assert!(text.contains(env!("CARGO_PKG_VERSION")));

    block_on(core.shutdown());
}

#[test]
fn a_cancelled_token_answers_cancelled_and_a_fresh_call_still_works() {
    let (_scratch, core) = scratch_core("ffi-diagnostics-cancel");
    let cancelled = token();
    cancelled.cancel();

    let err = block_on(core.diagnostics_package(
        cancelled,
        "app-1.2.3".to_owned(),
        "macOS 15.1".to_owned(),
    ))
    .expect_err("the call was cancelled before it began");
    assert!(matches!(err, CoreError::Cancelled), "{err}");

    let text = block_on(core.diagnostics_package(
        token(),
        "app-1.2.3".to_owned(),
        "macOS 15.1".to_owned(),
    ))
    .expect("a fresh call is unaffected");
    assert!(text.contains("\"accounts\""));

    block_on(core.shutdown());
}
