//! The exported surface (spec/07 "Shape", "Lifecycle", "Threading,
//! cancellation and FFI rules"): records mirroring `sb-service`'s
//! DTOs field by field, and the shared `Core` object. Every method
//! converts service types into this crate's own records, keeping the
//! boundary one-way — nothing from below crosses into Swift.

use std::path::PathBuf;
use std::sync::Arc;

use tokio::sync::RwLock;

use crate::CoreError;
use crate::cancel::{CancelToken, until_cancelled};

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

/// The platform's default roots as the shell spells them (spec/07
/// "Threading, cancellation and FFI rules": the one synchronous call
/// allowed to reach the environment). A root that isn't UTF-8 is
/// `Unavailable` naming it — paths don't cross FFI, so the shell
/// can't spell it otherwise.
#[uniffi::export]
pub fn platform_default_config() -> Result<CoreConfig, CoreError> {
    let config = sb_service::CoreConfig::platform_default()?;
    Ok(CoreConfig {
        data_root: utf8_root("data", config.data_root)?,
        cache_root: utf8_root("cache", config.cache_root)?,
    })
}

/// A root as the shell's string; a non-UTF-8 root is `Unavailable`
/// naming which root and what bytes it held.
fn utf8_root(which: &str, root: PathBuf) -> Result<String, CoreError> {
    root.into_os_string()
        .into_string()
        .map_err(|raw| CoreError::Unavailable {
            reason: format!(
                "the platform's {which} root is not UTF-8: {}",
                PathBuf::from(raw).display()
            ),
        })
}

/// Open the core (spec/07 "Lifecycle"): the shells' one entry point.
/// A failed open authorizes nothing; the caller shows the error. The
/// token is honoured only until the open starts: the engine's open
/// thread runs to the end and can't be interrupted midway (spec/07).
#[uniffi::export]
pub async fn open_core(
    token: Arc<CancelToken>,
    config: CoreConfig,
) -> Result<Arc<Core>, CoreError> {
    if token.is_cancelled() {
        return Err(CoreError::Cancelled);
    }
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
    /// Crate-visible so each domain module (`windows`) can implement
    /// its share of the exported `Core` methods.
    pub(crate) service: RwLock<Option<sb_service::Core>>,
}

/// What a call after `shutdown` always answers (spec/07: "any call
/// after it is `Unavailable`"). Shared with the error's constructor.
pub(crate) const SHUT_DOWN_MESSAGE: &str = "the core is shut down";

#[uniffi::export]
impl Core {
    /// `set_setting` (spec/07 "Commands"): one command under an
    /// idempotency key the shell made (`UUID()` in Swift). The read
    /// side of the shared lock is held for the call's duration, so a
    /// concurrent `shutdown` waits for it. A command honours the
    /// token only until it holds the core: once begun it runs to
    /// completion and returns its receipt (spec/07).
    pub async fn set_setting(
        &self,
        token: Arc<CancelToken>,
        idempotency_key: String,
        key: String,
        value: String,
    ) -> Result<Receipt, CoreError> {
        let receipt = self
            .command(&token, async |core| {
                core.set_setting(&idempotency_key, &key, &value).await
            })
            .await?;
        Ok(Receipt::from_service(&receipt))
    }

