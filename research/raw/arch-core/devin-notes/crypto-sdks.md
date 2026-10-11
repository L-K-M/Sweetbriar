# Sharing a Rust Core Between Native UIs: Four Case Studies

## 1. Bitwarden — Internal SDK (sdk-internal)

Bitwarden's shared core lives in the private `bitwarden/sdk-internal` repo, consumed by the `bitwarden/clients` monorepo (web/desktop), `bitwarden/android`, and `bitwarden/ios`. The guiding principle, per their contributing docs: "everything except presentational logic belongs in the SDK." The SDK owns business logic, state management, API communication, cryptography, and data models; clients own only UI rendering and platform integrations.

**What lives in Rust:** `bitwarden-core` (the `Client` container for runtime persistent data, with feature crates extending it via extension traits), `bitwarden-crypto` (encryption, key derivation), `bitwarden-vault` (cipher data model), plus auto-generated API/identity bindings. Client TypeScript still holds UI and adapters; e.g., cipher decryption was migrated to the SDK in PR #14206 (merged May 2025) behind a feature flag, with temporary adapter code translating between TS domain models and SDK models. Argon2 KDF was consolidated into the SDK in PR #15401 (July 2025), dropping three separate client implementations.

**Bindings per platform:** Mobile uses **UniFFI** (`bitwarden-uniffi` crate generates Kotlin and Swift bindings). Web uses **wasm-bindgen** (`bitwarden-wasm-internal`), published to npm as `@bitwarden/sdk-internal` and `@bitwarden/commercial-sdk-internal`. Desktop additionally uses **napi-rs** native modules for OS-specific functionality (biometrics, keychain).

**State of the public offering (2025–2026):** The internal SDK is private. The public-facing **Secrets Manager SDK** (`bitwarden/sdk` repo, open source) provides Rust API, CLI, Node-API, and language bindings via a JSON-schema/QuickType pipeline — a different architecture from the internal SDK. As of 2026, the internal SDK is mid-migration to async: PR #906 (April 2026) made `InternalClient` get/set methods async across both UniFFI and WASM boundaries.

**Pain points:** Breaking-change coordination is a first-class problem — an automated "Breaking Change Detection" workflow runs client builds against SDK branches, and merges to `sdk-internal` `main` trigger rolling auto-PRs to each client. The "breaking change window" blocks other teams' SDK updates until the consuming client PR ships. Temporary TS↔SDK adapter layers add conversion overhead during incremental migration.

**Sources:** github.com/bitwarden/sdk-internal, contributing.bitwarden.com/architecture/sdk, github.com/bitwarden/clients/pull/14206, github.com/bitwarden/clients/pull/15401, github.com/bitwarden/sdk-internal/pull/906

---

## 2. 1Password — Rust Core (1Password 8)

1Password 8 (announced 2021) is the most public case of a Rust core shared across all major desktop and mobile platforms plus browser extension. The Windows team adopted Rust in 2017; the core project consolidated the "1Password.next" prototype into a headless client application with its own runtime loop.

**What lives in Rust:** Business logic, view models, data model, secrets management, sync/conflict resolution, cryptography, database access, and server communication. The core stops "just short of the user interface." Native UIs (SwiftUI on iOS, web/Electron on macOS/Windows/Linux) consume the core via FFI. State lives in the core; the sidebar vault-grouping change, for example, required zero frontend changes because grouping logic lived in the core.

**FFI tooling:** 1Password built **Typeshare** (open-sourced November 2022) — a procedural-macro tool that generates consistent type definitions across Swift, Kotlin, and TypeScript from annotated Rust structs, handling serialization/deserialization automatically. Node.js/Electron uses **Neon** bindings. The browser extension compiles the core to **WASM**. Native platforms use hand-written FFI with Tokio as the async runtime; the core's runtime loop watches a channel for incoming FFI requests.

**What went well:** Cross-platform consistency (search behaves identically everywhere), memory safety for secrets (zeroize crate), single bug-fix location. The strict compiler catches cross-platform regressions early.

**Regrets/pain points:** WASM "did not take us as far as we hoped" — standing up an entire runtime (networking, sync) in WASM proved very difficult. They initially built two macOS apps (SwiftUI + Electron) and later killed the SwiftUI Mac app, a "painful decision." A TOTP library proliferation (XKCD 927) preceded consolidation. Open-sourcing components requires versioning/CI/maintenance overhead that limits what they publish.

**Sources:** 1password.com/blog/1password-8-the-story-so-far (2021), 1password.com/blog/typeshare-for-rust (2022), rustfoundation.org/media/secure-app-development-with-rusts-memory-model (2022), serokell.io/blog/rust-in-production-1password (2021), syntax.fm/show/776 (2024), corrode.dev/podcast/s04e06-1password (2025)

---

## 3. Signal — libsignal

Signal's `libsignal` repo (signalapp/libsignal) implements the Signal Protocol in Rust, exposed to Java, Swift, and TypeScript.

**What lives in Rust:** `libsignal-protocol` (Double Ratchet), plus other cryptographic primitives. Java/Swift/TypeScript are thin wrapper libraries over the Rust core.

