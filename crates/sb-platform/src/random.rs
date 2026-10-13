//! `OsRandom`: OS randomness behind the `Random` seam.

use std::io;

use sb_types::random::Random;

/// `getentropy` writes at most 256 bytes per call on both Linux and
/// macOS, and fails above it; larger requests are chunked.
const GETENTROPY_MAX: usize = 256;

/// OS randomness through `libc::getentropy`.
#[derive(Debug, Default, Clone, Copy)]
pub struct OsRandom;

impl Random for OsRandom {
    fn fill(&self, dest: &mut [u8]) -> io::Result<()> {
        for chunk in dest.chunks_mut(GETENTROPY_MAX) {
            // SAFETY: `chunk` points at `chunk.len()` writable,
            // initialized bytes (at most `GETENTROPY_MAX`, within
            // getentropy's limit) and getentropy writes nowhere else.
            let filled = unsafe { libc::getentropy(chunk.as_mut_ptr().cast(), chunk.len()) };
            if filled == -1 {
                return Err(io::Error::last_os_error());
            }
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn two_fills_of_32_bytes_differ() {
        let mut a = [0u8; 32];
        let mut b = [0u8; 32];
        OsRandom.fill(&mut a).unwrap();
        OsRandom.fill(&mut b).unwrap();
        assert_ne!(a, b, "two OS random fills came back identical");
    }

    /// Above `getentropy`'s per-call limit, so the chunking runs: every
    /// chunk, the short last one included, is written (an all-zero
    /// 232-byte random chunk has probability 2^-1856).
    #[test]
    fn a_1000_byte_fill_writes_every_chunk() {
        let mut big = [0u8; 1000];
        OsRandom.fill(&mut big).unwrap();
        for (index, chunk) in big.chunks(GETENTROPY_MAX).enumerate() {
            assert!(
                chunk.iter().any(|&b| b != 0),
                "chunk {index} left unwritten"
            );
        }
    }
}
