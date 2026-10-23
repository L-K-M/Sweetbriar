//! One source revision per committed ordinary job, without persistent row fanout.

use crate::{StoreError, schema::classify};

pub(crate) const SCHEMA_VERSION: u32 = 12;
use rusqlite::{Connection, Transaction, functions::FunctionFlags};
use std::sync::{
    Arc,
    atomic::{AtomicBool, Ordering},
};

#[derive(Clone, Copy)]
pub(crate) enum Scope {
    Observe,
    Activation,
}

#[derive(Default)]
pub(crate) struct Tracking {
    dirty: AtomicBool,
    suppressed: AtomicBool,
}

impl Tracking {
    pub(crate) fn install(conn: &Connection) -> Result<Arc<Self>, StoreError> {
        let tracking = Arc::new(Self::default());
        let changed = tracking.clone();
        conn.create_scalar_function(
            "sb_corpus_changed",
            0,
            FunctionFlags::SQLITE_UTF8,
            move |_| {
                if !changed.suppressed.load(Ordering::SeqCst) {
                    changed.dirty.store(true, Ordering::SeqCst);
                }
                Ok(0)
            },
        )
        .map_err(sql_error)?;
        let dirty = tracking.clone();
        conn.create_scalar_function(
            "sb_corpus_dirty",
            0,
            FunctionFlags::SQLITE_UTF8,
            move |_| Ok(dirty.dirty.swap(false, Ordering::SeqCst)),
        )
        .map_err(sql_error)?;
        let version: u32 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(sql_error)?;
        if version < SCHEMA_VERSION {
            return Ok(tracking);
        }

        for table in [
            "logical_messages",
            "occurrences",
            "message_headers",
            "d_message_summary",
            "acquisitions",
            "d_list_rows",
            "d_message_bindings",
        ] {
            let columns = conn
                .prepare(&format!("PRAGMA table_info({table})"))
                .map_err(sql_error)?
                .query_map([], |row| row.get::<_, String>(1))
                .map_err(sql_error)?
                .collect::<rusqlite::Result<Vec<_>>>()
                .map_err(sql_error)?;
            let tuple = |prefix: &str| {
                columns
                    .iter()
                    .map(|name| format!("{prefix}.\"{}\"", name.replace('"', "\"\"")))
                    .collect::<Vec<_>>()
                    .join(",")
            };
            for event in ["INSERT", "DELETE", "UPDATE"] {
                let guard = if event == "UPDATE" {
                    format!("WHEN ({}) IS NOT ({})", tuple("OLD"), tuple("NEW"))
                } else {
                    String::new()
                };
                conn.execute_batch(&format!("CREATE TEMP TRIGGER sb_corpus_{table}_{event} AFTER {event} ON main.{table} {guard} BEGIN SELECT sb_corpus_changed(); END;")).map_err(sql_error)?;
            }
        }
        conn.execute_batch("CREATE TEMP TRIGGER sb_corpus_mailbox AFTER UPDATE ON main.mailboxes WHEN (OLD.kind,OLD.role) IS NOT (NEW.kind,NEW.role) BEGIN SELECT sb_corpus_changed(); END;
            CREATE TEMP TRIGGER sb_corpus_publication_insert AFTER INSERT ON main.d_repair_assemblies WHEN NEW.completed_at IS NOT NULL BEGIN SELECT sb_corpus_changed(); END;
            CREATE TEMP TRIGGER sb_corpus_publication AFTER UPDATE ON main.d_repair_assemblies WHEN OLD.completed_at IS NULL AND NEW.completed_at IS NOT NULL BEGIN SELECT sb_corpus_changed(); END;
            CREATE TEMP TRIGGER sb_corpus_root_insert AFTER INSERT ON main.store_meta WHEN NEW.key IN('mime_repair_target','store_uuid','incarnation') BEGIN SELECT sb_corpus_changed(); END;
            CREATE TEMP TRIGGER sb_corpus_root_delete AFTER DELETE ON main.store_meta WHEN OLD.key IN('mime_repair_target','store_uuid','incarnation') BEGIN SELECT sb_corpus_changed(); END;
            CREATE TEMP TRIGGER sb_corpus_root_update AFTER UPDATE ON main.store_meta WHEN (OLD.key,OLD.value) IS NOT (NEW.key,NEW.value) AND (OLD.key IN('mime_repair_target','store_uuid','incarnation') OR NEW.key IN('mime_repair_target','store_uuid','incarnation')) BEGIN SELECT sb_corpus_changed(); END;")
            .map_err(sql_error)?;
        Ok(tracking)
    }

    pub(crate) fn begin(&self, scope: Scope) {
        self.dirty.store(false, Ordering::SeqCst);
        self.suppressed
            .store(matches!(scope, Scope::Activation), Ordering::SeqCst);
    }
}

pub(crate) fn counter(conn: &Connection, key: &str) -> Result<u64, StoreError> {
    let text: String = conn
        .query_row("SELECT value FROM store_meta WHERE key=?1", [key], |row| {
            row.get(0)
        })
        .map_err(sql_error)?;
    text.parse::<u64>()
        .ok()
        .filter(|value| *value <= i64::MAX as u64)
        .ok_or_else(|| StoreError::corrupt("invalid repair catch-up counter"))
}

pub(crate) fn set(tx: &Transaction, key: &str, value: u64) -> Result<(), StoreError> {
    if tx
        .execute(
            "UPDATE store_meta SET value=?2 WHERE key=?1 AND value IS NOT ?2",
            (key, value.to_string()),
        )
        .map_err(sql_error)?
        > 1
    {
        return Err(StoreError::corrupt("duplicate repair catch-up counter"));
    }
    Ok(())
}

pub(crate) fn flush(tx: &Transaction) -> Result<(), StoreError> {
    let dirty: bool = tx
        .query_row("SELECT sb_corpus_dirty()", [], |row| row.get(0))
        .map_err(sql_error)?;
    if !dirty {
        return Ok(());
    }
    let next = counter(tx, "repair_inputs")?
        .checked_add(1)
        .filter(|n| *n <= i64::MAX as u64)
        .ok_or_else(|| StoreError::corrupt("repair input revision exhausted"))?;
    set(tx, "repair_inputs", next)
}

fn sql_error(error: rusqlite::Error) -> StoreError {
    classify(error, "repair input tracking")
}
