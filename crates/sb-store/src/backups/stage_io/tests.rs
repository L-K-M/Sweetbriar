//! Tests for the retained positioned-I/O adapter (M1.8e3b2b2). A
//! small deterministic trait adapter scripts every operation, and a
//! real-filesystem case proves identity and pathname replacement.

use std::collections::VecDeque;
use std::io;
use std::path::Path;
use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use rusqlite::ffi;
use sb_test_support::{FaultFs, FaultKind, FsOp, ScratchDir, SimFs, real_fs};
use sb_types::fs::{FileIdentity, Fs, ReadWriteFile};

use super::{end_offset, file_size, read, sync, truncate, write};

#[derive(Debug)]
enum ReadStep {
    Exact,
    Short(usize),
    Oversized(usize),
    Interrupted,
    Error(io::Error),
    Panic,
}

#[derive(Debug)]
enum WriteStep {
    Exact,
    Short(usize),
    Zero,
    Oversized(usize),
    Interrupted,
    Error(io::Error),
    Panic,
}

#[derive(Debug)]
enum TruncateStep {
    Error(io::Error),
    Panic,
}

#[derive(Debug)]
enum SyncStep {
    Error(io::Error),
    Panic,
}

#[derive(Debug)]
enum LenStep {
    Value(u64),
    Error(io::Error),
    Panic,
}

/// A deterministic [`ReadWriteFile`]: an in-memory byte vector plus a
/// scripted response for each operation. An empty script means the
/// exact, successful behavior.
struct Scripted {
    bytes: Mutex<Vec<u8>>,
    reads: Mutex<VecDeque<ReadStep>>,
    writes: Mutex<VecDeque<WriteStep>>,
    truncates: Mutex<VecDeque<TruncateStep>>,
    syncs: Mutex<VecDeque<SyncStep>>,
    lens: Mutex<VecDeque<LenStep>>,
    sync_count: AtomicUsize,
    identity: FileIdentity,
}

impl Scripted {
    fn new(bytes: &[u8]) -> Self {
        Self {
            bytes: Mutex::new(bytes.to_vec()),
            reads: Mutex::new(VecDeque::new()),
            writes: Mutex::new(VecDeque::new()),
            truncates: Mutex::new(VecDeque::new()),
            syncs: Mutex::new(VecDeque::new()),
            lens: Mutex::new(VecDeque::new()),
            sync_count: AtomicUsize::new(0),
            identity: FileIdentity {
                device: 1,
                inode: 2,
            },
        }
    }

    fn on_read(&self, step: ReadStep) {
        self.reads.lock().expect("script lock").push_back(step);
    }

    fn on_write(&self, step: WriteStep) {
        self.writes.lock().expect("script lock").push_back(step);
    }

    fn on_truncate(&self, step: TruncateStep) {
        self.truncates.lock().expect("script lock").push_back(step);
    }

    fn on_sync(&self, step: SyncStep) {
        self.syncs.lock().expect("script lock").push_back(step);
    }

    fn on_len(&self, step: LenStep) {
        self.lens.lock().expect("script lock").push_back(step);
    }

    fn sync_count(&self) -> usize {
        self.sync_count.load(Ordering::SeqCst)
    }

    fn write_exact(&self, bytes: &[u8], offset: u64) -> io::Result<()> {
        let start = usize::try_from(offset).map_err(|_| io::ErrorKind::InvalidInput)?;
        let end = start
            .checked_add(bytes.len())
            .ok_or(io::ErrorKind::InvalidInput)?;
        let mut source = self.bytes.lock().expect("script lock");
        if end > source.len() {
            source.resize(end, 0);
        }
        source[start..end].copy_from_slice(bytes);
        Ok(())
    }
}

impl ReadWriteFile for Scripted {
    fn read_at(&self, bytes: &mut [u8], offset: u64) -> io::Result<usize> {
        let step = self
            .reads
            .lock()
            .expect("script lock")
            .pop_front()
            .unwrap_or(ReadStep::Exact);
        match step {
            ReadStep::Oversized(n) => Ok(n),
            ReadStep::Interrupted => Err(io::Error::from(io::ErrorKind::Interrupted)),
            ReadStep::Error(error) => Err(error),
            ReadStep::Panic => panic!("injected read panic"),
            ReadStep::Exact | ReadStep::Short(_) => {
                let start = usize::try_from(offset).map_err(|_| io::ErrorKind::InvalidInput)?;
                let limit = match step {
                    ReadStep::Short(n) => n,
                    _ => usize::MAX,
                };
                let source = self.bytes.lock().expect("script lock");
                let available = source.len().saturating_sub(start);
                let n = available.min(bytes.len()).min(limit);
                bytes[..n].copy_from_slice(&source[start..start + n]);
                Ok(n)
            }
        }
    }

