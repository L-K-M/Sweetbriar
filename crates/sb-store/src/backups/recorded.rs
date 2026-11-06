//! Point-in-time verification of a generation recorded in this store.
use super::*;
use rusqlite::OptionalExtension;
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::io::{Read, Write};
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};

fn io_error(e: io::Error) -> StoreError {
    StoreError::unavailable_io(format!("backup verification I/O: {e}"), e.kind())
}

struct RecordedGeneration {
    location: String,
    started: i64,
    state: String,
    manifest_hash: Option<String>,
    count: Option<i64>,
    total: Option<i64>,
    completed: Option<i64>,
}

fn cancelled(cancel: &AtomicBool) -> bool {
    cancel.load(Ordering::Acquire)
}

struct HashReader<R> {
    input: R,
    hash: Sha256,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub(super) enum StreamPhase {
    Manifest,
    Object,
}
impl<R: Read> Read for HashReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.input.read(buf)?;
        self.hash.update(&buf[..n]);
        Ok(n)
    }
}

fn digest(text: &str) -> Result<[u8; 32], StoreError> {
    if text.len() != 64
        || !text
            .bytes()
            .all(|c| c.is_ascii_digit() || (b'a'..=b'f').contains(&c))
    {
        return Err(StoreError::corrupt("invalid recorded manifest digest"));
    }
    let mut out = [0; 32];
    for (i, pair) in text.as_bytes().as_chunks::<2>().0.iter().enumerate() {
        out[i] = super::json::hex(pair[0]).unwrap() * 16 + super::json::hex(pair[1]).unwrap();
    }
    Ok(out)
}

impl Backups {
    pub(crate) fn verify_recorded(
        &self,
        generation: &str,
        cancel: &AtomicBool,
    ) -> Result<BackupImageInspection, StoreError> {
        self.verify_recorded_with(generation, cancel, |_| {})
    }

