//! `sb-types` — L6. Shared vocabulary only: ids, timestamps, email
//! addresses, byte ranges, and the seam traits (`Fs`, `Clock`,
//! `Random`). No implementations and no dependencies on other `sb-*`
//! crates, so it can't become a catch-all.
#![forbid(unsafe_code)]

pub mod appearance;
pub mod fs;
pub mod id;
pub mod random;
pub mod time;
