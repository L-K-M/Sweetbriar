#if canImport(Sweetbriar)
@testable import Sweetbriar
#endif
import AppKit
import SwiftUI
import SweetbriarBridge
import Testing

@Suite("List appearance")
@MainActor
struct ListAppearanceTests {
    private let account = AccountAppearance(
        accountId: "one", colorSrgb: "#ff0000",
        tokens: AppearanceTokens(
            light: AppearanceVariant(markerSrgb: "#ff0000", stripSrgb: "#ffffff", foregroundSrgb: "#000000", outlineSrgb: "#000000"),
            dark: AppearanceVariant(markerSrgb: "#00ff00", stripSrgb: "#000000", foregroundSrgb: "#ffffff", outlineSrgb: "#ffffff")
        ), symbol: nil, monogram: "W", collides: true, assignment: .custom, revision: 0
    )

    private var content: MessageRowContent {
        MessageRowFormatter().present(sender: "Sender", subject: "Subject", snippet: "Snippet",
                                      dateMilliseconds: 0, unread: false, flagged: false,
                                      hasAttachments: false, tags: [], accountID: "one")
    }

    @Test func mixedAccountWindowsShowMarkersAndMailboxesStayPlain() {
        #expect(WindowSpec.flagged.showsAccountMarker)
        #expect(WindowSpec.unified(.inbox).showsAccountMarker)
        #expect(WindowSpec.thread(1).showsAccountMarker)
        #expect(!WindowSpec.mailbox(1).showsAccountMarker)
    }

