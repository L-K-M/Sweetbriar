//! `sb-bindgen` — tool. The Swift bindings generator pinned to the
//! workspace's `uniffi` version (spec/01), so bindings and runtime
//! can't drift. Delegates to UniFFI's own Swift CLI entry point;
//! `cargo xtask ffi` and `check-ffi` drive it.

fn main() {
    uniffi::uniffi_bindgen_swift()
}
