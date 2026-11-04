//! Authentication and isolated inspection of a backup database image.
use super::*;
use rusqlite::OptionalExtension;
use sha2::{Digest, Sha256};
use std::io::{Read, Write};
use std::ops::ControlFlow;

/// Result of inspecting the database bytes, or an explicit cancellation.
#[derive(Debug)]
pub enum BackupImageInspection {
    /// The retained image's identity and schema version.
    Inspected {
        store_uuid: String,
        schema_version: i64,
    },
    /// The caller stopped inspection; no verification claim was made.
    Cancelled,
}

#[cfg(test)]
fn transaction_cut(point: &str) {
    if std::env::var("SB_VERIFIER_TX_CUT").as_deref() == Ok(point) {
        let _ = std::process::Command::new("/bin/kill")
            .args(["-9", &std::process::id().to_string()])
            .status();
        panic!("failed to cut verifier at {point}");
    }
}

fn io_error(e: io::Error) -> StoreError {
    StoreError::unavailable_io(format!("verifier scratch I/O failed: {e}"), e.kind())
}
fn paths(root: &Path, id: &str) -> (PathBuf, PathBuf) {
    (
        root.join(format!("verifier-{id}")),
        root.join(format!(".verifier-{id}.owner")),
    )
}
fn incarnation(conn: &Connection) -> Result<String, StoreError> {
    conn.query_row(
        "SELECT value FROM store_meta WHERE key='incarnation'",
        [],
        |r| r.get(0),
    )
    .map_err(|e| classify(e, "read verifier incarnation"))
}

impl Backups {
    fn recover_images(&self, path: &Path) -> Result<(), StoreError> {
        let root = self.fs.open_confined_dir(path).map_err(io_error)?;
        loop {
            let conn = open_connection(&self.state_db, Access::ReadOnly, READER_PRAGMAS)?;
            let current = incarnation(&conn)?;
            let pending: Option<(String, String)> = conn.query_row(
                "SELECT id, token FROM verifier_scratch WHERE incarnation=?1 ORDER BY id LIMIT 1", [&current],
                |r| Ok((r.get(0)?, r.get(1)?)),
            ).optional().map_err(|e| classify(e, "read verifier cleanup"))?;
            drop(conn);
            let Some((id, token)) = pending else {
                return Ok(());
            };
            scratch::cleanup(root.as_ref(), &id, &token, None).map_err(io_error)?;
            super::ledger::purge(&self.writer, &id)?;
            await_write(self.writer.submit(WritePriority::Index, move |tx| {
                tx.execute("DELETE FROM verifier_scratch WHERE id=?1", [&id])
                    .map_err(|e| classify(e, "finish verifier cleanup"))?;
                Ok(())
            }))?;
        }
    }

    pub(crate) fn inspect_image(
        &self,
        generation: &Path,
        expected: [u8; 32],
        length: u64,
        cancelled: &std::sync::atomic::AtomicBool,
        progress: impl FnMut(u64) -> ControlFlow<()>,
    ) -> Result<BackupImageInspection, StoreError> {
        self.inspect_image_with(generation, expected, length, cancelled, progress, |_| {
            Ok(ControlFlow::Continue(()))
        })
    }

