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
- the hostile rendering corpus (`cargo test -p sb-mime --test hostile`);
- the Dovecot protocol suite once M2 lands (`cargo xtask ci --docker`).

On pushes to main, CI's `window-timings` job also records the M1.3
list window timings over 100k rows; they are never asserted. Run them
locally with `cargo test -p sb-engine --release --lib
list_window_timings -- --ignored --nocapture`.

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
- the composer probe's app-hosted tests in the same bundle (M0.11):
  ⌘B as one undoable edit, marked text left unstyled until commit,
  `isOnTextKit2` staying true, the accessibility value, and the
  Foundation-only `MarkdownStyler`/`MarkdownFormatting` suites;
- the import flow XCUITest (M1.4l, `SweetbriarUITests/ImportFlowTests`
  on the `SweetbriarSmoke` scheme): a bundled `.mbox` through File →
  Import… into fresh roots (`-dataRoot`, `-cacheRoot`, `-importPath`),
  the inventory, the report window, then the mailbox's two rows;
- the search flow XCUITest (M1.6p, `SweetbriarUITests/SearchFlowTests`
  on the same scheme): a Search typed into an empty archive follows an
  import's invalidations, then body, subject and address queries, the
  reader, scope and sort, a zero-hit coverage note with its details,
  and clearing back to the mailbox;
- the index controls XCUITest (M1.6p2, `SweetbriarUITests/
  IndexControlFlowTests`): over a generated 5,001-message archive, a
  rebuild shows standalone progress while Search can't open, Cancel
  pauses it, a relaunch keeps it paused, Resume completes it; then an
  unreadable object store pauses the index phase on I/O, the retries
  move the note on an open Search, and Retry Now finishes it;
