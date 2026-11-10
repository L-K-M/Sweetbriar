//! The weekly check of a retained generation in place (spec/03 "Backup
//! generations", plan M1.8h4b): its manifest against the digest its row
//! recorded, then the database and every object copy against the
//! manifest, with the verdict recorded on the generation.
//!
//! Lighter than [`Backups::verify_recorded`]: no image copy, no
//! reconciliation ledger. Those prove a generation restorable when it
//! is chosen; this proves its bytes are still the ones it published,
//! for every retained generation every week. The first problem found
//! decides: an absent file is `missing`, any mismatch `damaged`. A file
//! changed while it is read reads as damaged, which a backup copy
//! someone is modifying is.

use super::*;
use rusqlite::OptionalExtension;
use sha2::{Digest, Sha256};
use std::io::Read;
use std::sync::atomic::{AtomicBool, Ordering};

/// How a retained generation's bytes held up.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenerationVerdict {
    /// The manifest, database and every object match.
    Intact,
    /// A file the manifest names differs from it, or the manifest from
    /// its recorded digest.
    Damaged,
    /// The generation's directory, its manifest, database or an object
    /// is absent.
    Missing,
}

impl GenerationVerdict {
    pub(crate) fn as_str(self) -> &'static str {
        match self {
            Self::Intact => "intact",
            Self::Damaged => "damaged",
            Self::Missing => "missing",
        }
    }

    pub(crate) fn parse(text: &str) -> Option<Self> {
        match text {
            "intact" => Some(Self::Intact),
            "damaged" => Some(Self::Damaged),
            "missing" => Some(Self::Missing),
            _ => None,
        }
    }
}

/// What one check of a generation came to.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum GenerationCheck {
    /// Checked, and the verdict recorded on the generation.
    Recorded(GenerationVerdict),
    /// The directory holding it isn't there (an unmounted destination):
    /// nothing recorded, the next run tries again.
    DestinationAbsent,
    /// Retention removed it, or it stopped being complete: nothing to
    /// record.
    Gone,
    /// Stopped when asked: nothing recorded.
    Stopped,
}

/// The outcome of reading a generation, before recording it.
enum Inspected {
    Verdict(GenerationVerdict),
    DestinationAbsent,
    Gone,
    Stopped,
}

/// A file's bytes against the manifest's record.
enum Compared {
    Match,
    Mismatch,
    Stopped,
}

/// Why a manifest sink stopped the parse.
enum SinkStop {
    Verdict(GenerationVerdict),
    Failed(StoreError),
}

fn io_error(e: io::Error) -> StoreError {
    StoreError::unavailable_io(format!("backup check I/O: {e}"), e.kind())
}

/// What an open's error says about the generation: an absent entry is
/// `missing`; a directory where a file belongs, or a file or fifo where
/// a directory belongs, is `damaged`, since publication creates only
/// directories and regular files. Anything else is an I/O error,
/// including a symlink's ELOOP, which std doesn't name yet: nothing is
/// recorded and the next run tries again.
fn opened_verdict(error: io::Error) -> Result<GenerationVerdict, StoreError> {
    match error.kind() {
        io::ErrorKind::NotFound => Ok(GenerationVerdict::Missing),
        io::ErrorKind::InvalidInput
        | io::ErrorKind::NotADirectory
        | io::ErrorKind::IsADirectory => Ok(GenerationVerdict::Damaged),
        _ => Err(io_error(error)),
    }
}

fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

impl Backups {
    /// Check `generation` in place and record the verdict on its row
    /// (bookkeeping: never a change that makes a backup due). Holds the
    /// backup operation while reading, so a backup or a restore waits
    /// for at most this one generation.
    pub(crate) fn check_generation(
        &self,
        generation: &str,
        cancel: &AtomicBool,
    ) -> Result<GenerationCheck, StoreError> {
        if self.writer.is_writer_thread() {
            return Err(StoreError::internal(
                "the writer thread cannot check a backup through its own queue",
            ));
        }
        // The verdict is a write, which a latched store refuses: say
        // so before reading the generation, not after.
        self.objects
            .latch()
            .refuse("record a backup generation's check")?;
        let token = self.reserve_operation()?;
        let inspected = self.inspect_in_place(generation, cancel);
        drop(token);
        let verdict = match inspected? {
            // A stop raised at any point records nothing.
            Inspected::Verdict(_) if cancel.load(Ordering::Acquire) => {
                return Ok(GenerationCheck::Stopped);
            }
            Inspected::Verdict(verdict) => verdict,
            Inspected::DestinationAbsent => return Ok(GenerationCheck::DestinationAbsent),
            Inspected::Gone => return Ok(GenerationCheck::Gone),
            Inspected::Stopped => return Ok(GenerationCheck::Stopped),
        };
        let now = self.clock.now().as_millis();
        let generation = generation.to_owned();
        let future = self.writer.upkeep(WritePriority::Index, move |tx| {
            tx.execute(
                "UPDATE backup_generations SET verified_at = ?2, verification = ?3
                 WHERE generation_id = ?1 AND state = 'complete'",
                params![generation, now, verdict.as_str()],
            )
            .map_err(|e| classify(e, "record a generation check"))
        });
        // Retention may have removed it after the read.
        Ok(match await_write(future)? {
            0 => GenerationCheck::Gone,
            _ => GenerationCheck::Recorded(verdict),
        })
    }