**Binding approach — hand-written, NOT UniFFI:** Signal uses a custom proc-macro bridge system (`libsignal-bridge-macros`) with `#[bridge_fn]` and `#[bridge_io]` attributes. A single Rust function annotation generates three language-specific entry points: C FFI (for Swift, via `cbindgen`), JNI (for Java/Android, names munged into `Java_org_signal_client_internal_Native_*` statics), and Node.js (for TypeScript, via `linkme` registration with the Neon runtime). This was deliberately built in-house rather than adopting UniFFI, to support Signal's specific requirements across three language targets from one codebase. Async support is asymmetric: the **Node bridge supports async** (via `signal-neon-futures`, returning JS Promises on the JS event loop), while **FFI and JNI do not support async execution** — functions are expected to complete immediately without blocking.

**Shipping:** Java/Android ships as Maven artifacts (`org.signal:libsignal-client`, `libsignal-android` AAR with native `.so` for armeabi-v7a/arm64-v8a/x86/x86_64) from build-artifacts.signal.org. iOS ships as a **CocoaPod** (`LibSignalClient`) — the canonical distribution; a Swift Package exists for local development only, not as a binary-target distribution (per swift/README.md and issue #609, static `.a` linking is unsupported because the library is used from multiple binaries in the app). Desktop ships as the NPM package `@signalapp/libsignal-client` with native libraries for Windows/macOS/Linux.

**Lessons:** The bridge generates *safe interfaces* to Rust APIs, explicitly not "perfect C, Java, or TypeScript APIs" — idiomatic wrappers are built on top. Type conversion traits (`ArgTypeInfo`, `ResultTypeInfo`) must be implemented per language, and buffer handling for FFI requires special multi-parameter treatment.

**Sources:** github.com/signalapp/libsignal (README, rust/bridge/README.md, swift/README.md), jrose-signal.github.io/rendered-docs/libsignal_bridge_macros, github.com/signalapp/libsignal/issues/609

---

## 4. Wire — core-crypto

Wire's `core-crypto` repo (wireapp/core-crypto) provides MLS (via a Wire-maintained OpenMLS fork) and legacy Proteus (Signal double-ratchet) encryption in Rust, shared across iOS, Android, and web.

**What lives in Rust:** The `CoreCrypto` top-level handle, `Session` (MLS state), `ProteusCentral` (legacy), `MlsProvider` (bridging OpenMLS to the keystore via the `OpenMlsCryptoProvider` trait), and the encrypted keystore. Native apps own the UI and delivery-service transport; CoreCrypto owns all keying material and protocol state.

**Binding tech:** **UniFFI** for Kotlin/Swift (generated from `crypto-ffi` crate UDL annotations; Kotlin bindings additionally had a hand-written wrapper class that was eliminated in favor of generated code). **wasm-bindgen** for browser/Electron (TypeScript via `#[wasm_bindgen]` annotations). Node.js native bindings use **uniffi-bindgen-react-native (ubrn)** generating N-API from the same UniFFI-annotated source. Published as: Maven Central `com.wire:core-crypto-android`, XCFramework `WireCoreCrypto` (Swift async/await API), npm `@wireapp/core-crypto` (browser + native entry points).

**Async/state crossing:** All mutations go through a **transaction model** — `newTransaction`/`transaction` callback buffers changes in memory and flushes atomically to the keystore on commit; on throw, all changes are discarded. This gives UniFFI bindings (which cannot enforce Rust ownership across FFI) deterministic transaction semantics. Native targets use `async_trait` with `Send` bounds; WASM uses `async_trait(?Send)` because browser APIs are single-threaded.

**Pain points:** Eliminating the hand-written Kotlin wrapper class in favor of UniFFI-generated code required extensive breaking changes (method renames, parameter reordering, removed methods). Migrating byte-vector newtypes across platforms caused per-platform API drift (Android had newtypes earlier than other clients). The WASM keystore encryption (AES-256-GCM over IndexedDB) has not been independently audited.

**Sources:** github.com/wireapp/core-crypto (README, ARCHITECTURE.md, CHANGELOG.md), wireapp.github.io/core-crypto/main/rust/core_crypto, mintlify.wiki/wireapp/core-crypto/concepts/architecture

---

## Cross-cutting Observations

| | Bitwarden | 1Password | Signal | Wire |
|---|---|---|---|---|
| **Mobile bindings** | UniFFI | Hand-written FFI | FFI (Swift) + JNI (Java) | UniFFI |
| **Web bindings** | wasm-bindgen | WASM + Neon | Node (Neon) | wasm-bindgen + UBRN |
| **Type sync** | Manual adapters | Typeshare | ArgTypeInfo traits | UniFFI generated |
| **Async across FFI** | In progress (2026) | Tokio runtime | Node only | UniFFI async traits |
| **State model** | Client + Repositories | Core-owned, view models | Transaction-like APIs | Transaction context |

Common patterns: (1) Rust owns crypto, data model, and business logic; native owns UI only. (2) UniFFI dominates when generating Kotlin+Swift from one definition; hand-written bridges persist when async/type-system needs diverge per platform. (3) Breaking-change detection and multi-repo coordination are the hardest operational problems. (4) WASM is viable for crypto libraries but standing up a full runtime (networking, sync) remains difficult.
