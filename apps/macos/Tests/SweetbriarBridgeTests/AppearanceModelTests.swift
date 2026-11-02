// `AppearanceModel` against a fake source (spec/07 "Other queries",
// "Subscriptions"; spec/02 "Appearance storage"): the snapshot,
// palette and assignment map field by field, an invalidation
// re-reads on the same stream, `resetRequired` re-reads and
// resubscribes at the new generation, a failed read keeps the last
// snapshot, and cancelling the follow ends its pull.

import Foundation
import Testing
@testable import SweetbriarBridge

/// A scripted `AppearanceSource`: the snapshot and palette as set,
/// subscriptions fed by `emit`; `appearance()` can be made to fail,
/// and every read and subscription is recorded.
actor FakeAppearanceSource: AppearanceSource {
    /// The palette `palette()` answers, fixed at init (it is a
    /// constant in the core, spec/02).
    nonisolated let paletteColors: [PaletteColor]
    private var snapshot = Appearance(generation: 0, accounts: [], identities: [])
    private var failure: (any Error)?
    /// Every `appearance()` call, counted on entry.
    private(set) var reads = 0
    /// Every stream `appearanceEvents` handed out, in order.
    private(set) var subscriptions: [FakeAppearanceEvents] = []

    init(palette: [PaletteColor] = []) {
        paletteColors = palette
    }

    /// Set the snapshot `appearance()` answers.
    func set(_ snapshot: Appearance) {
        self.snapshot = snapshot
    }

    /// Make `appearance()` throw `error` from now on.
    func fail(_ error: any Error) {
        failure = error
    }

    func appearance() async throws -> Appearance {
        reads += 1
        if let failure { throw failure }
        return snapshot
    }

    func appearanceEvents(generation: UInt64) async throws -> any AppearanceEventSource {
        let events = FakeAppearanceEvents(generation: generation)
        subscriptions.append(events)
        return events
    }

    nonisolated func palette() -> [PaletteColor] {
        paletteColors
    }
}

/// One fake appearance stream (spec/07): emitted events queue until a
/// `next` takes them; a `next` with none waits and ends on
/// cancellation.
actor FakeAppearanceEvents: AppearanceEventSource {
    /// The mark `appearanceEvents` handed it.
    let generation: UInt64
    private var queued: [AppearanceEvent] = []
    private var waiter: CheckedContinuation<AppearanceEvent, any Error>?
    /// The most `next` calls waiting at once.
    private(set) var maxWaiting = 0
    /// Waiting `next` calls that ended by cancellation.
    private(set) var cancelledNexts = 0
    private var waiting = 0

    init(generation: UInt64) {
        self.generation = generation
    }

    func emit(_ event: AppearanceEvent) {
        if let waiter {
            self.waiter = nil
            waiter.resume(returning: event)
            return
        }
        queued.append(event)
    }

    func next() async throws -> AppearanceEvent {
        if !queued.isEmpty {
            return queued.removeFirst()
        }
        try Task.checkCancellation()
        waiting += 1
        maxWaiting = max(maxWaiting, waiting)
        defer { waiting -= 1 }
        return try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { waiter = $0 }
        } onCancel: {
            Task { await self.cancelWaiter() }
        }
    }

    private func cancelWaiter() {
        guard let waiter else { return }
        self.waiter = nil
        cancelledNexts += 1
        waiter.resume(throwing: CancellationError())
    }
}

private let archiveTokens = AppearanceTokens(
    light: AppearanceVariant(
        markerSrgb: "#0072b2", stripSrgb: "#b7ddfd",
        foregroundSrgb: "#000000", outlineSrgb: "#005b90"
    ),
    dark: AppearanceVariant(
        markerSrgb: "#1a7ebe", stripSrgb: "#04314e",
        foregroundSrgb: "#ffffff", outlineSrgb: "#98d1ff"
    )
)

/// Tokens whose strings are all empty — an edge the codec must carry.
private let emptyTokens = AppearanceTokens(
    light: AppearanceVariant(
        markerSrgb: "", stripSrgb: "", foregroundSrgb: "", outlineSrgb: ""
    ),
    dark: AppearanceVariant(
        markerSrgb: "", stripSrgb: "", foregroundSrgb: "", outlineSrgb: ""
    )
)

/// A one-account snapshot at `generation`: `accountId`'s local
/// archive resolves to slot 0's blue (spec/02).
private func archive(generation: UInt64, accountId: String = "a1") -> Appearance {
    Appearance(
        generation: generation,
        accounts: [
            AccountAppearance(
                accountId: accountId, colorSrgb: "#0072b2",
                tokens: archiveTokens, symbol: nil, monogram: "L",
                collides: false, assignment: .paletteV1(slot: 0), revision: 1
            )
        ],
        identities: []
    )
}

