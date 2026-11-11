//! Reconcile a manifest with the retained image, without verifying object bytes.
//! All ledger effects belong to the image owner and are discarded on failure.

use std::io::{self, Read};
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};

use rusqlite::Connection;

use super::{
    BackupImageInspection, Backups, attachments::Hash, home::Home, image_inspect, ledger, manifest,
    references,
};
use crate::StoreError;

#[cfg(test)]
mod tests;

pub(super) struct Expected {
    pub database: manifest::Entry,
    pub object_count: u64,
    /// Database and unique object bytes, as recorded by the backup writer.
    pub total_bytes: u64,
}

impl Backups {
    /// Reconcile only; this does not select a generation or verify its objects.
    pub(super) fn reconcile_image(
        &self,
        generation: &std::path::Path,
        input: impl Read,
        expected: Expected,
        cancelled: &AtomicBool,
        progress: impl FnMut(u64) -> ControlFlow<()>,
    ) -> Result<BackupImageInspection, StoreError> {
        self.inspect_image_with(
            generation,
            expected.database.sha256,
            expected.database.bytes,
            cancelled,
            progress,
            |conn, id| run(conn, self, id, input, expected, cancelled),
        )
    }
}

struct Tracked<'a, R> {
    input: R,
    error: &'a mut Option<io::Error>,
}

impl<R: Read> Read for Tracked<'_, R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        self.input.read(buf).map_err(|e| {
            *self.error = Some(e);
            io::Error::other("manifest read failed")
        })
    }
}

pub(super) fn uuid(bytes: [u8; 16]) -> String {
    let mut out = String::with_capacity(36);
    use std::fmt::Write;
    for (i, byte) in bytes.iter().enumerate() {
        if matches!(i, 4 | 6 | 8 | 10) {
            out.push('-');
        }
        write!(out, "{byte:02x}").expect("write to string");
    }
    out
}

fn reference_hash(hash: Hash) -> Result<[u8; 32], StoreError> {
    let Hash::Bytes(text) = hash else {
        return Err(StoreError::corrupt("invalid snapshot reference hash"));
    };
    let mut result = [0; 32];
    for (out, pair) in result.iter_mut().zip(text.as_chunks::<2>().0) {
        if pair
            .iter()
            .any(|c| !c.is_ascii_digit() && !(b'a'..=b'f').contains(c))
        {
            return Err(StoreError::corrupt("invalid snapshot reference hash"));
        }
        *out = super::json::hex(pair[0]).expect("validated hash") * 16
            + super::json::hex(pair[1]).expect("validated hash");
    }
    Ok(result)
}