    /// `settings()` (spec/07 "Queries"): every registered key with its
    /// effective value, the default for one never set. A query stops
    /// wherever it waits — the core's read lock and the read itself
    /// both race the token (spec/07).
    pub async fn settings(&self, token: Arc<CancelToken>) -> Result<Vec<Setting>, CoreError> {
        let settings = self
            .query(&token, async |core| core.settings().await)
            .await?;
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

impl Core {
    /// Run a query (spec/07): the token races every wait — the core's
    /// read lock and the work itself — so a cancel stops it wherever
    /// it waits. The read side is held throughout, so `shutdown`
    /// waits for the query.
    pub(crate) async fn query<T>(
        &self,
        token: &CancelToken,
        work: impl AsyncFnOnce(&sb_service::Core) -> Result<T, sb_service::CoreError>,
    ) -> Result<T, CoreError> {
        let guard = until_cancelled(token, self.service.read()).await?;
        let Some(core) = guard.as_ref() else {
            return Err(CoreError::unavailable_shut_down());
        };
        until_cancelled(token, work(core))
            .await?
            .map_err(CoreError::from)
    }

    /// Run a command (spec/07): the token counts only until the
    /// command holds the core's read side. From then on the command
    /// runs to completion, so one that committed always returns its
    /// receipt.
    pub(crate) async fn command<T>(
        &self,
        token: &CancelToken,
        work: impl AsyncFnOnce(&sb_service::Core) -> Result<T, sb_service::CoreError>,
    ) -> Result<T, CoreError> {
        let guard = until_cancelled(token, self.service.read()).await?;
        let Some(core) = guard.as_ref() else {
            return Err(CoreError::unavailable_shut_down());
        };
        work(core).await.map_err(CoreError::from)
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
    use std::task::{Context, Poll, Waker};

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

    /// A fresh token, the way the adapter makes one per call.
    fn fresh_token() -> Arc<CancelToken> {
        CancelToken::new()
    }

    /// The `appearance.compose_cue` setting as `settings()` reports it.
    fn compose_cue(settings: &[Setting]) -> &Setting {
        settings
            .iter()
            .find(|setting| setting.key == "appearance.compose_cue")
            .expect("the registry key is always reported")
    }

    /// The pilot's end-to-end path over FFI: open, set a setting
    /// under a shell-made idempotency key, read it back, shut down.
    #[test]
    fn open_set_read_shutdown() {
        let scratch = ScratchDir::new("ffi-lifecycle");
        let core = sb_test_support::block_on(open_core(fresh_token(), scratch_config(&scratch)))
            .expect("a fresh store opens");

        let receipt = sb_test_support::block_on(core.set_setting(
            fresh_token(),
            "11111111-1111-4111-8111-111111111111".to_owned(),
            "appearance.compose_cue".to_owned(),
            "strong".to_owned(),
        ))
        .expect("a known key and value commit");
        assert_eq!(receipt.command, "set_setting");
        assert!(receipt.created_at_ms > 0);

        let settings =
            sb_test_support::block_on(core.settings(fresh_token())).expect("settings read back");
        let setting = compose_cue(&settings);
        assert_eq!(setting.value, "strong");
        assert!(!setting.is_default);

        sb_test_support::block_on(core.shutdown());
    }

    /// A call after `shutdown` is `Unavailable` naming the state, and
    /// a second `shutdown` is harmless (spec/07: the shared object).
    #[test]
    fn after_shutdown_calls_answer_unavailable_and_second_shutdown_is_harmless() {
        let scratch = ScratchDir::new("ffi-shutdown");
        let core = sb_test_support::block_on(open_core(fresh_token(), scratch_config(&scratch)))
            .expect("opens");

        sb_test_support::block_on(core.shutdown());

        let err = sb_test_support::block_on(core.set_setting(
            fresh_token(),
            "22222222-2222-4222-8222-222222222222".to_owned(),
            "appearance.compose_cue".to_owned(),
            "standard".to_owned(),
        ))
        .expect_err("the core is shut down");
        assert!(
            matches!(err, CoreError::Unavailable { ref reason } if reason == SHUT_DOWN_MESSAGE)
        );

        let err = sb_test_support::block_on(core.settings(fresh_token()))
            .expect_err("the core is shut down");
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
        let core = sb_test_support::block_on(open_core(fresh_token(), scratch_config(&scratch)))
            .expect("opens");

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
        let err = sb_test_support::block_on(core.settings(fresh_token()))
            .expect_err("the core is shut down");
        assert!(matches!(err, CoreError::Unavailable { .. }));
    }

    /// An unknown setting key maps to `InvalidInput` through the FFI
    /// error enum, as spec/07's error surface requires.
    #[test]
    fn unknown_key_is_invalid_input() {
        let scratch = ScratchDir::new("ffi-unknown-key");
        let core = sb_test_support::block_on(open_core(fresh_token(), scratch_config(&scratch)))
            .expect("opens");

        let err = sb_test_support::block_on(core.set_setting(
            fresh_token(),
            "33333333-3333-4333-8333-333333333333".to_owned(),
            "no.such.key".to_owned(),
            "value".to_owned(),
        ))
        .expect_err("the registry doesn't hold the key");
        assert!(matches!(err, CoreError::InvalidInput { .. }));

        sb_test_support::block_on(core.shutdown());
    }

    /// A token cancelled before a command answers `Cancelled` and the
    /// command never ran — `settings()` still reports the default
    /// (spec/07: a command honours the token until the core begins it).
    #[test]
    fn a_command_cancelled_before_it_begins_leaves_no_effect() {
        let scratch = ScratchDir::new("ffi-cancelled-before");
        let core = sb_test_support::block_on(open_core(fresh_token(), scratch_config(&scratch)))
            .expect("opens");

        let token = fresh_token();
        token.cancel();
        let err = sb_test_support::block_on(core.set_setting(
            token,
            "44444444-4444-4444-8444-444444444444".to_owned(),
            "appearance.compose_cue".to_owned(),
            "strong".to_owned(),
        ))
        .expect_err("the command was cancelled before it began");
        assert!(matches!(err, CoreError::Cancelled));

        let settings = sb_test_support::block_on(core.settings(fresh_token())).expect("settings");
        let setting = compose_cue(&settings);
        assert_eq!(setting.value, "standard");
        assert!(setting.is_default, "the cancelled command ran");

        sb_test_support::block_on(core.shutdown());
    }

    /// A query stops wherever it waits — the core's read lock
    /// included (spec/07): with the write side held, `settings` stays
    /// pending, and cancelling its token resolves it `Cancelled`.
    /// Polled by hand, so nothing depends on scheduling.
    #[test]
    fn a_query_waiting_in_the_core_answers_cancelled() {
        let scratch = ScratchDir::new("ffi-query-cancelled");
        let core = sb_test_support::block_on(open_core(fresh_token(), scratch_config(&scratch)))
            .expect("opens");

        // The write side held: the query can only wait on it.
        let writing = sb_test_support::block_on(core.service.write());

        let token = fresh_token();
        let mut query = pin!(core.settings(token.clone()));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(
            query.as_mut().poll(&mut cx).is_pending(),
            "the query didn't wait on the core's lock"
        );

        token.cancel();
        assert!(
            matches!(
                query.as_mut().poll(&mut cx),
                Poll::Ready(Err(CoreError::Cancelled))
            ),
            "a waiting query didn't answer Cancelled"
        );

        drop(writing);
        sb_test_support::block_on(core.shutdown());
    }

    /// A command the core has begun ignores the token and returns its
    /// receipt (spec/07). The first poll holds the read side and waits
    /// on the writer thread: submitting to an OS thread resolves
    /// asynchronously — the store's transaction can't have committed
    /// in the instructions between submit and this poll, so `Pending`
    /// is required, not hoped for. Cancel then drive to the end: the
    /// receipt still arrives and the value is set.
    #[test]
    fn a_begun_command_returns_its_receipt_despite_a_cancel() {
        let scratch = ScratchDir::new("ffi-command-begun");
        let core = sb_test_support::block_on(open_core(fresh_token(), scratch_config(&scratch)))
            .expect("opens");

        let token = fresh_token();
        let mut command = pin!(core.set_setting(
            token.clone(),
            "55555555-5555-4555-8555-555555555555".to_owned(),
            "appearance.compose_cue".to_owned(),
            "strong".to_owned(),
        ));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(
            command.as_mut().poll(&mut cx).is_pending(),
            "the command finished before its first poll"
        );

        token.cancel();
        let receipt = sb_test_support::block_on(command.as_mut())
            .expect("a begun command runs to completion");
        assert_eq!(receipt.command, "set_setting");

        let settings = sb_test_support::block_on(core.settings(fresh_token())).expect("settings");
        let setting = compose_cue(&settings);
        assert_eq!(setting.value, "strong");
        assert!(!setting.is_default, "the begun command didn't commit");

        sb_test_support::block_on(core.shutdown());
    }

    /// `open_core` honours the token until the open starts: an
    /// already-cancelled token answers `Cancelled` and opens nothing
    /// (spec/07 — the open thread, once started, can't be interrupted).
    #[test]
    fn open_core_with_a_cancelled_token_opens_nothing() {
        let scratch = ScratchDir::new("ffi-open-cancelled");
        let token = fresh_token();
        token.cancel();
        let result = sb_test_support::block_on(open_core(token, scratch_config(&scratch)));
        assert!(
            matches!(result, Err(CoreError::Cancelled)),
            "the open was cancelled before it started"
        );
        assert!(
            !scratch.path().join("store").exists(),
            "a cancelled open created the store"
        );
    }

    /// `platform_default_config` hands the shell absolute roots —
    /// the store's open path requires absolute paths (spec/03).
    #[test]
    fn platform_default_config_returns_absolute_roots() {
        let config = platform_default_config().expect("a home directory exists in tests");
        assert!(PathBuf::from(&config.data_root).is_absolute());
        assert!(PathBuf::from(&config.cache_root).is_absolute());
    }

    /// A gate for the helper tests: work parked on it stays pending
    /// until `open`, whatever polls it.
    fn gate() -> Arc<tokio::sync::Notify> {
        Arc::new(tokio::sync::Notify::new())
    }

    /// A query stops while its work waits, not only at the lock
    /// (spec/07): with the work parked on a gate, cancelling the token
    /// resolves the query `Cancelled` before the gate opens.
    #[test]
    fn a_query_whose_work_is_waiting_answers_cancelled() {
        let scratch = ScratchDir::new("ffi-query-work-cancelled");
        let core = sb_test_support::block_on(open_core(fresh_token(), scratch_config(&scratch)))
            .expect("opens");

        let token = fresh_token();
        let parked = gate();
        let mut query = pin!(core.query(&token, async |_core| {
            parked.notified().await;
            Ok(())
        }));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(
            query.as_mut().poll(&mut cx).is_pending(),
            "the query's work didn't wait on the gate"
        );

        token.cancel();
        assert!(
            matches!(
                query.as_mut().poll(&mut cx),
                Poll::Ready(Err(CoreError::Cancelled))
            ),
            "a query whose work was waiting ignored the cancel"
        );

        sb_test_support::block_on(core.shutdown());
    }

    /// A begun command ignores a cancel while its work waits (spec/07):
    /// with the work parked on a gate, a cancel leaves it pending, and
    /// opening the gate completes it with its result.
    #[test]
    fn a_command_whose_work_is_waiting_ignores_the_cancel() {
        let scratch = ScratchDir::new("ffi-command-work-cancelled");
        let core = sb_test_support::block_on(open_core(fresh_token(), scratch_config(&scratch)))
            .expect("opens");

        let token = fresh_token();
        let parked = gate();
        let mut command = pin!(core.command(&token, async |_core| {
            parked.notified().await;
            Ok(42)
        }));
        let mut cx = Context::from_waker(Waker::noop());
        assert!(
            command.as_mut().poll(&mut cx).is_pending(),
            "the command's work didn't wait on the gate"
        );

        token.cancel();
        assert!(
            command.as_mut().poll(&mut cx).is_pending(),
            "a begun command answered the cancel"
        );

        parked.notify_one();
        assert!(
            matches!(command.as_mut().poll(&mut cx), Poll::Ready(Ok(42))),
            "the begun command didn't complete after its work did"
        );

        sb_test_support::block_on(core.shutdown());
    }
}
