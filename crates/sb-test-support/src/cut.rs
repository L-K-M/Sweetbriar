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

use sb_types::fs::{
    ConfinedDir, DirectoryIdentity, FileIdentity, Fs, Lease, Location, LockError, Metadata,
    ReadFile, ReadWriteFile, WriteFile,
};

/// Give up after this many cut points: a run that never survives either
/// fails on its first operation every time or does not use the `Fs`
/// seam, and looping forever would hang CI.
const MAX_CUT_POINTS: u64 = 4096;

/// How long one crash-cut child may run: a hung child (a deadlock in
/// the code under test) fails here instead of at the CI job's timeout.
const CHILD_DEADLINE: Duration = Duration::from_secs(120);

/// How often a waiting harness checks on its child.
const CHILD_POLL: Duration = Duration::from_millis(10);

/// How long a child waits for its own `SIGKILL` to end it.
const KILL_PATIENCE: Duration = Duration::from_secs(30);

/// The exit status of a child whose `SIGKILL` never ended it: not a
/// signal, so its parent reports the run as abnormal.
const KILL_REFUSED_EXIT: i32 = 86;

/// Count one operation; cut the process at the `nth`.
fn trip(count: &AtomicU64, nth: u64) {
    let n = count.fetch_add(1, Ordering::SeqCst) + 1;
    if n == nth {
        // SAFETY: killing our own process takes no capability a child
        // test does not have.
        cut(
            || unsafe {
                libc::kill(libc::getpid(), libc::SIGKILL);
            },
            KILL_PATIENCE,
        );
    }
}

/// End this process at its cut point, running nothing more on this
/// thread.
///
/// `SIGKILL` can't be caught or blocked, but the process doesn't end
/// before `kill` returns: the kernel ends it when one of its threads
/// next takes the signal, and on a loaded machine the calling thread
/// runs on for a while first. It must not return and must not unwind.
/// An unwind runs the test's destructors, and a scratch directory's
/// `Drop` removes the very leftovers the parent is about to inspect, in a
/// child that still dies of `SIGKILL` and so looks cleanly cut.
///
/// So the thread waits for its death. A kernel that refused the kill
/// ends the wait after `patience`: the process exits without unwinding
/// or running exit handlers, with a status that is no signal.
fn cut(kill: impl FnOnce(), patience: Duration) -> ! {
    kill();
    thread::sleep(patience);
    // No diagnostic I/O: it can block or panic instead of reaching _exit.
    // The parent reports the abnormal exit with its own cut-point number.
    // SAFETY: `_exit` only ends the process; skipping destructors and
    // exit handlers is the point.
    unsafe { libc::_exit(KILL_REFUSED_EXIT) }
}

/// An `Fs` that kills the process, `SIGKILL`, immediately before its
/// Nth operation (any kind). It is the sharpest possible cut: the
/// thread that reached it runs nothing more, not even an unwind.
/// Operations on handles it handed out (`write`, `sync_all`) count
/// too; `flush` does nothing on a real file and is not counted.
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
        trip(&self.count, self.nth);
    }
}

struct CutConfinedDir {
    inner: Box<dyn ConfinedDir>,
    count: Arc<AtomicU64>,
    nth: u64,
}

impl CutConfinedDir {
    fn trip(&self) {
        trip(&self.count, self.nth);
    }
}

