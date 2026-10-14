# AGENTS

How agents build and test Sweetbriar. The binding rules live in
[plan/README.md](plan/README.md) ("Rules for agents") — read it first;
this file is the short operational summary.

## Commands

```sh
cargo xtask ci        # the full local gate; must be green before a PR
cargo xtask notices   # regenerate THIRD_PARTY_NOTICES.md
cargo xtask check-layers  # the spec/01 layer-boundary checker
cargo xtask ffi --bindings-only  # regenerate the committed Swift bindings
cargo xtask check-ffi     # bindings drift and import check + the async-callback lint
```

After changing `sb-ffi`'s exports, run `cargo xtask ffi --bindings-only`
and commit the regenerated files under `apps/macos/SweetbriarCore/`.
`cargo xtask ffi` without the flag also builds the XCFramework, which
needs macOS.

`check-layers` reads rustdoc's unstable JSON output, so it needs the
pinned nightly toolchain:

```sh
rustup toolchain install nightly-2026-10-10 --profile minimal
```

Building stays on the stable pin in `rust-toolchain.toml`; moving the
nightly pin is its own PR (spec/01 "Toolchain").

`cargo xtask ci` runs, in order: `cargo fmt --all --check`,
`cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace`, `cargo doc --workspace --no-deps` with
`RUSTDOCFLAGS="-D warnings"`, `cargo xtask check-layers`,
`cargo deny check`, and the `THIRD_PARTY_NOTICES.md` drift check
(regenerates to a temp file and diffs; regenerate with
`cargo xtask notices` and commit the result).

Building `sb-store` compiles the bundled SQLite (spec/03), so a C
compiler must be on `PATH`. Its compile options come from
`LIBSQLITE3_FLAGS` in `.cargo/config.toml`.

## What runs where

**Linux, every PR — agents verify locally:**

- fmt, clippy, tests and docs (`cargo xtask ci`, all steps above);
- `cargo xtask check-layers` (the spec/01 layer-boundary rules in
  `layers.toml`);
- `cargo deny check` (licences, advisories, bans, sources);
- the notices drift check;
- the Dovecot protocol suite once M2 lands (`cargo xtask ci --docker`).

**Only in macOS CI (`.github/workflows/macos.yml`):**

- `cargo test --workspace --target aarch64-apple-darwin` (M0.1 shape);
- `cargo xtask check-layers` on the macOS toolchain — it sees the
  macOS-cfg re-export surface (spec/01);
- `check-ffi` and `xtask ffi` — both also run on Linux, except
  `check-ffi`'s Swift type-check and the XCFramework. `check-ffi`
  everywhere also lints the spec/08 import boundary: no `.swift` file
  under `apps/macos` outside `SweetbriarBridge`/`SweetbriarCore` may
  import `SweetbriarCore`/`SweetbriarCoreFFI`;
- XcodeGen (`apps/macos/project.yml` → `Sweetbriar.xcodeproj`,
  gitignored), `xcodebuild build test` on the `Sweetbriar` scheme —
  the Swift-Testing `SweetbriarBridgeTests` included — and the zipped
  `.app` artifact `Sweetbriar-app` (M0.9);
- the app-hosted `SweetbriarTests` bundle (M0.10, a `bundle.unit-test`
  target on the `Sweetbriar` scheme): the benchmark's Foundation-only
  statistics, report and comparison bars plus the shared row
  presentation and the context-click rule;
- the M0.10 list benchmark: the Release app launches with
  `-runListBenchmark <report.json>` after the Release zip step,
  `jq -e '.ok == true'` gates on its verdict, and the report uploads
  as the `list-benchmark` artifact — its numbers are diagnostic (the
  owner's Mac decides between the two lists);
- anything touching `apps/` or generated bindings.

Agents can't run these locally — make the YAML correct by inspection.

The owner runs the same benchmark from the app's Debug → Run List
Benchmark menu item; it finishes in a report window with a Copy
button. `-runListBenchmark` writes the file, prints a short summary
and quits — even a run that can't start writes `ok: false`.

## Rules for agents (summary of plan/README.md)

- One plan step per PR; all of that step's acceptance checks green.
- The spec wins; never silently deviate or weaken a check to pass.
- Never weaken safety rules to make a test pass.
- Tests come with the code; bug fixes start with a failing test.
- Dependencies: exact pinned versions, `cargo deny` must pass, each new
  one justified in the PR body.
- No owner data in the repository; fixtures are synthetic or freely
  licensed.
- Keep PRs reviewable and inside the spec/01 module layout.
