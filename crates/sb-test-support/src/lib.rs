//! `sb-test-support` — dev-only. The real seams (`Fs`, `Clock`,
//! `Random`) and scratch directories for tests, plus the fault and
//! crash-injection filesystems (spec/11) and the crash-cut harness.
//! Not part of the shipping core; other crates use it only from
//! tests.

mod cut;
mod fault;
mod sim;
mod sqlite;
#[cfg(target_os = "linux")]
mod threads;

use std::future::Future;
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};
use std::thread;

use sb_platform::{OsRandom, RealFs, SystemClock};
use sb_types::fs::Fs;
use sb_types::random::Random;
use sb_types::time::{Clock, Timestamp};

pub use cut::{CutFs, run_crash_cut_points, run_crash_cuts};
pub use fault::{FaultFs, FaultKind, FsOp};
pub use sim::SimFs;
pub use sqlite::{execute_sql, store_meta_value};
#[cfg(target_os = "linux")]
pub use threads::with_new_threads_refused;

/// Drive `future` to completion on the calling thread: a minimal
/// executor for tests, with no runtime dependency (spec/01 keeps the
/// store's future runtime-free; tests await it with this). The waker
/// unparks the thread, so a wake from the writer thread resumes the
/// wait; spurious parks are harmless because the future is re-polled.
pub fn block_on<F: Future>(future: F) -> F::Output {
    use std::pin::pin;
    use std::task::{Context, Poll, Wake, Waker};

    /// Wakes by unparking the blocked test thread. `Thread::unpark`
    /// leaves a permit, so an unpark that lands before the `park`
    /// below cannot be lost.
    struct ThreadParker(thread::Thread);

    impl Wake for ThreadParker {
        fn wake(self: Arc<Self>) {
            self.0.unpark();
        }
    }

    let waker = Waker::from(Arc::new(ThreadParker(thread::current())));
    let mut cx = Context::from_waker(&waker);
    let mut future = pin!(future);
    loop {
        match future.as_mut().poll(&mut cx) {
            Poll::Ready(output) => return output,
            Poll::Pending => thread::park(),
        }
    }
}

/// The real `Fs` implementation, for tests that exercise actual
/// filesystem behaviour (spec/01 rule 5).
pub fn real_fs() -> Arc<dyn Fs> {
    Arc::new(RealFs)
}

/// The system clock, for tests that need time to pass (spec/01
/// rule 5).
pub fn system_clock() -> Arc<dyn Clock> {
    Arc::new(SystemClock)
}

/// OS randomness, for tests that need ids that can't collide.
pub fn os_random() -> Arc<dyn Random> {
    Arc::new(OsRandom)
}

/// A clock that always returns the `Timestamp` it was built with, so
/// `created_at`-style values are reproducible.
pub struct FixedClock {
    at: Timestamp,
}

impl FixedClock {
    pub fn new(at: Timestamp) -> Self {
        Self { at }
    }
}

impl Clock for FixedClock {
    fn now(&self) -> Timestamp {
        self.at
    }
}

/// The odd constant of splitmix64.
const SPLITMIX_GAMMA: u64 = 0x9E37_79B9_7F4A_7C15;

/// Deterministic randomness (spec/01 rule 5): splitmix64 over a seed,
/// so two draws in a row differ but the same seed replays the same
/// byte stream.
pub struct SeededRandom {
    state: Mutex<u64>,
}

impl SeededRandom {
    pub fn new(seed: u64) -> Self {
        Self {
            state: Mutex::new(seed),
        }
    }
}

impl Random for SeededRandom {
    fn fill(&self, dest: &mut [u8]) -> io::Result<()> {
        let mut state = self.state.lock().expect("a panicking thread held the seed");
        for chunk in dest.chunks_mut(8) {
            *state = state.wrapping_add(SPLITMIX_GAMMA);
            let mut z = *state;
            z = (z ^ (z >> 30)).wrapping_mul(0xBF58_476D_1CE4_E5B9);
            z = (z ^ (z >> 27)).wrapping_mul(0x94D0_49BB_1331_11EB);
            z ^= z >> 31;
            for (slot, byte) in chunk.iter_mut().zip(z.to_le_bytes()) {
                *slot = byte;
            }
        }
        Ok(())
    }
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

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use super::*;

    #[test]
    fn fixed_clock_returns_its_timestamp() {
        let clock = FixedClock::new(Timestamp::from_millis(42));
        assert_eq!(clock.now().as_millis(), 42);
        assert_eq!(clock.now().as_millis(), 42);
    }

    /// `block_on` returns an already-ready future's output without
    /// parking, and one that pends first: the waker registered by the
    /// executor is called from another thread, as the store's writer
    /// thread resolves a submitted write.
    #[test]
    fn block_on_returns_ready_and_pending_futures() {
        assert_eq!(block_on(async { 7 + 35 }), 42);

        /// Pends once, wakes from another thread, then completes.
        struct PendOnce {
            polled: bool,
        }
        impl Future for PendOnce {
            type Output = u32;
            fn poll(
                mut self: std::pin::Pin<&mut Self>,
                cx: &mut std::task::Context<'_>,
            ) -> std::task::Poll<u32> {
                if self.polled {
                    std::task::Poll::Ready(42)
                } else {
                    self.polled = true;
                    let waker = cx.waker().clone();
                    thread::spawn(move || {
                        thread::sleep(Duration::from_millis(20));
                        waker.wake();
                    });
                    std::task::Poll::Pending
                }
            }
        }

        assert_eq!(block_on(PendOnce { polled: false }), 42);
    }

    /// Two draws in a row differ, so two UUIDs made back to back
    /// (store_uuid and incarnation) come out distinct.
    #[test]
    fn seeded_random_draws_differ_within_and_across_fills() {
        let random = SeededRandom::new(7);
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        random.fill(&mut a).unwrap();
        random.fill(&mut b).unwrap();
        assert_ne!(a, b, "two draws from one seed are identical");
        assert_ne!(&a[..8], &a[8..16], "one 16-byte draw repeats itself");

        // The same seed replays the same stream over every byte: a
        // differently pre-filled buffer of an odd length (a short last
        // chunk) comes out identical.
        let mut zeros = [0u8; 37];
        let mut ones = [0xFFu8; 37];
        SeededRandom::new(7).fill(&mut zeros).unwrap();
        SeededRandom::new(7).fill(&mut ones).unwrap();
        assert_eq!(zeros, ones, "a fill left bytes unwritten");
        assert_eq!(a, zeros[..32]);

        let mut other = [0u8; 32];
        SeededRandom::new(8).fill(&mut other).unwrap();
        assert_ne!(a, other, "different seeds produced the same bytes");
    }
}
