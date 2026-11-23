// The arrows after a selection was filed out of the list (spec/08
// "Keyboard filing": "Afterwards"), on the real AppKit list.

#if canImport(Sweetbriar)
@testable import Sweetbriar
#endif
#if canImport(AppKit)
import AppKit
import Carbon.HIToolbox
import SweetbriarBridge
import Testing

private actor FiledRowsSource: ListWindowSource, WindowEventSource {
    private var ids: [Int64]
    private var generation: UInt64 = 1

    init(ids: [Int64]) { self.ids = ids }

    func file(_ filed: Set<Int64>) {
        ids.removeAll { filed.contains($0) }
        generation += 1
    }

    private func page() -> ListWindow {
        let rows = ids.enumerated().map { rank, id in
            ListRow(
                occurrenceId: id, messageId: id, accountId: "account-1", mailboxId: 1,
                sortDate: -Int64(rank), sender: "Sender", subject: "Subject", snippet: "Snippet",
                unread: false, flagged: false, hasAttachments: false, tags: [])
        }
        return ListWindow(
            windowId: 1, generation: generation, rows: rows, before: nil, after: nil,
            hasMoreAfter: false, hasMoreBefore: false, total: .exact(n: UInt64(ids.count)), search: nil)
    }

    func openWindow(
        spec: WindowSpec, anchor: SweetbriarBridge.Anchor, limit: UInt32
    ) async throws -> ListWindow { page() }

    func readWindow(
        id: UInt64, generation: UInt64, anchor: SweetbriarBridge.Anchor, limit: UInt32
    ) async throws -> ListWindow {
        if case .around(let wanted) = anchor, !ids.contains(wanted) {
            throw CoreFailure.notFound(what: "occurrence")
        }
        return page()
    }

    func windowIDs(id: UInt64) async throws -> WindowIDs {
        WindowIDs(generation: generation, ids: ids)
    }

    func closeWindow(id: UInt64) async {}

    func windowEvents(id: UInt64) async throws -> any WindowEventSource { self }

    func next() async throws -> WindowEvent {
        try await Task.sleep(for: .seconds(3600))
        throw CancellationError()
    }
}

private struct ImmediateClock: FrameClock {
    func nextFrame() async { try? await Task.sleep(for: .milliseconds(1)) }
}

@MainActor
@Suite("list selection after filing", .serialized)
struct ListSelectionAfterFilingTests {
    @Test(arguments: [
        ([1, 2], kVK_DownArrow, Int64(20)),
        ([1, 2], kVK_UpArrow, Int64(50)),
        ([4], kVK_DownArrow, Int64(20)),
        ([0], kVK_UpArrow, Int64(40)),
    ])
    func theArrowsContinueFromTheFirstFiledRowsRank(
        rows: [Int], key: Int, expected: Int64
    ) async throws {
        try await withList { source, model, list, table in
            table.selectRowIndexes(IndexSet(rows), byExtendingSelection: false)
            await source.file(list.selectedIDs)
            try await model.rebuild()
            #expect(list.selectedIDs.isEmpty)

            table.keyDown(with: try arrow(key))

            #expect(list.selectedIDs == [expected])
        }
    }

    /// Clearing the selection by hand keeps AppKit's own arrows.
    @Test func aSelectionClearedByHandIsntRemembered() async throws {
        try await withList { _, _, list, table in
            table.selectRowIndexes([3], byExtendingSelection: false)
            table.deselectAll(nil)

            table.keyDown(with: try arrow(kVK_DownArrow))

            #expect(list.selectedIDs == [50])
        }
    }

    /// Any app can read the drag pasteboard, so it says only that rows
    /// are dragged, never which.
    @Test func theDragPasteboardDoesntNameTheRows() async throws {
        try await withList { _, _, list, table in
            list.contextActions = { _ in [] }
            let dragged = [0, 1].compactMap { row in
                (list.tableView(table, pasteboardWriterForRow: row) as? NSPasteboardItem)?
                    .string(forType: MessageListDrag.pasteboardType)
            }

            #expect(dragged.count == 2)
            #expect(Set(dragged).count == 1)
        }
    }

    private func withList(
        _ body: @MainActor (
            FiledRowsSource, ListWindowModel, AppKitPrototypeList, NSTableView
        ) async throws -> Void
    ) async throws {
        let source = FiledRowsSource(ids: [50, 40, 30, 20, 10])
        let model = ListWindowModel(source: source, spec: .mailbox(1), clock: ImmediateClock())
        let list = AppKitPrototypeList(model: model, formatter: MessageRowFormatter())
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 400, height: 400),
            styleMask: [.titled], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        defer { window.close() }
        window.contentView = list
        try await model.open()
        let table = try #require((list.listView as? NSScrollView)?.documentView as? NSTableView)
        table.reloadData()
        window.makeFirstResponder(table)
        try await body(source, model, list, table)
        await model.close()
    }

    private func arrow(_ key: Int) throws -> NSEvent {
        let function = key == kVK_DownArrow ? NSDownArrowFunctionKey : NSUpArrowFunctionKey
        let text = String(UnicodeScalar(UInt16(function))!)
        return try #require(NSEvent.keyEvent(
            with: .keyDown, location: .zero, modifierFlags: [.numericPad, .function], timestamp: 0,
            windowNumber: 0, context: nil, characters: text, charactersIgnoringModifiers: text,
            isARepeat: false, keyCode: UInt16(key)))
    }
}
#endif
