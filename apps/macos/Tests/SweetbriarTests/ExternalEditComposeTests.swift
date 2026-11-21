import AppKit
import SweetbriarBridge
import Testing

#if canImport(Sweetbriar)
    @testable import Sweetbriar
#endif

/// Stands in for the default app: it records what it was asked to open
/// and the test saves the file as an editor would.
@MainActor
private final class FakeEditorOpener: ExternalEditorOpening {
    var name: String? = "Fake Editor"
    var opened: [URL] = []
    func applicationName(for file: URL) -> String? { name }
    func open(_ file: URL) async throws { opened.append(file) }
}

@MainActor
private func externalEventually(_ predicate: @MainActor () -> Bool) async -> Bool {
    let deadline = ContinuousClock.now + .seconds(10)
    while !predicate() {
        if ContinuousClock.now >= deadline { return false }
        try? await Task.sleep(for: .milliseconds(5))
    }
    return true
}

private func save(_ text: String, to file: URL) throws {
    try Data(text.utf8).write(to: file, options: .atomic)
}

private func saveInvalid(to file: URL) throws {
    try Data([0xFF, 0xFE]).write(to: file, options: .atomic)
}

private struct ExternalClock: FrameClock { func nextFrame() async { await Task.yield() } }

/// A Markdown draft holding "Hello ", open in a window whose external
/// edits go under the fixture's cache root and open in `opener`.
@MainActor
private func withExternalWindow(
    test: @MainActor (ComposeWindowController, CoreBridge, FakeEditorOpener, URL) async throws -> Void
) async throws {
    let root = FileManager.default.temporaryDirectory.appendingPathComponent("sb-external-\(UUID().uuidString)")
    let bridge = try await CoreBridge.open(
        config: CoreConfig(
            dataRoot: root.appendingPathComponent("data").path,
            cacheRoot: root.appendingPathComponent("cache").path),
        snapshots: FrameBatcher<[Setting]>(clock: ExternalClock()) { _ in })
    let opener = FakeEditorOpener()
    var controller: ComposeWindowController?
    let result: Result<Void, any Error>
    do {
        let created = try await bridge.createDraft(idempotencyKey: UUID().uuidString, source: .new)
        _ = try await bridge.saveDraft(
            idempotencyKey: UUID().uuidString, draftId: created.draftId, baseRevisionId: nil,
            content: DraftContent(
                identityId: nil, format: .markdown, plainFlowed: true, subject: "External",
                to: [], cc: [], bcc: [], bodySource: "Hello ", attachments: []))
        let window = ComposeWindowController(
            source: bridge, draftId: created.draftId,
            externalEditor: ExternalEditor(root: bridge.externalEditRoot, opener: opener))
        controller = window
        window.showWindow(nil)
        try #require(await externalEventually { window.textView.isEditable && window.textView.string == "Hello " })
        window.window?.makeFirstResponder(window.textView)
        try await test(window, bridge, opener, bridge.externalEditRoot)
        result = .success(())
    } catch { result = .failure(error) }
    if let controller, !(await controller.closeWithoutSavingAndWait()) {
        Issue.record("the window didn't close")
    }
    await bridge.shutdown()
    try? FileManager.default.removeItem(at: root)
    try result.get()
}

@MainActor
private func bannerText(_ controller: ComposeWindowController) -> String? {
    (controller.externalEditBanner.views.first as? NSTextField)?.stringValue
}

@MainActor
private func editsLeft(in root: URL) -> [String] {
    (try? FileManager.default.contentsOfDirectory(atPath: root.path)) ?? []
}

