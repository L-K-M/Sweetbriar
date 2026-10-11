# Case Studies: Sharing a Rust Core Between Native UIs

Research notes for a report on shared-core architectures. Compiled 2026-10-11.

## 1. Red Badger — Crux

**Architecture.** Crux (github.com/redbadger/crux) splits an app into a Rust **Core** (business logic, Elm-architecture: `Event`/`Model`/`Effect`/`ViewModel`, event-sourced `update` function) and a thin native **Shell** (SwiftUI, Jetpack Compose, React, or Wasm). The core is side-effect-free; it *requests* effects (HTTP, KV, time, location, secrets) from the shell via managed side-effects, and the shell executes and resolves them. The entire FFI surface is three methods on a `Bridge`: `update`, `resolve`, `view` — all data crosses as `bincode`-serialized byte arrays. The core compiles per-platform: static library + Swift bindings on Apple, dynamic library + Kotlin on Android, Wasm in the browser.

**Type generation.** Two layers: (a) **BoltFFI** generates the byte-oriented FFI bindings (migrated from UniFFI in crux_core 0.19.0, June 2026); (b) **facet-based typegen** generates the app types. You derive `Facet` on types crossing the boundary and annotate the `Effect` enum with `#[effect(facet_typegen)]`; a `codegen` binary in the shared crate (`TypeRegistry::new().register_app::<App>()` then `.swift()/.kotlin()/.typescript()/.csharp()`) emits foreign types plus a matching `bincode` serialization runtime. Breaking type changes in the core cause compile errors in shells. C# typegen (via `facet_generate`, net10.0 + CommunityToolkit.Mvvm) landed in 0.18.0 (May 2026).

**Examples.** `counter` (basics, tests), `counter-http` (HTTP capability), `weather` (real app: location, HTTP, secret, KV capabilities; iOS/Android/Leptos/Next.js shells), `notes` (collaborative editor: Automerge CRDT document, custom `PubSub` capability over `BroadcastChannel` for cross-tab sync, debounced save to `crux_kv`; currently web-only), `counter-middleware`, `counter-routing` (effect routing lanes: Serialized/Parked/Buffer). A `cat_facts` example existed historically (KV + time + HTTP capabilities, shell persists facts to a file) but is **no longer on master** — removed after the 0.17 era (unverified: exact removal date).

