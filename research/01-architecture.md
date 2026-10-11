# 01 — Architecture: a Rust core under native UIs

Synthesis of [raw/arch-core/devin.md](raw/arch-core/devin.md) and
[raw/arch-core/codex.md](raw/arch-core/codex.md), with input from the macOS,
Linux, performance and data-safety reports. Checked 2026-10-11.

## Summary

- **The pattern is proven, including for email.** Proton rebuilt its mobile
  Mail apps on a shared Rust core in 2025 (~80% shared code, iOS crash rate
  0.12% → 0.05%, company-reported). Element X runs on matrix-rust-sdk
  through UniFFI, Firefox shares Rust components the same way, Delta Chat
  ships a Rust email engine, and Ghostty puts one core under SwiftUI and
  GTK4 shells.
- **The cost is also proven.** Element's parity took longer than planned,
  and the SDK work turned out harder than the app work. 1Password abandoned
  its SwiftUI Mac app. Dropbox dropped its shared C++ core in 2019.
  Thunderbird runs three separate native stacks. A shared engine does not
  make each native UI cheap: start with one platform and a narrow vertical
  slice.
- **The boundary is commands plus windowed, observable query projections.**
  Not a fine-grained SDK (chatty, unstable). Not a Crux-style whole-app
  view model (serializes large models, absorbs platform navigation).
- **Rust owns meaning and durability.** That covers identity, MIME,
  storage, sync, search, rules, templates, drafts, the send queue and undo.
  Swift and GTK own presentation, editing state, accessibility and
  platform services.
- **Use UniFFI 0.32 behind a hand-written adapter.** The adapter adds
  cancellation, AsyncSequence and main-actor delivery. Keep the façade
  small, so that a later move to BoltFFI or a hand-written C ABI stays
  contained.
- **Start with one process, and keep the writer movable.** If sync after
  quitting becomes a release requirement, move the sole store owner into
  an `SMAppService` per-user agent, without changing the API.

## Layering

The user's rule applies: each layer talks only to the layer directly below.
Codex's crate graph follows it.

```
 L0  Sweetbriar.app (AppKit + SwiftUI)        sweetbriar-gtk (GTK4/libadwaita)
     app extensions, optional agent host        sweetbriar-cli
            │                                          │
 L1  SweetbriarBridge (Swift adapter)          sb-native-client (Rust)
     sb-ffi (UniFFI exports)                    no FFI on Linux
     [local or XPC/socket transport hidden here]
            └───────────────────┬──────────────────────┘
 L2                         sb-service
                commands + idempotency keys + receipts,
                query windows, subscriptions, public DTOs, errors
                                │
 L3                         sb-engine
                EngineHost (owns the one Tokio runtime), sync scheduler,
                drafts, outbox, rules, undo, reconciliation, index jobs
                                │
 L4                        sb-resources
                one façade over the leaves; maps types, errors, capabilities
                                │
 L5   sb-store      sb-mime     sb-search     sb-platform    sb-backend-*
      SQLite, FTS5, parse/      optional      Keychain /     imap, smtp, gmail,
      blobs, journal, build/    Tantivy       Secret Svc,    graph, jmap, ews,
      backups       sanitize                  files          proton (seam)
```

Rules:

- Leaves never call each other. Backends never write the store; the
  engine applies their deltas. Search never opens the authoritative DB.
- No `sb-common` imported everywhere. Types belong to the layer that
  defines them and are re-exported upward. Enforce allowed edges with
  `cargo metadata` checks in CI.
- Start with modules inside fewer crates and split them when the boundary
  is useful. The graph above is the target structure, not day-one
  scaffolding.

## The core/UI contract

### Commands

- Commands express intent: `archive_messages`, `save_draft`,
  `enqueue_send`, `apply_rule`, `undo_operation`.
- Durable commands take a caller-generated **idempotency key** and return a
  **receipt** once durably accepted. Abandoning the Swift waiter never
  cancels an accepted operation; status is queried by the same key.
- Cancellation is per operation. Queries, previews and searches are
  disposable. Cancelling a queued send is its own domain command.
