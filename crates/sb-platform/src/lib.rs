//! `sb-platform` — L5. Secret store, clock, filesystem seams, paths and
//! platform facts: the real implementations of the `sb-types` seam
//! traits. Platform services enter the core only through here.
//!
//! Unsafe code lives only in this crate (spec/01), each block with a
//! `// SAFETY:` comment.
#[cfg(not(any(target_os = "linux", target_os = "macos")))]
compile_error!("sb-platform supports Linux and macOS only");

mod clock;
mod fs;
mod location;
mod random;
mod roots;

pub use clock::SystemClock;
pub use fs::RealFs;
pub use random::OsRandom;
pub use roots::{default_cache_root, default_data_root};

// Adjacent item exports let resources compose platform seams without a store.
pub use sb_types::fs::{Fs, Lease, LockError};
pub use sb_types::random::Random;
