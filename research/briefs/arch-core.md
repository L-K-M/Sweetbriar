# Topic: Architecture of a Rust core with native UIs (Swift on macOS, native toolkit on Linux)

Research how such an app should be built. Questions:

1. Case studies. How do shipping products share a Rust core with native
   UIs? For each: what lives in Rust vs native code, the FFI tooling, how
   async and live updates cross the boundary, what went well, what they
   regret. Cover at least: Element X (matrix-rust-sdk, UniFFI,
   matrix-sdk-ffi), Mozilla application-services (Firefox iOS/Android),
   Bitwarden SDK, 1Password's Rust core, Signal (libsignal), Delta Chat /
   chatmail core (Rust email-based messenger with several UIs), Mullvad VPN
   app, Proton (check whether Proton's 2025-2026 mail/pass/drive apps use a
   shared Rust core and how), Wire core-crypto, Red Badger's Crux framework,
   Thunderbird (Rust in desktop Thunderbird, e.g. its Exchange/EWS work, and
   Thunderbird for iOS). Add others you find relevant.
2. FFI tooling. Compare UniFFI (proc-macros, async/await support, callback
   interfaces, Swift 6 strict concurrency / Sendable, serialization
   overhead, generated code quality), swift-bridge, cbindgen + hand-written
   C/Swift glue, Diplomat, Crux typegen, BoltFFI and anything newer (verify
   it exists). Include performance numbers where available and maturity as
   of 2026.
3. Async across the boundary: who owns the tokio runtime, cancellation,
   mapping to Swift async/await and AsyncSequence, main-thread delivery,
   backpressure for streams of updates.
4. API granularity: coarse "view model" core (Crux, Elm-style) vs fine
   grained domain API; push vs pull; observing changes (e.g.
   matrix-rust-sdk's eyeball / VectorDiff streams for timelines); how to
   expose lists of 100k+ messages across FFI (windowed queries, row ids and
   lazy fetch, diffs); avoiding copies of large bodies.
5. Process model on macOS: single process vs a background sync agent
   (launchd agent / SMAppService login item / XPC service) that keeps
   syncing when the UI is closed; crash isolation; app extensions
   (notifications, share extension, Spotlight, Quick Look) that need the
   store (App Group container); SQLite access from several processes.
6. Build and tooling: xcframework packaging, cargo-swift or similar, Swift
   Package Manager integration, universal binaries (is Intel support still
   needed? macOS 26 support status for Intel), debugging and crash
   symbolication across Rust and Swift, binary size, CI.
7. Reuse on Linux: using the same core from a Rust GTK app directly (no
   FFI), or from other toolkits. What boundary keeps both front ends thin?
8. Propose a concrete layered architecture for Sweetbriar with an ASCII
   diagram: crates, responsibilities, and the strict layer boundaries
   (each layer only talks to the one directly below it). Name the crates
   you would create.
