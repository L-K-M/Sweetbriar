//! Point-in-time external generation verification and retained handoff.
use super::*;
use sb_types::fs::{ConfinedDir, DirectoryIdentity, ReadFile};
use sha2::{Digest, Sha256};
use std::cell::RefCell;
use std::io::{Read, Seek, SeekFrom};
use std::ops::ControlFlow;
use std::sync::atomic::{AtomicBool, Ordering};

fn io_error(error: io::Error) -> StoreError {
    if error.kind() == io::ErrorKind::InvalidData {
        StoreError::corrupt(format!("invalid external manifest: {error}"))
    } else {
        StoreError::unavailable_io(format!("external backup I/O: {error}"), error.kind())
    }
}

struct HashReader<R> {
    input: R,
    hash: Sha256,
}
impl<R: Read> Read for HashReader<R> {
    fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
        let n = self.input.read(buf)?;
        self.hash.update(&buf[..n]);
        Ok(n)
    }
}

/// A retained generation capability verified at one point in time.
/// The source remains mutable: staging must reverify every byte it copies.
pub struct VerifiedExternalBackup {
    generation: String,
    pub(super) source: Box<dyn ConfinedDir>,
    pub(super) identity: DirectoryIdentity,
    pub(super) manifest_sha256: [u8; 32],
}
impl VerifiedExternalBackup {
    /// The selected generation's canonical identifier.
    pub fn generation_id(&self) -> &str {
        &self.generation
    }
    /// The retained source directory. Its contents can change after verification.
    pub fn source(&self) -> &dyn ConfinedDir {
        self.source.as_ref()
    }
    /// The verified directory's filesystem identity.
    pub fn identity(&self) -> DirectoryIdentity {
        self.identity
    }
    /// Digest of the exact manifest verified during selection.
    pub fn manifest_sha256(&self) -> [u8; 32] {
        self.manifest_sha256
    }
}

struct Prepared {
    file: Box<dyn ReadFile>,
    manifest: manifest::Manifest,
    digest: [u8; 32],
    count: u64,
    total: u64,
}

impl Backups {
    pub(crate) fn verify_external(
        &self,
        parent: &Path,
        generation: &str,
        cancel: &AtomicBool,
    ) -> Result<Option<VerifiedExternalBackup>, StoreError> {
        let parent = self.fs.open_confined_dir(parent).map_err(io_error)?;
        self.verify_external_in(parent.as_ref(), generation, cancel, || {})
    }

    pub(crate) fn discover_external(
        &self,
        parent: &Path,
        cancel: &AtomicBool,
        mut visitor: impl FnMut(VerifiedExternalBackup) -> ControlFlow<()>,
    ) -> Result<ControlFlow<()>, StoreError> {
        let parent = self.fs.open_confined_dir(parent).map_err(io_error)?;
        let mut failure = None;
        let result = parent
            .visit_children(&mut |name| {
                if cancel.load(Ordering::Acquire) {
                    return Ok(ControlFlow::Break(()));
                }
                let Some(name) = name.to_str() else {
                    return Ok(ControlFlow::Continue(()));
                };
                let Ok(id) = name.parse::<Uuid>() else {
                    return Ok(ControlFlow::Continue(()));
                };
                if id.to_string() != name {
                    return Ok(ControlFlow::Continue(()));
                }
                match self.verify_external_in(parent.as_ref(), name, cancel, || {}) {
                    Ok(Some(selection)) => Ok(visitor(selection)),
                    Ok(None) => Ok(ControlFlow::Break(())),
                    Err(StoreError::Corrupt { .. }) => Ok(ControlFlow::Continue(())),
                    Err(e)
                        if matches!(
                            e.io_kind(),
                            Some(
                                io::ErrorKind::NotFound
                                    | io::ErrorKind::InvalidInput
                                    | io::ErrorKind::NotADirectory
                            )
                        ) =>
                    {
                        Ok(ControlFlow::Continue(()))
                    }
                    Err(e) => {
                        failure = Some(e);
                        Ok(ControlFlow::Break(()))
                    }
                }
            })
            .map_err(io_error)?;
        if let Some(e) = failure {
            return Err(e);
        }
        Ok(result)
    }

