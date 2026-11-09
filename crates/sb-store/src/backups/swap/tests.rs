//! The swap's renames with every step failed in turn: whatever is left
//! on disk, the next open's recovery resolves it to the old root or the
//! restored one, never neither, and removes the marker.

use std::path::PathBuf;

use sb_test_support::{FaultFs, FaultKind, FsOp, ScratchDir, real_fs};

use super::*;
use crate::restore_pending::{Recovered, marker_path, recover};

const NEW: &str = "00000000-0000-4000-8000-0000000000a1";
const OLD: &str = "00000000-0000-4000-8000-0000000000b2";
const GENERATION: &str = "00000000-0000-4000-8000-0000000000c3";

struct Swap {
    _scratch: ScratchDir,
    paths: StorePaths,
    pending: PendingRestore,
    staging: PathBuf,
    preserved: PathBuf,
}

/// A live root and a stage, each a directory naming itself and holding
/// a `state.sqlite` stand-in.
fn swap(label: &str) -> Swap {
    let scratch = ScratchDir::new(label);
    let parent = std::fs::canonicalize(scratch.path()).unwrap();
    let paths = StorePaths::new(parent.join("store")).unwrap();
    let pending = PendingRestore::new(paths.data_root(), NEW, GENERATION, OLD).unwrap();
    let staging = parent.join(&pending.staging);
    let preserved = parent.join(&pending.preserved);
    for (dir, label) in [
        (paths.data_root().to_path_buf(), "old"),
        (staging.clone(), "restored"),
    ] {
        std::fs::create_dir(&dir).unwrap();
        std::fs::write(dir.join("which"), label).unwrap();
        std::fs::write(dir.join("state.sqlite"), label).unwrap();
    }
    Swap {
        _scratch: scratch,
        paths,
        pending,
        staging,
        preserved,
    }
}

fn which(path: &Path) -> String {
    std::fs::read_to_string(path.join("which")).unwrap()
}

fn commit(fs: &dyn Fs, swap: &Swap) -> Result<(), StoreError> {
    commit_swap(
        fs,
        &swap.paths,
        &swap.staging,
        &swap.preserved,
        &swap.pending,
    )
}

#[test]
fn a_clean_commit_moves_the_stage_in_and_preserves_the_root() {
    let swap = swap("swap-commit");
    commit(real_fs().as_ref(), &swap).unwrap();
    assert_eq!(which(swap.paths.data_root()), "restored");
    assert_eq!(which(&swap.preserved), "old");
    assert!(!swap.staging.exists());
    assert!(!marker_path(&swap.paths).exists());
    assert_eq!(
        recover(real_fs().as_ref(), &swap.paths).unwrap(),
        Recovered::Nothing
    );
}

#[test]
fn a_failed_step_leaves_a_state_the_next_open_resolves() {
    let denied = FaultKind::Kind(io::ErrorKind::PermissionDenied);
    let operations = [
        FsOp::Remove,
        FsOp::Create,
        FsOp::Write,
        FsOp::SyncFile,
        FsOp::Rename,
        FsOp::SyncDir,
    ];
    for operation in operations {
        let calibration = swap("swap-fault-count");
        let counting = FaultFs::new(real_fs(), operation, u64::MAX, denied);
        commit(&counting, &calibration).unwrap();
        let total = counting.calls(operation);
        assert!(total > 0, "{operation:?} occurs in a commit");
        for nth in 1..=total {
            let swap = swap("swap-fault");
            let fault = FaultFs::new(real_fs(), operation, nth, denied);

            let failed = commit(&fault, &swap);

            assert_eq!(
                failed.unwrap_err().io_kind(),
                Some(io::ErrorKind::PermissionDenied),
                "{operation:?} #{nth} of {total}"
            );
            let resolved = recover(real_fs().as_ref(), &swap.paths).unwrap();
            let root = which(swap.paths.data_root());
            match root.as_str() {
                // Not started: the stage stays as evidence.
                "old" => assert_eq!(which(&swap.staging), "restored"),
                // Completed now or before: the old root is preserved.
                "restored" => assert_eq!(which(&swap.preserved), "old"),
                other => panic!("{operation:?} #{nth}: root holds {other}"),
            }
            assert!(
                !marker_path(&swap.paths).exists(),
                "{operation:?} #{nth}: {resolved:?} left the marker"
            );
        }
    }
}

use std::io;
