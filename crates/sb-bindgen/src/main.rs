//! `sb-bindgen` — tool. `uniffi-bindgen-swift` pinned to the workspace's
//! `uniffi` version so bindings and runtime can't drift. The UniFFI
//! dependency lands in M0.8 with `sb-ffi`'s exports.

use std::process::ExitCode;

fn main() -> ExitCode {
    eprintln!("sb-bindgen: UniFFI is not wired in yet (lands in M0.8)");
    ExitCode::from(2)
}
