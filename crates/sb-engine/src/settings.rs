//! The settings domain (spec/02 "Commands, receipts and settings"):
//! the registry — each key with its values and its default — and the
//! two operations over it, `set_setting` (a command, under an
//! idempotency key, spec/07) and `settings()` (a query). Values are
//! strings; `value_json` is the JSON string literal.

use sb_resources::{
    BACKUP_DESTINATION_SETTING, Receipt, WritePriority, read_all, run_once, upsert,
    valid_backup_destination,
};

use crate::engine::validate_idempotency_key;
use crate::{Engine, EngineError};

/// `set_setting`'s name in `command_receipts` (spec/07).
const SET_SETTING: &str = "set_setting";

#[cfg(test)]
mod tests;

/// One registered setting (spec/02): its key, its values and its
/// default.
struct RegisteredSetting {
    key: &'static str,
    values: Values,
    default: &'static str,
}

enum Values {
    Choices(&'static [&'static str]),
    BackupDirectory,
    SenderList,
    MailboxList,
    IdentityDefault,
}

impl Values {
    fn accepts(&self, value: &str) -> bool {
        match self {
            Self::Choices(values) => values.contains(&value),
            Self::BackupDirectory => valid_backup_destination(value),
            Self::SenderList => valid_sender_list(value),
            Self::MailboxList => valid_mailbox_list(value),
            Self::IdentityDefault => valid_identity_default(value),
        }
    }

    fn expected(&self) -> String {
        match self {
            Self::Choices(values) => format!("one of {}", values.join(", ")),
            Self::BackupDirectory => {
                "an absolute directory path without NUL, or empty for the default".into()
            }
            Self::SenderList => {
                "lowercase addresses, each with one '@' and at most 254 bytes, sorted and unique, joined by newlines; at most 1,000, or empty".into()
            }
            Self::MailboxList => {
                "mailbox ids as canonical positive decimals, unique, joined by newlines; at most 100, or empty".into()
            }
            Self::IdentityDefault => {
                "an identity id of 1 to 128 UTF-8 bytes without control characters, or empty".into()
            }
        }
    }
}

/// The most mailboxes `filing.recent_mailboxes` ranks (spec/02).
const MAX_RECENT_MAILBOXES: usize = 100;

/// `filing.recent_mailboxes`'s canonical form (spec/02): empty, or
/// mailbox ids as canonical positive decimals, unique, joined by
/// newlines, at most [`MAX_RECENT_MAILBOXES`]. Their order is the
/// ranking, so it isn't sorted.
fn valid_mailbox_list(value: &str) -> bool {
    if value.is_empty() {
        return true;
    }
    let ids: Vec<&str> = value.split('\n').collect();
    if ids.len() > MAX_RECENT_MAILBOXES {
        return false;
    }
    ids.iter().enumerate().all(|(index, id)| {
        id.parse::<i64>()
            .is_ok_and(|parsed| parsed > 0 && parsed.to_string() == *id)
            && !ids[..index].contains(id)
    })
}

/// `reader.reading_view_senders`'s canonical form (spec/02): empty, or
/// lowercase addresses, each with one `@` and at most 254 bytes, sorted
/// and unique, joined by newlines; at most 1,000. Any other spelling is
/// refused, so one set of senders has one stored value.
fn valid_sender_list(value: &str) -> bool {
    if value.is_empty() {
        return true;
    }
    let addresses: Vec<&str> = value.split('\n').collect();
    if addresses.len() > 1_000 {
        return false;
    }
    addresses.iter().enumerate().all(|(index, address)| {
        address.len() <= 254
            && address.matches('@').count() == 1
            && address.to_lowercase() == *address
            && (index == 0 || addresses[index - 1] < *address)
    })
}

/// The longest `identity.default` id in UTF-8 bytes (spec/02).
const MAX_IDENTITY_ID_BYTES: usize = 128;

/// `identity.default` (spec/02): empty, or an id of 1 to
/// [`MAX_IDENTITY_ID_BYTES`] UTF-8 bytes with no control characters.
fn valid_identity_default(value: &str) -> bool {
    value.is_empty()
        || (value.len() <= MAX_IDENTITY_ID_BYTES && !value.chars().any(char::is_control))
}

/// `sending.undo_seconds`'s dotted key (spec/02), shared with the
/// send command that reads its effective value.
pub(crate) const UNDO_SECONDS_KEY: &str = "sending.undo_seconds";

/// The pilot's settings registry (spec/02), which later steps extend.
const REGISTRY: &[RegisteredSetting] = &[
    RegisteredSetting {
        key: "appearance.compose_cue",
        values: Values::Choices(&["wash", "standard", "strong", "minimal"]),
        // Color Wash since the owner's 2026-11-24 decision (spec/02);
        // a stored choice, `standard` included, is kept.
        default: "wash",
    },
    RegisteredSetting {
        key: UNDO_SECONDS_KEY,
        values: Values::Choices(&["5", "7", "10", "20", "30"]),
        default: "7",
    },
    RegisteredSetting {
        key: "notifications.previews",
        values: Values::Choices(&["off", "on"]),
        default: "off",
    },
    RegisteredSetting {
        key: BACKUP_DESTINATION_SETTING,
        values: Values::BackupDirectory,
        default: "",
    },
    RegisteredSetting {
        key: "reader.mark_read",
        values: Values::Choices(&["immediately", "2", "5", "10", "manual"]),
        default: "immediately",
    },
    RegisteredSetting {
        key: "reader.reading_view_senders",
        values: Values::SenderList,
        default: "",
    },
    RegisteredSetting {
        key: "filing.recent_mailboxes",
        values: Values::MailboxList,
        default: "",
    },
    RegisteredSetting {
        key: "identity.default",
        values: Values::IdentityDefault,
        default: "",
    },
];

/// The setting a key names, or `None` when the registry holds no such
/// key.
fn registered(key: &str) -> Option<&'static RegisteredSetting> {
    REGISTRY.iter().find(|setting| setting.key == key)
}

/// A value's canonical JSON (spec/02): the pilot's settings values are
/// strings, so the JSON string literal.
fn value_json(value: &str) -> String {
    serde_json::Value::String(value.to_owned()).to_string()
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
        validate_idempotency_key(idempotency_key)?;
        let setting = registered(key).ok_or_else(|| {
            EngineError::invalid_input("key", format!("unknown setting key '{key}'"))
        })?;
        if !setting.values.accepts(value) {
            return Err(EngineError::invalid_input(
                "value",
                format!(
                    "'{value}' is not a value of '{key}'; expected {}",
                    setting.values.expected()
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
        .map(
            |setting| match rows.iter().find(|row| row.key() == setting.key) {
                None => Ok(Setting::new(setting.key, setting.default, true)),
                Some(row) => {
                    let corrupt = || {
                        EngineError::corrupt(format!(
                            "setting '{}' is not canonical JSON for {}",
                            setting.key,
                            setting.values.expected()
                        ))
                    };
                    let value: String =
                        serde_json::from_str(row.value_json()).map_err(|_| corrupt())?;
                    if !setting.values.accepts(&value) || value_json(&value) != row.value_json() {
                        return Err(corrupt());
                    }
                    Ok(Setting::new(setting.key, &value, value == setting.default))
                }
            },
        )
        .collect()
}
