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
    func formattingUndoRedo() async throws {
        try await withFocusedEditor { textView in
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
    func pasteEmoji(mode: ComposerMode) async throws {
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

        try await withFocusedEditor { (textView: ComposerTextView) throws in
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

    /// A disabled menu item still consumes its key, so the menu bar
    /// itself must pass ⌘⌫ on to text being edited (spec/08 "Keyboard
    /// filing"): it never files mail.
    @Test("⌘⌫ stays the text's while it is edited")
    func commandDeleteStaysTheTexts() async throws {
        try await withFocusedEditor { textView in
            textView.string = "say word"

            let handled = try press("\u{8}", keyCode: kVK_Delete)
            #expect(!handled)
            #expect(textView.string == "say word")
        }
    }

    private func withFocusedEditor(
        _ body: (ComposerTextView) throws -> Void
    ) async throws {
        let previousWindow = NSApplication.shared.keyWindow
        let window = try #require(controller.window)
        defer {
            controller.close()
            previousWindow?.makeKey()
        }
        // An app-hosted runner may start behind another application. Keyboard
        // routing needs an active app, not merely an ordered-front window.
        NSApplication.shared.activate()
        let deadline = ContinuousClock.now + .seconds(2)
        while !NSApplication.shared.isActive, ContinuousClock.now < deadline {
            // Yield to AppKit's real event pump; a nested Foundation run loop
            // alone does not dispatch the application's activation event.
            try await Task.sleep(for: .milliseconds(10))
        }
        try #require(NSApplication.shared.isActive)
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
    var searchRuns = 0
}

@Suite("benchmark menu availability", .serialized)
@MainActor
struct BenchmarkMenuTests {
    @Test("the benchmark commands share the in-flight gate and dispatch their own action")
    func benchmarkCommands() throws {
        let probe = MenuProbe()
        let builder = MainMenuBuilder(
            onFillSynthetic: {}, onShowPrototype: {}, onImplementationChanged: { _ in },
            onRunBenchmark: { probe.syntheticRuns += 1 },
            onRunStoreBenchmark: { probe.storeRuns += 1 },
            onRunSearchBenchmark: { probe.searchRuns += 1 },
            isBenchmarkEnabled: { probe.enabled }, onShowComposerProbe: {}, onImport: {}
        )
        let menu = builder.makeMainMenu()
        let debug = try #require(menu.items.compactMap(\.submenu).first { $0.title == "Debug" })
        let synthetic = try #require(debug.items.first { $0.title == "Run List Benchmark" })
        let store = try #require(debug.items.first { $0.title == "Run Store Benchmark" })
        let search = try #require(debug.items.first { $0.title == "Run Search Benchmark" })
        for item in [synthetic, store, search] {
            #expect(builder.validateMenuItem(item))
        }
        probe.enabled = false
        for item in [synthetic, store, search] {
            #expect(!builder.validateMenuItem(item))
        }
        probe.enabled = true
        for item in [synthetic, store, search] {
            #expect(NSApp.sendAction(try #require(item.action), to: builder, from: item))
        }
        #expect(probe.syntheticRuns == 1)
        #expect(probe.storeRuns == 1)
        #expect(probe.searchRuns == 1)
    }
}

@MainActor
private final class MessageMenuProbe {
    var enabled = false
    var replyRuns = 0
    var replyAllRuns = 0
    var forwardRuns = 0
}

@Suite("message commands", .serialized)
@MainActor
struct MessageMenuTests {
    @Test("Reply, Reply All and Forward carry their identifiers, shortcuts and selection gate")
    func messageCommands() throws {
        let probe = MessageMenuProbe()
        let builder = MainMenuBuilder(
            onFillSynthetic: {}, onShowPrototype: {}, onImplementationChanged: { _ in },
            onRunBenchmark: {}, onRunStoreBenchmark: {}, onRunSearchBenchmark: {},
            isBenchmarkEnabled: { true }, onShowComposerProbe: {}, onImport: {},
            onReply: { probe.replyRuns += 1 },
            onReplyAll: { probe.replyAllRuns += 1 },
            onForward: { probe.forwardRuns += 1 },
            isReplyEnabled: { probe.enabled })
        let menu = builder.makeMainMenu()
        let message = try #require(
            menu.items.compactMap(\.submenu).first { $0.title == "Message" })
        let reply = try #require(message.items.first { $0.title == "Reply" })
        let replyAll = try #require(message.items.first { $0.title == "Reply All" })
        let forward = try #require(message.items.first { $0.title == "Forward" })

        #expect(reply.identifier?.rawValue == "ch.lkmc.Sweetbriar.cmd.reply")
        #expect(replyAll.identifier?.rawValue == "ch.lkmc.Sweetbriar.cmd.replyAll")
        #expect(forward.identifier?.rawValue == "ch.lkmc.Sweetbriar.cmd.forward")
        #expect(reply.keyEquivalent == "r")
        #expect(replyAll.keyEquivalent == "R")
        #expect(forward.keyEquivalent == "F")
        #expect(reply.keyEquivalentModifierMask == .command)
        // An uppercase key equivalent implies Shift: ⇧⌘R and ⇧⌘F.
        #expect(replyAll.keyEquivalentModifierMask == .command)
        #expect(forward.keyEquivalentModifierMask == .command)

        for item in [reply, replyAll, forward] {
            #expect(!builder.validateMenuItem(item))
        }
        probe.enabled = true
        for item in [reply, replyAll, forward] {
            #expect(builder.validateMenuItem(item))
            #expect(NSApp.sendAction(try #require(item.action), to: builder, from: item))
        }
        #expect(probe.replyRuns == 1)
        #expect(probe.replyAllRuns == 1)
        #expect(probe.forwardRuns == 1)
    }

    @Test("Mark as Read and Star are nil-target ⇧⌘U and ⇧⌘L commands")
    func flagCommands() throws {
        let builder = MainMenuBuilder(
            onFillSynthetic: {}, onShowPrototype: {}, onImplementationChanged: { _ in },
            onRunBenchmark: {}, onRunStoreBenchmark: {}, onRunSearchBenchmark: {},
            isBenchmarkEnabled: { true }, onShowComposerProbe: {}, onImport: {})
        let message = try #require(
            builder.makeMainMenu().items.compactMap(\.submenu).first { $0.title == "Message" })
        let find = { (id: String) in message.items.first { $0.identifier?.rawValue == id } }
        let read = try #require(find("ch.lkmc.Sweetbriar.cmd.toggleRead"))
        let star = try #require(find("ch.lkmc.Sweetbriar.cmd.toggleStar"))
        #expect(read.title == "Mark as Read")
        #expect(star.title == "Star")
        #expect(read.keyEquivalent == "U" && star.keyEquivalent == "L")
        #expect(read.keyEquivalentModifierMask == .command)
        #expect(star.keyEquivalentModifierMask == .command)
        // The main window answers them down the responder chain.
        #expect(read.target == nil && star.target == nil)
        #expect(read.action == #selector(MessageFlagCommands.toggleRead(_:)))
        #expect(star.action == #selector(MessageFlagCommands.toggleStar(_:)))
    }
}
#endif