// Retain the physical cursor across bounded batches. Keyset seeks can skip
// a duplicate generation hidden by corrupt rowid order in the source image.
fn generation_matches(
    conn: &Connection,
    manifest: &manifest::Manifest,
    cancelled: &AtomicBool,
) -> Result<ControlFlow<()>, StoreError> {
    let shape = image_inspect::guarded(
        conn,
        cancelled,
        image_inspect::MAX_PROGRESS_CALLS,
        |conn| {
            let table: bool = conn.query_row("SELECT EXISTS(SELECT 1 FROM pragma_table_list WHERE schema='main' AND name='backup_generations' AND type='table' AND wr=0)", [], |row| row.get(0))
            .map_err(|e| image_inspect::inspection_error(e, "read snapshot generation schema"))?;
            if !table {
                return Err(StoreError::corrupt("invalid snapshot generation table"));
            }
            let mut stmt = conn
                .prepare("SELECT name, type, hidden FROM pragma_table_xinfo('backup_generations')")
                .map_err(|e| {
                    image_inspect::inspection_error(e, "read snapshot generation schema")
                })?;
            let mut rows = stmt.query([]).map_err(|e| {
                image_inspect::inspection_error(e, "read snapshot generation schema")
            })?;
            let mut found = 0u8;
            while let Some(row) = rows.next().map_err(|e| {
                image_inspect::inspection_error(e, "read snapshot generation schema")
            })? {
                let name: String = row.get(0).map_err(|e| {
                    image_inspect::inspection_error(e, "read snapshot generation schema")
                })?;
                let ty: String = row.get(1).map_err(|e| {
                    image_inspect::inspection_error(e, "read snapshot generation schema")
                })?;
                let hidden: i64 = row.get(2).map_err(|e| {
                    image_inspect::inspection_error(e, "read snapshot generation schema")
                })?;
                if ["rowid", "_rowid_", "oid"]
                    .iter()
                    .any(|n| name.eq_ignore_ascii_case(n))
                {
                    return Err(StoreError::corrupt("snapshot generation shadows rowid"));
                }
                for (i, (column, expected)) in [
                    ("generation_id", "TEXT"),
                    ("started_at", "INTEGER"),
                    ("state", "TEXT"),
                ]
                .iter()
                .enumerate()
                {
                    if name == *column && hidden == 0 && ty.eq_ignore_ascii_case(expected) {
                        found |= 1 << i;
                    }
                }
            }
            if found != 7 {
                return Err(StoreError::corrupt("invalid snapshot generation columns"));
            }
            Ok(())
        },
    )?;
    if shape.is_none() {
        return Ok(ControlFlow::Break(()));
    }
    let target = uuid(manifest.generation_id);
    let Some(mut stmt) = image_inspect::guarded(
        conn,
        cancelled,
        image_inspect::MAX_PROGRESS_CALLS,
        |_| {
            conn.prepare("SELECT rowid, typeof(generation_id), substr(CAST(generation_id AS BLOB),1,37), started_at, typeof(state), substr(CAST(state AS BLOB),1,12) FROM backup_generations NOT INDEXED ORDER BY rowid")
            .map_err(|e| image_inspect::inspection_error(e, "read snapshot generation"))
        },
    )?
    else {
        return Ok(ControlFlow::Break(()));
    };
    let Some(mut rows) =
        image_inspect::guarded(conn, cancelled, image_inspect::MAX_PROGRESS_CALLS, |_| {
            stmt.query([])
                .map_err(|e| image_inspect::inspection_error(e, "read snapshot generation"))
        })?
    else {
        return Ok(ControlFlow::Break(()));
    };
    let mut last = None;
    let mut matched = false;
    loop {
        let page =
            image_inspect::guarded(conn, cancelled, image_inspect::MAX_PROGRESS_CALLS, |_| {
                let mut count = 0;
                while count < ledger::BATCH {
                    let Some(row) = rows.next().map_err(|e| {
                        image_inspect::inspection_error(e, "read snapshot generation")
                    })?
                    else {
                        break;
                    };
                    if cancelled.load(Ordering::Acquire) {
                        break;
                    }
                    let id = row.get::<_, i64>(0).map_err(|e| {
                        image_inspect::inspection_error(e, "read snapshot generation")
                    })?;
                    if last.is_some_and(|previous| id <= previous) {
                        return Err(StoreError::corrupt(
                            "snapshot generation rowids are out of order",
                        ));
                    }
                    last = Some(id);
                    let kind: String = row.get(1).map_err(|e| {
                        image_inspect::inspection_error(e, "read snapshot generation")
                    })?;
                    let id: Vec<u8> = row.get(2).map_err(|e| {
                        image_inspect::inspection_error(e, "read snapshot generation")
                    })?;
                    if kind == "text" && id == target.as_bytes() {
                        let started: i64 = row.get(3).map_err(|e| {
                            image_inspect::inspection_error(e, "read snapshot generation")
                        })?;
                        let state_kind: String = row.get(4).map_err(|e| {
                            image_inspect::inspection_error(e, "read snapshot generation")
                        })?;
                        let state: Vec<u8> = row.get(5).map_err(|e| {
                            image_inspect::inspection_error(e, "read snapshot generation")
                        })?;
                        if matched
                            || started != manifest.created_at
                            || state_kind != "text"
                            || state != b"in_progress"
                        {
                            return Err(StoreError::corrupt(
                                "snapshot generation identity mismatch",
                            ));
                        }
                        matched = true;
                    }
                    count += 1;
                }
                Ok(count)
            })?;
        let Some(count) = page else {
            return Ok(ControlFlow::Break(()));
        };
        if count < ledger::BATCH {
            break;
        }
    }
    if !matched {
        return Err(StoreError::corrupt("snapshot generation identity mismatch"));
    }
    if cancelled.load(Ordering::Acquire) {
        Ok(ControlFlow::Break(()))
    } else {
        Ok(ControlFlow::Continue(()))
    }
}

/// The image and ledger are already owned by `id`. The caller supplies the
/// authenticated database digest and independently recorded totals. Neither
/// this function nor a successful image inspection verifies object file bytes.
pub(super) fn run(
    conn: &Connection,
    backups: &Backups,
    id: &str,
    input: impl Read,
    expected: Expected,
    cancelled: &AtomicBool,
) -> Result<ControlFlow<()>, StoreError> {
    run_checked(
        conn,
        &backups.store_home(),
        id,
        input,
        expected,
        cancelled,
        None,
    )
}

