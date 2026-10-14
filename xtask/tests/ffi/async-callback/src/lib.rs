//! The async-callback fixture: an exported surface that violates
//! spec/07 — a trait Swift implements whose method is `async`
//! (UniFFI's "foreign async callback", declared the way UniFFI's own
//! futures fixture declares it: `with_foreign` over
//! `#[async_trait]`). UniFFI 0.32.2's Swift 6 async-callback fix is
//! unreleased, which is why `cargo xtask check-ffi` must flag exactly
//! this shape. Nothing here is called; the metadata alone is the
//! test.

use std::sync::Arc;

uniffi::setup_scaffolding!();

/// The violation: a foreign-implemented trait with an async method.
#[uniffi::export(with_foreign)]
#[async_trait::async_trait]
pub trait SlowGetter: Send + Sync {
    async fn get(&self, key: String) -> String;
}

/// A legal object, so the fixture also carries ordinary exports.
#[derive(uniffi::Object)]
pub struct Fixture;

#[uniffi::export]
impl Fixture {
    #[uniffi::constructor]
    pub fn new() -> Arc<Self> {
        Arc::new(Self)
    }

    /// Takes the foreign trait; never awaits it.
    pub fn take(&self, getter: Arc<dyn SlowGetter>) -> u32 {
        drop(getter);
        0
    }
}