- Send states are explicit: `Held` (undo window) → `Queued` → `Submitting`
  → `Accepted` | `Failed` | `OutcomeUnknown`. See
  [04-data-safety](04-data-safety.md).

### Queries and live updates

- `open_query(spec, anchor, limit)` returns a query id, a generation, count
  information, stable message ids and one batch of rows. Start experiments
  at 200 rows per page and a 600-row cache.
- Rows carry display fields only (ids, revision, sender, subject, date,
  flags, attachment marker, bounded snippet). Never MIME trees or bodies.
- Updates arrive as generation-tagged edits (insert, remove, update, move,
  reset), following matrix-rust-sdk's `VectorDiff`. Snapshot and
  subscription are established atomically.
- **Pull, don't push.** `Subscription.next_batch(limit)` with one
  outstanding pull, plus an optional coalesced "data available" wake-up.
  A bounded buffer that overflows returns `ResetRequired`; the UI then
  regenerates its window from authoritative state. A positional edit is
  never dropped silently. BoltFFI's streams drop events when full, so they
  would need the same reset handling.
- A query generation is not a long-lived SQLite read transaction (that
  would block checkpoints).
- Selection and bulk actions use a core-side selection token, not 100,000
  serialized ids.
- Bodies and attachments go through `MessageHandle` / `BodyLease`: chunks
  or a permission-scoped file. Never multi-megabyte buffers through
  UniFFI.

### Errors

Delta Chat's account-loss incident came from an ambiguous
`dc_is_configured()`: after an error, an encrypted account looked
unconfigured and UI cleanup could delete it. So errors are typed:
`NotFound`, `Locked`, `Unavailable`, `Corrupt`, `NeedsMigration`, and a
failed open never authorizes account creation or deletion.

### Where view models live

The question is still open; recommendation for v1:

- **Shared in Rust:** sorting, threading, search grammar, rule execution,
  draft semantics, undo, list windows and diffs.
- **Native:** focus, selection gestures, window layout, editor and IME
  state, menus.
- On macOS, Swift `@Observable` projections over core query windows. Since
  macOS 26, AppKit tracks Observation reads in `draw`/`layout`, so AppKit
  and SwiftUI views can share one model (devin, unverified).
- Composer text state stays native and submits revisions to the core. No
  FFI round trip per keystroke.

This differs from 1Password and Crux, which put view models in the core.
It keeps the front ends idiomatic at the cost of some duplication.

## Async and runtime

- One `EngineHost` per engine-hosting process owns a controlled Tokio
  runtime. Blocking SQLite runs on a dedicated store worker. MIME and
  indexing run on bounded CPU pools.
- Never `block_on` on the main thread, and never create a runtime per FFI
  call.
