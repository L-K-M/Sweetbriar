import AppKit
import SweetbriarBridge
import Testing

#if canImport(Sweetbriar)
    @testable import Sweetbriar
#endif

@MainActor
private func commandEventually(_ predicate: @MainActor () -> Bool) async -> Bool {
    let deadline = ContinuousClock.now + .seconds(10)
    while !predicate() {
        if ContinuousClock.now >= deadline || Task.isCancelled { return false }
        try? await Task.sleep(for: .milliseconds(5))
    }
    return true
}

private struct CommandClock: FrameClock { func nextFrame() async { await Task.yield() } }

/// A plain draft open in a compose window over a fresh archive.
@MainActor
private func withComposeWindow(
    test: @MainActor (ComposeWindowController) async throws -> Void
) async throws {
    let root = FileManager.default.temporaryDirectory.appendingPathComponent("sb-menu-\(UUID().uuidString)")
    let bridge = try await CoreBridge.open(
        config: CoreConfig(
            dataRoot: root.appendingPathComponent("data").path,
            cacheRoot: root.appendingPathComponent("cache").path),
        snapshots: FrameBatcher<[Setting]>(clock: CommandClock()) { _ in })
    var controller: ComposeWindowController?
    let result: Result<Void, any Error>
    do {
        let created = try await bridge.createDraft(idempotencyKey: UUID().uuidString, source: .new)
        let window = ComposeWindowController(source: bridge, draftId: created.draftId)
        controller = window
        window.showWindow(nil)
        try #require(await commandEventually { window.textView.isEditable })
        try await test(window)
        result = .success(())
    } catch { result = .failure(error) }
    if let controller, !(await controller.closeAndWait()) {
        _ = await controller.closeWithoutSavingAndWait()
    }
    await bridge.shutdown()
    try? FileManager.default.removeItem(at: root)
    try result.get()
}

private func menu(_ title: String, in bar: NSMenu) throws -> NSMenu {
    try #require(bar.items.compactMap(\.submenu).first { $0.title == title })
}

@MainActor
private func builder() -> MainMenuBuilder {
    MainMenuBuilder(
        onFillSynthetic: {}, onShowPrototype: {}, onImplementationChanged: { _ in },
        onRunBenchmark: {}, onRunStoreBenchmark: {}, onRunSearchBenchmark: {},
        isBenchmarkEnabled: { false }, onShowComposerProbe: {}, onImport: {})
}

/// The composer's menu commands (File → Save and Attach Files…, Message
/// → Send, Format → Make Plain Text) and its header rows.
@Suite(.serialized, .timeLimit(.minutes(1))) @MainActor
struct ComposeMenuCommandsTests {
    /// Regression: the rows used gravity areas, so an empty token field
    /// stayed at its few-point intrinsic width instead of filling the row.
    @Test func recipientFieldsFillTheirRows() async throws {
        try await withComposeWindow { controller in
            let content = try #require(controller.window?.contentView)
            content.layoutSubtreeIfNeeded()
            let subject = controller.subjectField.frame.width
            #expect(subject > 300)
            for field in [controller.toRecipients] {
                #expect(field.control.frame.width > subject * 0.8)
            }
        }
    }

