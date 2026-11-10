//! Verified copy and durable manifest publication (spec/03 steps 4–6).

use std::io::{self, Read, Write};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use serde::Serialize;
use sha2::{Digest, Sha256};

use super::*;
use crate::objects::read_error_as_store_error;

const CHUNK: usize = 64 * 1024;

#[derive(Serialize)]
struct Entry {
    sha256: String,
    bytes: u64,
}

#[derive(Serialize)]
struct Database {
    file: &'static str,
    sha256: String,
    bytes: u64,
}

#[derive(Serialize)]
struct Manifest {
    format: u32,
    store_uuid: String,
    generation_id: String,
    app_version: &'static str,
    schema_version: i64,
    created_at: i64,
    database: Database,
    objects: Vec<Entry>,
}

// Test-only process cuts bracket actual writer transaction boundaries.
#[cfg(test)]
fn tx_cut(point: &str) {
    if std::env::var("SB_BACKUP_TX_CUT").as_deref() == Ok(point) {
        let _ = std::process::Command::new("/bin/kill")
            .args(["-9", &std::process::id().to_string()])
            .status();
        panic!("process cut failed at {point}");
    }
}

fn io_error(e: io::Error) -> StoreError {
    StoreError::unavailable_io(format!("cannot publish backup generation: {e}"), e.kind())
}

fn hex(bytes: &[u8]) -> String {
    use std::fmt::Write as _;
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        write!(&mut output, "{byte:02x}").expect("writing to a String cannot fail");
    }
    output
}

/// Read and hash in bounded memory; when copying, hash precisely the
/// bytes written and refuse a source that does not match its name.
fn stream(mut input: impl Read, mut output: Option<&mut dyn Write>) -> Result<Entry, StoreError> {
    let mut hash = Sha256::new();
    let mut bytes = 0u64;
    let mut buffer = [0; CHUNK];
    loop {
        let n = input.read(&mut buffer).map_err(|e| {
            read_error_as_store_error(&e)
                .cloned()
                .unwrap_or_else(|| io_error(e))
        })?;
        if n == 0 {
            break;
        }
        if let Some(file) = output.as_mut() {
            file.write_all(&buffer[..n]).map_err(io_error)?;
        }
        hash.update(&buffer[..n]);
        bytes = bytes
            .checked_add(n as u64)
            .ok_or_else(|| StoreError::corrupt("backup file length overflow"))?;
    }
    Ok(Entry {
        sha256: hex(&hash.finalize()),
        bytes,
    })
}

/// Confirm the opaque snapshot is this store's active generation and
/// all its pins still exist before file I/O begins.
fn check_snapshot(
    tx: &rusqlite::Transaction<'_>,
    snapshot: &BackupSnapshot,
    after_first: impl FnOnce(),
) -> Result<(), StoreError> {
    check_generation(tx, snapshot)?;
    let mut stmt = tx.prepare("SELECT object_sha256 FROM object_pins WHERE pin_kind='backup_in_progress' AND pin_ref=?1 ORDER BY object_sha256")
        .map_err(|e| classify(e, "check backup pins"))?;
    let mut rows = stmt
        .query([&snapshot.generation_id])
        .map_err(|e| classify(e, "check backup pins"))?;
    let mut after_first = Some(after_first);
    for expected in snapshot.referenced.iter() {
        let actual: Option<String> = rows
            .next()
            .map_err(|e| classify(e, "read backup pins"))?
            .map(|row| row.get(0))
            .transpose()
            .map_err(|e| classify(e, "read backup pin"))?;
        if actual.as_ref() != Some(expected) {
            return Err(StoreError::corrupt("backup pins do not match the snapshot"));
        }
        if let Some(pause) = after_first.take() {
            pause();
        }
    }
    if rows
        .next()
        .map_err(|e| classify(e, "read backup pins"))?
        .is_some()
    {
        return Err(StoreError::corrupt("backup pins do not match the snapshot"));
    }
    Ok(())
}

fn check_generation(
    tx: &rusqlite::Transaction<'_>,
    snapshot: &BackupSnapshot,
) -> Result<(), StoreError> {
    let found: bool = tx.query_row(
        "SELECT EXISTS(SELECT 1 FROM backup_generations WHERE generation_id=?1 AND state='in_progress' AND location=?2)",
        params![snapshot.generation_id, snapshot.location.to_str()], |r| r.get(0),
    ).map_err(|e| classify(e, "check backup generation"))?;
    if !found {
        return Err(StoreError::conflict(
            "backup generation is not in progress in this store",
        ));
    }
    Ok(())
}