- the M0.10 list benchmark: the Release app launches with
  `-runListBenchmark <report.json>` after the Release zip step; the
  step passes when all four runs happened without a generation abort
  and AppKit's cold run passed its checks (SwiftUI's failures are
  findings, kept in the report's `ok` and failures; AppKit's warm run
  inherits SwiftUI's footprint on CI), and
  the report uploads as the `list-benchmark` artifact — its numbers
  are diagnostic (the owner's Mac decides between the two lists);
- anything touching `apps/` or generated bindings.

Agents can't run these locally — make the YAML correct by inspection.

M0.12 adds `nightly.yml` (daily/manual): Swift unit tests, the
`SweetbriarSmoke` XCUITest launch and an ad-hoc signed DMG. `xcode-27`
is advisory; fault/proptest placeholders stay skipped until their
suites exist. M1.1 enables the 60-second Linux MIME fuzz run; reproduction
and its isolated nightly toolchain are documented in `fuzz/README.md`.
Tags `v*` run the Rust CI gate and the same macOS package
checks before publishing the DMG. The tag must match the workspace
version; keep the README and XcodeGen marketing version in step.

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
<!-- shared-rules:start -->

## Working practices

- Follow explicit task instructions over the default workflow below.
- Writing the code is not finishing the task. A task is finished when
  its changes are merged to main through a PR that passed CI and review,
  or when the user explicitly accepts a different end state.
- Start every task on current code. Fetch first, then cut the task
  branch from origin/main — never from a stale local branch or an old
  checkout. To continue existing work, rebase or merge the latest
  origin/main into it before editing. Never overwrite existing work to
  update.
- Resolve ambiguity before making consequential changes. State low-risk
  assumptions; ask when scope, safety, or expected behavior is unclear.
- Keep changes focused. Do not modify unrelated code, formatting, or comments.
- Prefer surgical edits over whole-file rewrites when the result is equivalent.
- Stage only intended files. Inspect the diff before committing.

## Communication

- Be concise, factual, and direct. Preserve necessary context and uncertainty.
- Avoid praise, motivational filler, emojis, and em dashes in new prose.
- Address the reader directly in user-facing copy.
- Report what was verified and what remains unverified. Never imply that an
  unavailable check passed.

## Code design

- Prefer early returns and shallow nesting. Separate logical blocks with
  blank lines.
- Use descriptive constants or enums for meaningful or repeated values.
  Use existing standard definitions for protocol/specification constants.
  Keep obvious, one-off values inline.
- Use enums for behavioral modes that would otherwise require ambiguous
  boolean arguments.
- Default members to private. Widen visibility only for required consumers,
  and review the change as an API design decision.
- Follow the repository's declared dependency boundaries. UI and controllers
  must use application services rather than directly accessing databases,
  subprocesses, sockets, or other low-level mechanisms.
- Encapsulate low-level mechanics behind domain-oriented interfaces.
- Reuse genuinely shared logic. Avoid speculative abstractions and layers
  that only forward calls.
- Prefer pure functions for business rules and immutable data where practical.
  Isolate side effects; document non-obvious state ownership or synchronization.
- Explain non-obvious intent, constraints, and tradeoffs in comments.
  Do not narrate obvious code. Add examples or diagrams when they clarify it.

## Validation and errors

- Validate untrusted input at entry points. Where practical, represent valid
  states in types and enforce persistent invariants in database schemas.
- Represent absence and failure explicitly.
- Use assertions for internal programming invariants, not external-input
  validation or required runtime error handling.
- Prefer explicit, actionable errors over silent failure or undocumented
  fallback. Document intentional recovery behavior.
- Never report a skipped or failed operation as successful.

## Bug fixes

1. Identify the root cause and define an observable success criterion.
2. Add a regression test and observe the relevant failure before fixing it.
3. Implement the fix and observe the test passing.
4. Check surrounding behavior for regressions and architectural consistency.

If an automated regression test is impractical, document the reproduction
and verification procedure. State any inability to reproduce the failure.

## Verification

- Run relevant tests and lint after changes.
- Choose coverage by affected behavior and risk, not patch size.
- Use integration or end-to-end tests for critical workflows and boundaries;
  test isolated business rules at the lowest effective level.
- Run broader suites for cross-cutting or high-risk changes, and the full
  required release checks before releasing.
- Validate the requested command, options, platform, and configuration.
  Unrelated green CI is not proof that the reported problem is fixed.
- Recheck after the final edit. Distinguish local checks from CI results.

## Commit messages

- Use a capitalized, imperative subject without a final period.
- Target 50 characters; never exceed 72.
- Separate the subject and body with one blank line.
- Wrap body text at 72 characters.
- Explain what changed and why. Leave implementation mechanics to the code.

## Implementation and review

Unless explicitly instructed otherwise:

1. Work on a focused branch cut from the latest origin/main and open a PR
   against main before reporting the task as done.
2. Inspect CI results and completed review feedback for the latest commit.
   A successful reviewer job does not mean the review found no problems.
3. Address important findings or explain why they do not apply. Handle minor
   findings according to the stopping rules below.
4. Evaluate each fix in the surrounding project, add regression coverage,
   and rerun affected checks before pushing.
5. Repeat until a stopping criterion is met.
6. Merge without asking again once the stopping criterion is met, required
   checks pass on the latest commit, and no unresolved blockers or required
   human review requests remain.

### Reviewer context limits

The automated PR reviewer does not see the user's original prompt or
conversation. It may suggest changes that go against or beyond what the
user asked for. Do not implement such suggestions. Note each conflict and
report it to the user at the end of the thread.

### Automated review stopping rules

Judge findings by verified impact, not the reviewer's severity label.
Important findings concern correctness, security, data loss, broken builds,
or materially degraded behavior/performance.

Track completed review rounds and consecutive rounds without important
findings. Reruns of the same revision and integration failures do not count.

- No applicable actionable feedback: finish immediately.
- First minor-only round: optionally fix worthwhile, low-risk findings.
  Do not manufacture another push merely to obtain another review.
- Two consecutive rounds without important findings: stop responding to
  automated nitpicks, even if actionable minor suggestions remain.
  Defer worthwhile leftovers rather than continuing the cycle.
- A confirmed important finding resets the minor-only streak. Address it
  and verify the fix before continuing.

After ten completed rounds, enter stabilization:

- Stop optional cleanup, refactoring, and nitpick fixes.
- One completed review without confirmed important findings is sufficient
  to finish, even if minor suggestions remain.
- Continue only for confirmed important defects. If resolving them stalls,
  report the blockers rather than continuing indefinitely.

These limits end optional automated-feedback work. They do not waive
confirmed blockers, unresolved human review requests, or required checks.

### Reviewer integration failures

After two consecutive reviewer-integration failures, stop and report the
review gap. Do not treat failures as approval. An explicit user instruction
may waive review; report that waiver rather than claiming review passed.

## Ending a task

- A task ends with its changes merged to main — not with code written,
  and not with a PR merely opened. An open PR is work in progress:
  monitor CI on the latest commit, address review findings per the
  stopping rules, and merge once the criteria are met.
- Never finish with uncommitted changes or unpushed commits in the
  worktree. Commit, push, and open or update the PR first.
- If a step is impossible (missing push access, CI failure, reviewer
  outage), report the exact blocker instead. Never present unreviewed or
  unmerged work as finished.
- Before finishing, confirm: the requested behavior is implemented
  without unrelated changes; relevant checks pass on the latest code;
  important review findings are addressed or rejected with reasons;
  deferred suggestions, remaining risks, and validation gaps are
  disclosed.
- The final response states where the work stands: branch, PR, CI
  status, review rounds completed, and whether it is merged.

<!-- shared-rules:end -->
