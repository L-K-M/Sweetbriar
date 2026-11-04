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

    @Test func appKitUsesAccountTokensAndNames() throws {
        let cell = MessageRowCellView(showsAccountMarker: true)
        cell.frame = NSRect(x: 0, y: 0, width: 300, height: 56)
        cell.configure(content: content, account: account, accountLabel: "Work · one", usesAppearance: true)
        #expect(cell.accessibilityLabel()?.contains("account Work · one") == true)
        for name: NSAppearance.Name in [.aqua, .darkAqua] {
            cell.appearance = NSAppearance(named: name)
            cell.layoutSubtreeIfNeeded()
            let bitmap = try render(cell)
            let pixel = try #require(bitmap.colorAt(x: 2, y: 28)?.usingColorSpace(.sRGB))
            #expect(name == .aqua ? pixel.redComponent > 0.8 : pixel.greenComponent > 0.8)
        }
        var updated = account
        updated.monogram = "N"
        updated.tokens.light.markerSrgb = "#0000ff"
        cell.appearance = NSAppearance(named: .aqua)
        cell.configure(content: content, account: updated, accountLabel: "New", usesAppearance: true)
        let pixel = try #require(render(cell).colorAt(x: 2, y: 28)?.usingColorSpace(.sRGB))
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
            let pixel = try #require(bitmap.colorAt(x: 2, y: 28)?.usingColorSpace(.sRGB))
            #expect(scheme == .light ? pixel.redComponent > 0.8 : pixel.greenComponent > 0.8)
        }
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
        let pixel = try #require(render(cell).colorAt(x: 2, y: 28)?.usingColorSpace(.sRGB))
        #expect(pixel.blueComponent > 0.8)
        #expect(cell.accessibilityLabel()?.contains("account West") == true)
        cell.configure(content: content, account: nil, usesAppearance: true)
        #expect(cell.accessibilityLabel()?.contains("account West") == false)
        let missing = try #require(render(cell).colorAt(x: 2, y: 28)?.usingColorSpace(.sRGB))
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
