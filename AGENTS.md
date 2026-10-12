# AGENTS

How agents build and test Sweetbriar. The binding rules live in
[plan/README.md](plan/README.md) ("Rules for agents") — read it first;
this file is the short operational summary.

## Commands

```sh
cargo xtask ci        # the full local gate; must be green before a PR
cargo xtask notices   # regenerate THIRD_PARTY_NOTICES.md
cargo xtask check-layers  # M0.2 — not implemented yet
```

`cargo xtask ci` runs, in order: `cargo fmt --all --check`,
`cargo clippy --workspace --all-targets -- -D warnings`,
`cargo test --workspace`, `cargo doc --workspace --no-deps` with
`RUSTDOCFLAGS="-D warnings"`, `cargo deny check`, and the
`THIRD_PARTY_NOTICES.md` drift check (regenerates to a temp file and
diffs; regenerate with `cargo xtask notices` and commit the result).

## What runs where

**Linux, every PR — agents verify locally:**

- fmt, clippy, tests and docs (`cargo xtask ci`, all steps above);
- `cargo deny check` (licences, advisories, bans, sources);
- the notices drift check;
- `check-layers` once M0.2 lands;
- the Dovecot protocol suite once M2 lands (`cargo xtask ci --docker`).

**Only in macOS CI (`.github/workflows/macos.yml`):**

- `cargo test --workspace --target aarch64-apple-darwin` (M0.1 shape);
- `xtask ffi` / `check-ffi`, the XCFramework, `xcodebuild` and Swift
  tests (M0.8/M0.9);
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