impl ConfinedDir for CutConfinedDir {
    fn visit_children(
        &self,
        visitor: &mut dyn FnMut(&Path) -> std::io::Result<std::ops::ControlFlow<()>>,
    ) -> std::io::Result<std::ops::ControlFlow<()>> {
        self.trip();
        self.inner.visit_children(&mut |name| {
            self.trip();
            visitor(name)
        })
    }
    fn set_ownership(&self, bytes: &[u8]) -> std::io::Result<()> {
        self.trip();
        self.inner.set_ownership(bytes)
    }
    fn ownership(&self) -> std::io::Result<Vec<u8>> {
        self.trip();
        self.inner.ownership()
    }
    fn identity(&self) -> std::io::Result<DirectoryIdentity> {
        self.trip();
        self.inner.identity()
    }
    fn open_dir(&self, name: &Path) -> std::io::Result<Box<dyn ConfinedDir>> {
        self.trip();
        Ok(Box::new(Self {
            inner: self.inner.open_dir(name)?,
            count: self.count.clone(),
            nth: self.nth,
        }))
    }
    fn create_private_dir(&self, name: &Path) -> std::io::Result<()> {
        self.trip();
        self.inner.create_private_dir(name)
    }
    fn create(&self, name: &Path) -> std::io::Result<Box<dyn WriteFile>> {
        self.trip();
        Ok(Box::new(CutWrite {
            inner: self.inner.create(name)?,
            count: self.count.clone(),
            nth: self.nth,
        }))
    }
    fn create_read_write(&self, name: &Path) -> std::io::Result<Box<dyn ReadWriteFile>> {
        self.trip();
        Ok(Box::new(CutReadWrite {
            inner: self.inner.create_read_write(name)?,
            count: self.count.clone(),
            nth: self.nth,
        }))
    }
    fn remove_file(&self, name: &Path) -> std::io::Result<()> {
        self.trip();
        self.inner.remove_file(name)
    }
    fn remove_empty_dir(&self, name: &Path) -> std::io::Result<()> {
        self.trip();
        self.inner.remove_empty_dir(name)
    }
    fn sync(&self) -> std::io::Result<()> {
        self.trip();
        self.inner.sync()
    }
    fn open_regular(&self, path: &Path) -> std::io::Result<(Box<dyn ReadFile>, Metadata)> {
        self.trip();
        self.inner.open_regular(path)
    }
    fn open_regular_identified(
        &self,
        path: &Path,
    ) -> std::io::Result<(Box<dyn ReadFile>, Metadata, FileIdentity)> {
        self.trip();
        self.inner.open_regular_identified(path)
    }
}

impl Fs for CutFs {
    fn open_confined_dir(&self, path: &Path) -> std::io::Result<Box<dyn ConfinedDir>> {
        self.trip();
        Ok(Box::new(CutConfinedDir {
            inner: self.inner.open_confined_dir(path)?,
            count: self.count.clone(),
            nth: self.nth,
        }))
    }

    fn open_no_follow(&self, path: &Path) -> std::io::Result<Box<dyn sb_types::fs::ReadFile>> {
        self.trip();
        self.inner.open_no_follow(path)
    }
    fn create(&self, path: &Path) -> std::io::Result<Box<dyn WriteFile>> {
        self.trip();
        let inner = self.inner.create(path)?;
        Ok(Box::new(CutWrite {
            inner,
            count: Arc::clone(&self.count),
            nth: self.nth,
        }))
    }