    fn write_at(&self, bytes: &[u8], offset: u64) -> io::Result<usize> {
        let step = self
            .writes
            .lock()
            .expect("script lock")
            .pop_front()
            .unwrap_or(WriteStep::Exact);
        match step {
            WriteStep::Zero => Ok(0),
            WriteStep::Oversized(n) => Ok(n),
            WriteStep::Interrupted => Err(io::Error::from(io::ErrorKind::Interrupted)),
            WriteStep::Error(error) => Err(error),
            WriteStep::Panic => panic!("injected write panic"),
            WriteStep::Exact | WriteStep::Short(_) => {
                let n = match step {
                    WriteStep::Short(n) => bytes.len().min(n),
                    _ => bytes.len(),
                };
                self.write_exact(&bytes[..n], offset)?;
                Ok(n)
            }
        }
    }

    fn set_len(&self, len: u64) -> io::Result<()> {
        match self.truncates.lock().expect("script lock").pop_front() {
            Some(TruncateStep::Error(error)) => Err(error),
            Some(TruncateStep::Panic) => panic!("injected truncate panic"),
            None => {
                let len = usize::try_from(len).map_err(|_| io::ErrorKind::InvalidInput)?;
                self.bytes.lock().expect("script lock").resize(len, 0);
                Ok(())
            }
        }
    }

    fn len(&self) -> io::Result<u64> {
        match self.lens.lock().expect("script lock").pop_front() {
            Some(LenStep::Value(len)) => Ok(len),
            Some(LenStep::Error(error)) => Err(error),
            Some(LenStep::Panic) => panic!("injected len panic"),
            None => Ok(self.bytes.lock().expect("script lock").len() as u64),
        }
    }

    fn sync_all(&self) -> io::Result<()> {
        self.sync_count.fetch_add(1, Ordering::SeqCst);
        match self.syncs.lock().expect("script lock").pop_front() {
            Some(SyncStep::Error(error)) => Err(error),
            Some(SyncStep::Panic) => panic!("injected sync panic"),
            None => Ok(()),
        }
    }

    fn identity(&self) -> io::Result<FileIdentity> {
        Ok(self.identity)
    }
}

#[test]
fn independent_bytes_at_offsets_round_trip() {
    let file = Scripted::new(b"");
    assert_eq!(write(&file, b"a", 0), ffi::SQLITE_OK);
    assert_eq!(write(&file, b"bc", 5), ffi::SQLITE_OK);
    assert_eq!(write(&file, b"def", 100), ffi::SQLITE_OK);
    assert_eq!(file_size(&file), Ok(103));
    let mut bytes = [0xFF; 103];
    assert_eq!(read(&file, &mut bytes, 0), ffi::SQLITE_OK);
    assert_eq!(&bytes[..1], b"a");
    assert!(bytes[1..5].iter().all(|&b| b == 0));
    assert_eq!(&bytes[5..7], b"bc");
    assert!(bytes[7..100].iter().all(|&b| b == 0));
    assert_eq!(&bytes[100..103], b"def");
}

#[test]
fn sparse_growth_and_truncate_round_trip() {
    let file = Scripted::new(b"");
    assert_eq!(write(&file, b"xy", 4), ffi::SQLITE_OK);
    assert_eq!(file_size(&file), Ok(6));
    let mut bytes = [0xFF; 6];
    assert_eq!(read(&file, &mut bytes, 0), ffi::SQLITE_OK);
    assert_eq!(&bytes, b"\0\0\0\0xy");

    assert_eq!(truncate(&file, 2), ffi::SQLITE_OK);
    assert_eq!(file_size(&file), Ok(2));
    let mut short = [0xFF; 6];
    assert_eq!(read(&file, &mut short, 0), ffi::SQLITE_IOERR_SHORT_READ);
    assert_eq!(&short[..2], b"\0\0");
    assert!(short[2..].iter().all(|&b| b == 0));

    assert_eq!(truncate(&file, 6), ffi::SQLITE_OK);
    let mut grown = [0xFF; 6];
    assert_eq!(read(&file, &mut grown, 0), ffi::SQLITE_OK);
    assert!(grown.iter().all(|&b| b == 0));
}

