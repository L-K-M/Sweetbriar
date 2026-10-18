//! The crash-cut harness (spec/11 "Crash cuts"): run a named ignored
//! test of the current binary as a child process, over and over, where
//! the child kills itself with `SIGKILL` before the Nth filesystem
//! operation (N = 1, 2, … through [`CutFs`]) — until one run completes
//! without being killed. After each killed run the caller's check
//! closure reopens the leftovers and asserts the invariants.

use std::env;
use std::path::{Path, PathBuf};
use std::process::{Child, Command, ExitStatus};
use std::sync::Arc;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use sb_types::fs::{Fs, Lease, Location, LockError, Metadata, WriteFile};

/// Give up after this many cut points: a run that never survives either
/// fails on its first operation every time or does not use the `Fs`
/// seam, and looping forever would hang CI.
const MAX_CUT_POINTS: u64 = 4096;

/// How long one crash-cut child may run: a hung child (a deadlock in
/// the code under test) fails here instead of at the CI job's timeout.
const CHILD_DEADLINE: Duration = Duration::from_secs(120);

/// How often a waiting harness checks on its child.
const CHILD_POLL: Duration = Duration::from_millis(10);

/// An `Fs` that kills the process, `SIGKILL`, immediately before its
/// Nth operation (any kind) — the sharpest possible cut, with nothing
/// running afterwards. Operations on handles it handed out (`write`,
/// `sync_all`) count too; `flush` does nothing on a real file and is
/// not counted.
pub struct CutFs {
    inner: Arc<dyn Fs>,
    /// 1-based: die before this call, of any operation.
    nth: u64,
    /// Shared with the write handles this `Fs` hands out.
    count: Arc<AtomicU64>,
}

impl CutFs {
    /// Kill the process before the `nth` operation.
    pub fn new(inner: Arc<dyn Fs>, nth: u64) -> Self {
        Self {
            inner,
            nth,
            count: Arc::new(AtomicU64::new(0)),
        }
    }

    /// How many operations ran so far: an uncut run (`nth` past every
    /// operation) measures where cut points can land.
    pub fn operations(&self) -> u64 {
        self.count.load(Ordering::SeqCst)
    }

    /// Count one operation; kill the process if it is the cut point.
    fn trip(&self) {
        let n = self.count.fetch_add(1, Ordering::SeqCst) + 1;
        if n == self.nth {
            // SIGKILL cannot be caught or blocked, so this never
            // returns; the message is only for a kernel that refused.
            // SAFETY: killing our own process takes no capability a
            // child test does not have.
            unsafe {
                libc::kill(libc::getpid(), libc::SIGKILL);
            }
            panic!("SIGKILL at cut point {n} did not kill the process");
        }
    }
}

impl Fs for CutFs {
    fn create(&self, path: &Path) -> std::io::Result<Box<dyn WriteFile>> {
        self.trip();
        let inner = self.inner.create(path)?;
        Ok(Box::new(CutWrite {
            inner,
            count: Arc::clone(&self.count),
            nth: self.nth,
        }))
    }

    fn open(&self, path: &Path) -> std::io::Result<Box<dyn sb_types::fs::ReadFile>> {
        self.trip();
        self.inner.open(path)
    }

    fn open_regular(
        &self,
        path: &Path,
    ) -> std::io::Result<(Box<dyn sb_types::fs::ReadFile>, Metadata)> {
        self.trip();
        self.inner.open_regular(path)
    }

    fn metadata(&self, path: &Path) -> std::io::Result<Option<Metadata>> {
        self.trip();
        self.inner.metadata(path)
    }

    fn read_dir(&self, path: &Path) -> std::io::Result<Vec<PathBuf>> {
        self.trip();
        self.inner.read_dir(path)
    }

    fn create_dir(&self, path: &Path) -> std::io::Result<()> {
        self.trip();
        self.inner.create_dir(path)
    }

    fn sync_dir(&self, path: &Path) -> std::io::Result<()> {
        self.trip();
        self.inner.sync_dir(path)
    }

    fn rename(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        self.trip();
        self.inner.rename(from, to)
    }

    fn remove(&self, path: &Path) -> std::io::Result<()> {
        self.trip();
        self.inner.remove(path)
    }

    fn canonicalize(&self, path: &Path) -> std::io::Result<PathBuf> {
        self.trip();
        self.inner.canonicalize(path)
    }

