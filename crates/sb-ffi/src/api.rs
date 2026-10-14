//! The exported surface (spec/07 "Shape", "Lifecycle", "Threading,
//! cancellation and FFI rules"): records mirroring `sb-service`'s
//! DTOs field by field, and the shared `Core` object. Every method
//! converts service types into this crate's own records, keeping the
//! boundary one-way — nothing from below crosses into Swift.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::CoreError;

/// Where the core keeps what, as the shell spells it (spec/07
/// "Lifecycle"): `data_root` holds the store (spec/03), `cache_root`
/// the disposable renditions. Strings, not paths: paths don't cross
/// FFI (spec/07 "DTOs").
#[derive(Debug, Clone, uniffi::Record)]
pub struct CoreConfig {
    pub data_root: String,
    pub cache_root: String,
}

/// The record of one command that committed (spec/07 "Commands"),
/// mirroring the service's receipt.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Receipt {
    /// The command's name, e.g. `set_setting`.
    pub command: String,
    /// The command's canonical JSON answer.
    pub result_json: String,
    /// When the command committed, milliseconds since the epoch.
    pub created_at_ms: i64,
}

/// One setting as `settings()` reports it (spec/02): the effective
/// value and whether it is the registry's default.
#[derive(Debug, Clone, PartialEq, Eq, uniffi::Record)]
pub struct Setting {
    pub key: String,
    pub value: String,
    pub is_default: bool,
}

/// Open the core (spec/07 "Lifecycle"): the shells' one entry point.
/// A failed open authorizes nothing; the caller shows the error.
#[uniffi::export]
pub async fn open_core(config: CoreConfig) -> Result<Arc<Core>, CoreError> {
    let service = sb_service::open_core(sb_service::CoreConfig {
        data_root: PathBuf::from(config.data_root),
        cache_root: PathBuf::from(config.cache_root),
    })
    .await?;
    Ok(Arc::new(Core {
        service: RwLock::new(Some(service)),
    }))
}

/// The core as Swift holds it: one shared object (spec/07). The
/// service core sits behind a runtime-agnostic RwLock — the engine
/// below owns the one runtime, and the futures this object returns
/// are driven by the foreign caller's polling, so plain `tokio`
/// lock futures are all that is needed here.
#[derive(uniffi::Object)]
pub struct Core {
    /// The live service core; `None` once `shutdown` took it.
    service: RwLock<Option<sb_service::Core>>,
}

/// What a call after `shutdown` always answers (spec/07: "any call
/// after it is `Unavailable`"). Shared with the error's constructor.
pub(crate) const SHUT_DOWN_MESSAGE: &str = "the core is shut down";

#[uniffi::export]
impl Core {
    /// `set_setting` (spec/07 "Commands"): one command under an
    /// idempotency key the shell made (`UUID()` in Swift). The read
    /// side of the shared lock is held for the call's duration, so a
    /// concurrent `shutdown` waits for it.
    pub async fn set_setting(
        &self,
        idempotency_key: String,
        key: String,
        value: String,
    ) -> Result<Receipt, CoreError> {
        let guard = self.service.read().await;
        let Some(core) = guard.as_ref() else {
            return Err(CoreError::unavailable_shut_down());
        };
        let receipt = core
            .set_setting(&idempotency_key, &key, &value)
            .await
            .map_err(CoreError::from)?;
        Ok(Receipt::from_service(&receipt))
    }

    /// `settings()` (spec/07 "Queries"): every registered key with its
    /// effective value, the default for one never set.
    pub async fn settings(&self) -> Result<Vec<Setting>, CoreError> {
        let guard = self.service.read().await;
        let Some(core) = guard.as_ref() else {
            return Err(CoreError::unavailable_shut_down());
        };
        let settings = core.settings().await.map_err(CoreError::from)?;
        Ok(settings.iter().map(Setting::from_service).collect())
    }

    /// Shut the core down (spec/07 "Lifecycle"): the write side of the
    /// shared lock, so this waits for every call in flight before it
    /// closes the core; a second call finds nothing left and does
    /// nothing. The spec's signature carries no error: a close failure
    /// leaves the marker unrecorded and the next open's `quick_check`
    /// is the safety net (spec/03 "Integrity").
    pub async fn shutdown(&self) {
        let mut guard = self.service.write().await;
        if let Some(core) = guard.take() {
            core.shutdown().await;
        }
    }
}

impl Receipt {
    /// Mirror the service's receipt field by field.
    fn from_service(receipt: &sb_service::Receipt) -> Self {
        Self {
            command: receipt.command.clone(),
            result_json: receipt.result_json.clone(),
            created_at_ms: receipt.created_at_ms,
        }
    }
}