#[test]
fn short_reads_are_retried() {
    let file = Scripted::new(b"abcdefgh");
    file.on_read(ReadStep::Short(3));
    let mut bytes = [0; 8];
    assert_eq!(read(&file, &mut bytes, 0), ffi::SQLITE_OK);
    assert_eq!(&bytes, b"abcdefgh");
}

#[test]
fn interrupted_reads_are_retried() {
    let file = Scripted::new(b"abcdefgh");
    file.on_read(ReadStep::Interrupted);
    file.on_read(ReadStep::Interrupted);
    let mut bytes = [0; 8];
    assert_eq!(read(&file, &mut bytes, 0), ffi::SQLITE_OK);
    assert_eq!(&bytes, b"abcdefgh");
}

#[test]
fn short_writes_are_retried() {
    let file = Scripted::new(b"");
    file.on_write(WriteStep::Short(2));
    file.on_write(WriteStep::Short(2));
    assert_eq!(write(&file, b"abcdef", 0), ffi::SQLITE_OK);
    let mut bytes = [0; 6];
    assert_eq!(read(&file, &mut bytes, 0), ffi::SQLITE_OK);
    assert_eq!(&bytes, b"abcdef");
}

#[test]
fn interrupted_writes_are_retried() {
    let file = Scripted::new(b"");
    file.on_write(WriteStep::Interrupted);
    assert_eq!(write(&file, b"xy", 0), ffi::SQLITE_OK);
    assert_eq!(file_size(&file), Ok(2));
}

#[test]
fn eof_zero_fills_the_remainder() {
    let file = Scripted::new(b"abcd");
    let mut bytes = [0xFF; 8];
    assert_eq!(read(&file, &mut bytes, 0), ffi::SQLITE_IOERR_SHORT_READ);
    assert_eq!(&bytes[..4], b"abcd");
    assert!(bytes[4..].iter().all(|&b| b == 0));

    let mut tail = [0xFF; 4];
    assert_eq!(read(&file, &mut tail, 4), ffi::SQLITE_IOERR_SHORT_READ);
    assert!(tail.iter().all(|&b| b == 0));
}

#[test]
fn short_read_then_eof_zero_fills() {
    let file = Scripted::new(b"abc");
    file.on_read(ReadStep::Short(2));
    let mut bytes = [0xFF; 6];
    assert_eq!(read(&file, &mut bytes, 0), ffi::SQLITE_IOERR_SHORT_READ);
    assert_eq!(&bytes[..3], b"abc");
    assert!(bytes[3..].iter().all(|&b| b == 0));
}

#[test]
fn read_errors_and_oversized_counts_zero_the_tail() {
    let file = Scripted::new(b"abcdefgh");
    file.on_read(ReadStep::Short(3));
    file.on_read(ReadStep::Error(io::Error::from(io::ErrorKind::Other)));
    let mut bytes = [0xFF; 8];
    assert_eq!(read(&file, &mut bytes, 0), ffi::SQLITE_IOERR_READ);
    assert_eq!(&bytes[..3], b"abc");
    assert!(bytes[3..].iter().all(|&b| b == 0));

    let file = Scripted::new(b"abc");
    file.on_read(ReadStep::Oversized(10));
    let mut bytes = [0xFF; 3];
    assert_eq!(read(&file, &mut bytes, 0), ffi::SQLITE_IOERR_READ);
    assert!(bytes.iter().all(|&b| b == 0));
}

#[test]
fn read_panic_after_partial_progress_is_contained() {
    let file = Scripted::new(b"abcdefgh");
    file.on_read(ReadStep::Short(3));
    file.on_read(ReadStep::Panic);
    let mut bytes = [0xFF; 8];
    assert_eq!(read(&file, &mut bytes, 0), ffi::SQLITE_IOERR_READ);
    assert_eq!(&bytes[..3], b"abc");
    assert!(bytes[3..].iter().all(|&b| b == 0));
}

#[test]
fn zero_and_oversized_writes_fail() {
    let file = Scripted::new(b"");
    file.on_write(WriteStep::Zero);
    assert_eq!(write(&file, b"abc", 0), ffi::SQLITE_IOERR_WRITE);

    let file = Scripted::new(b"");
    file.on_write(WriteStep::Oversized(10));
    assert_eq!(write(&file, b"abc", 0), ffi::SQLITE_IOERR_WRITE);
    assert_eq!(file_size(&file), Ok(0));
}

