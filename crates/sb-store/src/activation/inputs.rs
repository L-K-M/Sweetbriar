//! Runtime invalidation only: leases never certify persisted non-fit evidence.

use std::collections::BTreeMap;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use rusqlite::{Connection, functions::FunctionFlags, types::ValueRef};

use crate::{StoreError, schema::classify};

type Key = (i64, i64);
#[cfg(test)]
mod tests;
const MESSAGE: i64 = 0;
const ACQUISITION: i64 = 1;
const MAILBOX: i64 = 2;
const ROOT: i64 = 3;
const ACTIVATION_SCHEMA_VERSION: i64 = 11;

#[derive(Debug, Default)]
pub(super) struct State {
    failed: AtomicBool,
    dead: AtomicBool,
    slots: Mutex<BTreeMap<Key, (u64, u64)>>,
}

pub(crate) struct Owner(Arc<State>);

impl Owner {
    pub(crate) fn install(conn: &Connection) -> Result<Self, StoreError> {
        let state = Arc::new(State::default());
        // Old-writer migration fixtures must open without installing newer-schema triggers.
        let version: i64 = conn
            .pragma_query_value(None, "user_version", |row| row.get(0))
            .map_err(sql_error)?;
        if version < ACTIVATION_SCHEMA_VERSION {
            state.dead.store(true, Ordering::SeqCst);
            return Ok(Self(state));
        }
        let notify = state.clone();
        // TEMP triggers can call this honest, side-effecting function with trusted_schema off.
        conn.create_scalar_function(
            "sb_activation_input",
            3,
            FunctionFlags::SQLITE_UTF8,
            move |ctx| {
                if let (ValueRef::Integer(kind), ValueRef::Integer(old), ValueRef::Integer(new)) =
                    (ctx.get_raw(0), ctx.get_raw(1), ctx.get_raw(2))
                {
                    if let Ok(mut slots) = notify.slots.lock() {
                        for (index, id) in [old, new].into_iter().enumerate() {
                            if index == 1 && old == new {
                                continue;
                            }
                            if let Some((_, stamp)) = slots.get_mut(&(kind, id)) {
                                match stamp.checked_add(1) {
                                    Some(next) => *stamp = next,
                                    None => notify.failed.store(true, Ordering::SeqCst),
                                }
                            }
                        }
                    } else {
                        notify.failed.store(true, Ordering::SeqCst);
                    }
                } else {
                    notify.failed.store(true, Ordering::SeqCst);
                }
                // Observer failure must not reject an unrelated ordinary write.
                Ok(0)
            },
        )
        .map_err(sql_error)?;

        for (kind, key, tables) in [
            (
                MESSAGE,
                "message_id",
                &[
                    "occurrences",
                    "logical_messages",
                    "message_headers",
                    "d_list_rows",
                    "d_message_bindings",
                ][..],
            ),
            (
                ACQUISITION,
                "acquisition_id",
                &[
                    "acquisitions",
                    "d_message_summary",
                    "d_repair_assemblies",
                    "d_repair_pieces",
                ][..],
            ),
        ] {
            for table in tables {
                for (event, old, new) in [
                    ("INSERT", "0".into(), format!("NEW.{key}")),
                    ("UPDATE", format!("OLD.{key}"), format!("NEW.{key}")),
                    ("DELETE", format!("OLD.{key}"), "0".into()),
                ] {
                    conn.execute_batch(&format!("CREATE TEMP TRIGGER sb_activation_{table}_{event} AFTER {event} ON main.{table}
                        BEGIN SELECT sb_activation_input({kind},{old},{new}); END")).map_err(sql_error)?;
                }
            }
        }
        conn.execute_batch(&format!("CREATE TEMP TRIGGER sb_activation_mailboxes AFTER UPDATE ON main.mailboxes
            WHEN (OLD.role,OLD.kind) IS NOT (NEW.role,NEW.kind) BEGIN SELECT sb_activation_input({MAILBOX},0,0); END;
            CREATE TEMP TRIGGER sb_activation_root_insert AFTER INSERT ON main.store_meta
            WHEN NEW.key IN('mime_repair_target','store_uuid','incarnation') BEGIN SELECT sb_activation_input({ROOT},0,0); END;
            CREATE TEMP TRIGGER sb_activation_root_update AFTER UPDATE ON main.store_meta
            WHEN OLD.key IN('mime_repair_target','store_uuid','incarnation') OR NEW.key IN('mime_repair_target','store_uuid','incarnation')
            BEGIN SELECT sb_activation_input({ROOT},0,0); END;
            CREATE TEMP TRIGGER sb_activation_root_delete AFTER DELETE ON main.store_meta
            WHEN OLD.key IN('mime_repair_target','store_uuid','incarnation') BEGIN SELECT sb_activation_input({ROOT},0,0); END;"))
            .map_err(sql_error)?;
        Ok(Self(state))
    }

    pub(super) fn handle(&self) -> Arc<State> {
        self.0.clone()
    }
}

impl Drop for Owner {
    fn drop(&mut self) {
        self.0.dead.store(true, Ordering::SeqCst);
    }
}

#[derive(Debug)]
pub(super) struct Lease {
    state: Arc<State>,
    keys: [Key; 4],
    stamps: [u64; 4],
}

impl Lease {
    pub(super) fn new(
        state: Arc<State>,
        message: i64,
        acquisition: i64,
    ) -> Result<Self, StoreError> {
        let keys = [
            (MESSAGE, message),
            (ACQUISITION, acquisition),
            (MAILBOX, 0),
            (ROOT, 0),
        ];
        let mut stamps = [0; 4];
        {
            let mut slots = state
                .slots
                .lock()
                .map_err(|_| StoreError::internal("activation observer poisoned"))?;
            if keys
                .iter()
                .any(|key| slots.get(key).is_some_and(|entry| entry.0 == u64::MAX))
            {
                return Err(StoreError::internal("activation lease exhausted"));
            }
            for (key, stamp) in keys.iter().zip(&mut stamps) {
                let entry = slots.entry(*key).or_default();
                entry.0 += 1;
                *stamp = entry.1;
            }
        }
        let lease = Self {
            state,
            keys,
            stamps,
        };
        lease.healthy()?;
        Ok(lease)
    }

    pub(super) fn healthy(&self) -> Result<(), StoreError> {
        if self.state.failed.load(Ordering::SeqCst) {
            return Err(StoreError::internal("activation observer failed"));
        }
        Ok(())
    }

    pub(super) fn owns(&self, owner: &Arc<State>) -> bool {
        Arc::ptr_eq(&self.state, owner) && !self.state.dead.load(Ordering::SeqCst)
    }

    pub(super) fn matches(&self, owner: &Arc<State>) -> Result<bool, StoreError> {
        self.healthy()?;
        if !self.owns(owner) {
            return Ok(false);
        }
        let slots = self
            .state
            .slots
            .lock()
            .map_err(|_| StoreError::internal("activation observer poisoned"))?;
        Ok(self
            .keys
            .iter()
            .zip(self.stamps)
            .all(|(key, stamp)| slots.get(key).is_some_and(|entry| entry.1 == stamp)))
    }
}

impl Drop for Lease {
    fn drop(&mut self) {
        if let Ok(mut slots) = self.state.slots.lock() {
            for key in self.keys {
                if let Some(entry) = slots.get_mut(&key) {
                    entry.0 -= 1;
                    if entry.0 == 0 {
                        slots.remove(&key);
                    }
                }
            }
        }
    }
}

fn sql_error(error: rusqlite::Error) -> StoreError {
    classify(error, "activation input observer")
}

pub(super) fn same_owner(left: &Arc<State>, right: &Arc<State>) -> bool {
    Arc::ptr_eq(left, right) && !left.dead.load(Ordering::SeqCst)
}

pub(super) fn healthy(state: &State) -> Result<(), StoreError> {
    if state.failed.load(Ordering::SeqCst) {
        return Err(StoreError::internal("activation observer failed"));
    }
    Ok(())
}
