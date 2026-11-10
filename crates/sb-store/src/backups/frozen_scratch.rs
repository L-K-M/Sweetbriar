//! Verifier scratch inside a sound store's root can outlive a latch.
//! Guard each mutation, including writes through already-open files,
//! not the verification: source reads, callbacks and writer waits must
//! never hold a file step. Sibling scratch needs no such guard.

use std::io::{self, Write};
use std::path::Path;
use std::sync::Arc;

use sb_types::fs::{ConfinedDir, DirectoryIdentity, Metadata, ReadFile, WriteFile};

use crate::FileObjectStore;
use crate::integrity::FileStep;

pub(super) fn protect(
    inner: Box<dyn ConfinedDir>,
    objects: &Arc<FileObjectStore>,
) -> Box<dyn ConfinedDir> {
    Box::new(Scratch {
        inner,
        objects: objects.clone(),
    })
}

fn step(objects: &Arc<FileObjectStore>) -> io::Result<FileStep<'_>> {
    objects
        .latch()
        .step("write verifier scratch")
        .map_err(io::Error::other)
}

struct Scratch {
    inner: Box<dyn ConfinedDir>,
    objects: Arc<FileObjectStore>,
}

impl ConfinedDir for Scratch {
    fn identity(&self) -> io::Result<DirectoryIdentity> {
        self.inner.identity()
    }

    fn ownership(&self) -> io::Result<Vec<u8>> {
        self.inner.ownership()
    }

    fn set_ownership(&self, bytes: &[u8]) -> io::Result<()> {
        let _step = step(&self.objects)?;
        self.inner.set_ownership(bytes)
    }

    fn open_dir(&self, name: &Path) -> io::Result<Box<dyn ConfinedDir>> {
        self.inner
            .open_dir(name)
            .map(|inner| protect(inner, &self.objects))
    }

    fn create_private_dir(&self, name: &Path) -> io::Result<()> {
        let _step = step(&self.objects)?;
        self.inner.create_private_dir(name)
    }

    fn create(&self, name: &Path) -> io::Result<Box<dyn WriteFile>> {
        let _step = step(&self.objects)?;
        Ok(Box::new(ScratchFile {
            inner: self.inner.create(name)?,
            objects: self.objects.clone(),
        }))
    }

    fn remove_file(&self, name: &Path) -> io::Result<()> {
        let _step = step(&self.objects)?;
        self.inner.remove_file(name)
    }

    fn remove_empty_dir(&self, name: &Path) -> io::Result<()> {
        let _step = step(&self.objects)?;
        self.inner.remove_empty_dir(name)
    }

    fn sync(&self) -> io::Result<()> {
        let _step = step(&self.objects)?;
        self.inner.sync()
    }

    fn open_regular(&self, path: &Path) -> io::Result<(Box<dyn ReadFile>, Metadata)> {
        self.inner.open_regular(path)
    }
}

// Only the verifier's sequential write capability is exposed. The
// unused positional-write capability remains unsupported, not unguarded.
struct ScratchFile {
    inner: Box<dyn WriteFile>,
    objects: Arc<FileObjectStore>,
}

impl Write for ScratchFile {
    fn write(&mut self, bytes: &[u8]) -> io::Result<usize> {
        let _step = step(&self.objects)?;
        self.inner.write(bytes)
    }

    fn flush(&mut self) -> io::Result<()> {
        let _step = step(&self.objects)?;
        self.inner.flush()
    }
}

impl WriteFile for ScratchFile {
    fn sync_all(&mut self) -> io::Result<()> {
        let _step = step(&self.objects)?;
        self.inner.sync_all()
    }
}
