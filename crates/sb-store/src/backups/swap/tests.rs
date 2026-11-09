//! The swap's renames with every step failed in turn, or followed by a
//! power cut: whatever is left on disk, the next open's recovery
//! resolves it to the old root or the restored one, never neither, and
//! removes the marker.

use std::io::{self, Read, Write};
use std::path::PathBuf;
use std::sync::Arc;

use sb_test_support::{FaultFs, FaultKind, FsOp, ScratchDir, SimFs, real_fs};

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

/// The same swap on a simulated disk, everything flushed.
fn sim_swap() -> (Arc<SimFs>, StorePaths, PendingRestore) {
    let fs = Arc::new(SimFs::new());
    let paths = StorePaths::new("/data/store").unwrap();
    let pending = PendingRestore::new(paths.data_root(), NEW, GENERATION, OLD).unwrap();
    let parent = Path::new("/data");
    for (dir, label) in [
        (paths.data_root().to_path_buf(), "old"),
        (parent.join(&pending.staging), "restored"),
    ] {
        fs.create_dir_all(&dir).unwrap();
        for name in ["which", "state.sqlite"] {
            let mut file = fs.create(&dir.join(name)).unwrap();
            file.write_all(label.as_bytes()).unwrap();
            file.sync_all().unwrap();
        }
        fs.sync_dir(&dir).unwrap();
    }
    fs.sync_dir(parent).unwrap();
    (fs, paths, pending)
}

/// What a simulated directory names itself, if it exists.
fn sim_which(fs: &SimFs, dir: &Path) -> Option<String> {
    let mut text = String::new();
    fs.open(&dir.join("which"))
        .ok()?
        .read_to_string(&mut text)
        .unwrap();
    Some(text)
}

/// Power lost before any one step of the commit, or after its last:
/// what survives on disk is a state startup resolves to the old root
/// with the stage kept, or to the restored root with the old one
/// preserved, durably, and never to no root at all.
#[test]
fn a_power_cut_anywhere_in_the_commit_leaves_a_state_startup_resolves() {
    let denied = FaultKind::Kind(io::ErrorKind::PermissionDenied);
    let operations = [
        FsOp::Remove,
        FsOp::Create,
        FsOp::Write,
        FsOp::SyncFile,
        FsOp::Rename,
        FsOp::SyncDir,
    ];
    let mut cuts = vec![None];
    for operation in operations {
        let (fs, paths, pending) = sim_swap();
        let counting = FaultFs::new(fs, operation, u64::MAX, denied);
        let parent = Path::new("/data");
        commit_swap(
            &counting,
            &paths,
            &parent.join(&pending.staging),
            &parent.join(&pending.preserved),
            &pending,
        )
        .unwrap();
        cuts.extend((1..=counting.calls(operation)).map(|nth| Some((operation, nth))));
    }
    let mut resolved = Vec::new();
    for cut in cuts {
        let (fs, paths, pending) = sim_swap();
        let parent = Path::new("/data");
        let (staging, preserved) = (
            parent.join(&pending.staging),
            parent.join(&pending.preserved),
        );
        let stopped = match cut {
            // The commit stops before this step: the power goes then.
            Some((operation, nth)) => {
                let fault = FaultFs::new(Arc::clone(&fs) as Arc<dyn Fs>, operation, nth, denied);
                commit_swap(&fault, &paths, &staging, &preserved, &pending).unwrap_err();
                true
            }
            None => {
                commit_swap(fs.as_ref(), &paths, &staging, &preserved, &pending).unwrap();
                false
            }
        };
        fs.power_cut();

        let recovered = recover(fs.as_ref(), &paths).unwrap();
        // Recovery's own steps are durable too.
        fs.power_cut();
        let root = sim_which(&fs, paths.data_root());
        match root.as_deref() {
            Some("old") => assert_eq!(sim_which(&fs, &staging).as_deref(), Some("restored")),
            Some("restored") => assert_eq!(sim_which(&fs, &preserved).as_deref(), Some("old")),
            other => panic!("{cut:?}: the data root holds {other:?}"),
        }
        assert!(
            fs.metadata(&marker_path(&paths)).unwrap().is_none(),
            "{cut:?}: {recovered:?} left the marker"
        );
        assert!(stopped || root.as_deref() == Some("restored"));
        resolved.push(root.unwrap());
    }
    assert!(resolved.iter().any(|root| root == "old"));
    assert!(resolved.iter().any(|root| root == "restored"));
}
