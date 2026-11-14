import AppKit
import Testing

@MainActor
private final class ProbeRecipientToken: NSObject {
    let raw: String
    init(_ raw: String) { self.raw = raw }
}

@MainActor
private final class ProbeRecipientDelegate: NSObject, NSTokenFieldDelegate {
    func tokenField(_ tokenField: NSTokenField, representedObjectForEditing editingString: String) -> Any? {
        ProbeRecipientToken(editingString)
    }
    func tokenField(_ tokenField: NSTokenField, shouldAdd tokens: [Any], at index: Int) -> [Any] { tokens }
    func tokenField(_ tokenField: NSTokenField, displayStringForRepresentedObject representedObject: Any)
        -> String?
    {
        (representedObject as? ProbeRecipientToken)?.raw
    }
    func tokenField(_ tokenField: NSTokenField, editingStringForRepresentedObject representedObject: Any)
        -> String?
    {
        (representedObject as? ProbeRecipientToken)?.raw
    }
}

@Suite(.serialized) @MainActor
struct RecipientTokenNativeTests {
    @Test func unchangedNativeTokenFieldUndoesItsWholeEditingSession() throws {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 600, height: 120),
            styleMask: [.titled, .closable], backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        let field = NSTokenField(frame: NSRect(x: 20, y: 30, width: 560, height: 35))
        let delegate = ProbeRecipientDelegate()
        field.delegate = delegate
        field.tokenizingCharacterSet = CharacterSet()
        field.objectValue = [ProbeRecipientToken]()
        window.contentView?.addSubview(field)
        defer {
            window.close()
            field.delegate = nil
        }
        window.makeKeyAndOrderFront(nil)
        field.selectText(nil)
        let editor = try #require(field.currentEditor() as? NSTextView)
        editor.insertText("first@example.test", replacementRange: NSRange(location: 0, length: 0))
        editor.doCommand(by: #selector(NSResponder.insertNewline(_:)))
        #expect((field.objectValue as? [ProbeRecipientToken])?.map(\.raw) == ["first@example.test"])
        editor.setSelectedRange(NSRange(location: 0, length: editor.string.utf16.count))
        editor.insertText("second@example.test", replacementRange: editor.selectedRange())
        editor.doCommand(by: #selector(NSResponder.insertNewline(_:)))
        let undo = try #require(editor.undoManager)
        #expect(undo.canUndo)
        if undo.canUndo { undo.undo() }
        #expect((field.objectValue as? [ProbeRecipientToken])?.isEmpty == true)
        #expect(undo.canRedo)
        if undo.canRedo { undo.redo() }
        #expect((field.objectValue as? [ProbeRecipientToken])?.map(\.raw) == ["second@example.test"])
    }

    @Test func nativeValidationPreservesQuotedInputAndEditingFocus() throws {
        let window = NSWindow(
            contentRect: NSRect(x: 0, y: 0, width: 600, height: 120), styleMask: [.titled, .closable],
            backing: .buffered, defer: false)
        window.isReleasedWhenClosed = false
        let field = NSTokenField(frame: NSRect(x: 20, y: 30, width: 560, height: 35))
        let delegate = ProbeRecipientDelegate()
        field.delegate = delegate
        field.tokenizingCharacterSet = CharacterSet()
        field.objectValue = [ProbeRecipientToken("existing@example.test")]
        window.contentView?.addSubview(field)
        defer {
            window.close()
            field.delegate = nil
        }
        window.makeKeyAndOrderFront(nil)
        field.selectText(nil)
        let editor = try #require(field.currentEditor() as? NSTextView)
        editor.setSelectedRange(NSRange(location: editor.string.utf16.count, length: 0))
        let input = "\"Doe, Alex\" <User@EXAMPLE.test>; other@example.test"
        editor.insertText(input, replacementRange: editor.selectedRange())
        field.validateEditing()
        #expect(field.currentEditor() === editor)
        #expect(
            (field.objectValue as? [ProbeRecipientToken])?.contains { $0.raw.utf8.elementsEqual(input.utf8) }
                == true)
        field.objectValue = [
            ProbeRecipientToken("existing@example.test"), ProbeRecipientToken("new@example.test"),
        ]
        field.validateEditing()
        #expect(field.currentEditor() === editor)
        #expect(
            (field.objectValue as? [ProbeRecipientToken])?.map(\.raw) == [
                "existing@example.test", "new@example.test",
            ])
    }
}
