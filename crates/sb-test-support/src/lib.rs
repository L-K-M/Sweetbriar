//! `sb-test-support` — dev-only. The real `Fs` and scratch directories
//! for tests, plus (in later steps) the fake IMAP/SMTP server,
//! fixtures loader and crash-cut harness. Not part of the shipping
//! core; other crates use it only from tests.

use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};

use sb_platform::RealFs;
use sb_types::fs::Fs;

/// The real `Fs` implementation, for tests that exercise actual
/// filesystem behaviour (spec/01 rule 5).
pub fn real_fs() -> Arc<dyn Fs> {
    Arc::new(RealFs)
}

/// A unique directory under `std::env::temp_dir()`, removed on drop.
/// Its path is canonical (macOS's `/var` is a symlink to
/// `/private/var`), so it compares equal to paths the store resolves.
pub struct ScratchDir {
    root: PathBuf,
}

static NEXT_SCRATCH: AtomicU64 = AtomicU64::new(0);

impl ScratchDir {
    /// A new empty scratch dir; `label` should be a filename-safe word
    /// naming the test.
    pub fn new(label: &str) -> Self {
        // PID + counter + label: unique across processes and tests,
        // stepping over leftovers from killed runs.
        for _ in 0..100 {
            let seq = NEXT_SCRATCH.fetch_add(1, Ordering::Relaxed);
            let root =
                std::env::temp_dir().join(format!("sb-test-{}-{seq}-{label}", std::process::id()));
            match std::fs::create_dir(&root) {
                Ok(()) => {
                    let root = std::fs::canonicalize(&root)
                        .unwrap_or_else(|e| panic!("cannot resolve {}: {e}", root.display()));
                    return Self { root };
                }
                Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => continue,
                Err(e) => panic!("cannot create scratch dir {}: {e}", root.display()),
            }
        }
        panic!("could not allocate a scratch dir for {label}");
    }

    pub fn path(&self) -> &Path {
        &self.root
    }
}

impl Drop for ScratchDir {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.root);
    }
}
