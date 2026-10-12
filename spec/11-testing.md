# 11 — Testing and CI

Rests on research [04-data-safety](../research/04-data-safety.md)
(testing strategy), D3 in
[debate/04-resolution](../research/debate/04-resolution.md) and
[raw/plan-build-ci/devin.md](../research/raw/plan-build-ci/devin.md).

## Test layers

| Layer | What | Runs on |
|---|---|---|
| **Unit** | Pure functions: parsers, query AST, color tokens, state transitions | Linux CI, every PR |
| **Store** | Schema, migrations, publication protocol, GC, backup and restore, against a temp directory | Linux CI, every PR |
| **Model** | `proptest` state machines: intent planning and reconciliation against an in-memory IMAP model; outbox transitions | Linux CI, every PR (bounded cases); nightly (more cases) |
| **Protocol** | The IMAP and SMTP engine against a scripted fake server (below) and against Dovecot in Docker | Linux CI, every PR |
| **Fault injection** | Crash cuts and I/O faults at every durable transition (below) | Linux CI, every PR (sampled); nightly (full) |
| **Fuzz** | MIME parser, mbox splitter, query parser (`cargo-fuzz`) | Nightly, Linux |
| **FFI** | Rust built for `aarch64-apple-darwin`; UniFFI Swift bindings generated and compiled under Swift 6 strict concurrency; binding drift check | macOS CI, PRs touching `crates/` or `apps/` |
| **Swift unit** | Adapter, view models (Swift Testing) | macOS CI, same PRs |
| **UI** | XCUITest smoke flows: launch, open mailbox, open message, compose and save draft | macOS nightly |
| **Manual qualification** | On the owner's Mac against real Proton Bridge: the checklist per milestone (plan) | Owner, per milestone |

Every behaviour in the spec that says **must** has at least one automated
test, except where the spec says "manual qualification".

## The fake IMAP/SMTP server

- An in-process, scriptable server in a `test-support` module, built on
  the same IMAP codec as the client (see 01 for the crate).
- **Scripts** are sequences of expected commands and canned responses,
  plus fault knobs:
  - drop the connection after command N, or after a tagged OK;
  - change UIDVALIDITY between sessions;
  - expunge or move messages behind the client's back;
  - return partial FETCH results;
  - delay;
  - advertise or withhold capabilities (`UIDPLUS`, `MOVE`, `CONDSTORE`,
    `QRESYNC`).
- **A "Bridge profile"** configures it like Proton Bridge per 04:
  capabilities, label and folder layout, the same message in several
  mailboxes, and SMTP saving Sent itself.
- **Dovecot** (Docker, pinned image) runs the same protocol suite against
  a real server in CI. Dovecot doesn't emulate Bridge's quirks; the
  Bridge profile covers those.

## Fault injection

- **Seams:** the store and engine take `Clock`, `Fs` and `Transport`
  traits, so tests can inject failures. Production uses real
  implementations; these seams exist from the first commit.
- **Crash cuts:**
  - **How:** a test harness runs the operation in a child process and
    kills it (`SIGKILL`) at a numbered cut point. It then reopens the store
    and checks invariants.
  - **Where** — at minimum:
    - each step of object publication (03);
    - before, inside and after each write transaction;
    - after an IMAP command is sent but before its response;
    - after SMTP `DATA` but before the final reply;
    - during backup and restore;
    - during migrations.
- **I/O faults:** short writes, `ENOSPC`, `EIO` on `fsync`, `rename`
  failure. Each must produce an error, never a reported success.
- **Invariants** checked after every cut:
  1. Every acknowledged draft revision and receipt exists.
  2. No row references a missing object.
  3. No send attempt was submitted twice. The fake server counts
     submissions.
  4. No intent's remote effect happened without its intent row, and no
     done intent lacks its effect.
  5. `PRAGMA integrity_check` and `foreign_key_check` pass.
