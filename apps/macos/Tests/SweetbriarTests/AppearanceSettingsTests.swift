import AppKit
import Testing
import SweetbriarBridge
@testable import Sweetbriar

@MainActor
struct AppearanceSettingsTests {
    @Test func customColorValidation() {
        #expect(AppearanceSettingsModel.canonicalColor(.red) == "#ff0000")
        #expect(AppearanceSettingsModel.canonicalColor(NSColor.red.withAlphaComponent(0.5)) == nil)
        #expect(AppearanceSettingsModel.canonicalColor(NSColor(patternImage: NSImage(size: NSSize(width: 1, height: 1)))) == nil)
    }

    @Test func presetSelectionUsesDisplayedRevisionAndPreservesSymbol() async throws {
        let source = SettingsAppearanceSource()
        let appearance = AppearanceModel(source: source)
        await appearance.refresh()
        let model = AppearanceSettingsModel(appearance: appearance)
        await model.choose(.paletteV1(slot: 1), account: settingsAccount)
        #expect(await source.calls == [SettingsAppearanceSource.Call(revision: 3, change: .init(color: .paletteV1(slot: 1), symbol: "S"))])
        #expect(model.error == nil)
    }

    @Test func samePresetDoesNotWrite() async {
        let source = SettingsAppearanceSource()
        let appearance = AppearanceModel(source: source)
        await appearance.refresh()
        let model = AppearanceSettingsModel(appearance: appearance)
        await model.choose(.paletteV1(slot: 0), account: settingsAccount)
        #expect(await source.calls.isEmpty)
    }

    @Test func conflictRefreshesAndNeverRetriesStaleChoice() async {
        let source = SettingsAppearanceSource()
        await source.setFailure(.conflict(detail: "stale"))
        let appearance = AppearanceModel(source: source)
        await appearance.refresh()
        let model = AppearanceSettingsModel(appearance: appearance)
        await model.choose(.paletteV1(slot: 1), account: settingsAccount)
        #expect(model.error != nil)
        #expect(await source.calls.count == 1)
        #expect(await source.reads == 2)
    }

    @Test func staleDialogCannotOverwriteNewerRevision() async {
        let source = SettingsAppearanceSource()
        let appearance = AppearanceModel(source: source)
        await appearance.refresh()
        let model = AppearanceSettingsModel(appearance: appearance)
        await source.externalChange()
        await appearance.refresh()
        await model.choose(.custom(colorSrgb: "#abcdef"), account: settingsAccount)
        #expect(await source.calls.isEmpty)
        #expect(model.error != nil)
    }

