// Exercise the shipped menu, not direct editor calls: text expanders
// need Paste's shortcut, and formatting must be undoable with the keyboard.

#if canImport(Sweetbriar)
@testable import Sweetbriar
#endif
#if canImport(AppKit)
import AppKit
import Carbon.HIToolbox
import Testing

@Suite("editing keyboard shortcuts", .serialized)
@MainActor
struct MainMenuTests {
    private let controller = ComposerProbeWindowController()

    @Test("⌘B, ⌘Z and ⇧⌘Z format, undo and redo through the main menu")
    func formattingUndoRedo() throws {
        try withFocusedEditor { textView in
            textView.mode = .markdown
            textView.string = "say word now"
            textView.setSelectedRange(NSRange(location: 4, length: 4))

            #expect(try press("b", keyCode: kVK_ANSI_B))
            try #require(textView.string == "say **word** now")

            #expect(try press("z", keyCode: kVK_ANSI_Z))
            #expect(textView.string == "say word now")
            #expect(textView.undoManager?.canRedo == true)

            #expect(try press(
                "Z", keyCode: kVK_ANSI_Z, modifiers: [.command, .shift]
            ))
            #expect(textView.string == "say **word** now")
            #expect(textView.isOnTextKit2)
        }
    }

    @Test("⌘V replaces a text-expander trigger with an emoji", arguments: [
        ComposerMode.plain, .markdown,
    ])
    func pasteEmoji(mode: ComposerMode) throws {
        let pasteboard = NSPasteboard.general
        // Copy eagerly: clearing the board invalidates its original items.
        let previousItems = (pasteboard.pasteboardItems ?? []).map { original in
            let copy = NSPasteboardItem()
            for type in original.types {
                if let data = original.data(forType: type) {
                    copy.setData(data, forType: type)
                }
            }
            return copy
        }
        defer {
            pasteboard.clearContents()
            pasteboard.writeObjects(previousItems)
        }
        pasteboard.clearContents()
        try #require(pasteboard.setString("👩🏽‍💻", forType: .string))

        try withFocusedEditor { (textView: ComposerTextView) throws in
            textView.mode = mode
            textView.string = "hello :rocket: there"
            textView.setSelectedRange(NSRange(location: 6, length: 8))

            #expect(try press("v", keyCode: kVK_ANSI_V))
            #expect(textView.string == "hello 👩🏽‍💻 there")

            #expect(try press("z", keyCode: kVK_ANSI_Z))
            #expect(textView.string == "hello :rocket: there")
            #expect(textView.isOnTextKit2)
        }
    }

    private func withFocusedEditor(
        _ body: (ComposerTextView) throws -> Void
    ) throws {
        let previousWindow = NSApplication.shared.keyWindow
        let window = try #require(controller.window)
        defer {
            controller.close()
            previousWindow?.makeKey()
        }
        window.makeKeyAndOrderFront(nil)
        try #require(window.makeFirstResponder(controller.textView))
        try #require(NSApplication.shared.keyWindow === window)

        try body(controller.textView)
    }

    private func press(
        _ character: String,
        keyCode: Int,
        modifiers: NSEvent.ModifierFlags = [.command]
    ) throws -> Bool {
        let menu = try #require(NSApplication.shared.mainMenu)
        let window = try #require(controller.window)
        let event = try #require(NSEvent.keyEvent(
            with: .keyDown,
            location: .zero,
            modifierFlags: modifiers,
            timestamp: ProcessInfo.processInfo.systemUptime,
            windowNumber: window.windowNumber,
            context: nil,
            characters: character,
            charactersIgnoringModifiers: character,
            isARepeat: false,
            keyCode: UInt16(keyCode)
        ))
        menu.update()
        return menu.performKeyEquivalent(with: event)
    }
}
#endif

#if canImport(AppKit)
/// What the menu's callbacks saw: a reference the sendable callbacks
/// share, since they can't capture a mutable local.
@MainActor
private final class MenuProbe {
    var enabled = true
    var syntheticRuns = 0
    var storeRuns = 0
}

@Suite("benchmark menu availability", .serialized)
@MainActor
struct BenchmarkMenuTests {
    @Test("both benchmark commands share the in-flight gate and dispatch their own action")
    func benchmarkCommands() throws {
        let probe = MenuProbe()
        let builder = MainMenuBuilder(
            onFillSynthetic: {}, onShowPrototype: {}, onImplementationChanged: { _ in },
            onRunBenchmark: { probe.syntheticRuns += 1 },
            onRunStoreBenchmark: { probe.storeRuns += 1 },
            isBenchmarkEnabled: { probe.enabled }, onShowComposerProbe: {}, onImport: {}
        )
        let menu = builder.makeMainMenu()
        let debug = try #require(menu.items.compactMap(\.submenu).first { $0.title == "Debug" })
        let synthetic = try #require(debug.items.first { $0.title == "Run List Benchmark" })
        let store = try #require(debug.items.first { $0.title == "Run Store Benchmark" })
        #expect(builder.validateMenuItem(synthetic))
        #expect(builder.validateMenuItem(store))
        probe.enabled = false
        #expect(!builder.validateMenuItem(synthetic))
        #expect(!builder.validateMenuItem(store))
        probe.enabled = true
        #expect(NSApp.sendAction(try #require(synthetic.action), to: builder, from: synthetic))
        #expect(NSApp.sendAction(try #require(store.action), to: builder, from: store))
        #expect(probe.syntheticRuns == 1)
        #expect(probe.storeRuns == 1)
    }
}
#endif
