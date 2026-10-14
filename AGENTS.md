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
  `check-ffi`'s Swift type-check and the XCFramework; `xcodebuild`
  and Swift tests (M0.9);
- anything touching `apps/` or generated bindings.

Agents can't run these locally — make the YAML correct by inspection.

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