- **Power loss:** `kill -9` is not power-loss evidence. Power-loss
  simulation runs nightly on two layers that each drop unflushed writes:
  - an `Fs` implementation for object files that buffers until `sync`;
  - a **test SQLite VFS** wrapping the default VFS (through
    `libsqlite3-sys`, or a crate such as `sqlite-vfs`, pinned at use).
    It tracks writes since the last `xSync` on the database, WAL and
    journal files, and discards them on a simulated power cut. Bundled
    SQLite does its own file I/O, so the `Fs` seam alone can't cover
    it. Real power-cut tests on the
  owner's Mac are a manual qualification before the pilot is declared
  done.

## Fixtures

`fixtures/` holds:
- **Messages:** a corpus of `.eml` files covering multipart variants,
  charsets, format=flowed, TNEF, malformed MIME, huge headers, deep
  nesting and hostile HTML (scripts, CSS exfiltration, remote images,
  CID traversal).
- **Imports:** Apple Mail `.mbox` export samples; mboxo and mboxrd files.
- **Bridge-shaped mailboxes:** dumps for the fake server.
- **Owner-consented messages:** none in the repository (public repo).
  Real-mail tests run only on the owner's Mac.

## CI

The repository is public, so GitHub-hosted runners are free.

| Workflow | Trigger | Runner | Jobs |
|---|---|---|---|
| `ci.yml` | every PR and push to `main` | `ubuntu-24.04` | `cargo fmt --check`; `cargo clippy --workspace --all-targets -- -D warnings`; `cargo test --workspace`; `cargo doc` (deny broken links); `cargo xtask check-layers`; `cargo deny check`; `cargo about generate` to a temp file and diff against `THIRD_PARTY_NOTICES.md`; Dovecot protocol suite |
| `macos.yml` | **every** PR and push to `main` | `macos-26` | A `changes` job (path filter on `crates/`, `xtask/`, `apps/`, `Cargo.toml`, `Cargo.lock`, `.cargo/`, `rust-toolchain.toml` and `.github/workflows/macos.yml`); the build jobs run only if it matched (job-level `if:`): `cargo test` (aarch64); `cargo xtask ffi`; `cargo xtask check-ffi` (drift); `xcodegen generate`; `xcodebuild build` + `test` with Swift 6 strict concurrency; upload `.xcresult` on failure. A final `macos` job always runs and succeeds when the build jobs succeeded or were skipped, so the required check never hangs |
| `nightly.yml` | schedule (daily) + manual | `ubuntu-24.04`, `macos-26`, `xcode-27` (advisory) | full fault-injection and proptest runs, fuzzing (time-boxed), XCUITest smoke, unsigned `.dmg` artifact |
| `release.yml` | tag `v*` | `macos-26` | unsigned (ad-hoc) `.dmg` attached to the GitHub Release; Developer ID signing, notarization and Sparkle come later (plan) |

**Rules:**
- **Pinning:** runners are pinned (`macos-26`, not `macos-latest`). Every
  job logs `rustc -Vv`, `xcodebuild -version` and the image version.
- **Workflow hygiene:**
  - `permissions: contents: read` by default;
  - `concurrency` with cancel-in-progress for PRs only;
  - `timeout-minutes` on every job;
  - third-party actions pinned to commit SHAs.
- **Caching:** `sccache` for Rust (with `Swatinem/rust-cache` as the
  fallback) and SPM package caching keyed on `Package.resolved`. No
  DerivedData caching.
- **Branch protection on `main`:** required checks are `ci` and `macos`.
  Each is a final aggregate job that always runs and succeeds only when
  its workflow's other jobs succeeded (or, for `macos`, were skipped).
  The nightly checks are advisory.
- **What agents must run before opening a PR:** `cargo xtask ci`, which
  runs everything in `ci.yml` except the Dovecot suite, available as
  `cargo xtask ci --docker`. macOS-only checks are verified by the
  `macos.yml` job. `AGENTS.md` says which is which.
