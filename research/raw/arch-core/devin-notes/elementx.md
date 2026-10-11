# Element X: Sharing a Rust Core Between Native UIs

Research case study notes (Oct 2026). Element X is the total rewrite of Element's
iOS and Android clients on top of `matrix-rust-sdk`, bridged into Swift/Kotlin via
the `matrix-sdk-ffi` crate and Mozilla's UniFFI.

## 1. What lives in Rust vs. native Swift/Kotlin

The Rust side owns essentially all Matrix "guts": the sync loop (Sliding Sync),
end-to-end encryption (`matrix-sdk-crypto` + vodozemac), room state, the event
cache, timeline and room-list logic, media, and persistence (SQLite via
`matrix-sdk-sqlite`). The layering is: `matrix-sdk` (mid-level client) →
`matrix-sdk-ui` (high-level UI-oriented primitives: room list service, timeline)
→ `matrix-sdk-ffi` (UniFFI bindings over "important concepts" of both). The
native apps keep only UI and product opinions: SwiftUI/Jetpack Compose screens,
navigation (MVVM-C coordinators on iOS), design system (Compound), app services,
and SDK-wrapping "proxy" services. Element X iOS's AGENTS.md states the rule
explicitly: "Services = where product-level opinions live (Rust SDK stays
spec-faithful)", and views never import `MatrixRustSDK` directly — FFI types are
mapped to app-owned Swift types via `init(rustValue:)` / `var rustValue`.

## 2. The FFI layer

`matrix-sdk-ffi` is a facade crate of small Rust wrappers over `matrix-sdk` /
`matrix-sdk-ui`, exposed to UniFFI. It uses **proc-macros** (`#[uniffi::export]`,
`#[uniffi::Record]`, etc.) as the preferred mechanism, with a legacy `api.udl`
file still present; the bindings CONTRIBUTING.md says "Attributes are preferred,
where applicable." A companion crate `matrix-sdk-ffi-macros` (v0.7.0) holds
custom macros. UniFFI generates Swift and Kotlin bindings plus C headers/module
maps; the Apple build (`cargo xtask swift build-framework`) compiles static libs
for iOS/simulator/macOS, lipo's fat binaries, runs `uniffi-bindgen-swift`, and
packages an **XCFramework** (`MatrixSDKFFI.xcframework`) with generated Swift
sources. Distribution is via Swift Package Manager: the official
`matrix-org/matrix-rust-components-swift` repo vendors the prebuilt XCFramework
as an SPM binary target. Element X iOS, however, consumes Element's own
"cutdown" fork `element-hq/matrix-rust-components-swift` (iOS + simulator only,
versioning decoupled from the official package), imported via XcodeGen
`project.yml`. Android gets an AAR via `matrix-rust-components-kotlin` on Maven.
Binary size is actively managed: iOS uses an `opt-level = "s"` profile (~33%
smaller), Android adds LTO + stripping (almost halving size).

## 3. Live updates across the boundary

Updates flow as **callback listeners**, not async streams. The FFI exposes
listener protocols (e.g. `TimelineListener.onUpdate(diff: [VectorDiff])`,
`RoomListEntriesListener.onUpdate(roomEntriesUpdate: [RoomListEntriesUpdate])`)
where `RoomListEntriesUpdate` is an enum of list operations (append, clear,
pushFront/Back, popFront/Back, insert, set, remove, truncate, reset). Subscriptions
are eyeball-based: `RoomListService.allRooms().entriesWithDynamicAdapters(...)`
returns a handle whose `controller()` can set filters, and `TaskHandle` values
let Swift cancel subscriptions. `SyncService` (built from
`client.syncService().finish()`) owns the sync loop and vends the room list
service. On the Swift side, Element X iOS (MVVM-C, Swift 6) implements these
listener protocols in services/view models and bridges results into the UI with
the Swift Observation framework (`@Observable`, `StateStoreViewModelV2`) and
Combine `PassthroughSubject`s for coordinator actions; older screens still use
`ObservableObject`. The UniFFI async support itself was contributed by Element:
"cancellable async bindings to uniffi … complete with beautifully simple
async/await non-blocking semantics" (matrix.org Matrix 2.0 blog).

## 4. Why a facade instead of the raw SDK API

UniFFI cannot express the full Rust SDK surface (generics, traits, async
streams, lifetimes), so `matrix-sdk-ffi` curates a UI-oriented subset: records,
enums, and `uniffi::Object` interfaces with async methods and listener
callbacks. This also lets the SDK team evolve the internal API freely while
keeping a stable, idiomatic foreign surface, and it builds on `matrix-sdk-ui`
so apps get battle-tested room-list/timeline logic instead of reinventing it —
"the vast majority of the heavy lifting can be handled in matrix-rust-sdk …
performance improvements can all be handled in one place to the benefit of all
clients" (matrix.org).

## 5. What went well, what hurt

**Well:** one shared engine — "bug fixes on one platform immediately benefit the
other" (Element X: Ignition); instant launch/sync from Sliding Sync + Rust
performance; solid E2EE via vodozemac; memory/concurrency safety. **Painful:**
Sliding Sync's first implementation "ended up being a bit too complicated" with
simplifications expected (matrix.org); Element had to upstream cancellable async
support to UniFFI; they maintain a forked Swift components package because the
official one supports more platforms than they need; and they hand-tuned Cargo
profiles to control framework binary size. The Swift API is explicitly unstable
("the API could change at any point").

## 6. Current versions (Oct 2026, from workspace `Cargo.toml` on main)

- `matrix-sdk` / `matrix-sdk-ffi`: **0.19.1**
- `uniffi` / `uniffi_bindgen`: **0.32.1**
- `matrix-sdk-ffi-macros`: **0.7.0**
- `matrix-rust-components-swift` (official) latest tag seen: 25.11.11 (Nov 2025)

## Sources

- https://github.com/matrix-org/matrix-rust-sdk/blob/main/ARCHITECTURE.md
- https://github.com/matrix-org/matrix-rust-sdk/blob/main/Cargo.toml
- https://github.com/matrix-org/matrix-rust-sdk/blob/main/bindings/CONTRIBUTING.md
- https://github.com/matrix-org/matrix-rust-sdk/blob/main/bindings/matrix-sdk-ffi/Cargo.toml
- https://github.com/matrix-org/matrix-rust-sdk/blob/main/bindings/apple/README.md
- https://github.com/matrix-org/matrix-rust-components-swift (README, Walkthrough example)
- https://github.com/element-hq/matrix-rust-components-swift (README)
- https://github.com/element-hq/element-x-ios (README, AGENTS.md)
- https://github.com/matrix-org/matrix-rust-components-kotlin
- https://matrix.org/blog/2023/09/matrix-2-0
- https://element.io/blog/element-x-experience-the-future-of-element
- https://element.io/blog/element-x-ignition
- https://element.io/blog/deep-dive-into-element-x
- https://mozilla.github.io/uniffi-rs/ (proc-macros, uniffi-bindgen-swift docs)
