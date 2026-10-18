//! Tests for import reports (plan M1.2c3c, spec/03): durable
//! replacement, name validation and injected write/rename/flush faults.

use std::sync::Arc;

use sb_test_support::{FaultFs, FaultKind, FsOp, ScratchDir, SeededRandom, real_fs};

use super::*;

const JOB: &str = "00000000-0000-4000-8000-0000000000c4";

fn reports(fs: Arc<dyn Fs>, scratch: &ScratchDir) -> ImportReports {
    ImportReports::new(
        fs,
        Arc::new(SeededRandom::new(7)),
        scratch.path().to_path_buf(),
    )
}

/// Every name in the directory, sorted.
fn names(scratch: &ScratchDir) -> Vec<String> {
    let mut names: Vec<String> = std::fs::read_dir(scratch.path())
        .unwrap()
        .map(|entry| entry.unwrap().file_name().into_string().unwrap())
        .collect();
    names.sort();
    names
}

#[test]
fn a_report_is_replaced_whole_and_leaves_no_temporary_file() {
    let scratch = ScratchDir::new("report-replace");
    let reports = reports(real_fs(), &scratch);
    assert!(!reports.exists(JOB).unwrap());
    reports
        .write(JOB, b"{\"v\":1,\"state\":\"running\"}")
        .unwrap();
    reports.write(JOB, b"{\"v\":1}").unwrap();
    assert!(reports.exists(JOB).unwrap());
    let path = reports.path(JOB).unwrap();
    assert_eq!(std::fs::read(&path).unwrap(), b"{\"v\":1}");
    assert_eq!(names(&scratch), [format!("import-report-{JOB}.json")]);
}

/// A report reads back as written; none yet is `None`, and one that
/// isn't UTF-8 is `Corrupt`.
#[test]
fn a_report_reads_back_as_written() {
    let scratch = ScratchDir::new("report-read");
    let reports = reports(real_fs(), &scratch);
    assert_eq!(reports.read(JOB).unwrap(), None);
    reports.write(JOB, b"{\"v\":1}").unwrap();
    assert_eq!(reports.read(JOB).unwrap().as_deref(), Some("{\"v\":1}"));

    reports.write(JOB, &[0xff, 0xfe]).unwrap();
    let error = reports.read(JOB).unwrap_err();
    assert!(matches!(error, StoreError::Corrupt { .. }), "{error:?}");
    assert!(matches!(
        reports.read("../x"),
        Err(StoreError::InvalidInput { .. })
    ));
}

#[test]
fn job_ids_cannot_name_other_paths() {
    let scratch = ScratchDir::new("report-names");
    let reports = reports(real_fs(), &scratch);
    for job in ["", "../x", "a/b", "ABC", "x".repeat(65).as_str(), "id.json"] {
        let error = reports.write(job, b"{}").unwrap_err();
        assert!(matches!(error, StoreError::InvalidInput { .. }), "{job:?}");
    }
    assert!(names(&scratch).is_empty());
}

#[test]
fn a_failed_write_keeps_the_old_report_and_removes_its_temporary_file() {
    for op in [FsOp::Write, FsOp::SyncFile, FsOp::Rename] {
        let scratch = ScratchDir::new("report-fault");
        reports(real_fs(), &scratch).write(JOB, b"old").unwrap();
        let faulty = reports(
            Arc::new(FaultFs::new(real_fs(), op, 1, FaultKind::Eio)),
            &scratch,
        );
        let error = faulty.write(JOB, b"new").unwrap_err();
        assert!(
            matches!(error, StoreError::Unavailable { .. }),
            "{op:?}: {error:?}"
        );
        assert_eq!(
            std::fs::read(faulty.path(JOB).unwrap()).unwrap(),
            b"old",
            "{op:?}"
        );
        assert_eq!(
            names(&scratch),
            [format!("import-report-{JOB}.json")],
            "{op:?}"
        );
    }
    // A failed directory flush is reported, never taken as durable.
    let scratch = ScratchDir::new("report-flush");
    let faulty = reports(
        Arc::new(FaultFs::new(real_fs(), FsOp::SyncDir, 1, FaultKind::Eio)),
        &scratch,
    );
    let error = faulty.write(JOB, b"new").unwrap_err();
    assert!(matches!(error, StoreError::Unavailable { .. }), "{error:?}");
}