    @Test func committedChangeReachesBothWindows() async throws {
        let source = SettingsAppearanceSource()
        let shared = SharedAppearance()
        let first = shared.model(source: source)
        let second = shared.model(source: source)
        for _ in 0..<200 {
            if first.appearance != nil, await source.waiting { break }
            try await Task.sleep(for: .milliseconds(10))
        }
        let model = AppearanceSettingsModel(appearance: first)
        await model.choose(.paletteV1(slot: 1), account: settingsAccount)
        for _ in 0..<200 where second.appearance?.generation != 1 {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(first === second)
        #expect(second.appearance?.accounts.first?.assignment == .paletteV1(slot: 1))
        if let committed = second.appearance?.accounts.first {
            await model.choose(.paletteV1(slot: 1), account: committed)
        }
        #expect(await source.calls.count == 1)
        #expect(await source.subscriptions == 1)
    }

    @Test func cancellationAfterCommitRefreshesWithoutClaimingFailure() async {
        let source = SettingsAppearanceSource()
        await source.cancelAfterCommit()
        let appearance = AppearanceModel(source: source)
        await appearance.refresh()
        let model = AppearanceSettingsModel(appearance: appearance)
        await model.choose(.paletteV1(slot: 1), account: settingsAccount)
        #expect(appearance.appearance?.accounts.first?.assignment == .paletteV1(slot: 1))
        #expect(model.error?.contains("interrupted") == true)
        #expect(await source.calls.count == 1)
    }

    @Test func droppingWindowDoesNotAbandonInFlightSave() async throws {
        let source = SettingsAppearanceSource()
        await source.holdNextWrite()
        let appearance = AppearanceModel(source: source)
        await appearance.refresh()
        var model: AppearanceSettingsModel? = AppearanceSettingsModel(appearance: appearance)
        let task = Task { [model] in await model?.choose(.paletteV1(slot: 1), account: settingsAccount) }
        for _ in 0..<200 where !(await source.writeWaiting) {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(await source.writeWaiting)
        model = nil
        await source.releaseWrite()
        await task.value
        #expect(appearance.appearance?.accounts.first?.assignment == .paletteV1(slot: 1))
        #expect(await source.calls.count == 1)
    }

    @Test func failureDoesNotChangeCommittedAppearance() async {
        let source = SettingsAppearanceSource()
        await source.setFailure(.unavailable(reason: "disk"))
        let appearance = AppearanceModel(source: source)
        await appearance.refresh()
        let model = AppearanceSettingsModel(appearance: appearance)
        await model.choose(.paletteV1(slot: 1), account: settingsAccount)
        #expect(model.error != nil)
        #expect(appearance.appearance?.accounts.first?.assignment == .paletteV1(slot: 0))
    }
}

private let settingsAccount = AccountAppearance(
    accountId: "one", colorSrgb: "#000000",
    tokens: .init(light: .init(markerSrgb: "#000000", stripSrgb: "#ffffff", foregroundSrgb: "#000000", outlineSrgb: "#000000"), dark: .init(markerSrgb: "#ffffff", stripSrgb: "#000000", foregroundSrgb: "#ffffff", outlineSrgb: "#ffffff")),
    symbol: "S", monogram: "S", collides: false, assignment: .paletteV1(slot: 0), revision: 3
)

private actor SettingsAppearanceSource: AppearanceSource, AppearanceEventSource {
    struct Call: Equatable { let revision: Int64; let change: AccountAppearanceChange }
    private(set) var calls: [Call] = []
    private(set) var reads = 0
    private var failure: CoreFailure?
    private var cancelOnCommit = false
    private var holdWrite = false
    private var writeContinuation: CheckedContinuation<Void, Never>?
    var writeWaiting: Bool { writeContinuation != nil }
    func holdNextWrite() { holdWrite = true }
    func releaseWrite() { writeContinuation?.resume(); writeContinuation = nil }
    private var account = settingsAccount
    private var generation: UInt64 = 0
    private var continuation: CheckedContinuation<AppearanceEvent, any Error>?
    private(set) var subscriptions = 0
    var waiting: Bool { continuation != nil }
    func cancelAfterCommit() { cancelOnCommit = true }
    func externalChange() { account.revision += 1; generation += 1 }
    func setFailure(_ value: CoreFailure) { failure = value }
    func appearance() async throws -> Appearance { reads += 1; return .init(generation: generation, accounts: [account], identities: []) }
    func appearanceEvents(generation: UInt64) async throws -> any AppearanceEventSource {
        subscriptions += 1
        return self
    }
    func next() async throws -> AppearanceEvent {
        try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation = $0 }
        } onCancel: { Task { await self.cancelWait() } }
    }
    private func cancelWait() {
        continuation?.resume(throwing: CancellationError())
        continuation = nil
    }
    func setAccountAppearance(accountId: String, expectedRevision: Int64, change: AccountAppearanceChange) async throws -> Receipt {
        calls.append(.init(revision: expectedRevision, change: change))
        if let failure { throw failure }
        if holdWrite {
            holdWrite = false
            await withCheckedContinuation { writeContinuation = $0 }
        }
        switch change.color {
        case .paletteV1(let slot): account.assignment = .paletteV1(slot: slot)
        case .custom(let color): account.assignment = .custom; account.colorSrgb = color
        }
        account.revision += 1
        generation += 1
        continuation?.resume(returning: .invalidated(generation: generation))
        continuation = nil
        if cancelOnCommit { throw CancellationError() }
        return .init(command: "set", resultJson: "{}", createdAtMs: 0)
    }
    func setIdentityAppearance(identityId: String, expectedRevision: Int64?, change: IdentityAppearanceChange) async throws -> Receipt { fatalError("unused") }
    nonisolated func palette() -> [PaletteColor] { [] }
}