/// The generated records map field by field (spec/07): `nil`
/// options, both assignment variants, empty strings, extreme
/// integers, non-ASCII text and every token byte.
@Test
func appearanceMappingKeepsEveryField() {
    let mapped = Appearance(.init(
        generation: .max,
        accounts: [
            .init(
                accountId: "work", colorSrgb: "#112233",
                tokens: .init(
                    light: .init(
                        markerSrgb: "#0072b2", stripSrgb: "#b7ddfd",
                        foregroundSrgb: "#000000", outlineSrgb: "#005b90"
                    ),
                    dark: .init(
                        markerSrgb: "#1a7ebe", stripSrgb: "#04314e",
                        foregroundSrgb: "#ffffff", outlineSrgb: "#98d1ff"
                    )
                ),
                symbol: "Lé", monogram: "Lé", collides: true,
                assignment: .custom, revision: .max
            ),
            .init(
                accountId: "home", colorSrgb: "#e69f00",
                tokens: .init(
                    light: .init(markerSrgb: "", stripSrgb: "", foregroundSrgb: "", outlineSrgb: ""),
                    dark: .init(markerSrgb: "", stripSrgb: "", foregroundSrgb: "", outlineSrgb: "")
                ),
                symbol: nil, monogram: "H", collides: false,
                assignment: .paletteV1(slot: .max), revision: 1
            ),
        ],
        identities: [
            .init(
                identityId: "ada", accountId: "work", colorSrgb: "#445566",
                colorOverrideSrgb: "#445566",
                tokens: .init(
                    light: .init(
                        markerSrgb: "#0072b2", stripSrgb: "#b7ddfd",
                        foregroundSrgb: "#000000", outlineSrgb: "#005b90"
                    ),
                    dark: .init(
                        markerSrgb: "#1a7ebe", stripSrgb: "#04314e",
                        foregroundSrgb: "#ffffff", outlineSrgb: "#98d1ff"
                    )
                ),
                symbolOverride: "AB", monogram: "AB", revision: 7
            ),
            .init(
                identityId: "zéd", accountId: "home", colorSrgb: "#0072b2",
                colorOverrideSrgb: nil,
                tokens: .init(
                    light: .init(markerSrgb: "", stripSrgb: "", foregroundSrgb: "", outlineSrgb: ""),
                    dark: .init(markerSrgb: "", stripSrgb: "", foregroundSrgb: "", outlineSrgb: "")
                ),
                symbolOverride: nil, monogram: "Z", revision: nil
            ),
        ]
    ))
    #expect(mapped == Appearance(
        generation: .max,
        accounts: [
            AccountAppearance(
                accountId: "work", colorSrgb: "#112233",
                tokens: archiveTokens, symbol: "Lé", monogram: "Lé",
                collides: true, assignment: .custom, revision: .max
            ),
            AccountAppearance(
                accountId: "home", colorSrgb: "#e69f00",
                tokens: emptyTokens, symbol: nil, monogram: "H",
                collides: false, assignment: .paletteV1(slot: .max), revision: 1
            ),
        ],
        identities: [
            IdentityAppearance(
                identityId: "ada", accountId: "work", colorSrgb: "#445566",
                colorOverrideSrgb: "#445566", tokens: archiveTokens,
                symbolOverride: "AB", monogram: "AB", revision: 7
            ),
            IdentityAppearance(
                identityId: "zéd", accountId: "home", colorSrgb: "#0072b2",
                colorOverrideSrgb: nil, tokens: emptyTokens,
                symbolOverride: nil, monogram: "Z", revision: nil
            ),
        ]
    ))
}

/// The palette record and the assignment enum map case by case,
/// payloads and boundary slots included (spec/02).
@Test
func paletteAndAssignmentMappingsKeepEveryCase() {
    #expect(PaletteColor(.init(slot: .max, name: "Jaune-é", colorSrgb: "#f0e442"))
        == PaletteColor(slot: .max, name: "Jaune-é", colorSrgb: "#f0e442"))
    #expect(PaletteColor(.init(slot: 0, name: "Blue", colorSrgb: "#0072b2"))
        == PaletteColor(slot: 0, name: "Blue", colorSrgb: "#0072b2"))
    #expect(AppearanceAssignment(.paletteV1(slot: .max)) == .paletteV1(slot: .max))
    #expect(AppearanceAssignment(.paletteV1(slot: 0)) == .paletteV1(slot: 0))
    #expect(AppearanceAssignment(.custom) == .custom)
}