    /// Run a caller's work on the authenticated private image, after bounded
    /// metadata inspection and before closing the connection or its scratch.
    /// Completion reports only metadata inspection, not generation verification.
    /// Callback errors retain their classification; partial effects belong to
    /// the caller and must be discarded on failure or cancellation.
    pub(crate) fn inspect_image_with(
        &self,
        generation: &Path,
        expected: [u8; 32],
        length: u64,
        cancelled: &std::sync::atomic::AtomicBool,
        mut progress: impl FnMut(u64) -> ControlFlow<()>,
        callback: impl FnOnce(&Connection) -> Result<ControlFlow<()>, StoreError>,
    ) -> Result<BackupImageInspection, StoreError> {
        if self.writer.is_writer_thread() {
            return Err(StoreError::internal("writer cannot inspect a backup"));
        }
        let _guard = self
            .image_lock
            .try_lock()
            .map_err(|_| StoreError::conflict("backup image verification is already running"))?;
        let root = self
            .state_db
            .parent()
            .expect("state database has parent")
            .join("recovery");
        // Cleanup from an earlier failed or cancelled call remains
        // durable and must be retried before creating another image.
        self.recover_images(&root)?;
        let id = Uuid::from_random_bytes(self.draw_id()?).to_string();
        let token = Uuid::from_random_bytes(self.draw_id()?).to_string();
        let (dir, marker) = paths(Path::new(""), &id);
        let root_dir = self.fs.open_confined_dir(&root).map_err(io_error)?;
        if self
            .fs
            .metadata(&root.join(&dir))
            .map_err(io_error)?
            .is_some()
            || self
                .fs
                .metadata(&root.join(&marker))
                .map_err(io_error)?
                .is_some()
        {
            return Err(StoreError::conflict("verifier scratch name already exists"));
        }
        let source = self.fs.open_confined_dir(generation).map_err(io_error)?;
        let (mut input, _) = source
            .open_regular(Path::new(SNAPSHOT_FILE))
            .map_err(io_error)?;
        let row_id = id.clone();
        let row_token = token.clone();
        await_write(self.writer.submit(WritePriority::Index, move |tx| {
            let inserted = tx.execute("INSERT INTO verifier_scratch SELECT ?1, ?2, value FROM store_meta WHERE key='incarnation'", params![row_id, row_token])
                .map_err(|e| classify(e, "record verifier scratch"))?;
            if inserted != 1 { return Err(StoreError::corrupt("verifier incarnation is absent")); }
            #[cfg(test)]
            transaction_cut("insert-before-commit");
            Ok(())
        }))?;
        #[cfg(test)]
        transaction_cut("insert-after-commit");
        let mut retained = None;
        let outcome = (|| {
            let scratch = scratch::prepare(root_dir.as_ref(), &id, &token).map_err(io_error)?;
            retained = Some(scratch);
            let scratch = retained.as_ref().expect("scratch was prepared");
            let mut output = scratch.create(Path::new(SNAPSHOT_FILE)).map_err(io_error)?;
            let mut hash = Sha256::new();
            let mut total = 0u64;
            let mut buf = [0u8; 65536];
            if cancelled.load(std::sync::atomic::Ordering::Relaxed) || progress(0).is_break() {
                return Ok(BackupImageInspection::Cancelled);
            }
            loop {
                if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                    return Ok(BackupImageInspection::Cancelled);
                }
                let remain = length.saturating_sub(total);
                let limit = usize::try_from(remain.saturating_add(1).min(buf.len() as u64))
                    .expect("bounded by buffer");
                let n = input.read(&mut buf[..limit]).map_err(io_error)?;
                if n == 0 {
                    break;
                }
                total = total
                    .checked_add(n as u64)
                    .ok_or_else(|| StoreError::corrupt("verifier length overflow"))?;
                if total > length {
                    return Err(StoreError::corrupt("backup database length mismatch"));
                }
                output.write_all(&buf[..n]).map_err(io_error)?;
                hash.update(&buf[..n]);
                if progress(total).is_break() {
                    return Ok(BackupImageInspection::Cancelled);
                }
            }
            if total != length || hash.finalize().as_slice() != expected {
                return Err(StoreError::corrupt("backup database digest mismatch"));
            }
            output.sync_all().map_err(io_error)?;
            drop(output);
            scratch.sync().map_err(io_error)?;
            let (mut pinned, _) = scratch
                .open_regular(Path::new(SNAPSHOT_FILE))
                .map_err(io_error)?;
            // The progress callback can run while the copy is open. Hash
            // the completed private image through the very handle SQLite
            // will open, so changes to copied bytes cannot inherit the
            // source stream's authentication.
            let mut copied_hash = Sha256::new();
            let mut copied_len = 0u64;
            loop {
                if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                    return Ok(BackupImageInspection::Cancelled);
                }
                let remain = length.saturating_sub(copied_len);
                let limit = usize::try_from(remain.saturating_add(1).min(buf.len() as u64))
                    .expect("bounded by buffer");
                let n = pinned.read(&mut buf[..limit]).map_err(io_error)?;
                if n == 0 {
                    break;
                }
                copied_len = copied_len
                    .checked_add(n as u64)
                    .ok_or_else(|| StoreError::corrupt("verifier length overflow"))?;
                if copied_len > length {
                    return Err(StoreError::corrupt("verifier image length mismatch"));
                }
                copied_hash.update(&buf[..n]);
            }
            if copied_len != length || copied_hash.finalize().as_slice() != expected {
                return Err(StoreError::corrupt("verifier image digest mismatch"));
            }
            if cancelled.load(std::sync::atomic::Ordering::Relaxed) {
                return Ok(BackupImageInspection::Cancelled);
            }
            let conn = crate::image_vfs::open(pinned)
                .map_err(|e| classify(e, "open authenticated backup image"))?;
            let inspected = image_inspect::inspect(&conn, cancelled).and_then(|inspected| {
                if cancelled.load(std::sync::atomic::Ordering::Acquire) {
                    return Ok(image_inspect::Inspection::Cancelled);
                }
                match inspected {
                    image_inspect::Inspection::Cancelled => Ok(inspected),
                    _ => match callback(&conn)? {
                        ControlFlow::Continue(()) => Ok(inspected),
                        ControlFlow::Break(()) => Ok(image_inspect::Inspection::Cancelled),
                    },
                }
            });
            conn.close()
                .map_err(|(_, e)| classify(e, "close backup image"))?;
            Ok(match inspected? {
                image_inspect::Inspection::Inspected {
                    store_uuid,
                    schema_version,
                } => BackupImageInspection::Inspected {
                    store_uuid,
                    schema_version,
                },
                image_inspect::Inspection::Cancelled => BackupImageInspection::Cancelled,
            })
        })();
        #[cfg(test)]
        transaction_cut("before-cleanup");
        scratch::cleanup(root_dir.as_ref(), &id, &token, retained.as_deref()).map_err(io_error)?;
        super::ledger::purge(&self.writer, &id)?;
        #[cfg(test)]
        transaction_cut("after-cleanup");
        await_write(self.writer.submit(WritePriority::Index, move |tx| {
            tx.execute("DELETE FROM verifier_scratch WHERE id=?1", [&id])
                .map_err(|e| classify(e, "finish verifier cleanup"))?;
            #[cfg(test)]
            transaction_cut("delete-before-commit");
            Ok(())
        }))?;
        #[cfg(test)]
        transaction_cut("delete-after-commit");
        if cancelled.load(std::sync::atomic::Ordering::Acquire) {
            return Ok(BackupImageInspection::Cancelled);
        }
        outcome
    }
}