- UniFFI 0.32.1's Swift async template does not propagate Swift task
  cancellation into Rust. Add a `withTaskCancellationHandler` adapter with
  an explicit request token, and cover the race where cancellation arrives
  before the token (Signal's `TokioAsyncContext` handles this).
- Callbacks arrive on arbitrary threads. The Swift adapter applies ordered
  batches on `MainActor`, never one task per event.
- Data safety must not depend on cancellation: a dropped future stops at
  the next `.await`, possibly mid-operation. Invariants live in the store.

## Process model on macOS

| Model | Sync while UI closed | Isolation | Cost |
|---|---|---|---|
| In-process engine | Only while the app runs (windows closed is fine) | None | Lowest |
| XPC service | No (on demand, killed when idle) | Good | Medium |
| `SMAppService` LaunchAgent + IPC | Yes, while enabled | Good | IPC protocol, consent UI, upgrades |

Recommendation:

1. Ship in-process first, with the transport hidden in L1.
2. Move the store owner into a per-user agent if background sync and
   notifications after quit are required. An exclusive ownership lease
   ensures the agent and an in-process fallback never both write.
3. IPC needs its own versioned DTOs; UniFFI handles can't cross processes.
4. Extensions (Share, Quick Look, Spotlight, notifications) never become
   writers. They hand off through the agent, or through an atomic,
   versioned file in the App Group container that the engine consumes.

SQLite works across processes (WAL) but has no cross-process change
notification. One writer; everyone else goes through it.

## FFI tooling

| Tool | Status 2026-10-11 | Use |
|---|---|---|
| UniFFI 0.32.1 | Mature; Element, Mozilla, Bitwarden, Wire, Proton use it | Default. Strict Swift 6 compile of the actual exports is an early proof obligation (async foreign-trait fix merged July 2026) |
| BoltFFI | New (Dec 2025); Crux moved to it; author benchmarks 1.4× to 13.7× faster than UniFFI | Re-evaluate in 2027, or if a measured hot path fails |
| swift-bridge | Maintained, Swift-only | Alternative if UniFFI's Swift output disappoints |
| cbindgen + hand glue | Full control | Escape hatch for a proven hot path |
| Crux | Pre-1.0 whole-app framework | Not for the whole desktop UI |

## Build and packaging

- One Rust `staticlib` (`sb-ffi`) with all components, the Mozilla
  "megazord" approach. Package it as an XCFramework and consume it from a
  local SPM binary target inside the app repo, the pattern Firefox iOS
  moved to.
- `cargo-swift` lags UniFFI versions; a small `xtask` is the fallback.
- macOS 27 is Apple-silicon only. macOS 26 still runs on four Intel models.
  Build arm64 first; ship universal only if the support policy says so.
- Release builds: LTO, `panic = "abort"` in the FFI library (or
  `catch_unwind` at every export), stripped binaries with archived dSYMs.
  Expect a core of roughly 8–25 MB per architecture.
- CI: Rust tests on macOS and Linux, binding drift check, strict Swift 6
  compile, cancellation and subscription tests, signed agent and extension
  builds, crash and restore fixtures.

## Linux

The GTK4 app calls `sb-native-client` directly: same services, no FFI. The
GTK main context stays the UI executor, and the engine runtime returns
values over bounded channels. GTK list widgets recycle rows but don't page
data, so the app needs a paged `GListModel` over core query windows.
Fractal (GTK over matrix-rust-sdk) is the precedent. See
[09-linux](09-linux.md).

## Disagreements between the reports

| Topic | Devin | Codex | Resolution |
|---|---|---|---|
| Live updates | Push through UniFFI sink callbacks into AsyncStream | Pull `next_batch` with coalesced wake-ups and overflow reset | Pull. It gives backpressure and correctness under lag; a wake-up callback keeps latency low |
| Daemon | Phase 2 LaunchAgent, plan for it | Only if sync after quit is a release requirement | Same mechanism, different urgency. It's a product decision for the plan |
| Intel | arm64 only is fine | Universal if the policy covers Tahoe-era Intel | arm64 first; decide policy explicitly |

## Open questions

- Is sync after quit (and new-mail notifications with no app running) a
  v1 requirement?
- Should any interaction logic be shared Crux-style, for example a compose
  state machine?
- Do UniFFI's per-call costs matter for row batches at 120 Hz? Measure
  with the real DTOs before changing tools.

## Key sources

- [Proton: next-generation Mail apps](https://proton.me/blog/next-generation-proton-mail-mobile-apps)
- [matrix-sdk-ffi timeline](https://github.com/matrix-org/matrix-rust-sdk/blob/main/bindings/matrix-sdk-ffi/src/timeline/mod.rs)
- [Delta Chat: why JSON-RPC bindings exist](https://delta.chat/en/2025-02-11-why-jsonrpc-bindings-exist)
- [Element X retrospective](https://element.io/blog/element-x-and-pro-updates-a-glimpse-into-the-future/)
- [1Password 8 story](https://1password.com/blog/1password-8-the-story-so-far)
- [Dropbox: the cost of sharing code](https://dropbox.tech/mobile/the-not-so-hidden-cost-of-sharing-code-between-ios-and-android)
- [UniFFI changelog](https://github.com/mozilla/uniffi-rs/blob/main/CHANGELOG.md)
- [libsignal TokioAsyncContext](https://github.com/signalapp/libsignal/blob/main/swift/Sources/LibSignalClient/TokioAsyncContext.swift)
- [SMAppService](https://developer.apple.com/documentation/servicemanagement/smappservice)
- [Mailspring-Sync](https://github.com/Foundry376/Mailspring-Sync)