#[test]
fn write_errors_and_panics_map_to_ioerr_write() {
    let file = Scripted::new(b"");
    file.on_write(WriteStep::Error(io::Error::from(io::ErrorKind::Other)));
    assert_eq!(write(&file, b"abc", 0), ffi::SQLITE_IOERR_WRITE);

    let file = Scripted::new(b"");
    file.on_write(WriteStep::Panic);
    assert_eq!(write(&file, b"abc", 0), ffi::SQLITE_IOERR_WRITE);
}

#[test]
fn write_panic_after_partial_progress_is_contained() {
    let file = Scripted::new(b"");
    file.on_write(WriteStep::Short(3));
    file.on_write(WriteStep::Panic);
    assert_eq!(write(&file, b"abcdefgh", 0), ffi::SQLITE_IOERR_WRITE);
    let mut bytes = [0; 3];
    assert_eq!(read(&file, &mut bytes, 0), ffi::SQLITE_OK);
    assert_eq!(&bytes, b"abc");
}

#[test]
fn storage_full_without_a_platform_errno_is_still_full() {
    let file = Scripted::new(b"");
    file.on_write(WriteStep::Error(io::Error::from(
        io::ErrorKind::StorageFull,
    )));
    assert_eq!(write(&file, b"data", 0), ffi::SQLITE_FULL);
}

#[test]
fn native_storage_full_maps_to_sqlite_full() {
    let inner = SimFs::new();
    inner.create_dir_all(Path::new("/x")).unwrap();
    let fs = FaultFs::new(
        std::sync::Arc::new(inner),
        FsOp::Write,
        1,
        FaultKind::NoSpace,
    );
    let root = fs.open_confined_dir(Path::new("/x")).unwrap();
    let file = root.create_read_write(Path::new("db")).unwrap();
    assert_eq!(write(file.as_ref(), b"data", 0), ffi::SQLITE_FULL);
}

#[test]
fn truncate_failures_map_to_ioerr_truncate() {
    let file = Scripted::new(b"abcdef");
    file.on_truncate(TruncateStep::Error(io::Error::from(io::ErrorKind::Other)));
    assert_eq!(truncate(&file, 3), ffi::SQLITE_IOERR_TRUNCATE);
    assert_eq!(file_size(&file), Ok(6));

    let file = Scripted::new(b"abcdef");
    file.on_truncate(TruncateStep::Error(io::Error::from(
        io::ErrorKind::StorageFull,
    )));
    assert_eq!(truncate(&file, 3), ffi::SQLITE_IOERR_TRUNCATE);

    let file = Scripted::new(b"abcdef");
    file.on_truncate(TruncateStep::Panic);
    assert_eq!(truncate(&file, 3), ffi::SQLITE_IOERR_TRUNCATE);
}

#[test]
fn sync_always_calls_retained_sync_all() {
    let file = Scripted::new(b"abc");
    assert_eq!(sync(&file), ffi::SQLITE_OK);
    assert_eq!(file.sync_count(), 1);

    let file = Scripted::new(b"abc");
    file.on_sync(SyncStep::Error(io::Error::from(io::ErrorKind::Other)));
    assert_eq!(sync(&file), ffi::SQLITE_IOERR_FSYNC);
    assert_eq!(file.sync_count(), 1);

    let file = Scripted::new(b"abc");
    file.on_sync(SyncStep::Panic);
    assert_eq!(sync(&file), ffi::SQLITE_IOERR_FSYNC);
    assert_eq!(file.sync_count(), 1);
}

#[test]
fn other_operations_never_flush() {
    let file = Scripted::new(b"abc");
    let mut bytes = [0; 3];
    assert_eq!(read(&file, &mut bytes, 0), ffi::SQLITE_OK);
    assert_eq!(write(&file, b"x", 0), ffi::SQLITE_OK);
    assert_eq!(truncate(&file, 2), ffi::SQLITE_OK);
    assert_eq!(file_size(&file), Ok(2));
    assert_eq!(file.sync_count(), 0);
}