    fn verify_external_in(
        &self,
        parent: &dyn ConfinedDir,
        generation: &str,
        cancel: &AtomicBool,
        after_prepare: impl FnOnce(),
    ) -> Result<Option<VerifiedExternalBackup>, StoreError> {
        let parsed: Uuid = generation
            .parse()
            .map_err(|_| StoreError::corrupt("invalid generation id"))?;
        if parsed.to_string() != generation {
            return Err(StoreError::corrupt("invalid generation id"));
        }
        let source = parent.open_dir(Path::new(generation)).map_err(io_error)?;
        let identity = source.identity().map_err(io_error)?;
        let prepared = RefCell::new(None::<Prepared>);
        let outcome = self.inspect_image_session(
            source.as_ref(),
            cancel,
            |_| ControlFlow::Continue(()),
            |_| {
                let (mut file, _) = source
                    .open_regular(Path::new("manifest.json"))
                    .map_err(io_error)?;
                let mut count = 0u64;
                let mut total = 0u64;
                let mut input = HashReader {
                    input: &mut file,
                    hash: Sha256::new(),
                };
                let manifest = manifest::parse(
                    &mut input,
                    || {
                        if cancel.load(Ordering::Acquire) {
                            Err(io::ErrorKind::Interrupted.into())
                        } else {
                            Ok(())
                        }
                    },
                    |entry| {
                        count = count.checked_add(1).ok_or_else(|| {
                            io::Error::new(io::ErrorKind::InvalidData, "object count overflow")
                        })?;
                        total = total.checked_add(entry.bytes).ok_or_else(|| {
                            io::Error::new(io::ErrorKind::InvalidData, "object bytes overflow")
                        })?;
                        Ok(())
                    },
                );
                if cancel.load(Ordering::Acquire) {
                    return Ok(([0; 32], 0));
                }
                let manifest = manifest.map_err(io_error)?;
                if reconciliation::uuid(manifest.generation_id) != generation {
                    return Err(StoreError::corrupt("manifest generation mismatch"));
                }
                total = total
                    .checked_add(manifest.database.bytes)
                    .ok_or_else(|| StoreError::corrupt("manifest total overflow"))?;
                let digest = input.hash.finalize().into();
                let image = (manifest.database.sha256, manifest.database.bytes);
                *prepared.borrow_mut() = Some(Prepared {
                    file,
                    manifest,
                    digest,
                    count,
                    total,
                });
                after_prepare();
                Ok(image)
            },
            |image, id, _| {
                let mut state = prepared.borrow_mut();
                let state = state.as_mut().expect("prepared manifest");
                state.file.seek(SeekFrom::Start(0)).map_err(io_error)?;
                let mut input = HashReader {
                    input: &mut state.file,
                    hash: Sha256::new(),
                };
                let expected = reconciliation::Expected {
                    database: manifest::Entry {
                        sha256: state.manifest.database.sha256,
                        bytes: state.manifest.database.bytes,
                    },
                    object_count: state.count,
                    total_bytes: state.total,
                };
                let store = reconciliation::uuid(state.manifest.store_uuid);
                let result = reconciliation::run_checked(
                    image,
                    self,
                    id,
                    &mut input,
                    expected,
                    cancel,
                    Some((generation, state.manifest.created_at, &store)),
                )?;
                if result.is_break() {
                    return Ok(result);
                }
                if input.hash.finalize().as_slice() != state.digest {
                    return Err(StoreError::corrupt("manifest changed during verification"));
                }
                if recorded::verify_objects(self, source.as_ref(), id, cancel, || {})?.is_break() {
                    return Ok(ControlFlow::Break(()));
                }
                let (mut current, _) = source
                    .open_regular(Path::new("manifest.json"))
                    .map_err(io_error)?;
                let mut hash = Sha256::new();
                let mut buf = [0; 65536];
                loop {
                    if cancel.load(Ordering::Acquire) {
                        return Ok(ControlFlow::Break(()));
                    }
                    let n = current.read(&mut buf).map_err(io_error)?;
                    if n == 0 {
                        break;
                    }
                    hash.update(&buf[..n]);
                }
                if hash.finalize().as_slice() != state.digest {
                    return Err(StoreError::corrupt(
                        "manifest path changed during verification",
                    ));
                }
                if parent
                    .open_dir(Path::new(generation))
                    .map_err(io_error)?
                    .identity()
                    .map_err(io_error)?
                    != identity
                {
                    return Err(StoreError::corrupt("backup generation directory changed"));
                }
                Ok(ControlFlow::Continue(()))
            },
        )?;
        match outcome {
            BackupImageInspection::Cancelled => Ok(None),
            BackupImageInspection::Inspected { .. } => Ok(Some(VerifiedExternalBackup {
                generation: generation.to_owned(),
                source,
                identity,
                manifest_sha256: prepared.into_inner().expect("prepared manifest").digest,
            })),
        }
    }

    #[cfg(test)]
    pub(super) fn verify_external_after_prepare(
        &self,
        parent: &Path,
        generation: &str,
        cancel: &AtomicBool,
        after_prepare: impl FnOnce(),
    ) -> Result<Option<VerifiedExternalBackup>, StoreError> {
        let parent = self.fs.open_confined_dir(parent).map_err(io_error)?;
        self.verify_external_in(parent.as_ref(), generation, cancel, after_prepare)
    }
}
