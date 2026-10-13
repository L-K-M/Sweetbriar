//! `sb-test-support` — dev-only. The real seams (`Fs`, `Clock`,
//! `Random`) and scratch directories for tests, plus (in later steps)
//! the fake IMAP/SMTP server, fixtures loader and crash-cut harness.
//! Not part of the shipping core; other crates use it only from
//! tests.

use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::sync::Mutex;
use std::sync::atomic::{AtomicU64, Ordering};

use sb_platform::{OsRandom, RealFs, SystemClock};
use sb_types::fs::Fs;
use sb_types::random::Random;
use sb_types::time::{Clock, Timestamp};

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
    use super::*;

    #[test]
    fn fixed_clock_returns_its_timestamp() {
        let clock = FixedClock::new(Timestamp::from_millis(42));
        assert_eq!(clock.now().as_millis(), 42);
        assert_eq!(clock.now().as_millis(), 42);
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
