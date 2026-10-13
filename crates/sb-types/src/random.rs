//! The `Random` seam (spec/01 rule 5): the one source of randomness,
//! so ids made from it are reproducible in tests. `sb-platform`
//! implements the OS source.

use std::io;

/// The randomness seam: fill `dest` entirely with unpredictable bytes,
/// or fail. Ids (spec/02 rule 3) draw their bytes through it.
pub trait Random: Send + Sync {
    fn fill(&self, dest: &mut [u8]) -> io::Result<()>;
}
