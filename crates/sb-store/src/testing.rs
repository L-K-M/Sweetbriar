//! Helpers the crate's tests share: deterministic seams and scratch
//! stores.

use std::sync::Arc;

use sb_test_support::{FixedClock, ScratchDir, SeededRandom};
use sb_types::fs::Fs;
use sb_types::time::Timestamp;

use crate::{Seams, StorePaths};

/// The moment tests open at (2025-01-01T00:00:00Z).
pub(crate) const FIXED_NOW_MS: i64 = 1_735_689_600_000;

/// Deterministic seams over `fs`: one clock value and one seed, so
/// `created_at` and the UUIDs are reproducible.
pub(crate) fn test_seams(fs: Arc<dyn Fs>) -> Seams {
    Seams::new(
        fs,
        Arc::new(FixedClock::new(Timestamp::from_millis(FIXED_NOW_MS))),
        Arc::new(SeededRandom::new(42)),
    )
}

/// A fresh data root under a unique scratch dir, as `StorePaths`.
pub(crate) fn scratch_paths(label: &str) -> (ScratchDir, StorePaths) {
    let scratch = ScratchDir::new(label);
    let paths = StorePaths::new(scratch.path().join("store")).unwrap();
    (scratch, paths)
}
