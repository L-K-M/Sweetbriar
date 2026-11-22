// Edit → Undo takes filing back in the list only (spec/08 "Keyboard
// filing"): ⌘Z while text is edited never moves mail.

#if canImport(Sweetbriar)
@testable import Sweetbriar
#endif
#if canImport(AppKit)
import AppKit
import SweetbriarBridge
import Testing

private actor NoRowsSource: ListWindowSource {
    func openWindow(
        spec: WindowSpec, anchor: SweetbriarBridge.Anchor, limit: UInt32
    ) async throws -> ListWindow {
        throw CoreFailure.unavailable(reason: "no rows")
    }

    func readWindow(
        id: UInt64, generation: UInt64, anchor: SweetbriarBridge.Anchor, limit: UInt32
    ) async throws -> ListWindow {
        throw CoreFailure.unavailable(reason: "no rows")
    }

    func windowIDs(id: UInt64) async throws -> WindowIDs {
        throw CoreFailure.unavailable(reason: "no rows")
    }

    func closeWindow(id: UInt64) async {}

    func windowEvents(id: UInt64) async throws -> any WindowEventSource {
        throw CoreFailure.unavailable(reason: "no rows")
    }
}

private struct ImmediateClock: FrameClock {
    func nextFrame() async {}
}

@MainActor
@Suite("filing undo scope")
struct FilingUndoScopeTests {
    @Test func filingUndoIsTheListsNotTheTexts() throws {
        let model = ListWindowModel(source: NoRowsSource(), spec: .mailbox(1), clock: ImmediateClock())
        let list = AppKitPrototypeList(model: model, formatter: MessageRowFormatter())
        let filing = UndoManager()
        list.filingUndoManager = filing
        let editor = NSTextView()
        editor.allowsUndo = true
        let content = NSView()
        content.addSubview(list)
        content.addSubview(editor)
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 400, height: 400),
            styleMask: [.titled], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        defer { window.close() }
        window.contentView = content

        let table = try #require((list.listView as? NSScrollView)?.documentView as? NSTableView)

        #expect(table.undoManager === filing)
        #expect(editor.undoManager !== filing)
    }
}
#endif
