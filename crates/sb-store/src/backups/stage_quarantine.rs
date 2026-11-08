//! Isolated quarantine of a retained private image. The returned observation
//! certifies no source/live namespace binding or eligibility for a swap.

use std::io;
use std::panic::{AssertUnwindSafe, catch_unwind};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::{Connection, OptionalExtension, limits::Limit, params};
use sha2::{Digest, Sha256};

use super::{
    image_inspect::{MAX_PROGRESS_CALLS, guarded},
    quarantine::{self, Quarantine},
    stage_canonical::{self, inspection_error},
    stage_copy::{cancelled, io_error},
    stage_files::StageFiles,
    stage_input, stage_integrity, stage_schema,
    stage_vfs::CloseError,
};
use crate::{Seams, StoreError};
use sb_types::fs::{DirectoryIdentity, FileIdentity};

#[derive(Debug)]
pub(super) struct QuarantinedImage {
    pub(super) root_identity: DirectoryIdentity,
    pub(super) database_identity: FileIdentity,
    pub(super) sha256: [u8; 32],
    pub(super) bytes: u64,
    pub(super) schema_version: i64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum ImagePhase {
    BytesVerified,
    ReadOnlyInspected,
    WriterInspected,
    Quarantined,
    Checkpointed,
    Closed,
    Flushed,
}

fn active(cancel: &AtomicBool) -> Result<(), StoreError> {
    if cancel.load(Ordering::Acquire) {
        Err(cancelled())
    } else {
        Ok(())
    }
}

fn retained<T>(context: &str, call: impl FnOnce() -> io::Result<T>) -> Result<T, StoreError> {
    catch_unwind(AssertUnwindSafe(call))
        .unwrap_or_else(|_| Err(io::Error::other("retained image callback panicked")))
        .map_err(|error| io_error(context, error))
}

fn fingerprint(
    files: &StageFiles,
    bytes: u64,
    cancel: &AtomicBool,
) -> Result<[u8; 32], StoreError> {
    let mut position = 0u64;
    let mut hash = Sha256::new();
    let mut buffer = [0u8; 64 * 1024];
    let witness = bytes
        .checked_add(1)
        .ok_or_else(|| StoreError::corrupt("retained image length exceeds hashing bound"))?;
    loop {
        active(cancel)?;
        let count = usize::try_from((witness - position).min(buffer.len() as u64)).unwrap();
        let read = match retained("hashing retained image", || {
            files.database.read_at(&mut buffer[..count], position)
        }) {
            Err(error) if error.io_kind() == Some(io::ErrorKind::Interrupted) => continue,
            result => result?,
        };
        if read > count {
            return Err(StoreError::corrupt("invalid retained image read count"));
        }
        if read == 0 {
            break;
        }
        position += read as u64;
        if position > bytes {
            return Err(StoreError::corrupt("retained image grew during hashing"));
        }
        hash.update(&buffer[..read]);
    }
    if position != bytes {
        return Err(StoreError::corrupt("retained image length changed"));
    }
    Ok(hash.finalize().into())
}

fn source_uuid(connection: &Connection, expected: &str) -> Result<(), StoreError> {
    let mut statement = connection
        .prepare("SELECT value FROM store_meta NOT INDEXED WHERE key='store_uuid' LIMIT 2")
        .map_err(|e| inspection_error(e, "prepare staged identity"))?;
    let mut rows = statement
        .query([])
        .map_err(|e| inspection_error(e, "read staged identity"))?;
    let row = rows
        .next()
        .map_err(|e| inspection_error(e, "read staged identity"))?
        .ok_or_else(|| StoreError::corrupt("staged identity is missing"))?;
    let value: String = row
        .get(0)
        .map_err(|e| inspection_error(e, "read staged identity"))?;
    let parsed = value
        .parse::<crate::Uuid>()
        .map_err(|_| StoreError::corrupt("staged identity is invalid"))?;
    if rows
        .next()
        .map_err(|e| inspection_error(e, "read staged identity"))?
        .is_some()
        || expected.parse::<crate::Uuid>().ok() != Some(parsed)
    {
        return Err(StoreError::corrupt(
            "staged identity differs from the manifest",
        ));
    }
    Ok(())
}

fn close_error(error: CloseError) -> StoreError {
    match error {
        CloseError::Sqlite(error) => inspection_error(error, "close staged quarantine"),
        CloseError::IoError => StoreError::unavailable("staged quarantine close failed on I/O"),
        _ => StoreError::corrupt("staged quarantine WAL state is ineligible"),
    }
}

/// The caller still authenticates the manifest, all objects and every source,
/// live and stage binding before exposing a qualified stage. No SQL connection
/// crosses this return. Failed stages are neither removed nor reused.
pub(super) fn run(
    files: &StageFiles,
    seams: &Seams,
    input: &Quarantine<'_>,
    expected_uuid: &str,
    mut progress: impl FnMut(ImagePhase),
) -> Result<QuarantinedImage, StoreError> {
    let cancel = input.cancel.as_ref();
    let mut phase = |point| {
        progress(point);
        active(cancel)
    };
    active(cancel)?;
    if retained("measuring retained image", || files.database.len())? != files.database_bytes
        || fingerprint(files, files.database_bytes, cancel)? != files.database_sha256
    {
        return Err(StoreError::corrupt(
            "staged bytes differ from the selected image",
        ));
    }
    phase(ImagePhase::BytesVerified)?;
    stage_schema::inspect(files.database_reader(), cancel)?.ok_or_else(cancelled)?;
    stage_integrity::inspect(files.database_reader(), seams, cancel)?.ok_or_else(cancelled)?;
    phase(ImagePhase::ReadOnlyInspected)?;
    let create = |name| {
        files
            .root
            .create_read_write(Path::new(name))
            .map(Into::into)
            .map_err(|e| io_error("creating private staging sidecar", e))
    };
    let (mut writer, mut budget) = stage_input::open_bounded(
        files.database.clone(),
        create("state.sqlite-journal")?,
        create("state.sqlite-statement-journal")?,
        create("state.sqlite-wal")?,
        files.database_bytes,
        input.cancel.clone(),
    )?;
    let work = (|| {
        let (catalog, value_limit) =
            stage_canonical::inspect_writer(writer.connection(), seams, cancel)?
                .ok_or_else(cancelled)?;
        guarded(
            writer.connection(),
            cancel,
            MAX_PROGRESS_CALLS,
            |connection| source_uuid(connection, expected_uuid),
        )?
        .ok_or_else(cancelled)?;
        phase(ImagePhase::WriterInspected)?;
        if fingerprint(files, files.database_bytes, cancel)? != files.database_sha256 {
            return Err(StoreError::corrupt(
                "staged bytes changed during inspection",
            ));
        }
        if let Some(error) = budget.error() {
            return Err(error);
        }
        budget.begin_data_phase()?;
        let connection = writer.connection_mut();
        connection
            .set_limit(Limit::SQLITE_LIMIT_LENGTH, value_limit)
            .map_err(|e| inspection_error(e, "restore staged data value limit"))?;
        connection
            .execute_batch("PRAGMA query_only=OFF;")
            .map_err(|e| inspection_error(e, "enable isolated quarantine writes"))?;
        quarantine::apply(connection, input)?;
        phase(ImagePhase::Quarantined)?;
        // Borrow before checkpoint. Calling connection() afterward would
        // invalidate the wrapper's checkpoint certification.
        guarded(
            writer.connection(),
            cancel,
            quarantine::work_allowance(files.database_bytes),
            |_| writer.checkpoint_wal().map_err(close_error),
        )?
        .ok_or_else(cancelled)?;
        let bytes = retained("measuring quarantined image", || files.database.len())?;
        let sha256 = fingerprint(files, bytes, cancel)?;
        phase(ImagePhase::Checkpointed)?;
        Ok((catalog.schema_version, bytes, sha256))
    })();
    let closed = writer.close();
    if let Some(error) = budget.error() {
        return Err(error);
    }
    let (schema_version, bytes, sha256) = match (work, closed) {
        (Err(_), Err(error @ (CloseError::Sqlite(_) | CloseError::IoError))) => {
            return Err(close_error(error));
        }
        (Err(error), _) => return Err(error),
        (Ok(_), Err(error)) => return Err(close_error(error)),
        (Ok(observed), Ok(())) => observed,
    };
    phase(ImagePhase::Closed)?;
    retained("flushing quarantined database", || {
        files.database.sync_all()
    })?;
    retained("flushing quarantined directory", || files.root.sync())?;
    phase(ImagePhase::Flushed)?;
    let observed =
        stage_integrity::inspect_after(files.database_reader(), seams, cancel, |connection| {
            audit(connection, input, expected_uuid)
        })?
        .ok_or_else(cancelled)?;
    if observed.file_bytes != bytes || fingerprint(files, bytes, cancel)? != sha256 {
        return Err(StoreError::corrupt(
            "quarantined image changed after checkpoint",
        ));
    }
    active(cancel)?;
    Ok(QuarantinedImage {
        root_identity: retained("reading quarantined root identity", || {
            files.root.identity()
        })?,
        database_identity: retained("reading quarantined file identity", || {
            files.database.identity()
        })?,
        sha256,
        bytes,
        schema_version,
    })
}

fn audit(
    connection: &Connection,
    input: &Quarantine<'_>,
    expected_uuid: &str,
) -> Result<(), StoreError> {
    connection
        .set_limit(Limit::SQLITE_LIMIT_LENGTH, 1_048_576)
        .map_err(|e| inspection_error(e, "bound quarantine metadata audit"))?;
    source_uuid(connection, expected_uuid)?;
    let incarnation: String = connection
        .query_row(
            "SELECT value FROM store_meta NOT INDEXED WHERE key='incarnation'",
            [],
            |row| row.get(0),
        )
        .map_err(|e| inspection_error(e, "audit quarantined incarnation"))?;
    if incarnation != input.new_incarnation {
        return Err(StoreError::corrupt("quarantined incarnation changed"));
    }
    for sql in [
        "SELECT 1 FROM accounts NOT INDEXED WHERE state IS NOT 'paused' LIMIT 1",
        "SELECT 1 FROM import_jobs NOT INDEXED WHERE state IN ('inventory','awaiting_confirmation','running') LIMIT 1",
        "SELECT 1 FROM intents NOT INDEXED WHERE state IN ('pending','in_flight','parked') LIMIT 1",
        "SELECT 1 FROM send_attempts NOT INDEXED WHERE state IN ('held','queued','submitting') LIMIT 1",
        "SELECT 1 FROM backup_generations NOT INDEXED WHERE state='in_progress' LIMIT 1",
        "SELECT 1 FROM object_pins NOT INDEXED WHERE pin_kind IN ('backup_in_progress','reservation') LIMIT 1",
        "SELECT 1 FROM backup_cleanup_pending LIMIT 1",
        "SELECT 1 FROM verifier_objects LIMIT 1",
        "SELECT 1 FROM verifier_scratch LIMIT 1",
    ] {
        if connection
            .query_row(sql, [], |_| Ok(()))
            .optional()
            .map_err(|e| inspection_error(e, "audit quarantined authority"))?
            .is_some()
        {
            return Err(StoreError::corrupt(
                "quarantined image retains active authority",
            ));
        }
    }
    let location = Path::new(input.source_location);
    let rewritten = location
        .strip_prefix(input.current_root)
        .ok()
        .map(|relative| input.preserved_root.join(relative));
    let expected_location = rewritten
        .as_deref()
        .unwrap_or(location)
        .to_str()
        .ok_or_else(|| StoreError::corrupt("invalid quarantined source location"))?;
    let row: (String, String, String, i64, i64, i64) = connection.query_row(
        "SELECT state, location, manifest_sha256, object_count, total_bytes, completed_at FROM backup_generations NOT INDEXED WHERE generation_id=?1",
        [input.generation], |row| Ok((row.get(0)?,row.get(1)?,row.get(2)?,row.get(3)?,row.get(4)?,row.get(5)?)),
    ).map_err(|e| inspection_error(e, "audit restored generation"))?;
    if row
        != (
            "complete".into(),
            expected_location.into(),
            input.manifest_sha256.into(),
            input.object_count,
            input.total_bytes,
            input.completed_at,
        )
    {
        return Err(StoreError::corrupt(
            "quarantined generation metadata changed",
        ));
    }
    if connection
        .query_row(
            "SELECT 1 FROM backup_ownership WHERE generation_id=?1",
            params![input.generation],
            |_| Ok(()),
        )
        .optional()
        .map_err(|e| inspection_error(e, "audit restored generation ownership"))?
        .is_some()
    {
        return Err(StoreError::corrupt(
            "quarantined generation retains old ownership",
        ));
    }
    Ok(())
}