    fn lock_exclusive(&self, path: &Path) -> Result<Box<dyn Lease>, LockError> {
        self.trip();
        self.inner.lock_exclusive(path)
    }

    fn location(&self, path: &Path) -> std::io::Result<Location> {
        self.trip();
        self.inner.location(path)
    }
}

/// A write handle whose writes and flushes pass through the cut
/// counter.
struct CutWrite {
    inner: Box<dyn WriteFile>,
    count: Arc<AtomicU64>,
    nth: u64,
}

impl CutWrite {
    fn trip(&self) {
        let n = self.count.fetch_add(1, Ordering::SeqCst) + 1;
        if n == self.nth {
            // SAFETY: as in `CutFs::trip`.
            unsafe {
                libc::kill(libc::getpid(), libc::SIGKILL);
            }
            panic!("SIGKILL at cut point {n} did not kill the process");
        }
    }
}

impl std::io::Write for CutWrite {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.trip();
        self.inner.write(buf)
    }

    fn flush(&mut self) -> std::io::Result<()> {
        self.inner.flush()
    }
}

impl WriteFile for CutWrite {
    fn sync_all(&mut self) -> std::io::Result<()> {
        self.trip();
        self.inner.sync_all()
    }
}

/// Run `child` (an `#[ignore]`d test of this binary, invoked with
/// `--exact <child> --ignored`) once per cut point `n` = 1, 2, … with
/// `n` in `cut_env` and a fresh directory from `fresh_root` in
/// `root_env`, until a run completes without being killed. After each
/// killed run, `check` inspects that run's directory. A child that
/// exits any other way fails the harness loudly. Returns how many
/// runs were killed — a caller that expected cuts can assert on it,
/// so a broken `CutFs` cannot make the whole exercise vacuous.
pub fn run_crash_cuts(
    cut_env: &str,
    root_env: &str,
    child: &str,
    fresh_root: &mut dyn FnMut(u64) -> PathBuf,
    check: &mut dyn FnMut(&Path),
) -> u64 {
    let exe = env::current_exe().expect("the test binary path");
    let mut n: u64 = 1;
    loop {
        let root = fresh_root(n);
        let spawned = Command::new(&exe)
            .args(["--exact", child, "--ignored"])
            .env(cut_env, n.to_string())
            .env(root_env, &root)
            .spawn()
            .expect("spawn the crash-cut child");
        let status = wait_with_deadline(spawned, CHILD_DEADLINE);
        if status.success() {
            // The cut point was past every operation the run makes.
            return n - 1;
        }
        let killed = std::os::unix::process::ExitStatusExt::signal(&status);
        if killed == Some(libc::SIGKILL) {
            check(&root);
        } else {
            panic!("crash-cut child at N={n} exited abnormally: {status}");
        }
        n += 1;
        assert!(
            n <= MAX_CUT_POINTS,
            "the child never survived {MAX_CUT_POINTS} cut points"
        );
    }
}

/// Run `child` once per cut point in `points`, as [`run_crash_cuts`]
/// does, for suites that sample cut points (a seeded set) instead of
/// sweeping them all. A run that survives its point (it made fewer
/// operations this time) is skipped; `check` sees each killed run.
/// Returns how many runs were killed.
pub fn run_crash_cut_points(
    cut_env: &str,
    root_env: &str,
    child: &str,
    points: &[u64],
    fresh_root: &mut dyn FnMut(u64) -> PathBuf,
    check: &mut dyn FnMut(&Path),
) -> u64 {
    let exe = env::current_exe().expect("the test binary path");
    let mut killed = 0;
    for &n in points {
        let root = fresh_root(n);
        let spawned = Command::new(&exe)
            .args(["--exact", child, "--ignored"])
            .env(cut_env, n.to_string())
            .env(root_env, &root)
            .spawn()
            .expect("spawn the crash-cut child");
        let status = wait_with_deadline(spawned, CHILD_DEADLINE);
        if status.success() {
            continue;
        }
        let signal = std::os::unix::process::ExitStatusExt::signal(&status);
        assert_eq!(
            signal,
            Some(libc::SIGKILL),
            "crash-cut child at N={n} exited abnormally: {status}"
        );
        check(&root);
        killed += 1;
    }
    killed
}

/// Wait for `child`, killing it and failing loudly once `deadline`
/// passes.
fn wait_with_deadline(mut child: Child, deadline: Duration) -> ExitStatus {
    let started = Instant::now();
    loop {
        if let Some(status) = child.try_wait().expect("wait for the crash-cut child") {
            return status;
        }
        if started.elapsed() > deadline {
            let _ = child.kill();
            let _ = child.wait();
            panic!("a crash-cut child ran past {deadline:?}");
        }
        thread::sleep(CHILD_POLL);
    }
}

