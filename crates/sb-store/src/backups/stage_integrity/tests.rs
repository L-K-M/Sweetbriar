use std::io::{self, Read, Seek, SeekFrom};
use std::path::PathBuf;
use std::sync::{
    Arc,
    atomic::{AtomicU64, AtomicUsize},
};

use rusqlite::Connection;
use sb_test_support::{ScratchDir, real_fs};

use super::*;
use crate::testing::{scratch_paths, test_seams};

mod corruption;

struct Image {
    _scratch: ScratchDir,
    path: PathBuf,
}
impl Image {
    fn new() -> Self {
        let (scratch, paths) = scratch_paths("stage-integrity");
        crate::open(&paths, &test_seams(real_fs()))
            .unwrap()
            .close()
            .unwrap();
        Self {
            _scratch: scratch,
            path: paths.state_db().to_owned(),
        }
    }
    fn sql(&self, sql: &str) {
        Connection::open(&self.path)
            .unwrap()
            .execute_batch(sql)
            .unwrap();
    }
    fn inspect(&self) -> Result<Option<Inspection>, StoreError> {
        inspect(
            Box::new(std::fs::File::open(&self.path).unwrap()),
            &test_seams(real_fs()),
            &AtomicBool::new(false),
        )
    }
    fn large(&self) {
        let request = format!("{{\"body\":\"{}\"}}", "x".repeat(2_000_000));
        let conn = Connection::open(&self.path).unwrap();
        assert_eq!(conn.execute("INSERT INTO import_jobs(job_id,state,request_json,checkpoint_json,revision,created_at,updated_at) VALUES('large-request','failed',?1,?1,1,0,0)", [&request]).unwrap(), 1);
        conn.close().unwrap();
    }
}

struct Counted {
    file: std::fs::File,
    bytes: Arc<AtomicU64>,
}
impl Read for Counted {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        let read = self.file.read(buffer)?;
        self.bytes.fetch_add(read as u64, Ordering::SeqCst);
        Ok(read)
    }
}
impl Seek for Counted {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.file.seek(to)
    }
}

fn catalog_bytes(image: &Image) -> u64 {
    let bytes = Arc::new(AtomicU64::new(0));
    assert!(
        stage_canonical::inspect(
            Box::new(Counted {
                file: std::fs::File::open(&image.path).unwrap(),
                bytes: bytes.clone(),
            }),
            &test_seams(real_fs()),
            &AtomicBool::new(false)
        )
        .unwrap()
        .is_some()
    );
    bytes.load(Ordering::SeqCst)
}

#[test]
fn genuine_current_images_and_large_values_pass_without_writes() {
    for large in [false, true] {
        let image = Image::new();
        if large {
            image.large();
        }
        let before = std::fs::read(&image.path).unwrap();
        let checked = image.inspect().unwrap().unwrap();
        assert_eq!(
            checked.schema_version,
            i64::from(crate::schema::LATEST_SCHEMA_VERSION)
        );
        assert_eq!(checked.file_bytes, before.len() as u64);
        assert_eq!(std::fs::read(&image.path).unwrap(), before);
        image.sql("VACUUM");
        assert!(image.inspect().unwrap().is_some());
    }
}

#[test]
fn full_scan_lifts_catalog_value_limits_and_reads_the_archive_payload() {
    let image = Image::new();
    image.large();
    let catalog = catalog_bytes(&image);
    assert!(catalog < 1_000_000);
    let bytes = Arc::new(AtomicU64::new(0));
    assert!(
        inspect(
            Box::new(Counted {
                file: std::fs::File::open(&image.path).unwrap(),
                bytes: bytes.clone(),
            }),
            &test_seams(real_fs()),
            &AtomicBool::new(false)
        )
        .unwrap()
        .is_some()
    );
    assert!(bytes.load(Ordering::SeqCst) > 2_000_000);
    assert!(bytes.load(Ordering::SeqCst) > catalog);
}

#[test]
fn independent_integrity_input_and_work_limits_actually_fire() {
    let image = Image::new();
    image.large();
    let bytes = Arc::new(AtomicU64::new(0));
    let phase_start = AtomicU64::new(0);
    let cancel = AtomicBool::new(false);
    let result = stage_canonical::inspect_then(
        Box::new(Counted {
            file: std::fs::File::open(&image.path).unwrap(),
            bytes: bytes.clone(),
        }),
        &test_seams(real_fs()),
        &cancel,
        4096,
        |connection| {
            phase_start.store(bytes.load(Ordering::SeqCst), Ordering::SeqCst);
            guarded(connection, &cancel, u32::MAX, check)
        },
    );
    assert!(
        matches!(result, Err(StoreError::Corrupt { ref detail }) if detail == "restore integrity input budget exceeded"),
        "{result:?}"
    );
    assert!(phase_start.load(Ordering::SeqCst) > 0);
    assert_eq!(
        bytes.load(Ordering::SeqCst) - phase_start.load(Ordering::SeqCst),
        4096
    );
    let result = inspect_with_budgets(
        Box::new(std::fs::File::open(&image.path).unwrap()),
        &test_seams(real_fs()),
        &AtomicBool::new(false),
        Some(Budgets {
            input: u64::MAX,
            work: 1,
        }),
    );
    assert!(
        matches!(result, Err(StoreError::Corrupt { ref detail }) if detail == "verifier SQLite work limit exceeded"),
        "{result:?}"
    );
}

