<!-- provenance: helper=codex model=gpt-6.1-sol started=2026-10-11T18:14:20+00:00 exit=0 brief=research/briefs/ -->

Saved to [report.md](codex-artifacts/report.md).

# Sweetbriar: a Rust core with native macOS and Linux UIs

**Research date: 2026-10-11.** Current status statements were checked on that date; historical accounts carry publication dates. “Recommendation” identifies proposed Sweetbriar design. Source inspection is evidence of an implementation, not an independent performance audit. Missing public evidence is identified explicitly.

## 1. TL;DR

- **Build a platform-independent Rust email engine and a task-oriented application API.** Share sync, storage, MIME, search, drafts, send queues, rules and undo semantics. Keep widgets, editing, accessibility, windows and platform integration native. Mozilla, Element X and Proton provide working precedents. [Mozilla architecture](https://firefox-source-docs.mozilla.org/rust-components/developing-rust-components/index.html), [Element X](https://github.com/element-hq/element-x-ios), [Proton engineering](https://proton.me/blog/next-generation-proton-mail-mobile-apps).
- **Start with UniFFI for Swift, plus a handwritten concurrency adapter.** The checked changelog lists 0.32.1, released 2026-09-08. Swift `async` generation does not establish cancellation, runtime ownership or main-thread delivery. [Changelog](https://github.com/mozilla/uniffi-rs/blob/main/CHANGELOG.md), [async documentation](https://mozilla.github.io/uniffi-rs/latest/futures.html).
- **Use windowed observable queries.** Stable IDs, bounded pages, versioned changes and explicit resets should be the contract. Matrix timelines and Proton live queries provide precedents. [Matrix listener](https://github.com/matrix-org/matrix-rust-sdk/blob/main/bindings/matrix-sdk-ffi/src/timeline/mod.rs), [Proton facade](https://github.com/ProtonMail/clients/blob/main/project/mail/rust/mail/mail-uniffi/src/lib.rs).
- **Let Rust own scheduling and durable operations.** Swift cancellation should stop disposable queries; it must not silently retract an accepted draft save or send operation. Signal demonstrates explicit, race-aware cancellation. [Signal implementation](https://github.com/signalapp/libsignal/blob/main/swift/Sources/LibSignalClient/TokioAsyncContext.swift).
- **Keep one authoritative store writer.** SQLite WAL allows concurrent readers but only one writer; long readers obstruct checkpoints. Delta Chat’s account-loss incident illustrates why error semantics belong in the data-safety design. [SQLite WAL](https://www.sqlite.org/wal.html), [Delta Chat retrospective](https://delta.chat/en/2025-02-11-why-jsonrpc-bindings-exist).
- **Treat sync after quitting as a deployment requirement.** An embedded XPC service and a registered per-user background agent have different lifecycles. Prototype one process; use `SMAppService` and IPC if continuous background operation is required. [Apple service management](https://developer.apple.com/documentation/servicemanagement/smappservice), [XPC lifecycle](https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingXPCServices.html).
- **Benchmark realistic interfaces before changing generators.** BoltFFI exists and publishes promising measurements, with substantially different ratios by workload. Current Crux documentation uses BoltFFI plus separate type generation. [BoltFFI benchmarks](https://github.com/boltffi/boltffi/blob/main/benchmarks/README.md), [Crux typegen](https://redbadger.github.io/crux/part-4/typegen.html).
- **Keep Linux reuse practical.** Rust GTK4 can call the same application service directly. Package the macOS library as an XCFramework-backed Swift package, and support Intel if the chosen OS/hardware policy requires it. [GTK guidance](https://gtk-rs.org/gtk4-rs/stable/latest/book/main_event_loop.html), [Apple binary packages](https://developer.apple.com/documentation/xcode/distributing-binary-frameworks-as-swift-packages), [Tahoe compatibility](https://support.apple.com/en-us/122867).

## 2. Findings

### 2.1 Case studies: what is actually shared

“Rust core” ranges from cryptographic components to an entire application model. These cases should not be treated as interchangeable.

#### Element X and matrix-rust-sdk

**Verified implementation, checked 2026-10-11.** Element X iOS supplies a native SwiftUI application over `matrix-rust-sdk`. Rust supplies networking, synchronization, encryption and persistence; `matrix-sdk-ui` adds timeline and room-list behavior. `matrix-sdk-ffi` exposes a designed UniFFI facade rather than native widgets. [iOS repository](https://github.com/element-hq/element-x-ios), [SDK](https://github.com/matrix-org/matrix-rust-sdk).

The timeline bridge subscribes to an initial collection plus `VectorDiff` updates, delivers an initial reset, then forwards batches through a listener. `TaskHandle.cancel()` aborts the task; dropping the handle cancels it. The current native runtime helper obtains a Tokio handle through `async_compat`. Callback delivery does not imply Swift main-actor execution. [Timeline code](https://github.com/matrix-org/matrix-rust-sdk/blob/main/bindings/matrix-sdk-ffi/src/timeline/mod.rs), [task handle](https://github.com/matrix-org/matrix-rust-sdk/blob/main/bindings/matrix-sdk-ffi/src/task_handle.rs), [runtime](https://github.com/matrix-org/matrix-rust-sdk/blob/main/bindings/matrix-sdk-ffi/src/runtime.rs).

**Published outcome and difficulty:** Element’s 2025-11-12 retrospective credits the reusable SDK foundation and adoption by other clients. Feature parity took longer than expected: the SDK was incomplete, while evolving protocols compounded the rewrite. SDK work became more complex than application work. **Implication:** share query models, but avoid launching several front ends while simultaneously inventing the engine. [Retrospective](https://element.io/blog/element-x-and-pro-updates-a-glimpse-into-the-future/).

#### Mozilla application-services

**Verified implementation, checked 2026-10-11.** Mozilla shares Rust components for accounts, synchronization, history/bookmark-related services and other browser functionality. Firefox iOS and Android retain native application/UI code; UniFFI is the principal Swift/Kotlin binding mechanism. Mozilla cites avoiding duplicate implementations and preserving interoperability as motivations. [Repository](https://github.com/mozilla/application-services/blob/main/README.md), [architecture](https://firefox-source-docs.mozilla.org/rust-components/developing-rust-components/index.html).

Not every component becomes a Tokio actor. Mozilla distinguishes blocking APIs, native worker queues and async functions. `SuggestStore` offers an interrupt operation for an outstanding query. Apple distribution consolidates Rust components into a “megazord” and XCFramework with generated Swift alongside. [Threading](https://firefox-source-docs.mozilla.org/rust-components/developing-rust-components/threading.html), [SuggestStore](https://mozilla.github.io/application-services/swift/Classes/SuggestStore.html), [iOS packaging](https://mozilla.github.io/application-services/book/howtos/locally-published-components-in-firefox-ios.html).

**Assessment:** a strong precedent for a stable domain API and consolidated packaging. Its documented threading/build machinery also shows the integration work that remains. A specific public “we regret UniFFI” account was not verified.

#### Bitwarden SDK

**Verified implementation, checked 2026-10-11.** Bitwarden’s native mobile applications use Swift and Kotlin. Rust SDK functionality crosses through UniFFI; web consumers use `wasm-bindgen`. Its architecture separates bindings, application interfaces, feature domains and core utilities, covering vault, cryptographic and password-generation functionality. This is a product-facing SDK rather than a shared widget tree. [Native-app announcement](https://bitwarden.com/blog/native-mobile-apps/), [bindings](https://contributing.bitwarden.com/architecture/sdk/language-bindings/), [crate architecture](https://contributing.bitwarden.com/architecture/sdk/crate-structure/).

The current exported `Client` uses UniFFI's Tokio async-runtime annotation, supplies async calls and accepts host-managed state. A native management-profile update is pushed into a shared Rust handle; that is distinct from exporting an entire observable UI. A universal UI-update/cancellation policy remains **(unverified)**. [Client source](https://github.com/bitwarden/sdk-internal/blob/main/crates/bitwarden-uniffi/src/lib.rs), [profile bridge](https://github.com/bitwarden/sdk-internal/blob/main/crates/bitwarden-uniffi/src/managed_settings.rs).

**Assessment:** the application-interface layer protects native code from internals. Bitwarden presents native responsiveness/shared logic as benefits; its internal SDK warns that its password-manager API is unstable and unsupported for third-party use, and breaking SDK changes require coordinated client updates. This is documented integration cost, not a verified regret retrospective. [Announcement](https://bitwarden.com/blog/native-mobile-apps/), [SDK warning and release process](https://github.com/bitwarden/sdk-internal).

#### 1Password

**Historical evidence, published 2021-05-18 and 2021-08-12; consulted 2026-10-11.** 1Password describes Rust covering encryption, database, sync and business logic, including view models. Native mobile UIs sit above it. The described desktop architecture uses Electron/React/TypeScript, Neon for Rust/Node integration and Typeshare for shared types. Typeshare generates definitions; it is not a complete Swift function-binding or async system. [Linux engineering account](https://dteare.medium.com/behind-the-scenes-of-1password-for-linux-d59b19143a23), [1Password 8 account](https://1password.com/blog/1password-8-the-story-so-far), [Typeshare](https://1password.com/blog/typeshare-for-rust).

**Published difficulty:** 1Password abandoned a parallel SwiftUI Mac implementation, citing divergent OS-support needs and additional Mac-specific work. Rust did not make a native Mac front end inexpensive. Exact proprietary Swift FFI, live-update and cancellation mechanisms in current 2026 releases are **(unverified)**; historical articles cannot establish them. [Mac discussion](https://1password.com/blog/1password-8-the-story-so-far).

**Implication:** budget explicitly for native UI ownership. Shared engine code does not remove the cost of producing each interface.

A current check supports continued Rust use: 1Password's **2026-09-21** engineering article describes its client TLS stack using `rustls` and `rustls-platform-verifier`, and changing the cryptographic provider through Cargo. It does not disclose the current Swift binding implementation. [2026 client-stack account](https://1password.com/blog/post-quantum-tls-without-code-changes).

#### Signal / libsignal

**Verified implementation, checked 2026-10-11.** `libsignal` supplies Rust security/networking components rather than the complete conversation store and UI. Custom bridge macros support generated C functions/Swift wrappers, Java/JNI and Node/TypeScript paths. Documentation describes argument conversion, async calls and panic handling. [Library](https://github.com/signalapp/libsignal), [bridge](https://github.com/signalapp/libsignal/blob/main/rust/bridge/README.md), [Swift wrapper](https://github.com/signalapp/libsignal/blob/main/swift/README.md).

`TokioAsyncContext` owns a native runtime handle. Swift installs `withTaskCancellationHandler` and uses locked state to handle cancellation before Rust supplies its cancellation ID. `AsyncUtils.swift` includes a bulk-polled stream abstraction. These make cancellation and stream ownership explicit. [Cancellation](https://github.com/signalapp/libsignal/blob/main/swift/Sources/LibSignalClient/TokioAsyncContext.swift), [async/stream code](https://github.com/signalapp/libsignal/blob/main/swift/Sources/LibSignalClient/AsyncUtils.swift).

**Assessment:** custom bindings buy precise control. Their maintenance cost is an architectural inference, not a documented Signal regret; a comparative public retrospective was not verified.

#### Delta Chat / chatmail core

**Verified implementation, checked 2026-10-11.** Rust supplies email transport, MIME, encryption, contacts, messages and storage. Mobile clients use C bindings, Swift on iOS and Java/JNI on Android; desktop uses a separate Rust JSON-RPC server over stdio. The repository exposes both interfaces. [Core](https://github.com/chatmail/core).

**Published lessons, 2025-02-11:** desktop JSON-RPC removed wrapping layers and blocking `block_on` calls that stalled the UI. C pointers were unsuitable for crossing processes. More consequentially, an ambiguous `dc_is_configured()` result made an encrypted account appear unconfigured after an error; UI cleanup could then remove it. The replacement distinguished whether the account was open. Async RPC and explicit errors were benefits; manual references and ambiguous legacy contracts were costs. [Retrospective](https://delta.chat/en/2025-02-11-why-jsonrpc-bindings-exist).

**Recommendation:** distinguish `NotFound`, `Locked`, `Unavailable`, `Corrupt` and `NeedsMigration`. A failed open must never authorize account creation or deletion.

#### Mullvad VPN

**Verified implementation, checked 2026-10-11.** A Rust daemon owns sensitive operation while an Electron/React desktop GUI and CLI use gRPC over a Unix-domain socket. Event subscriptions report state/settings; Android uses JNI. The repository describes iOS as a standalone implementation. An iOS-specific Rust crate exists, but does not establish reuse of the full desktop daemon. [Repository](https://github.com/mullvad/mullvadvpn-app), [architecture](https://github.com/mullvad/mullvadvpn-app/blob/main/docs/architecture.md), [iOS entry point](https://github.com/mullvad/mullvadvpn-app/blob/main/mullvad-ios/src/lib.rs).

The iOS subset combines cbindgen C headers with a UniFFI tunnel adapter. The adapter calls Swift from Rust-owned threads and provides deterministic `stop()` as well as teardown on drop. Its Swift configuration folds the FFI header into the existing module to avoid a second module linking the same static library. [C header build](https://github.com/mullvad/mullvadvpn-app/blob/main/mullvad-ios/build.rs), [UniFFI adapter](https://github.com/mullvad/mullvadvpn-app/blob/main/mullvad-ios/src/tunnel_adapter/ffi/mod.rs), [Swift configuration](https://raw.githubusercontent.com/mullvad/mullvadvpn-app/main/mullvad-ios/uniffi.toml).

The architecture discusses actor coordination and potential deadlocks. **Assessment:** a daemon can preserve work independently of UI failure, but requires command ordering, subscriptions and reconnect behavior. Desktop Mullvad is a process-model precedent, not evidence for a native Swift desktop UI. No separate migration-regret account was verified. [Architecture](https://github.com/mullvad/mullvadvpn-app/blob/main/docs/architecture.md).

#### Proton Mail, Pass and Drive

**Mail:** Proton’s 2026-02-02 account describes its 2025 mobile rewrite: SwiftUI and Kotlin/Compose over Rust, about 80% shared code, with much business logic including some navigation and infinite scrolling. It reports more consistent delivery, but also shared regressions and organizational platform silos requiring feature-oriented ownership. These are company-reported results. [Engineering account](https://proton.me/blog/next-generation-proton-mail-mobile-apps).

The public Mail facade uses UniFFI. An application-lifetime `MailSession` retains database/background resources; live queries return initial results and a `WatchHandle.disconnect()`. The old `rust-mail` repository was archived on 2026-06-12 and relocated to the clients monorepo, not abandoned. [Current facade](https://github.com/ProtonMail/clients/blob/main/project/mail/rust/mail/mail-uniffi/src/lib.rs), [relocation](https://github.com/ProtonMail/rust-mail). As checked 2026-10-11, the separate iOS repository still warns that internal SDK distribution prevents building that checkout entirely from public sources. [iOS README](https://github.com/ProtonMail/ios-mail).

**Pass:** `proton-pass-common` includes shared Rust components, mobile UniFFI, a `PassRustCore` Swift package/XCFramework and web/Wasm integration. This verifies shared components, not an entire universal application engine. Proton also describes Rust reuse between Pass and Authenticator. [Pass repository](https://github.com/protonpass/proton-pass-common), [Authenticator engineering](https://proton.me/blog/authenticator-rust).

**Drive:** the official SDK currently documents TypeScript and C# implementations, with Swift/Kotlin bindings around C# in incubation. A production shared Rust Drive engine is **(unverified)**. Mail’s architecture should not be generalized across Proton products. [Drive SDK](https://github.com/ProtonDriveApps/sdk/blob/main/client/README.md).

Proton’s Rust Nation 2026 repository demonstrates reusable Crux features in example applications; it does not prove that shipping Mail adopted the current Crux architecture wholesale. [Demo](https://github.com/ProtonMail/proton-rust-nation-2026/blob/main/README.md).

#### Wire core-crypto

**Verified implementation, checked 2026-10-11.** Rust covers MLS/Proteus cryptographic state and persistent storage; surrounding applications and UI remain outside that scope. Native Swift/Kotlin use UniFFI; the architecture also describes TypeScript bindings through UBRN. [Architecture](https://github.com/wireapp/core-crypto/blob/main/cc-book/src/development/ARCHITECTURE.md), [bindings](https://wireapp.github.io/core-crypto/).

Swift supports `try await cc.transaction { ctx in ... }`: success commits, throwing rolls back, and the context is invalid outside the callback’s lifetime. Transactions serialize; documentation discusses long batches blocking other operations and Swift/Kotlin cancellation. **Assessment:** a strong transactional foreign-callback contract. Sweetbriar should instead keep transactions short and engine-owned, rather than allowing arbitrary UI code to hold one while awaiting network or user input. The blocking concern is documented behavior, not an invented regret. [Transactions](https://github.com/wireapp/core-crypto/blob/main/cc-book/src/concepts/client/transactions.md).

#### Red Badger Crux

**Framework, not a shipping email product; checked 2026-10-11.** Crux shares an Elm-style Rust application: `Event`, `Model`, `Effect`, `ViewModel`, and update/resolve/view entry points. The shell renders and performs effects, then returns results. Maintainers describe it as production-ready while pre-1.0 and subject to breaking changes. [Repository](https://github.com/redbadger/crux).

Current native integration uses **BoltFFI**, with separate Facet-based generation of Swift/Kotlin/etc. values and bincode support. Older descriptions of Crux packaging are insufficient. Streaming effects are supported; the shell drives platform async work. Documentation discusses future incremental-view improvements rather than guaranteeing cheap diffs for all view updates. [Typegen architecture](https://redbadger.github.io/crux/part-4/typegen.html).

**Assessment:** attractive when platforms intentionally share application interaction semantics. Sweetbriar’s strongest concern is a universal view model absorbing platform-specific navigation or large mailbox snapshots. This is a trade-off, not a verified user regret.

#### Thunderbird desktop and iOS

**Desktop:** an April 2024 article describes Rust EWS integration into Gecko/C++/JavaScript via XPCOM and Gecko async machinery. It documents vendor/build complexity, XPCOM boilerplate and tooling friction. This is incremental adoption rather than a complete portable Rust email engine. Thunderbird announced built-in EWS email support in version 145 in November 2025. [Rust integration](https://blog.thunderbird.net/2024/04/adventures-in-rust-bringing-exchange-support-to-thunderbird/), [shipping announcement](https://blog.thunderbird.net/2025/11/thunderbird-adds-native-microsoft-exchange-email-support/).

**iOS:** the 2026-10-01 announcement describes a native app under development, with community testing planned. Its checked `Core` package is **Swift**, using SwiftNIO IMAP/SMTP, URLSession JMAP and Swift async/streams, including IMAP IDLE via `AsyncStream<IdleEvent>`. It does not establish desktop Rust mail-core reuse. [Announcement](https://blog.thunderbird.net/2026/10/thunderbird-for-ios-a-first-look-at-the-native-iphone-email-app-in-development/), [package](https://github.com/thunderbird/thunderbird-ios/blob/main/Core/Package.swift), [core design](https://github.com/thunderbird/thunderbird-ios/blob/main/Core/README.md).

**Backend status:** Microsoft’s documentation, updated 2026-09-04 and checked 2026-10-11, says Exchange Online EWS disablement starts in October 2026 and completes in April 2027. Its Graph roadmap identifies remaining and permanent parity gaps. **Recommendation:** prioritize Graph for Microsoft 365 and evaluate on-premises Exchange separately. The older blanket “October 2026 shutdown” description is insufficient. [Current schedule](https://learn.microsoft.com/en-us/exchange/clients-and-mobile-in-exchange-online/deprecation-of-ews-exchange-online).

### 2.2 FFI tooling, maturity and performance

#### UniFFI: conservative default

UniFFI supports proc-macro exports, generated objects/records/enums/errors and foreign callback interfaces. Rust `async fn` becomes Swift async/await. Protocols permit foreign implementations and mocks. Generated `Sendable` conformance does not make UI callbacks safe: a `@MainActor` object and a nonisolated foreign callback have different obligations. [Swift overview](https://mozilla.github.io/uniffi-rs/latest/swift/overview.html), [async support](https://mozilla.github.io/uniffi-rs/latest/futures.html).

The changelog lists **0.32.1, released 2026-09-08**, as checked 2026-10-11. Release 0.32.0, dated 2026-06-30, added synchronous borrowed byte input via `&[u8]`/`[ByRef] bytes`, excluding async functions. Mutable borrowing is under **unreleased** changes, not a 0.32.1 promise. Swift concurrency fixes remain relevant: a fix for async foreign-trait Swift 6 generation merged on 2026-07-15. Compile the exact generated interface under strict concurrency. [Changelog](https://github.com/mozilla/uniffi-rs/blob/main/CHANGELOG.md), [Swift 6 fix](https://github.com/mozilla/uniffi-rs/pull/2943).

UniFFI does **binary lifting/lowering, not JSON RPC**. Primitives can pass directly; compound values commonly use a `RustBuffer` and reconstruction in Swift. Objects use handles. This internal representation is not a promised persistent-file or IPC format. Bulk strings, records and bodies therefore deserve allocation/copy measurements. [Conversion internals](https://mozilla.github.io/uniffi-rs/latest/internals/lifting_and_lowering.html).

Generated Swift is inspectable, but a handwritten adapter should provide cancellation, `AsyncSequence`, errors and actor delivery. Released Swift async generation does not automatically propagate Swift task cancellation into Rust. [0.32.1 template](https://github.com/mozilla/uniffi-rs/blob/v0.32.1/uniffi_bindgen/src/bindings/swift/templates/Async.swift), [cancellation documentation](https://mozilla.github.io/uniffi-rs/latest/futures.html).

#### Alternatives

| Tool | Verified scope as of 2026-10-11 | Strength | Limitation for Sweetbriar |
|---|---|---|---|
| **UniFFI** | Swift/Kotlin and other bindings; objects, errors, async, callbacks | Strong shipping precedents; broad domain APIs | Compound conversion costs; cancellation/concurrency need integration. [Docs](https://mozilla.github.io/uniffi-rs/latest/swift/overview.html) |
| **swift-bridge** | Two-way Rust/Swift bridge modules; generated C/Swift; async/throwing functions; Rust-owned wrappers | Swift-specific ownership and low-level access | Narrower surface; repository lists `Arc<T>` as unsupported. [Repository](https://github.com/chinedufn/swift-bridge) |
| **cbindgen + manual glue** | C/C++ header generation for a designed Rust ABI | Precise buffers, handles, ABI and scheduling | No generated Swift ownership, cancellation, continuations or streams. [cbindgen](https://github.com/mozilla/cbindgen), [Signal precedent](https://github.com/signalapp/libsignal/blob/main/rust/bridge/README.md) |
| **Diplomat** | Restricted bridge modules, C ABI and several language backends | Deliberate exported API subset | Current backend list excludes Swift; Swift request remains open. [Book](https://rust-diplomat.github.io/diplomat/), [issue](https://github.com/rust-diplomat/diplomat/issues/143) |
| **Crux + typegen** | Events/effects/view models; bincode; native BoltFFI packaging | Small shell boundary; shared interaction logic | Serialized models; framework lifecycle and pre-1.0 evolution. [Crux](https://github.com/redbadger/crux), [typegen](https://redbadger.github.io/crux/part-4/typegen.html) |
| **BoltFFI** | Existing Rust export/generation project, Swift and other targets, async/callbacks and packaging | Performance-oriented, public harnesses | Less independently established application evidence; reproduce benchmarks for selected versions. [Repository](https://github.com/boltffi/boltffi), [benchmarks](https://github.com/boltffi/boltffi/blob/main/benchmarks/README.md) |

For manual C, specify ownership, length, allocator/free pairing, errors and callback lifetime. Prevent Rust panics from unwinding through an ABI that forbids it; panic catching cannot recover from `panic=abort`. Header generation does not solve these contracts. [Rust FFI guidance](https://doc.rust-lang.org/nomicon/ffi.html).

**Generated API quality:** `swift-bridge` exposes ownership wrappers such as `RustString`, `RustStr` and `RustVec`, while UniFFI commonly reconstructs native value types. The former permits more explicit lifetime control but may expose Rust-shaped APIs; the latter is convenient for immutable Swift projections but can copy compound values. `swift-bridge` currently states a minimum supported Swift version of **6.0**, checked 2026-10-11; that does not replace compiling Sweetbriar's actual exports under strict concurrency. [swift-bridge supported surface](https://github.com/chinedufn/swift-bridge), [UniFFI conversions](https://mozilla.github.io/uniffi-rs/latest/internals/lifting_and_lowering.html).

**BoltFFI's async difference:** current documentation describes cooperative caller-to-Rust cancellation, native Swift `AsyncStream`, callbacks and batch-pull subscriptions. It also says its stream ring buffers drop new events when full rather than blocking the producer. That behavior is unsuitable for positional mailbox diffs without additional gap/reset handling. Like UniFFI, it still needs an appropriate Rust runtime when exported futures depend on Tokio I/O. These are documented capabilities, not locally exercised guarantees. [Async documentation](https://boltffi.dev/docs/async), [stream semantics](https://boltffi.dev/docs/streaming).

An adjacent tool, `swift-rs`, exists but primarily supports calling Swift functions **from Rust**, with Swift runtime/package linking. It is not a substitute for a complete generated Rust-domain API consumed by Swift. [Repository](https://github.com/Brendonovich/swift-rs).

**Available numbers:** BoltFFI publishes this Swift/macOS Apple M3 snapshot, identified as v0.24.0 and consulted 2026-10-11. These are **author-published microbenchmarks, not reproduced for this report**. The current harness says competing wrappers share Rust implementations. [Definitions/results](https://github.com/boltffi/boltffi/blob/main/benchmarks/README.md).

| Workload | BoltFFI | UniFFI | Published ratio |
|---|---:|---:|---:|
| Echo a 1 KB string | 10,209 ns | 14,292 ns | 1.4× |
| Generate 1,000 nested user profiles | 701,604 ns | 5,174,792 ns | 7.4× |
| 100 foreign callbacks | 14,834 ns | 203,791 ns | 13.7× |

These support testing record conversion and callback batching, not a whole-client speedup. Sub-nanosecond/no-op results deserve particular scrutiny for optimization and harness effects. No independent current application benchmark comparing every candidate was verified. **Recommendation:** measure mailbox batches, cancellation, body transfer and sustained updates before adding a second FFI system.

### 2.3 Async, cancellation and live updates

**Separate three responsibilities:** Swift's task executor, the FFI future bridge, and the Rust scheduler. UniFFI maps futures into Swift `async`; it does not prescribe the lifetime of Sweetbriar's synchronization engine. Tokio provides explicit runtime builders and handles, while `tokio-util::CancellationToken` provides cooperative cancellation. [UniFFI futures](https://mozilla.github.io/uniffi-rs/latest/futures.html), [Tokio runtime](https://docs.rs/tokio/latest/tokio/runtime/index.html), [CancellationToken](https://docs.rs/tokio-util/latest/tokio_util/sync/struct.CancellationToken.html).

**Recommended ownership:** one `EngineHost` owns a controlled Tokio runtime per engine-hosting process. All network/background tasks are spawned on its handle. Exposed async methods send commands and await responses; avoid accidentally polling socket/timer futures without the intended runtime context. Run blocking SQLite work on a dedicated store worker and bound MIME/indexing CPU work separately. `shutdown()` stops intake, durably records pending work, cancels ephemeral jobs and joins workers off the UI thread. Do not create a runtime per FFI call or invoke `block_on` on the main thread. This is proposed ownership, not an assertion that UniFFI creates it automatically.

**Cancellation must be operation-specific.** The inspected UniFFI 0.32.1 Swift template lacks automatic Swift-to-Rust cancellation; its foreign-callback direction has different machinery. Therefore use a Swift `withTaskCancellationHandler` adapter plus an explicit Rust request/token contract. Signal's handoff state handles cancellation before a cancellation ID arrives; Sweetbriar needs the same race coverage. [Released template](https://github.com/mozilla/uniffi-rs/blob/v0.32.1/uniffi_bindgen/src/bindings/swift/templates/Async.swift), [Signal handoff](https://github.com/signalapp/libsignal/blob/main/swift/Sources/LibSignalClient/TokioAsyncContext.swift).

Proposed distinctions:

- Search, viewport fetch and preview generation are disposable: cancellation stops computation and suppresses stale results.
- `save_draft` and `enqueue_send` receive caller-generated idempotency keys. A durable receipt means acceptance; abandoning the Swift waiter does not cancel the accepted operation. After an interrupted reply, query status using that same key.
- Cancelling a queued send is a separate domain command, with explicit `Queued`, `Submitting`, `Accepted`, `Failed` and `OutcomeUnknown` states. SMTP has timeout/duplicate-delivery ambiguities; a local transaction cannot promise exactly-once remote delivery. [RFC 5321, timeout discussion](https://www.rfc-editor.org/rfc/rfc5321).

**Recommended stream bridge:** expose `Subscription.next_batch(limit)` as an async pull operation, then implement a Swift `AsyncSequence` wrapper with one outstanding pull per subscription. An optional callback can announce “data available,” coalesced to one outstanding wake-up. It should not carry an unbounded stream of individual row objects. Explicit `close()` releases the subscription; automatic disposal is a fallback, not the only lifecycle contract.

Use bounded queues for lossless command delivery. Tokio's bounded `mpsc` waits when capacity is exhausted; `watch` retains only the latest value and suits progress/settings snapshots, not ordered mailbox edits. Slow UI subscribers must not stall the store writer: bounded query buffers should return `ResetRequired` on overflow, then regenerate a window from authoritative state. [mpsc](https://docs.rs/tokio/latest/tokio/sync/mpsc/index.html), [watch](https://docs.rs/tokio/latest/tokio/sync/watch/index.html).

The Swift adapter receives immutable values away from UI isolation, then applies an ordered batch on `MainActor`. It must avoid creating one independent main-actor task per event, which could obscure ordering and grow queues. Neither `Sendable` nor an async return type is a main-thread guarantee. Keep callbacks nonblocking and never invoke foreign code while holding a store/engine lock. These are proposed contracts to test, rather than generator guarantees.

### 2.4 API granularity and 100,000-message mailboxes

A fine-grained domain API shares reusable services but can become chatty: one getter per row field multiplies boundary calls. A Crux-style application model shares more interaction behavior, but serializes events/effects/views and can over-centralize platform-specific presentation. Current Crux documentation discusses this serialization architecture explicitly. [Crux typegen](https://redbadger.github.io/crux/part-4/typegen.html).

**Recommendation: a hybrid application API.** Commands should express user intent—`archive_messages`, `save_draft`, `apply_rule`, `undo_operation`—while queries expose immutable, task-specific projections. Share sorting, threading, search language, rule execution and durable compose semantics. Keep focus, window layout, menu presentation, selection gestures and editor/IME state native. Avoid both a Rust property call for every cell and a universal serialized model containing every mailbox.

Matrix's `eyeball`/`eyeball-im` family supplies observable-state/vector machinery. The SDK timeline facade exposes the initial vector and `VectorDiff` batches rather than forcing the UI to repeatedly fetch a full timeline. That pattern is reusable, but mail requires its own ordering, identity and lag contract. [Observable-vector crate](https://docs.rs/crate/eyeball-im/latest), [Matrix timeline bridge](https://github.com/matrix-org/matrix-rust-sdk/blob/main/bindings/matrix-sdk-ffi/src/timeline/mod.rs).

**Proposed list contract, including 100,000+ messages:**

1. `open_query(spec, anchor, limit)` returns a query ID, generation, count information, stable message IDs and one row batch. Start experiments with **200 rows/page and a 600-row cache**; these are tuning proposals, not measured optimal sizes.
2. Rows contain only display fields: account/message ID, revision, sender, subject, date, flags, attachment indicator and bounded snippet. Use engine-generated identities with provider mappings, rather than trusting a message header as the local primary key. Fetch missing rows in batches. Preserve selection by ID, never by current position.
3. Use deterministic ordering with a unique tie-breaker, and keyset pagination for sequential scrolling. SQLite documents why `OFFSET` costs increase with skipped rows and how row-value comparisons support efficient scrolling. Random scrollbar jumps require rank/anchor support or a bounded query index, not blindly repeating large offsets. [SQLite scrolling example](https://www.sqlite.org/rowvalue.html).
4. `next_batch()` returns generation-tagged insert/remove/update/move edits for the materialized window, or invalidates it. Establish the snapshot/subscription atomically so no updates disappear between initial fetch and observation. Never drop one positional edit and apply later edits to the wrong base generation.
5. A query generation is not a long-lived SQLite read transaction. Advance or invalidate it as mail arrives; preserve a viewport anchor during refresh. Exact frozen search results can use a bounded materialized ID set with expiry. Avoid an indefinite snapshot that prevents checkpoints.

Bulk selection should use a core-side query/selection token or bounded ID batches, not marshal 100,000 full rows. Counts can be asynchronously computed and clearly distinguished from estimates. These are proposed behaviors; neither UniFFI nor native widget virtualization supplies them automatically.

**Bodies and attachments:** use separate `MessageHandle`/`BodyLease` APIs. Keep immutable raw message bytes and decoded parts outside row DTOs. A lease can provide bounded chunks or a permission-controlled file/descriptor for preview/export. Rust borrowing for synchronous UniFFI input does not make an async returned body zero-copy. Pointer-based sharing needs explicit retained ownership, lifetime and matching deallocation; across IPC use supported file/data transport instead of pointers. [UniFFI byte limitations](https://github.com/mozilla/uniffi-rs/blob/main/CHANGELOG.md), [XPC transport types](https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingXPCServices.html).

**Recommendation:** ship chunk/file transfer first, then optimize a measured hot path. Preserve original RFC 5322 messages independently of parsed or rendered content, so parser upgrades and search rebuilds are nondestructive. [Message format](https://www.rfc-editor.org/rfc/rfc5322).

### 2.5 macOS processes, extensions and data safety

**Closing a window is not quitting the application.** Sweetbriar can keep an in-process engine running with no windows if that is the intended app lifecycle. Continuing after the app actually quits requires a separately managed process.

Apple documents embedded XPC services as on-demand, crash-restarted helpers that may be terminated when idle; they must tolerate sudden termination. They are useful for isolation, but are not an unconditional always-running mail agent. Current `SMAppService`, available from macOS 13, registers login items, launch agents and daemons, with user authorization/status reflected by the API. Apple's sample includes a GUI-less agent and XPC communication. [XPC lifecycle](https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingXPCServices.html), [SMAppService](https://developer.apple.com/documentation/servicemanagement/smappservice), [agent sample](https://developer.apple.com/documentation/servicemanagement/updating-your-app-package-installer-to-use-the-new-service-management-api).

**Recommendation:** begin the engine prototype in one process. If sync after quitting is a first-release requirement, move its sole owner into a registered **per-user agent before release**. A small Swift host can handle Service Management/XPC and call the Rust service through the same generated bridge. The UI uses a native client protocol with local and XPC implementations. Account operations need neither root privileges nor a system daemon.

IPC is an additional boundary: UniFFI object handles cannot cross it. Define independently versioned request/response DTOs, operation IDs, subscription generations, size limits and reconnect behavior. After an agent crash, recover committed state and query operation status rather than blindly resubmitting a send. Bind connections to the intended application identity and test signed/sandboxed production bundles. This is proposed design; a successfully running development executable is insufficient proof of the packaged topology.

Acquire an exclusive engine-ownership lease before opening the writer. Agent startup and any in-process fallback must negotiate that ownership; they must never both become active writers because a connection briefly failed. This is a proposed cross-process invariant to exercise during startup, restart and upgrade tests.

**App Groups and extensions:** Apple supports shared containers and IPC for group members, including specific service-name rules for sandboxed macOS IPC. Obtain the container through the platform API, rather than hard-coding its path. App Group membership shares access; it does not coordinate database migrations or transactions. [App Groups entitlement](https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.security.application-groups), [extension sharing guidance](https://developer.apple.com/library/archive/documentation/General/Conceptual/ExtensibilityPG/ExtensionScenarios.html).

Recommended roles:

- **Share extension:** durably hand off a compose/import request before reporting success. If the agent is unavailable, use an atomic, versioned handoff file in the group container; the engine deduplicates and consumes it later. Do not create a second unrestricted database writer.
- **Notifications:** use native notification presentation. A notification service extension modifies eligible remote alerts under a time limit; it is not a general IMAP/background-sync executor. Give it only bounded cached data and required secrets. [Notification service contract](https://developer.apple.com/documentation/usernotifications/unnotificationserviceextension).
- **Spotlight:** export a derived Core Spotlight index with stable identifiers and explicit privacy choices. A macOS file-format importer is a separate plugin mechanism; Apple explicitly says Spotlight File Import extensions do not provide macOS functionality. [Core Spotlight](https://developer.apple.com/documentation/corespotlight).
- **Quick Look:** preview a leased attachment/exported message file; avoid teaching a preview extension to migrate or own the mailbox database. Native macOS attachment preview can use `QLPreviewPanel`. [Quick Look UI](https://developer.apple.com/documentation/quicklookui/qlpreviewpanel).

**SQLite across processes:** WAL supports same-machine readers and one writer, not a database on a network filesystem. Readers need appropriate WAL/shared-memory access; do not mark a changing live database `immutable`. Keep read transactions short, handle `SQLITE_BUSY`, and coordinate migrations through the sole owner. [WAL rules](https://www.sqlite.org/wal.html), [WAL file/locking model](https://www.sqlite.org/walformat.html). Prefer extension requests through the agent. If an extension must open the store locally, use a restricted read-only service through the same L1–L5 interfaces, without migration/write capabilities or a UI-to-SQL shortcut.

**Time-sensitive fix:** SQLite documents a rare WAL-reset race discovered on 2026-03-03, fixed in **3.51.3, released 2026-03-13**, with selected backports. Pin a release containing that fix and verify `sqlite_version()` in every shipped host/extension; “the OS has SQLite” is insufficient. [Release note](https://www.sqlite.org/releaselog/3_51_3.html), [race details](https://www.sqlite.org/wal.html).

**Recommended durability policy:** authoritative drafts, settings and operation journals use WAL with `synchronous=FULL`; evaluate macOS `fullfsync` deliberately. SQLite explains that WAL `NORMAL` can roll back acknowledged commits after power loss, whereas `FULL` adds a commit sync. File blobs require their own durable write/rename protocol before database references become committed. Report “Saved” only after the corresponding durable acknowledgement. [SQLite synchronization](https://www.sqlite.org/pragma.html).

Use transactional draft revisions and conflict detection for multiple compose windows. Preserve original blobs, recovery records and backups; never treat an index as the only copy. SQLite's online backup API provides a coherent backup while the database is live; copying only the main file during WAL activity does not capture the complete committed state. [Backup API](https://www.sqlite.org/backup.html), [WAL file role](https://www.sqlite.org/walformat.html). SQLite's durability model depends on filesystem/hardware assumptions; recovery/export and tested backup restore belong in the safety contract. [Atomic-commit assumptions](https://www.sqlite.org/atomiccommit.html).

### 2.6 Build, debugging, binary size and CI

**Packaging recommendation:** build one Rust `staticlib` through `sb-ffi`, generate Swift/C/module-map outputs using exactly the same UniFFI version, and package the library into an XCFramework. A source Swift target wraps a local development `binaryTarget` or a release ZIP with a checksum. Apple documents both binary-package forms and `xcodebuild -create-xcframework`. Keep architecture slices from different platforms separate; combine arm64/x86_64 only for the same macOS platform. [Swift packages](https://developer.apple.com/documentation/xcode/distributing-binary-frameworks-as-swift-packages), [XCFramework construction](https://developer.apple.com/documentation/xcode/creating-a-multi-platform-binary-framework-bundle).

`cargo-swift` automates UniFFI-based Swift packaging on macOS. Its README, checked 2026-10-11, requires matching tool/UniFFI versions and lists combinations through UniFFI 0.31.1; compatibility with the selected 0.32.1 must be established rather than assumed. A small repository-owned `xtask` build pipeline is a reasonable fallback. Consolidating Rust libraries also avoids accidentally linking several independently initialized cores into one host. [cargo-swift](https://github.com/antoniusnaumann/cargo-swift), [Mozilla consolidation](https://mozilla.github.io/application-services/book/howtos/locally-published-components-in-firefox-ios.html).

**Intel status, checked 2026-10-11:** Apple's WWDC25 statement calls macOS Tahoe the final major release for Intel Macs. Its compatibility page, published **2026-09-14**, includes the 2019 16-inch MacBook Pro, 2020 13-inch four-port MacBook Pro, 2020 27-inch iMac and 2019 Mac Pro. Therefore “macOS 26 supported” does not imply Apple-silicon-only. Rust supports `aarch64-apple-darwin` and `x86_64-apple-darwin`. [Apple statement](https://developer.apple.com/videos/play/wwdc2025/102/), [compatible hardware](https://support.apple.com/en-us/122867), [Rust targets](https://doc.rust-lang.org/rustc/platform-support/apple-darwin.html).

**Recommendation:** Apple silicon for the first development milestone; produce a universal macOS build if release policy includes those Intel users. Publish an explicit support policy instead of inferring demand from OS availability. No claim about Intel's remaining market share or future security-support duration was verified here.

For debugging, preserve Rust debug information through the final link and archive the final app/agent/extension binaries and matching dSYMs. Apple's symbolication requires matching build UUIDs and supports `crashlog`/`atos`; a static archive alone is not a substitute for final-image symbols. Mixed-stack debugging should be validated with a deliberate Rust panic and a Swift-triggered crash in a release-like build. [Apple symbolication](https://developer.apple.com/documentation/xcode/adding-identifiable-symbol-names-to-a-crash-report).

Binary size depends on dependencies, generic instantiations, debug information and linker decisions. Cargo profiles expose LTO, optimization, debug and stripping controls. **Recommendation:** retain production symbols as separate artifacts, measure per-architecture executable size/RSS/startup, and experiment with ThinLTO and reduced features. Do not impose a guessed megabyte budget or optimize away useful failure diagnostics before measurement. [Cargo profiles](https://doc.rust-lang.org/cargo/reference/profiles.html).

Proposed CI gates: Rust tests on macOS/Linux; generated-interface drift checks; strict Swift 6 compilation; native integration tests for cancellation and subscription disposal; minimum-supported/current OS runs; signed agent/extension packaging; and crash/restore fixtures. Exercise Intel on actual compatible hardware if it is supported. Record Rust/Xcode/generator versions and archive checksums, symbols and schema versions for every release. No app or benchmark was built locally during this research assignment.

### 2.7 Linux reuse

**Recommendation:** use Rust `gtk4` plus `libadwaita` for a GNOME/Ubuntu-oriented native UI, calling `sb-native-client` directly. This bypasses Swift FFI while preserving the same application service contract. The GTK main context remains the UI executor; the engine's Tokio runtime runs separately and returns values through bounded channels. GTK's guide demonstrates this division explicitly. [GTK event-loop guidance](https://gtk-rs.org/gtk4-rs/stable/latest/book/main_event_loop.html), [libadwaita Rust bindings](https://world.pages.gitlab.gnome.org/Rust/libadwaita-rs/stable/latest/docs/libadwaita/).

GTK scalable lists recycle visible widgets, but that does not automatically make the backing data windowed: its tutorial constructs a model with 100,000 GObjects. Implement a paged `GListModel`/presentation cache above Sweetbriar queries, rather than eagerly materializing every mail record. [GTK list model/view guide](https://gtk-rs.org/gtk4-rs/stable/latest/book/list_widgets.html).

An additional precedent is **Fractal**: its maintainer's 2021-05-07 architecture account separates GTK/GObject presentation from reusable `matrix-rust-sdk` functionality. This demonstrates direct Rust-core use from a native GTK shell, while also documenting adaptation between their different paradigms. It is historical architectural evidence, not a verified 2026 feature inventory. [Fractal internals](https://blogs.gnome.org/jsparber/2021/05/07/the-internals-of-fractal-next/).

Qt remains an alternative for a KDE-oriented front end; KDAB's `cxx-qt` provides Rust/Qt interoperability. **Trade-off:** libadwaita deliberately favors GNOME conventions, so “native Linux” needs a stated desktop target. The core should contain no GTK, Qt or Swift types, permitting another front end without altering storage/sync semantics. [CXX-Qt](https://github.com/KDAB/cxx-qt).

## 3. Options compared

### Core/UI boundary

| Boundary | Shared behavior | Native-shell cost | Main risk | Sweetbriar assessment |
|---|---|---|---|---|
| Fine-grained domain library | Protocol/storage/crypto primitives | High: each UI coordinates workflows | Repeated orchestration and chatty FFI | Too low-level as the only public API |
| Entire shared application/view model | Navigation, effects, model and UI projections | Lower for intentionally matching flows | Platform coupling and serialized large models | Useful for selected workflows, not the whole desktop UI |
| **Commands + observable query projections** | Durable operations, account semantics, query/rule/draft behavior | Native rendering and local interaction | Query/diff protocol needs careful design | **Recommended middle ground** |

This table is architectural assessment. The concrete precedents are component APIs at Mozilla, timelines at Matrix, and application models at Crux; their differing boundaries explain why no single “Rust core” pattern wins automatically. [Mozilla](https://firefox-source-docs.mozilla.org/rust-components/developing-rust-components/index.html), [Matrix](https://github.com/matrix-org/matrix-rust-sdk/blob/main/bindings/matrix-sdk-ffi/src/timeline/mod.rs), [Crux](https://github.com/redbadger/crux).

### Process choice

| Model | Survives closing windows | Sync after quitting UI | Failure isolation | Cost/constraint |
|---|---|---|---|---|
| One app process | Yes, if app remains running | No | Shared process | Simplest ownership/debugging |
| Embedded XPC service | While its lifecycle/work permits | Not a persistent-sync guarantee | Separate process | On-demand/idle termination; restart-safe state required |
| **Registered per-user agent + IPC** | Yes while enabled/running | **Designed for this** | Separate UI/engine | Registration, signing, reconnect, updates and user-disabled state |

These are lifecycle distinctions, not promises of continuous networking during sleep or logout. Apple documents XPC service management separately from registered background helpers. [XPC](https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingXPCServices.html), [SMAppService](https://developer.apple.com/documentation/servicemanagement/smappservice).

### Rust storage/search candidates

| Mechanism | Role | Benefit | Architectural obligation |
|---|---|---|---|
| `rusqlite` with deliberately bundled SQLite | Authoritative metadata, drafts/settings, operation journal | Explicit transactions; controlled SQLite build | Blocking worker, migrations, durable settings, backup/restore |
| SQLite FTS5 | Initial transactional full-text index | Same database/transaction system | Prove ranking, tokenizer and query-language quality |
| `tantivy` | Richer separate search index candidate | Dedicated Rust search engine | Rebuildable derived state, commit/reload coordination and indexing watermark |
| `mail-parser` / `mail-builder` | MIME parsing/composition candidate | Reusable Rust mail implementation | Preserve raw originals; fuzz malformed input; validate composition round trips |

The crate documentation verifies these mechanisms; the choice is a recommendation, not a measured Sweetbriar result. Tantivy has its own writer commits/readers, so atomicity with SQLite is not automatic. [rusqlite](https://docs.rs/rusqlite/latest/rusqlite/), [bundled build](https://github.com/rusqlite/rusqlite), [FTS5](https://www.sqlite.org/fts5.html), [Tantivy](https://docs.rs/tantivy/latest/tantivy/), [mail-parser](https://docs.rs/mail-parser/latest/mail_parser/), [mail-builder](https://docs.rs/mail-builder/latest/mail_builder/).

**Layering consequence:** initial FTS5 tables and queries belong inside `sb-store`, under its writer/transaction owner. Reserve the separate `sb-search` leaf for a derived external index such as Tantivy; it must not independently open and modify the authoritative SQLite database.

## 4. Recommendations and concrete layered architecture

### Proposed crate graph

**Strict rule:** cross-layer first-party calls/imports target only the immediately lower layer. The L5 leaves are independent and do not call/import each other. No UI-to-store shortcut, backend-to-store write, or search-to-MIME dependency. Third-party libraries and OS APIs are implementation dependencies of the leaf that owns them. Results and events return through the same intervening interfaces.

```text
L0  SweetbriarMac (SwiftUI + AppKit)       sb-ui-gtk (Rust GTK4)
    Swift agent / extension hosts        sb-cli (optional)
                    |                           |
                    v                           v
L1  SweetbriarBridge + sb-ffi              sb-native-client
    Generated Swift + native adapter      Rust presentation adapter
    Local/IPC transport hidden here       No FFI required
                    |                           |
                    +-------------+-------------+
                                  v
L2                         sb-service
                 Commands, receipts, query windows,
                 subscriptions, public DTOs/errors
                                  |
                                  v
L3                          sb-engine
                 EngineHost/runtime + domain workflows
                 Sync, drafts, send, rules, undo,
                 reconciliation and index scheduling
                                  |
                                  v
L4                         sb-resources
                 One facade over independent mechanisms;
                 maps their types, capabilities and errors
                                  |
                                  v
L5  sb-store | sb-mime | sb-search | sb-platform | account adapters
    SQLite/    MIME      optional    secrets/    sb-backend-imap
    FTS5/blobs            Tantivy     OS access  sb-backend-graph
                                                sb-backend-gmail
                                                sb-backend-jmap
                                                sb-backend-proton
                                  |
                                  v
    External crates, network protocols, filesystem and OS APIs
```

The diagram is a **logical application/dependency graph**, not a claim that FFI is IPC. In the agent deployment, L1 contains the client transport and agent-side bridge; L2–L5 run in the agent. In-process development omits that transport. Extensions use restricted L1 entry points. Neither deployment grants a client direct database access.

| Crate/package | Owns | Forbidden responsibility |
|---|---|---|
| `sb-ffi` / `SweetbriarBridge` | Generated exports, object lifetime and Swift concurrency/transport adaptation | Sync decisions, SQL or mailbox-wide UI snapshots |
| `sb-native-client` | Rust-native access to equivalent service operations; executor adaptation | GTK types inside lower layers |
| `sb-service` | Stable commands/errors, receipts, row projections, query generations and bounded subscriptions | Widgets, provider HTTP requests or SQLite statements |
| `sb-engine` | Domain invariants and coordination; one EngineHost runtime; durable draft/send/rule/undo workflows | Public Swift types or direct leaf imports |
| `sb-resources` | Adjacent-layer facade; resource construction, capability/error/type translation | User workflow policy or independent durable state |
| `sb-store` | Schema/migrations, transactions, FTS5, blob manifest, operation journal and backups | Parsing MIME, contacting providers or interpreting UI selection |
| `sb-mime` | Parse/build/decode/sanitize mechanism contracts | Persistence, network or UI rendering |
| `sb-search` | Optional external index/query mechanisms and index schema | Sole ownership of mail; direct store reads/writes |
| `sb-platform` | OS-backed secret/file capabilities | Window presentation or account business rules |
| `sb-backend-*` | Provider authentication/transport/delta mechanisms and capabilities | Writing Sweetbriar's store or calling other leaves |

Types belong to the layer that defines their contract and are **re-exported through each adjacent facade**. Do not introduce an all-purpose `sb-common` dependency imported everywhere: that would bypass the requested boundaries. Native adapters own their bridge DTOs so changing UniFFI does not contaminate domain code. Enforce permitted first-party dependency edges using Cargo metadata and source review.

This strict layering imposes forwarding/type-translation work. Keep facades small and task-oriented; do not invent an abstraction for every SQLite method. Start with modules for unimplemented provider leaves, extracting crates when independently useful. The named graph is the target ownership structure, not a requirement to scaffold every provider on day one.

### Durable workflow and provider boundaries

**Recommendation:** `sb-engine` coordinates a received raw message through `sb-resources`: parse, persist authoritative bytes/metadata, then schedule derived indexing. Leaves do not call each other. In the same store transaction, record an indexing work item and monotonically increasing revision; update the searchable watermark only after the index has committed and become visible. If indexing fails, the message remains safe and the job can retry.

Search grammar, threading, saved searches, filter ordering and undo semantics belong above individual providers. Adapters expose capabilities rather than pretending every account behaves like IMAP folders. Gmail's API exposes labels/message resources; JMAP specifies its own mail data model and query/change operations; Graph has a distinct message API. These justify a normalization boundary with provider-specific escape hatches, rather than copying IMAP assumptions into every UI. [Gmail messages](https://developers.google.com/workspace/gmail/api/reference/rest/v1/users.messages), [JMAP Mail, RFC 8621](https://www.rfc-editor.org/rfc/rfc8621), [Graph messages](https://learn.microsoft.com/en-us/graph/api/resources/message?view=graph-rest-1.0).

Reserve `sb-backend-proton` as an integration seam, **not a claim of permitted third-party access or a commitment to reuse Proton's application SDK**. Verify authentication, licensing and distribution requirements before implementing it. The Rust Mail source establishes architecture; it does not alone establish a supported third-party mail API. [Proton facade](https://github.com/ProtonMail/clients/blob/main/project/mail/rust/mail/mail-uniffi/src/lib.rs).

On macOS, use SwiftUI where it fits and AppKit where the mail table, editor, menus and multi-window interaction need finer native control. Treat that as a measured product choice. The shared core should persist compose revisions and durable operations; native text editing can maintain its immediate editing state and submit revisions without delegating every keystroke to an FFI view model.

### Trade-offs and strongest counterarguments

| Recommendation | Why choose it | Strongest argument against it |
|---|---|---|
| Shared Rust engine | One data-safety/backend/search implementation for a long-lived cross-platform product | Swift-only would reduce first-platform toolchains and debugging complexity; native UIs still cost separately |
| UniFFI initially | Shipping precedents and automatic domain bindings | Bulk records/callbacks may be materially slower than BoltFFI or carefully designed C; generated concurrency must be tested |
| Commands + windowed projections | Shares semantics while permitting different native interaction | More bespoke query/subscription design than a complete Crux application model |
| One writer, derived search | Clear recovery and migration ownership | Store-worker contention and index lag require scheduling/visibility policy |
| Optional per-user agent | Sync and accepted operations can outlive the UI | IPC, background authorization, upgrade races and diagnostics increase operating cost |
| Strict adjacent layers | Makes ownership auditable and prevents frontend/provider persistence shortcuts | Facade churn and mapping boilerplate can slow development if boundaries are too granular |

The strongest overall objection is **integration cost**: shared Rust plus native UI adds language, lifetime, scheduler and build boundaries while retaining much of each UI's development cost. 1Password's abandoned native Mac implementation and Element's long parity effort make that objection concrete. Sweetbriar's long-term backend diversity and data-safety goals justify accepting it, provided the initial product remains one platform and a narrow vertical slice. [1Password](https://1password.com/blog/1password-8-the-story-so-far), [Element](https://element.io/blog/element-x-and-pro-updates-a-glimpse-into-the-future/).

### First validation milestone

Build one account path and a real native mailbox, composer and search result window before expanding providers. Use representative **100,000 and 1,000,000-message fixtures**, malformed MIME and large attachments. Proposed acceptance targets are **under 100 ms p95 for a cached first-page query** and **under 4 ms of main-thread work per applied UI batch** on a declared reference Mac; these are targets requiring measurement, not established results.

Test cancellation before/after request registration, subscriber lag/reset, native disposal, engine shutdown and restart. Crash at draft/blob/journal commit boundaries; verify acknowledged saves recover and unacknowledged outcomes can be reconciled. Run disk-full and failed-write scenarios, simultaneous compose-window revisions, interrupted migrations, backup restore, and agent/UI version mismatch. Compile the exact exported async/callback shapes with strict Swift 6 concurrency. Benchmark UniFFI and BoltFFI using identical Sweetbriar row/buffer workloads only if the default boundary fails a target.

## 5. Risks and open questions

- **Background semantics:** must sync continue after explicit Quit, only after closing windows, or only while enabled? Sleep, logout, network loss and user-disabled background items need distinct status and recovery behavior.
- **Distribution/support policy:** Mac App Store versus direct distribution, minimum macOS, Intel demand and extension scope remain product decisions. Prototype the exact signed App Group/agent topology before committing to it.
- **Generator evolution:** pin runtime/generator versions together and retain generated-source review. Swift 6 callback isolation and cancellation must pass executable tests; current generator documentation is not proof for every API shape.
- **Durability scope:** define precisely when “Saved” and “Send accepted” appear, how local-only mail is backed up, and how disk-full, locked/corrupt stores and power loss are handled. Never silently recreate an inaccessible store.
- **Provider normalization:** account capabilities, remote identifiers, ambiguous sends and Graph/EWS differences must remain visible to domain workflows. Current EWS disablement and Graph gaps make this immediate. [Microsoft status, updated 2026-09-04](https://learn.microsoft.com/en-us/exchange/clients-and-mobile-in-exchange-online/deprecation-of-ews-exchange-online).
- **Search consistency and privacy:** decide acceptable indexing lag, multilingual/tokenizer quality, attachment indexing, encrypted-at-rest requirements and what content may enter Spotlight. “Best-in-class” requires comparative workload evaluation, not selection of a crate alone.
- **Native behavior:** GTK/libadwaita favors GNOME; Qt may better suit KDE. On macOS, keyboard editing, accessibility, menus and multi-window drafts need their own acceptance criteria.
- **Evidence limits:** proprietary 1Password Swift bridge details, a production Rust Drive core, universal strict-concurrency coverage of alternative generators and neutral end-to-end FFI benchmarks remain **(unverified)**. Public-source availability also does not ensure an externally reproducible product build, as Proton's iOS checkout warning demonstrates. [Proton warning](https://github.com/ProtonMail/ios-mail).

## 6. Sources consulted

All sources below were consulted on **2026-10-11**. Publication/release dates used for time-sensitive claims appear in the report. Repository links describe the inspected branch at that date; they do not imply a stable API. Apple pages that required JavaScript were also fetched through their official Markdown endpoints. Only consulted sources used by the report are listed; failed fetches and unused search results are omitted.

**Products and frameworks**

- Element/Matrix: [Element X iOS](https://github.com/element-hq/element-x-ios), [matrix-rust-sdk](https://github.com/matrix-org/matrix-rust-sdk), [timeline bridge](https://github.com/matrix-org/matrix-rust-sdk/blob/main/bindings/matrix-sdk-ffi/src/timeline/mod.rs), [task lifetime](https://github.com/matrix-org/matrix-rust-sdk/blob/main/bindings/matrix-sdk-ffi/src/task_handle.rs), [runtime](https://github.com/matrix-org/matrix-rust-sdk/blob/main/bindings/matrix-sdk-ffi/src/runtime.rs), [rewrite retrospective](https://element.io/blog/element-x-and-pro-updates-a-glimpse-into-the-future/), [eyeball-im](https://docs.rs/crate/eyeball-im/latest).
- Mozilla: [application-services](https://github.com/mozilla/application-services/blob/main/README.md), [component architecture](https://firefox-source-docs.mozilla.org/rust-components/developing-rust-components/index.html), [threading](https://firefox-source-docs.mozilla.org/rust-components/developing-rust-components/threading.html), [SuggestStore](https://mozilla.github.io/application-services/swift/Classes/SuggestStore.html), [Apple packaging](https://mozilla.github.io/application-services/book/howtos/locally-published-components-in-firefox-ios.html).
- Bitwarden: [native apps](https://bitwarden.com/blog/native-mobile-apps/), [language bindings](https://contributing.bitwarden.com/architecture/sdk/language-bindings/), [crate structure](https://contributing.bitwarden.com/architecture/sdk/crate-structure/), [internal SDK/release process](https://github.com/bitwarden/sdk-internal), [Client](https://github.com/bitwarden/sdk-internal/blob/main/crates/bitwarden-uniffi/src/lib.rs), [managed-state bridge](https://github.com/bitwarden/sdk-internal/blob/main/crates/bitwarden-uniffi/src/managed_settings.rs).
- 1Password: [Linux architecture](https://dteare.medium.com/behind-the-scenes-of-1password-for-linux-d59b19143a23), [1Password 8/Mac trade-offs](https://1password.com/blog/1password-8-the-story-so-far), [Typeshare](https://1password.com/blog/typeshare-for-rust), [2026 client TLS](https://1password.com/blog/post-quantum-tls-without-code-changes).
- Signal: [libsignal](https://github.com/signalapp/libsignal), [bridge architecture](https://github.com/signalapp/libsignal/blob/main/rust/bridge/README.md), [Swift bindings](https://github.com/signalapp/libsignal/blob/main/swift/README.md), [runtime/cancellation](https://github.com/signalapp/libsignal/blob/main/swift/Sources/LibSignalClient/TokioAsyncContext.swift), [async/streams](https://github.com/signalapp/libsignal/blob/main/swift/Sources/LibSignalClient/AsyncUtils.swift).
- Delta Chat/chatmail: [Rust core](https://github.com/chatmail/core), [C-to-RPC lessons and data loss](https://delta.chat/en/2025-02-11-why-jsonrpc-bindings-exist).
- Mullvad: [application](https://github.com/mullvad/mullvadvpn-app), [daemon/client architecture](https://github.com/mullvad/mullvadvpn-app/blob/main/docs/architecture.md), [iOS Rust scope](https://github.com/mullvad/mullvadvpn-app/blob/main/mullvad-ios/src/lib.rs), [C header generation](https://github.com/mullvad/mullvadvpn-app/blob/main/mullvad-ios/build.rs), [UniFFI tunnel adapter](https://github.com/mullvad/mullvadvpn-app/blob/main/mullvad-ios/src/tunnel_adapter/ffi/mod.rs), [Swift module configuration](https://raw.githubusercontent.com/mullvad/mullvadvpn-app/main/mullvad-ios/uniffi.toml).
- Proton: [Mail engineering](https://proton.me/blog/next-generation-proton-mail-mobile-apps), [current Rust facade](https://github.com/ProtonMail/clients/blob/main/project/mail/rust/mail/mail-uniffi/src/lib.rs), [repository relocation](https://github.com/ProtonMail/rust-mail), [iOS build warning](https://github.com/ProtonMail/ios-mail), [Pass common](https://github.com/protonpass/proton-pass-common), [Authenticator/Rust](https://proton.me/blog/authenticator-rust), [Drive SDK](https://github.com/ProtonDriveApps/sdk/blob/main/client/README.md), [Crux demonstration](https://github.com/ProtonMail/proton-rust-nation-2026/blob/main/README.md).
- Wire: [core-crypto architecture](https://github.com/wireapp/core-crypto/blob/main/cc-book/src/development/ARCHITECTURE.md), [binding documentation](https://wireapp.github.io/core-crypto/), [transaction/cancellation semantics](https://github.com/wireapp/core-crypto/blob/main/cc-book/src/concepts/client/transactions.md).
- Crux: [framework/status](https://github.com/redbadger/crux), [current typegen/binding architecture](https://redbadger.github.io/crux/part-4/typegen.html).
- Thunderbird: [Rust EWS integration](https://blog.thunderbird.net/2024/04/adventures-in-rust-bringing-exchange-support-to-thunderbird/), [Exchange shipping announcement](https://blog.thunderbird.net/2025/11/thunderbird-adds-native-microsoft-exchange-email-support/), [iOS announcement](https://blog.thunderbird.net/2026/10/thunderbird-for-ios-a-first-look-at-the-native-iphone-email-app-in-development/), [Swift core package](https://github.com/thunderbird/thunderbird-ios/blob/main/Core/Package.swift), [Swift core architecture](https://github.com/thunderbird/thunderbird-ios/blob/main/Core/README.md).
- Fractal: [native GTK/shared SDK architecture](https://blogs.gnome.org/jsparber/2021/05/07/the-internals-of-fractal-next/).

**FFI, runtimes and build tools**

- UniFFI: [Swift API](https://mozilla.github.io/uniffi-rs/latest/swift/overview.html), [async/cancellation](https://mozilla.github.io/uniffi-rs/latest/futures.html), [conversion internals](https://mozilla.github.io/uniffi-rs/latest/internals/lifting_and_lowering.html), [changelog](https://github.com/mozilla/uniffi-rs/blob/main/CHANGELOG.md), [released Swift async template](https://github.com/mozilla/uniffi-rs/blob/v0.32.1/uniffi_bindgen/src/bindings/swift/templates/Async.swift), [Swift 6 fix](https://github.com/mozilla/uniffi-rs/pull/2943).
- Alternatives: [swift-bridge](https://github.com/chinedufn/swift-bridge), [cbindgen](https://github.com/mozilla/cbindgen), [Diplomat book](https://rust-diplomat.github.io/diplomat/), [Diplomat Swift request](https://github.com/rust-diplomat/diplomat/issues/143), [swift-rs](https://github.com/Brendonovich/swift-rs).
- BoltFFI: [project](https://github.com/boltffi/boltffi), [benchmark harness/results](https://github.com/boltffi/boltffi/blob/main/benchmarks/README.md), [async contract](https://boltffi.dev/docs/async), [stream/drop semantics](https://boltffi.dev/docs/streaming).
- Rust: [FFI/unwinding](https://doc.rust-lang.org/nomicon/ffi.html), [Tokio runtime](https://docs.rs/tokio/latest/tokio/runtime/index.html), [CancellationToken](https://docs.rs/tokio-util/latest/tokio_util/sync/struct.CancellationToken.html), [bounded mpsc](https://docs.rs/tokio/latest/tokio/sync/mpsc/index.html), [watch](https://docs.rs/tokio/latest/tokio/sync/watch/index.html), [Apple targets](https://doc.rust-lang.org/rustc/platform-support/apple-darwin.html), [Cargo profiles](https://doc.rust-lang.org/cargo/reference/profiles.html), [cargo-swift](https://github.com/antoniusnaumann/cargo-swift).

**Platform, storage and protocol references**

- Apple background operation: [XPC lifecycle/transport](https://developer.apple.com/library/archive/documentation/MacOSX/Conceptual/BPSystemStartup/Chapters/CreatingXPCServices.html), [SMAppService](https://developer.apple.com/documentation/servicemanagement/smappservice), [agent sample](https://developer.apple.com/documentation/servicemanagement/updating-your-app-package-installer-to-use-the-new-service-management-api), [App Groups](https://developer.apple.com/documentation/bundleresources/entitlements/com.apple.security.application-groups), [extension sharing](https://developer.apple.com/library/archive/documentation/General/Conceptual/ExtensibilityPG/ExtensionScenarios.html).
- Apple integrations: [notification service extension](https://developer.apple.com/documentation/usernotifications/unnotificationserviceextension), [Core Spotlight](https://developer.apple.com/documentation/corespotlight), [Quick Look UI](https://developer.apple.com/documentation/quicklookui/qlpreviewpanel).
- Apple deployment/debugging: [binary Swift packages](https://developer.apple.com/documentation/xcode/distributing-binary-frameworks-as-swift-packages), [XCFramework creation](https://developer.apple.com/documentation/xcode/creating-a-multi-platform-binary-framework-bundle), [symbolication](https://developer.apple.com/documentation/xcode/adding-identifiable-symbol-names-to-a-crash-report), [WWDC25 Intel statement](https://developer.apple.com/videos/play/wwdc2025/102/), [Tahoe compatibility](https://support.apple.com/en-us/122867).
- SQLite: [WAL/concurrency/race](https://www.sqlite.org/wal.html), [WAL format/locks](https://www.sqlite.org/walformat.html), [3.51.3 fix](https://www.sqlite.org/releaselog/3_51_3.html), [synchronization pragmas](https://www.sqlite.org/pragma.html), [backup](https://www.sqlite.org/backup.html), [atomic-commit assumptions](https://www.sqlite.org/atomiccommit.html), [keyset scrolling](https://www.sqlite.org/rowvalue.html), [FTS5](https://www.sqlite.org/fts5.html).
- Rust mechanisms: [rusqlite API](https://docs.rs/rusqlite/latest/rusqlite/), [bundling policy](https://github.com/rusqlite/rusqlite), [Tantivy](https://docs.rs/tantivy/latest/tantivy/), [mail-parser](https://docs.rs/mail-parser/latest/mail_parser/), [mail-builder](https://docs.rs/mail-builder/latest/mail_builder/).
- Linux: [GTK executors](https://gtk-rs.org/gtk4-rs/stable/latest/book/main_event_loop.html), [GTK scalable lists](https://gtk-rs.org/gtk4-rs/stable/latest/book/list_widgets.html), [libadwaita Rust bindings](https://world.pages.gitlab.gnome.org/Rust/libadwaita-rs/stable/latest/docs/libadwaita/), [CXX-Qt](https://github.com/KDAB/cxx-qt).
- Mail/provider contracts: [RFC 5322](https://www.rfc-editor.org/rfc/rfc5322), [RFC 5321](https://www.rfc-editor.org/rfc/rfc5321), [JMAP Mail/RFC 8621](https://www.rfc-editor.org/rfc/rfc8621), [Gmail messages](https://developers.google.com/workspace/gmail/api/reference/rest/v1/users.messages), [Graph messages](https://learn.microsoft.com/en-us/graph/api/resources/message?view=graph-rest-1.0), [current Exchange Online EWS schedule](https://learn.microsoft.com/en-us/exchange/clients-and-mobile-in-exchange-online/deprecation-of-ews-exchange-online).