/// Reconcile in `home`, the running verification's: its ledger holds
/// the manifest's objects under `id`.
pub(super) fn run_checked(
    conn: &Connection,
    home: &Home,
    id: &str,
    input: impl Read,
    expected: Expected,
    cancelled: &AtomicBool,
    identity: Option<(&str, i64, &str)>,
) -> Result<ControlFlow<()>, StoreError> {
    let mut entries = Vec::with_capacity(ledger::BATCH);
    let mut count = 0u64;
    let mut total = 0u64;
    let mut sink_error = None;
    let mut read_error = None;
    let parsed = manifest::parse(
        Tracked {
            input,
            error: &mut read_error,
        },
        || {
            if cancelled.load(Ordering::Acquire) {
                Err(io::Error::new(io::ErrorKind::Interrupted, "cancelled"))
            } else {
                Ok(())
            }
        },
        |entry| {
            let result = (|| {
                count = count
                    .checked_add(1)
                    .ok_or_else(|| StoreError::corrupt("manifest object count overflow"))?;
                total = total
                    .checked_add(entry.bytes)
                    .ok_or_else(|| StoreError::corrupt("manifest total overflow"))?;
                entries.push(entry);
                if entries.len() == ledger::BATCH {
                    ledger::insert(home, id.to_owned(), std::mem::take(&mut entries))?;
                }
                Ok::<_, StoreError>(())
            })();
            result.map_err(|error| {
                sink_error = Some(error);
                io::Error::other("manifest sink failed")
            })
        },
    );
    if let Some(e) = sink_error {
        return Err(e);
    }
    if let Some(e) = read_error {
        return Err(StoreError::unavailable_io(
            format!("read manifest: {e}"),
            e.kind(),
        ));
    }
    if cancelled.load(Ordering::Acquire) {
        return Ok(ControlFlow::Break(()));
    }
    let manifest = parsed.map_err(|e| {
        if e.kind() == io::ErrorKind::InvalidData {
            StoreError::corrupt(format!("invalid manifest: {e}"))
        } else {
            StoreError::unavailable_io(format!("read manifest: {e}"), e.kind())
        }
    })?;
    if let Some((generation, started, store)) = identity
        && (uuid(manifest.generation_id) != generation
            || manifest.created_at != started
            || uuid(manifest.store_uuid) != store)
    {
        return Err(StoreError::corrupt(
            "manifest identity differs from selected generation",
        ));
    }
    ledger::insert(home, id.to_owned(), entries)?;
    let total = total
        .checked_add(manifest.database.bytes)
        .ok_or_else(|| StoreError::corrupt("manifest total overflow"))?;
    if manifest.database != expected.database
        || count != expected.object_count
        || total != expected.total_bytes
    {
        return Err(StoreError::corrupt("manifest database or totals mismatch"));
    }
    let inspected = image_inspect::inspect(conn, cancelled)?;
    match inspected {
        image_inspect::Inspection::Cancelled => return Ok(ControlFlow::Break(())),
        image_inspect::Inspection::Inspected {
            store_uuid,
            schema_version,
        } if store_uuid == uuid(manifest.store_uuid)
            && schema_version == manifest.schema_version => {}
        _ => return Err(StoreError::corrupt("snapshot identity mismatch")),
    }
    if generation_matches(conn, &manifest, cancelled)?.is_break() {
        return Ok(ControlFlow::Break(()));
    }
    let mut refs = Vec::with_capacity(ledger::BATCH);
    let walked = references::walk(conn, cancelled, |reference| {
        refs.push((reference_hash(reference.hash)?, reference.size));
        if refs.len() == ledger::BATCH {
            ledger::reference_sized(home, id.to_owned(), std::mem::take(&mut refs))?;
        }
        Ok(())
    })?;
    if walked.is_break() || cancelled.load(Ordering::Acquire) {
        return Ok(ControlFlow::Break(()));
    }
    ledger::reference_sized(home, id.to_owned(), refs)?;
    let ledger_conn = home.reader()?;
    let mut after = None;
    loop {
        if cancelled.load(Ordering::Acquire) {
            return Ok(ControlFlow::Break(()));
        }
        let page = ledger::page(&ledger_conn, id, after)?;
        if page.iter().any(|row| !row.referenced) {
            return Err(StoreError::corrupt("manifest object absent from snapshot"));
        }
        if page.len() < ledger::BATCH {
            break;
        }
        after = page.last().map(|row| row.entry.sha256);
    }
    if cancelled.load(Ordering::Acquire) {
        Ok(ControlFlow::Break(()))
    } else {
        Ok(ControlFlow::Continue(()))
    }
}
