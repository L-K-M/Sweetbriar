//! Verified-copy prerequisite for M1.8e3b's private restore staging
//! (spec/03 "Restore" step 2): copy one regular file through retained
//! confined source and destination handles, bounded by [`CHUNK`], with
//! cancellation checked per chunk and the exact length and digest checked
//! at EOF. A failure leaves unqualified evidence behind. The staging
//! integration consumes these helpers; this module qualifies nothing.

use std::io::{self, Read};
use std::path::Path;
use std::sync::atomic::{AtomicBool, Ordering};

use sha2::{Digest, Sha256};

use crate::StoreError;

/// One bounded copy buffer: 64 KiB, as the publication protocol uses.
const CHUNK: usize = 64 * 1024;

pub(super) fn io_error(context: &str, error: io::Error) -> StoreError {
    if error.kind() == io::ErrorKind::InvalidData {
        StoreError::corrupt(format!(
            "invalid restore staging input while {context}: {error}"
        ))
    } else {
        StoreError::unavailable_io(
            format!("restore staging I/O while {context}: {error}"),
            error.kind(),
        )
    }
}

pub(super) fn cancelled() -> StoreError {
    StoreError::unavailable("restore staging cancelled")
}

/// Stream `from` below `source` into a fresh file `to` below
/// `dest_parent`, checking cancellation per chunk and verifying the
/// exact bytes against `expected_sha` and `expected_bytes`. Memory stays
/// bounded by [`CHUNK`]; a failure leaves unqualified evidence behind.
pub(super) fn copy_verified(
    source: &dyn sb_types::fs::ConfinedDir,
    from: &Path,
    dest_parent: &dyn sb_types::fs::ConfinedDir,
    to: &Path,
    expected_sha: [u8; 32],
    expected_bytes: u64,
    cancel: &AtomicBool,
) -> Result<(), StoreError> {
    let (mut input, _) = source
        .open_regular(from)
        .map_err(|error| io_error(&format!("reading {}", from.display()), error))?;
    let mut output = dest_parent
        .create(to)
        .map_err(|error| io_error(&format!("creating {}", to.display()), error))?;
    copy_stream(
        &mut *input,
        &mut *output,
        from,
        to,
        expected_sha,
        expected_bytes,
        cancel,
    )
}