#[derive(Clone, Copy)]
enum Event {
    Cancel,
    Io,
    CancelIo,
    Panic,
}
struct Watched {
    file: std::fs::File,
    active: Arc<AtomicBool>,
    event: Event,
    cancel: Arc<AtomicBool>,
    fired: Arc<AtomicBool>,
    drops: Arc<AtomicUsize>,
}
impl Read for Watched {
    fn read(&mut self, buffer: &mut [u8]) -> io::Result<usize> {
        if self.active.load(Ordering::Acquire) {
            self.fired.store(true, Ordering::Release);
            if matches!(self.event, Event::Panic) {
                panic!("injected integrity read panic");
            }
            if matches!(self.event, Event::Cancel | Event::CancelIo) {
                self.cancel.store(true, Ordering::Release);
            }
            if matches!(self.event, Event::Io | Event::CancelIo) {
                return Err(io::ErrorKind::PermissionDenied.into());
            }
        }
        self.file.read(buffer)
    }
}
impl Seek for Watched {
    fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
        self.file.seek(to)
    }
}
impl Drop for Watched {
    fn drop(&mut self) {
        self.drops.fetch_add(1, Ordering::SeqCst);
    }
}

#[test]
fn faults_and_cancellation_after_catalog_return_no_observation_and_close_once() {
    let image = Image::new();
    image.large();
    let budgets = Budgets::for_size(std::fs::metadata(&image.path).unwrap().len()).unwrap();
    for event in [Event::Cancel, Event::Io, Event::CancelIo, Event::Panic] {
        let cancel = Arc::new(AtomicBool::new(false));
        let fired = Arc::new(AtomicBool::new(false));
        let drops = Arc::new(AtomicUsize::new(0));
        let closed = crate::image_vfs::closes_on_this_thread();
        let active = Arc::new(AtomicBool::new(false));
        let result = stage_canonical::inspect_then(
            Box::new(Watched {
                file: std::fs::File::open(&image.path).unwrap(),
                active: active.clone(),
                event,
                cancel: cancel.clone(),
                fired: fired.clone(),
                drops: drops.clone(),
            }),
            &test_seams(real_fs()),
            &cancel,
            budgets.input,
            |connection| {
                active.store(true, Ordering::Release);
                guarded(connection, &cancel, budgets.work, check)
            },
        );
        assert!(
            fired.load(Ordering::Acquire),
            "integrity-phase event never fired"
        );
        if matches!(event, Event::Io | Event::Panic) {
            assert_eq!(
                result.as_ref().unwrap_err().io_kind(),
                Some(if matches!(event, Event::Panic) {
                    io::ErrorKind::Other
                } else {
                    io::ErrorKind::PermissionDenied
                })
            );
            assert!(
                matches!(result, Err(StoreError::Unavailable { .. })),
                "{result:?}"
            );
        } else {
            assert_eq!(result.unwrap(), None);
        }
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert_eq!(crate::image_vfs::closes_on_this_thread(), closed + 1);
    }
}

#[test]
fn retained_file_and_ignored_sidecars_survive_namespace_replacement() {
    let image = Image::new();
    image.large();
    let before = std::fs::read(&image.path).unwrap();
    let file = std::fs::File::open(&image.path).unwrap();
    let moved = image.path.with_extension("retained");
    std::fs::rename(&image.path, &moved).unwrap();
    std::fs::write(&image.path, b"replacement").unwrap();
    let wal = image.path.with_file_name("state.db-wal");
    let journal = image.path.with_file_name("state.db-journal");
    for path in [&wal, &journal] {
        std::fs::write(path, b"untrusted sidecar").unwrap();
    }
    assert!(
        inspect(
            Box::new(file),
            &test_seams(real_fs()),
            &AtomicBool::new(false)
        )
        .unwrap()
        .is_some()
    );
    assert_eq!(std::fs::read(&moved).unwrap(), before);
    assert_eq!(std::fs::read(&image.path).unwrap(), b"replacement");
    for path in [&wal, &journal] {
        assert_eq!(std::fs::read(path).unwrap(), b"untrusted sidecar");
    }
}