#[cfg(test)]
mod tests {
    use std::fs;
    use std::io::Write as _;

    use super::*;

    /// The env vars the operation child reads: which operation, and
    /// the directory it works in.
    const OP_ENV: &str = "SB_CUT_TEST_OP";
    const ROOT_ENV: &str = "SB_CUT_TEST_ROOT";

    /// Every operation `CutFs` counts, by the name the child takes.
    const OPS: [&str; 14] = [
        "create",
        "write",
        "sync_all",
        "open",
        "open_regular",
        "metadata",
        "read_dir",
        "create_dir",
        "sync_dir",
        "rename",
        "remove",
        "canonicalize",
        "lock_exclusive",
        "location",
    ];

    /// Not a test: `every_operation_is_a_cut_point` runs it as a child.
    /// It sets up `root/file` without cuts, then performs one operation
    /// through a `CutFs` whose cut point is exactly that operation; it
    /// must die before the operation acts.
    #[test]
    #[ignore = "spawned by every_operation_is_a_cut_point"]
    fn cut_op_child() {
        let op = env::var(OP_ENV).expect("operation env");
        let root = PathBuf::from(env::var(ROOT_ENV).expect("root env"));
        let file = root.join("file");
        fs::write(&file, b"before").unwrap();
        let real: Arc<dyn Fs> = Arc::new(sb_platform::RealFs);

        // `write` and `sync_all` act on a handle, so the cut point is
        // the operation after `create`.
        let nth = match op.as_str() {
            "write" => 2,
            "sync_all" => 3,
            _ => 1,
        };
        let cut = CutFs::new(real, nth);
        let _ = match op.as_str() {
            "create" => cut.create(&root.join("new")).map(drop),
            "write" | "sync_all" => {
                let mut handle = cut.create(&root.join("new")).unwrap();
                handle.write_all(b"after").unwrap();
                handle.sync_all()
            }
            "open" => cut.open(&file).map(drop),
            "open_regular" => cut.open_regular(&file).map(drop),
            "metadata" => cut.metadata(&file).map(drop),
            "read_dir" => cut.read_dir(&root).map(drop),
            "create_dir" => cut.create_dir(&root.join("dir")),
            "sync_dir" => cut.sync_dir(&root),
            "rename" => cut.rename(&file, &root.join("renamed")),
            "remove" => cut.remove(&file),
            "canonicalize" => cut.canonicalize(&file).map(drop),
            "lock_exclusive" => cut
                .lock_exclusive(&root.join("lock"))
                .map(drop)
                .map_err(|_| std::io::Error::other("lock")),
            "location" => cut.location(&root).map(drop),
            other => panic!("unknown operation {other}"),
        };
        panic!("{op} ran past its cut point");
    }

    /// Each operation `CutFs` wraps is a cut point: the child dies by
    /// `SIGKILL` before the operation changes anything, so a crash
    /// sweep that misses an operation can't go unnoticed.
    #[test]
    fn every_operation_is_a_cut_point() {
        let exe = env::current_exe().unwrap();
        let scratch = crate::ScratchDir::new("cut-every-op");
        for op in OPS {
            let root = scratch.path().join(op);
            fs::create_dir(&root).unwrap();
            let child = Command::new(&exe)
                .args(["--exact", "cut::tests::cut_op_child", "--ignored"])
                .env(OP_ENV, op)
                .env(ROOT_ENV, &root)
                .spawn()
                .unwrap();
            let status = wait_with_deadline(child, CHILD_DEADLINE);
            assert_eq!(
                std::os::unix::process::ExitStatusExt::signal(&status),
                Some(libc::SIGKILL),
                "{op}: the child was not killed at its cut point ({status})"
            );

            // Nothing the operation would have changed did change.
            assert_eq!(fs::read(root.join("file")).unwrap(), b"before", "{op}");
            assert!(!root.join("renamed").exists(), "{op}");
            assert!(!root.join("dir").exists(), "{op}");
            assert!(!root.join("lock").exists(), "{op}");
            let new = root.join("new");
            match op {
                "create" => assert!(!new.exists(), "create ran"),
                "write" => assert_eq!(fs::read(&new).unwrap(), b"", "write ran"),
                _ => {}
            }
        }
    }
}