    @Test func flaggedControllerShowsBothAccountsAndSurvivesAppearanceReset() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let config = CoreConfig(dataRoot: root.appendingPathComponent("data").path,
                                cacheRoot: root.appendingPathComponent("cache").path)
        let batcher = FrameBatcher<[Setting]>(clock: ImmediateClock()) { _ in }
        let bridge = try await CoreBridge.open(config: config, snapshots: batcher)
        let sidebar = SidebarModel(source: TwoSidebarAccounts())
        await sidebar.reload()
        let source = TwoAccountAppearanceSource()
        let shared = SharedAppearance()
        let appearance = shared.model(source: source)
        let controller = MessageListViewController(bridge: bridge, sidebar: sidebar, appearance: appearance,
                                                   listSource: TwoFlaggedRows(), clock: ImmediateClock())
        let window = NSWindow(contentRect: NSRect(x: 0, y: 0, width: 600, height: 300),
                              styleMask: [.titled], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        // These pixel assertions use the light tokens, independent of the Mac's theme.
        window.appearance = NSAppearance(named: .aqua)
        window.contentViewController = controller
        defer { window.close() }
        controller.show(.flagged)
        for _ in 0..<300 where controller.model?.row(at: 1) == nil || appearance.appearance == nil {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(controller.model?.row(at: 1) != nil)
        let list = try #require(controller.list)
        let scroll = try #require(list.listView as? NSScrollView)
        let table = try #require(scroll.documentView as? NSTableView)
        table.reloadData()
        controller.view.layoutSubtreeIfNeeded()
        let first = try #require(table.view(atColumn: 0, row: 0, makeIfNecessary: true) as? MessageRowCellView)
        let second = try #require(table.view(atColumn: 0, row: 1, makeIfNecessary: true) as? MessageRowCellView)
        first.layoutSubtreeIfNeeded()
        second.layoutSubtreeIfNeeded()
        #expect(first.accessibilityLabel()?.contains("account 1 · Work · account-1") == true)
        #expect(second.accessibilityLabel()?.contains("account 2 · Work · account-2") == true)
        #expect(first.subviews.compactMap { $0 as? NSTextField }.contains { $0.stringValue.hasPrefix("1 · Work") && !$0.isHidden })
        #expect(second.subviews.compactMap { $0 as? NSTextField }.contains { $0.stringValue.hasPrefix("2 · Work") && !$0.isHidden })
        try checkRail(render(first))
        try checkRail(render(second))
        #expect(try hasMonogramInk(render(first)))
        #expect(try hasMonogramInk(render(second)))
        table.selectRowIndexes(IndexSet(integer: 0), byExtendingSelection: false)
        for _ in 0..<200 where !(await source.waiting) {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(await source.waiting)
        await source.removeSecondAndReset()
        for _ in 0..<200 where appearance.appearance?.generation != 1 {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(appearance.appearance?.generation == 1)
        for _ in 0..<200 {
            if let cell = table.view(atColumn: 0, row: 1, makeIfNecessary: true) as? MessageRowCellView,
               cell.accessibilityLabel()?.contains("account ") == false { break }
            try await Task.sleep(for: .milliseconds(10))
        }
        let missing = try #require(table.view(atColumn: 0, row: 1, makeIfNecessary: true) as? MessageRowCellView)
        #expect(missing.accessibilityLabel()?.contains("account ") == false)
        #expect(missing.subviews[0].isHidden && missing.subviews[1].isHidden)
        #expect(table.selectedRowIndexes == IndexSet(integer: 0))
        await controller.close()
        await bridge.shutdown()
    }

    @Test func swiftUIFlaggedRowsShowMixedAccountCues() throws {
        var second = sampleAccount
        second.accountId = "account-2"
        func renderRow(_ account: AccountAppearance, label: String, spec: WindowSpec) throws -> NSBitmapImageRep {
            let host = NSHostingView(rootView: RenderedMessageRow(content: content, account: account,
                accountLabel: label, usesAppearance: true, showsAccountMarker: spec.showsAccountMarker)
                .environment(\.colorScheme, .light))
            host.frame = NSRect(x: 0, y: 0, width: 300, height: 56)
            host.layoutSubtreeIfNeeded()
            return try render(host)
        }
        let first = try renderRow(sampleAccount, label: "1 · Work · account-1", spec: .flagged)
        let other = try renderRow(second, label: "2 · Work · account-2", spec: .flagged)
        try checkRail(first)
        try checkRail(other)
        #expect(try hasMonogramInk(first))
        #expect(try hasMonogramInk(other))
        #expect(first.tiffRepresentation != other.tiffRepresentation)
        let single = try renderRow(sampleAccount, label: "Work", spec: .mailbox(1))
        let pixel = try #require(single.colorAt(x: 4, y: 28)?.usingColorSpace(.sRGB))
        #expect(pixel.alphaComponent < 0.1)
    }

    private func hasMonogramInk(_ bitmap: NSBitmapImageRep) throws -> Bool {
        let marker = appearanceColor(sampleAccount.tokens.light.markerSrgb)
        for y in 15..<41 {
            for x in 18..<44 {
                guard let pixel = bitmap.colorAt(x: x, y: y)?.usingColorSpace(.sRGB) else { continue }
                if abs(pixel.redComponent - marker.redComponent) < 0.15,
                   abs(pixel.greenComponent - marker.greenComponent) < 0.15,
                   abs(pixel.blueComponent - marker.blueComponent) < 0.15 { return true }
            }
        }
        return false
    }

    @Test func appKitUsesAccountTokensAndNames() throws {
        let cell = MessageRowCellView(showsAccountMarker: true)
        cell.frame = NSRect(x: 0, y: 0, width: 300, height: 56)
        cell.configure(content: content, account: account, accountLabel: "Work · one", usesAppearance: true)
        #expect(cell.accessibilityLabel()?.contains("account Work · one") == true)
        for name: NSAppearance.Name in [.aqua, .darkAqua] {
            cell.appearance = NSAppearance(named: name)
            cell.layoutSubtreeIfNeeded()
            let bitmap = try render(cell)
            let pixel = try #require(bitmap.colorAt(x: 4, y: 28)?.usingColorSpace(.sRGB))
            #expect(name == .aqua ? pixel.redComponent > 0.8 : pixel.greenComponent > 0.8)
        }
        var updated = account
        updated.monogram = "N"
        updated.tokens.light.markerSrgb = "#0000ff"
        cell.appearance = NSAppearance(named: .aqua)
        cell.configure(content: content, account: updated, accountLabel: "New", usesAppearance: true)
        let pixel = try #require(render(cell).colorAt(x: 4, y: 28)?.usingColorSpace(.sRGB))
        #expect(pixel.blueComponent > 0.8)
        #expect(cell.accessibilityLabel()?.contains("account New") == true)
    }

    @Test func swiftUIUsesLightAndDarkTokens() throws {
        for scheme: ColorScheme in [.light, .dark] {
            let view = NSHostingView(rootView: RenderedMessageRow(content: content, account: account,
                accountLabel: "Work", usesAppearance: true).environment(\.colorScheme, scheme))
            view.frame = NSRect(x: 0, y: 0, width: 300, height: 56)
            view.layoutSubtreeIfNeeded()
            let bitmap = try render(view)
            let pixel = try #require(bitmap.colorAt(x: 4, y: 28)?.usingColorSpace(.sRGB))
            #expect(scheme == .light ? pixel.redComponent > 0.8 : pixel.greenComponent > 0.8)
        }
    }

    @Test func coreTokensContrastOnSelectedSurfaces() async throws {
        let root = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString)
        defer { try? FileManager.default.removeItem(at: root) }
        let config = CoreConfig(dataRoot: root.appendingPathComponent("data").path,
                                cacheRoot: root.appendingPathComponent("cache").path)
        let batcher = FrameBatcher<[Setting]>(clock: ImmediateClock()) { _ in }
        let bridge = try await CoreBridge.open(config: config, snapshots: batcher)
        do {
            var current = try #require(try await bridge.appearance().accounts.first)
            let colors: [AccountColor] = (0..<7).map { .paletteV1(slot: UInt8($0)) }
                + [.custom(colorSrgb: "#000000"), .custom(colorSrgb: "#ffffff")]
            for color in colors {
                _ = try await bridge.setAccountAppearance(
                    accountId: current.accountId, expectedRevision: current.revision,
                    change: AccountAppearanceChange(color: color, symbol: nil))
                current = try #require(try await bridge.appearance().accounts.first)
                for (name, scheme): (NSAppearance.Name, ColorScheme) in [(.aqua, .light), (.darkAqua, .dark)] {
                    for strong in [false, true] {
                        let base = SurfaceView(background: .selectedContentBackgroundColor)
                        base.frame = NSRect(x: 0, y: 0, width: 300, height: 56)
                        base.appearance = NSAppearance(named: name)
                        let cell = MessageRowCellView(showsAccountMarker: true)
                        cell.contrastOptions = { (strong, strong) }
                        cell.frame = base.bounds
                        cell.configure(content: content, account: current, accountLabel: "Work", usesAppearance: true)
                        base.addSubview(cell)
                        base.layoutSubtreeIfNeeded()
                        try checkRail(render(base))
                        let swift = NSHostingView(rootView: RenderedMessageRow(content: content, account: current,
                            accountLabel: "Work", usesAppearance: true, contrastOverride: strong)
                            .environment(\.colorScheme, scheme)
                            .background(Color(nsColor: .selectedContentBackgroundColor)))
                        swift.frame = base.bounds
                        swift.appearance = NSAppearance(named: name)
                        swift.layoutSubtreeIfNeeded()
                        try checkRail(render(swift))
                    }
                }
            }
        } catch {
            await bridge.shutdown()
            throw error
        }
        await bridge.shutdown()
    }

    private func checkRail(_ bitmap: NSBitmapImageRep) throws {
        let edge = try #require(bitmap.colorAt(x: 0, y: 28)?.usingColorSpace(.sRGB))
        let strip = try #require(bitmap.colorAt(x: 1, y: 28)?.usingColorSpace(.sRGB))
        let marker = try #require(bitmap.colorAt(x: 4, y: 28)?.usingColorSpace(.sRGB))
        #expect(contrast(marker, strip) >= 3)
        #expect(contrast(edge, strip) >= 3)
    }

    private func contrast(_ left: NSColor, _ right: NSColor) -> CGFloat {
        func luminance(_ color: NSColor) -> CGFloat {
            func linear(_ value: CGFloat) -> CGFloat {
                value <= 0.04045 ? value / 12.92 : pow((value + 0.055) / 1.055, 2.4)
            }
            return 0.2126 * linear(color.redComponent) + 0.7152 * linear(color.greenComponent)
                + 0.0722 * linear(color.blueComponent)
        }
        let a = luminance(left)
        let b = luminance(right)
        return (max(a, b) + 0.05) / (min(a, b) + 0.05)
    }

    @Test func collisionLabelsAndStrongMonogramsDrawInBothBackends() throws {
        let sections = [SidebarModel.Section(id: "a", title: "Work", entries: []),
                        SidebarModel.Section(id: "b", title: "Work", entries: [])]
        #expect(MessageListViewController.accountLabel("a", sections: sections).hasPrefix("1 · "))
        #expect(MessageListViewController.accountLabel("b", sections: sections).hasPrefix("2 · "))
        let cell = MessageRowCellView(showsAccountMarker: true)
        cell.frame = NSRect(x: 0, y: 0, width: 300, height: 56)
        cell.contrastOptions = { (false, false) }
        cell.configure(content: content, account: account, accountLabel: "Work one", usesAppearance: true)
        cell.layoutSubtreeIfNeeded()
        let one = try #require(render(cell).tiffRepresentation)
        #expect(cell.accessibilityLabel()?.contains("Work one") == true)
        cell.configure(content: content, account: account, accountLabel: "Work two", usesAppearance: true)
        let two = try #require(render(cell).tiffRepresentation)
        #expect(one != two)
        #expect(cell.accessibilityLabel()?.contains("Work two") == true)
        cell.contrastOptions = { (false, true) }
        cell.configure(content: content, account: account, accountLabel: "Work two", usesAppearance: true)
        #expect(try render(cell).tiffRepresentation != two)

        func swift(_ label: String, strong: Bool) throws -> Data {
            let view = NSHostingView(rootView: RenderedMessageRow(content: content, account: account,
                accountLabel: label, usesAppearance: true, contrastOverride: strong))
            view.frame = cell.bounds
            view.layoutSubtreeIfNeeded()
            return try #require(render(view).tiffRepresentation)
        }
        let first = try swift("Work one", strong: false)
        let second = try swift("Work two", strong: false)
        #expect(first != second)
        #expect(try swift("Work two", strong: true) != second)
    }

    @Test func accountMarkersDoNotPaintTheRowSurface() throws {
        for appearance: NSAppearance.Name in [.aqua, .darkAqua] {
            for background in [NSColor.textBackgroundColor, NSColor.selectedContentBackgroundColor] {
                let base = SurfaceView(background: background)
                base.frame = NSRect(x: 0, y: 0, width: 300, height: 56)
                base.appearance = NSAppearance(named: appearance)
                let cell = MessageRowCellView(showsAccountMarker: true)
                cell.frame = base.bounds
                cell.configure(content: content, account: account, accountLabel: "Work", usesAppearance: true)
                base.addSubview(cell)
                base.layoutSubtreeIfNeeded()
                let bitmap = try render(base)
                var resolved: NSColor?
                base.effectiveAppearance.performAsCurrentDrawingAppearance {
                    resolved = background.usingColorSpace(.sRGB)
                }
                let expected = try #require(resolved)
                let actual = try #require(bitmap.colorAt(x: 295, y: 3)?.usingColorSpace(.sRGB))
                #expect(abs(actual.redComponent - expected.redComponent) < 0.03)
                #expect(abs(actual.greenComponent - expected.greenComponent) < 0.03)
                #expect(abs(actual.blueComponent - expected.blueComponent) < 0.03)
                let swift = NSHostingView(rootView: RenderedMessageRow(content: content, account: account,
                    accountLabel: "Work", usesAppearance: true)
                    .background(Color(nsColor: background)))
                swift.frame = base.bounds
                swift.appearance = NSAppearance(named: appearance)
                swift.layoutSubtreeIfNeeded()
                let swiftPixel = try #require(render(swift).colorAt(x: 295, y: 3)?.usingColorSpace(.sRGB))
                #expect(abs(swiftPixel.redComponent - expected.redComponent) < 0.03)
                #expect(abs(swiftPixel.greenComponent - expected.greenComponent) < 0.03)
                #expect(abs(swiftPixel.blueComponent - expected.blueComponent) < 0.03)
            }
        }
    }

    @Test func reusedCellUsesTheNewAccountAndHidesMissingAppearances() throws {
        let cell = MessageRowCellView(showsAccountMarker: true)
        cell.frame = NSRect(x: 0, y: 0, width: 300, height: 56)
        cell.appearance = NSAppearance(named: .aqua)
        cell.configure(content: content, account: account, accountLabel: "Work", usesAppearance: true)
        var second = account
        second.accountId = "two"
        second.tokens.light.markerSrgb = "#0000ff"
        cell.configure(content: content, account: second, accountLabel: "West", usesAppearance: true)
        cell.layoutSubtreeIfNeeded()
        let pixel = try #require(render(cell).colorAt(x: 4, y: 28)?.usingColorSpace(.sRGB))
        #expect(pixel.blueComponent > 0.8)
        #expect(cell.accessibilityLabel()?.contains("account West") == true)
        cell.configure(content: content, account: nil, usesAppearance: true)
        #expect(cell.accessibilityLabel()?.contains("account West") == false)
        let missing = try #require(render(cell).colorAt(x: 4, y: 28)?.usingColorSpace(.sRGB))
        #expect(missing.alphaComponent < 0.1)
    }

    @Test func appKitListFollowsCommittedAppearanceUpdates() async throws {
        let source = SidebarAppearanceSource()
        let shared = SharedAppearance()
        let appearance = shared.model(source: source)
        let model = ListWindowModel(source: OneRowSource(), spec: .debugSynthetic, clock: ImmediateClock())
        let list = AppKitPrototypeList(model: model, formatter: MessageRowFormatter(),
                                       appearance: appearance, accountLabel: { _ in "Work" })
        list.frame = NSRect(x: 0, y: 0, width: 300, height: 100)
        let window = NSWindow(contentRect: list.frame, styleMask: [.titled], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = list
        defer { window.close() }
        try await model.open()
        for _ in 0..<200 where model.row(at: 0) == nil || !model.hasExactIndex || appearance.appearance == nil {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(model.row(at: 0) != nil)
        #expect(appearance.appearance?.generation == 0)
        let scroll = try #require(list.listView as? NSScrollView)
        let table = try #require(scroll.documentView as? NSTableView)
        table.reloadData()
        table.selectRowIndexes(IndexSet(integer: 0), byExtendingSelection: false)
        let rowView = try #require(table.rowView(atRow: 0, makeIfNecessary: true))
        #expect(rowView.isSelected)
        let cell = try #require(table.view(atColumn: 0, row: 0, makeIfNecessary: true) as? MessageRowCellView)
        table.layoutSubtreeIfNeeded()
        try checkRail(render(cell))
        #expect(cell.accessibilityLabel()?.contains("account Work") == true)
        cell.layoutSubtreeIfNeeded()
        let original = try #require(render(cell).tiffRepresentation)
        for _ in 0..<200 where !(await source.waiting) {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(await source.waiting)
        await source.update()
        for _ in 0..<200 where appearance.appearance?.generation != 1 {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(appearance.appearance?.generation == 1)
        var changed = false
        for _ in 0..<200 {
            let current = try #require(table.view(atColumn: 0, row: 0, makeIfNecessary: true) as? MessageRowCellView)
            current.layoutSubtreeIfNeeded()
            if try render(current).tiffRepresentation != original {
                changed = true
                break
            }
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(changed)
        let selectedRow = try #require(table.rowView(atRow: 0, makeIfNecessary: true))
        #expect(selectedRow.isSelected)
        await model.close()
    }

    @Test func swiftUIListFollowsCommittedAppearanceUpdates() async throws {
        let source = SidebarAppearanceSource()
        let shared = SharedAppearance()
        let appearance = shared.model(source: source)
        let model = ListWindowModel(source: OneRowSource(), spec: .debugSynthetic, clock: ImmediateClock())
        let list = SwiftUIPrototypeList(model: model, formatter: MessageRowFormatter(),
                                        appearance: appearance, accountLabel: { _ in "Work" })
        list.frame = NSRect(x: 0, y: 0, width: 300, height: 100)
        let window = NSWindow(contentRect: list.frame, styleMask: [.titled], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        window.contentView = list
        defer { window.close() }
        try await model.open()
        for _ in 0..<200 where model.row(at: 0) == nil || appearance.appearance == nil {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(model.row(at: 0) != nil)
        #expect(appearance.appearance?.generation == 0)
        list.layoutSubtreeIfNeeded()
        let original = try #require(render(list.listView).tiffRepresentation)
        for _ in 0..<200 where !(await source.waiting) {
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(await source.waiting)
        await source.update()
        var changed = false
        for _ in 0..<200 {
            if appearance.appearance?.generation == 1,
               try render(list.listView).tiffRepresentation != original {
                changed = true
                break
            }
            try await Task.sleep(for: .milliseconds(10))
        }
        #expect(changed)
        await model.close()
    }

    private func render(_ view: NSView) throws -> NSBitmapImageRep {
        let bitmap = try #require(view.bitmapImageRepForCachingDisplay(in: view.bounds))
        view.cacheDisplay(in: view.bounds, to: bitmap)
        return bitmap
    }
}

private struct ImmediateClock: FrameClock {
    func nextFrame() async { try? await Task.sleep(for: .milliseconds(1)) }
}

private struct TwoSidebarAccounts: SidebarSource {
    func accounts() async throws -> [Account] {
        ["account-1", "account-2"].enumerated().map { index, id in
            Account(accountId: id, kind: .local, displayName: "Work", state: .active,
                    stateReason: nil, sortOrder: Int64(index))
        }
    }
    func mailboxes(accountId: String) async throws -> Mailboxes { Mailboxes(generation: 0, mailboxes: []) }
}

private actor TwoAccountAppearanceSource: AppearanceSource, AppearanceEventSource {
    private var generation: UInt64 = 0
    private var continuation: CheckedContinuation<AppearanceEvent, any Error>?
    var waiting: Bool { continuation != nil }

    func appearance() async throws -> Appearance {
        var second = sampleAccount
        second.accountId = "account-2"
        return Appearance(generation: generation, accounts: generation == 0 ? [sampleAccount, second] : [sampleAccount], identities: [])
    }
    func appearanceEvents(generation: UInt64) async throws -> any AppearanceEventSource { self }
    func next() async throws -> AppearanceEvent {
        try await withTaskCancellationHandler {
            try await withCheckedThrowingContinuation { continuation = $0 }
        } onCancel: { Task { await self.cancel() } }
    }
    private func cancel() {
        continuation?.resume(throwing: CancellationError())
        continuation = nil
    }
    func removeSecondAndReset() {
        generation = 1
        continuation?.resume(returning: .resetRequired)
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

private actor TwoFlaggedRows: ListWindowSource, WindowEventSource {
    private var rows: [ListRow] {
        ["account-1", "account-2"].enumerated().map { index, id in
            ListRow(occurrenceId: Int64(index + 1), messageId: Int64(index + 1), accountId: id,
                    mailboxId: 1, sortDate: 0, sender: "Sender", subject: "Subject", snippet: "Snippet",
                    unread: false, flagged: true, hasAttachments: false, tags: [])
        }
    }
    func openWindow(spec: WindowSpec, anchor: SweetbriarBridge.Anchor, limit: UInt32) async throws -> ListWindow {
        guard spec == .flagged else { throw CancellationError() }
        return ListWindow(windowId: 1, generation: 0, rows: rows, before: nil, after: nil,
                          hasMoreAfter: false, hasMoreBefore: false, total: .exact(n: 2), search: nil)
    }
    func readWindow(id: UInt64, generation: UInt64, anchor: SweetbriarBridge.Anchor, limit: UInt32) async throws -> ListWindow {
        try await openWindow(spec: .flagged, anchor: anchor, limit: limit)
    }
    func windowIDs(id: UInt64) async throws -> WindowIDs { WindowIDs(generation: 0, ids: [1, 2]) }
    func closeWindow(id: UInt64) async {}
    func windowEvents(id: UInt64) async throws -> any WindowEventSource { self }
    func next() async throws -> WindowEvent {
        try await Task.sleep(for: .seconds(3600))
        throw CancellationError()
    }
}

private actor OneRowSource: ListWindowSource, WindowEventSource {
    private var row: ListRow {
        ListRow(occurrenceId: 1, messageId: 1, accountId: "account-1", mailboxId: 1,
                sortDate: 0, sender: "Sender", subject: "Subject", snippet: "Snippet",
                unread: false, flagged: false, hasAttachments: false, tags: [])
    }

    func openWindow(spec: WindowSpec, anchor: SweetbriarBridge.Anchor, limit: UInt32) async throws -> ListWindow {
        ListWindow(windowId: 1, generation: 0, rows: [row], before: nil, after: nil,
                   hasMoreAfter: false, hasMoreBefore: false, total: .exact(n: 1), search: nil)
    }
    func readWindow(id: UInt64, generation: UInt64, anchor: SweetbriarBridge.Anchor, limit: UInt32) async throws -> ListWindow {
        try await openWindow(spec: .debugSynthetic, anchor: anchor, limit: limit)
    }
    func windowIDs(id: UInt64) async throws -> WindowIDs { WindowIDs(generation: 0, ids: [1]) }
    func closeWindow(id: UInt64) async {}
    func windowEvents(id: UInt64) async throws -> any WindowEventSource { self }
    func next() async throws -> WindowEvent {
        try await Task.sleep(for: .seconds(3600))
        throw CancellationError()
    }
}

private final class SurfaceView: NSView {
    let background: NSColor

    init(background: NSColor) {
        self.background = background
        super.init(frame: .zero)
    }

    required init?(coder: NSCoder) { fatalError("init(coder:) is unsupported") }

    override func draw(_ dirtyRect: NSRect) {
        background.setFill()
        bounds.fill()
    }
}