impl Backups {
    pub(crate) fn complete(
        &self,
        snapshot: BackupSnapshot,
        cancelled: &AtomicBool,
    ) -> Result<BackupReceipt, StoreError> {
        self.objects
            .latch()
            .refuse("complete a backup generation")?;
        let mut active = self
            .active
            .try_lock()
            .map_err(|_| StoreError::conflict("a backup operation is already running"))?;
        if !active
            .as_ref()
            .and_then(Weak::upgrade)
            .is_some_and(|token| Arc::ptr_eq(&token, &snapshot.active))
        {
            return Err(StoreError::conflict(
                "backup generation is not active in this store",
            ));
        }
        // A read transaction provides one consistent view without
        // occupying the single writer while the pin corpus is scanned.
        let mut conn = open_connection(&self.state_db, Access::ReadOnly, READER_PRAGMAS)?;
        let tx = conn
            .transaction()
            .map_err(|e| classify(e, "begin backup pin read"))?;
        check_snapshot(&tx, &snapshot, || {
            #[cfg(test)]
            {
                self.pause(|p| &p.during_pin_validation);
                self.pause(|p| &p.during_pin_validation);
            }
        })?;
        tx.commit()
            .map_err(|e| classify(e, "finish backup pin read"))?;
        drop(conn);

        // Read metadata from the same point-in-time snapshot, then close
        // SQLite before the final file flush and hash.
        let conn = snapshot_connection(&snapshot.state_db())?;
        let store_uuid: String = conn
            .query_row(
                "SELECT value FROM store_meta WHERE key='store_uuid'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| classify(e, "read backup store UUID"))?;
        let schema_version = crate::schema::read_user_version(&conn)?;
        let created_at: i64 = conn
            .query_row(
                "SELECT started_at FROM backup_generations WHERE generation_id=?1",
                [&snapshot.generation_id],
                |r| r.get(0),
            )
            .map_err(|e| classify(e, "read backup creation time"))?;
        // The changes this generation holds: a commit after the
        // snapshot keeps the store due for the next one (spec/03).
        let backed_up = if schema_version >= i64::from(crate::changes::SCHEMA_VERSION) {
            Some(crate::changes::read(
                &conn,
                crate::changes::CONTENT_CHANGES,
            )?)
        } else {
            None
        };
        conn.close()
            .map_err(|(_, e)| classify(e, "close backup snapshot"))?;
        snapshot
            .snapshot_handle
            .lock()
            .expect("snapshot handle lock poisoned")
            .sync_all()
            .map_err(io_error)?;
        let database = stream(
            self.fs
                .open_regular(&snapshot.state_db())
                .map_err(io_error)?
                .0,
            None,
        )?;
        let objects = self.copy_objects(&snapshot, cancelled)?;
        let manifest = Manifest {
            format: 1,
            store_uuid,
            generation_id: snapshot.generation_id.clone(),
            app_version: crate::schema::APP_VERSION,
            schema_version,
            created_at,
            database: Database {
                file: SNAPSHOT_FILE,
                sha256: database.sha256,
                bytes: database.bytes,
            },
            objects,
        };
        let total = manifest
            .objects
            .iter()
            .try_fold(manifest.database.bytes, |sum, entry| {
                sum.checked_add(entry.bytes)
            })
            .and_then(|sum| i64::try_from(sum).ok())
            .ok_or_else(|| StoreError::corrupt("backup total size overflow"))?;
        let count = i64::try_from(manifest.objects.len())
            .map_err(|_| StoreError::corrupt("backup object count overflow"))?;
        let data = serde_json::to_vec(&manifest)
            .map_err(|e| StoreError::internal(format!("cannot serialize backup manifest: {e}")))?;
        let manifest_sha = hex(&Sha256::digest(&data));
        {
            // The manifest is a step of its own: none on a latched
            // store.
            let _step = self
                .objects
                .latch()
                .step("write a backup generation's manifest")?;
            let mut file = self
                .fs
                .create(&snapshot.location.join("manifest.json"))
                .map_err(io_error)?;
            file.write_all(&data).map_err(io_error)?;
            file.sync_all().map_err(io_error)?;
            self.fs.sync_dir(&snapshot.location).map_err(io_error)?;
            self.fs
                .sync_dir(snapshot.location.parent().expect("generation has parent"))
                .map_err(io_error)?;
        }

        #[cfg(test)]
        {
            self.pause(|p| &p.before_pin_release);
            self.pause(|p| &p.before_pin_release);
        }
        // The in-progress row gates the entire GC pass while pin removal
        // yields between bounded transactions. A crash leaves it gated.
        self.release_pins(&snapshot)?;
        let completed_at = self.clock.now().as_millis();
        let receipt = BackupReceipt {
            generation_id: snapshot.generation_id.clone(),
            location: snapshot.location.clone(),
            manifest_sha256: manifest_sha.clone(),
        };
        let check = snapshot.clone();
        await_write(self.writer.upkeep(WritePriority::Index, move |tx| {
            check_generation(tx, &check)?;
            let remaining: bool = tx.query_row("SELECT EXISTS(SELECT 1 FROM object_pins WHERE pin_kind='backup_in_progress' AND pin_ref=?1)", [&check.generation_id], |r| r.get(0))
                .map_err(|e| classify(e, "check remaining backup pins"))?;
            if remaining { return Err(StoreError::corrupt("backup pins remain during completion")); }
            tx.execute("UPDATE backup_generations SET state='complete', completed_at=?2, manifest_sha256=?3, object_count=?4, total_bytes=?5 WHERE generation_id=?1", params![check.generation_id, completed_at, manifest_sha, count, total])
                .map_err(|e| classify(e, "complete backup generation"))?;
            if let Some(mark) = backed_up {
                crate::changes::record_backed_up(tx, mark)?;
            }
            #[cfg(test)]
            tx_cut("before-completion-commit");
            Ok(())
        }))?;
        snapshot.completed.store(true, Ordering::Release);
        #[cfg(test)]
        tx_cut("after-completion-commit");
        *active = None;
        self.retain_protecting(Some(&snapshot.generation_id))?;
        Ok(receipt)
    }

    pub(super) fn release_pins(&self, snapshot: &BackupSnapshot) -> Result<(), StoreError> {
        for (batch, chunk) in snapshot.referenced.chunks(PIN_BATCH).enumerate() {
            #[cfg(test)]
            tx_cut(&format!("before-batch-{batch}"));
            let chunk = chunk.to_vec();
            let check = snapshot.clone();
            await_write(self.writer.upkeep(WritePriority::Index, move |tx| {
                check_generation(tx, &check)?;
                for (offset, sha) in chunk.into_iter().enumerate() {
                    let removed = tx.execute("DELETE FROM object_pins WHERE pin_kind=?1 AND pin_ref=?2 AND object_sha256=?3", params![PIN_KIND, check.generation_id, sha])
                        .map_err(|e| classify(e, "release backup pin"))?;
                    if removed != 1 {
                        return Err(StoreError::corrupt("backup pin disappeared during release"));
                    }
                    #[cfg(test)]
                    if offset == 0 { tx_cut(&format!("inside-batch-{batch}")); }
                    #[cfg(not(test))]
                    let _ = offset;
                }
                Ok(())
            }))?;
            #[cfg(test)]
            tx_cut(&format!("after-batch-{batch}"));
            #[cfg(not(test))]
            let _ = batch;
        }
        Ok(())
    }

    fn copy_objects(
        &self,
        snapshot: &BackupSnapshot,
        cancelled: &AtomicBool,
    ) -> Result<Vec<Entry>, StoreError> {
        let root = snapshot.location.join("objects");
        let sha_root = root.join("sha256");
        self.fs.create_dir(&root).map_err(io_error)?;
        self.fs.create_dir(&sha_root).map_err(io_error)?;
        let mut entries = Vec::with_capacity(snapshot.referenced.len());
        // Clone each object where the filesystem can (spec/03 step 4:
        // APFS `clonefile`), so a generation costs no second copy of
        // the object store; the first refusal of its kind means none
        // will clone, and the rest are copied.
        let mut cloning = true;
        for sha in snapshot.referenced.iter() {
            // Stop at the next object: the generation stays
            // `in_progress` for the next run's recovery (spec/03).
            if cancelled.load(Ordering::Acquire) {
                return Err(StoreError::unavailable("the backup was cancelled"));
            }
            // One object is one step of file work (spec/03
            // "Integrity"): a latch waits for the object being copied
            // and the copy stops here, its generation `in_progress`.
            let _step = self
                .objects
                .latch()
                .step("copy an object into a backup generation")?;
            #[cfg(test)]
            if entries.is_empty() {
                self.pause(|p| &p.first_object_step);
                self.pause(|p| &p.first_object_step);
            }
            let ab = sha_root.join(&sha[..2]);
            let cd = ab.join(&sha[2..4]);
            create_or_existing_dir(self.fs.as_ref(), &ab)?;
            create_or_existing_dir(self.fs.as_ref(), &cd)?;
            if cloning {
                match self.clone_object(sha, &cd.join(sha))? {
                    Cloned::Verified(entry) => {
                        self.fs.sync_dir(&cd).map_err(io_error)?;
                        self.fs.sync_dir(&ab).map_err(io_error)?;
                        entries.push(entry);
                        continue;
                    }
                    Cloned::Unsupported => cloning = false,
                    Cloned::Failed => {}
                }
            }
            // The object store validates the hash and provides its
            // ordinary verifying, quarantining reader.
            let input = self.objects.open_backup_reference(sha)?;
            let mut output = self.fs.create(&cd.join(sha)).map_err(io_error)?;
            let entry = stream(input, Some(output.as_mut()))?;
            if entry.sha256 != *sha {
                return Err(StoreError::corrupt(format!(
                    "backup source object {sha} failed hash verification"
                )));
            }
            output.sync_all().map_err(io_error)?;
            self.fs.sync_dir(&cd).map_err(io_error)?;
            self.fs.sync_dir(&ab).map_err(io_error)?;
            entries.push(entry);
        }
        self.fs.sync_dir(&sha_root).map_err(io_error)?;
        self.fs.sync_dir(&root).map_err(io_error)?;
        self.fs.sync_dir(&snapshot.location).map_err(io_error)?;
        Ok(entries)
    }

    /// Clone one object into the generation, flushed, and hash what the
    /// generation now holds, so the manifest names those exact bytes.
    fn clone_object(&self, sha: &str, target: &Path) -> Result<Cloned, StoreError> {
        let source = self
            .objects
            .object_path(&crate::objects::ObjectId::from_hex(sha, 0)?);
        match self.fs.clone_file(&source, target) {
            Ok(()) => {}
            Err(e)
                if matches!(
                    e.kind(),
                    io::ErrorKind::Unsupported | io::ErrorKind::CrossesDevices
                ) =>
            {
                return Ok(Cloned::Unsupported);
            }
            // A missing or unreadable source: the copy's verifying
            // reader reports it as for any object. A clone can also fail
            // after creating `target` (its flush is part of the clone):
            // the copy's exclusive create needs the name free.
            Err(_) => {
                match self.fs.remove(target) {
                    Ok(()) => {}
                    Err(e) if e.kind() == io::ErrorKind::NotFound => {}
                    Err(e) => return Err(io_error(e)),
                }
                return Ok(Cloned::Failed);
            }
        }
        let (file, _) = self.fs.open_regular(target).map_err(io_error)?;
        let entry = stream(file, None)?;
        if entry.sha256 != sha {
            // The clone shares the source's bytes: the source is
            // damaged, and its verifying reader quarantines it.
            let _ = self
                .objects
                .open_backup_reference(sha)
                .and_then(|source| stream(source, None));
            return Err(StoreError::corrupt(format!(
                "backup source object {sha} failed hash verification"
            )));
        }
        Ok(Cloned::Verified(entry))
    }
}

/// What cloning one object did.
enum Cloned {
    /// Cloned, flushed and hashed to its name.
    Verified(Entry),
    /// This filesystem, or this pair of locations, can't clone: copy
    /// this object and the rest.
    Unsupported,
    /// The clone failed otherwise: copy this object.
    Failed,
}

fn create_or_existing_dir(fs: &dyn Fs, path: &Path) -> Result<(), StoreError> {
    match fs.create_dir(path) {
        Ok(()) => Ok(()),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {
            match fs.metadata(path).map_err(io_error)? {
                Some(md) if md.kind() == FileKind::Directory => Ok(()),
                _ => Err(io_error(e)),
            }
        }
        Err(e) => Err(io_error(e)),
    }
}
