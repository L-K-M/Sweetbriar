//! Import job reports in `recovery/` (spec/03, 09). A report is derived
//! from the job's rows, so it is written to a fresh temporary name,
//! flushed, renamed over the final name and the directory flushed:
//! readers see the old report or the new one, never a torn file.

use std::io::{self, Write};
use std::path::PathBuf;
use std::sync::Arc;

use sb_types::fs::{FileKind, Fs};
use sb_types::random::Random;

use crate::StoreError;

const PREFIX: &str = "import-report-";
const SUFFIX: &str = ".json";

/// Random bytes in a temporary name; only a broken seam collides twice.
const TEMP_BYTES: usize = 8;
const NAME_ATTEMPTS: usize = 8;

/// The longest job id a report is named after; ids are UUIDs.
const MAX_JOB_ID: usize = 64;

pub(crate) struct ImportReports {
    fs: Arc<dyn Fs>,
    random: Arc<dyn Random>,
    recovery: PathBuf,
}

impl ImportReports {
    pub(crate) fn new(fs: Arc<dyn Fs>, random: Arc<dyn Random>, recovery: PathBuf) -> Self {
        Self {
            fs,
            random,
            recovery,
        }
    }

    /// `recovery/import-report-<job>.json`. A job id is lowercase hex
    /// and hyphens, so it can never name a path outside `recovery/`.
    pub(crate) fn path(&self, job: &str) -> Result<PathBuf, StoreError> {
        let valid = !job.is_empty()
            && job.len() <= MAX_JOB_ID
            && job
                .bytes()
                .all(|b| matches!(b, b'0'..=b'9' | b'a'..=b'f' | b'-'));
        if !valid {
            return Err(StoreError::InvalidInput {
                message: format!("an import job id is lowercase hex and hyphens, not {job:?}"),
            });
        }
        Ok(self.recovery.join(format!("{PREFIX}{job}{SUFFIX}")))
    }

    pub(crate) fn exists(&self, job: &str) -> Result<bool, StoreError> {
        let path = self.path(job)?;
        let md = self.fs.metadata(&path).map_err(|e| {
            StoreError::unavailable(format!("cannot check the import report {job}: {e}"))
        })?;
        Ok(md.is_some_and(|md| md.kind() == FileKind::File))
    }

    /// Replace `job`'s report with `bytes` durably (spec/03). On failure
    /// the temporary file is removed, best-effort; the old report stays.
    pub(crate) fn write(&self, job: &str, bytes: &[u8]) -> Result<(), StoreError> {
        let path = self.path(job)?;
        let failed = |e: io::Error| {
            StoreError::unavailable(format!("cannot write the import report {job}: {e}"))
        };
        let (temp, mut file) = self.create_temp(job)?;
        let written = file
            .write_all(bytes)
            .and_then(|()| file.sync_all())
            .and_then(|()| {
                drop(file);
                self.fs.rename(&temp, &path)
            });
        if let Err(e) = written {
            let _ = self.fs.remove(&temp);
            return Err(failed(e));
        }
        self.fs.sync_dir(&self.recovery).map_err(failed)
    }

    fn create_temp(
        &self,
        job: &str,
    ) -> Result<(PathBuf, Box<dyn sb_types::fs::WriteFile>), StoreError> {
        for _ in 0..NAME_ATTEMPTS {
            let mut name = [0u8; TEMP_BYTES];
            self.random.fill(&mut name).map_err(|e| {
                StoreError::unavailable(format!("cannot name the import report {job}: {e}"))
            })?;
            let suffix: String = name.iter().map(|b| format!("{b:02x}")).collect();
            let temp = self
                .recovery
                .join(format!("{PREFIX}{job}{SUFFIX}.tmp-{suffix}"));
            match self.fs.create(&temp) {
                Ok(file) => return Ok((temp, file)),
                Err(e) if e.kind() == io::ErrorKind::AlreadyExists => continue,
                Err(e) => {
                    return Err(StoreError::unavailable(format!(
                        "cannot create the import report {job}: {e}"
                    )));
                }
            }
        }
        Err(StoreError::unavailable(format!(
            "cannot create the import report {job}: no free name"
        )))
    }
}

#[cfg(test)]
mod tests;