/// `follow` publishes the snapshot and palette, subscribes at its
/// generation, and re-reads on an invalidation — on the same stream.
@MainActor
@Test(.timeLimit(.minutes(1)))
func aFollowLoadsThenReReadsOnInvalidation() async throws {
    let fake = FakeAppearanceSource(
        palette: [PaletteColor(slot: 0, name: "Blue", colorSrgb: "#0072b2")]
    )
    await fake.set(archive(generation: 0))
    let model = AppearanceModel(source: fake)

    let follow = Task { await model.follow() }
    try #require(await eventually { await fake.subscriptions.count == 1 })
    #expect(model.appearance == archive(generation: 0))
    #expect(model.palette == [PaletteColor(slot: 0, name: "Blue", colorSrgb: "#0072b2")])
    #expect(model.lastError == nil)
    #expect(await fake.subscriptions.map(\.generation) == [0])

    await fake.set(archive(generation: 1))
    await fake.subscriptions[0].emit(.invalidated(generation: 1))
    try #require(await eventually { await fake.reads == 2 })
    #expect(model.appearance == archive(generation: 1))
    #expect(await fake.subscriptions.count == 1)

    follow.cancel()
    await follow.value
}

/// `resetRequired` re-reads and resubscribes at the answer's
/// generation (spec/07).
@MainActor
@Test(.timeLimit(.minutes(1)))
func aResetReReadsAndResubscribesAtTheNewGeneration() async throws {
    let fake = FakeAppearanceSource()
    await fake.set(archive(generation: 0))
    let model = AppearanceModel(source: fake)

    let follow = Task { await model.follow() }
    try #require(await eventually { await fake.subscriptions.count == 1 })

    await fake.set(archive(generation: 33))
    await fake.subscriptions[0].emit(.resetRequired)
    try #require(await eventually { await fake.subscriptions.count == 2 })
    #expect(model.appearance == archive(generation: 33))
    #expect(await fake.subscriptions.map(\.generation) == [0, 33])

    follow.cancel()
    await follow.value
}

/// A failed re-read ends the follow with `lastError`, keeping the
/// snapshot it showed.
@MainActor
@Test(.timeLimit(.minutes(1)))
func aFailedReReadKeepsTheSnapshot() async throws {
    let fake = FakeAppearanceSource()
    await fake.set(archive(generation: 0))
    let model = AppearanceModel(source: fake)

    let follow = Task { await model.follow() }
    try #require(await eventually { await fake.subscriptions.count == 1 })

    await fake.fail(CoreFailure.corrupt(detail: "damaged"))
    await fake.subscriptions[0].emit(.invalidated(generation: 1))
    await follow.value
    #expect(model.appearance == archive(generation: 0))
    #expect(model.lastError != nil)
}

/// Cancelling the task running `follow` cancels the parked `next`
/// and ends the loop quietly: `lastError` stays nil and the model
/// keeps what it showed.
@MainActor
@Test(.timeLimit(.minutes(1)))
func cancellingTheFollowEndsItsPull() async throws {
    let fake = FakeAppearanceSource()
    await fake.set(archive(generation: 0))
    let model = AppearanceModel(source: fake)

    let follow = Task { await model.follow() }
    try #require(await eventually { await fake.subscriptions.count == 1 })
    let events = await fake.subscriptions[0]
    try #require(await eventually { await events.maxWaiting == 1 })

    follow.cancel()
    await follow.value
    #expect(await events.cancelledNexts == 1)
    #expect(model.lastError == nil)
    #expect(model.appearance == archive(generation: 0))
}

/// `appearance()` and `palette()` cross the real bridge field by
/// field (spec/07, spec/02): a fresh core resolves its local archive
/// at generation 0 — slot 0's blue, the palette assignment, the
/// derived tokens the shell draws — and the seven v1 presets.
@Test(.timeLimit(.minutes(1)))
func appearanceAndPaletteCrossTheRealBridge() async throws {
    let (dir, config) = tempConfig()
    defer { try? FileManager.default.removeItem(at: dir) }
    let bridge = try await CoreBridge.open(
        config: config, snapshots: await sinkBatcher(clock: ManualFrameClock())
    )
    let archiveAccount = try #require(try await bridge.accounts().first)

    let snapshot = try await bridge.appearance()
    #expect(snapshot == archive(generation: 0, accountId: archiveAccount.accountId))

    let palette = bridge.palette()
    #expect(palette.map(\.slot) == [0, 1, 2, 3, 4, 5, 6])
    #expect(palette.map(\.name) == [
        "Blue", "Orange", "Bluish green", "Reddish purple",
        "Sky blue", "Vermilion", "Yellow"
    ])
    #expect(palette.map(\.colorSrgb) == [
        "#0072b2", "#e69f00", "#009e73", "#cc79a7",
        "#56b4e9", "#d55e00", "#f0e442"
    ])

    await bridge.shutdown()
}