    pub(super) fn verify_recorded_with(
        &self,
        generation: &str,
        cancel: &AtomicBool,
        progress: impl FnMut(StreamPhase),
    ) -> Result<BackupImageInspection, StoreError> {
        let progress = RefCell::new(progress);
        if self.writer.is_writer_thread() {
            return Err(StoreError::internal("writer cannot verify a backup"));
        }
        let token = Arc::new(());
        {
            let mut active = self
                .active
                .try_lock()
                .map_err(|_| StoreError::conflict("backup operation is already running"))?;
            if active.as_ref().and_then(Weak::upgrade).is_some() {
                return Err(StoreError::conflict("backup operation is already running"));
            }
            *active = Some(Arc::downgrade(&token));
        }
        let conn = open_connection(&self.state_db, Access::ReadOnly, READER_PRAGMAS)?;
        let row = conn.query_row(
            "SELECT location, started_at, state, manifest_sha256, object_count, total_bytes, completed_at FROM backup_generations WHERE generation_id=?1", [generation],
            |r| Ok(RecordedGeneration {
                location: r.get(0)?,
                started: r.get(1)?,
                state: r.get(2)?,
                manifest_hash: r.get(3)?,
                count: r.get(4)?,
                total: r.get(5)?,
                completed: r.get(6)?,
            })
        ).optional().map_err(|e| classify(e, "read recorded generation"))?;
        let RecordedGeneration {
            location,
            started,
            state,
            manifest_hash,
            count,
            total,
            completed,
        } = row.ok_or_else(|| StoreError::not_found(format!("backup generation {generation}")))?;
        match state.as_str() {
            "complete" => {}
            "in_progress" | "failed" => {
                return Err(StoreError::conflict("backup generation is not complete"));
            }
            _ => return Err(StoreError::corrupt("invalid recorded generation state")),
        }
        let (Some(manifest_hash), Some(count), Some(total), Some(_completed)) =
            (manifest_hash, count, total, completed)
        else {
            return Err(StoreError::corrupt(
                "incomplete metadata for complete generation",
            ));
        };
        let parsed_id: Uuid = generation
            .parse()
            .map_err(|_| StoreError::corrupt("invalid generation id"))?;
        if parsed_id.to_string() != generation
            || count < 0
            || total < 0
            || Path::new(&location)
                .file_name()
                .and_then(|name| name.to_str())
                != Some(generation)
        {
            return Err(StoreError::corrupt("invalid recorded generation"));
        }
        let store: String = conn
            .query_row(
                "SELECT value FROM store_meta WHERE key='store_uuid'",
                [],
                |r| r.get(0),
            )
            .map_err(|e| classify(e, "read store identity"))?;
        drop(conn);
        let expected_hash = digest(&manifest_hash)?;
        let path = Path::new(&location);
        let parent = self
            .fs
            .open_confined_dir(
                path.parent()
                    .ok_or_else(|| StoreError::corrupt("invalid backup location"))?,
            )
            .map_err(io_error)?;
        let name = Path::new(generation);
        let source = parent.open_dir(name).map_err(io_error)?;
        let identity = source.identity().map_err(io_error)?;
        let database = RefCell::new(None);
        let outcome = self.inspect_image_session(
            source.as_ref(),
            cancel,
            |_| ControlFlow::Continue(()),
            |scratch| {
                let (mut input, _) = source
                    .open_regular(Path::new("manifest.json"))
                    .map_err(|error| source_open_error(error, "backup manifest"))?;
                let mut output = scratch
                    .create(Path::new("manifest.json"))
                    .map_err(io_error)?;
                let mut buf = [0; 65536];
                loop {
                    if cancelled(cancel) {
                        return Ok(([0; 32], 0));
                    }
                    let n = input.read(&mut buf).map_err(io_error)?;
                    if n == 0 {
                        break;
                    }
                    output.write_all(&buf[..n]).map_err(io_error)?;
                    progress.borrow_mut()(StreamPhase::Manifest);
                }
                output.sync_all().map_err(io_error)?;
                drop(output);
                scratch.sync().map_err(io_error)?;
                let (input, _) = scratch
                    .open_regular(Path::new("manifest.json"))
                    .map_err(io_error)?;
                let mut input = HashReader {
                    input,
                    hash: Sha256::new(),
                };
                let parsed = manifest::parse(
                    &mut input,
                    || {
                        if cancelled(cancel) {
                            Err(io::Error::from(io::ErrorKind::Interrupted))
                        } else {
                            Ok(())
                        }
                    },
                    |_| Ok(()),
                );
                let manifest = match parsed {
                    Err(e) if e.kind() == io::ErrorKind::Interrupted && cancelled(cancel) => {
                        return Ok(([0; 32], 0));
                    }
                    Err(error) if error.kind() == io::ErrorKind::InvalidData => {
                        return Err(StoreError::corrupt(format!(
                            "invalid backup manifest: {error}"
                        )));
                    }
                    result => result.map_err(io_error)?,
                };
                if input.hash.finalize().as_slice() != expected_hash {
                    return Err(StoreError::corrupt("manifest digest mismatch"));
                }
                let result = (manifest.database.sha256, manifest.database.bytes);
                *database.borrow_mut() = Some(manifest.database);
                Ok(result)
            },
            |image, id, scratch| {
                let (input, _) = scratch
                    .open_regular(Path::new("manifest.json"))
                    .map_err(io_error)?;
                let mut input = HashReader {
                    input,
                    hash: Sha256::new(),
                };
                let expected = reconciliation::Expected {
                    database: database.borrow_mut().take().expect("prepared manifest"),
                    object_count: count as u64,
                    total_bytes: total as u64,
                };
                let result = reconciliation::run_checked(
                    image,
                    self,
                    id,
                    &mut input,
                    expected,
                    cancel,
                    Some((generation, started, &store)),
                )?;
                if result.is_break() {
                    return Ok(result);
                }
                if input.hash.finalize().as_slice() != expected_hash {
                    return Err(StoreError::corrupt("manifest digest mismatch"));
                }
                let conn = open_connection(&self.state_db, Access::ReadOnly, READER_PRAGMAS)?;
                let mut after = None;
                loop {
                    if cancelled(cancel) {
                        return Ok(ControlFlow::Break(()));
                    }
                    let page = ledger::page(&conn, id, after)?;
                    for row in &page {
                        let hash = row
                            .entry
                            .sha256
                            .iter()
                            .map(|b| format!("{b:02x}"))
                            .collect::<String>();
                        let path =
                            format!("objects/sha256/{}/{}/{}", &hash[..2], &hash[2..4], hash);
                        let (mut file, _) =
                            source.open_regular(Path::new(&path)).map_err(|error| {
                                source_open_error(error, format!("backup object {hash}"))
                            })?;
                        let mut hasher = Sha256::new();
                        let mut length = 0u64;
                        let mut buf = [0; 65536];
                        loop {
                            if cancelled(cancel) {
                                return Ok(ControlFlow::Break(()));
                            }
                            let n = file.read(&mut buf).map_err(io_error)?;
                            if n == 0 {
                                break;
                            }
                            length = length
                                .checked_add(n as u64)
                                .ok_or_else(|| StoreError::corrupt("object length overflow"))?;
                            if length > row.entry.bytes {
                                return Err(StoreError::corrupt("object length mismatch"));
                            }
                            hasher.update(&buf[..n]);
                            progress.borrow_mut()(StreamPhase::Object);
                        }
                        if length != row.entry.bytes
                            || hasher.finalize().as_slice() != row.entry.sha256
                        {
                            return Err(StoreError::corrupt("object digest or length mismatch"));
                        }
                    }
                    if page.len() < ledger::BATCH {
                        break;
                    }
                    after = page.last().map(|row| row.entry.sha256);
                }
                if cancelled(cancel) {
                    Ok(ControlFlow::Break(()))
                } else {
                    if parent
                        .open_dir(name)
                        .map_err(io_error)?
                        .identity()
                        .map_err(io_error)?
                        != identity
                    {
                        return Err(StoreError::corrupt("backup generation directory changed"));
                    }
                    Ok(ControlFlow::Continue(()))
                }
            },
        );
        drop(token);
        outcome
    }
}