    fn inspect_in_place(
        &self,
        generation: &str,
        cancel: &AtomicBool,
    ) -> Result<Inspected, StoreError> {
        if cancel.load(Ordering::Acquire) {
            return Ok(Inspected::Stopped);
        }
        let conn = open_connection(&self.state_db, Access::ReadOnly, READER_PRAGMAS)?;
        let row: Option<(String, String, Option<String>)> = conn
            .query_row(
                "SELECT location, state, manifest_sha256 FROM backup_generations
                 WHERE generation_id = ?1",
                [generation],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(|e| classify(e, "read a generation to check"))?;
        drop(conn);
        let Some((location, state, manifest_hash)) = row else {
            return Ok(Inspected::Gone);
        };
        if state != "complete" {
            return Ok(Inspected::Gone);
        }
        let manifest_hash = manifest_hash
            .ok_or_else(|| StoreError::corrupt("a complete generation has no manifest digest"))?;
        let expected_manifest = recorded::digest(&manifest_hash)?;
        let path = Path::new(&location);
        let (Some(parent_path), Some(name)) = (path.parent(), path.file_name()) else {
            return Err(StoreError::corrupt("invalid backup location"));
        };
        if name.to_str() != Some(generation) {
            return Err(StoreError::corrupt("invalid recorded generation"));
        }

        let parent = match self.fs.open_confined_dir(parent_path) {
            Ok(parent) => parent,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {
                return Ok(Inspected::DestinationAbsent);
            }
            Err(e) => return Err(io_error(e)),
        };
        let source = match parent.open_dir(Path::new(name)) {
            Ok(source) => source,
            Err(e) => return opened_verdict(e).map(Inspected::Verdict),
        };
        let identity = source.identity().map_err(io_error)?;
        let stopped = || cancel.load(Ordering::Acquire);

        // The manifest, hashed as it is parsed, its objects checked as
        // they stream past.
        let manifest_file = match source.open_regular(Path::new("manifest.json")) {
            Ok((file, _)) => file,
            Err(e) => return opened_verdict(e).map(Inspected::Verdict),
        };
        let mut input = recorded::HashReader {
            input: manifest_file,
            hash: Sha256::new(),
        };
        let mut sink_stop = None;
        let parsed = manifest::parse(
            &mut input,
            || {
                if stopped() {
                    Err(io::Error::from(io::ErrorKind::Interrupted))
                } else {
                    Ok(())
                }
            },
            |entry| {
                let path = {
                    let sha = hex(&entry.sha256);
                    format!("objects/sha256/{}/{}/{sha}", &sha[..2], &sha[2..4])
                };
                let verdict = match compare(source.as_ref(), &path, &entry, &stopped) {
                    Ok(Compared::Match) => return Ok(()),
                    Ok(Compared::Stopped) => {
                        return Err(io::Error::from(io::ErrorKind::Interrupted));
                    }
                    Ok(Compared::Mismatch) => SinkStop::Verdict(GenerationVerdict::Damaged),
                    Err(e) => match opened_verdict(e) {
                        Ok(verdict) => SinkStop::Verdict(verdict),
                        Err(error) => SinkStop::Failed(error),
                    },
                };
                sink_stop = Some(verdict);
                Err(io::Error::other("the check stopped at an object"))
            },
        );
        let manifest = match (parsed, sink_stop) {
            (_, Some(SinkStop::Verdict(verdict))) => return Ok(Inspected::Verdict(verdict)),
            (_, Some(SinkStop::Failed(error))) => return Err(error),
            (Err(e), None) if e.kind() == io::ErrorKind::Interrupted && stopped() => {
                return Ok(Inspected::Stopped);
            }
            (Err(e), None) if e.kind() == io::ErrorKind::InvalidData => {
                return Ok(Inspected::Verdict(GenerationVerdict::Damaged));
            }
            (Err(e), None) => return Err(io_error(e)),
            (Ok(manifest), None) => manifest,
        };
        if input.hash.finalize().as_slice() != expected_manifest
            || hex(&manifest.generation_id) != generation.replace('-', "")
        {
            return Ok(Inspected::Verdict(GenerationVerdict::Damaged));
        }

        match compare(source.as_ref(), SNAPSHOT_FILE, &manifest.database, &stopped) {
            Ok(Compared::Match) => {}
            Ok(Compared::Stopped) => return Ok(Inspected::Stopped),
            Ok(Compared::Mismatch) => return Ok(Inspected::Verdict(GenerationVerdict::Damaged)),
            Err(e) => return opened_verdict(e).map(Inspected::Verdict),
        }

        // The directory read is the one opened at the start.
        #[cfg(test)]
        {
            self.pause(|p| &p.before_identity_recheck);
            self.pause(|p| &p.before_identity_recheck);
        }
        let now = match parent.open_dir(Path::new(name)) {
            Ok(dir) => dir.identity().map_err(io_error)?,
            Err(e) => return opened_verdict(e).map(Inspected::Verdict),
        };
        if now != identity {
            return Ok(Inspected::Verdict(GenerationVerdict::Damaged));
        }
        Ok(Inspected::Verdict(GenerationVerdict::Intact))
    }
}

/// Hash `path` under `dir` against `entry`, polling the stop between
/// chunks.
fn compare(
    dir: &dyn sb_types::fs::ConfinedDir,
    path: &str,
    entry: &manifest::Entry,
    stopped: &dyn Fn() -> bool,
) -> io::Result<Compared> {
    let (mut file, _) = dir.open_regular(Path::new(path))?;
    let mut hasher = Sha256::new();
    let mut length = 0u64;
    let mut buf = [0; 65536];
    loop {
        if stopped() {
            return Ok(Compared::Stopped);
        }
        let n = file.read(&mut buf)?;
        if n == 0 {
            break;
        }
        length = length.saturating_add(n as u64);
        if length > entry.bytes {
            return Ok(Compared::Mismatch);
        }
        hasher.update(&buf[..n]);
    }
    Ok(
        if length == entry.bytes && hasher.finalize().as_slice() == entry.sha256 {
            Compared::Match
        } else {
            Compared::Mismatch
        },
    )
}

#[cfg(test)]
mod tests;