    @Test func theMenusCarryTheDraftCommandsAsNilTargetItems() throws {
        let bar = builder().makeMainMenu()
        let file = try menu("File", in: bar)
        let save = try #require(file.items.first { $0.title == "Save" })
        let attach = try #require(file.items.first { $0.title == "Attach Files…" })
        let close = try #require(file.items.first { $0.title == "Close" })
        let send = try #require(try menu("Message", in: bar).items.first { $0.title == "Send" })
        let format = try #require(
            try menu("Format", in: bar).items.first { $0.action == #selector(ComposeCommands.toggleComposeFormat(_:)) })
        #expect(save.action == #selector(ComposeCommands.saveDraft(_:)))
        #expect(attach.action == #selector(ComposeCommands.attachFiles(_:)))
        #expect(send.action == #selector(ComposeCommands.sendMessage(_:)))
        #expect(close.action == #selector(NSWindow.performClose(_:)))
        #expect([save, attach, send, format, close].allSatisfy { $0.target == nil })
        // ⌘S, ⇧⌘A, ⇧⌘D and ⇧⌘T (an uppercase key implies Shift), ⌘W.
        #expect(save.keyEquivalent == "s" && attach.keyEquivalent == "A")
        #expect(send.keyEquivalent == "D" && format.keyEquivalent == "T" && close.keyEquivalent == "w")
        #expect(save.identifier?.rawValue == "ch.lkmc.Sweetbriar.cmd.saveDraft")
        #expect(attach.identifier?.rawValue == "ch.lkmc.Sweetbriar.cmd.attachFiles")
        #expect(send.identifier?.rawValue == "ch.lkmc.Sweetbriar.cmd.sendMessage")
        #expect(format.identifier?.rawValue == "ch.lkmc.Sweetbriar.cmd.toggleFormat")
        // Close lives in File only.
        let window = try menu("Window", in: bar)
        #expect(!window.items.contains { $0.action == #selector(NSWindow.performClose(_:)) })
    }

    @Test func theDraftCommandsFollowTheWindowsControls() async throws {
        try await withComposeWindow { controller in
            let bar = builder().makeMainMenu()
            let save = try #require(try menu("File", in: bar).items.first { $0.title == "Save" })
            let send = try #require(try menu("Message", in: bar).items.first { $0.title == "Send" })
            let format = try #require(
                try menu("Format", in: bar).items.first {
                    $0.action == #selector(ComposeCommands.toggleComposeFormat(_:))
                })
            // No sending identity in a fresh archive: Send is disabled,
            // as the button is.
            #expect(controller.validateMenuItem(send) == controller.sendButton.isEnabled)
            #expect(!controller.validateMenuItem(send))
            #expect(controller.validateMenuItem(save))

            // Make Markdown switches the format and retitles itself.
            #expect(controller.validateMenuItem(format))
            #expect(format.title == "Make Markdown")
            controller.toggleComposeFormat(format)
            #expect(controller.textView.mode == .markdown)
            #expect(controller.validateMenuItem(format))
            #expect(format.title == "Make Plain Text")
            controller.toggleComposeFormat(format)
            #expect(controller.textView.mode == .plain)

            // File → Save saves through the same explicit save.
            controller.subjectField.stringValue = "Saved from the menu"
            controller.saveDraft(save)
            #expect(await commandEventually { controller.model.content?.subject == "Saved from the menu" })
        }
    }

    @Test func theEditorsUseTheFindBar() {
        #expect(ComposerTextView().usesFindBar)
        #expect(PlainTextReaderView().usesFindBar)
        #expect(ReadingView().usesFindBar)
    }

    @Test func theViewAndMailboxMenusReachTheKeyWindow() throws {
        let bar = builder().makeMainMenu()
        let view = try menu("View", in: bar)
        let source = try #require(view.items.first { $0.title == "Message Source" })
        let reading = try #require(view.items.first { $0.title == "Reading View" })
        #expect(source.action == #selector(ReaderCommands.toggleMessageSource(_:)))
        #expect(reading.action == #selector(ReaderCommands.toggleReadingView(_:)))
        #expect(source.target == nil && reading.target == nil)
        #expect(view.items.contains { $0.action == #selector(NSSplitViewController.toggleSidebar(_:)) })
        #expect(view.items.contains { $0.action == #selector(NSWindow.toggleFullScreen(_:)) })

        let mailbox = try menu("Mailbox", in: bar)
        let ids = mailbox.items.compactMap { $0.representedObject as? String }
        #expect(ids == [
            SidebarModel.unifiedInboxID, SidebarModel.unifiedFlaggedID,
            SidebarModel.unifiedDraftsID, SidebarModel.unifiedOutBoxID,
        ])
        #expect(mailbox.items.map(\.keyEquivalent) == ["1", "2", "3", "4"])
        #expect(mailbox.items.allSatisfy { $0.action == #selector(MailboxCommands.goToMailbox(_:)) })
    }
}