/// The streaming half of [`copy_verified`], split out so a test can
/// drive it with a custom `Read` and cancel deterministically between
/// chunks. The outer function's behavior is unchanged: cancellation is
/// checked before every chunk, the length is checked before an excess
/// chunk is written, and the digest and exact length are checked at EOF.
fn copy_stream<R: Read + ?Sized, W: sb_types::fs::WriteFile + ?Sized>(
    input: &mut R,
    output: &mut W,
    from: &Path,
    to: &Path,
    expected_sha: [u8; 32],
    expected_bytes: u64,
    cancel: &AtomicBool,
) -> Result<(), StoreError> {
    let mut hash = Sha256::new();
    let mut length = 0u64;
    let mut buffer = [0u8; CHUNK];
    loop {
        if cancel.load(Ordering::Acquire) {
            return Err(cancelled());
        }
        let n = input
            .read(&mut buffer)
            .map_err(|error| io_error(&format!("reading {}", from.display()), error))?;
        if n == 0 {
            break;
        }
        length = length
            .checked_add(n as u64)
            .ok_or_else(|| StoreError::corrupt("restore staging length overflow"))?;
        if length > expected_bytes {
            return Err(StoreError::corrupt(
                "restore source is longer than its manifest declares",
            ));
        }
        output
            .write_all(&buffer[..n])
            .map_err(|error| io_error(&format!("writing {}", to.display()), error))?;
        hash.update(&buffer[..n]);
    }
    if length != expected_bytes || hash.finalize().as_slice() != expected_sha {
        return Err(StoreError::corrupt(
            "restore source digest or length does not match the manifest",
        ));
    }
    output
        .sync_all()
        .map_err(|error| io_error(&format!("flushing {}", to.display()), error))?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use std::io::{self, Read, Write};
    use std::path::{Path, PathBuf};
    use std::sync::Arc;
    use std::sync::atomic::{AtomicBool, Ordering};

    use sb_test_support::{FaultFs, FaultKind, FsOp, ScratchDir, real_fs};
    use sb_types::fs::{ConfinedDir, Fs};
    use sha2::{Digest, Sha256};

    use super::*;

    /// A fresh scratch root with empty `source/` and `dest/`
    /// directories over the real filesystem.
    struct Fixture {
        _scratch: ScratchDir,
        fs: Arc<dyn Fs>,
        source: PathBuf,
        dest: PathBuf,
    }

    impl Fixture {
        fn new(label: &str) -> Self {
            let scratch = ScratchDir::new(label);
            let fs = real_fs();
            let source = scratch.path().join("source");
            let dest = scratch.path().join("dest");
            fs.create_dir(&source).unwrap();
            fs.create_dir(&dest).unwrap();
            Self {
                _scratch: scratch,
                fs,
                source,
                dest,
            }
        }

        fn write_source(&self, name: &str, bytes: &[u8]) {
            let mut file = self.fs.create(&self.source.join(name)).unwrap();
            file.write_all(bytes).unwrap();
            file.sync_all().unwrap();
        }

        fn confined(&self, dir: &Path) -> Box<dyn ConfinedDir> {
            self.fs.open_confined_dir(dir).unwrap()
        }

        fn dest_len(&self, name: &str) -> u64 {
            std::fs::metadata(self.dest.join(name)).unwrap().len()
        }

        fn dest_bytes(&self, name: &str) -> Vec<u8> {
            std::fs::read(self.dest.join(name)).unwrap()
        }
    }

    fn sha256(bytes: &[u8]) -> [u8; 32] {
        Sha256::digest(bytes).into()
    }

    #[test]
    fn exact_empty_and_multi_chunk_copy() {
        let fix = Fixture::new("stage-copy-exact");
        let cancel = AtomicBool::new(false);

        let empty: Vec<u8> = Vec::new();
        fix.write_source("empty", &empty);
        let source = fix.confined(&fix.source);
        let dest = fix.confined(&fix.dest);
        copy_verified(
            source.as_ref(),
            Path::new("empty"),
            dest.as_ref(),
            Path::new("empty"),
            sha256(&empty),
            0,
            &cancel,
        )
        .unwrap();
        assert_eq!(fix.dest_bytes("empty"), empty);

        let content: Vec<u8> = (0..CHUNK * 3 + 17).map(|i| (i % 251) as u8).collect();
        fix.write_source("multi", &content);
        let source = fix.confined(&fix.source);
        let dest = fix.confined(&fix.dest);
        copy_verified(
            source.as_ref(),
            Path::new("multi"),
            dest.as_ref(),
            Path::new("multi"),
            sha256(&content),
            content.len() as u64,
            &cancel,
        )
        .unwrap();
        assert_eq!(fix.dest_bytes("multi"), content);
    }

    #[test]
    fn wrong_hash_truncation_and_growth_fail_without_excess_output() {
        let fix = Fixture::new("stage-copy-verdicts");
        let cancel = AtomicBool::new(false);
        let content: Vec<u8> = (0..CHUNK * 2 + 11).map(|i| (i % 253) as u8).collect();
        fix.write_source("file", &content);
        let source = fix.confined(&fix.source);
        let dest = fix.confined(&fix.dest);

        let err = copy_verified(
            source.as_ref(),
            Path::new("file"),
            dest.as_ref(),
            Path::new("wrong-hash"),
            [0u8; 32],
            content.len() as u64,
            &cancel,
        )
        .unwrap_err();
        assert!(matches!(err, StoreError::Corrupt { .. }), "{err:?}");
        assert_eq!(fix.dest_len("wrong-hash"), content.len() as u64);

        let err = copy_verified(
            source.as_ref(),
            Path::new("file"),
            dest.as_ref(),
            Path::new("truncated"),
            sha256(&content),
            content.len() as u64 + 1,
            &cancel,
        )
        .unwrap_err();
        assert!(matches!(err, StoreError::Corrupt { .. }), "{err:?}");
        assert_eq!(fix.dest_len("truncated"), content.len() as u64);

        let err = copy_verified(
            source.as_ref(),
            Path::new("file"),
            dest.as_ref(),
            Path::new("growth"),
            sha256(&content[..CHUNK]),
            CHUNK as u64,
            &cancel,
        )
        .unwrap_err();
        assert!(matches!(err, StoreError::Corrupt { .. }), "{err:?}");
        assert_eq!(fix.dest_len("growth"), CHUNK as u64);
    }

    #[test]
    fn source_and_destination_symlinks_and_existing_destination_are_refused() {
        use std::os::unix::fs::symlink;

        let fix = Fixture::new("stage-copy-symlinks");
        let cancel = AtomicBool::new(false);
        let target = fix._scratch.path().join("target");
        std::fs::write(&target, b"through the link").unwrap();
        symlink(&target, fix.source.join("source-link")).unwrap();
        symlink(&target, fix.dest.join("dest-link")).unwrap();
        fix.write_source("real", b"real bytes");
        std::fs::write(fix.dest.join("taken"), b"old contents").unwrap();

        let source = fix.confined(&fix.source);
        let dest = fix.confined(&fix.dest);

        let err = copy_verified(
            source.as_ref(),
            Path::new("source-link"),
            dest.as_ref(),
            Path::new("out"),
            sha256(b"through the link"),
            15,
            &cancel,
        )
        .unwrap_err();
        assert!(matches!(err, StoreError::Unavailable { .. }), "{err:?}");
        assert!(!fix.dest.join("out").exists());

        let err = copy_verified(
            source.as_ref(),
            Path::new("real"),
            dest.as_ref(),
            Path::new("dest-link"),
            sha256(b"real bytes"),
            10,
            &cancel,
        )
        .unwrap_err();
        assert!(matches!(err, StoreError::Unavailable { .. }), "{err:?}");
        assert_eq!(
            std::fs::read(&target).unwrap(),
            b"through the link".to_vec()
        );

        let err = copy_verified(
            source.as_ref(),
            Path::new("real"),
            dest.as_ref(),
            Path::new("taken"),
            sha256(b"real bytes"),
            10,
            &cancel,
        )
        .unwrap_err();
        assert!(matches!(err, StoreError::Unavailable { .. }), "{err:?}");
        assert_eq!(
            std::fs::read(fix.dest.join("taken")).unwrap(),
            b"old contents".to_vec()
        );
    }

    #[test]
    fn pre_cancellation_fails_before_any_read() {
        let fix = Fixture::new("stage-copy-pre-cancel");
        fix.write_source("file", b"payload");
        let source = fix.confined(&fix.source);
        let dest = fix.confined(&fix.dest);
        let cancel = AtomicBool::new(true);
        let err = copy_verified(
            source.as_ref(),
            Path::new("file"),
            dest.as_ref(),
            Path::new("out"),
            sha256(b"payload"),
            7,
            &cancel,
        )
        .unwrap_err();
        assert!(
            matches!(err, StoreError::Unavailable { io_kind: None, .. }),
            "{err:?}"
        );
        assert_eq!(fix.dest_len("out"), 0);
    }

    #[test]
    fn read_write_and_flush_faults_are_reached() {
        // The read fault: the first chunk read fails, leaving an empty
        // destination file.
        let fix = Fixture::new("stage-copy-read-fault");
        let content = vec![7u8; CHUNK + 3];
        fix.write_source("file", &content);
        let fault = FaultFs::new(fix.fs.clone(), FsOp::Read, 1, FaultKind::Eio);
        let source = fault.open_confined_dir(&fix.source).unwrap();
        let dest = fault.open_confined_dir(&fix.dest).unwrap();
        let cancel = AtomicBool::new(false);
        let err = copy_verified(
            source.as_ref(),
            Path::new("file"),
            dest.as_ref(),
            Path::new("out"),
            sha256(&content),
            content.len() as u64,
            &cancel,
        )
        .unwrap_err();
        assert!(matches!(err, StoreError::Unavailable { .. }), "{err:?}");
        assert!(err.io_kind().is_some());
        assert_eq!(fault.calls(FsOp::Read), 1, "the read fault was reached");
        assert_eq!(fix.dest_len("out"), 0);

        // The write fault: the first chunk is half-written, then fails.
        let fix = Fixture::new("stage-copy-write-fault");
        let content = vec![9u8; CHUNK + 3];
        fix.write_source("file", &content);
        let fault = FaultFs::new(fix.fs.clone(), FsOp::Write, 1, FaultKind::ShortWrite);
        let source = fault.open_confined_dir(&fix.source).unwrap();
        let dest = fault.open_confined_dir(&fix.dest).unwrap();
        let cancel = AtomicBool::new(false);
        let err = copy_verified(
            source.as_ref(),
            Path::new("file"),
            dest.as_ref(),
            Path::new("out"),
            sha256(&content),
            content.len() as u64,
            &cancel,
        )
        .unwrap_err();
        assert!(matches!(err, StoreError::Unavailable { .. }), "{err:?}");
        assert!(err.io_kind().is_some());
        assert_eq!(fault.calls(FsOp::Write), 1, "the write fault was reached");
        assert_eq!(fix.dest_len("out"), CHUNK as u64 / 2);

        // The flush fault: the bytes land, the file fsync fails.
        let fix = Fixture::new("stage-copy-flush-fault");
        let content = b"small payload".to_vec();
        fix.write_source("file", &content);
        let fault = FaultFs::new(fix.fs.clone(), FsOp::SyncFile, 1, FaultKind::Eio);
        let source = fault.open_confined_dir(&fix.source).unwrap();
        let dest = fault.open_confined_dir(&fix.dest).unwrap();
        let cancel = AtomicBool::new(false);
        let err = copy_verified(
            source.as_ref(),
            Path::new("file"),
            dest.as_ref(),
            Path::new("out"),
            sha256(&content),
            content.len() as u64,
            &cancel,
        )
        .unwrap_err();
        assert!(matches!(err, StoreError::Unavailable { .. }), "{err:?}");
        assert!(err.io_kind().is_some());
        assert_eq!(
            fault.calls(FsOp::SyncFile),
            1,
            "the flush fault was reached"
        );
        assert_eq!(fix.dest_bytes("out"), content);
    }

    #[test]
    fn cancellation_is_checked_between_chunks() {
        struct CancelAfterFirstPiece {
            content: Vec<u8>,
            pos: usize,
            cancel: Arc<AtomicBool>,
        }

        impl Read for CancelAfterFirstPiece {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if self.pos >= self.content.len() {
                    return Ok(0);
                }
                let n = 1000.min(buf.len()).min(self.content.len() - self.pos);
                buf[..n].copy_from_slice(&self.content[self.pos..self.pos + n]);
                self.pos += n;
                self.cancel.store(true, Ordering::Release);
                Ok(n)
            }
        }

        let fix = Fixture::new("stage-copy-midchunk-cancel");
        let dest = fix.confined(&fix.dest);
        let mut output = dest.create(Path::new("out")).unwrap();
        let cancel = Arc::new(AtomicBool::new(false));
        let mut reader = CancelAfterFirstPiece {
            content: vec![5u8; CHUNK * 2],
            pos: 0,
            cancel: Arc::clone(&cancel),
        };
        let err = copy_stream(
            &mut reader,
            &mut *output,
            Path::new("from"),
            Path::new("to"),
            [0u8; 32],
            CHUNK as u64 * 2,
            cancel.as_ref(),
        )
        .unwrap_err();
        assert!(
            matches!(err, StoreError::Unavailable { io_kind: None, .. }),
            "{err:?}"
        );
        drop(output);
        assert_eq!(fix.dest_len("out"), 1000);
    }
}