@MainActor
private var editMenuItem: NSMenuItem {
    NSMenuItem(
        title: "Edit in External Editor",
        action: #selector(ComposeWindowController.editInExternalEditor(_:)), keyEquivalent: "E")
}

/// Edit in External Editor (spec/08 "External editor", M4.4e4).
@Suite(.serialized, .timeLimit(.minutes(1))) @MainActor
struct ExternalEditComposeTests {
    @Test func theEditMenuOffersShiftCommandE() throws {
        let builder = MainMenuBuilder(
            onFillSynthetic: {}, onShowPrototype: {}, onImplementationChanged: { _ in },
            onRunBenchmark: {}, onRunStoreBenchmark: {}, onRunSearchBenchmark: {},
            isBenchmarkEnabled: { false }, onShowComposerProbe: {}, onImport: {})
        let edit = try #require(builder.makeMainMenu().items.compactMap(\.submenu).first { $0.title == "Edit" })
        let item = try #require(
            edit.items.first { $0.action == #selector(ComposeWindowController.editInExternalEditor(_:)) })
        #expect(item.title == "Edit in External Editor")
        #expect(item.keyEquivalent == "E" && item.keyEquivalentModifierMask == [.command])
    }

    /// The body goes out as `draft.md`, stays read-only under the banner
    /// while the rest stays editable, and each save replaces it as one
    /// undoable edit that autosaves. Done ends the edit and deletes the
    /// file.
    @Test func savesReplaceTheBodyAndDoneEndsTheEdit() async throws {
        try await withExternalWindow { controller, _, opener, root in
            #expect(controller.validateMenuItem(editMenuItem))
            controller.editInExternalEditor(nil)
            try #require(await externalEventually { opener.opened.count == 1 })
            let file = opener.opened[0]
            #expect(file.lastPathComponent == "draft.md")
            #expect(try String(contentsOf: file, encoding: .utf8) == "Hello ")
            #expect(!controller.textView.isEditable)
            #expect(controller.subjectField.isEditable)
            #expect(!controller.externalEditBanner.isHidden)
            #expect(bannerText(controller) == "Editing in Fake Editor. Changes you save there appear here.")
            #expect(!controller.validateMenuItem(editMenuItem))

            try save("Hello from outside", to: file)
            try #require(await externalEventually { controller.textView.string == "Hello from outside" })
            #expect(controller.model.content?.bodySource == "Hello from outside")
            let undo = try #require(controller.window?.undoManager)
            #expect(undo.undoActionName == "External Edit")
            #expect(!controller.textView.isEditable)

            #expect(await controller.finishExternalEdit())
            #expect(controller.externalEdit == nil)
            #expect(controller.textView.isEditable)
            #expect(controller.externalEditBanner.isHidden)
            #expect(editsLeft(in: root).isEmpty)
            undo.undo()
            #expect(controller.textView.string == "Hello ")
        }
    }

    /// An invalid save shows why in the banner and changes nothing; Done
    /// keeps the edit until a later save fixes it.
    @Test func anInvalidSaveKeepsTheEditOpenWithItsReason() async throws {
        try await withExternalWindow { controller, _, opener, root in
            controller.editInExternalEditor(nil)
            try #require(await externalEventually { opener.opened.count == 1 })
            let file = opener.opened[0]
            try saveInvalid(to: file)
            try #require(
                await externalEventually { bannerText(controller) == ExternalEditFiles.ReadFailure.notUTF8.message })
            #expect(controller.textView.string == "Hello ")
            #expect(!(await controller.finishExternalEdit()))
            #expect(controller.externalEdit != nil && FileManager.default.fileExists(atPath: file.path))

            try save("Fixed", to: file)
            #expect(await controller.finishExternalEdit())
            #expect(controller.textView.string == "Fixed" && editsLeft(in: root).isEmpty)
        }
    }

    /// Closing reads a save made just before it, and the final save
    /// stores it.
    @Test func closingAppliesTheLastSaveBeforeTheFinalSave() async throws {
        try await withExternalWindow { controller, bridge, opener, root in
            controller.editInExternalEditor(nil)
            try #require(await externalEventually { opener.opened.count == 1 })
            try save("Closing words", to: opener.opened[0])
            #expect(await controller.closeAndWait())
            let draft = try await bridge.draft(id: controller.model.draftId)
            #expect(draft.revision?.bodySource == "Closing words")
            #expect(editsLeft(in: root).isEmpty)
        }
    }

    /// An invalid final read asks: Cancel keeps the window and the file,
    /// Discard External Edits closes and deletes it.
    @Test func closingOverAnInvalidReadAsksToDiscard() async throws {
        try await withExternalWindow { controller, bridge, opener, root in
            controller.editInExternalEditor(nil)
            try #require(await externalEventually { opener.opened.count == 1 })
            let file = opener.opened[0]
            try saveInvalid(to: file)

            let cancelled = Task { await controller.closeAndWait() }
            try #require(await externalEventually { controller.externalDiscardAlert != nil })
            let alert = try #require(controller.externalDiscardAlert)
            #expect(alert.buttons.map(\.title) == ["Cancel", "Discard External Edits"])
            controller.window?.endSheet(alert.window, returnCode: .alertFirstButtonReturn)
            #expect(!(await cancelled.value))
            #expect(controller.window?.isVisible == true)
            #expect(FileManager.default.fileExists(atPath: file.path))

            let discarded = Task { await controller.closeAndWait() }
            try #require(await externalEventually { controller.externalDiscardAlert != nil })
            let again = try #require(controller.externalDiscardAlert)
            controller.window?.endSheet(again.window, returnCode: .alertSecondButtonReturn)
            #expect(await discarded.value)
            #expect(editsLeft(in: root).isEmpty)
            let draft = try await bridge.draft(id: controller.model.draftId)
            #expect(draft.revision?.bodySource == "Hello ")
        }
    }

    /// A file no app opens leaves the body editable, says why and is
    /// deleted.
    @Test func aFileNoAppOpensLeavesTheBodyEditable() async throws {
        try await withExternalWindow { controller, _, opener, root in
            opener.name = nil
            controller.editInExternalEditor(nil)
            #expect(controller.externalEdit == nil && opener.opened.isEmpty)
            #expect(controller.textView.isEditable)
            #expect(controller.statusLabel.stringValue == "No app can open draft.md files.")
            #expect(editsLeft(in: root).isEmpty)
        }
    }

    /// Close Without Saving drops the external edits with the rest.
    @Test func closeWithoutSavingDropsTheExternalEdits() async throws {
        try await withExternalWindow { controller, _, opener, root in
            controller.editInExternalEditor(nil)
            try #require(await externalEventually { opener.opened.count == 1 })
            #expect(await controller.closeWithoutSavingAndWait())
            #expect(editsLeft(in: root).isEmpty)
        }
    }
}