impl Setting {
    /// Mirror the service's setting field by field.
    fn from_service(setting: &sb_service::Setting) -> Self {
        Self {
            key: setting.key.clone(),
            value: setting.value.clone(),
            is_default: setting.is_default,
        }
    }
}

#[cfg(test)]
mod tests {
    use std::pin::pin;
    use std::task::{Context, Waker};

    use super::*;
    use sb_test_support::ScratchDir;

    /// A config over a fresh scratch directory, the way the shell
    /// passes roots it was given.
    fn scratch_config(scratch: &ScratchDir) -> CoreConfig {
        CoreConfig {
            data_root: scratch.path().join("store").to_string_lossy().into_owned(),
            cache_root: scratch.path().join("cache").to_string_lossy().into_owned(),
        }
    }

    /// The pilot's end-to-end path over FFI: open, set a setting
    /// under a shell-made idempotency key, read it back, shut down.
    #[test]
    fn open_set_read_shutdown() {
        let scratch = ScratchDir::new("ffi-lifecycle");
        let core = sb_test_support::block_on(open_core(scratch_config(&scratch)))
            .expect("a fresh store opens");

        let receipt = sb_test_support::block_on(core.set_setting(
            "11111111-1111-4111-8111-111111111111".to_owned(),
            "appearance.compose_cue".to_owned(),
            "strong".to_owned(),
        ))
        .expect("a known key and value commit");
        assert_eq!(receipt.command, "set_setting");
        assert!(receipt.created_at_ms > 0);

        let settings = sb_test_support::block_on(core.settings()).expect("settings read back");
        let setting = settings
            .iter()
            .find(|setting| setting.key == "appearance.compose_cue")
            .expect("the committed setting is listed");
        assert_eq!(setting.value, "strong");
        assert!(!setting.is_default);

        sb_test_support::block_on(core.shutdown());
    }

    /// A call after `shutdown` is `Unavailable` naming the state, and
    /// a second `shutdown` is harmless (spec/07: the shared object).
    #[test]
    fn after_shutdown_calls_answer_unavailable_and_second_shutdown_is_harmless() {
        let scratch = ScratchDir::new("ffi-shutdown");
        let core = sb_test_support::block_on(open_core(scratch_config(&scratch))).expect("opens");

        sb_test_support::block_on(core.shutdown());

        let err = sb_test_support::block_on(core.set_setting(
            "22222222-2222-4222-8222-222222222222".to_owned(),
            "appearance.compose_cue".to_owned(),
            "standard".to_owned(),
        ))
        .expect_err("the core is shut down");
        assert!(
            matches!(err, CoreError::Unavailable { ref reason } if reason == SHUT_DOWN_MESSAGE)
        );

        let err = sb_test_support::block_on(core.settings()).expect_err("the core is shut down");
        assert!(
            matches!(err, CoreError::Unavailable { ref reason } if reason == SHUT_DOWN_MESSAGE)
        );

        sb_test_support::block_on(core.shutdown());
    }

    /// `shutdown` waits for a call in flight: while the read side is
    /// held — as every call holds it for its duration — `shutdown`
    /// stays pending; once it's released, `shutdown` completes and
    /// later calls are `Unavailable`. Polled by hand, so nothing
    /// depends on scheduling.
    #[test]
    fn shutdown_waits_for_a_call_in_flight() {
        let scratch = ScratchDir::new("ffi-wait-for-inflight");
        let core = sb_test_support::block_on(open_core(scratch_config(&scratch))).expect("opens");

        // A stand-in for a call in flight: the read side every call holds.
        let in_flight = sb_test_support::block_on(core.service.read());

        let mut shutdown = pin!(core.shutdown());
        let mut cx = Context::from_waker(Waker::noop());
        assert!(
            shutdown.as_mut().poll(&mut cx).is_pending(),
            "shutdown returned while a call was in flight"
        );

        drop(in_flight);
        sb_test_support::block_on(shutdown);
        let err = sb_test_support::block_on(core.settings()).expect_err("the core is shut down");
        assert!(matches!(err, CoreError::Unavailable { .. }));
    }

    /// An unknown setting key maps to `InvalidInput` through the FFI
    /// error enum, as spec/07's error surface requires.
    #[test]
    fn unknown_key_is_invalid_input() {
        let scratch = ScratchDir::new("ffi-unknown-key");
        let core = sb_test_support::block_on(open_core(scratch_config(&scratch))).expect("opens");

        let err = sb_test_support::block_on(core.set_setting(
            "33333333-3333-4333-8333-333333333333".to_owned(),
            "no.such.key".to_owned(),
            "value".to_owned(),
        ))
        .expect_err("the registry doesn't hold the key");
        assert!(matches!(err, CoreError::InvalidInput { .. }));

        sb_test_support::block_on(core.shutdown());
    }
}