    fn clone_file(&self, from: &Path, to: &Path) -> std::io::Result<()> {
        self.trip();
        self.inner.clone_file(from, to)
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

    fn create_private_dir(&self, path: &Path) -> std::io::Result<()> {
        self.trip();
        self.inner.create_private_dir(path)
    }

    fn restrict_dir(&self, path: &Path) -> std::io::Result<()> {
        self.trip();
        self.inner.restrict_dir(path)
    }

    fn remove_dir_all(&self, path: &Path) -> std::io::Result<()> {
        self.trip();
        self.inner.remove_dir_all(path)
    }

    fn quarantine(&self, path: &Path) -> std::io::Result<()> {
        self.trip();
        self.inner.quarantine(path)
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
struct CutReadWrite {
    inner: Box<dyn ReadWriteFile>,
    count: Arc<AtomicU64>,
    nth: u64,
}

impl CutReadWrite {
    fn trip(&self) {
        trip(&self.count, self.nth);
    }
}

impl ReadWriteFile for CutReadWrite {
    fn read_at(&self, bytes: &mut [u8], offset: u64) -> std::io::Result<usize> {
        self.trip();
        self.inner.read_at(bytes, offset)
    }
    fn write_at(&self, bytes: &[u8], offset: u64) -> std::io::Result<usize> {
        self.trip();
        self.inner.write_at(bytes, offset)
    }
    fn set_len(&self, len: u64) -> std::io::Result<()> {
        self.trip();
        self.inner.set_len(len)
    }
    fn len(&self) -> std::io::Result<u64> {
        self.trip();
        self.inner.len()
    }
    fn sync_all(&self) -> std::io::Result<()> {
        self.trip();
        self.inner.sync_all()
    }
    fn identity(&self) -> std::io::Result<FileIdentity> {
        self.trip();
        self.inner.identity()
    }
}

struct CutWrite {
    inner: Box<dyn WriteFile>,
    count: Arc<AtomicU64>,
    nth: u64,
}

impl CutWrite {
    fn trip(&self) {
        trip(&self.count, self.nth);
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

/// What a sampled crash-cut run produced: the points that killed their
/// child, and those whose child finished first (it made fewer
/// operations), so a caller can name them.
#[derive(Debug, Default, PartialEq, Eq)]
pub struct CutPoints {
    pub killed: Vec<u64>,
    pub survived: Vec<u64>,
}

/// Run `child` once per cut point in `points`, as [`run_crash_cuts`]
/// does, for suites that sample cut points (a seeded set) instead of
/// sweeping them all. `check` sees each killed run; a run that survives
/// its point is recorded, not checked.
pub fn run_crash_cut_points(
    cut_env: &str,
    root_env: &str,
    child: &str,
    points: &[u64],
    fresh_root: &mut dyn FnMut(u64) -> PathBuf,
    check: &mut dyn FnMut(&Path),
) -> CutPoints {
    let exe = env::current_exe().expect("the test binary path");
    let mut report = CutPoints::default();
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
            report.survived.push(n);
            continue;
        }
        let signal = std::os::unix::process::ExitStatusExt::signal(&status);
        assert_eq!(
            signal,
            Some(libc::SIGKILL),
            "crash-cut child at N={n} exited abnormally: {status}"
        );
        check(&root);
        report.killed.push(n);
    }
    report
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

    #[test]
    fn confined_enumeration_counts_each_cut_point() {
        use std::ops::ControlFlow;
        let inner = Arc::new(crate::SimFs::new());
        inner.create_dir(Path::new("/root")).unwrap();
        inner.create(Path::new("/root/a")).unwrap();
        inner.create(Path::new("/root/b")).unwrap();
        let fs = CutFs::new(inner, u64::MAX);
        let root = fs.open_confined_dir(Path::new("/root")).unwrap();
        assert_eq!(
            root.visit_children(&mut |_| Ok(ControlFlow::Continue(())))
                .unwrap(),
            ControlFlow::Continue(())
        );
        assert_eq!(fs.operations(), 4);
    }

    /// The env var the late- and refused-kill children read: the
    /// directory they report in.
    const REPORT_ENV: &str = "SB_CUT_TEST_REPORT";

    /// Writes `unwound` into the report directory when dropped: what a
    /// test's scratch directory does to its leftovers, made visible.
    struct Unwound(PathBuf);

    impl Drop for Unwound {
        fn drop(&mut self) {
            let _ = fs::write(self.0.join("unwound"), b"");
        }
    }

    fn report_root() -> PathBuf {
        PathBuf::from(env::var(REPORT_ENV).expect("report env"))
    }

    fn spawn_child(name: &str, report: &Path) -> Child {
        Command::new(env::current_exe().unwrap())
            .args(["--exact", name, "--ignored"])
            .env(REPORT_ENV, report)
            .spawn()
            .unwrap()
    }

    /// A spawned child that is killed and reaped when its parent test
    /// unwinds: `Child`'s own `Drop` does neither, so a test that
    /// panics before its own `kill` would leave the process asleep.
    struct KillOnDrop(Child);

    impl KillOnDrop {
        fn try_wait(&mut self) -> std::io::Result<Option<ExitStatus>> {
            self.0.try_wait()
        }
    }

    impl Drop for KillOnDrop {
        fn drop(&mut self) {
            let _ = self.0.kill();
            let _ = self.0.wait();
        }
    }

    /// A cut whose kill has not landed: it only reports that the cut
    /// was reached, as a kernel under load leaves the thread running.
    #[test]
    #[ignore = "spawned by a_kill_that_lands_late_runs_no_destructor"]
    fn late_kill_child() {
        let root = report_root();
        let _leftovers = Unwound(root.clone());
        cut(
            || fs::write(root.join("cut"), b"").unwrap(),
            // Far longer than the parent's observation window, but
            // bounded: a child orphaned by a failing parent still ends
            // on its own (the parent's guard kills it first).
            Duration::from_secs(60),
        );
    }

    /// Until the kill lands, the cutting thread neither returns nor
    /// unwinds: a destructor would remove the leftovers of a child
    /// that then dies of `SIGKILL` and passes for cleanly cut.
    #[test]
    fn a_kill_that_lands_late_runs_no_destructor() {
        let scratch = crate::ScratchDir::new("cut-late-kill");
        let mut child = KillOnDrop(spawn_child("cut::tests::late_kill_child", scratch.path()));
        let started = Instant::now();
        while !scratch.path().join("cut").exists() {
            let ended = child.try_wait().unwrap();
            assert!(ended.is_none(), "the child ended before its cut: {ended:?}");
            assert!(started.elapsed() < CHILD_DEADLINE, "the child never cut");
            thread::sleep(CHILD_POLL);
        }
        // An unwind would have run the destructor within microseconds.
        thread::sleep(Duration::from_millis(300));
        let ended = child.try_wait().unwrap();
        let unwound = scratch.path().join("unwound").exists();
        // A panic in either assert below unwinds through the guard,
        // which kills and reaps the child before the test ends.
        drop(child);

        assert!(ended.is_none(), "the thread ran on past the cut: {ended:?}");
        assert!(!unwound, "a destructor ran after the cut");
    }

    /// Dropping the guard ends a child that is still asleep: a parent
    /// test that unwinds before its own `kill` must not leave the
    /// process running.
    #[test]
    fn kill_on_drop_ends_a_sleeping_child() {
        let scratch = crate::ScratchDir::new("cut-kill-on-drop");
        let pid = {
            let child = spawn_child("cut::tests::late_kill_child", scratch.path());
            let pid = child.id() as libc::pid_t;
            let guard = KillOnDrop(child);
            // Let the child reach its cut and settle into its long sleep.
            let started = Instant::now();
            while !scratch.path().join("cut").exists() {
                assert!(started.elapsed() < CHILD_DEADLINE, "the child never cut");
                thread::sleep(CHILD_POLL);
            }
            let dropped = Instant::now();
            drop(guard);
            // The guard's `kill` ends the child at once; without it,
            // `wait` would block until the child's own patience ran out.
            assert!(
                dropped.elapsed() < Duration::from_secs(5),
                "dropping the guard took {:?}: the child was not killed",
                dropped.elapsed()
            );
            pid
        };
        // The guard's `wait` reaped it, so a signal-0 probe finds no
        // such process; it died of the guard's `SIGKILL`, not by
        // unwinding.
        let mut alive = true;
        for _ in 0..100 {
            if unsafe { libc::kill(pid, 0) } != 0 {
                alive = false;
                break;
            }
            thread::sleep(CHILD_POLL);
        }
        assert!(!alive, "the dropped child is still alive (pid {pid})");
        assert!(!scratch.path().join("unwound").exists());
    }

    /// A cut whose kill never lands.
    #[test]
    #[ignore = "spawned by a_refused_kill_exits_without_unwinding"]
    fn refused_kill_child() {
        let _leftovers = Unwound(report_root());
        cut(|| {}, Duration::from_millis(50));
    }

    /// A kernel that refused the kill is reported by the exit status,
    /// which is no signal, and still no destructor runs.
    #[test]
    fn a_refused_kill_exits_without_unwinding() {
        let scratch = crate::ScratchDir::new("cut-refused-kill");
        let child = spawn_child("cut::tests::refused_kill_child", scratch.path());
        let status = wait_with_deadline(child, CHILD_DEADLINE);

        assert_eq!(status.code(), Some(KILL_REFUSED_EXIT), "{status}");
        assert!(!scratch.path().join("unwound").exists());
    }

    /// Failed diagnostics must not unwind the refused-kill fallback.
    #[test]
    fn a_refused_kill_with_broken_stderr_never_unwinds() {
        let scratch = crate::ScratchDir::new("cut-refused-broken-stderr");
        // Close the peer before spawning so every diagnostic sees
        // EPIPE, not EBADF (which stdio deliberately ignores).
        let (stderr, peer) = std::os::unix::net::UnixStream::pair().unwrap();
        drop(peer);
        let stderr: std::os::fd::OwnedFd = stderr.into();
        let child = Command::new(env::current_exe().unwrap())
            .args([
                "--exact",
                "cut::tests::refused_kill_child",
                "--ignored",
                "--nocapture",
            ])
            .env(REPORT_ENV, scratch.path())
            .stderr(stderr)
            .spawn()
            .unwrap();
        let status = wait_with_deadline(child, CHILD_DEADLINE);
        assert_eq!(status.code(), Some(KILL_REFUSED_EXIT), "{status}");
        assert!(!scratch.path().join("unwound").exists());
    }

    /// The env vars the operation child reads: which operation, and
    /// the directory it works in.
    const OP_ENV: &str = "SB_CUT_TEST_OP";
    const ROOT_ENV: &str = "SB_CUT_TEST_ROOT";

    /// Every operation `CutFs` counts, by the name the child takes.
    const OPS: [&str; 26] = [
        "identified_read",
        "create_rw",
        "read_at",
        "write_at",
        "set_len",
        "rw_len",
        "rw_identity",
        "rw_sync",
        "create",
        "write",
        "sync_all",
        "open",
        "open_regular",
        "metadata",
        "read_dir",
        "create_dir",
        "create_private_dir",
        "restrict_dir",
        "remove_dir_all",
        "quarantine",
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
        if op == "remove_dir_all" {
            fs::create_dir(root.join("victim")).unwrap();
            fs::write(root.join("victim/keep"), b"before").unwrap();
        }
        let real: Arc<dyn Fs> = Arc::new(sb_platform::RealFs);

        // `write` and `sync_all` act on a handle, so the cut point is
        // the operation after `create`.
        let nth = match op.as_str() {
            "create_rw" | "identified_read" => 2,
            "read_at" | "write_at" | "set_len" | "rw_len" | "rw_identity" | "rw_sync" => 3,
            "write" => 2,
            "sync_all" => 3,
            _ => 1,
        };
        let cut = CutFs::new(real, nth);
        let _ = match op.as_str() {
            "identified_read" => cut
                .open_confined_dir(&root.canonicalize().unwrap())
                .unwrap()
                .open_regular_identified(Path::new("file"))
                .map(drop),
            "create_rw" | "read_at" | "write_at" | "set_len" | "rw_len" | "rw_identity"
            | "rw_sync" => {
                let dir = cut
                    .open_confined_dir(&root.canonicalize().unwrap())
                    .unwrap();
                let handle = dir.create_read_write(Path::new("new")).unwrap();
                match op.as_str() {
                    "read_at" => handle.read_at(&mut [0; 1], 0).map(drop),
                    "write_at" => handle.write_at(b"after", 0).map(drop),
                    "set_len" => handle.set_len(3),
                    "rw_len" => handle.len().map(drop),
                    "rw_identity" => handle.identity().map(drop),
                    "rw_sync" => handle.sync_all(),
                    _ => panic!("create_read_write ran past cut point"),
                }
            }
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
            "create_private_dir" => cut.create_private_dir(&root.join("dir")),
            "restrict_dir" => cut.restrict_dir(&root),
            "remove_dir_all" => cut.remove_dir_all(&root.join("victim")),
            "quarantine" => cut.quarantine(&file),
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
            use std::os::unix::fs::PermissionsExt;
            let root = scratch.path().join(op);
            fs::create_dir(&root).unwrap();
            fs::set_permissions(&root, fs::Permissions::from_mode(0o755)).unwrap();
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
            assert_eq!(
                fs::metadata(&root).unwrap().permissions().mode() & 0o777,
                0o755,
                "{op}"
            );
            if op == "remove_dir_all" {
                assert_eq!(fs::read(root.join("victim/keep")).unwrap(), b"before");
            }
            let new = root.join("new");
            match op {
                "create" | "create_rw" => assert!(!new.exists(), "create ran"),
                "write_at" | "set_len" => assert_eq!(fs::read(&new).unwrap(), b"", "{op} ran"),
                "write" => assert_eq!(fs::read(&new).unwrap(), b"", "write ran"),
                _ => {}
            }
        }
    }
}
