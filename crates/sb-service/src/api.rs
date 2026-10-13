//! `open_core`, `Core`, `CoreConfig` and the DTOs (spec/07 "Shape",
//! "Lifecycle"): the API surface the shells talk to. DTOs are plain
//! service-owned records — no engine or store type crosses into L1
//! (spec/01 rule 3), so this crate re-exports nothing and converts
//! field by field instead.

use std::path::PathBuf;

use sb_engine::Engine;

use crate::CoreError;

/// Where the core keeps what (spec/07 "Lifecycle"): the store lives
/// under `data_root` (spec/03), disposable renditions under
/// `cache_root`. The pilot's store uses only the data root; the cache
/// root is carried so later steps don't change the API's shape.
#[derive(Debug, Clone)]
pub struct CoreConfig {
    pub data_root: PathBuf,
    pub cache_root: PathBuf,
}

impl CoreConfig {
    /// The platform's default roots (spec/03 "On-disk layout"): macOS
    /// `~/Library/Application Support/Sweetbriar` and
    /// `~/Library/Caches/Sweetbriar`; Linux's XDG data and cache dirs.
    /// A host without a home directory fails rather than guessing.
    pub fn platform_default() -> Result<Self, CoreError> {
        let unavailable = |what, e: std::io::Error| CoreError::Unavailable {
            reason: format!("cannot determine the default {what} root: {e}"),
        };
        Ok(Self {
            data_root: sb_engine::default_data_root().map_err(|e| unavailable("data", e))?,
            cache_root: sb_engine::default_cache_root().map_err(|e| unavailable("cache", e))?,
        })
    }
}

/// Open the core (spec/07 "Lifecycle"): the engine starts its runtime
/// and opens the store at the config's data root. A failed open
/// authorizes nothing (spec/07); the caller shows the error.
pub async fn open_core(config: CoreConfig) -> Result<Core, CoreError> {
    Ok(Core {
        engine: Engine::open(config.data_root).await?,
    })
}

/// The record of one command that committed (spec/07 "Commands"): what
/// ran, what it answered, and when. A repeated idempotency key returns
/// this stored receipt and does nothing else.
#[derive(Debug, Clone, PartialEq, Eq)]
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
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setting {
    pub key: String,
    pub value: String,
    pub is_default: bool,
}

/// An open core (spec/07): the shell's one handle. Every method maps
/// the engine's result into service types; `shutdown` ends it.
pub struct Core {
    engine: Engine,
}

impl Core {
    /// `set_setting` (spec/07 "Commands"): one command under an
    /// idempotency key, returning its receipt. A repeated key returns
    /// the stored receipt; an unknown key or value is `InvalidInput`.
    pub async fn set_setting(
        &self,
        idempotency_key: &str,
        key: &str,
        value: &str,
    ) -> Result<Receipt, CoreError> {
        let receipt = self.engine.set_setting(idempotency_key, key, value).await?;
        Ok(Receipt {
            command: receipt.command().to_owned(),
            result_json: receipt.result_json().to_owned(),
            created_at_ms: receipt.created_at().as_millis(),
        })
    }

    /// `settings()` (spec/07 "Queries"): every registered key with its
    /// effective value, the default for one never set.
    pub async fn settings(&self) -> Result<Vec<Setting>, CoreError> {
        Ok(self
            .engine
            .settings()
            .await?
            .into_iter()
            .map(|setting| Setting {
                key: setting.key().to_owned(),
                value: setting.value().to_owned(),
                is_default: setting.is_default(),
            })
            .collect())
    }

    /// A fresh idempotency key for a shell without its own UUID
    /// source (spec/07 "Commands").
    pub fn new_idempotency_key(&self) -> String {
        self.engine.new_idempotency_key()
    }

    /// Shut the core down (spec/07 "Lifecycle"): close the store — the
    /// clean-shutdown marker, the writer, the lease — then end the
    /// runtime. The spec's signature carries no error; a close failure
    /// leaves the marker unrecorded, and the next open's `quick_check`
    /// is the safety net (spec/03 "Integrity").
    pub async fn shutdown(self) {
        let _ = self.engine.shutdown().await;
    }
}

impl std::fmt::Debug for Core {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("Core")
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use sb_engine::EngineError;

    /// The platform defaults are absolute paths — the store's open
    /// path requires that (spec/03) — and computing them creates
    /// nothing.
    #[test]
    fn platform_default_roots_are_absolute() {
        let config = CoreConfig::platform_default().expect("a home directory exists in tests");
        assert!(config.data_root.is_absolute());
        assert!(config.cache_root.is_absolute());
    }

    /// The mapping keeps every variant the engine names (spec/07):
    /// one assertion per variant, so a new engine failure without a
    /// mapping fails to compile instead of silently becoming
    /// `Internal`.
    #[test]
    fn engine_errors_map_variant_for_variant() {
        /// One engine failure and the check that its mapped value is
        /// the matching `CoreError` variant.
        type Case = (EngineError, fn(&CoreError));
        let cases: [Case; 8] = [
            (
                EngineError::NotFound {
                    what: "w".to_owned(),
                },
                |e| assert!(matches!(e, CoreError::NotFound { .. })),
            ),
            (EngineError::Locked, |e| {
                assert!(matches!(e, CoreError::Locked))
            }),
            (
                EngineError::Unavailable {
                    reason: "r".to_owned(),
                },
                |e| assert!(matches!(e, CoreError::Unavailable { .. })),
            ),
            (
                EngineError::Corrupt {
                    detail: "d".to_owned(),
                },
                |e| assert!(matches!(e, CoreError::Corrupt { .. })),
            ),
            (
                EngineError::NewerStore {
                    app_version: "9".to_owned(),
                },
                |e| assert!(matches!(e, CoreError::NewerStore { .. })),
            ),
            (
                EngineError::InvalidInput {
                    field: "key".to_owned(),
                    message: "m".to_owned(),
                },
                |e| assert!(matches!(e, CoreError::InvalidInput { .. })),
            ),
            (
                EngineError::Conflict {
                    detail: "c".to_owned(),
                },
                |e| assert!(matches!(e, CoreError::Conflict { .. })),
            ),
            (
                EngineError::Internal {
                    detail: "i".to_owned(),
                },
                |e| assert!(matches!(e, CoreError::Internal { .. })),
            ),
        ];
        for (engine_error, assert_maps) in cases {
            assert_maps(&CoreError::from(engine_error));
        }
    }
}
