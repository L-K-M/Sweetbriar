//! Physical input limits for a fresh retained writable SQLite connection.
//! These bounds authenticate nothing and grant no stage eligibility.

use std::io;
use std::sync::{
    Arc, Mutex,
    atomic::{AtomicBool, Ordering},
};

use sb_types::fs::{FileIdentity, ReadWriteFile};

use super::{stage_canonical::inspection_error, stage_copy::cancelled, stage_vfs::ConfinedSqlite};
use crate::StoreError;

const CATALOG_BYTES: u64 = 2 * 1024 * 1024;

struct Counter {
    remaining: u64,
    exhausted: bool,
    cancelled: bool,
    fault: Option<io::ErrorKind>,
    promoted: bool,
}

struct State {
    counter: Mutex<Counter>,
    cancel: Arc<AtomicBool>,
}

pub(super) struct InputBudget {
    state: Arc<State>,
    data_bytes: u64,
}

impl InputBudget {
    fn wrap(
        file: Arc<dyn ReadWriteFile>,
        verified_bytes: u64,
        cancel: Arc<AtomicBool>,
    ) -> Result<(Arc<dyn ReadWriteFile>, Self), StoreError> {
        if cancel.load(Ordering::Acquire) {
            return Err(cancelled());
        }
        let measured = file.len().map_err(|error| {
            StoreError::unavailable_io("measure retained writable image", error.kind())
        })?;
        if measured != verified_bytes {
            return Err(StoreError::corrupt(
                "retained writable image length changed",
            ));
        }
        let data_bytes = measured
            .checked_mul(64)
            .and_then(|bytes| bytes.checked_add(CATALOG_BYTES))
            .ok_or_else(|| StoreError::corrupt("writable image exceeds input allowance"))?;
        let state = Arc::new(State {
            counter: Mutex::new(Counter {
                remaining: CATALOG_BYTES,
                exhausted: false,
                cancelled: false,
                fault: None,
                promoted: false,
            }),
            cancel,
        });
        let wrapped = Arc::new(LimitedFile {
            file,
            state: Arc::clone(&state),
        });
        Ok((wrapped, Self { state, data_bytes }))
    }

    /// Call once, only after bounded catalog verification succeeds. A failed
    /// phase cannot be renewed; changing a cursor or file length grants nothing.
    pub(super) fn begin_data_phase(&mut self) -> Result<(), StoreError> {
        if let Some(error) = self.error() {
            return Err(error);
        }
        let mut counter = self
            .state
            .counter
            .lock()
            .map_err(|_| StoreError::unavailable("writable input counter poisoned"))?;
        if counter.promoted {
            return Err(StoreError::internal(
                "writable input phase already advanced",
            ));
        }
        // Recheck while holding the same lock used by reads. A concurrent
        // read cannot exhaust the old allowance between check and promotion.
        if self.state.cancel.load(Ordering::Acquire) {
            counter.cancelled = true;
        }
        if counter.cancelled || counter.exhausted || counter.fault.is_some() {
            drop(counter);
            return Err(self.error().unwrap_or_else(cancelled));
        }
        counter.remaining = self.data_bytes;
        counter.promoted = true;
        Ok(())
    }

    /// Preserve real callback causes independently of SQLite's result text.
    /// The qualifier must check this before accepting any phase or close.
    pub(super) fn error(&self) -> Option<StoreError> {
        let mut counter = match self.state.counter.lock() {
            Ok(counter) => counter,
            Err(_) => {
                return Some(StoreError::unavailable_io(
                    "writable input callback panicked",
                    io::ErrorKind::Other,
                ));
            }
        };
        if self.state.cancel.load(Ordering::Acquire) {
            counter.cancelled = true;
        }
        if counter.cancelled {
            return Some(cancelled());
        }
        if let Some(kind) = counter.fault {
            return Some(StoreError::unavailable_io(
                "retained writable image I/O",
                kind,
            ));
        }
        counter
            .exhausted
            .then(|| StoreError::corrupt("writable SQLite input limit exceeded"))
    }
}

struct LimitedFile {
    file: Arc<dyn ReadWriteFile>,
    state: Arc<State>,
}

impl LimitedFile {
    fn recorded<T>(&self, result: io::Result<T>) -> io::Result<T> {
        if let Err(error) = &result
            && error.kind() != io::ErrorKind::Interrupted
            && let Ok(mut counter) = self.state.counter.lock()
        {
            counter.fault.get_or_insert(error.kind());
        }
        result
    }
}

impl ReadWriteFile for LimitedFile {
    fn read_at(&self, bytes: &mut [u8], offset: u64) -> io::Result<usize> {
        if bytes.is_empty() {
            return Ok(0);
        }
        // SQLite already serializes its connection. Holding this lock through
        // the read also makes accounting exact for shared concurrent cursors.
        // A panic poisons the counter and the VFS contains it, failing closed.
        let mut counter = self
            .state
            .counter
            .lock()
            .map_err(|_| io::Error::other("writable input counter poisoned"))?;
        if self.state.cancel.load(Ordering::Acquire) {
            counter.cancelled = true;
        }
        if counter.cancelled {
            return Err(io::Error::other("writable input cancelled"));
        }
        if let Some(kind) = counter.fault {
            return Err(kind.into());
        }
        if counter.remaining == 0 || counter.exhausted {
            counter.exhausted = true;
            return Err(io::Error::other("writable input exhausted"));
        }
        let count = bytes
            .len()
            .min(usize::try_from(counter.remaining).unwrap_or(usize::MAX));
        match self.file.read_at(&mut bytes[..count], offset) {
            Ok(read) if read <= count => {
                counter.remaining -= read as u64;
                Ok(read)
            }
            Ok(_) => {
                counter.fault.get_or_insert(io::ErrorKind::InvalidData);
                Err(io::ErrorKind::InvalidData.into())
            }
            Err(error) => {
                if error.kind() != io::ErrorKind::Interrupted {
                    counter.fault.get_or_insert(error.kind());
                }
                Err(error)
            }
        }
    }

    // Keep rollback and flush possible after a read failure. These operations
    // retain their existing authority and record errors without renewing input.
    fn write_at(&self, bytes: &[u8], offset: u64) -> io::Result<usize> {
        self.recorded(self.file.write_at(bytes, offset))
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        self.recorded(self.file.set_len(len))
    }

    fn len(&self) -> io::Result<u64> {
        self.recorded(self.file.len())
    }

    fn sync_all(&self) -> io::Result<()> {
        self.recorded(self.file.sync_all())
    }

    fn identity(&self) -> io::Result<FileIdentity> {
        self.recorded(self.file.identity())
    }
}

/// Attach the input guard before even the writer's schema-free open. The
/// caller still installs SQL metadata/work limits before schema-loading SQL.
pub(super) fn open_bounded(
    main: Arc<dyn ReadWriteFile>,
    journal: Arc<dyn ReadWriteFile>,
    statement: Arc<dyn ReadWriteFile>,
    wal: Arc<dyn ReadWriteFile>,
    verified_bytes: u64,
    cancel: Arc<AtomicBool>,
) -> Result<(ConfinedSqlite, InputBudget), StoreError> {
    let (main, budget) = InputBudget::wrap(main, verified_bytes, cancel)?;
    let writer = ConfinedSqlite::open_wal(main, journal, statement, wal).map_err(|error| {
        budget
            .error()
            .unwrap_or_else(|| inspection_error(error, "open bounded staging writer"))
    })?;
    if let Some(error) = budget.error() {
        drop(writer);
        return Err(error);
    }
    Ok((writer, budget))
}

#[cfg(test)]
mod tests;
