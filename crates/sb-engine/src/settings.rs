//! The settings domain (spec/02 "Commands, receipts and settings"):
//! the registry — each key with its values and its default — and the
//! two operations over it, `set_setting` (a command, under an
//! idempotency key, spec/07) and `settings()` (a query). Values are
//! strings; `value_json` is the JSON string literal.

use sb_resources::{Receipt, WritePriority, read_all, run_once, upsert};

use crate::{Engine, EngineError};

/// `set_setting`'s name in `command_receipts` (spec/07).
const SET_SETTING: &str = "set_setting";

/// One registered setting (spec/02): its key, its values and its
/// default.
struct RegisteredSetting {
    key: &'static str,
    values: &'static [&'static str],
    default: &'static str,
}

/// The pilot's settings registry (spec/02), which later steps extend.
const REGISTRY: &[RegisteredSetting] = &[RegisteredSetting {
    key: "appearance.compose_cue",
    values: &["standard", "strong"],
    default: "standard",
}];

/// The setting a key names, or `None` when the registry holds no such
/// key.
fn registered(key: &str) -> Option<&'static RegisteredSetting> {
    REGISTRY.iter().find(|setting| setting.key == key)
}

/// A value's canonical JSON (spec/02): the pilot's settings values are
/// strings, so the JSON string literal.
fn value_json(value: &str) -> String {
    format!("\"{value}\"")
}

/// One setting as the engine reports it (spec/02): the effective value
/// and whether it is the registry's default.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Setting {
    key: String,
    value: String,
    is_default: bool,
}

impl Setting {
    fn new(key: &str, value: &str, is_default: bool) -> Self {
        Self {
            key: key.to_owned(),
            value: value.to_owned(),
            is_default,
        }
    }

    /// The setting's dotted key, e.g. `appearance.compose_cue`.
    pub fn key(&self) -> &str {
        &self.key
    }

    /// The effective value: the stored one, or the registry's default
    /// when the setting was never set.
    pub fn value(&self) -> &str {
        &self.value
    }

    /// Whether the effective value is the default (the setting was
    /// never set, or was set back to the default).
    pub fn is_default(&self) -> bool {
        self.is_default
    }
}

impl Engine {
    /// `set_setting` (spec/02, spec/07 "Commands"): validate the key
    /// and value against the registry — an unknown one is
    /// `InvalidInput` — then one user-priority write whose closure
    /// runs the command under the idempotency key: a repeated key
    /// returns the stored receipt without running (a key spent on
    /// another command is `Conflict`), the first run upserts the row
    /// and answers canonical JSON naming the key and the row's new
    /// revision.
    pub async fn set_setting(
        &self,
        idempotency_key: &str,
        key: &str,
        value: &str,
    ) -> Result<Receipt, EngineError> {
        let setting = registered(key).ok_or_else(|| {
            EngineError::invalid_input("key", format!("unknown setting key '{key}'"))
        })?;
        if !setting.values.contains(&value) {
            return Err(EngineError::invalid_input(
                "value",
                format!(
                    "'{value}' is not a value of '{key}'; expected one of {}",
                    setting.values.join(", ")
                ),
            ));
        }
        let stored_json = value_json(value);
        let now = self.resources().now();
        let command_key = idempotency_key.to_owned();
        let setting_key = key.to_owned();
        self.resources()
            .write(WritePriority::User, move |tx| {
                run_once(tx, &command_key, SET_SETTING, now, |tx| {
                    let revision = upsert(tx, &setting_key, &stored_json, now)?;
                    Ok(format!(
                        "{{\"key\":\"{setting_key}\",\"revision\":{revision}}}"
                    ))
                })
            })
            .await
            .map_err(EngineError::from)
    }

    /// `settings()` (spec/02): every registered key with its effective
    /// value — the default for one never set — and whether it is the
    /// default. A stored `value_json` no registered value matches is
    /// `Corrupt`: never coerce it or silently drop the key. Rows for
    /// keys this build doesn't register are left alone and unreported;
    /// a newer app's store refuses to open here before this matters.
    pub async fn settings(&self) -> Result<Vec<Setting>, EngineError> {
        let rows = self
            .read_through(|resources| resources.read(read_all))
            .await?;
        settings_from(rows)
    }
}

/// Fold the stored rows over the registry: the reported settings, in
/// registry order, or the first `Corrupt` value.
fn settings_from(rows: Vec<sb_resources::StoredSetting>) -> Result<Vec<Setting>, EngineError> {
    REGISTRY
        .iter()
        .map(|setting| match rows.iter().find(|row| row.key() == setting.key) {
            None => Ok(Setting::new(setting.key, setting.default, true)),
            Some(row) => match setting
                .values
                .iter()
                .find(|value| value_json(value) == row.value_json())
            {
                // The stored literal equals the default exactly when
                // the setting holds the default value.
                Some(value) => Ok(Setting::new(setting.key, value, *value == setting.default)),
                None => Err(EngineError::corrupt(format!(
                    "setting '{}' holds {}, which none of its registered values ({}) serialize to",
                    setting.key,
                    row.value_json(),
                    setting.values.join(", ")
                ))),
            },
        })
        .collect()
}