#[test]
fn budget_policy_scales_from_actual_bytes_and_checked_arithmetic() {
    let small = Budgets::for_size(0).unwrap();
    let large = Budgets::for_size(64 * 1024 * 1024).unwrap();
    assert!(large.input > small.input && large.work > small.work);
    assert!(Budgets::for_size(u64::MAX).is_err());
}

#[test]
fn malformed_catalog_refusals_do_not_echo_input_names() {
    let image = Image::new();
    image.sql("PRAGMA writable_schema=ON; UPDATE sqlite_schema SET name='PRIVATE_SCHEMA_MARKER', sql='CREATE TABLE invalid ???' WHERE name='accounts'; PRAGMA writable_schema=OFF;");
    let error = image.inspect().unwrap_err();
    assert!(
        !format!("{error:?}").contains("PRIVATE_SCHEMA_MARKER"),
        "{error:?}"
    );
}

#[test]
fn pre_cancel_and_size_callback_failures_drop_without_opening_sqlite() {
    struct SizeFault {
        event: Event,
        cancel: Arc<AtomicBool>,
        fired: Arc<AtomicBool>,
        drops: Arc<AtomicUsize>,
    }
    impl Read for SizeFault {
        fn read(&mut self, _: &mut [u8]) -> io::Result<usize> {
            panic!("read before size failure");
        }
    }
    impl Seek for SizeFault {
        fn seek(&mut self, _: SeekFrom) -> io::Result<u64> {
            self.fired.store(true, Ordering::Release);
            if matches!(self.event, Event::Panic) {
                panic!("injected size panic");
            }
            if matches!(self.event, Event::CancelIo) {
                self.cancel.store(true, Ordering::Release);
            }
            Err(io::ErrorKind::PermissionDenied.into())
        }
    }
    impl Drop for SizeFault {
        fn drop(&mut self) {
            self.drops.fetch_add(1, Ordering::SeqCst);
        }
    }
    for (pre_cancel, event) in [
        (true, Event::Io),
        (false, Event::Io),
        (false, Event::Panic),
        (false, Event::CancelIo),
    ] {
        let cancel = Arc::new(AtomicBool::new(pre_cancel));
        let fired = Arc::new(AtomicBool::new(false));
        let drops = Arc::new(AtomicUsize::new(0));
        let closed = crate::image_vfs::closes_on_this_thread();
        let result = inspect(
            Box::new(SizeFault {
                event,
                cancel: cancel.clone(),
                fired: fired.clone(),
                drops: drops.clone(),
            }),
            &test_seams(real_fs()),
            &cancel,
        );
        assert_eq!(fired.load(Ordering::Acquire), !pre_cancel);
        if pre_cancel || matches!(event, Event::CancelIo) {
            assert_eq!(result.unwrap(), None);
        } else {
            let error = result.unwrap_err();
            assert!(matches!(error, StoreError::Unavailable { .. }));
            assert_eq!(
                error.io_kind(),
                Some(if matches!(event, Event::Panic) {
                    io::ErrorKind::Other
                } else {
                    io::ErrorKind::PermissionDenied
                })
            );
        }
        assert_eq!(drops.load(Ordering::SeqCst), 1);
        assert_eq!(crate::image_vfs::closes_on_this_thread(), closed);
    }
}

#[test]
fn cancellation_when_the_retained_reader_closes_cannot_return_an_observation() {
    struct LateCancel {
        file: std::fs::File,
        cancel: Arc<AtomicBool>,
    }
    impl Read for LateCancel {
        fn read(&mut self, bytes: &mut [u8]) -> io::Result<usize> {
            self.file.read(bytes)
        }
    }
    impl Seek for LateCancel {
        fn seek(&mut self, to: SeekFrom) -> io::Result<u64> {
            self.file.seek(to)
        }
    }
    impl Drop for LateCancel {
        fn drop(&mut self) {
            self.cancel.store(true, Ordering::Release);
        }
    }
    let image = Image::new();
    assert!(image.inspect().unwrap().is_some());
    let cancel = Arc::new(AtomicBool::new(false));
    let closed = crate::image_vfs::closes_on_this_thread();
    assert_eq!(
        inspect(
            Box::new(LateCancel {
                file: std::fs::File::open(&image.path).unwrap(),
                cancel: cancel.clone()
            }),
            &test_seams(real_fs()),
            &cancel
        )
        .unwrap(),
        None
    );
    assert!(cancel.load(Ordering::Acquire));
    assert_eq!(crate::image_vfs::closes_on_this_thread(), closed + 1);
}
