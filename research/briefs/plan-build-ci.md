# Topic: Build, signing and CI pipeline for Sweetbriar (Rust core + Swift macOS app + Linux)

Sweetbriar will be implemented mostly by AI agents working in a Linux
environment; the owner reviews and merges PRs. macOS builds and tests must
run on **GitHub Actions macOS runners**; the owner runs the app on their
own Mac. The repo is `L-K-M/Sweetbriar` on GitHub. The project snapshot is
in `./research/` (architecture in `research/01-architecture.md`, macOS UI
in `research/08-macos-ui.md`, distribution D12).

An implementation plan is about to be written. Give the concrete,
verified facts and a recommended setup:

1. **GitHub-hosted macOS runners** as of 2026-10: available images
   (macos-15, macos-26, arm64?), Xcode versions installed (Xcode 26.x/27?),
   whether macOS 27 images exist yet, default Swift version, Rust
   availability; minute multipliers and free-tier limits for **private**
   vs public repos; concurrency limits; typical cold build times. Cite
   GitHub's runner-images repository and docs.
2. **Rust → Swift pipeline**: building the Rust core for
   `aarch64-apple-darwin` (and `x86_64` if needed), UniFFI 0.32.x binding
   generation (Swift), packaging as an XCFramework or a local Swift
   package, `cargo-swift` vs a small `xtask`; caching (sccache,
   Swatinem/rust-cache); reproducibility.
3. **Swift app project**: Xcode project vs Swift Package Manager for an
   AppKit app; `xcodebuild` in CI, unit tests (XCTest/Swift Testing), UI
   tests on runners (do they work headless?), strict Swift 6 concurrency
   checking.
4. **Signing and notarization in CI**: Developer ID certificates in
   GitHub secrets, keychain setup, `notarytool` with App Store Connect API
   keys, hardened runtime, Sparkle EdDSA signing; what can wait until a
   first distributable build; how unsigned/ad-hoc builds run on the
   owner's Mac meanwhile (Gatekeeper, `xattr`, local "Sign to Run
   Locally").
5. **Linux CI**: ubuntu runners for the Rust core (fmt, clippy, tests,
   `cargo-deny` licence/advisory checks), and later GTK4/libadwaita
   builds.
6. **Repository layout** that works for agents and CI: Cargo workspace,
   crates vs modules, `apps/macos`, `apps/linux`, `xtask`, test fixtures,
   docs; branch protection and required checks.
7. **Agent workflow constraints**: agents can't run Xcode locally, so
   which checks must be Linux-runnable, which only in macOS CI, and how to
   keep the macOS feedback loop short (e.g. a fast "build + unit test"
   macOS job per PR, slower UI tests nightly).
8. **Recommendation**: a concrete CI matrix and step-by-step bootstrap
   order (first workflow files to create), with the costs and the
   strongest counter-argument.