#[test]
fn file_size_rejects_overflow_and_failures() {
    let file = Scripted::new(b"abc");
    assert_eq!(file_size(&file), Ok(3));

    let file = Scripted::new(b"");
    file.on_len(LenStep::Value(i64::MAX as u64 + 1));
    assert_eq!(file_size(&file), Err(ffi::SQLITE_IOERR_FSTAT));

    let file = Scripted::new(b"");
    file.on_len(LenStep::Value(u64::MAX));
    assert_eq!(file_size(&file), Err(ffi::SQLITE_IOERR_FSTAT));

    let file = Scripted::new(b"");
    file.on_len(LenStep::Error(io::Error::from(io::ErrorKind::Other)));
    assert_eq!(file_size(&file), Err(ffi::SQLITE_IOERR_FSTAT));

    let file = Scripted::new(b"");
    file.on_len(LenStep::Panic);
    assert_eq!(file_size(&file), Err(ffi::SQLITE_IOERR_FSTAT));
}

#[test]
fn negative_offsets_and_overflow_are_rejected() {
    let file = Scripted::new(b"abc");
    let mut bytes = [0; 1];
    assert_eq!(read(&file, &mut bytes, -1), ffi::SQLITE_IOERR_READ);
    assert_eq!(write(&file, b"x", -1), ffi::SQLITE_IOERR_WRITE);
    assert_eq!(truncate(&file, -1), ffi::SQLITE_IOERR_TRUNCATE);

    let file = Scripted::new(b"");
    assert_eq!(read(&file, &mut bytes, i64::MAX), ffi::SQLITE_IOERR_READ);
    assert_eq!(write(&file, b"x", i64::MAX), ffi::SQLITE_IOERR_WRITE);
    assert_eq!(read(&file, &mut [], i64::MAX), ffi::SQLITE_OK);
    assert_eq!(write(&file, &[], i64::MAX), ffi::SQLITE_OK);

    assert_eq!(end_offset(i64::MAX, 0), Some(i64::MAX));
    assert_eq!(end_offset(i64::MAX, 1), None);
    assert_eq!(end_offset(0, usize::MAX), None);
    assert_eq!(end_offset(i64::MAX, usize::MAX), None);
    assert_eq!(end_offset(i64::MAX - 1, 1), Some(i64::MAX));
}

#[test]
fn no_small_record_cap() {
    let len = 1024 * 1024 + 1;
    let pattern: Vec<u8> = (0..len).map(|i| (i % 251) as u8).collect();
    let file = Scripted::new(b"");
    assert_eq!(write(&file, &pattern, 0), ffi::SQLITE_OK);
    assert_eq!(file_size(&file), Ok(len as i64));
    let mut actual = vec![0; len];
    assert_eq!(read(&file, &mut actual, 0), ffi::SQLITE_OK);
    assert_eq!(actual, pattern);
}

#[test]
fn retained_file_identity_and_native_pathname_replacement() {
    let scratch = ScratchDir::new("stage-io-retained");
    let fs = real_fs();
    let original = scratch.path().join("stage");
    std::fs::create_dir(&original).unwrap();
    let root = fs.open_confined_dir(&original).unwrap();
    let file = root.create_read_write(Path::new("state.sqlite")).unwrap();
    let file_id = file.identity().unwrap();
    let dir_id = root.identity().unwrap();

    assert_eq!(write(file.as_ref(), b"safe", 0), ffi::SQLITE_OK);
    assert_eq!(file_size(file.as_ref()), Ok(4));

    let moved = scratch.path().join("moved");
    std::fs::rename(&original, &moved).unwrap();
    std::fs::create_dir(&original).unwrap();
    std::fs::write(original.join("state.sqlite"), b"other root").unwrap();
    std::fs::rename(moved.join("state.sqlite"), moved.join("retained")).unwrap();
    std::fs::write(moved.join("state.sqlite"), b"other file").unwrap();

    assert_eq!(write(file.as_ref(), b"keep", 4), ffi::SQLITE_OK);
    assert_eq!(truncate(file.as_ref(), 8), ffi::SQLITE_OK);
    assert_eq!(sync(file.as_ref()), ffi::SQLITE_OK);
    assert_eq!(file_size(file.as_ref()), Ok(8));
    let mut bytes = [0; 8];
    assert_eq!(read(file.as_ref(), &mut bytes, 0), ffi::SQLITE_OK);
    assert_eq!(&bytes, b"safekeep");
    assert_eq!(file.identity().unwrap(), file_id);
    assert_eq!(root.identity().unwrap(), dir_id);
    assert_eq!(std::fs::read(moved.join("retained")).unwrap(), b"safekeep");
    assert_eq!(
        std::fs::read(moved.join("state.sqlite")).unwrap(),
        b"other file"
    );
    assert_eq!(
        std::fs::read(original.join("state.sqlite")).unwrap(),
        b"other root"
    );
}