**Maturity 2025–2026.** Very active: 0.17.0 (Mar 2026, removed legacy Capability API, unified Bridge), 0.18.0 (May 2026, C# typegen, fluent effect test assertions), 0.19.0 (Jun 2026, BoltFFI replaces UniFFI, Effect Routing), 0.20.0 (Aug 2026, non-reuse of EffectIds, fire-and-forget leak fix, `crux_http` error-body/headers API). ~2,445 commits; small team (Stuart Harris, Martin Geisler, Matthew Hedgpeth et al.); book at redbadger.github.io/crux. Still pre-1.0; roadmap mentions Dart/C++ typegen and Flutter shells.

**vs UniFFI.** UniFFI generates rich typed bindings (objects, callbacks, async) from a `.udl`/proc-macro interface — better for exposing a large library API. Crux deliberately keeps FFI to three byte-oriented methods and generates a *separate* typed message layer; the tradeoff is a much smaller, stable binding surface and Elm-style enforced discipline, at the cost of manual shell plumbing and bincode serialization overhead.

## 2. AppFlowy — Rust core + Flutter UI

AppFlowy (Notion alternative) uses Flutter/Dart for UI and a Rust workspace (`frontend/rust-lib/`) for all business logic, persistence (SQLite), CRDT collaboration (Yrs), search, and storage. Connection is **Dart FFI** (`dart-ffi` crate): Dart calls `async_event(port, bytes)` / `sync_event`, and Rust posts responses back via `allo_isolate` ports. The protocol is an **Event-Dispatch** pattern: events and protobuf data structs are defined in Rust (`event_map.rs`, `#[event(input=.., output=..)]` on enums, `ProtoBuf` derive on structs); build scripts generate matching Dart/TS event classes and protobuf modules. A dispatcher routes each event to the registering module's handler. BLoC holds Dart-side state; the Rust core is the source of truth. AppFlowy explicitly chose event-dispatch over codegen'd function bridges (flutter_rust_bridge) to stay UI-agnostic — the same Rust backend also serves a Tauri (TS) frontend. Cost: protobuf serialize/deserialize overhead, painful for large payloads (images).

## 3. Spacedrive — sd-core + rspc

Spacedrive (cross-platform file manager) has a pure-Rust core (`sdcore`: filesystem, SQLite/SeaORM DB, Iroh P2P, jobs) with CQRS/DDD "operations" (actions/queries). UIs: React 19 (Tauri 2 desktop, React Native/Expo mobile, web), plus CLI/daemon/server. **rspc** (oscartbeaumont/rspc) provides end-to-end typesafe RPC: define a Rust function in a router, call it from TypeScript with auto-generated types (via Specta). rspc is **transport-agnostic, not FFI** — served over HTTP (Axum) or Tauri IPC. Spacedrive runs a **daemon**: desktop/CLI/web connect over Unix domain sockets (WebSockets for web) using JSON-RPC 2.0 with versioned method strings (`action:files.copy.input`); the iOS app embeds the core via FFI (`handle_core_msg`) with Specta-generated Swift types. Events flow through a broadcast `EventBus` (generic `ResourceChanged` events) into a normalized TanStack Query cache. **Pros for local-first:** one core binary serves every client; type safety across the boundary; daemon enables multi-client and headless deployment. **Cons:** network/IPC latency vs in-process FFI; JSON serialization; operational complexity of a daemon process.

## 4. Anytype — anytype-heart (Go, instructive non-Rust)

Anytype's `anytype-heart` is a Go middleware library (319 packages: block editor, object-graph DB, CRDT sync via custom Any-Sync protocol, E2E encryption, per-space SQLite) shared by all clients. Desktop (anytype-ts, React/MobX) talks **gRPC + protobuf** with a persistent event stream; mobile embeds the core as a **C library via gomobile** (not gRPC) with generated Swift/Java protobuf bindings. Same lesson as Spacedrive: shared core + thin native shells, protobuf as the boundary, transport differs per host (gRPC on desktop, FFI on mobile).

## 5. Email clients and other instructive apps

- **himalaya** (pimalaya): Rust email *library* stack — `io-email` (sans-IO, `no_std` coroutines per protocol: IMAP, JMAP, SMTP, Gmail, Graph, Maildir) with a shared least-common-denominator API. Front-ends: CLI, official TUI (`himalaya-tui`, ratatui), Vim/Emacs/Raycast plugins. Closest thing to a shared Rust mail core, but all front-ends are terminal/CLI — no GUI.
- **meli**: Rust TUI mail client (IMAP/JMAP/NNTP/Maildir/notmuch). Single binary, no shared-core split.
- **Stalwart**: Rust mail *server* (JMAP/IMAP/SMTP/CalDAV/etc.), not a client; its JMAP crates are server-side. **Thundermail** (tb.pro) is Thunderbird's hosting service built on Stalwart — not a GUI client on a Rust core.
- **No GUI email client built on a shared Rust core found** (unverified: none known as of 2026-10). The himalaya library stack is the obvious foundation for one.
- **Zed**: Rust end-to-end including its UI (GPUI) — the opposite pattern (no native shells), not instructive for this report.
- **Ghostty** (Zig, not Rust): instructive **core-in-library + native embedder** pattern. `libghostty` is a cross-platform C-ABI library (terminal emulation, fonts, render state); the macOS GUI is Swift/AppKit/SwiftUI and the Linux GUI is Zig/GTK4, both linking the library. Embedder implements callbacks (wakeup, clipboard, actions) and provides a native view; `libghostty-vt` is a zero-dependency subset for embedders. Caveat: embedding API is unstable, used only by the macOS app so far.

## 6. UniFFI users

The UniFFI README/docs do **not** maintain a formal users list — it states only that UniFFI is "used extensively by Mozilla in Firefox mobile and desktop browsers" (application-services: sync/storage; Nimbus; Glean) with "a growing community of users." Known production users from ecosystem evidence: Mozilla (Firefox Android/iOS/desktop), juspay/hyperswitch-prism (multi-language SDKs), synonymdev/vss-rust-client-ffi (Lightning VSS mobile), and `uniffi-bindgen-react-native` consumers (matrix SDK, ChessTiles, Whip). UniFFI 0.32.x is current; still pre-1.0.

## Sources

- https://github.com/redbadger/crux (README, releases, examples)
- https://redbadger.github.io/crux/ and https://redbadger.github.io/crux/part-4/typegen.html (book)
- https://github.com/redbadger/crux/releases (0.17.0–0.20.0 changelogs)
- https://github.com/redbadger/crux/tree/master/examples/notes/README.md
- https://appflowy.com/blog/tech-design-flutter-rust
- https://docs.appflowy.io/docs/documentation/software-contributions/architecture/backend/event
- https://appflowy-io-appflowy.mintlify.app/developer/rust-backend and /developer/architecture
- https://github.com/spacedriveapp/spacedrive (README, AGENTS.md)
- https://v2.spacedrive.com/core/ops, /core/events, /react/ui/hooks, /overview/whitepaper
- https://rspc.dev/ (overview, production users)
- https://github.com/anyproto/anytype-heart (README, AGENTS.md, docs/Build.md)
- https://deepwiki.com/anyproto/anytype-ts/2-core-systems
- https://github.com/pimalaya/himalaya and https://github.com/pimalaya/himalaya-tui and https://github.com/pimalaya/io-email
- https://github.com/meli/meli
- https://github.com/stalwartlabs/stalwart
- https://www.tb.pro/ and https://support.tb.pro/hc/en-us/articles/43937477830547 (Thundermail)
- https://github.com/mozilla/uniffi-rs (README)
- https://github.com/jhugman/uniffi-bindgen-react-native (users list)
- https://github.com/juspay/hyperswitch-prism (UniFFI blog)
- https://github.com/synonymdev/vss-rust-client-ffi
- https://ghostty.org/docs/about and https://mintlify.wiki/ghostty-org/ghostty/api/embedding
- https://github.com/ghostty-org/ghostty and https://github.com/ghostty-org/ghostling
