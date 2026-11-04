#if canImport(Sweetbriar)
@testable import Sweetbriar
#endif
import AppKit
import SweetbriarBridge
import Testing

@Suite("Sidebar appearance")
@MainActor
struct SidebarAppearanceTests {
    @Test func duplicateNamesHaveStableDistinctLabels() {
        let first = SidebarModel.Section(id: "account-1", title: "Work", entries: [])
        let second = SidebarModel.Section(id: "account-2", title: "Work", entries: [])
        let other = SidebarModel.Section(id: "account-3", title: "West", entries: [])
        let sections = [first, second, other]
        #expect(SidebarViewController.accountLabel(first, among: sections) == "Work · account-1")
        #expect(SidebarViewController.accountLabel(second, among: Array(sections.reversed())) == "Work · account-2")
        #expect(SidebarViewController.accountLabel(other, among: sections) == "West")
        let cell = SidebarHeaderCell(identifier: .init("test"))
        cell.configure(title: SidebarViewController.accountLabel(first, among: sections), account: nil, isAccount: true)
        #expect(cell.accessibilityLabel() == "Account, Work · account-1")
    }

    @Test func badgesDrawInLightDarkAndHighContrast() throws {
        let view = SidebarMonogramView(frame: NSRect(x: 0, y: 0, width: 20, height: 20))
        view.account = sampleAccount
        for name: NSAppearance.Name in [.aqua, .darkAqua, .accessibilityHighContrastAqua, .accessibilityHighContrastDarkAqua] {
            view.appearance = NSAppearance(named: name)
            view.needsDisplay = true
            let bitmap = try #require(view.bitmapImageRepForCachingDisplay(in: view.bounds))
            view.cacheDisplay(in: view.bounds, to: bitmap)
            let pixel = try #require(bitmap.colorAt(x: 10, y: 3)?.usingColorSpace(.sRGB))
            let dark = name == .darkAqua || name == .accessibilityHighContrastDarkAqua
            #expect(dark ? pixel.redComponent < 0.5 : pixel.redComponent > 0.5)
        }
    }

    @Test func windowsShareCommittedUpdates() async throws {
        let source = SidebarAppearanceSource()
        let shared = SharedAppearance()
        let first = shared.model(source: source)
        let second = shared.model(source: source)
        #expect(first === second)
        let sidebarModel = SidebarModel(source: SidebarAccounts())
        await sidebarModel.reload()
        let left = SidebarViewController(model: sidebarModel, appearance: first)
        let right = SidebarViewController(model: sidebarModel, appearance: second)
        _ = left.view
        _ = right.view
        left.reloadSections()
        right.reloadSections()
        for _ in 0..<200 where first.appearance?.generation != 0 {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(first.appearance?.generation == 0)
        for _ in 0..<200 where !(await source.waiting) {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(await source.waiting)
        await source.update()
        for _ in 0..<200 where second.appearance?.generation != 1 {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(first.appearance?.generation == 1)
        #expect(second.appearance?.generation == 1)
        for _ in 0..<200 where header(left)?.badge.account?.monogram != "N" || header(right)?.badge.account?.monogram != "N" {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(header(left)?.badge.account?.monogram == "N")
        #expect(header(right)?.badge.account?.monogram == "N")
        #expect(await source.subscriptions == 1)
    }

    private func header(_ sidebar: SidebarViewController) -> SidebarHeaderCell? {
        guard let scroll = sidebar.view as? NSScrollView,
              let outline = scroll.documentView as? NSOutlineView else { return nil }
        for row in 0..<outline.numberOfRows {
            if let cell = outline.view(atColumn: 0, row: row, makeIfNecessary: true) as? SidebarHeaderCell,
               cell.accessibilityLabel() == "Account, Work" { return cell }
        }
        return nil
    }
}

private struct SidebarAccounts: SidebarSource {
    func accounts() async throws -> [Account] {
        [Account(accountId: "account-1", kind: .local, displayName: "Work", state: .active, stateReason: nil, sortOrder: 0)]
    }
    func mailboxes(accountId: String) async throws -> Mailboxes {
        Mailboxes(generation: 0, mailboxes: [])
    }
}

private let sampleAccount = AccountAppearance(
    accountId: "account-1", colorSrgb: "#0072b2",
    tokens: AppearanceTokens(
        light: AppearanceVariant(markerSrgb: "#0072b2", stripSrgb: "#ffffff", foregroundSrgb: "#000000", outlineSrgb: "#000000"),
        dark: AppearanceVariant(markerSrgb: "#56b4e9", stripSrgb: "#000000", foregroundSrgb: "#ffffff", outlineSrgb: "#ffffff")
    ), symbol: nil, monogram: "W", collides: true, assignment: .custom, revision: 0
)

private actor SidebarAppearanceSource: AppearanceSource, AppearanceEventSource {
    private var generation: UInt64 = 0
    private var continuation: CheckedContinuation<AppearanceEvent, any Error>?
    private(set) var subscriptions = 0
    var waiting: Bool { continuation != nil }

    func appearance() async throws -> Appearance {
        var account = sampleAccount
        if generation == 1 { account.monogram = "N" }
        return Appearance(generation: generation, accounts: [account], identities: [])
    }
    func appearanceEvents(generation: UInt64) async throws -> any AppearanceEventSource {
        subscriptions += 1
        return self
    }
    func next() async throws -> AppearanceEvent {
        try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation = $0 }
        } onCancel: {
            Task { await self.cancel() }
        }
    }
    private func cancel() {
        continuation?.resume(throwing: CancellationError())
        continuation = nil
    }
    func update() {
        generation = 1
        continuation?.resume(returning: .invalidated(generation: 1))
        continuation = nil
    }
    func setAccountAppearance(accountId: String, expectedRevision: Int64, change: AccountAppearanceChange) async throws -> Receipt {
        fatalError("unused")
    }
    func setIdentityAppearance(identityId: String, expectedRevision: Int64?, change: IdentityAppearanceChange) async throws -> Receipt {
        fatalError("unused")
    }
    nonisolated func palette() -> [PaletteColor] { [] }
}
