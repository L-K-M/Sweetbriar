//! Retained positioned I/O translated to SQLite result codes
//! (M1.8e3b2b2). The upcoming confined writable VFS calls these
//! helpers from its C callbacks: they take a retained
//! [`ReadWriteFile`] and caller-owned slices, never a pathname, and
//! contain injected panics before any later C boundary. This module
//! registers no VFS, takes no lock, opens no SQL connection, changes
//! no journal mode and grants no stage eligibility.

use std::ffi::c_int;
use std::io;
use std::panic::{self, AssertUnwindSafe};

use rusqlite::ffi;
use sb_types::fs::ReadWriteFile;

/// The checked end of `[offset, offset + len)` within SQLite's signed
/// offset range. A length that does not fit `i64`, or an end past
/// `i64::MAX`, is rejected; no other cap is applied.
fn end_offset(offset: i64, len: usize) -> Option<i64> {
    offset.checked_add(i64::try_from(len).ok()?)
}

/// `xRead`: exact positioned reads, retrying interruptions. The
/// buffer is zeroed first, so a short read's remainder, an error's
/// tail and even a panic's unwritten bytes are never stale. A short
/// read reports `SQLITE_IOERR_SHORT_READ`; other failures report
/// `SQLITE_IOERR_READ`. Panics map to `SQLITE_IOERR_READ`.
pub(super) fn read(file: &dyn ReadWriteFile, bytes: &mut [u8], offset: i64) -> c_int {
    panic::catch_unwind(AssertUnwindSafe(|| {
        bytes.fill(0);
        if offset < 0 || end_offset(offset, bytes.len()).is_none() {
            return ffi::SQLITE_IOERR_READ;
        }
        let mut done = 0;
        let mut at = offset;
        while done < bytes.len() {
            match file.read_at(&mut bytes[done..], at as u64) {
                Ok(0) => return ffi::SQLITE_IOERR_SHORT_READ,
                Ok(n) if n <= bytes.len() - done => {
                    done += n;
                    at += n as i64;
                }
                Ok(_) => return ffi::SQLITE_IOERR_READ,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(_) => return ffi::SQLITE_IOERR_READ,
            }
        }
        ffi::SQLITE_OK
    }))
    .unwrap_or(ffi::SQLITE_IOERR_READ)
}

/// `xWrite`: positioned writes, retrying interruptions and short
/// writes until the whole slice lands. A zero or oversized count is
/// `SQLITE_IOERR_WRITE`; a storage-full error kind is
/// `SQLITE_FULL`; other failures are `SQLITE_IOERR_WRITE`. Panics map
/// to `SQLITE_IOERR_WRITE`.
pub(super) fn write(file: &dyn ReadWriteFile, bytes: &[u8], offset: i64) -> c_int {
    panic::catch_unwind(AssertUnwindSafe(|| {
        if offset < 0 || end_offset(offset, bytes.len()).is_none() {
            return ffi::SQLITE_IOERR_WRITE;
        }
        let mut done = 0;
        let mut at = offset;
        while done < bytes.len() {
            match file.write_at(&bytes[done..], at as u64) {
                Ok(0) => return ffi::SQLITE_IOERR_WRITE,
                Ok(n) if n <= bytes.len() - done => {
                    done += n;
                    at += n as i64;
                }
                Ok(_) => return ffi::SQLITE_IOERR_WRITE,
                Err(error) if error.kind() == io::ErrorKind::Interrupted => continue,
                Err(error) if error.kind() == io::ErrorKind::StorageFull => {
                    return ffi::SQLITE_FULL;
                }
                Err(_) => return ffi::SQLITE_IOERR_WRITE,
            }
        }
        ffi::SQLITE_OK
    }))
    .unwrap_or(ffi::SQLITE_IOERR_WRITE)
}

/// `xTruncate`: a negative size is rejected; any retained failure is
/// `SQLITE_IOERR_TRUNCATE`, including storage full. Panics map to
/// `SQLITE_IOERR_TRUNCATE`.
pub(super) fn truncate(file: &dyn ReadWriteFile, size: i64) -> c_int {
    panic::catch_unwind(AssertUnwindSafe(|| {
        if size < 0 {
            return ffi::SQLITE_IOERR_TRUNCATE;
        }
        match file.set_len(size as u64) {
            Ok(()) => ffi::SQLITE_OK,
            Err(_) => ffi::SQLITE_IOERR_TRUNCATE,
        }
    }))
    .unwrap_or(ffi::SQLITE_IOERR_TRUNCATE)
}

/// `xSync`: always the retained `sync_all`, never a pathname or a
/// directory flush. A failure is `SQLITE_IOERR_FSYNC`; panics map to
/// the same code.
pub(super) fn sync(file: &dyn ReadWriteFile) -> c_int {
    panic::catch_unwind(AssertUnwindSafe(|| match file.sync_all() {
        Ok(()) => ffi::SQLITE_OK,
        Err(_) => ffi::SQLITE_IOERR_FSYNC,
    }))
    .unwrap_or(ffi::SQLITE_IOERR_FSYNC)
}

/// `xFileSize`: the retained length as SQLite's signed offset. A
/// length past `i64::MAX` or a retained failure is
/// `SQLITE_IOERR_FSTAT`; panics map to the same code.
pub(super) fn file_size(file: &dyn ReadWriteFile) -> Result<i64, c_int> {
    panic::catch_unwind(AssertUnwindSafe(|| {
        let len = file.len().map_err(|_| ffi::SQLITE_IOERR_FSTAT)?;
        i64::try_from(len).map_err(|_| ffi::SQLITE_IOERR_FSTAT)
    }))
    .unwrap_or(Err(ffi::SQLITE_IOERR_FSTAT))
}

#[cfg(test)]
mod tests;
